use crate::JsonSchema;
use crate::ResponsesApiTool;
use crate::ToolSpec;
use std::collections::BTreeMap;

pub const CRON_CREATE_TOOL_NAME: &str = "CronCreate";
pub const CRON_LIST_TOOL_NAME: &str = "CronList";
pub const CRON_DELETE_TOOL_NAME: &str = "CronDelete";
pub const SCHEDULE_WAKEUP_TOOL_NAME: &str = "ScheduleWakeup";

pub fn create_cron_create_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "cron".to_string(),
        JsonSchema::string(Some(
            "Standard 5-field cron expression in local time: \"M H DoM Mon DoW\".".to_string(),
        )),
    );
    properties.insert(
        "prompt".to_string(),
        JsonSchema::string(Some("The prompt to enqueue at each fire time.".to_string())),
    );
    properties.insert(
        "recurring".to_string(),
        JsonSchema::boolean(Some(
            "true (default) fires on every cron match until deleted or auto-expired after 7 days; false fires once at the next match."
                .to_string(),
        )),
    );
    properties.insert(
        "durable".to_string(),
        JsonSchema::boolean(Some(
            "true persists to .codex/scheduled_tasks.json and survives Codex restarts; false (default) is session-only."
                .to_string(),
        )),
    );

    ToolSpec::Function(ResponsesApiTool {
        name: CRON_CREATE_TOOL_NAME.to_string(),
        description: "Schedule a prompt to be enqueued at a future time. Supports recurring schedules and one-shot reminders using standard 5-field cron syntax."
            .to_string(),
        strict: false,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["cron".to_string(), "prompt".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
        defer_loading: None,
    })
}

pub fn create_cron_list_tool() -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: CRON_LIST_TOOL_NAME.to_string(),
        description: "List active scheduled cron jobs for this Codex session and the current project's durable .codex/scheduled_tasks.json file."
            .to_string(),
        strict: false,
        parameters: JsonSchema::object(BTreeMap::new(), Some(Vec::new()), Some(false.into())),
        output_schema: None,
        defer_loading: None,
    })
}

pub fn create_cron_delete_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "id".to_string(),
        JsonSchema::string(Some("Job ID returned by CronCreate.".to_string())),
    );

    ToolSpec::Function(ResponsesApiTool {
        name: CRON_DELETE_TOOL_NAME.to_string(),
        description: "Cancel a scheduled cron job by ID.".to_string(),
        strict: false,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["id".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
        defer_loading: None,
    })
}

pub fn create_schedule_wakeup_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "delaySeconds".to_string(),
        JsonSchema::integer(Some(
            "Delay in seconds before the prompt is enqueued.".to_string(),
        )),
    );
    properties.insert(
        "prompt".to_string(),
        JsonSchema::string(Some(
            "The prompt to enqueue when the wakeup fires.".to_string(),
        )),
    );
    properties.insert(
        "reason".to_string(),
        JsonSchema::string(Some(
            "Short human-readable reason for the wakeup.".to_string(),
        )),
    );

    ToolSpec::Function(ResponsesApiTool {
        name: SCHEDULE_WAKEUP_TOOL_NAME.to_string(),
        description: "Schedule a one-shot session-only prompt wakeup after a delay. This mirrors Claude Code's dynamic ScheduleWakeup loop primitive."
            .to_string(),
        strict: false,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["delaySeconds".to_string(), "prompt".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
        defer_loading: None,
    })
}
