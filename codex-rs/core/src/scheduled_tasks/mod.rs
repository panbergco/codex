mod cron;

use crate::session::session::Session;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseInputItem;
use cron::cron_to_human;
use cron::jittered_next_cron_run_ms;
use cron::next_cron_run_ms;
use cron::one_shot_jittered_next_cron_run_ms;
use cron::parse_cron_expression;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;
use tokio::time::Duration;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tracing::warn;
use uuid::Uuid;

const CHECK_INTERVAL: Duration = Duration::from_secs(1);
const MAX_JOBS: usize = 50;
const RECURRING_MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CronTask {
    pub(crate) id: String,
    pub(crate) cron: String,
    pub(crate) prompt: String,
    pub(crate) created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_fired_at: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) recurring: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    permanent: bool,
    #[serde(default, skip)]
    pub(crate) durable: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct WakeupTask {
    id: String,
    prompt: String,
    reason: Option<String>,
    fire_at: i64,
    created_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct CronFile {
    tasks: Vec<CronTask>,
}

#[derive(Debug, Clone)]
pub(crate) struct CronCreateResult {
    pub(crate) id: String,
    pub(crate) human_schedule: String,
    pub(crate) recurring: bool,
    pub(crate) durable: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct WakeupCreateResult {
    pub(crate) id: String,
    pub(crate) fire_at: i64,
}

pub(crate) struct ScheduledTasks {
    project_root: PathBuf,
    session_crons: Mutex<Vec<CronTask>>,
    wakeups: Mutex<Vec<WakeupTask>>,
    started: AtomicBool,
    cancellation_token: CancellationToken,
}

impl ScheduledTasks {
    pub(crate) fn new(project_root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            project_root,
            session_crons: Mutex::new(Vec::new()),
            wakeups: Mutex::new(Vec::new()),
            started: AtomicBool::new(false),
            cancellation_token: CancellationToken::new(),
        })
    }

    pub(crate) fn start_scheduler(self: &Arc<Self>, session: Weak<Session>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let scheduled_tasks = Arc::clone(self);
        tokio::spawn(async move {
            run_scheduler(scheduled_tasks, session).await;
        });
    }

    pub(crate) fn shutdown(&self) {
        self.cancellation_token.cancel();
    }

    pub(crate) async fn add_cron_task(
        &self,
        cron: String,
        prompt: String,
        recurring: bool,
        durable: bool,
    ) -> Result<CronCreateResult, String> {
        if parse_cron_expression(&cron).is_none() {
            return Err(format!(
                "Invalid cron expression '{cron}'. Expected 5 fields: M H DoM Mon DoW."
            ));
        }
        if next_cron_run_ms(&cron, now_ms()).is_none() {
            return Err(format!(
                "Cron expression '{cron}' does not match any calendar date in the next year."
            ));
        }
        if self.list_cron_tasks().await.len() >= MAX_JOBS {
            return Err(format!(
                "Too many scheduled jobs (max {MAX_JOBS}). Cancel one first."
            ));
        }

        let id = short_id();
        let task = CronTask {
            id: id.clone(),
            cron: cron.clone(),
            prompt,
            created_at: now_ms(),
            last_fired_at: None,
            recurring,
            permanent: false,
            durable,
        };

        if durable {
            let mut tasks = self.read_durable_cron_tasks().await;
            tasks.push(CronTask {
                durable: false,
                ..task.clone()
            });
            self.write_durable_cron_tasks(&tasks).await?;
        } else {
            self.session_crons.lock().await.push(task);
        }

        Ok(CronCreateResult {
            id,
            human_schedule: cron_to_human(&cron),
            recurring,
            durable,
        })
    }

    pub(crate) async fn list_cron_tasks(&self) -> Vec<CronTask> {
        let mut tasks = self.read_durable_cron_tasks().await;
        for task in &mut tasks {
            task.durable = true;
        }

        let mut session_tasks = self.session_crons.lock().await.clone();
        for task in &mut session_tasks {
            task.durable = false;
        }
        tasks.extend(session_tasks);
        tasks
    }

    pub(crate) async fn remove_cron_task(&self, id: &str) -> Result<bool, String> {
        let mut removed = false;
        {
            let mut session_tasks = self.session_crons.lock().await;
            let before = session_tasks.len();
            session_tasks.retain(|task| task.id != id);
            removed |= session_tasks.len() != before;
        }

        let durable_tasks = self.read_durable_cron_tasks().await;
        let before = durable_tasks.len();
        let remaining = durable_tasks
            .into_iter()
            .filter(|task| task.id != id)
            .collect::<Vec<_>>();
        if remaining.len() != before {
            removed = true;
            self.write_durable_cron_tasks(&remaining).await?;
        }

        Ok(removed)
    }

    pub(crate) async fn add_wakeup(
        &self,
        delay_seconds: u64,
        prompt: String,
        reason: Option<String>,
    ) -> WakeupCreateResult {
        let id = short_id();
        let created_at = now_ms();
        let fire_at = created_at.saturating_add((delay_seconds as i64).saturating_mul(1000));
        let wakeup = WakeupTask {
            id: id.clone(),
            prompt,
            reason,
            fire_at,
            created_at,
        };
        self.wakeups.lock().await.push(wakeup);
        WakeupCreateResult { id, fire_at }
    }

    async fn read_durable_cron_tasks(&self) -> Vec<CronTask> {
        let path = self.cron_file_path();
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Vec::new(),
            Err(err) => {
                warn!(
                    "failed to read scheduled tasks file {}: {err}",
                    path.display()
                );
                return Vec::new();
            }
        };
        let parsed: CronFile = match serde_json::from_str(&raw) {
            Ok(parsed) => parsed,
            Err(err) => {
                warn!(
                    "failed to parse scheduled tasks file {}: {err}",
                    path.display()
                );
                return Vec::new();
            }
        };

        parsed
            .tasks
            .into_iter()
            .filter_map(|mut task| {
                if task.id.is_empty()
                    || task.cron.is_empty()
                    || task.prompt.is_empty()
                    || task.created_at <= 0
                    || parse_cron_expression(&task.cron).is_none()
                {
                    debug!("skipping malformed scheduled task {}", task.id);
                    return None;
                }
                task.durable = true;
                Some(task)
            })
            .collect()
    }

    async fn write_durable_cron_tasks(&self, tasks: &[CronTask]) -> Result<(), String> {
        let dir = self.codex_dir();
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|err| format!("failed to create {}: {err}", dir.display()))?;
        let path = self.cron_file_path();
        let body = CronFile {
            tasks: tasks
                .iter()
                .cloned()
                .map(|mut task| {
                    task.durable = false;
                    task
                })
                .collect(),
        };
        let json = serde_json::to_string_pretty(&body)
            .map_err(|err| format!("failed to serialize scheduled tasks: {err}"))?;
        tokio::fs::write(&path, format!("{json}\n"))
            .await
            .map_err(|err| format!("failed to write {}: {err}", path.display()))
    }

    fn codex_dir(&self) -> PathBuf {
        self.project_root.join(".codex")
    }

    fn cron_file_path(&self) -> PathBuf {
        self.codex_dir().join("scheduled_tasks.json")
    }

    fn lock_file_path(&self) -> PathBuf {
        self.codex_dir().join("scheduled_tasks.lock")
    }

    async fn has_durable_cron_tasks(&self) -> bool {
        !self.read_durable_cron_tasks().await.is_empty()
    }
}

