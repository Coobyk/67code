//! Source-control surfaces: right-panel PullRequests tab.
//!
//! For the selected thread's project the pane renders:
//! * a VCS status header fed by the `subscribeVcsStatus` stream (one
//!   `snapshot {local, remote}` chunk, then `localUpdated`/`remoteUpdated`
//!   diffs; a one-shot `vcs.refreshStatus` backfills data or the server's
//!   reason when the stream ends before its first snapshot),
//! * a branch picker over `vcs.listRefs` that switches via `vcs.switchRef`
//!   and creates+checks out via `vcs.createRef` from an inline input,
//! * project pull requests from `pullRequests.list` with expandable rows
//!   (`pullRequests.detail` + `pullRequests.linkedThreads`), re-listed when
//!   the `pullRequests.subscribeRefreshes` stream emits, plus the thread's
//!   linked pull request pinned above the list.
//!
//! Reachable via the right-panel tab bar (`Tab::PullRequests` →
//! `render_prs_pane`); Esc dismisses the branch picker through
//! `on_close_overlay` → `vcs_dismiss_picker`.

use std::cell::{Cell, RefCell};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    div, prelude::*, px, rgb, Context, Div, Entity, Focusable, FontWeight, MouseButton, Rgba,
    Stateful, Subscription, Window,
};
use serde_json::{json, Value};

use crate::net::ClientCommand;
use crate::notifications::ToastKind;
use crate::text_field::{PromptSubmitted, TextField};
use crate::{theme, AppView};

/// GitHub-flavored state/verdict colors (mirrors the web PR row tones).
fn pr_open_color() -> Rgba {
    rgb(0x3fb950)
}
fn pr_merged_color() -> Rgba {
    rgb(0xa371f7)
}
fn pr_closed_color() -> Rgba {
    rgb(0xf85149)
}
fn check_pending_color() -> Rgba {
    rgb(0xd29922)
}
fn check_pass_color() -> Rgba {
    rgb(0x34d399)
}

#[derive(Default)]
pub struct State {
    /// Project id the pane's streams/queries are bound to. `&self` render
    /// reads it to spot a scope change; `sync_scheduled` coalesces the
    /// deferred resync.
    scope: RefCell<Option<String>>,
    sync_scheduled: Cell<bool>,
    /// Replies stamped with an older generation are dropped.
    generation: u64,

    /// `subscribeVcsStatus` stream id (closed on resync).
    status_stream: Option<String>,
    status_local: Option<Value>,
    status_remote: Option<Value>,
    /// Whether remote info arrived at least once — a null remote then means
    /// "no upstream", not "still loading".
    remote_seen: bool,
    status_error: Option<String>,

    /// Branch picker dropdown open.
    picker_open: bool,
    refs: Vec<Value>,
    refs_loading: bool,
    refs_loaded: bool,
    refs_error: Option<String>,
    /// Ref a switch/create is in flight for (row shows pending).
    ref_pending: Option<String>,
    new_branch_open: bool,
    new_branch_field: Option<Entity<TextField>>,
    field_subs: Vec<Subscription>,

    prs: Vec<Value>,
    prs_loading: bool,
    prs_loaded: bool,
    pr_error: Option<String>,
    /// Per-project failures carried inside a successful list result.
    pr_item_errors: Vec<String>,
    refreshes_stream: Option<String>,
    /// Expanded row key (`repository#number`) and its lazy detail.
    expanded: Option<String>,
    detail_key: Option<String>,
    detail_loading: bool,
    detail: Option<Value>,
    detail_error: Option<String>,
    linked_threads: Option<Vec<Value>>,
}

/// Stable row/expansion key for a list entry.
fn pr_key(entry: &Value) -> String {
    format!(
        "{}#{}",
        entry["repository"].as_str().unwrap_or(""),
        entry["number"].as_i64().unwrap_or(0)
    )
}

/// `(glyph, color)` for a PR's open/draft/merged/closed state.
fn pr_state_glyph(state: &str, is_draft: bool) -> (&'static str, Rgba) {
    match (state, is_draft) {
        ("merged", _) => ("◆", pr_merged_color()),
        ("closed", _) => ("●", pr_closed_color()),
        (_, true) => ("○", theme::fg_muted()),
        _ => ("●", pr_open_color()),
    }
}

fn pr_state_label(state: &str, is_draft: bool) -> &'static str {
    match (state, is_draft) {
        ("merged", _) => "Merged",
        ("closed", _) => "Closed",
        (_, true) => "Draft",
        _ => "Open",
    }
}

/// `(glyph, color)` for one `PullRequestCheck.status`.
fn check_glyph(status: &str) -> (&'static str, Rgba) {
    match status {
        "success" => ("✓", check_pass_color()),
        "failure" | "cancelled" => ("×", pr_closed_color()),
        "pending" | "action-required" => ("◐", check_pending_color()),
        _ => ("○", theme::fg_muted()),
    }
}

/// `(label, color)` for a `PullRequestReviewDecision`.
fn review_decision_chip(decision: &str) -> (String, Rgba) {
    match decision {
        "approved" => ("✓ Approved".to_string(), check_pass_color()),
        "changes-requested" => ("× Changes requested".to_string(), pr_closed_color()),
        _ => ("○ Awaiting review".to_string(), theme::fg_muted()),
    }
}

