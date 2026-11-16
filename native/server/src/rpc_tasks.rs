//! `scheduledTasks.*`, `agentSessions.*`, `projectClone.*` and the
//! `subscribeProjectClones` windowed stream.
//!
//! Scheduled tasks persist to `<home>/scheduled-tasks.json` (atomic
//! temp+rename writes); a module-level broadcast channel wakes subscribers on
//! every mutation. `scheduledTasks.runNow` replays the `apply_and_publish`
//! flow from `handlers.rs` — `handlers::apply_and_publish` is private, so the
//! ~10 lines are duplicated here.
//!
//! `projectClone.*` runs `git clone --progress` in a spawned task, parses the
//! stderr progress stream the way `apps/server/src/project/gitCloneProgress.ts`
//! does, and publishes full-snapshot lists to `subscribeProjectClones`
//! subscribers. A finished clone creates the project through the store's
//! `project.create` apply path so `project.created` events reach shell
//! subscribers.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex as StdMutex, OnceLock};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};
use time::format_description::FormatItem;
use time::{OffsetDateTime, Time};
use tokio::io::AsyncReadExt;
use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;

use crate::domain;
use crate::handlers::RpcResult;
use crate::provider;
use crate::ws_rpc::{OutTx, Window};
use crate::AppState;

const ISO_FORMAT: &[FormatItem<'static>] = time::macros::format_description!(
    "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
);

fn now_ms() -> i64 {
    let now = OffsetDateTime::now_utc();
    now.unix_timestamp() * 1000 + now.millisecond() as i64
}

fn ms_to_iso(ms: i64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .ok()?
        .format(ISO_FORMAT)
        .ok()
}

fn rpc_not_implemented(tag: &str) -> RpcResult {
    RpcResult::err(json!({
        "_tag": "RpcClientError",
        "message": format!("not implemented: {tag}"),
    }))
}

/// Field getter with dotted-path support (`"schedule.type"`).
fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    let mut cur = v;
    for part in key.split('.') {
        cur = cur.get(part)?;
    }
    cur.as_str()
}



// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

pub async fn handle(state: Arc<AppState>, tag: &str, payload: Value) -> RpcResult {
    match tag {
        "scheduledTasks.upsert" => tasks_upsert(state, payload).await,
        "scheduledTasks.setEnabled" => tasks_set_enabled(state, payload).await,
        "scheduledTasks.delete" => tasks_delete(state, payload).await,
        "scheduledTasks.runNow" => tasks_run_now(state, payload).await,
        "scheduledTasks.rotateWebhookToken" => tasks_rotate_webhook_token(state, payload).await,
        "scheduledTasks.listWebhookDeliveries" => tasks_list_deliveries(state, payload).await,
        "scheduledTasks.getWebhookDelivery" => tasks_get_delivery(state, payload).await,
        "agentSessions.scan" => agent_sessions_scan(state).await,
        "agentSessions.import" => agent_sessions_import(state, payload).await,
        "projectClone.start" => clone_start(state, payload).await,
        "projectClone.cancel" => clone_cancel(payload).await,
        "projectClone.retry" => clone_retry(state, payload).await,
        other => rpc_not_implemented(other),
    }
}

/// Claim a subscription stream tag; returns true if this module handled it.
pub async fn handle_subscription(
    state: Arc<AppState>,
    out: OutTx,
    request_id: u64,
    tag: &str,
    _payload: Value,
    window: Arc<Window>,
) -> bool {
    match tag {
        "scheduledTasks.subscribe" => {
            scheduled_tasks_stream(state, out, request_id, window).await;
            true
        }
        "subscribeProjectClones" => {
            project_clones_stream(out, request_id, window).await;
            true
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Scheduled tasks: JSON file store
// ---------------------------------------------------------------------------

/// The whole persisted document: wire-shaped tasks plus the webhook secrets
/// that must never leave the server, and per-task delivery logs.
fn tasks_file(state: &AppState) -> PathBuf {
    state.home_dir.join("scheduled-tasks.json")
}

fn empty_tasks_doc() -> Value {
    json!({ "tasks": [], "credentials": {}, "deliveries": {} })
}

fn load_tasks_doc(path: &Path) -> Value {
    let mut doc = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| empty_tasks_doc()),
        Err(_) => empty_tasks_doc(),
    };
    // Normalize a hand-edited or truncated file before positional access.
    if !doc["tasks"].is_array() {
        doc["tasks"] = json!([]);
    }
    if !doc["credentials"].is_object() {
        doc["credentials"] = json!({});
    }
    if !doc["deliveries"].is_object() {
        doc["deliveries"] = json!({});
    }
    doc
}

fn save_tasks_doc(path: &Path, doc: &Value) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(doc).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))
}

/// Serializes read-modify-write cycles on `scheduled-tasks.json`.
static TASKS_FILE_LOCK: Mutex<()> = Mutex::const_new(());

static TASKS_CHANGED: OnceLock<broadcast::Sender<()>> = OnceLock::new();

fn tasks_changed_tx() -> &'static broadcast::Sender<()> {
    TASKS_CHANGED.get_or_init(|| broadcast::channel(64).0)
}

fn notify_tasks_changed() {
    let _ = tasks_changed_tx().send(());
}

fn task_error(message: impl Into<String>, task_id: Option<&str>) -> Value {
    let mut e = json!({ "_tag": "ScheduledTaskError", "message": message.into() });
    if let Some(id) = task_id {
        e["taskId"] = json!(id);
    }
    e
}

fn new_webhook_token() -> String {
    // Two v4 UUIDs = 32 random bytes, hex keeps it URL-safe without a base64 dep.
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

const WEBHOOK_ROUTE_PREFIX: &str = "/api/hooks";

/// The task objects persisted in the file are already wire-shaped except for
/// `webhook`, which is derived from the credentials map on read.
fn wire_task(task: &Value, doc: &Value) -> Value {
    let mut out = task.clone();
    let obj = out.as_object_mut().unwrap();
    obj.remove("webhook");
    if str_field(task, "schedule.type") != Some("webhook") {
        return out;
    }
    let id = str_field(task, "id").unwrap_or("");
    let creds = doc.get("credentials").and_then(|c| c.get(id));
    let token = creds.and_then(|c| str_field(c, "token"));
    if let Some(token) = token {
        let has_secret = creds
            .and_then(|c| c.get("secret"))
            .is_some_and(|s| !s.is_null());
        obj.insert(
            "webhook".into(),
            json!({
                "path": format!("{WEBHOOK_ROUTE_PREFIX}/{}/{token}", domain::uri_component(id)),
                "url": Value::Null,
                "hasSecret": has_secret,
            }),
        );
    }
    out
}

/// `ScheduledTaskListResult.tasks` order: `created_at DESC, task_id ASC`.
fn sorted_wire_tasks(doc: &Value) -> Vec<Value> {
    let mut tasks: Vec<&Value> = doc
        .get("tasks")
        .and_then(Value::as_array)
        .map(|t| t.iter().collect())
        .unwrap_or_default();
    tasks.sort_by(|a, b| {
        let ka = str_field(a, "createdAt").unwrap_or("");
        let kb = str_field(b, "createdAt").unwrap_or("");
        kb.cmp(ka)
            .then_with(|| str_field(a, "id").unwrap_or("").cmp(str_field(b, "id").unwrap_or("")))
    });
    tasks.into_iter().map(|t| wire_task(t, doc)).collect()
}

fn find_task_index<'a>(doc: &'a Value, id: &str) -> Option<usize> {
    doc.get("tasks").and_then(Value::as_array)?.iter().position(|t| str_field(t, "id") == Some(id))
}

// ---------------------------------------------------------------------------
// Schedule computation (apps/server/src/scheduledTasks/Schedule.ts)
// ---------------------------------------------------------------------------

const MIN_INTERVAL_MS: i64 = 60_000;

fn parse_time_of_day(value: &str) -> Option<(u8, u8)> {
    let (h, m) = value.trim().split_once(':')?;
    let hour: u8 = h.parse().ok()?;
    let minute: u8 = m.parse().ok()?;
    (hour <= 23 && minute <= 59).then_some((hour, minute))
}