async fn run_scheduler(tasks: Arc<ScheduledTasks>, session: Weak<Session>) {
    let token = tasks.cancellation_token.clone();
    let mut lock_file = None;
    let mut check = tokio::time::interval(CHECK_INTERVAL);
    check.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut next_fire_at: HashMap<String, i64> = HashMap::new();
    let mut missed_asked: HashSet<String> = HashSet::new();
    let mut processed_initial_missed = false;

    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = check.tick() => {
                let Some(sess) = session.upgrade() else {
                    break;
                };
                if session_is_loading(&sess).await {
                    continue;
                }
                if lock_file.is_none() && tasks.has_durable_cron_tasks().await {
                    lock_file = try_acquire_scheduler_lock(&tasks.lock_file_path()).await;
                    processed_initial_missed = false;
                }
                if lock_file.is_some() && !processed_initial_missed {
                    process_missed_one_shots(&tasks, &sess, &mut missed_asked).await;
                    processed_initial_missed = true;
                }
                process_due_tasks(&tasks, &sess, lock_file.is_some(), &mut next_fire_at).await;
            }
        }
    }
}

async fn process_missed_one_shots(
    tasks: &ScheduledTasks,
    session: &Arc<Session>,
    missed_asked: &mut HashSet<String>,
) {
    let now = now_ms();
    let durable_tasks = tasks.read_durable_cron_tasks().await;
    let missed = durable_tasks
        .iter()
        .filter(|task| !task.recurring)
        .filter(|task| !missed_asked.contains(&task.id))
        .filter(|task| next_cron_run_ms(&task.cron, task.created_at).is_some_and(|next| next < now))
        .cloned()
        .collect::<Vec<_>>();
    if missed.is_empty() {
        return;
    }

    for task in &missed {
        missed_asked.insert(task.id.clone());
    }
    let missed_ids = missed
        .iter()
        .map(|task| task.id.as_str())
        .collect::<HashSet<_>>();
    let remaining = durable_tasks
        .into_iter()
        .filter(|task| !missed_ids.contains(task.id.as_str()))
        .collect::<Vec<_>>();
    if let Err(err) = tasks.write_durable_cron_tasks(&remaining).await {
        warn!("failed to remove missed scheduled tasks: {err}");
    }
    fire_prompt(session, build_missed_task_notification(&missed)).await;
}

