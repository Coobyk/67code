//! Settings sections: Scheduled Tasks, Secrets, Diagnostics, Usage Limits.
//!
//! Wire tags (native/server/src/rpc_tasks.rs + rpc_misc.rs):
//!   stream `scheduledTasks.subscribe` → full `{tasks:[…]}` snapshots
//!   `scheduledTasks.upsert` / `.setEnabled` / `.delete` / `.runNow`
//!   `secrets.answerRequest` — resolves a pending `secret_request` turn item
//!   `server.getProcessDiagnostics` / `getHostResources` / `getDatabaseStorage`
//!   `server.runStorageCleanup` / `reclaimDatabaseSpace`
//!   `server.getUsageSummary` / `refreshUsageRates`
//!
//! Deferred-open pattern: `render_*` only has `&self`, so first-render data
//! loads are queued through `cx.spawn` and run under `this.update` — the same
//! `&mut` context the click listeners get.

use std::collections::HashMap;

use gpui::{
    Context, Div, Entity, EntityInputHandler, Focusable, MouseButton, SharedString, Subscription,
    Window, div, prelude::*, px, rgb,
};
use serde_json::{Value, json};

use crate::notifications::ToastKind;
use crate::text_field::{PromptSubmitted, TextField};
use crate::{AppView, theme};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Human message out of an `Exit` failure frame (same walk as acp.rs).
fn exit_message(exit: &Value) -> String {
    let cause = &exit["cause"];
    for err in [
        &cause["error"],
        &cause["failure"],
        &exit["error"],
        &exit["failure"],
        cause,
    ] {
        if let Some(message) = err["message"].as_str().or_else(|| err["reason"].as_str()) {
            return message.to_string();
        }
        if let Some(message) = err.as_str() {
            return message.to_string();
        }
    }
    "The server request failed.".to_string()
}

/// `YYYY-MM-DD HH:MMZ` from an ISO timestamp; raw string when it doesn't fit.
fn iso_short(iso: &str) -> String {
    iso.get(..16)
        .filter(|_| iso.as_bytes().get(10) == Some(&b'T'))
        .map(|slice| format!("{}Z", slice.replace('T', " ")))
        .unwrap_or_else(|| iso.to_string())
}