/// Port of `summarizePullRequestChecks` (pullRequestPresentation.tsx).
fn summarize_checks(checks: &[Value]) -> String {
    if checks.is_empty() {
        return "No checks reported".to_string();
    }
    let count = |pred: &dyn Fn(&str) -> bool| {
        checks
            .iter()
            .filter(|c| pred(c["status"].as_str().unwrap_or("")))
            .count()
    };
    let failed = count(&|s| s == "failure" || s == "cancelled");
    let pending = count(&|s| s == "pending");
    let action = count(&|s| s == "action-required");
    let passed = count(&|s| s == "success");
    if failed > 0 {
        format!("{failed} of {} failing", checks.len())
    } else if action > 0 {
        format!("{action} awaiting action")
    } else if pending > 0 {
        format!("{pending} of {} running", checks.len())
    } else if passed == checks.len() {
        "All checks passed".to_string()
    } else {
        format!("{passed} of {} passing", checks.len())
    }
}

/// Readable message out of a Failure `Exit` frame. Tagged server errors put
/// their text in `requirement` (PullRequestUnavailableError), `detail`
/// (PullRequestOperationError/GitCommandError) or `message` (RpcClientError).
fn failure_message(exit: &Value) -> String {
    let error = &exit["cause"]["error"];
    for key in ["requirement", "detail", "message"] {
        if let Some(s) = error[key].as_str().filter(|s| !s.is_empty()) {
            return s.to_string();
        }
    }
    for candidate in [
        &exit["cause"]["message"],
        &exit["cause"]["defect"],
        &exit["error"]["message"],
        &exit["cause"],
    ] {
        if let Some(s) = candidate.as_str() {
            return s.to_string();
        }
    }
    "The request failed.".to_string()
}

/// Days-from-civil (Howard Hinnant) — days since 1970-01-01 for a y/m/d date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// ISO-8601 (`YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]`) → Unix epoch seconds.
fn parse_iso_epoch(s: &str) -> Option<i64> {
    if s.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let days = days_from_civil(num(0..4)?, num(5..7)?, num(8..10)?);
    let mut epoch = days * 86_400 + num(11..13)? * 3_600 + num(14..16)? * 60 + num(17..19)?;
    let mut rest = &s[19..];
    if rest.starts_with('.') {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    if let Some(sign) = rest
        .strip_prefix('+')
        .map(|_| 1i64)
        .or_else(|| rest.strip_prefix('-').map(|_| -1))
    {
        let hours: i64 = rest.get(1..3)?.parse().ok()?;
        let minutes: i64 = rest.get(4..6).unwrap_or("0").parse().unwrap_or(0);
        epoch -= sign * (hours * 3_600 + minutes * 60);
    }
    Some(epoch)
}

/// "5m ago" / "2d ago" from an ISO timestamp; short date on parse failure.
fn relative_time(iso: &str) -> String {
    if iso.is_empty() {
        return String::new();
    }
    let Some(epoch) = parse_iso_epoch(iso) else {
        return iso.get(..10).unwrap_or(iso).to_string();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let delta = now - epoch;
    if delta < 45 {
        return "just now".to_string();
    }
    let (n, unit) = if delta < 3_600 {
        (delta / 60, "m")
    } else if delta < 86_400 {
        (delta / 3_600, "h")
    } else if delta < 86_400 * 30 {
        (delta / 86_400, "d")
    } else if delta < 86_400 * 365 {
        (delta / (86_400 * 30), "mo")
    } else {
        (delta / (86_400 * 365), "y")
    };
    format!("{n}{unit} ago")
}

fn centered_message(title: &str, detail: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap_1()
        .p_4()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(title.to_string()),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::fg_muted())
                .text_center()
                .child(detail.to_string()),
        )
}

impl AppView {
    /// `(project_id, workspace_root)` of the selected thread's project —
    /// the scope every RPC in this pane carries.
    fn pr_scope(&self) -> Option<(String, String)> {
        let thread = self.state.selected_thread()?;
        let project = self
            .state
            .projects
            .iter()
            .find(|p| p.id == thread.project_id)?;
        Some((project.id.to_string(), project.path.to_string()))
    }

    /// Re-key the pane to the selected thread's project: tear down old
    /// streams, open `subscribeVcsStatus` + `pullRequests.subscribeRefreshes`,
    /// and kick the first `pullRequests.list`.
    fn vcs_sync_scope(&mut self, cx: &mut Context<Self>) {
        self.vcs_state.sync_scheduled.set(false);
        let scope = self.pr_scope();
        if self.vcs_state.scope.borrow().as_deref() == scope.as_ref().map(|(id, _)| id.as_str()) {
            return;
        }
        self.vcs_state.generation = self.vcs_state.generation.wrapping_add(1);
        let generation = self.vcs_state.generation;
        for id in [
            self.vcs_state.status_stream.take(),
            self.vcs_state.refreshes_stream.take(),
        ]
        .into_iter()
        .flatten()
        {
            self.close_stream(&id);
        }
        *self.vcs_state.scope.borrow_mut() = scope.as_ref().map(|(id, _)| id.clone());
        {
            let s = &mut self.vcs_state;
            s.status_local = None;
            s.status_remote = None;
            s.remote_seen = false;
            s.status_error = None;
            s.picker_open = false;
            s.refs.clear();
            s.refs_loading = false;
            s.refs_loaded = false;
            s.refs_error = None;
            s.ref_pending = None;
            s.new_branch_open = false;
            s.prs.clear();
            s.prs_loading = false;
            s.prs_loaded = false;
            s.pr_error = None;
            s.pr_item_errors.clear();
            s.expanded = None;
            s.detail_key = None;
            s.detail = None;
            s.detail_error = None;
            s.detail_loading = false;
            s.linked_threads = None;
        }

        let Some((_project_id, cwd)) = scope else {
            cx.notify();
            return;
        };

        match self.open_stream("subscribeVcsStatus", json!({ "cwd": cwd })) {
            Some((stream_id, rx)) => {
                self.vcs_state.status_stream = Some(stream_id);
                cx.spawn(async move |this, cx| {
                    while let Ok(chunk) = rx.recv().await {
                        let alive = this
                            .update(cx, |this, cx| {
                                this.vcs_apply_status_chunk(chunk, generation);
                                cx.notify();
                                true
                            })
                            .unwrap_or(false);
                        if !alive {
                            return;
                        }
                    }
                    let _ = this.update(cx, |this, cx| {
                        this.vcs_status_stream_ended(generation, cx);
                    });
                })
                .detach();
            }
            None => self.vcs_state.status_error = Some("Not connected to a server.".to_string()),
        }

        // Refresh-invalidation stream: each emitted revision re-lists. The
        // current server keeps it silent; consuming costs nothing.
        if let Some((stream_id, rx)) =
            self.open_stream("pullRequests.subscribeRefreshes", json!({}))
        {
            self.vcs_state.refreshes_stream = Some(stream_id);
            cx.spawn(async move |this, cx| {
                while let Ok(_chunk) = rx.recv().await {
                    let alive = this
                        .update(cx, |this, cx| {
                            let generation = this.vcs_state.generation;
                            this.vcs_load_prs(generation, cx);
                            true
                        })
                        .unwrap_or(false);
                    if !alive {
                        return;
                    }
                }
            })
            .detach();
        }

        self.vcs_load_prs(generation, cx);
        cx.notify();
    }