async fn process_due_tasks(
    tasks: &ScheduledTasks,
    session: &Arc<Session>,
    owns_durable_lock: bool,
    next_fire_at: &mut HashMap<String, i64>,
) {
    let now = now_ms();
    let mut seen = HashSet::new();

    if owns_durable_lock {
        let durable_tasks = tasks.read_durable_cron_tasks().await;
        let mut write_back = durable_tasks.clone();
        let mut changed = false;
        let mut fired_delete_ids = HashSet::new();

        for task in durable_tasks {
            seen.insert(task.id.clone());
            if should_fire(&task, now, next_fire_at) {
                fire_prompt(session, task.prompt.clone()).await;
                if task.recurring && !is_recurring_task_aged(&task, now) {
                    let new_next =
                        jittered_next_cron_run_ms(&task.cron, now, &task.id).unwrap_or(i64::MAX);
                    next_fire_at.insert(task.id.clone(), new_next);
                    for stored in &mut write_back {
                        if stored.id == task.id {
                            stored.last_fired_at = Some(now);
                            changed = true;
                        }
                    }
                } else {
                    fired_delete_ids.insert(task.id.clone());
                    next_fire_at.remove(&task.id);
                    changed = true;
                }
            }
        }

        if changed {
            write_back.retain(|task| !fired_delete_ids.contains(&task.id));
            if let Err(err) = tasks.write_durable_cron_tasks(&write_back).await {
                warn!("failed to update scheduled tasks file: {err}");
            }
        }
    }

    {
        let session_tasks = tasks.session_crons.lock().await.clone();
        let mut delete_ids = HashSet::new();
        for task in session_tasks {
            seen.insert(task.id.clone());
            if should_fire(&task, now, next_fire_at) {
                fire_prompt(session, task.prompt.clone()).await;
                if task.recurring && !is_recurring_task_aged(&task, now) {
                    let new_next =
                        jittered_next_cron_run_ms(&task.cron, now, &task.id).unwrap_or(i64::MAX);
                    next_fire_at.insert(task.id.clone(), new_next);
                } else {
                    delete_ids.insert(task.id.clone());
                    next_fire_at.remove(&task.id);
                }
            }
        }
        if !delete_ids.is_empty() {
            let mut session_tasks = tasks.session_crons.lock().await;
            session_tasks.retain(|task| !delete_ids.contains(&task.id));
        }
    }

    {
        let mut wakeups = tasks.wakeups.lock().await;
        let mut due = Vec::new();
        wakeups.retain(|wakeup| {
            if wakeup.fire_at <= now {
                due.push(wakeup.clone());
                false
            } else {
                seen.insert(wakeup.id.clone());
                true
            }
        });
        drop(wakeups);
        for wakeup in due {
            debug!(
                "firing scheduled wakeup {} reason={:?} age_ms={}",
                wakeup.id,
                wakeup.reason,
                now.saturating_sub(wakeup.created_at)
            );
            fire_prompt(session, wakeup.prompt).await;
        }
    }

    next_fire_at.retain(|id, _| seen.contains(id));
}

fn should_fire(task: &CronTask, now: i64, next_fire_at: &mut HashMap<String, i64>) -> bool {
    let next = *next_fire_at.entry(task.id.clone()).or_insert_with(|| {
        if task.recurring {
            jittered_next_cron_run_ms(
                &task.cron,
                task.last_fired_at.unwrap_or(task.created_at),
                &task.id,
            )
            .unwrap_or(i64::MAX)
        } else {
            one_shot_jittered_next_cron_run_ms(&task.cron, task.created_at, &task.id)
                .unwrap_or(i64::MAX)
        }
    });
    now >= next
}