/// `nextScheduledRunAt`: next run after `from_ms`, or None. Computed in UTC —
/// the TS server uses the host's local zone, and this port does not know it.
fn next_scheduled_run_at(schedule: &Value, from_ms: i64) -> Option<i64> {
    match str_field(schedule, "type")? {
        "webhook" => None,
        "once" => {
            let run_at = domain::iso_to_ms(str_field(schedule, "runAt")?)?;
            (run_at > from_ms).then_some(run_at)
        }
        "interval" => {
            let every = schedule
                .get("everyMs")
                .and_then(Value::as_i64)
                .unwrap_or(MIN_INTERVAL_MS)
                .max(MIN_INTERVAL_MS);
            Some(from_ms + every)
        }
        "fixed_time" => {
            let (hour, minute) = parse_time_of_day(str_field(schedule, "timeOfDay")?)?;
            let weekdays: Option<BTreeSet<i64>> = schedule
                .get("weekdays")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_i64).collect());
            let from = OffsetDateTime::from_unix_timestamp_nanos(from_ms as i128 * 1_000_000).ok()?;
            let time = Time::from_hms(hour, minute, 0).ok()?;
            for offset in 0..=7i64 {
                let day = (from + time::Duration::days(offset)).date();
                let candidate = day.with_time(time).assume_utc();
                let cand_ms = candidate.unix_timestamp() * 1000;
                if cand_ms <= from_ms {
                    continue;
                }
                if let Some(days) = &weekdays {
                    if !days.is_empty()
                        && !days.contains(&(candidate.weekday().number_days_from_sunday() as i64))
                    {
                        continue;
                    }
                }
                return Some(cand_ms);
            }
            None
        }
        _ => None,
    }
}

/// `isSameSchedule`: parsed comparison so reformatted equivalents keep a
/// pending run.
fn is_same_schedule(a: &Value, b: &Value) -> bool {
    let ty = str_field(a, "type").unwrap_or("");
    if ty != str_field(b, "type").unwrap_or("") {
        return false;
    }
    match ty {
        "interval" => {
            a.get("everyMs").and_then(Value::as_i64)
                == b.get("everyMs").and_then(Value::as_i64)
        }
        "webhook" => true,
        "once" => {
            str_field(a, "runAt").and_then(domain::iso_to_ms)
                == str_field(b, "runAt").and_then(domain::iso_to_ms)
        }
        "fixed_time" => {
            let key = |s: &Value| {
                let (h, m) = parse_time_of_day(str_field(s, "timeOfDay")?)?;
                let mut days: Vec<i64> = s
                    .get("weekdays")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_i64).collect())
                    .unwrap_or_default();
                days.sort_unstable();
                days.dedup();
                let day_key = if days.is_empty() || days.len() == 7 {
                    "daily".to_string()
                } else {
                    days.iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                };
                Some(format!("{h:02}:{m:02}:{day_key}"))
            };
            key(a).is_some() && key(a) == key(b)
        }
        _ => a == b,
    }
}