fn human_ms(ms: i64) -> String {
    if ms >= 60_000 && ms % 604_800_000 == 0 {
        format!("{}w", ms / 604_800_000)
    } else if ms >= 60_000 && ms % 86_400_000 == 0 {
        format!("{}d", ms / 86_400_000)
    } else if ms >= 60_000 && ms % 3_600_000 == 0 {
        format!("{}h", ms / 3_600_000)
    } else if ms >= 60_000 && ms % 60_000 == 0 {
        format!("{}m", ms / 60_000)
    } else {
        format!("{}s", ms / 1_000)
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Thousands-separated integer, like the web's `Intl.NumberFormat`.
fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `YYYY-MM-DD` (UTC) from unix seconds — civil-from-days, no time dep.
fn day_string(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

/// Small settings-surface button (same look as `acp_button`).
fn row_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_3()
        .py_1p5()
        .rounded_md()
        .text_xs()
        .bg(theme::accent())
        .when(enabled, |s| {
            s.cursor_pointer()
                .text_color(theme::fg())
                .hover(|s| s.bg(theme::hover()))
        })
        .when(!enabled, |s| s.text_color(theme::fg_muted()).opacity(0.6))
        .child(label.into())
}

/// Muted single-line hint (same look as acp.rs `hint`).
fn hint(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .py_1p5()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(text.into())
}

/// Caption above a group of rows (mirrors `group_label` in settings.rs).
fn group_label(text: &'static str) -> gpui::Div {
    div()
        .pb_1()
        .pt_3()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(text)
}

/// Read a text field's contents when no `&mut Window` is on the stack
/// (same helper as envs.rs `field_text`).
fn field_text(field: &Entity<TextField>, cx: &mut Context<AppView>) -> String {
    if let Some(text) = cx
        .with_window(field.entity_id(), |window, app| {
            field.update(app, |field, cx| {
                field.text_for_range(0..usize::MAX, &mut None, window, cx)
            })
        })
        .flatten()
    {
        return text;
    }
    for handle in cx.windows() {
        if let Ok(text) = handle.update(cx, |_root, window, app| {
            field.update(app, |field, cx| {
                field.text_for_range(0..usize::MAX, &mut None, window, cx)
            })
        }) {
            return text.unwrap_or_default();
        }
    }
    String::new()
}

/// Read a field inside an event listener that has `window`.
fn field_text_in(
    field: &Entity<TextField>,
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> String {
    field
        .update(cx, |field, cx| {
            field.text_for_range(0..usize::MAX, &mut None, window, cx)
        })
        .unwrap_or_default()
}

/// A labeled text input row used by the task form and the secret answer —
/// the web `Input` shell: `rounded-lg border-input`, `bg-input/32`, `px-3`.
fn labeled_field(caption: &'static str, field: &Entity<TextField>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().text_color(theme::fg_muted()).child(caption))
        .child(
            div()
                .px_3()
                .py_1()
                .rounded_lg()
                .border_1()
                .border_color(theme::input())
                .bg(theme::field_surface())
                .text_sm()
                .text_color(theme::fg())
                .child(field.clone()),
        )
}

// ---------------------------------------------------------------------------
// Scheduled tasks
// ---------------------------------------------------------------------------

/// One task row parsed from a `scheduledTasks.subscribe` snapshot.
#[derive(Clone, Default)]
struct TaskRow {
    id: String,
    title: String,
    prompt: String,
    enabled: bool,
    schedule: String,
    project_id: String,
    last_status: String,
    last_error: Option<String>,
    run_count: i64,
    next_run_at: Option<String>,
    webhook_path: Option<String>,
    blocked: Option<String>,
}

/// `ScheduledTaskSchedule` → one line of text (cron is not a wire type).
fn schedule_text(schedule: &Value) -> String {
    match str_of(schedule, "type") {
        Some("interval") => {
            let ms = schedule["everyMs"].as_i64().unwrap_or(0);
            format!("every {}", human_ms(ms))
        }
        Some("fixed_time") => {
            let tod = str_of(schedule, "timeOfDay").unwrap_or("??:??");
            const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
            let days: Vec<&str> = schedule["weekdays"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|d| d.as_i64())
                        .filter_map(|d| DAYS.get(d as usize).copied())
                        .collect()
                })
                .unwrap_or_default();
            if days.is_empty() || days.len() == 7 {
                format!("daily {tod}")
            } else {
                format!("{} {tod}", days.join(","))
            }
        }
        Some("once") => str_of(schedule, "runAt")
            .map(|at| format!("once {}", iso_short(at)))
            .unwrap_or_else(|| "once".to_string()),
        Some("webhook") => "webhook".to_string(),
        other => other.unwrap_or("unknown").to_string(),
    }
}

fn parse_task(v: &Value) -> Option<TaskRow> {
    let id = str_of(v, "id")?.to_string();
    Some(TaskRow {
        id,
        title: str_of(v, "title").unwrap_or("(untitled)").to_string(),
        prompt: str_of(v, "prompt").unwrap_or("").to_string(),
        enabled: v["enabled"].as_bool() == Some(true),
        schedule: schedule_text(&v["schedule"]),
        project_id: str_of(v, "projectId").unwrap_or("").to_string(),
        last_status: str_of(v, "lastRunStatus").unwrap_or("never").to_string(),
        last_error: str_of(v, "lastRunError").map(str::to_owned),
        run_count: v["runCount"].as_i64().unwrap_or(0),
        next_run_at: str_of(v, "nextRunAt").map(str::to_owned),
        webhook_path: v["webhook"]["path"].as_str().map(str::to_owned),
        blocked: str_of(v, "blockedByPolicy").map(str::to_owned),
    })
}

/// Compact schedule spec → `ScheduledTaskUpsertSchedule`. The wire has no
/// cron type; accepted spellings:
///   `every 30m` / `30m` `2h` `1d` `1w`  → interval
///   `daily 09:30` / `09:30` / `at 09:30` → fixed_time, every day
///   `mon,wed,fri 09:30`                  → fixed_time, those weekdays
///   `once 2026-03-01 09:30`             → once (UTC, must be future)
///   `webhook`                            → webhook URL trigger
fn parse_schedule(text: &str) -> Result<Value, String> {
    let raw = text.trim();
    let lower = raw.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let head = words.first().copied().unwrap_or("");
    if head == "webhook" {
        return Ok(json!({ "type": "webhook" }));
    }
    // `once 2026-03-01 09:30` — seconds/offset optional, UTC assumed.
    if head == "once" {
        let rest = raw[4..].trim();
        let mut stamp = rest.replace(' ', "T");
        if stamp.len() == 16 {
            stamp.push_str(":00Z");
        } else if stamp.len() == 19 {
            stamp.push('Z');
        }
        let ok = stamp.len() == 20
            && stamp.as_bytes()[4] == b'-'
            && stamp.as_bytes()[7] == b'-'
            && stamp.ends_with('Z')
            && stamp[..4].parse::<i32>().is_ok()
            && stamp[5..7]
                .parse::<u8>()
                .is_ok_and(|m| (1..=12).contains(&m))
            && stamp[8..10]
                .parse::<u8>()
                .is_ok_and(|d| (1..=31).contains(&d));
        if !ok {
            return Err("once needs a date+time like 2026-03-01 09:30.".to_string());
        }
        return Ok(json!({ "type": "once", "runAt": stamp }));
    }
    // <N><unit> interval, with optional leading "every".
    let spec = if head == "every" {
        words.get(1).copied().unwrap_or("")
    } else {
        head
    };
    let (num_part, unit_ms) = match spec.chars().last() {
        Some('m') => (&spec[..spec.len().saturating_sub(1)], 60_000i64),
        Some('h') => (&spec[..spec.len().saturating_sub(1)], 3_600_000),
        Some('d') => (&spec[..spec.len().saturating_sub(1)], 86_400_000),
        Some('w') => (&spec[..spec.len().saturating_sub(1)], 604_800_000),
        _ => ("", 0),
    };
    if unit_ms > 0 {
        return match num_part.parse::<i64>() {
            Ok(n) if n > 0 => Ok(json!({ "type": "interval", "everyMs": n * unit_ms })),
            _ => Err(format!(
                "Unparsable interval '{spec}' — try 30m, 2h, 1d, 1w."
            )),
        };
    }
    // fixed_time: `daily HH:MM` / `at HH:MM` / bare `HH:MM` / `mon,wed HH:MM`.
    const DOW: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
    let is_time = |t: &str| {
        let b = t.as_bytes();
        t.len() == 5
            && b[0].is_ascii_digit()
            && b[1].is_ascii_digit()
            && b[2] == b':'
            && b[3].is_ascii_digit()
            && b[4].is_ascii_digit()
            && t[..2].parse::<u8>().is_ok_and(|h| h <= 23)
            && t[3..].parse::<u8>().is_ok_and(|m| m <= 59)
    };
    let (mut weekdays, time_word) = match head {
        "daily" | "at" => (None, words.get(1).copied().unwrap_or("")),
        _ => {
            let mut days: Vec<i64> = Vec::new();
            let mut all_days = !head.is_empty();
            for part in head.split(',') {
                match DOW.iter().position(|d| *d == part) {
                    Some(d) => days.push(d as i64),
                    None => {
                        all_days = false;
                        break;
                    }
                }
            }
            if all_days && words.len() > 1 {
                (Some(std::mem::take(&mut days)), words[1])
            } else {
                (None, head)
            }
        }
    };
    if is_time(time_word) {
        let mut schedule = json!({ "type": "fixed_time", "timeOfDay": time_word });
        if let Some(days) = weekdays.take() {
            schedule["weekdays"] = json!(days);
        }
        return Ok(schedule);
    }
    if weekdays.is_some() {
        return Err("Weekday schedule needs HH:MM after the days.".to_string());
    }
    Err("Unparsable schedule — try '30m', 'daily 09:30', 'mon,wed 09:30', 'once 2026-03-01 09:30' or 'webhook'.".to_string())
}

#[derive(Default)]
struct TasksState {
    /// Open `scheduledTasks.subscribe` stream id (None once it ends).
    stream_id: Option<String>,
    /// A stream was requested — queued spawns don't reopen it.
    stream_started: bool,
    loaded: bool,
    error: Option<String>,
    tasks: Vec<TaskRow>,
    /// Task id whose delete is armed — a second click confirms.
    confirm_delete: Option<String>,
    /// Task id with a `runNow` in flight.
    running: Option<String>,
    /// Task id with a `setEnabled` in flight.
    toggling: Option<String>,
    // New-task mini form.
    form_open: bool,
    title_field: Option<Entity<TextField>>,
    prompt_field: Option<Entity<TextField>>,
    schedule_field: Option<Entity<TextField>>,
    draft_title: String,
    draft_prompt: String,
    draft_schedule: String,
    project_menu_open: bool,
    project_id: Option<String>,
    form_error: Option<SharedString>,
    saving: bool,
    subs: Vec<Subscription>,
}

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

#[derive(Default)]
struct SecretsState {
    /// turnItemId → answer field, created lazily per pending request.
    answer_fields: HashMap<String, Entity<TextField>>,
    /// Mirrors for button enabled state / fallback reads.
    draft_answers: HashMap<String, String>,
    /// turnItemIds answered from this client (disable until status echoes).
    answered: std::collections::HashSet<String>,
    /// turnItemId with an answer request in flight.
    answering: Option<String>,
    subs: Vec<Subscription>,
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

#[derive(Default)]
struct DiagState {
    loaded: bool,
    loading: bool,
    error: Option<String>,
    /// Raw `getProcessDiagnostics` value.
    process: Option<Value>,
    /// Raw `getHostResources` value.
    host: Option<Value>,
    /// Raw `getDatabaseStorage` value (may be JSON null → no db).
    db: Option<Value>,
    cleaning: bool,
    reclaiming: bool,
    /// Last cleanup outcome line ("freed 12.3 MiB · 1 removed").
    cleaned: Option<String>,
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum UsagePeriod {
    Today,
    #[default]
    Last7,
    Last30,
}

impl UsagePeriod {
    fn label(self) -> &'static str {
        match self {
            UsagePeriod::Today => "Today",
            UsagePeriod::Last7 => "7 days",
            UsagePeriod::Last30 => "30 days",
        }
    }
}

/// One `UsageSummary.buckets[]` row.
#[derive(Clone, Default)]
struct UsageBucket {
    when: String,
    provider: String,
    model: String,
    input: u64,
    cache_read: u64,
    cache_write: u64,
    output: u64,
    reasoning: u64,
    records: u64,
    sessions: u64,
}

#[derive(Default)]
struct UsageState {
    loaded: bool,
    loading: bool,
    error: Option<String>,
    period: UsagePeriod,
    buckets: Vec<UsageBucket>,
    pricing_status: Option<String>,
    read_at: Option<String>,
    refreshing: bool,
    /// Bumped on period change so late replies can't clobber new state.
    generation: u64,
}

// ---------------------------------------------------------------------------
// Section state owned by this file
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct State {
    tasks: TasksState,
    secrets: SecretsState,
    diagnostics: DiagState,
    usage: UsageState,
}

impl AppView {
    fn backend_toast(
        &mut self,
        kind: ToastKind,
        title: impl Into<SharedString>,
        detail: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.push_toast(kind, title, detail, None, cx);
    }

    // ---------------------------------------------------------------------
    // Scheduled tasks
    // ---------------------------------------------------------------------

    /// Selected project for the new-task form (falls back to the first).
    fn tasks_project_id(&self) -> Option<String> {
        let selected = self.backend_settings_state.tasks.project_id.as_deref();
        self.state
            .projects
            .iter()
            .find(|p| Some(p.id.as_ref()) == selected)
            .or_else(|| self.state.projects.first())
            .map(|p| p.id.to_string())
    }

    /// Open `scheduledTasks.subscribe` once; chunks carry full snapshots.
    fn tasks_ensure_stream(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.backend_settings_state.tasks;
        if state.stream_started && state.stream_id.is_some() {
            return;
        }
        let Some((stream_id, rx)) = self.open_stream("scheduledTasks.subscribe", json!({})) else {
            self.backend_settings_state.tasks.error =
                Some("Not connected to a server.".to_string());
            return;
        };
        let state = &mut self.backend_settings_state.tasks;
        state.stream_started = true;
        state.stream_id = Some(stream_id);
        state.error = None;
        cx.spawn(async move |this, cx| {
            while let Ok(chunk) = rx.recv().await {
                let tasks: Vec<TaskRow> = chunk["tasks"]
                    .as_array()
                    .map(|a| a.iter().filter_map(parse_task).collect())
                    .unwrap_or_default();
                let _ = this.update(cx, |this, cx| {
                    let state = &mut this.backend_settings_state.tasks;
                    state.tasks = tasks;
                    state.loaded = true;
                    cx.notify();
                });
            }
            // Stream ended (server stopped or disconnect): allow reopen.
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.backend_settings_state.tasks;
                state.stream_id = None;
                cx.notify();
            });
        })
        .detach();
    }

    /// `scheduledTasks.setEnabled` — the stream snapshot repaints the row.
    fn tasks_set_enabled(&mut self, id: String, enabled: bool, cx: &mut Context<Self>) {
        if self.backend_settings_state.tasks.toggling.is_some() {
            return;
        }
        let Some(rx) = self.send_query(
            "scheduledTasks.setEnabled",
            json!({ "id": id, "enabled": enabled }),
        ) else {
            self.backend_toast(
                ToastKind::Error,
                "Could not update task",
                Some("Not connected to a server.".to_string()),
                cx,
            );
            return;
        };
        self.backend_settings_state.tasks.toggling = Some(id);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let _ = this.update(cx, |this, cx| {
                this.backend_settings_state.tasks.toggling = None;
                if exit["_tag"].as_str() != Some("Success") {
                    this.backend_toast(
                        ToastKind::Error,
                        "Could not update task",
                        Some(exit_message(&exit)),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `scheduledTasks.runNow` — dispatches the prompt; toast the outcome.
    fn tasks_run_now(&mut self, id: String, cx: &mut Context<Self>) {
        if self.backend_settings_state.tasks.running.is_some() {
            return;
        }
        let Some(rx) = self.send_query("scheduledTasks.runNow", json!({ "id": id })) else {
            self.backend_toast(
                ToastKind::Error,
                "Could not run task",
                Some("Not connected to a server.".to_string()),
                cx,
            );
            return;
        };
        self.backend_settings_state.tasks.running = Some(id.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let _ = this.update(cx, |this, cx| {
                this.backend_settings_state.tasks.running = None;
                if exit["_tag"].as_str() == Some("Success") {
                    let value = &exit["value"];
                    if value["dispatchFailed"].as_bool() == Some(true) {
                        this.backend_toast(
                            ToastKind::Error,
                            "Task dispatch failed",
                            Some(format!("Task {id} did not start a run.")),
                            cx,
                        );
                    } else {
                        this.backend_toast(
                            ToastKind::Success,
                            "Task dispatched",
                            str_of(value, "threadId")
                                .map(|t| format!("Run started on thread {t}.")),
                            cx,
                        );
                    }
                } else {
                    this.backend_toast(
                        ToastKind::Error,
                        "Could not run task",
                        Some(exit_message(&exit)),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `scheduledTasks.delete` — two-stage arm/confirm like ACP delete.
    fn tasks_delete(&mut self, id: String, cx: &mut Context<Self>) {
        if self.backend_settings_state.tasks.confirm_delete.as_deref() != Some(id.as_str()) {
            self.backend_settings_state.tasks.confirm_delete = Some(id);
            cx.notify();
            return;
        }
        self.backend_settings_state.tasks.confirm_delete = None;
        let Some(rx) = self.send_query("scheduledTasks.delete", json!({ "id": id })) else {
            self.backend_toast(
                ToastKind::Error,
                "Could not delete task",
                Some("Not connected to a server.".to_string()),
                cx,
            );
            return;
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let _ = this.update(cx, |this, cx| {
                if exit["_tag"].as_str() == Some("Success") {
                    this.backend_settings_state
                        .tasks
                        .tasks
                        .retain(|t| t.id != id);
                    this.backend_toast(ToastKind::Success, "Scheduled task deleted", None, cx);
                } else {
                    this.backend_toast(
                        ToastKind::Error,
                        "Could not delete task",
                        Some(exit_message(&exit)),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Lazily create the new-task form fields + observers (envs.rs pattern).
    fn ensure_task_fields(&mut self, cx: &mut Context<Self>) {
        if self.backend_settings_state.tasks.title_field.is_some() {
            return;
        }
        let title_field = cx.new(|cx| TextField::new("Name (e.g. Nightly review)", cx));
        let prompt_field = cx.new(|cx| TextField::new("Prompt the task sends to a new thread", cx));
        let schedule_field = cx.new(|cx| {
            TextField::new(
                "30m · daily 09:30 · mon,wed 09:30 · once 2026-03-01 09:30 · webhook",
                cx,
            )
        });
        self.backend_settings_state
            .tasks
            .subs
            .push(cx.observe(&title_field, |this, field, cx| {
                this.backend_settings_state.tasks.draft_title = field_text(&field, cx);
                cx.notify();
            }));
        self.backend_settings_state.tasks.subs.push(cx.observe(
            &prompt_field,
            |this, field, cx| {
                this.backend_settings_state.tasks.draft_prompt = field_text(&field, cx);
                cx.notify();
            },
        ));
        self.backend_settings_state.tasks.subs.push(cx.observe(
            &schedule_field,
            |this, field, cx| {
                this.backend_settings_state.tasks.draft_schedule = field_text(&field, cx);
                cx.notify();
            },
        ));
        // Enter in any field submits the form.
        for field in [&title_field, &prompt_field, &schedule_field] {
            self.backend_settings_state.tasks.subs.push(cx.subscribe(
                field,
                |this, field, event: &PromptSubmitted, cx| {
                    this.submit_task_form(&field, &event.0, cx);
                },
            ));
        }
        let state = &mut self.backend_settings_state.tasks;
        state.title_field = Some(title_field);
        state.prompt_field = Some(prompt_field);
        state.schedule_field = Some(schedule_field);
    }

    /// "New task" opener: build fields on first use, clear drafts, focus name.
    fn open_task_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_task_fields(cx);
        let state = &mut self.backend_settings_state.tasks;
        state.form_open = true;
        state.form_error = None;
        state.draft_title.clear();
        state.draft_prompt.clear();
        state.draft_schedule.clear();
        for field in [
            state.title_field.clone(),
            state.prompt_field.clone(),
            state.schedule_field.clone(),
        ]
        .into_iter()
        .flatten()
        {
            field.update(cx, |field, cx| field.reset(window, cx));
        }
        if let Some(field) = self.backend_settings_state.tasks.title_field.clone() {
            field.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// Enter pressed in a task-form field (its text is already reset; the
    /// others are read via `field_text`).
    fn submit_task_form(
        &mut self,
        submitter: &Entity<TextField>,
        submitted: &SharedString,
        cx: &mut Context<Self>,
    ) {
        let (Some(title_f), Some(prompt_f), Some(schedule_f)) = (
            self.backend_settings_state.tasks.title_field.clone(),
            self.backend_settings_state.tasks.prompt_field.clone(),
            self.backend_settings_state.tasks.schedule_field.clone(),
        ) else {
            return;
        };
        let mut read = |f: &Entity<TextField>| {
            if submitter == f {
                submitted.to_string()
            } else {
                field_text(f, cx)
            }
        };
        let title = read(&title_f);
        let prompt = read(&prompt_f);
        let schedule = read(&schedule_f);
        self.tasks_upsert(title, prompt, schedule, None, cx);
    }

    /// "Create task" click — reads fields through the listener's `window`.
    fn add_task_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(title_f), Some(prompt_f), Some(schedule_f)) = (
            self.backend_settings_state.tasks.title_field.clone(),
            self.backend_settings_state.tasks.prompt_field.clone(),
            self.backend_settings_state.tasks.schedule_field.clone(),
        ) else {
            return;
        };
        let title = field_text_in(&title_f, window, cx);
        let prompt = field_text_in(&prompt_f, window, cx);
        let schedule = field_text_in(&schedule_f, window, cx);
        self.tasks_upsert(title, prompt, schedule, Some(window), cx);
    }

    /// `scheduledTasks.upsert` — validate, send, toast; the stream snapshot
    /// repaints the list. `window` is passed when fields should be reset on
    /// success (button path; the Enter path already cleared the submitter).
    fn tasks_upsert(
        &mut self,
        title: String,
        prompt: String,
        schedule: String,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let title = title.trim().to_string();
        let prompt = prompt.trim().to_string();
        if title.is_empty() || prompt.is_empty() {
            self.backend_settings_state.tasks.form_error =
                Some("A name and a prompt are required.".into());
            cx.notify();
            return;
        }
        let schedule = match parse_schedule(&schedule) {
            Ok(s) => s,
            Err(e) => {
                self.backend_settings_state.tasks.form_error = Some(e.into());
                cx.notify();
                return;
            }
        };
        let Some(project_id) = self.tasks_project_id() else {
            self.backend_settings_state.tasks.form_error =
                Some("No project on this server.".into());
            cx.notify();
            return;
        };
        if self.backend_settings_state.tasks.saving {
            return;
        }
        let payload = json!({
            "commandId": uuid::Uuid::new_v4().to_string(),
            "title": title,
            "prompt": prompt,
            "enabled": true,
            "schedule": schedule,
            "projectId": project_id,
            "workspaceStrategy": { "type": "root" },
            "modelSelection": self.model_selection(),
            "runtimeMode": self.composer_state.runtime_mode,
            "interactionMode": self.composer_state.interaction_mode,
            "createdBy": "user",
            "creationSource": "web",
        });
        let Some(rx) = self.send_query("scheduledTasks.upsert", payload) else {
            self.backend_settings_state.tasks.form_error =
                Some("Not connected to a server.".into());
            cx.notify();
            return;
        };
        self.backend_settings_state.tasks.saving = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.backend_settings_state.tasks;
                state.saving = false;
                if exit["_tag"].as_str() == Some("Success") {
                    state.form_open = false;
                    state.form_error = None;
                    this.backend_toast(ToastKind::Success, "Scheduled task saved", None, cx);
                } else {
                    state.form_error = Some(exit_message(&exit).into());
                }
                cx.notify();
            });
        })
        .detach();
        // Reset the fields on send so a failed save still shows the error.
        if let Some(window) = window {
            for field in [
                self.backend_settings_state.tasks.title_field.clone(),
                self.backend_settings_state.tasks.prompt_field.clone(),
                self.backend_settings_state.tasks.schedule_field.clone(),
            ]
            .into_iter()
            .flatten()
            {
                field.update(cx, |field, cx| field.reset(window, cx));
            }
        }
    }

    // ---------------------------------------------------------------------
    // Secrets
    // ---------------------------------------------------------------------

    /// Lazily create answer fields for pending `secret_request` items seen in
    /// the loaded thread timeline.
    fn secrets_ensure_fields(&mut self, cx: &mut Context<Self>) {
        let pending: Vec<String> = self
            .thread_items
            .iter()
            .filter(|i| {
                i.kind == "secret_request" && i.raw["secretStatus"].as_str() == Some("pending")
            })
            .map(|i| i.id.clone())
            .collect();
        for item_id in pending {
            if self
                .backend_settings_state
                .secrets
                .answer_fields
                .contains_key(&item_id)
            {
                continue;
            }
            let field = cx.new(|cx| TextField::new("Paste the secret value", cx));
            let key = item_id.clone();
            self.backend_settings_state.secrets.subs.push(cx.observe(
                &field,
                move |this, field, cx| {
                    let text = field_text(&field, cx);
                    this.backend_settings_state
                        .secrets
                        .draft_answers
                        .insert(key.clone(), text);
                    cx.notify();
                },
            ));
            self.backend_settings_state
                .secrets
                .answer_fields
                .insert(item_id, field);
        }
    }

    /// `secrets.answerRequest` — save/decline a pending secret prompt. The
    /// server tries `requestId`, `clientRequestId` and `turnItemId` keys.
    fn secrets_answer(
        &mut self,
        item_id: String,
        request_id: Option<String>,
        secret: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.backend_settings_state.secrets.answering.is_some() {
            return;
        }
        let answer = match &secret {
            Some(value) => json!({ "type": "save", "secret": value }),
            None => json!({ "type": "decline" }),
        };
        let mut payload = json!({ "turnItemId": item_id, "answer": answer });
        if let Some(request_id) = request_id {
            payload["requestId"] = json!(request_id);
        }
        let Some(rx) = self.send_query("secrets.answerRequest", payload) else {
            self.backend_toast(
                ToastKind::Error,
                "Could not answer secret request",
                Some("Not connected to a server.".to_string()),
                cx,
            );
            return;
        };
        self.backend_settings_state.secrets.answering = Some(item_id.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.backend_settings_state.secrets;
                state.answering = None;
                if exit["_tag"].as_str() == Some("Success") {
                    if exit["value"]["answered"].as_bool() == Some(true) {
                        state.answered.insert(item_id.clone());
                        state.answer_fields.remove(&item_id);
                        state.draft_answers.remove(&item_id);
                        this.backend_toast(
                            ToastKind::Success,
                            if secret.is_some() {
                                "Secret saved"
                            } else {
                                "Secret request declined"
                            },
                            None,
                            cx,
                        );
                    } else {
                        this.backend_toast(
                            ToastKind::Error,
                            "Secret request not pending",
                            Some("The server has no waiting prompt for this request.".to_string()),
                            cx,
                        );
                    }
                } else {
                    this.backend_toast(
                        ToastKind::Error,
                        "Could not answer secret request",
                        Some(exit_message(&exit)),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---------------------------------------------------------------------
    // Diagnostics
    // ---------------------------------------------------------------------

    /// Fire the three stats queries; one spawn folds all replies into state.
    fn diagnostics_load(&mut self, cx: &mut Context<Self>) {
        if self.backend_settings_state.diagnostics.loading {
            return;
        }
        let (Some(p), Some(h), Some(d)) = (
            self.send_query("server.getProcessDiagnostics", json!({})),
            self.send_query("server.getHostResources", json!({})),
            self.send_query("server.getDatabaseStorage", json!({})),
        ) else {
            self.backend_settings_state.diagnostics.error =
                Some("Not connected to a server.".to_string());
            cx.notify();
            return;
        };
        self.backend_settings_state.diagnostics.loading = true;
        self.backend_settings_state.diagnostics.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let p = p.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed."}}})
            });
            let h = h.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed."}}})
            });
            let d = d.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed."}}})
            });
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.backend_settings_state.diagnostics;
                state.loading = false;
                state.loaded = true;
                let mut error: Option<String> = None;
                for (slot, exit) in [
                    (&mut state.process, p),
                    (&mut state.host, h),
                    (&mut state.db, d),
                ] {
                    if exit["_tag"].as_str() == Some("Success") {
                        *slot = Some(exit["value"].clone());
                    } else if error.is_none() {
                        error = Some(exit_message(&exit));
                    }
                }
                state.error = error;
                cx.notify();
            });
        })
        .detach();
    }

    /// `server.runStorageCleanup` → report; toast the freed bytes.
    fn diagnostics_cleanup(&mut self, cx: &mut Context<Self>) {
        if self.backend_settings_state.diagnostics.cleaning {
            return;
        }
        let Some(rx) = self.send_query("server.runStorageCleanup", json!({})) else {
            self.backend_toast(
                ToastKind::Error,
                "Storage cleanup failed",
                Some("Not connected to a server.".to_string()),
                cx,
            );
            return;
        };
        self.backend_settings_state.diagnostics.cleaning = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed."}}})
            });
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.backend_settings_state.diagnostics;
                state.cleaning = false;
                if exit["_tag"].as_str() == Some("Success") {
                    let report = &exit["value"];
                    let freed = report["bytesFreed"].as_u64().unwrap_or(0);
                    let removed = report["counts"]["removed"].as_u64().unwrap_or(0);
                    let failed = report["counts"]["failed"].as_u64().unwrap_or(0);
                    state.cleaned = Some(format!(
                        "{} freed · {} removed · {} failed",
                        format_bytes(freed),
                        removed,
                        failed
                    ));
                    this.backend_toast(
                        ToastKind::Success,
                        "Storage cleanup finished",
                        Some(format!("{} freed.", format_bytes(freed))),
                        cx,
                    );
                    // Re-read the storage stats after compaction.
                    this.diagnostics_load(cx);
                } else {
                    this.backend_toast(
                        ToastKind::Error,
                        "Storage cleanup failed",
                        Some(exit_message(&exit)),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `server.reclaimDatabaseSpace` → VACUUM; toast ok/err.
    fn diagnostics_reclaim(&mut self, cx: &mut Context<Self>) {
        if self.backend_settings_state.diagnostics.reclaiming {
            return;
        }
        let Some(rx) = self.send_query("server.reclaimDatabaseSpace", json!({})) else {
            self.backend_toast(
                ToastKind::Error,
                "Reclaim failed",
                Some("Not connected to a server.".to_string()),
                cx,
            );
            return;
        };
        self.backend_settings_state.diagnostics.reclaiming = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed."}}})
            });
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.backend_settings_state.diagnostics;
                state.reclaiming = false;
                if exit["_tag"].as_str() == Some("Success") {
                    this.backend_toast(ToastKind::Success, "Database space reclaimed", None, cx);
                    this.diagnostics_load(cx);
                } else {
                    this.backend_toast(
                        ToastKind::Error,
                        "Could not reclaim database space",
                        Some(exit_message(&exit)),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---------------------------------------------------------------------
    // Usage
    // ---------------------------------------------------------------------

    /// `server.getUsageSummary` — day-bucketed tokens over the period.
    fn usage_load(&mut self, cx: &mut Context<Self>) {
        if self.backend_settings_state.usage.loading {
            return;
        }
        let now = unix_now();
        let (since, until) = match self.backend_settings_state.usage.period {
            UsagePeriod::Today => (now, now),
            UsagePeriod::Last7 => (now - 6 * 86_400, now),
            UsagePeriod::Last30 => (now - 29 * 86_400, now),
        };
        let payload = json!({
            "sinceDay": day_string(since),
            "untilDay": day_string(until),
            "timeZone": "UTC",
            "resolution": "day",
        });
        let Some(rx) = self.send_query("server.getUsageSummary", payload) else {
            self.backend_settings_state.usage.error =
                Some("Not connected to a server.".to_string());
            cx.notify();
            return;
        };
        self.backend_settings_state.usage.loading = true;
        self.backend_settings_state.usage.error = None;
        let generation = self.backend_settings_state.usage.generation;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed."}}})
            });
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.backend_settings_state.usage;
                if state.generation != generation {
                    return;
                }
                state.loading = false;
                state.loaded = true;
                if exit["_tag"].as_str() == Some("Success") {
                    let value = &exit["value"];
                    state.buckets = value["buckets"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|b| {
                                    let totals = &b["totals"];
                                    let day = str_of(b, "day").unwrap_or("").to_string();
                                    let when = str_of(b, "hourStart")
                                        .map(|h| format!("{day} {h}"))
                                        .unwrap_or(day);
                                    if when.is_empty() {
                                        return None;
                                    }
                                    Some(UsageBucket {
                                        when,
                                        provider: str_of(b, "provider").unwrap_or("?").to_string(),
                                        model: str_of(b, "model").unwrap_or("?").to_string(),
                                        input: totals["uncachedInputTokens"].as_u64().unwrap_or(0),
                                        cache_read: totals["cachedInputTokens"]
                                            .as_u64()
                                            .unwrap_or(0),
                                        cache_write: totals["cacheCreationTokens"]
                                            .as_u64()
                                            .unwrap_or(0),
                                        output: totals["outputTokens"].as_u64().unwrap_or(0),
                                        reasoning: totals["reasoningTokens"].as_u64().unwrap_or(0),
                                        records: b["records"].as_u64().unwrap_or(0),
                                        sessions: b["sessions"].as_u64().unwrap_or(0),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    state.pricing_status = str_of(&value["pricing"], "status").map(str::to_owned);
                    state.read_at = str_of(value, "readAt").map(str::to_owned);
                } else {
                    state.error = Some(exit_message(&exit));
                    state.buckets.clear();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `server.refreshUsageRates` → toast the pricing status.
    fn usage_refresh_rates(&mut self, cx: &mut Context<Self>) {
        if self.backend_settings_state.usage.refreshing {
            return;
        }
        let Some(rx) = self.send_query("server.refreshUsageRates", json!({})) else {
            self.backend_toast(
                ToastKind::Error,
                "Could not refresh usage rates",
                Some("Not connected to a server.".to_string()),
                cx,
            );
            return;
        };
        self.backend_settings_state.usage.refreshing = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed."}}})
            });
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.backend_settings_state.usage;
                state.refreshing = false;
                if exit["_tag"].as_str() == Some("Success") {
                    let status = str_of(&exit["value"], "status")
                        .unwrap_or("unknown")
                        .to_string();
                    state.pricing_status = Some(status.clone());
                    this.backend_toast(
                        ToastKind::Success,
                        "Usage rates refreshed",
                        Some(format!("Pricing status: {status}.")),
                        cx,
                    );
                } else {
                    this.backend_toast(
                        ToastKind::Error,
                        "Could not refresh usage rates",
                        Some(exit_message(&exit)),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---------------------------------------------------------------------
    // Sections
    // ---------------------------------------------------------------------

    pub(crate) fn render_tasks_section(&self, cx: &mut Context<Self>) -> Div {
        // First render on this section opens the subscribe stream.
        if self.backend_settings_state.tasks.stream_id.is_none() && self.commands.is_some() {
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| this.tasks_ensure_stream(cx));
            })
            .detach();
        }

        let state = &self.backend_settings_state.tasks;
        let connected = self.commands.is_some();
        let mut section = div().flex().flex_col().gap_1();

        // ── New-task mini form ──────────────────────────────────────
        if state.form_open {
            let mut form = div().flex().flex_col().gap_2().pb_2();
            for (caption, field) in [
                ("Name", state.title_field.clone()),
                ("Prompt", state.prompt_field.clone()),
                ("Schedule", state.schedule_field.clone()),
            ] {
                if let Some(field) = field {
                    form = form.child(labeled_field(caption, &field));
                }
            }
            // Project picker chip + in-flow menu (acp.rs pattern).
            let project_title = self
                .state
                .projects
                .iter()
                .find(|p| Some(p.id.as_ref()) == self.tasks_project_id().as_deref())
                .map(|p| p.name.to_string())
                .unwrap_or_else(|| "Select project".to_string());
            let picker_open = state.project_menu_open;
            form = form.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("Project"),
                    )
                    .child(
                        div()
                            .id("task-project-chip")
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme::input())
                            .bg(theme::field_surface())
                            .text_xs()
                            .cursor_pointer()
                            .text_color(theme::fg())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    this.backend_settings_state.tasks.project_menu_open =
                                        !this.backend_settings_state.tasks.project_menu_open;
                                    cx.notify();
                                }),
                            )
                            .child(div().max_w(px(280.)).truncate().child(project_title))
                            .child(div().text_color(theme::fg_muted()).child("▾")),
                    ),
            );
            if picker_open {
                let mut menu = div()
                    .flex()
                    .flex_col()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::raised())
                    .py_1();
                for (index, project) in self.state.projects.iter().enumerate() {
                    let pid = project.id.to_string();
                    let active = self.tasks_project_id().as_deref() == Some(pid.as_str());
                    menu = menu.child(
                        div()
                            .id(("task-project", index))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1p5()
                            .text_xs()
                            .cursor_pointer()
                            .when(active, |s| s.bg(theme::selected()))
                            .when(!active, |s| s.hover(|s| s.bg(theme::hover())))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    let state = &mut this.backend_settings_state.tasks;
                                    state.project_id = Some(pid.clone());
                                    state.project_menu_open = false;
                                    cx.notify();
                                }),
                            )
                            .child(
                                div()
                                    .w(px(14.))
                                    .flex_none()
                                    .text_color(theme::primary())
                                    .child(if active { "✓" } else { "" }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme::fg())
                                    .child(project.name.clone()),
                            ),
                    );
                }
                form = form.child(menu);
            }

            let can_save = connected
                && !state.saving
                && !state.draft_title.trim().is_empty()
                && !state.draft_prompt.trim().is_empty()
                && !state.draft_schedule.trim().is_empty();
            form = form.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pt_1()
                    .child(
                        row_button(
                            "task-create",
                            if state.saving {
                                "Saving…"
                            } else {
                                "Create task"
                            },
                            can_save,
                        )
                        .when(can_save, |s| {
                            s.on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, window, cx| {
                                    this.add_task_click(window, cx)
                                }),
                            )
                        }),
                    )
                    .child(row_button("task-cancel", "Cancel", true).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            let state = &mut this.backend_settings_state.tasks;
                            state.form_open = false;
                            state.form_error = None;
                            cx.notify();
                        }),
                    )),
            );
            if let Some(error) = &state.form_error {
                form = form.child(
                    div()
                        .text_xs()
                        .text_color(rgb(0xf87171))
                        .child(error.clone()),
                );
            }
            form = form.child(hint(
                "Schedule: 30m / 2h / 1d / 1w interval · daily HH:MM · mon,wed HH:MM · once YYYY-MM-DD HH:MM · webhook.",
            ));
            section = section.child(form);
        } else {
            let add_enabled = connected && !self.state.projects.is_empty();
            section = section.child(div().flex().child(
                row_button("task-new", "+ New scheduled task", add_enabled).when(
                    add_enabled,
                    |s| {
                        s.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, window, cx| this.open_task_form(window, cx)),
                        )
                    },
                ),
            ));
        }

        if let Some(error) = &state.error {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(rgb(0xf87171))
                    .child(format!("Could not load scheduled tasks: {error}")),
            );
        }

        // ── Task rows ───────────────────────────────────────────────
        if !state.tasks.is_empty() {
            let mut list = div()
                .flex()
                .flex_col()
                .pt_2()
                .border_t_1()
                .border_color(theme::border());
            for (index, task) in state.tasks.iter().enumerate() {
                let id = task.id.clone();
                let enabled = task.enabled;
                let running_this = state.running.as_deref() == Some(task.id.as_str());
                let toggling_this = state.toggling.as_deref() == Some(task.id.as_str());
                let armed = state.confirm_delete.as_deref() == Some(task.id.as_str());
                let project_name = self
                    .state
                    .projects
                    .iter()
                    .find(|p| p.id.as_ref() == task.project_id)
                    .map(|p| p.name.to_string())
                    .unwrap_or_else(|| task.project_id.clone());

                // Line 1: title · project · schedule · prompt preview.
                let mut left = div()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme::fg())
                                    .truncate()
                                    .child(task.title.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(10.))
                                    .text_color(theme::fg_muted())
                                    .child(task.schedule.clone()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(10.))
                                    .text_color(theme::fg_muted())
                                    .child(project_name),
                            ),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(10.))
                            .text_color(theme::fg_muted())
                            .child(task.prompt.clone()),
                    );

                // Line 2: run state.
                let mut meta = String::new();
                match task.last_status.as_str() {
                    "never" => meta.push_str("never ran"),
                    s => {
                        meta.push_str(s);
                        if task.run_count > 0 {
                            meta.push_str(&format!(" · {} runs", task.run_count));
                        }
                    }
                }
                if let Some(next) = &task.next_run_at {
                    meta.push_str(&format!(" · next {}", iso_short(next)));
                }
                if let Some(err) = &task.last_error {
                    meta.push_str(&format!(" · {err}"));
                }
                if let Some(reason) = &task.blocked {
                    meta.push_str(&format!(" · blocked: {reason}"));
                }
                left = left.child(
                    div()
                        .truncate()
                        .text_size(px(10.))
                        .text_color(theme::fg_muted())
                        .child(meta),
                );
                if let Some(path) = &task.webhook_path {
                    left = left.child(
                        div()
                            .truncate()
                            .font_family("monospace")
                            .text_size(px(10.))
                            .text_color(theme::fg_muted())
                            .child(path.clone()),
                    );
                }

                // Buttons: enable toggle / run now / delete.
                let mut buttons = div().flex().flex_none().items_center().gap_1();

                let toggle_label = if toggling_this {
                    "…"
                } else if enabled {
                    "Disable"
                } else {
                    "Enable"
                };
                let toggle_enabled =
                    connected && state.toggling.is_none() && task.blocked.is_none();
                let toggle_id = id.clone();
                buttons = buttons.child(
                    div()
                        .id(("task-toggle", index))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .when(toggle_enabled, |s| {
                            s.cursor_pointer()
                                .text_color(theme::fg())
                                .hover(|s| s.bg(theme::hover()))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _ev, _window, cx| {
                                        this.tasks_set_enabled(toggle_id.clone(), !enabled, cx);
                                    }),
                                )
                        })
                        .when(!toggle_enabled, |s| {
                            s.text_color(theme::fg_muted()).opacity(0.7)
                        })
                        .child(toggle_label),
                );

                let run_enabled = connected
                    && state.running.is_none()
                    && task.webhook_path.is_none()
                    && task.blocked.is_none();
                let run_id = id.clone();
                buttons = buttons.child(
                    div()
                        .id(("task-run", index))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .when(run_enabled, |s| {
                            s.cursor_pointer()
                                .text_color(theme::fg())
                                .hover(|s| s.bg(theme::hover()))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _ev, _window, cx| {
                                        this.tasks_run_now(run_id.clone(), cx)
                                    }),
                                )
                        })
                        .when(!run_enabled, |s| {
                            s.text_color(theme::fg_muted()).opacity(0.7)
                        })
                        .child(if running_this {
                            "Running…"
                        } else {
                            "Run now"
                        }),
                );

                let delete_label = if armed { "Confirm" } else { "Delete" };
                let delete_id = id.clone();
                buttons = buttons.child(
                    div()
                        .id(("task-delete", index))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .when(armed, |s| {
                            s.bg(rgb(0x7a2e2e))
                                .text_color(gpui::white())
                                .hover(|s| s.bg(rgb(0x9a3a3a)))
                        })
                        .when(!armed, |s| {
                            s.text_color(theme::fg_muted())
                                .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.tasks_delete(delete_id.clone(), cx)
                            }),
                        )
                        .child(delete_label),
                );

                list = list.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .py_2()
                        .when(index > 0, |s| s.border_t_1().border_color(theme::border()))
                        .child(left)
                        .child(buttons),
                );
            }
            section = section.child(list);
        } else if state.loaded {
            section = section.child(hint("No scheduled tasks on this server."));
        } else if !state.loaded && state.error.is_none() {
            section = section.child(hint("Loading scheduled tasks…"));
        }

        section
    }

    pub(crate) fn render_secrets_section(&self, cx: &mut Context<Self>) -> Div {
        // Pending `secret_request` turn items of the loaded thread timeline
        // surface here; answer fields are created lazily under `&mut`.
        let needs_fields = self.thread_items.iter().any(|i| {
            i.kind == "secret_request"
                && i.raw["secretStatus"].as_str() == Some("pending")
                && !self
                    .backend_settings_state
                    .secrets
                    .answer_fields
                    .contains_key(&i.id)
        });
        if needs_fields {
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| this.secrets_ensure_fields(cx));
            })
            .detach();
        }

        let state = &self.backend_settings_state.secrets;
        let connected = self.commands.is_some();
        let pending: Vec<_> = self
            .thread_items
            .iter()
            .filter(|i| {
                i.kind == "secret_request"
                    && i.raw["secretStatus"].as_str() == Some("pending")
                    && !state.answered.contains(&i.id)
            })
            .collect();

        let mut section = div().flex().flex_col().gap_1();

        if pending.is_empty() {
            section = section
                .child(hint("No pending secret requests."))
                .child(hint(
                    "Secrets are managed per-provider. When an agent asks for a secret during a run, its request appears here.",
                ));
            return section;
        }

        section = section.child(group_label("Pending requests"));
        let mut list = div()
            .flex()
            .flex_col()
            .border_t_1()
            .border_b_1()
            .border_color(theme::border());
        for (index, item) in pending.iter().enumerate() {
            let item_id = item.id.clone();
            let request_id = item.raw["requestId"].as_str().map(str::to_owned);
            let label = item.raw["label"].as_str().unwrap_or("Secret request");
            let reason = item.raw["reason"].as_str().unwrap_or("");
            let url = item.raw["url"].as_str();
            let delivery_file = item.raw["delivery"].as_str() == Some("file");
            let answering_this = state.answering.as_deref() == Some(item_id.as_str());

            let mut left = div().min_w_0().flex_1().child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(theme::fg())
                    .child(label.to_string()),
            );
            if !reason.is_empty() {
                left = left.child(
                    div()
                        .truncate()
                        .text_size(px(10.))
                        .text_color(theme::fg_muted())
                        .child(reason.to_string()),
                );
            }
            if let Some(url) = url {
                left = left.child(
                    div()
                        .truncate()
                        .font_family("monospace")
                        .text_size(px(10.))
                        .text_color(theme::fg_muted())
                        .child(url.to_string()),
                );
            }
            if delivery_file {
                left = left.child(
                    div()
                        .text_size(px(10.))
                        .text_color(theme::fg_muted())
                        .child("Delivered as an owner-only file the agent can read."),
                );
            }

            let mut right = div().flex().flex_col().flex_none().gap_1();
            if let Some(field) = state.answer_fields.get(&item_id) {
                right = right.child(
                    div()
                        .w(px(200.))
                        .px_2()
                        .py_1()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme::input())
                        .bg(theme::field_surface())
                        .text_xs()
                        .text_color(theme::fg())
                        .child(field.clone()),
                );
            }
            let save_ok = connected
                && state.answering.is_none()
                && !state
                    .draft_answers
                    .get(&item_id)
                    .map(|s| s.trim().is_empty())
                    .unwrap_or(true);
            let save_item = item_id.clone();
            let save_req = request_id.clone();
            let decline_item = item_id.clone();
            let decline_req = request_id;
            right = right.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .id(("secret-save", index))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .when(save_ok, |s| {
                                s.cursor_pointer()
                                    .text_color(theme::fg())
                                    .hover(|s| s.bg(theme::hover()))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _ev, window, cx| {
                                            let field = this
                                                .backend_settings_state
                                                .secrets
                                                .answer_fields
                                                .get(&save_item)
                                                .cloned();
                                            let secret = field
                                                .map(|f| field_text_in(&f, window, cx))
                                                .unwrap_or_else(|| {
                                                    this.backend_settings_state
                                                        .secrets
                                                        .draft_answers
                                                        .get(&save_item)
                                                        .cloned()
                                                        .unwrap_or_default()
                                                });
                                            this.secrets_answer(
                                                save_item.clone(),
                                                save_req.clone(),
                                                Some(secret),
                                                cx,
                                            );
                                        }),
                                    )
                            })
                            .when(!save_ok, |s| s.text_color(theme::fg_muted()).opacity(0.7))
                            .child(if answering_this { "…" } else { "Save" }),
                    )
                    .child(
                        div()
                            .id(("secret-decline", index))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .text_color(theme::fg_muted())
                            .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    this.secrets_answer(
                                        decline_item.clone(),
                                        decline_req.clone(),
                                        None,
                                        cx,
                                    );
                                }),
                            )
                            .child("Decline"),
                    ),
            );

            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .py_2()
                    .when(index > 0, |s| s.border_t_1().border_color(theme::border()))
                    .child(left)
                    .child(right),
            );
        }
        section.child(list)
    }

    pub(crate) fn render_diagnostics_section(&self, cx: &mut Context<Self>) -> Div {
        // First render loads the stats; a Refresh button re-fetches.
        let diag = &self.backend_settings_state.diagnostics;
        if !diag.loaded && !diag.loading && self.commands.is_some() {
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| this.diagnostics_load(cx));
            })
            .detach();
        }

        let state = &self.backend_settings_state.diagnostics;
        let connected = self.commands.is_some();
        let mut section = div().flex().flex_col().gap_1();

        // ── Server process ──────────────────────────────────────────
        section = section.child(group_label("Server process"));
        if let Some(p) = &state.process {
            let proc0 = &p["processes"][0];
            let rows: Vec<(&'static str, String)> = vec![
                (
                    "PID",
                    proc0["pid"]
                        .as_i64()
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                ),
                (
                    "Parent PID",
                    proc0["ppid"]
                        .as_i64()
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                ),
                (
                    "Uptime",
                    proc0["elapsed"].as_str().unwrap_or("").to_string(),
                ),
                (
                    "CPU",
                    proc0["cpuPercent"]
                        .as_f64()
                        .map(|c| format!("{c:.1}%"))
                        .unwrap_or_default(),
                ),
                (
                    "Memory (RSS)",
                    proc0["rssBytes"]
                        .as_u64()
                        .map(format_bytes)
                        .unwrap_or_default(),
                ),
            ];
            for (label, value) in rows {
                section = section.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .py_0p5()
                        .text_xs()
                        .child(
                            div()
                                .w(px(110.))
                                .flex_none()
                                .text_color(theme::fg_muted())
                                .child(label),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family("monospace")
                                .text_color(theme::fg())
                                .child(value),
                        ),
                );
            }
            if let Some(command) = proc0["command"].as_str() {
                section = section.child(
                    div()
                        .truncate()
                        .font_family("monospace")
                        .text_size(px(10.))
                        .text_color(theme::fg_muted())
                        .child(command.to_string()),
                );
            }
        } else {
            section = section.child(hint("No process diagnostics yet."));
        }

        // ── Host resources ──────────────────────────────────────────
        section = section.child(group_label("Host"));
        if let Some(h) = &state.host {
            let rows: Vec<(&'static str, String)> = vec![
                (
                    "CPU utilization",
                    h["cpuUtilization"]
                        .as_f64()
                        .map(|u| format!("{:.1}%", u * 100.0))
                        .unwrap_or_else(|| "n/a".to_string()),
                ),
                (
                    "CPU count",
                    h["cpuCount"]
                        .as_u64()
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                ),
                (
                    "Memory",
                    match (
                        h["availableMemoryBytes"].as_u64(),
                        h["totalMemoryBytes"].as_u64(),
                    ) {
                        (Some(avail), Some(total)) => {
                            format!(
                                "{} available of {}",
                                format_bytes(avail),
                                format_bytes(total)
                            )
                        }
                        _ => "n/a".to_string(),
                    },
                ),
            ];
            for (label, value) in rows {
                section = section.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .py_0p5()
                        .text_xs()
                        .child(
                            div()
                                .w(px(110.))
                                .flex_none()
                                .text_color(theme::fg_muted())
                                .child(label),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family("monospace")
                                .text_color(theme::fg())
                                .child(value),
                        ),
                );
            }
        } else {
            section = section.child(hint("No host metrics yet."));
        }

        // ── Database storage ────────────────────────────────────────
        section = section.child(group_label("Database"));
        match &state.db {
            Some(db) if !db.is_null() => {
                let rows: Vec<(&'static str, String)> = vec![
                    (
                        "Size",
                        db["databaseBytes"]
                            .as_u64()
                            .map(format_bytes)
                            .unwrap_or_default(),
                    ),
                    (
                        "Reclaimable",
                        db["reclaimableBytes"]
                            .as_u64()
                            .map(format_bytes)
                            .unwrap_or_default(),
                    ),
                    (
                        "Auto-vacuum",
                        if db["incrementalVacuumEnabled"].as_bool() == Some(true) {
                            "incremental".to_string()
                        } else {
                            "off".to_string()
                        },
                    ),
                ];
                for (label, value) in rows {
                    section = section.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .py_0p5()
                            .text_xs()
                            .child(
                                div()
                                    .w(px(110.))
                                    .flex_none()
                                    .text_color(theme::fg_muted())
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family("monospace")
                                    .text_color(theme::fg())
                                    .child(value),
                            ),
                    );
                }
            }
            _ => {
                section = section.child(hint("No database at this server home."));
            }
        }

        if let Some(error) = &state.error {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(rgb(0xf87171))
                    .child(format!("Diagnostics: {error}")),
            );
        }
        if let Some(cleaned) = &state.cleaned {
            section = section.child(hint(format!("Last cleanup: {cleaned}.")));
        }

        // ── Actions ─────────────────────────────────────────────────
        let refresh_enabled = connected && !state.loading;
        let cleanup_enabled = connected && !state.cleaning;
        let reclaim_enabled = connected && !state.reclaiming;
        section = section.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .pt_3()
                .child(
                    row_button(
                        "diag-refresh",
                        if state.loading {
                            "Loading…"
                        } else {
                            "Refresh"
                        },
                        refresh_enabled,
                    )
                    .when(refresh_enabled, |s| {
                        s.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _window, cx| this.diagnostics_load(cx)),
                        )
                    }),
                )
                .child(
                    row_button(
                        "diag-cleanup",
                        if state.cleaning {
                            "Cleaning…"
                        } else {
                            "Run storage cleanup"
                        },
                        cleanup_enabled,
                    )
                    .when(cleanup_enabled, |s| {
                        s.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _window, cx| this.diagnostics_cleanup(cx)),
                        )
                    }),
                )
                .child(
                    row_button(
                        "diag-reclaim",
                        if state.reclaiming {
                            "Reclaiming…"
                        } else {
                            "Reclaim database space"
                        },
                        reclaim_enabled,
                    )
                    .when(reclaim_enabled, |s| {
                        s.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _window, cx| this.diagnostics_reclaim(cx)),
                        )
                    }),
                ),
        );

        section
    }

    pub(crate) fn render_usage_section(&self, cx: &mut Context<Self>) -> Div {
        // First render loads the summary; period chips re-fetch.
        let usage = &self.backend_settings_state.usage;
        if !usage.loaded && !usage.loading && self.commands.is_some() {
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| this.usage_load(cx));
            })
            .detach();
        }

        let state = &self.backend_settings_state.usage;
        let connected = self.commands.is_some();
        let mut section = div().flex().flex_col().gap_1();

        // ── Period selector + actions ───────────────────────────────
        let mut controls = div().flex().items_center().gap_2();
        for (index, period) in [UsagePeriod::Today, UsagePeriod::Last7, UsagePeriod::Last30]
            .iter()
            .copied()
            .enumerate()
        {
            let active = state.period == period;
            controls = controls.child(
                div()
                    .id(("usage-period", index))
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .when(active, |s| s.bg(theme::selected()).text_color(theme::fg()))
                    .when(!active, |s| {
                        s.text_color(theme::fg_muted())
                            .hover(|s| s.bg(theme::hover()))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            let state = &mut this.backend_settings_state.usage;
                            if state.period != period {
                                state.period = period;
                                state.generation = state.generation.wrapping_add(1);
                                this.usage_load(cx);
                            }
                        }),
                    )
                    .child(period.label()),
            );
        }
        controls = controls.child(div().flex_1());
        let refresh_enabled = connected && !state.loading;
        controls = controls.child(
            row_button(
                "usage-refresh",
                if state.loading {
                    "Loading…"
                } else {
                    "Refresh"
                },
                refresh_enabled,
            )
            .when(refresh_enabled, |s| {
                s.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _window, cx| this.usage_load(cx)),
                )
            }),
        );
        let rates_enabled = connected && !state.refreshing;
        controls = controls.child(
            row_button(
                "usage-rates",
                if state.refreshing {
                    "Refreshing…"
                } else {
                    "Refresh rates"
                },
                rates_enabled,
            )
            .when(rates_enabled, |s| {
                s.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _window, cx| this.usage_refresh_rates(cx)),
                )
            }),
        );
        section = section.child(controls);

        if let Some(error) = &state.error {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(rgb(0xf87171))
                    .child(format!("Usage summary: {error}")),
            );
        }

        // ── Bucket table ────────────────────────────────────────────
        if !state.buckets.is_empty() {
            // Header
            let head_cell = |text: &'static str, width: f32| {
                div()
                    .w(px(width))
                    .flex_none()
                    .text_size(px(10.))
                    .text_color(theme::fg_muted())
                    .child(text)
            };
            section = section.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pt_2()
                    .pb_1()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(head_cell("Day", 150.))
                    .child(head_cell("Provider · model", 130.))
                    .child(div().flex_1().min_w_0().child(head_cell("Tokens", 80.))),
            );

            let mut list = div().flex().flex_col();
            for (index, bucket) in state.buckets.iter().enumerate() {
                let tokens = format!(
                    "in {} · cache {}+{} · out {}{}",
                    format_count(bucket.input),
                    format_count(bucket.cache_read),
                    format_count(bucket.cache_write),
                    format_count(bucket.output),
                    if bucket.reasoning > 0 {
                        format!(" · reasoning {}", format_count(bucket.reasoning))
                    } else {
                        String::new()
                    },
                );
                list = list.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .py_1p5()
                        .when(index > 0, |s| s.border_t_1().border_color(theme::border()))
                        .child(
                            div()
                                .w(px(150.))
                                .flex_none()
                                .truncate()
                                .text_xs()
                                .font_family("monospace")
                                .text_color(theme::fg())
                                .child(bucket.when.clone()),
                        )
                        .child(
                            div()
                                .w(px(130.))
                                .flex_none()
                                .truncate()
                                .text_xs()
                                .text_color(theme::fg())
                                .child(format!("{} · {}", bucket.provider, bucket.model)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .font_family("monospace")
                                .text_color(theme::fg())
                                .child(tokens),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(10.))
                                .text_color(theme::fg_muted())
                                .child(format!(
                                    "{} rec · {} sess",
                                    bucket.records, bucket.sessions
                                )),
                        ),
                );
            }
            section = section.child(list);
        } else if state.loaded && !state.loading && state.error.is_none() {
            section = section.child(hint("No usage recorded yet."));
        } else if state.loading {
            section = section.child(hint("Loading usage summary…"));
        }

        // ── Footer: pricing status + read stamp ─────────────────────
        let mut footer = String::new();
        if let Some(status) = &state.pricing_status {
            footer.push_str(&format!("Pricing: {status}"));
        }
        if let Some(read_at) = &state.read_at {
            if !footer.is_empty() {
                footer.push_str(" · ");
            }
            footer.push_str(&format!("read {}", iso_short(read_at)));
        }
        if !footer.is_empty() {
            section = section.child(hint(footer));
        }

        section
    }
}
