use crate::function_tool::FunctionCallError;
use crate::scheduled_tasks;
use crate::scheduled_tasks::cron_task_summary_line;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::hook_names::HookToolName;
use crate::tools::registry::PreToolUsePayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_tools::CRON_CREATE_TOOL_NAME;
use codex_tools::CRON_DELETE_TOOL_NAME;
use codex_tools::CRON_LIST_TOOL_NAME;
use codex_tools::SCHEDULE_WAKEUP_TOOL_NAME;
use codex_tools::ToolName;
use serde_json::Value;

pub struct CronCreateHandler;
pub struct CronListHandler;
pub struct CronDeleteHandler;
pub struct ScheduleWakeupHandler;

impl ToolHandler for CronCreateHandler {
    type Output = FunctionToolOutput;

    fn tool_name(&self) -> ToolName {
        ToolName::plain(CRON_CREATE_TOOL_NAME)
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true
    }

    fn pre_tool_use_payload(&self, invocation: &ToolInvocation) -> Option<PreToolUsePayload> {
        hook_payload(CRON_CREATE_TOOL_NAME, &invocation.payload)
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session, payload, ..
        } = invocation;
        let value = function_arguments_value(payload)?;
        let cron = required_string(&value, "cron")?;
        let prompt = required_string(&value, "prompt")?;
        let recurring = scheduled_tasks::semantic_bool(value.get("recurring"), true)
            .map_err(FunctionCallError::RespondToModel)?;
        let durable = scheduled_tasks::semantic_bool(value.get("durable"), false)
            .map_err(FunctionCallError::RespondToModel)?;
        let result = session
            .scheduled_tasks
            .add_cron_task(cron, prompt, recurring, durable)
            .await
            .map_err(FunctionCallError::RespondToModel)?;

        let where_text = if result.durable {
            "Persisted to .codex/scheduled_tasks.json"
        } else {
            "Session-only (not written to disk, dies when Codex exits)"
        };
        let text = if result.recurring {
            format!(
                "Scheduled recurring job {} ({}). {where_text}. Auto-expires after 7 days. Use CronDelete to cancel sooner.",
                result.id, result.human_schedule
            )
        } else {
            format!(
                "Scheduled one-shot task {} ({}). {where_text}. It will fire once then auto-delete.",
                result.id, result.human_schedule
            )
        };
        Ok(FunctionToolOutput::from_text(text, Some(true)))
    }
}

impl ToolHandler for CronListHandler {
    type Output = FunctionToolOutput;

    fn tool_name(&self) -> ToolName {
        ToolName::plain(CRON_LIST_TOOL_NAME)
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session, payload, ..
        } = invocation;
        match payload {
            ToolPayload::Function { .. } => {
                let tasks = session.scheduled_tasks.list_cron_tasks().await;
                let text = if tasks.is_empty() {
                    "No scheduled jobs.".to_string()
                } else {
                    tasks
                        .iter()
                        .map(cron_task_summary_line)
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Ok(FunctionToolOutput::from_text(text, Some(true)))
            }
            _ => Err(FunctionCallError::RespondToModel(
                "CronList handler received unsupported payload".to_string(),
            )),
        }
    }
}

impl ToolHandler for CronDeleteHandler {
    type Output = FunctionToolOutput;

    fn tool_name(&self) -> ToolName {
        ToolName::plain(CRON_DELETE_TOOL_NAME)
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session, payload, ..
        } = invocation;
        let value = function_arguments_value(payload)?;
        let id = required_string(&value, "id")?;
        let removed = session
            .scheduled_tasks
            .remove_cron_task(&id)
            .await
            .map_err(FunctionCallError::RespondToModel)?;
        if !removed {
            return Err(FunctionCallError::RespondToModel(format!(
                "No scheduled job with id '{id}'"
            )));
        }
        Ok(FunctionToolOutput::from_text(
            format!("Cancelled job {id}."),
            Some(true),
        ))
    }
}

impl ToolHandler for ScheduleWakeupHandler {
    type Output = FunctionToolOutput;

    fn tool_name(&self) -> ToolName {
        ToolName::plain(SCHEDULE_WAKEUP_TOOL_NAME)
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true
    }

    fn pre_tool_use_payload(&self, invocation: &ToolInvocation) -> Option<PreToolUsePayload> {
        hook_payload(SCHEDULE_WAKEUP_TOOL_NAME, &invocation.payload)
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session, payload, ..
        } = invocation;
        let value = function_arguments_value(payload)?;
        let delay_seconds = scheduled_tasks::integer_u64(
            value
                .get("delaySeconds")
                .or_else(|| value.get("delay_seconds"))
                .ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "missing required field `delaySeconds`".to_string(),
                    )
                })?,
            "delaySeconds",
        )
        .map_err(FunctionCallError::RespondToModel)?;
        let prompt = required_string(&value, "prompt")?;
        let reason = optional_string(&value, "reason")?;
        let result = session
            .scheduled_tasks
            .add_wakeup(delay_seconds, prompt, reason)
            .await;

        Ok(FunctionToolOutput::from_text(
            format!(
                "Scheduled wakeup {} in {} second(s) (fires at {}).",
                result.id, delay_seconds, result.fire_at
            ),
            Some(true),
        ))
    }
}

fn function_arguments_value(payload: ToolPayload) -> Result<Value, FunctionCallError> {
    match payload {
        ToolPayload::Function { arguments } => {
            serde_json::from_str::<Value>(&arguments).map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to parse function arguments: {err}"
                ))
            })
        }
        _ => Err(FunctionCallError::RespondToModel(
            "scheduled-task handler received unsupported payload".to_string(),
        )),
    }
}

fn hook_payload(name: &str, payload: &ToolPayload) -> Option<PreToolUsePayload> {
    let ToolPayload::Function { arguments } = payload else {
        return None;
    };
    let tool_input = serde_json::from_str(arguments).unwrap_or(Value::Null);
    Some(PreToolUsePayload {
        tool_name: HookToolName::new(name),
        tool_input,
    })
}

fn required_string(value: &Value, field: &str) -> Result<String, FunctionCallError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(format!("missing required field `{field}`"))
        })
}

fn optional_string(value: &Value, field: &str) -> Result<Option<String>, FunctionCallError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(FunctionCallError::RespondToModel(format!(
            "`{field}` must be a string"
        ))),
    }
}