    /// Fold one `subscribeVcsStatus` chunk (`snapshot`/`localUpdated`/
    /// `remoteUpdated`) into the header state.
    fn vcs_apply_status_chunk(&mut self, chunk: Value, generation: u64) {
        let s = &mut self.vcs_state;
        if s.generation != generation {
            return;
        }
        match chunk["_tag"].as_str() {
            Some("snapshot") => {
                s.status_error = None;
                s.status_local = Some(chunk["local"].clone());
                s.remote_seen = true;
                s.status_remote = (!chunk["remote"].is_null()).then(|| chunk["remote"].clone());
            }
            Some("localUpdated") => {
                s.status_local = Some(chunk["local"].clone());
            }
            Some("remoteUpdated") => {
                s.remote_seen = true;
                s.status_remote = (!chunk["remote"].is_null()).then(|| chunk["remote"].clone());
            }
            _ => {}
        }
    }

    /// The status stream exited. With no snapshot ever received, a one-shot
    /// `vcs.refreshStatus` recovers either the merged status or the server's
    /// error reason; otherwise the last data stays on screen.
    fn vcs_status_stream_ended(&mut self, generation: u64, cx: &mut Context<Self>) {
        if self.vcs_state.generation != generation {
            return;
        }
        self.vcs_state.status_stream = None;
        if self.vcs_state.status_local.is_some() {
            return;
        }
        let Some((_pid, cwd)) = self.pr_scope() else {
            return;
        };
        let Some(rx) = self.send_query("vcs.refreshStatus", json!({ "cwd": cwd })) else {
            self.vcs_state.status_error = Some("Not connected to a server.".to_string());
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.vcs_state;
                if s.generation != generation {
                    return;
                }
                if exit["_tag"].as_str() == Some("Success") {
                    let merged = &exit["value"];
                    // refreshStatus returns local+remote fields merged flat;
                    // split the remote-shaped keys back out for rendering.
                    s.status_remote = Some(json!({
                        "hasUpstream": merged["hasUpstream"].clone(),
                        "aheadCount": merged["aheadCount"].clone(),
                        "behindCount": merged["behindCount"].clone(),
                        "aheadOfDefaultCount": merged["aheadOfDefaultCount"].clone(),
                        "pr": merged["pr"].clone(),
                    }));
                    s.remote_seen = true;
                    s.status_local = Some(merged.clone());
                    s.status_error = None;
                } else {
                    s.status_error = Some(failure_message(&exit));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `pullRequests.list` for the scope project (all states, sorted
    /// server-side by recency). Guards against concurrent loads.
    fn vcs_load_prs(&mut self, generation: u64, cx: &mut Context<Self>) {
        if self.vcs_state.prs_loading || self.vcs_state.generation != generation {
            return;
        }
        let Some((project_id, _cwd)) = self.pr_scope() else {
            return;
        };
        let Some(rx) = self.send_query(
            "pullRequests.list",
            json!({ "projectId": project_id, "state": "all", "limit": 50 }),
        ) else {
            self.vcs_state.pr_error = Some("Not connected to a server.".to_string());
            self.vcs_state.prs_loaded = true;
            cx.notify();
            return;
        };
        self.vcs_state.prs_loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.vcs_state;
                if s.generation != generation {
                    return;
                }
                s.prs_loading = false;
                s.prs_loaded = true;
                if exit["_tag"].as_str() == Some("Success") {
                    let value = &exit["value"];
                    let mut entries: Vec<Value> =
                        value["entries"].as_array().cloned().unwrap_or_default();
                    // Open/draft first, then merged, then closed — recency
                    // inside each group (already sorted by updatedAt).
                    let rank = |e: &Value| match e["state"].as_str() {
                        Some("merged") => 1,
                        Some("closed") => 2,
                        _ => 0,
                    };
                    entries.sort_by_key(|e| rank(e));
                    s.prs = entries;
                    s.pr_item_errors = value["errors"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|e| {
                                    e["message"].as_str().map(|m| {
                                        format!(
                                            "{}: {m}",
                                            e["projectTitle"].as_str().unwrap_or("project")
                                        )
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    s.pr_error = None;
                } else {
                    s.pr_error = Some(failure_message(&exit));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `vcs.listRefs` — fills the branch picker.
    fn vcs_load_refs(&mut self, cx: &mut Context<Self>) {
        if self.vcs_state.refs_loading {
            return;
        }
        let Some((_pid, cwd)) = self.pr_scope() else {
            return;
        };
        let generation = self.vcs_state.generation;
        let Some(rx) = self.send_query("vcs.listRefs", json!({ "cwd": cwd, "limit": 100 })) else {
            self.vcs_state.refs_error = Some("Not connected to a server.".to_string());
            cx.notify();
            return;
        };
        self.vcs_state.refs_loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.vcs_state;
                if s.generation != generation {
                    return;
                }
                s.refs_loading = false;
                s.refs_loaded = true;
                if exit["_tag"].as_str() == Some("Success") {
                    s.refs = exit["value"]["refs"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    s.refs_error = None;
                } else {
                    s.refs_error = Some(failure_message(&exit));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `vcs.switchRef` — checkout a picked ref; the status stream reflects
    /// the new branch on its next poll.
    fn vcs_switch_ref(&mut self, ref_name: String, cx: &mut Context<Self>) {
        if self.vcs_state.ref_pending.is_some() {
            return;
        }
        let Some((_pid, cwd)) = self.pr_scope() else {
            return;
        };
        let generation = self.vcs_state.generation;
        let Some(rx) = self.send_query("vcs.switchRef", json!({ "cwd": cwd, "refName": ref_name }))
        else {
            self.vcs_state.refs_error = Some("Not connected to a server.".to_string());
            cx.notify();
            return;
        };
        self.vcs_state.ref_pending = Some(ref_name.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.vcs_state;
                if s.generation != generation {
                    return;
                }
                s.ref_pending = None;
                if exit["_tag"].as_str() == Some("Success") {
                    let name = exit["value"]["refName"]
                        .as_str()
                        .unwrap_or(&ref_name)
                        .to_string();
                    s.picker_open = false;
                    s.new_branch_open = false;
                    this.push_toast(
                        ToastKind::Success,
                        format!("Switched to {name}"),
                        None,
                        None,
                        cx,
                    );
                    this.vcs_load_refs(cx);
                } else {
                    let msg = failure_message(&exit);
                    s.refs_error = Some(msg.clone());
                    this.push_toast(
                        ToastKind::Error,
                        "Branch switch failed",
                        Some(msg),
                        None,
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `vcs.createRef` `{switchRef: true}` — create + check out a new branch
    /// from the inline input.
    fn vcs_create_ref(&mut self, ref_name: String, cx: &mut Context<Self>) {
        if self.vcs_state.ref_pending.is_some() {
            return;
        }
        let Some((_pid, cwd)) = self.pr_scope() else {
            return;
        };
        let generation = self.vcs_state.generation;
        let Some(rx) = self.send_query(
            "vcs.createRef",
            json!({ "cwd": cwd, "refName": ref_name, "switchRef": true }),
        ) else {
            self.vcs_state.refs_error = Some("Not connected to a server.".to_string());
            cx.notify();
            return;
        };
        self.vcs_state.ref_pending = Some(ref_name.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.vcs_state;
                if s.generation != generation {
                    return;
                }
                s.ref_pending = None;
                if exit["_tag"].as_str() == Some("Success") {
                    s.picker_open = false;
                    s.new_branch_open = false;
                    this.push_toast(
                        ToastKind::Success,
                        format!("Created and switched to {ref_name}"),
                        None,
                        None,
                        cx,
                    );
                    this.vcs_load_refs(cx);
                } else {
                    let msg = failure_message(&exit);
                    s.refs_error = Some(msg.clone());
                    this.push_toast(
                        ToastKind::Error,
                        "Branch create failed",
                        Some(msg),
                        None,
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Lazily build the new-branch `TextField` (entities need a context) and
    /// focus it. `PromptSubmitted` → `vcs.createRef`.
    fn vcs_open_new_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.vcs_state.new_branch_open = true;
        if self.vcs_state.new_branch_field.is_none() {
            let field = cx.new(|cx| TextField::new("new-branch-name", cx));
            self.vcs_state.field_subs.push(cx.subscribe_in(
                &field,
                window,
                |this, _field, event: &PromptSubmitted, _window, cx| {
                    let name = event.0.trim().to_string();
                    if !name.is_empty() {
                        this.vcs_create_ref(name, cx);
                    }
                },
            ));
            self.vcs_state.new_branch_field = Some(field);
        }
        if let Some(field) = self.vcs_state.new_branch_field.clone() {
            field.update(cx, |f, cx| f.reset(window, cx));
            field.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// Expand/collapse a PR row. Expanding fires `pullRequests.detail` and
    /// `pullRequests.linkedThreads` for the entry.
    fn vcs_toggle_pr(&mut self, key: String, cx: &mut Context<Self>) {
        if self.vcs_state.expanded.as_deref() == Some(key.as_str()) {
            self.vcs_state.expanded = None;
            cx.notify();
            return;
        }
        self.vcs_state.expanded = Some(key.clone());
        self.vcs_state.detail = None;
        self.vcs_state.detail_error = None;
        self.vcs_state.linked_threads = None;
        self.vcs_state.detail_loading = true;
        self.vcs_state.detail_key = Some(key.clone());
        cx.notify();

        let Some(entry) = self
            .vcs_state
            .prs
            .iter()
            .find(|e| pr_key(e) == key)
            .cloned()
        else {
            return;
        };
        let generation = self.vcs_state.generation;
        let mut payload = json!({
            "projectId": entry["projectId"].as_str().unwrap_or(""),
            "repository": entry["repository"].as_str().unwrap_or(""),
            "number": entry["number"].as_i64().unwrap_or(0),
        });
        if let Some(host) = entry["host"].as_str() {
            payload["host"] = json!(host);
        }
        if let Some(rx) = self.send_query("pullRequests.detail", payload.clone()) {
            let key = key.clone();
            cx.spawn(async move |this, cx| {
                let exit = rx
                    .recv()
                    .await
                    .unwrap_or_else(|_| json!({"_tag": "Failure"}));
                let _ = this.update(cx, |this, cx| {
                    let s = &mut this.vcs_state;
                    if s.generation != generation || s.detail_key.as_deref() != Some(key.as_str()) {
                        return;
                    }
                    s.detail_loading = false;
                    if exit["_tag"].as_str() == Some("Success") {
                        s.detail = Some(exit["value"].clone());
                        s.detail_error = None;
                    } else {
                        s.detail_error = Some(failure_message(&exit));
                    }
                    cx.notify();
                });
            })
            .detach();
        } else {
            self.vcs_state.detail_loading = false;
            self.vcs_state.detail_error = Some("Not connected to a server.".to_string());
        }
        if let Some(rx) = self.send_query("pullRequests.linkedThreads", payload) {
            let key = key.clone();
            cx.spawn(async move |this, cx| {
                let exit = rx
                    .recv()
                    .await
                    .unwrap_or_else(|_| json!({"_tag": "Failure"}));
                let _ = this.update(cx, |this, cx| {
                    let s = &mut this.vcs_state;
                    if s.generation != generation || s.detail_key.as_deref() != Some(key.as_str()) {
                        return;
                    }
                    if exit["_tag"].as_str() == Some("Success") {
                        s.linked_threads = Some(
                            exit["value"]["threads"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default(),
                        );
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    // -----------------------------------------------------------------------
    // Render
    // -----------------------------------------------------------------------

    /// Dismiss the branch picker if open. Returns true when it consumed the
    /// close so `on_close_overlay` can stop before other overlays.
    pub(crate) fn vcs_dismiss_picker(&mut self, cx: &mut Context<Self>) -> bool {
        if self.vcs_state.picker_open {
            self.vcs_state.picker_open = false;
            cx.notify();
            true
        } else {
            false
        }
    }

    pub(crate) fn render_prs_pane(&self, cx: &mut Context<Self>) -> Div {
        // Scope changed → schedule one resync (deferred: render runs &self).
        let scope_id = self.pr_scope().map(|(id, _)| id);
        if self.vcs_state.scope.borrow().as_deref() != scope_id.as_deref()
            && !self.vcs_state.sync_scheduled.replace(true)
        {
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| this.vcs_sync_scope(cx));
            })
            .detach();
        }

        if self.state.selected.is_none() || self.state.selected_thread().is_none() {
            return centered_message(
                "Select a thread",
                "Open a thread to see its project's branch status and pull requests.",
            );
        }
        if self.pr_scope().is_none() {
            return centered_message(
                "Project unavailable",
                "This thread's project is not in the server's project list.",
            );
        }

        let mut body = div().flex().flex_col().flex_1().min_h_0();
        body = body.child(self.render_prs_status_header(cx));
        if self.vcs_state.picker_open {
            body = body.child(self.render_branch_picker(cx));
        }
        body = body.child(self.render_prs_list_header(cx));
        body.child(self.render_prs_list(cx))
    }

    /// Status card: branch toggle row + upstream/ahead-behind/changes line.
    fn render_prs_status_header(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.vcs_state;
        let mut card = div()
            .m_2()
            .mb_0()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .flex()
            .flex_col();

        if let Some(err) = &s.status_error {
            return card.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(div().text_color(pr_closed_color()).child("⚠"))
                    .child(err.clone()),
            );
        }
        let Some(local) = s.status_local.as_ref() else {
            return card.child(muted_status_row("Loading repository status…"));
        };
        if local["isRepo"].as_bool() != Some(true) {
            return card.child(muted_status_row("Not a git repository"));
        }

        let ref_name = local["refName"]
            .as_str()
            .filter(|n| !n.is_empty())
            .unwrap_or("(detached HEAD)")
            .to_string();
        let picker_open = s.picker_open;
        card = card.child(
            div()
                .id("vcs-branch-toggle")
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded_t_md()
                .cursor_pointer()
                .hover(|h| h.bg(theme::hover()))
                .child(
                    div()
                        .w(px(12.))
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(if picker_open { "▾" } else { "▸" }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(ref_name),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child("switch"),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _window, cx| {
                        this.vcs_state.picker_open = !this.vcs_state.picker_open;
                        if this.vcs_state.picker_open {
                            this.vcs_load_refs(cx);
                        }
                        cx.notify();
                    }),
                ),
        );

        // Meta line: upstream, ahead/behind, working-tree changes.
        let mut meta = div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .pb_1p5()
            .text_xs()
            .text_color(theme::fg_muted())
            .flex_wrap();
        let remote = s.status_remote.as_ref();
        if !s.remote_seen {
            meta = meta.child("…");
        } else if remote.and_then(|r| r["hasUpstream"].as_bool()) != Some(true) {
            meta = meta.child("no upstream");
        } else if let Some(r) = remote {
            let upstream = s
                .refs
                .iter()
                .find(|r| r["current"].as_bool() == Some(true))
                .and_then(|r| r["upstreamRef"].as_str())
                .map(|u| u.strip_prefix("refs/remotes/").unwrap_or(u).to_string());
            meta = meta.child(format!(
                "→ {}",
                upstream.unwrap_or_else(|| "upstream".into())
            ));
            let ahead = r["aheadCount"].as_i64().unwrap_or(0);
            let behind = r["behindCount"].as_i64().unwrap_or(0);
            if ahead > 0 || behind > 0 {
                meta = meta.child(
                    div()
                        .text_color(theme::fg())
                        .child(format!("↑{ahead} ↓{behind}")),
                );
            }
        }
        if !s.remote_seen || remote.is_none() {
            let ahead_of_default = remote
                .and_then(|r| r["aheadOfDefaultCount"].as_i64())
                .unwrap_or(0);
            if ahead_of_default > 0 {
                meta = meta.child(format!("↑{ahead_of_default} vs base"));
            }
        }
        if local["hasWorkingTreeChanges"].as_bool() == Some(true) {
            let files = local["workingTree"]["files"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            let ins = local["workingTree"]["insertions"].as_i64().unwrap_or(0);
            let del = local["workingTree"]["deletions"].as_i64().unwrap_or(0);
            meta = meta.child(format!("· {files} changed"));
            if ins + del > 0 {
                meta = meta.child(
                    div()
                        .flex()
                        .gap_1()
                        .child(
                            div()
                                .text_color(check_pass_color())
                                .child(format!("+{ins}")),
                        )
                        .child(div().text_color(pr_closed_color()).child(format!("−{del}"))),
                );
            }
        } else {
            meta = meta.child("· clean");
        }
        card.child(meta)
    }

    /// Inline branch picker under the status card.
    fn render_branch_picker(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.vcs_state;
        let mut picker = div()
            .mx_2()
            .mt_1()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .flex()
            .flex_col();

        picker = picker.child(
            div()
                .id("vcs-new-branch")
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1p5()
                .cursor_pointer()
                .text_xs()
                .hover(|h| h.bg(theme::hover()))
                .child(div().text_color(theme::primary()).child("+"))
                .child("New branch")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, window, cx| {
                        this.vcs_open_new_branch(window, cx);
                    }),
                ),
        );
        if s.new_branch_open {
            let mut row = div().flex().flex_col().px_2().pb_1().gap_0p5();
            if let Some(field) = s.new_branch_field.clone() {
                row = row.child(
                    div()
                        .flex()
                        .items_center()
                        .h(px(24.))
                        .px_1()
                        .rounded_sm()
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::canvas())
                        .text_sm()
                        .child(field),
                );
            }
            row = row.child(
                div()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Enter to create and switch"),
            );
            picker = picker.child(row);
        }

        let mut list = div()
            .id("vcs-refs")
            .flex()
            .flex_col()
            .max_h(px(220.))
            .overflow_y_scroll()
            .border_t_1()
            .border_color(theme::border());
        if s.refs_loading && s.refs.is_empty() {
            list = list.child(muted_status_row("Loading refs…"));
        }
        if let Some(err) = &s.refs_error {
            list = list.child(
                div()
                    .px_2()
                    .py_1p5()
                    .text_xs()
                    .text_color(pr_closed_color())
                    .child(err.clone()),
            );
        }
        if s.refs_loaded && s.refs.is_empty() && !s.refs_loading {
            list = list.child(muted_status_row("No refs"));
        }
        for (ix, r) in s.refs.iter().enumerate() {
            let name = r["name"].as_str().unwrap_or("").to_string();
            let current = r["current"].as_bool() == Some(true);
            let remote = r["isRemote"].as_bool() == Some(true);
            let default = r["isDefault"].as_bool() == Some(true);
            let in_worktree = !r["worktreePath"].is_null() && !current;
            let pending = s.ref_pending.as_deref() == Some(name.as_str());
            let mut row = div()
                .id(("vcs-ref", ix))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .text_xs();
            if !current && !pending {
                row = row.cursor_pointer().hover(|h| h.bg(theme::hover()));
            }
            row = row
                .child(
                    div()
                        .w(px(12.))
                        .text_color(check_pass_color())
                        .child(if current { "✓" } else { "" }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(if current {
                            theme::fg()
                        } else if remote {
                            theme::fg_muted()
                        } else {
                            theme::fg()
                        })
                        .child(name.clone()),
                );
            if pending {
                row = row.child(div().text_color(theme::fg_muted()).child("…"));
            }
            if default {
                row = row.child(chip("default"));
            }
            if remote {
                row = row.child(chip("remote"));
            }
            if in_worktree {
                row = row.child(chip("worktree"));
            }
            if !current && !pending {
                let name = name.clone();
                row = row.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        this.vcs_switch_ref(name.clone(), cx);
                    }),
                );
            }
            list = list.child(row);
        }
        picker.child(list)
    }

    /// "Pull requests · N" section header + refresh button.
    fn render_prs_list_header(&self, cx: &mut Context<Self>) -> Div {
        let count = self.vcs_state.prs.len();
        let loading = self.vcs_state.prs_loading;
        div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .pt_2()
            .pb_1()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::fg_muted())
                            .child("Pull requests"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(if loading {
                                "…".to_string()
                            } else {
                                format!("{count}")
                            }),
                    ),
            )
            .child(
                div()
                    .id("vcs-pr-refresh")
                    .px_1p5()
                    .py_0p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .hover(|h| h.bg(theme::hover()))
                    .child("↻")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            let generation = this.vcs_state.generation;
                            this.vcs_load_prs(generation, cx);
                        }),
                    ),
            )
    }

    /// Scrollable PR rows + the thread's linked-PR badge on top.
    fn render_prs_list(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let s = &self.vcs_state;
        let mut list = div()
            .id("prs-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_2()
            .pb_2()
            .gap_0p5();

        // Thread's linked pull request pinned above the list.
        if let Some(link) = self
            .state
            .selected_thread()
            .and_then(|t| t.linked_pull_request.clone())
        {
            let number = link["number"].as_i64().unwrap_or(0);
            let repo = link["repository"].as_str().unwrap_or("").to_string();
            let url = link["url"].as_str().unwrap_or("").to_string();
            if number > 0 {
                list = list.child(
                    div()
                        .id("vcs-linked-pr")
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .border_1()
                        .border_color(theme::border())
                        .cursor_pointer()
                        .hover(|h| h.bg(theme::hover()))
                        .child(
                            div()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .bg(theme::selected())
                                .text_xs()
                                .text_color(theme::primary())
                                .child("Linked"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child(format!("#{number}")),
                        )
                        .child(div().flex_1().min_w_0().truncate().text_xs().child(repo))
                        .child(div().text_xs().text_color(theme::fg_muted()).child("↗"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |_this, _ev, _window, cx| {
                                cx.open_url(&url);
                            }),
                        ),
                );
            }
        }

        for err in &s.pr_item_errors {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(check_pending_color())
                    .child(format!("⚠ {err}")),
            );
        }

        if let Some(err) = &s.pr_error {
            if s.prs.is_empty() {
                return list.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .px_4()
                        .py_6()
                        .child(div().text_sm().child("Pull requests unavailable"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .text_center()
                                .child(err.clone()),
                        ),
                );
            }
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(pr_closed_color())
                    .child(err.clone()),
            );
        }

        if !s.prs_loaded || (s.prs_loading && s.prs.is_empty()) {
            return list.child(muted_status_row("Loading pull requests…"));
        }
        if s.prs.is_empty() && s.pr_item_errors.is_empty() {
            return list.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .px_4()
                    .py_6()
                    .child(div().text_sm().child("No pull requests"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .text_center()
                            .child("Open, merged and closed PRs for this project appear here."),
                    ),
            );
        }

        for (ix, entry) in s.prs.iter().enumerate() {
            let key = pr_key(entry);
            let expanded = s.expanded.as_deref() == Some(key.as_str());
            let state = entry["state"].as_str().unwrap_or("open").to_string();
            let is_draft = entry["isDraft"].as_bool() == Some(true);
            let (glyph, glyph_color) = pr_state_glyph(&state, is_draft);
            let number = entry["number"].as_i64().unwrap_or(0);
            let title = entry["title"].as_str().unwrap_or("").to_string();
            let repo = entry["repository"].as_str().unwrap_or("").to_string();
            let head = entry["headBranch"].as_str().unwrap_or("").to_string();
            let base = entry["baseBranch"].as_str().unwrap_or("").to_string();
            let author = entry["author"]["login"].as_str().unwrap_or("").to_string();
            let updated = relative_time(entry["updatedAt"].as_str().unwrap_or(""));
            let additions = entry["additions"].as_i64().unwrap_or(0);
            let deletions = entry["deletions"].as_i64().unwrap_or(0);
            let review = entry["reviewDecision"].as_str().map(str::to_owned);
            let review_requested = entry["viewerReviewRequested"].as_bool() == Some(true);

            let mut top = div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(format!("#{number}")),
                )
                .child(div().flex_1().min_w_0().truncate().text_sm().child(title));
            if review_requested {
                top = top.child(chip("review"));
            }
            if let Some(rd) = &review {
                let (label, color) = review_decision_chip(rd);
                top = top.child(div().text_xs().text_color(color).child(label));
            }
            if additions + deletions > 0 {
                top = top.child(
                    div()
                        .flex()
                        .gap_1()
                        .text_xs()
                        .child(
                            div()
                                .text_color(check_pass_color())
                                .child(format!("+{additions}")),
                        )
                        .child(
                            div()
                                .text_color(pr_closed_color())
                                .child(format!("−{deletions}")),
                        ),
                );
            }

            let mut sub = div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_xs()
                .text_color(theme::fg_muted())
                .child(div().flex_none().child(repo))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(format!("{head} → {base}")),
                );
            if !author.is_empty() {
                sub = sub.child(div().flex_none().child(author));
            }
            if !updated.is_empty() {
                sub = sub.child(div().flex_none().child(updated));
            }

            let key_click = key.clone();
            let mut row = div()
                .id(("vcs-pr", ix))
                .flex()
                .items_start()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .when(expanded, |d| d.bg(theme::selected()))
                .when(!expanded, |d| d.hover(|h| h.bg(theme::hover())))
                .child(
                    div()
                        .pt_0p5()
                        .text_sm()
                        .text_color(glyph_color)
                        .child(glyph),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(top)
                        .child(sub),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        this.vcs_toggle_pr(key_click.clone(), cx);
                    }),
                );
            if expanded {
                row = row.child(div().text_xs().text_color(theme::fg_muted()).child("▾"));
            }
            let mut group = div().flex().flex_col().child(row);
            if expanded {
                group = group.child(self.render_pr_detail(entry, cx));
            }
            list = list.child(group);
        }
        list
    }

    /// Inline detail block under an expanded PR row.
    fn render_pr_detail(&self, entry: &Value, cx: &mut Context<Self>) -> Div {
        let s = &self.vcs_state;
        let key = pr_key(entry);
        let mut block = div()
            .mx_2()
            .mb_1()
            .mt_0p5()
            .rounded_sm()
            .bg(theme::hover())
            .px_2()
            .py_1p5()
            .flex()
            .flex_col()
            .gap_1();

        if s.detail_key.as_deref() != Some(key.as_str()) {
            return block;
        }
        if s.detail_loading && s.detail.is_none() {
            return block.child(
                div()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Loading details…"),
            );
        }
        if let Some(err) = &s.detail_error {
            block = block.child(
                div()
                    .text_xs()
                    .text_color(pr_closed_color())
                    .child(err.clone()),
            );
        }
        if let Some(d) = &s.detail {
            let state = d["state"].as_str().unwrap_or("open");
            let is_draft = d["isDraft"].as_bool() == Some(true);
            let (_, state_color) = pr_state_glyph(state, is_draft);
            let mut line1 = div().flex().items_center().gap_2().text_xs().child(
                div()
                    .text_color(state_color)
                    .font_weight(FontWeight::MEDIUM)
                    .child(pr_state_label(state, is_draft)),
            );
            match d["mergeability"].as_str() {
                Some("conflicting") => {
                    line1 = line1.child(div().text_color(pr_closed_color()).child("conflicts"))
                }
                Some("mergeable") => {
                    line1 = line1.child(div().text_color(check_pass_color()).child("mergeable"))
                }
                _ => {}
            }
            let additions = d["additions"].as_i64().unwrap_or(0);
            let deletions = d["deletions"].as_i64().unwrap_or(0);
            let changed = d["changedFiles"].as_i64().unwrap_or(0);
            if additions + deletions > 0 || changed > 0 {
                line1 = line1.child(
                    div()
                        .text_color(theme::fg_muted())
                        .child(format!("{changed} files +{additions} −{deletions}")),
                );
            }
            let updated = relative_time(d["updatedAt"].as_str().unwrap_or(""));
            if !updated.is_empty() {
                line1 = line1.child(div().ml_auto().text_color(theme::fg_muted()).child(updated));
            }
            block = block.child(line1);

            let head = d["headBranch"].as_str().unwrap_or("");
            let base = d["baseBranch"].as_str().unwrap_or("");
            block = block.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(format!("{head} → {base}")),
            );

            let author = d["author"]["login"].as_str().unwrap_or("");
            let reviewers: Vec<String> = d["reviewers"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|r| r["login"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if !author.is_empty() || !reviewers.is_empty() {
                let who = if reviewers.is_empty() {
                    format!("author {author}")
                } else {
                    format!("author {author} · review {}", reviewers.join(", "))
                };
                block = block.child(div().text_xs().text_color(theme::fg_muted()).child(who));
            }

            let labels: Vec<String> = d["labels"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|l| l["name"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if !labels.is_empty() {
                let mut chips = div().flex().flex_wrap().gap_1();
                for label in labels {
                    chips = chips.child(chip(&label));
                }
                block = block.child(chips);
            }

            let checks = d["checks"].as_array().cloned().unwrap_or_default();
            let check_rows = checks.len().min(8);
            let mut checks_box = div().flex().flex_col().child(
                div()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(format!("Checks · {}", summarize_checks(&checks))),
            );
            for c in checks.iter().take(check_rows) {
                let status = c["status"].as_str().unwrap_or("");
                let (g, color) = check_glyph(status);
                let name = c["name"].as_str().unwrap_or("");
                checks_box = checks_box.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_xs()
                        .child(div().w(px(12.)).text_color(color).child(g))
                        .child(div().flex_1().min_w_0().truncate().child(name.to_string())),
                );
            }
            if checks.len() > check_rows {
                checks_box = checks_box.child(
                    div()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(format!("… {} more", checks.len() - check_rows)),
                );
            }
            block = block.child(checks_box);

            let body = d["body"].as_str().unwrap_or("").trim().to_string();
            if !body.is_empty() {
                let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
                let preview: String = flat.chars().take(160).collect();
                block = block.child(div().text_xs().text_color(theme::fg_muted()).child(
                    if flat.chars().count() > 160 {
                        format!("{preview}…")
                    } else {
                        preview
                    },
                ));
            }
        }

        if let Some(threads) = &s.linked_threads {
            if !threads.is_empty() {
                let mut box_ = div().flex().flex_col().child(
                    div()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child("Linked threads"),
                );
                for (ix, t) in threads.iter().take(6).enumerate() {
                    let tid = t["id"].as_str().unwrap_or("").to_string();
                    let title = t["title"].as_str().unwrap_or("Untitled").to_string();
                    box_ = box_.child(
                        div()
                            .id(("vcs-linked-thread", ix))
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .px_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_xs()
                            .hover(|h| h.bg(theme::hover()))
                            .child(div().text_color(theme::primary()).child("↗"))
                            .child(div().flex_1().min_w_0().truncate().child(title))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    // Same body as lib.rs's private select_thread.
                                    this.thread_items.clear();
                                    this.turn_started_at = None;
                                    if let Some(commands) = &this.commands {
                                        let _ = commands.send(ClientCommand::SubscribeThread {
                                            thread_id: tid.clone(),
                                        });
                                    }
                                    this.state.selected = Some(tid.clone().into());
                                    cx.notify();
                                }),
                            ),
                    );
                }
                block = block.child(box_);
            }
        }

        if let Some(url) = entry["url"].as_str().map(str::to_owned).or_else(|| {
            s.detail
                .as_ref()
                .and_then(|d| d["url"].as_str().map(str::to_owned))
        }) {
            block = block.child(
                div()
                    .id("vcs-pr-open")
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_1()
                    .py_0p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(theme::primary())
                    .hover(|h| h.bg(theme::hover()))
                    .child("Open on GitHub")
                    .child("↗")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_this, _ev, _window, cx| {
                            cx.open_url(&url);
                        }),
                    ),
            );
        }
        block
    }
}

/// Small muted badge used for "default"/"remote"/"worktree"/"review"/labels.
fn chip(text: &str) -> Div {
    div()
        .px_1p5()
        .py_0p5()
        .rounded_sm()
        .bg(theme::selected())
        .text_xs()
        .text_color(theme::fg_muted())
        .child(text.to_string())
}

/// Muted one-line status inside bordered blocks (loading/empty/not-a-repo).
fn muted_status_row(text: &str) -> Div {
    div()
        .px_2()
        .py_2()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(text.to_string())
}