fn is_recurring_task_aged(task: &CronTask, now: i64) -> bool {
    task.recurring && !task.permanent && now.saturating_sub(task.created_at) >= RECURRING_MAX_AGE_MS
}

async fn fire_prompt(session: &Arc<Session>, prompt: String) {
    let item = ResponseInputItem::Message {
        role: "user".to_string(),
        content: vec![ContentItem::InputText { text: prompt }],
        phase: None,
    };
    session.queue_response_items_for_next_turn(vec![item]).await;
    session.maybe_start_turn_for_pending_work().await;
}

async fn session_is_loading(session: &Session) -> bool {
    session.active_turn.lock().await.is_some()
}

async fn try_acquire_scheduler_lock(path: &Path) -> Option<std::fs::File> {
    let path = path.to_path_buf();
    match tokio::task::spawn_blocking(move || try_acquire_scheduler_lock_blocking(&path)).await {
        Ok(Ok(file)) => file,
        Ok(Err(err)) => {
            debug!("scheduled task lock unavailable: {err}");
            None
        }
        Err(err) => {
            debug!("scheduled task lock task failed: {err}");
            None
        }
    }
}

fn try_acquire_scheduler_lock_blocking(path: &Path) -> io::Result<Option<std::fs::File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => {
            let body = serde_json::json!({
                "pid": std::process::id(),
                "acquiredAt": now_ms(),
            })
            .to_string();
            file.set_len(0)?;
            let mut file_ref = &file;
            file_ref.write_all(body.as_bytes())?;
            file_ref.flush()?;
            Ok(Some(file))
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub(crate) fn cron_task_summary_line(task: &CronTask) -> String {
    let recurring = if task.recurring {
        " (recurring)"
    } else {
        " (one-shot)"
    };
    let durable = if task.durable { "" } else { " [session-only]" };
    format!(
        "{} - {}{}{}: {}",
        task.id,
        cron_to_human(&task.cron),
        recurring,
        durable,
        truncate_prompt(&task.prompt, 80)
    )
}

fn truncate_prompt(prompt: &str, max_chars: usize) -> String {
    if prompt.chars().count() <= max_chars {
        return prompt.to_string();
    }
    let mut truncated = prompt
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

fn build_missed_task_notification(missed: &[CronTask]) -> String {
    let plural = missed.len() > 1;
    let mut text = format!(
        "The following one-shot scheduled task{} missed while Codex was not running. {} already been removed from .codex/scheduled_tasks.json.\n\nDo NOT execute {} prompt{} yet. First ask the user whether to run {} now. Only execute if the user confirms.",
        if plural { "s were" } else { " was" },
        if plural { "They have" } else { "It has" },
        if plural { "these" } else { "this" },
        if plural { "s" } else { "" },
        if plural { "each one" } else { "it" },
    );

    for task in missed {
        text.push_str("\n\n");
        text.push_str(&format!(
            "[{}, created {}]\n{}\n{}\n{}",
            cron_to_human(&task.cron),
            task.created_at,
            fence_for(&task.prompt),
            task.prompt,
            fence_for(&task.prompt)
        ));
    }
    text
}

fn fence_for(text: &str) -> String {
    let mut longest = 0usize;
    let mut current = 0usize;
    for ch in text.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    "`".repeat(longest.saturating_add(1).max(3))
}

fn short_id() -> String {
    Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect()
}

pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(crate) fn semantic_bool(value: Option<&Value>, default: bool) -> Result<bool, String> {
    let Some(value) = value else {
        return Ok(default);
    };
    match value {
        Value::Bool(value) => Ok(*value),
        Value::String(value) if value.eq_ignore_ascii_case("true") => Ok(true),
        Value::String(value) if value.eq_ignore_ascii_case("false") => Ok(false),
        _ => Err("expected boolean".to_string()),
    }
}

pub(crate) fn integer_u64(value: &Value, field: &str) -> Result<u64, String> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| format!("{field} must be a non-negative integer")),
        Value::String(value) => value
            .parse::<u64>()
            .map_err(|_| format!("{field} must be a non-negative integer")),
        _ => Err(format!("{field} must be a non-negative integer")),
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missed_notification_fences_prompt() {
        let task = CronTask {
            id: "abc12345".to_string(),
            cron: "0 * * * *".to_string(),
            prompt: "run ``` nested".to_string(),
            created_at: 1,
            last_fired_at: None,
            recurring: false,
            permanent: false,
            durable: true,
        };

        let text = build_missed_task_notification(&[task]);
        assert!(text.contains("````\nrun ``` nested\n````"));
    }
}