/// `scheduledTaskThreadMode`: stored mode or the legacy derivation.
fn stored_thread_mode(task: &Value) -> String {
    str_field(task, "threadMode")
        .map(str::to_owned)
        .or_else(|| {
            task.get("threadId")
                .map(|tid| if tid.is_null() { "new_thread" } else { "continue" })
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "new_thread".to_string())
}

/// `resolveThreadMode`: explicit input wins; otherwise keep the stored mode,
/// upgrading `new_thread` to `continue` when the save binds a thread.
fn resolve_thread_mode(input: &Value, existing: Option<&Value>) -> String {
    if let Some(mode) = str_field(input, "threadMode") {
        return mode.to_string();
    }
    let stored = existing.map(stored_thread_mode).unwrap_or_else(|| "new_thread".to_string());
    if stored == "new_thread" && str_field(input, "threadId").is_some() {
        "continue".to_string()
    } else {
        stored
    }
}

// ---------------------------------------------------------------------------
// scheduledTasks.* handlers
// ---------------------------------------------------------------------------

async fn tasks_upsert(state: Arc<AppState>, payload: Value) -> RpcResult {
    let _guard = TASKS_FILE_LOCK.lock().await;
    let path = tasks_file(&state);
    let mut doc = load_tasks_doc(&path);

    let id = str_field(&payload, "id").map(str::to_owned).unwrap_or_else(|| {
        match str_field(&payload, "commandId") {
            Some(cid) => format!("scheduled-task:{cid}"),
            None => format!("scheduled-task:{}", domain::new_id()),
        }
    });
    let existing_idx = find_task_index(&doc, &id);
    let existing = existing_idx.map(|i| doc["tasks"][i].clone());

    if payload.get("requireExisting").and_then(Value::as_bool) == Some(true) && existing.is_none() {
        return RpcResult::err(task_error("Schedule task not found.", Some(&id)));
    }

    let title = str_field(&payload, "title").unwrap_or("").trim().to_string();
    let prompt = str_field(&payload, "prompt").unwrap_or("").trim().to_string();
    if title.is_empty() || prompt.is_empty() {
        return RpcResult::err(task_error("A title and prompt are required.", Some(&id)));
    }
    let enabled = payload.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    let project_id = str_field(&payload, "projectId").unwrap_or("").to_string();
    let input_schedule = payload.get("schedule").cloned().unwrap_or(json!({}));
    let schedule_ty = str_field(&input_schedule, "type").unwrap_or("");

    // Normalize the webhook schedule: strip secrets out of the stored schedule
    // and keep omitted dedupe/filters from the stored task.
    let schedule = if schedule_ty == "webhook" {
        let mut sched = Map::new();
        sched.insert("type".into(), json!("webhook"));
        let sig = input_schedule.get("signature").filter(|s| !s.is_null());
        sched.insert(
            "signature".into(),
            sig.map(|s| {
                json!({
                    "header": str_field(s, "header").unwrap_or("").to_lowercase(),
                    "encoding": str_field(s, "encoding").unwrap_or("hex"),
                    "prefix": str_field(s, "prefix").unwrap_or(""),
                })
            })
            .unwrap_or(Value::Null),
        );
        sched.insert(
            "maxDeliveryAgeMinutes".into(),
            input_schedule
                .get("maxDeliveryAgeMinutes")
                .cloned()
                .unwrap_or(Value::Null),
        );
        let existing_sched = existing.as_ref().map(|e| e.get("schedule").cloned().unwrap_or(json!({})));
        let kept = |key: &str| {
            input_schedule.get(key).cloned().or_else(|| {
                existing_sched.as_ref().and_then(|s| s.get(key).cloned())
            })
        };
        if let Some(dedupe) = kept("dedupe").filter(|d| !d.is_null()) {
            sched.insert("dedupe".into(), dedupe);
        }
        if let Some(filters) = kept("filters").filter(|f| {
            f.as_array().is_some_and(|a| !a.is_empty())
        }) {
            sched.insert("filters".into(), filters);
        }
        Value::Object(sched)
    } else {
        input_schedule.clone()
    };

    // Thread binding rules from the TS service.
    let thread_mode = resolve_thread_mode(&payload, existing.as_ref());
    let kept_thread_id = str_field(&payload, "threadId")
        .map(str::to_owned)
        .or_else(|| {
            existing
                .as_ref()
                .and_then(|e| str_field(e, "threadId").map(str::to_owned))
        });
    let moved_off_thread = existing.as_ref().is_some_and(|e| {
        str_field(e, "projectId") != Some(project_id.as_str())
            && kept_thread_id.as_deref() == str_field(e, "threadId")
            && kept_thread_id.is_some()
    });
    let thread_id = if thread_mode == "new_thread" || moved_off_thread {
        None
    } else {
        kept_thread_id
    };

    // A newly bound destination must still exist and belong to the project.
    if let Some(tid) = &thread_id {
        let unchanged = existing
            .as_ref()
            .and_then(|e| str_field(e, "threadId"))
            == Some(tid.as_str());
        if !unchanged {
            match state.store.thread_shell(tid).await {
                Some(shell) => {
                    if !shell.get("deletedAt").unwrap_or(&Value::Null).is_null() {
                        return RpcResult::err(task_error(
                            "That thread no longer exists.",
                            Some(&id),
                        ));
                    }
                    if !shell.get("archivedAt").unwrap_or(&Value::Null).is_null() {
                        return RpcResult::err(task_error(
                            "That thread is archived. Unarchive it or pick another destination.",
                            Some(&id),
                        ));
                    }
                    if str_field(&shell, "projectId") != Some(project_id.as_str()) {
                        return RpcResult::err(task_error(
                            "That thread belongs to a different project.",
                            Some(&id),
                        ));
                    }
                }
                None => {
                    return RpcResult::err(task_error(
                        "That thread no longer exists.",
                        Some(&id),
                    ));
                }
            }
        }
    }

    let now = now_ms();
    let schedule_unchanged = existing.as_ref().is_some_and(|e| {
        e.get("enabled").and_then(Value::as_bool) == Some(enabled)
            && is_same_schedule(e.get("schedule").unwrap_or(&Value::Null), &schedule)
    });
    // An enabled one-time task whose moment already passed can never run.
    if !schedule_unchanged
        && enabled
        && schedule_ty == "once"
        && next_scheduled_run_at(&schedule, now).is_none()
    {
        return RpcResult::err(task_error(
            "The run time has passed. Pick a time in the future.",
            Some(&id),
        ));
    }

    // Webhook credentials live beside the task so secrets stay server-side.
    if schedule_ty == "webhook" {
        let creds = doc["credentials"].as_object().cloned().unwrap_or_default();
        let existing_creds = creds.get(&id).cloned().unwrap_or(json!({}));
        let token = str_field(&existing_creds, "token")
            .map(str::to_owned)
            .unwrap_or_else(new_webhook_token);
        let sig = input_schedule.get("signature").filter(|s| !s.is_null());
        let secret = match sig {
            None => Value::Null,
            Some(s) => s
                .get("secret")
                .filter(|v| !v.is_null())
                .cloned()
                .or_else(|| existing_creds.get("secret").cloned())
                .unwrap_or(Value::Null),
        };
        if sig.is_some() && secret.is_null() {
            return RpcResult::err(task_error(
                "A webhook signature check needs a signing secret.",
                Some(&id),
            ));
        }
        if !doc["credentials"].is_object() {
            doc["credentials"] = json!({});
        }
        doc["credentials"][&id] = json!({ "token": token, "secret": secret });
    } else {
        // Leaving the webhook trigger drops its URL and signing secret.
        if let Some(creds) = doc["credentials"].as_object_mut() {
            creds.remove(&id);
        }
    }

    let now_iso = ms_to_iso(now).unwrap_or_else(domain::now_iso);
    let e = existing.as_ref();
    let get_keep = |key: &str, default: Value| {
        e.and_then(|t| t.get(key).cloned()).unwrap_or(default)
    };
    let next_run_at = if schedule_unchanged {
        get_keep("nextRunAt", Value::Null)
    } else {
        next_scheduled_run_at(&schedule, now)
            .and_then(ms_to_iso)
            .map(Value::from)
            .unwrap_or(Value::Null)
    };
    let mut task = json!({
        "id": id,
        "title": title,
        "prompt": prompt,
        "enabled": enabled,
        "schedule": schedule,
        "projectId": project_id,
        "threadId": thread_id.map(Value::from).unwrap_or(Value::Null),
        "threadMode": thread_mode,
        "workspaceStrategy": payload.get("workspaceStrategy").cloned().unwrap_or(json!({"type":"root"})),
        "modelSelection": payload.get("modelSelection").cloned().unwrap_or(Value::Null),
        "runtimeMode": str_field(&payload, "runtimeMode").unwrap_or("full-access"),
        "interactionMode": str_field(&payload, "interactionMode").unwrap_or("default"),
        "createdBy": e.and_then(|t| t.get("createdBy").cloned())
            .or_else(|| payload.get("createdBy").cloned())
            .unwrap_or(json!("user")),
        "creationSource": payload.get("creationSource").cloned()
            .or_else(|| e.and_then(|t| t.get("creationSource").cloned()))
            .unwrap_or(json!("web")),
        "createdAt": get_keep("createdAt", json!(now_iso)),
        "updatedAt": now_iso,
        "nextRunAt": next_run_at,
        "lastRunAt": get_keep("lastRunAt", Value::Null),
        "lastRunStatus": get_keep("lastRunStatus", json!("never")),
        "lastRunError": get_keep("lastRunError", Value::Null),
        "runCount": get_keep("runCount", json!(0)),
    });
    if let Some(b) = e.and_then(|t| t.get("blockedByPolicy").cloned()) {
        task["blockedByPolicy"] = b;
    }

    match existing_idx {
        Some(i) => doc["tasks"][i] = task.clone(),
        None => doc["tasks"].as_array_mut().unwrap().push(task.clone()),
    }
    if let Err(e) = save_tasks_doc(&path, &doc) {
        return RpcResult::err(task_error(format!("Could not save schedule task. {e}"), Some(&id)));
    }
    notify_tasks_changed();
    RpcResult::ok(json!({ "task": wire_task(&task, &doc) }))
}

async fn tasks_set_enabled(state: Arc<AppState>, payload: Value) -> RpcResult {
    let id = str_field(&payload, "id").unwrap_or("").to_string();
    let enabled = payload.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    let _guard = TASKS_FILE_LOCK.lock().await;
    let path = tasks_file(&state);
    let mut doc = load_tasks_doc(&path);
    let Some(i) = find_task_index(&doc, &id) else {
        return RpcResult::err(task_error("Schedule task not found.", Some(&id)));
    };
    let mut task = doc["tasks"][i].clone();
    if task.get("enabled").and_then(Value::as_bool) == Some(enabled) {
        return RpcResult::ok(json!({ "task": wire_task(&task, &doc) }));
    }
    let now = now_ms();
    let schedule = task.get("schedule").cloned().unwrap_or(Value::Null);
    // Turning an expired one-time task back on is refused, same as the save.
    if enabled
        && str_field(&schedule, "type") == Some("once")
        && next_scheduled_run_at(&schedule, now).is_none()
    {
        return RpcResult::err(task_error(
            "The run time has passed. Pick a time in the future.",
            Some(&id),
        ));
    }
    let next = if enabled {
        next_scheduled_run_at(&schedule, now)
            .and_then(ms_to_iso)
            .map(Value::from)
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    task["enabled"] = json!(enabled);
    task["nextRunAt"] = next;
    task["updatedAt"] = json!(ms_to_iso(now).unwrap_or_else(domain::now_iso));
    doc["tasks"][i] = task.clone();
    if let Err(e) = save_tasks_doc(&path, &doc) {
        return RpcResult::err(task_error(format!("Could not update schedule task. {e}"), Some(&id)));
    }
    notify_tasks_changed();
    RpcResult::ok(json!({ "task": wire_task(&task, &doc) }))
}

async fn tasks_delete(state: Arc<AppState>, payload: Value) -> RpcResult {
    let id = str_field(&payload, "id").unwrap_or("").to_string();
    let _guard = TASKS_FILE_LOCK.lock().await;
    let path = tasks_file(&state);
    let mut doc = load_tasks_doc(&path);
    if let Some(i) = find_task_index(&doc, &id) {
        doc["tasks"].as_array_mut().unwrap().remove(i);
    }
    if let Some(creds) = doc["credentials"].as_object_mut() {
        creds.remove(&id);
    }
    if let Some(deliveries) = doc["deliveries"].as_object_mut() {
        deliveries.remove(&id);
    }
    if let Err(e) = save_tasks_doc(&path, &doc) {
        return RpcResult::err(task_error(format!("Could not delete schedule task. {e}"), Some(&id)));
    }
    notify_tasks_changed();
    RpcResult::ok(json!({ "id": id }))
}

/// Replicates `handlers::apply_and_publish` (private): apply a command,
/// broadcast its committed events, and spawn any provider turn it produced.
async fn apply_and_publish(
    state: &Arc<AppState>,
    command: Value,
) -> Result<crate::store::ApplyResult, String> {
    let mut applied = state.store.apply_command(command).await?;
    for ev in &applied.events {
        let _ = state.events_tx.send(ev.clone());
    }
    if let Some(ctx) = applied.run_ctx.take() {
        provider::spawn_turn(state.clone(), ctx);
    }
    Ok(applied)
}

async fn tasks_run_now(state: Arc<AppState>, payload: Value) -> RpcResult {
    let id = str_field(&payload, "id").unwrap_or("").to_string();
    let path = tasks_file(&state);

    // Phase 1: load + mark running under the file lock.
    let task = {
        let _guard = TASKS_FILE_LOCK.lock().await;
        let mut doc = load_tasks_doc(&path);
        let Some(i) = find_task_index(&doc, &id) else {
            return RpcResult::err(task_error("Schedule task not found.", Some(&id)));
        };
        let task = doc["tasks"][i].clone();
        if str_field(&task, "schedule.type") == Some("webhook") {
            return RpcResult::err(task_error(
                "Webhook tasks run when their URL receives a request.",
                Some(&id),
            ));
        }
        if str_field(&task, "lastRunStatus") == Some("running") {
            return RpcResult::err(task_error("Schedule task is already running.", Some(&id)));
        }
        let started = ms_to_iso(now_ms()).unwrap_or_else(domain::now_iso);
        doc["tasks"][i]["lastRunAt"] = json!(started);
        doc["tasks"][i]["lastRunStatus"] = json!("running");
        doc["tasks"][i]["lastRunError"] = Value::Null;
        doc["tasks"][i]["updatedAt"] = json!(started);
        let _ = save_tasks_doc(&path, &doc);
        notify_tasks_changed();
        task
    };

    let started_ms = now_ms();
    let thread_mode = stored_thread_mode(&task);
    let project_id = str_field(&task, "projectId").unwrap_or("").to_string();
    let prompt = str_field(&task, "prompt").unwrap_or("").to_string();
    let fire_key = format!("{id}:{started_ms}:manual");

    // Phase 2: launch or reuse the thread, then dispatch the prompt. A bound
    // run follows the thread's own provider selection, so the dispatch carries
    // no model/runtime overrides.
    let mut create_err: Option<String> = None;
    let thread_id = match str_field(&task, "threadId") {
        Some(tid) => Some(tid.to_string()),
        None => {
            let new_thread_id = domain::new_id();
            let create = json!({
                "type": "thread.create",
                "commandId": format!("scheduled-task-thread:{fire_key}"),
                "threadId": new_thread_id,
                "projectId": project_id,
                "title": str_field(&task, "title").unwrap_or("Scheduled task"),
                "modelSelection": task.get("modelSelection").cloned().unwrap_or(Value::Null),
                "runtimeMode": str_field(&task, "runtimeMode").unwrap_or("full-access"),
                "interactionMode": str_field(&task, "interactionMode").unwrap_or("default"),
                "createdBy": task.get("createdBy").cloned().unwrap_or(json!("user")),
                "creationSource": task.get("creationSource").cloned().unwrap_or(json!("web")),
            });
            match apply_and_publish(&state, create).await {
                Ok(_) => Some(new_thread_id),
                Err(e) => {
                    create_err = Some(e);
                    None
                }
            }
        }
    };
    let outcome: Result<String, String> = match &thread_id {
        Some(tid) => {
            let mut dispatch = json!({
                "type": "message.dispatch",
                "commandId": format!("scheduled-task:{fire_key}"),
                "messageId": format!("scheduled-task-message:{fire_key}"),
                "threadId": tid,
                "text": prompt,
                "attachments": [],
                "createdBy": task.get("createdBy").cloned().unwrap_or(json!("user")),
                "creationSource": task.get("creationSource").cloned().unwrap_or(json!("web")),
            });
            if thread_mode == "fresh_session" {
                dispatch["freshContext"] = json!(true);
            }
            apply_and_publish(&state, dispatch).await.map(|_| tid.clone())
        }
        None => Err(create_err.unwrap_or_else(|| "thread launch failed".into())),
    };

    // Phase 3: record the run outcome and recompute the next run. A one-time
    // task counts from the run's start so a manual run before its time does
    // not use it up.
    let succeeded = outcome.is_ok();
    let final_task = {
        let _guard = TASKS_FILE_LOCK.lock().await;
        let mut doc = load_tasks_doc(&path);
        let now = now_ms();
        if let Some(i) = find_task_index(&doc, &id) {
            let mut t = doc["tasks"][i].clone();
            // Continue/fresh_session tasks keep their first thread.
            if succeeded
                && thread_mode != "new_thread"
                && t.get("threadId").unwrap_or(&Value::Null).is_null()
            {
                if let Some(tid) = &thread_id {
                    t["threadId"] = json!(tid);
                }
            }
            let schedule = t.get("schedule").cloned().unwrap_or(Value::Null);
            let enabled = t.get("enabled").and_then(Value::as_bool) == Some(true);
            let base = if str_field(&schedule, "type") == Some("once") {
                started_ms
            } else {
                now
            };
            let next = if enabled {
                next_scheduled_run_at(&schedule, base)
                    .and_then(ms_to_iso)
                    .map(Value::from)
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            };
            t["enabled"] = json!(enabled);
            t["updatedAt"] = json!(ms_to_iso(now).unwrap_or_else(domain::now_iso));
            t["lastRunAt"] = json!(ms_to_iso(started_ms).unwrap_or_else(domain::now_iso));
            t["nextRunAt"] = next;
            t["lastRunStatus"] = json!(if succeeded { "succeeded" } else { "failed" });
            t["lastRunError"] = match &outcome {
                Ok(_) => Value::Null,
                Err(e) => json!(e),
            };
            t["runCount"] = json!(t.get("runCount").and_then(Value::as_i64).unwrap_or(0) + 1);
            doc["tasks"][i] = t.clone();
            let _ = save_tasks_doc(&path, &doc);
            notify_tasks_changed();
            wire_task(&t, &doc)
        } else {
            wire_task(&task, &doc)
        }
    };

    RpcResult::ok(json!({
        "task": final_task,
        "threadId": thread_id.map(Value::from).unwrap_or(Value::Null),
        "dispatchFailed": !succeeded,
    }))
}

async fn tasks_rotate_webhook_token(state: Arc<AppState>, payload: Value) -> RpcResult {
    let id = str_field(&payload, "id").unwrap_or("").to_string();
    let _guard = TASKS_FILE_LOCK.lock().await;
    let path = tasks_file(&state);
    let mut doc = load_tasks_doc(&path);
    let Some(i) = find_task_index(&doc, &id) else {
        return RpcResult::err(task_error("Schedule task not found.", Some(&id)));
    };
    if str_field(&doc["tasks"][i], "schedule.type") != Some("webhook") {
        return RpcResult::err(task_error("Only webhook tasks have a URL token.", Some(&id)));
    }
    if !doc["credentials"].is_object() {
        doc["credentials"] = json!({});
    }
    let secret = doc["credentials"][&id].get("secret").cloned().unwrap_or(Value::Null);
    doc["credentials"][&id] = json!({ "token": new_webhook_token(), "secret": secret });
    doc["tasks"][i]["updatedAt"] = json!(ms_to_iso(now_ms()).unwrap_or_else(domain::now_iso));
    if let Err(e) = save_tasks_doc(&path, &doc) {
        return RpcResult::err(task_error(format!("Could not rotate webhook token. {e}"), Some(&id)));
    }
    notify_tasks_changed();
    RpcResult::ok(json!({ "task": wire_task(&doc["tasks"][i].clone(), &doc) }))
}

/// Outcomes clients that predate `acceptAnyOutcome` can decode.
const LEGACY_DELIVERY_OUTCOMES: &[&str] = &[
    "accepted",
    "dispatch_failed",
    "rejected_signature",
    "disabled",
    "rate_limited",
    "expired",
];

fn delivery_summary(delivery: &Value) -> Value {
    json!({
        "id": delivery.get("id").cloned().unwrap_or(Value::Null),
        "taskId": delivery.get("taskId").cloned().unwrap_or(Value::Null),
        "receivedAt": delivery.get("receivedAt").cloned().unwrap_or(Value::Null),
        "method": delivery.get("method").cloned().unwrap_or(json!("POST")),
        "contentType": delivery.get("contentType").cloned().unwrap_or(Value::Null),
        "bodyBytes": delivery.get("bodyBytes").cloned().unwrap_or(json!(0)),
        "outcome": delivery.get("outcome").cloned().unwrap_or(json!("error")),
        "signatureVerified": delivery.get("signatureVerified").cloned().unwrap_or(json!(false)),
        "missingFields": delivery.get("missingFields").cloned().unwrap_or(json!([])),
        "error": delivery.get("error").cloned().unwrap_or(Value::Null),
    })
}

async fn tasks_list_deliveries(state: Arc<AppState>, payload: Value) -> RpcResult {
    let id = str_field(&payload, "id").unwrap_or("").to_string();
    let accept_any = payload.get("acceptAnyOutcome").and_then(Value::as_bool) == Some(true);
    let path = tasks_file(&state);
    let doc = load_tasks_doc(&path);
    let deliveries = doc
        .get("deliveries")
        .and_then(|d| d.get(&id))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let summaries: Vec<Value> = deliveries
        .iter()
        .filter(|d| {
            accept_any
                || str_field(d, "outcome").is_some_and(|o| LEGACY_DELIVERY_OUTCOMES.contains(&o))
        })
        .map(delivery_summary)
        .collect();
    RpcResult::ok(json!({ "deliveries": summaries }))
}

async fn tasks_get_delivery(state: Arc<AppState>, payload: Value) -> RpcResult {
    let id = str_field(&payload, "id").unwrap_or("").to_string();
    let delivery_id = str_field(&payload, "deliveryId").unwrap_or("");
    let path = tasks_file(&state);
    let doc = load_tasks_doc(&path);
    let deliveries = doc
        .get("deliveries")
        .and_then(|d| d.get(&id))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    match deliveries
        .iter()
        .find(|d| str_field(d, "id") == Some(delivery_id))
    {
        Some(d) => RpcResult::ok(json!({ "delivery": d })),
        None => RpcResult::err(task_error("Webhook delivery not found.", Some(&id))),
    }
}

/// `scheduledTasks.subscribe`: each stream item is a full
/// `ScheduledTaskListResult` snapshot — first the current list, then a fresh
/// list after every mutation.
async fn scheduled_tasks_stream(
    state: Arc<AppState>,
    out: OutTx,
    request_id: u64,
    window: Arc<Window>,
) {
    let mut rx = tasks_changed_tx().subscribe();
    let doc = load_tasks_doc(&tasks_file(&state));
    if !window
        .send_chunk(&out, request_id, vec![json!({ "tasks": sorted_wire_tasks(&doc) })])
        .await
    {
        return;
    }
    loop {
        match rx.recv().await {
            Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(_) => break,
        }
        if window.is_cancelled() {
            return;
        }
        let doc = load_tasks_doc(&tasks_file(&state));
        if !window
            .send_chunk(&out, request_id, vec![json!({ "tasks": sorted_wire_tasks(&doc) })])
            .await
        {
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// agentSessions.*
// ---------------------------------------------------------------------------

/// One discovered external session before grouping into project candidates.
struct DiscoveredSession {
    cwd: String,
    /// `AgentSessionSource` literal reported on the wire.
    source: &'static str,
    /// Some("devin") when the session is importable through the ACP mapping
    /// path; transcript sessions have no importer in this port.
    driver: Option<&'static str>,
    session_id: String,
    title: Option<String>,
    last_active_ms: Option<i64>,
}

/// `devin list --format json` — proven in this environment; the rows carry
/// `id`, `working_directory`, `title`, `last_activity_at` (unix seconds).
/// The CLI lists sessions for the directory it runs in, so it is invoked once
/// per candidate workspace root and results are deduplicated by session id.
async fn discover_devin_sessions_in(dir: &str) -> Vec<DiscoveredSession> {
    let listed = tokio::process::Command::new("devin")
        .args(["list", "--format", "json"])
        .current_dir(dir)
        .output()
        .await;
    let Ok(out) = listed else { return vec![] };
    if !out.status.success() {
        return vec![];
    }
    let rows: Vec<Value> = match serde_json::from_slice(&out.stdout) {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    rows.iter()
        .filter_map(|row| {
            let session_id = row
                .get("id")
                .or_else(|| row.get("sessionId"))
                .and_then(Value::as_str)?
                .to_string();
            let cwd = row
                .get("working_directory")
                .or_else(|| row.get("workingDirectory"))
                .or_else(|| row.get("cwd"))
                .and_then(Value::as_str)?
                .to_string();
            Some(DiscoveredSession {
                cwd,
                // The AgentSessionSource literal has no "devin"; devin's agent
                // is Claude-based, so its sessions count as claudeAgent here.
                source: "claudeAgent",
                driver: Some("devin"),
                session_id,
                title: row.get("title").and_then(Value::as_str).map(str::to_owned),
                last_active_ms: row
                    .get("last_activity_at")
                    .or_else(|| row.get("lastActivityAt"))
                    .or_else(|| row.get("updatedAt"))
                    .and_then(|v| domain::epochish_to_iso(v))
                    .and_then(|iso| domain::iso_to_ms(&iso)),
            })
        })
        .collect()
}

/// The devin CLI indexes every session globally in
/// `~/.local/share/devin/cli/sessions.db` (WAL SQLite). Reading it directly
/// finds sessions in any directory, unlike `devin list` which is scoped to
/// its cwd. Read-only open; a missing/unreadable DB just yields no rows.
fn discover_devin_sessions_db() -> Vec<DiscoveredSession> {
    let Some(home) = std::env::var_os("HOME") else { return vec![] };
    let db = PathBuf::from(home).join(".local/share/devin/cli/sessions.db");
    if !db.is_file() {
        return vec![];
    }
    let conn = match rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match conn.prepare(
        "SELECT id, working_directory, title, last_activity_at \
         FROM sessions WHERE hidden = 0 ORDER BY last_activity_at DESC",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, i64>(3)?,
        ))
    });
    let Ok(rows) = rows else { return vec![] };
    rows.flatten()
        .take(MAX_SESSION_FILES_PER_SOURCE)
        .map(|(session_id, cwd, title, last_activity_at)| DiscoveredSession {
            cwd,
            // The AgentSessionSource literal has no "devin"; devin's agent
            // is Claude-based, so its sessions count as claudeAgent here.
            source: "claudeAgent",
            driver: Some("devin"),
            session_id,
            title,
            last_active_ms: Some(last_activity_at * 1000),
        })
        .collect()
}

/// Devin sessions across the server's known workspace roots.
async fn discover_devin_sessions(state: &AppState) -> Vec<DiscoveredSession> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for s in discover_devin_sessions_db() {
        if seen.insert(s.session_id.clone()) {
            out.push(s);
        }
    }
    // Supplement with `devin list` per known workspace root — covers installs
    // whose session index lives somewhere other than the default path.
    let mut dirs: Vec<String> = state
        .store
        .list_projects()
        .await
        .iter()
        .filter_map(|p| str_field(p, "workspaceRoot").map(str::to_owned))
        .collect();
    dirs.push(state.project_dir.to_string_lossy().to_string());
    dirs.sort();
    dirs.dedup();
    for dir in dirs {
        for s in discover_devin_sessions_in(&dir).await {
            if seen.insert(s.session_id.clone()) {
                out.push(s);
            }
        }
    }
    out
}

/// First JSON line of a codex `rollout-*.jsonl` carries `payload.cwd`. The
/// `session_meta` record embeds the full system prompt, so the line can be
/// tens of KB — read a whole line rather than a fixed-size prefix.
fn read_first_line(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, f);
    reader.read_line(&mut line).ok()?;
    let line = line.trim_end().to_string();
    (!line.is_empty()).then_some(line)
}

fn file_mtime_ms(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

const MAX_SESSION_FILES_PER_SOURCE: usize = 5000;

fn collect_jsonl_files(dir: &Path, out: &mut Vec<PathBuf>, cap: usize) {
    if out.len() >= cap {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if out.len() >= cap {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, out, cap);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

/// `~/.codex/sessions/**/rollout-*.jsonl` — session_meta's `payload.cwd`.
fn discover_codex_sessions(truncated: &mut bool) -> Vec<DiscoveredSession> {
    let Some(home) = std::env::var_os("HOME") else { return vec![] };
    let root = PathBuf::from(home).join(".codex/sessions");
    let mut files = Vec::new();
    collect_jsonl_files(&root, &mut files, MAX_SESSION_FILES_PER_SOURCE + 1);
    if files.len() > MAX_SESSION_FILES_PER_SOURCE {
        *truncated = true;
        files.truncate(MAX_SESSION_FILES_PER_SOURCE);
    }
    files
        .iter()
        .filter_map(|path| {
            let first = read_first_line(path)?;
            let rec: Value = serde_json::from_str(&first).ok()?;
            let cwd = rec
                .pointer("/payload/cwd")
                .or_else(|| rec.get("cwd"))
                .and_then(Value::as_str)?
                .to_string();
            let session_id = rec
                .pointer("/payload/id")
                .or_else(|| rec.pointer("/payload/session_id"))
                .or_else(|| rec.get("sessionId"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .map(str::to_owned)
                })?;
            Some(DiscoveredSession {
                cwd,
                source: "codex",
                driver: None,
                session_id,
                title: None,
                last_active_ms: file_mtime_ms(path),
            })
        })
        .collect()
}

/// `~/.claude/projects/<slug>/*.jsonl` — each file is one session; the `cwd`
/// field on any record decodes the slug better than splitting on `-`.
fn discover_claude_sessions(truncated: &mut bool) -> Vec<DiscoveredSession> {
    let Some(home) = std::env::var_os("HOME") else { return vec![] };
    let root = PathBuf::from(home).join(".claude/projects");
    let mut files = Vec::new();
    collect_jsonl_files(&root, &mut files, MAX_SESSION_FILES_PER_SOURCE + 1);
    if files.len() > MAX_SESSION_FILES_PER_SOURCE {
        *truncated = true;
        files.truncate(MAX_SESSION_FILES_PER_SOURCE);
    }
    files
        .iter()
        .filter_map(|path| {
            use std::io::BufRead;
            let f = std::fs::File::open(path).ok()?;
            let reader = std::io::BufReader::with_capacity(16 * 1024, f);
            let mut cwd = None;
            let mut session_id = None;
            let mut title = None;
            for line in reader.lines().take(200) {
                let Ok(line) = line else { break };
                let Ok(rec) = serde_json::from_str::<Value>(&line) else { continue };
                if cwd.is_none() {
                    cwd = rec.get("cwd").and_then(Value::as_str).map(str::to_owned);
                }
                if session_id.is_none() {
                    session_id = rec
                        .get("sessionId")
                        .or_else(|| rec.get("session_id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                if title.is_none() {
                    title = rec
                        .get("customTitle")
                        .or_else(|| rec.get("aiTitle"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                if cwd.is_some() && session_id.is_some() {
                    break;
                }
            }
            Some(DiscoveredSession {
                cwd: cwd?,
                source: "claudeAgent",
                driver: None,
                session_id: session_id.or_else(|| {
                    path.file_stem().and_then(|s| s.to_str()).map(str::to_owned)
                })?,
                title,
                last_active_ms: file_mtime_ms(path),
            })
        })
        .collect()
}

fn normalize_dir_path(p: &str) -> String {
    let trimmed = p.trim();
    let mut s = if trimmed == "~" {
        std::env::var("HOME").unwrap_or_else(|_| trimmed.to_string())
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        format!("{}/{rest}", std::env::var("HOME").unwrap_or_default())
    } else {
        trimmed.to_string()
    };
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

async fn agent_sessions_scan(state: Arc<AppState>) -> RpcResult {
    let mut truncated = false;
    let mut sessions = discover_devin_sessions(&state).await;
    sessions.extend(discover_codex_sessions(&mut truncated));
    sessions.extend(discover_claude_sessions(&mut truncated));

    // Group sessions by working directory.
    struct Acc {
        title: Option<String>,
        sources: BTreeSet<&'static str>,
        session_ids: Vec<(Option<&'static str>, String)>,
        last_active_ms: Option<i64>,
    }
    let mut by_dir: HashMap<String, Acc> = HashMap::new();
    for s in sessions {
        let dir = normalize_dir_path(&s.cwd);
        let acc = by_dir.entry(dir).or_insert_with(|| Acc {
            title: None,
            sources: BTreeSet::new(),
            session_ids: vec![],
            last_active_ms: None,
        });
        if acc.title.is_none() {
            acc.title = s.title;
        }
        acc.sources.insert(s.source);
        acc.session_ids.push((s.driver, s.session_id));
        acc.last_active_ms = match (acc.last_active_ms, s.last_active_ms) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }

    // Match discovered directories to known projects by workspace root.
    let projects = state.store.list_projects().await;
    let project_for = |dir: &str| -> Option<String> {
        projects.iter().find_map(|p| {
            let root = str_field(p, "workspaceRoot")?;
            (normalize_dir_path(root) == dir).then(|| {
                str_field(p, "id").or_else(|| str_field(p, "projectId")).map(str::to_owned)
            })?
        })
    };

    let mut candidates: Vec<Value> = Vec::with_capacity(by_dir.len());
    for (dir, acc) in by_dir {
        let project_id = project_for(&dir);
        // All sessions imported ⇒ the candidate counts as already imported.
        // Only devin sessions carry an import mapping; anything else is unimported.
        let mut all_imported = project_id.is_some() && !acc.session_ids.is_empty();
        if all_imported {
            for (driver, sid) in &acc.session_ids {
                let Some(driver) = driver else {
                    all_imported = false;
                    break;
                };
                if state
                    .store
                    .acp_imported_thread_id(driver, project_id.as_deref().unwrap_or(""), sid)
                    .await
                    .is_none()
                {
                    all_imported = false;
                    break;
                }
            }
        }
        let title = acc.title.clone().unwrap_or_else(|| {
            Path::new(&dir)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&dir)
                .to_string()
        });
        let mut cand = json!({
            "path": dir,
            "title": title,
            "sources": acc.sources.iter().collect::<Vec<_>>(),
            "threadCount": acc.session_ids.len(),
            "lastActiveAt": acc.last_active_ms.and_then(ms_to_iso).map(Value::from).unwrap_or(Value::Null),
            "alreadyImported": all_imported,
        });
        if let Some(pid) = project_id {
            cand["projectId"] = json!(pid);
        }
        candidates.push(cand);
    }
    // Newest activity first, then path for stability.
    candidates.sort_by(|a, b| {
        let ka = str_field(a, "lastActiveAt").unwrap_or("");
        let kb = str_field(b, "lastActiveAt").unwrap_or("");
        kb.cmp(ka)
            .then_with(|| str_field(a, "path").unwrap_or("").cmp(str_field(b, "path").unwrap_or("")))
    });

    RpcResult::ok(json!({
        "candidates": candidates,
        "scannedAt": domain::now_iso(),
        "truncated": truncated,
    }))
}

/// `agentSessions.import`: import every devin session whose working directory
/// is the project's workspace root, using the same `thread.create` +
/// `importedNativeThread` path as `server.importAcpRegistrySession`.
async fn agent_sessions_import(state: Arc<AppState>, payload: Value) -> RpcResult {
    let project_id = str_field(&payload, "projectId").unwrap_or("").to_string();
    let project = match state.store.project_shell(&project_id).await {
        Some(p) => p,
        None => {
            return RpcResult::err(json!({
                "_tag": "AgentSessionImportProjectNotFoundError",
                "projectId": project_id,
            }));
        }
    };
    let root = normalize_dir_path(str_field(&project, "workspaceRoot").unwrap_or(""));
    if let Some(expected) = str_field(&payload, "expectedWorkspaceRoot") {
        if normalize_dir_path(expected) != root {
            return RpcResult::err(json!({
                "_tag": "AgentSessionImportProjectChangedError",
                "projectId": project_id,
            }));
        }
    }

    let mut seen_ids = std::collections::HashSet::new();
    let sessions: Vec<DiscoveredSession> = discover_devin_sessions_db()
        .into_iter()
        .chain(discover_devin_sessions_in(&root).await)
        .filter(|s| seen_ids.insert(s.session_id.clone()))
        .collect();
    let mut imported = 0i64;
    let mut skipped = 0i64;
    for session in sessions.iter().filter(|s| normalize_dir_path(&s.cwd) == root) {
        if state
            .store
            .acp_imported_thread_id("devin", &project_id, &session.session_id)
            .await
            .is_some()
        {
            skipped += 1;
            continue;
        }
        let thread_id = domain::imported_acp_thread_id("devin", "devin", &session.session_id);
        let title = session
            .title
            .clone()
            .unwrap_or_else(|| format!("Devin session {}", session.session_id));
        let command = json!({
            "type": "thread.create",
            "commandId": domain::new_id(),
            "threadId": thread_id,
            "projectId": project_id,
            "title": title,
            "modelSelection": { "instanceId": "devin", "model": "swe-2-high" },
            "runtimeMode": "approval-required",
            "interactionMode": "default",
            "createdBy": "user",
            "creationSource": "web",
            "providerInstanceId": "devin",
            "importedNativeThread": {
                "ref": { "driver": "devin", "nativeId": session.session_id, "strength": "strong" },
            },
        });
        match apply_and_publish(&state, command).await {
            Ok(_) => {
                state
                    .store
                    .record_acp_import("devin", &project_id, &session.session_id, &thread_id)
                    .await;
                imported += 1;
            }
            Err(e) => {
                tracing::warn!(%e, session = %session.session_id, "agent session import failed");
                skipped += 1;
            }
        }
    }

    // Codex/Claude transcript sessions have no import path in this port yet;
    // they count as skipped so totals reflect what was actually imported.
    let mut truncated = false;
    skipped += discover_codex_sessions(&mut truncated)
        .into_iter()
        .chain(discover_claude_sessions(&mut truncated))
        .filter(|s| normalize_dir_path(&s.cwd) == root)
        .count() as i64;

    RpcResult::ok(json!({ "importedCount": imported, "skippedCount": skipped }))
}

// ---------------------------------------------------------------------------
// projectClone.* — in-memory tracker with git progress
// ---------------------------------------------------------------------------

const CLONE_DETAIL_MAX: usize = 200;
const CLONE_ERROR_MAX: usize = 1000;
/// Finished snapshots stay visible briefly, like the TS tracker's 30s.
const DONE_RETENTION: Duration = Duration::from_secs(30);

fn clamp_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max.saturating_sub(1);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

struct CloneJob {
    title: String,
    /// The URL handed to git; may carry credentials and never leaves the server.
    clone_url: String,
    destination: String,
}

struct TrackedClone {
    snapshot: Value,
    job: CloneJob,
    cancel: Option<CancellationToken>,
}

struct CloneTracker {
    inner: StdMutex<HashMap<String, TrackedClone>>,
    changes: broadcast::Sender<Vec<Value>>,
    sequence: AtomicI64,
}

static CLONES: OnceLock<CloneTracker> = OnceLock::new();

fn clone_tracker() -> &'static CloneTracker {
    CLONES.get_or_init(|| CloneTracker {
        inner: StdMutex::new(HashMap::new()),
        changes: broadcast::channel(64).0,
        sequence: AtomicI64::new(0),
    })
}

fn next_clone_sequence() -> i64 {
    clone_tracker().sequence.fetch_add(1, Ordering::SeqCst) + 1
}

/// Snapshot list in insertion order (map values), mirroring the TS tracker.
fn clone_snapshot_list() -> Vec<Value> {
    let tracker = clone_tracker();
    let map = tracker.inner.lock().unwrap();
    let mut snaps: Vec<Value> = map.values().map(|t| t.snapshot.clone()).collect();
    snaps.sort_by_key(|s| s.get("sequence").and_then(Value::as_i64).unwrap_or(0));
    snaps
}

fn publish_clones() {
    let _ = clone_tracker().changes.send(clone_snapshot_list());
}

/// Mutate one tracked clone's snapshot in place and republish the list.
fn update_clone(project_id: &str, mutate: impl FnOnce(&mut Value)) {
    {
        let tracker = clone_tracker();
        let mut map = tracker.inner.lock().unwrap();
        let Some(tracked) = map.get_mut(project_id) else { return };
        mutate(&mut tracked.snapshot);
        tracked.snapshot["sequence"] = json!(next_clone_sequence());
    }
    publish_clones();
}

fn source_control_error(provider: &str, detail: impl Into<String>) -> Value {
    json!({
        "_tag": "SourceControlRepositoryError",
        "provider": provider,
        "operation": "cloneRepository",
        "detail": detail.into(),
    })
}

/// `parseGitCloneProgressLine`: stage prefix + `NNN% (done/total), detail`.
fn parse_clone_progress(line: &str) -> Option<(&'static str, Option<i64>, Option<String>)> {
    let t = line.trim();
    let stage = [
        ("remote: Enumerating objects", "counting"),
        ("remote: Counting objects", "counting"),
        ("remote: Compressing objects", "counting"),
        ("Receiving objects", "receiving"),
        ("Resolving deltas", "resolving"),
        ("Updating files", "checkout"),
        ("Checking out files", "checkout"),
    ]
    .iter()
    .find(|(prefix, _)| t.starts_with(prefix))
    .map(|(_, stage)| *stage)?;
    // ": 45% (12/34), 1.2 MiB | 3.4 MiB/s" → percent + trailing detail.
    let pct_pos = t.find('%')?;
    let digits_start = t[..pct_pos].rfind(|c: char| !c.is_ascii_digit()).map(|i| i + 1).unwrap_or(0);
    let percent: i64 = t[digits_start..pct_pos].parse().ok()?;
    let detail = t
        .splitn(2, ')')
        .nth(1)
        .map(|tail| tail.trim().trim_end_matches(", done.").trim().to_string())
        .filter(|d| !d.is_empty() && d != "done.");
    Some((stage, Some(percent.clamp(0, 100)), detail))
}

/// `cloneUrl.replace(...)` from the TS service: when `?` follows the authority
/// with no path (`https://host?x@y`), git parses the suffix as userinfo —
/// insert the path `/` before the query.
fn normalize_clone_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let rest = &url[scheme_end + 3..];
    let Some(q) = rest.find('?') else {
        return url.to_string();
    };
    match rest.find('/') {
        Some(slash) if slash < q => url.to_string(),
        _ => format!("{}/?{}", &url[..scheme_end + 3 + q], &rest[q + 1..]),
    }
}

/// Strip `user:pass@` userinfo so errors and snapshots never echo credentials.
fn redact_remote_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else { return url.to_string() };
    let rest = &url[scheme_end + 3..];
    let host_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..host_end];
    let Some(at) = authority.rfind('@') else {
        return url.to_string();
    };
    format!("{}://{}{}", &url[..scheme_end], &authority[at..], &rest[host_end..])
}

/// `git clone --progress` in a spawned task; parses stderr redraw segments and
/// keeps the last non-progress lines for the failure message.
fn spawn_clone_task(state: Arc<AppState>, project_id: String, cancel: CancellationToken) {
    tokio::spawn(async move {
        let (clone_url, destination) = {
            let tracker = clone_tracker();
            let map = tracker.inner.lock().unwrap();
            match map.get(&project_id) {
                Some(t) => (t.job.clone_url.clone(), t.job.destination.clone()),
                None => return,
            }
        };
        let dest = Path::new(&destination);
        let parent = dest.parent().unwrap_or_else(|| Path::new("/"));
        let dir_name = dest.file_name().unwrap_or_default();

        let mut child = match tokio::process::Command::new("git")
            .args(["clone", "--progress", "--", &clone_url])
            .arg(dir_name)
            .current_dir(parent)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                update_clone(&project_id, |snap| {
                    snap["phase"] = json!("failed");
                    snap["error"] = json!(clamp_text(&format!("git failed to spawn: {e}"), CLONE_ERROR_MAX));
                    snap["endedAt"] = json!(domain::now_iso());
                });
                return;
            }
        };

        let mut stderr = child.stderr.take().expect("stderr piped");
        let mut buf = [0u8; 8192];
        let mut pending = String::new();
        let mut stderr_tail: Vec<String> = Vec::new();
        let mut cancelled = false;

        let status = loop {
            let read = tokio::select! {
                _ = cancel.cancelled() => {
                    cancelled = true;
                    let _ = child.kill().await;
                    break child.wait().await;
                }
                r = stderr.read(&mut buf) => r,
            };
            match read {
                Ok(0) => break child.wait().await,
                Ok(n) => {
                    pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                    // Git redraws progress with \r; each segment is a "line".
                    let mut parts: Vec<String> = pending
                        .split(['\r', '\n'])
                        .map(str::to_owned)
                        .collect();
                    pending = parts.pop().unwrap_or_default();
                    for part in parts {
                        let trimmed = part.trim().to_string();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Some((stage, percent, detail)) = parse_clone_progress(&trimmed) {
                            update_clone(&project_id, |snap| {
                                snap["stage"] = json!(stage);
                                snap["percent"] = percent.map(Value::from).unwrap_or(Value::Null);
                                snap["detail"] = detail
                                    .map(|d| json!(clamp_text(&d, CLONE_DETAIL_MAX)))
                                    .unwrap_or(Value::Null);
                                snap["stalled"] = json!(false);
                            });
                        } else if !trimmed.starts_with("Cloning into") {
                            stderr_tail.push(redact_remote_url(&trimmed));
                            if stderr_tail.len() > 4 {
                                stderr_tail.remove(0);
                            }
                        }
                    }
                }
                Err(e) => {
                    stderr_tail.push(format!("stderr read failed: {e}"));
                    break child.wait().await;
                }
            }
        };

        if cancelled {
            update_clone(&project_id, |snap| {
                snap["phase"] = json!("cancelled");
                snap["endedAt"] = json!(domain::now_iso());
            });
            let _ = std::fs::remove_dir_all(&destination);
            return;
        }

        match status {
            Ok(s) if s.success() => {
                update_clone(&project_id, |snap| {
                    snap["phase"] = json!("done");
                    snap["percent"] = json!(100);
                    snap["stalled"] = json!(false);
                    snap["endedAt"] = json!(domain::now_iso());
                });
                let ended_at = {
                    let tracker = clone_tracker();
                    let map = tracker.inner.lock().unwrap();
                    map.get(&project_id)
                        .and_then(|t| t.snapshot.get("endedAt").and_then(Value::as_str))
                        .map(str::to_owned)
                };
                // Register the project so `project.created` reaches subscribers.
                let create = {
                    let tracker = clone_tracker();
                    let map = tracker.inner.lock().unwrap();
                    map.get(&project_id).map(|t| {
                        json!({
                            "type": "project.create",
                            "commandId": format!("project-clone-create:{project_id}"),
                            "projectId": project_id,
                            "title": t.job.title,
                            "workspaceRoot": t.job.destination,
                        })
                    })
                };
                if let Some(command) = create {
                    if let Err(e) = apply_and_publish(&state, command).await {
                        tracing::warn!(%e, %project_id, "project.create after clone failed");
                    }
                }
                // Done snapshots drop out of the stream after a retention beat.
                let pid = project_id.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(DONE_RETENTION).await;
                    let removed = {
                        let tracker = clone_tracker();
                        let mut map = tracker.inner.lock().unwrap();
                        let still_done = map.get(&pid).is_some_and(|t| {
                            str_field(&t.snapshot, "phase") == Some("done")
                                && t.snapshot.get("endedAt").and_then(Value::as_str)
                                    == ended_at.as_deref()
                        });
                        still_done.then(|| map.remove(&pid)).is_some()
                    };
                    if removed {
                        publish_clones();
                    }
                });
            }
            _ => {
                let detail = if stderr_tail.is_empty() {
                    "The repository could not be cloned.".to_string()
                } else {
                    stderr_tail.join(" ")
                };
                update_clone(&project_id, |snap| {
                    snap["phase"] = json!("failed");
                    snap["error"] = json!(clamp_text(&detail, CLONE_ERROR_MAX));
                    snap["endedAt"] = json!(domain::now_iso());
                });
            }
        }
    });
}

async fn clone_start(state: Arc<AppState>, payload: Value) -> RpcResult {
    let project_id = str_field(&payload, "projectId").unwrap_or("").to_string();
    let title = str_field(&payload, "title").unwrap_or("").to_string();
    let destination = normalize_dir_path(str_field(&payload, "destinationPath").unwrap_or(""));
    let provider = str_field(&payload, "provider").unwrap_or("github").to_string();
    let protocol = str_field(&payload, "protocol").unwrap_or("auto").to_string();

    if destination.is_empty() {
        return RpcResult::err(source_control_error(
            "unknown",
            "Choose a destination path before cloning.",
        ));
    }

    // Resolve the clone URL + repository info without a provider round-trip.
    let repository = match (
        str_field(&payload, "repository"),
        str_field(&payload, "remoteUrl"),
    ) {
        (Some(repo), _) => {
            let name = repo.trim().trim_end_matches(".git").to_string();
            json!({
                "provider": provider,
                "nameWithOwner": name,
                "url": format!("https://{provider}.com/{name}"),
                "sshUrl": format!("git@{provider}.com:{name}.git"),
            })
        }
        (None, _) => Value::Null,
    };
    let clone_url = match str_field(&payload, "remoteUrl") {
        Some(url) => url.trim().to_string(),
        None => match str_field(&payload, "repository") {
            Some(repo) if protocol == "ssh" => {
                format!("git@{provider}.com:{}.git", repo.trim().trim_end_matches(".git"))
            }
            Some(repo) => {
                format!("https://{provider}.com/{}.git", repo.trim().trim_end_matches(".git"))
            }
            None => {
                return RpcResult::err(source_control_error(
                    &provider,
                    "Enter a repository path or clone URL before cloning.",
                ));
            }
        },
    };
    // Git misreads @ in a pathless URL's query as userinfo; insert the path `/`.
    let clone_url = normalize_clone_url(&clone_url);
    let remote_url = redact_remote_url(&clone_url);

    // Destination must not exist or must be an empty directory.
    let dest = Path::new(&destination);
    if dest.exists() {
        let is_empty_dir = dest
            .is_dir()
            .then(|| std::fs::read_dir(dest).map(|mut d| d.next().is_none()).unwrap_or(false))
            .unwrap_or(false);
        if !is_empty_dir {
            return RpcResult::err(source_control_error(
                "unknown",
                if dest.is_dir() {
                    "Destination path already exists and is not empty."
                } else {
                    "Destination path already exists and is not a directory."
                },
            ));
        }
    } else if let Some(parent) = dest.parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return RpcResult::err(source_control_error(
                    "unknown",
                    format!("Could not create {}: {e}", parent.display()),
                ));
            }
        }
    }
    let destination = match std::fs::canonicalize(dest.parent().unwrap_or(Path::new("/"))) {
        Ok(abs_parent) => abs_parent.join(dest.file_name().unwrap_or_default()).to_string_lossy().to_string(),
        Err(_) => destination,
    };

    let snapshot = json!({
        "projectId": project_id,
        "remoteUrl": remote_url,
        "destinationPath": destination,
        "repository": repository,
        "phase": "running",
        "stage": "connecting",
        "percent": Value::Null,
        "detail": Value::Null,
        "stalled": false,
        "error": Value::Null,
        "startedAt": domain::now_iso(),
        "endedAt": Value::Null,
        "sequence": next_clone_sequence(),
    });

    let cancel = CancellationToken::new();
    {
        let tracker = clone_tracker();
        let mut map = tracker.inner.lock().unwrap();
        let conflict = map.values().any(|t| {
            str_field(&t.snapshot, "projectId") == Some(project_id.as_str())
                || t.job.destination == destination
        });
        if conflict {
            return RpcResult::err(source_control_error(
                &provider,
                "A clone into this destination is already in progress.",
            ));
        }
        map.insert(
            project_id.clone(),
            TrackedClone {
                snapshot: snapshot.clone(),
                job: CloneJob {
                    title: title.clone(),
                    clone_url: clone_url.clone(),
                    destination: destination.clone(),
                },
                cancel: Some(cancel.clone()),
            },
        );
    }
    publish_clones();
    spawn_clone_task(state, project_id.clone(), cancel);

    RpcResult::ok(json!({
        "projectId": project_id,
        "cwd": destination,
        "remoteUrl": remote_url,
        "repository": repository,
    }))
}

/// `projectClone.cancel`: interrupt a running clone; the clone task marks the
/// snapshot cancelled and deletes the partial checkout.
async fn clone_cancel(payload: Value) -> RpcResult {
    let project_id = str_field(&payload, "projectId").unwrap_or("").to_string();
    let token = {
        let tracker = clone_tracker();
        let map = tracker.inner.lock().unwrap();
        map.get(&project_id).and_then(|t| {
            (str_field(&t.snapshot, "phase") == Some("running"))
                .then(|| t.cancel.clone())
                .flatten()
        })
    };
    match token {
        Some(t) => {
            t.cancel();
            RpcResult::ok(json!({ "applied": true }))
        }
        None => RpcResult::ok(json!({ "applied": false })),
    }
}

/// `projectClone.retry`: clear the failed/cancelled checkout and relaunch.
async fn clone_retry(state: Arc<AppState>, payload: Value) -> RpcResult {
    let project_id = str_field(&payload, "projectId").unwrap_or("").to_string();
    let token = {
        let tracker = clone_tracker();
        let mut map = tracker.inner.lock().unwrap();
        match map.get_mut(&project_id) {
            Some(t)
                if matches!(
                    str_field(&t.snapshot, "phase"),
                    Some("failed") | Some("cancelled")
                ) =>
            {
                let _ = std::fs::remove_dir_all(&t.job.destination);
                t.snapshot["phase"] = json!("running");
                t.snapshot["stage"] = json!("connecting");
                t.snapshot["percent"] = Value::Null;
                t.snapshot["detail"] = Value::Null;
                t.snapshot["stalled"] = json!(false);
                t.snapshot["error"] = Value::Null;
                t.snapshot["startedAt"] = json!(domain::now_iso());
                t.snapshot["endedAt"] = Value::Null;
                t.snapshot["sequence"] = json!(next_clone_sequence());
                let token = CancellationToken::new();
                t.cancel = Some(token.clone());
                token
            }
            _ => return RpcResult::ok(json!({ "applied": false })),
        }
    };
    publish_clones();
    spawn_clone_task(state, project_id, token);
    RpcResult::ok(json!({ "applied": true }))
}

/// `subscribeProjectClones`: each stream item is a `ProjectCloneListEvent` —
/// the full snapshot array — emitted on every tracker change.
async fn project_clones_stream(out: OutTx, request_id: u64, window: Arc<Window>) {
    let mut rx = clone_tracker().changes.subscribe();
    if !window
        .send_chunk(&out, request_id, vec![Value::Array(clone_snapshot_list())])
        .await
    {
        return;
    }
    loop {
        match rx.recv().await {
            Ok(list) => {
                if window.is_cancelled() {
                    return;
                }
                if !window.send_chunk(&out, request_id, vec![Value::Array(list)]).await {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                if !window
                    .send_chunk(&out, request_id, vec![Value::Array(clone_snapshot_list())])
                    .await
                {
                    return;
                }
            }
            Err(_) => break,
        }
    }
}
