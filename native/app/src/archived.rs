//! Archived threads settings section (P2): list archived threads grouped by
//! project, with unarchive / delete actions — mirroring
//! ArchivedThreadsPanel.tsx. Data comes from the
//! `orchestration.getArchivedShellSnapshot` query RPC; mutations are flat
//! `orchestration.dispatchCommand` payloads (`thread.unarchive` /
//! `thread.delete`). Delete is destructive, so its button is two-stage:
//! first click arms the row ("Confirm delete"), second dispatches — the same
//! pattern palette.rs uses for `delete_confirm`.

use std::cell::Cell;
use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{Context, IntoElement, MouseButton, div, prelude::*, rgb};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppView;
use crate::theme;

/// Request/response RPC tag for the archived shell snapshot.
const ARCHIVED_SNAPSHOT_TAG: &str = "orchestration.getArchivedShellSnapshot";

/// Red used for destructive labels across the app (palette/sidebar danger).
fn danger() -> gpui::Rgba {
    rgb(0xf87171)
}

/// Delay before re-querying after a dispatched command so the server has
/// applied it by the time the snapshot is read back.
const POST_ACTION_REFRESH: Duration = Duration::from_millis(400);

/// One archived thread row, parsed from the snapshot's thread shells.
#[derive(Default)]
pub struct ArchivedThread {
    pub id: String,
    pub project_id: String,
    pub title: String,
    /// `archivedAt` ISO instant (empty when absent — falls back to created).
    pub archived_at: String,
    /// `createdAt` ISO instant.
    pub created_at: String,
}

/// Project shell subset needed for group titles.
#[derive(Default)]
pub struct ArchivedProject {
    pub id: String,
    pub title: String,
}

#[derive(Default)]
pub struct State {
    /// Archived threads, sorted newest-archive-first.
    pub threads: Vec<ArchivedThread>,
    /// Projects from the same snapshot (group titles + thread lookup).
    pub projects: Vec<ArchivedProject>,
    /// A snapshot query is in flight. `Cell` because the lazy-load kick runs
    /// from the `&self` render path (the section loads on first render).
    pub loading: Cell<bool>,
    /// Last load/refresh failure, shown as a status line.
    pub error: Option<String>,
    /// Fetched at least once (auto-load when the section first renders).
    pub loaded: Cell<bool>,
    /// Thread ids with an unarchive/delete dispatch in flight — their row
    /// buttons stay disabled until the refresh lands.
    pub pending: HashSet<String>,
    /// Thread id whose delete button is armed ("Confirm delete").
    pub delete_armed: Option<String>,
}

/// Small caption above a group of rows (settings.rs keeps its own copy).
fn group_label(text: String) -> gpui::Div {
    div()
        .pb_1()
        .pt_3()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(text)
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

/// Parse an ISO-8601 timestamp (`YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]`) into
/// seconds since the Unix epoch. None when the shape doesn't match.
fn parse_iso_epoch(s: &str) -> Option<i64> {
    if s.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let days = days_from_civil(num(0..4)?, num(5..7)?, num(8..10)?);
    let mut epoch = days * 86_400 + num(11..13)? * 3_600 + num(14..16)? * 60 + num(17..19)?;
    // Skip a fractional-second suffix, then apply the zone offset.
    let mut rest = &s[19..];
    if rest.starts_with('.') {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    if let Some(sign) = rest.strip_prefix('+').map(|_| 1i64).or_else(|| rest.strip_prefix('-').map(|_| -1))
    {
        let hours: i64 = rest.get(1..3)?.parse().ok()?;
        let minutes: i64 = rest.get(4..6).unwrap_or("0").parse().unwrap_or(0);
        epoch -= sign * (hours * 3_600 + minutes * 60);
    }
    Some(epoch)
}

/// "5m ago" / "2d ago" from an ISO timestamp; short date on parse failure.
fn relative_time(iso: &str) -> String {
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

/// A readable message out of a Failure `Exit` frame's cause.
fn exit_failure_message(exit: &Value) -> String {
    let cause = &exit["cause"];
    for candidate in [
        &cause["message"],
        &cause["error"]["message"],
        &cause["defect"],
        &cause["reason"],
        cause,
    ] {
        if let Some(s) = candidate.as_str() {
            return s.to_string();
        }
    }
    cause.to_string().chars().take(160).collect()
}

impl AppView {
    /// Kick off the snapshot query on first render of the section. With no
    /// command channel (offline mock) there is nothing to fetch — mark it
    /// loaded so the empty state shows instead of retrying every frame.
    fn maybe_load_archived(&self, cx: &mut Context<Self>) {
        let state = &self.archived_state;
        if state.loaded.get() || state.loading.get() {
            return;
        }
        match self.send_query(ARCHIVED_SNAPSHOT_TAG, json!({})) {
            Some(rx) => {
                state.loading.set(true);
                Self::await_archived_reply(rx, cx);
            }
            None => state.loaded.set(true),
        }
    }

    /// Await a snapshot query's `Exit` frame and fold it into state.
    fn await_archived_reply(rx: smol::channel::Receiver<Value>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                this.apply_archived_reply(reply);
                cx.notify();
            });
        })
        .detach();
    }

    /// Re-query the archived snapshot (Refresh button + post-action resync).
    pub fn refresh_archived(&mut self, cx: &mut Context<Self>) {
        let Some(rx) = self.send_query(ARCHIVED_SNAPSHOT_TAG, json!({})) else {
            let state = &mut self.archived_state;
            state.loading.set(false);
            state.loaded.set(true);
            state.error = Some("Not connected to a server.".to_string());
            cx.notify();
            return;
        };
        self.archived_state.error = None;
        self.archived_state.loading.set(true);
        Self::await_archived_reply(rx, cx);
        cx.notify();
    }

    /// Refresh shortly after a dispatched command so the server has applied
    /// it before the snapshot is read back.
    fn refresh_archived_soon(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            smol::Timer::after(POST_ACTION_REFRESH).await;
            let _ = this.update(cx, |this, cx| this.refresh_archived(cx));
        })
        .detach();
    }

    /// Fold the query `Exit` frame into archived state.
    fn apply_archived_reply(&mut self, reply: Result<Value, smol::channel::RecvError>) {
        let state = &mut self.archived_state;
        state.loading.set(false);
        state.loaded.set(true);
        // In-flight row actions settle once fresh data lands.
        state.pending.clear();
        state.delete_armed = None;
        match reply {
            Err(_) => {
                state.error = Some("Lost connection before the reply arrived.".to_string());
            }
            Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                let value = &exit["value"];
                state.projects = value["projects"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|p| ArchivedProject {
                                id: p["id"].as_str().unwrap_or_default().to_string(),
                                title: p["title"]
                                    .as_str()
                                    .unwrap_or("Untitled project")
                                    .to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let mut threads: Vec<ArchivedThread> = value["threads"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|t| ArchivedThread {
                                id: t["id"].as_str().unwrap_or_default().to_string(),
                                project_id: t["projectId"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                                title: t["title"].as_str().unwrap_or("Untitled").to_string(),
                                archived_at: t["archivedAt"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                                created_at: t["createdAt"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // Newest archive first; id breaks ties (mirrors the web panel).
                threads.sort_by(|a, b| {
                    let ka = if a.archived_at.is_empty() {
                        &a.created_at
                    } else {
                        &a.archived_at
                    };
                    let kb = if b.archived_at.is_empty() {
                        &b.created_at
                    } else {
                        &b.archived_at
                    };
                    kb.cmp(ka).then_with(|| b.id.cmp(&a.id))
                });
                state.threads = threads;
                state.error = None;
            }
            Ok(exit) => {
                state.error = Some(exit_failure_message(&exit));
            }
        }
    }

    /// Unarchive button: dispatch `thread.unarchive`, drop the row locally,
    /// then resync. The refresh is authoritative — a failed command brings
    /// the row back.
    fn archived_unarchive(&mut self, thread_id: String, cx: &mut Context<Self>) {
        let state = &mut self.archived_state;
        if state.pending.contains(&thread_id) {
            return;
        }
        state.pending.insert(thread_id.clone());
        state.delete_armed = None;
        state.threads.retain(|t| t.id != thread_id);
        self.dispatch(json!({
            "type": "thread.unarchive",
            "commandId": Uuid::new_v4().to_string(),
            "threadId": thread_id,
        }));
        self.refresh_archived_soon(cx);
        cx.notify();
    }

    /// Delete button: first click arms the row ("Confirm delete"), second
    /// dispatches `thread.delete` and drops the row locally before resync.
    fn archived_delete(&mut self, thread_id: String, cx: &mut Context<Self>) {
        let state = &mut self.archived_state;
        if state.pending.contains(&thread_id) {
            return;
        }
        if state.delete_armed.as_deref() != Some(thread_id.as_str()) {
            state.delete_armed = Some(thread_id);
            cx.notify();
            return;
        }
        state.delete_armed = None;
        state.pending.insert(thread_id.clone());
        state.threads.retain(|t| t.id != thread_id);
        self.dispatch(json!({
            "type": "thread.delete",
            "commandId": Uuid::new_v4().to_string(),
            "threadId": thread_id,
        }));
        self.refresh_archived_soon(cx);
        cx.notify();
    }

    fn render_archived_row(
        &self,
        thread: &ArchivedThread,
        row_ix: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let pending = self.archived_state.pending.contains(&thread.id);
        let armed = self.archived_state.delete_armed.as_deref() == Some(thread.id.as_str());
        let unarchive_id = thread.id.clone();
        let delete_id = thread.id.clone();

        let archived_rel = relative_time(if thread.archived_at.is_empty() {
            &thread.created_at
        } else {
            &thread.archived_at
        });
        let created_rel = relative_time(&thread.created_at);

        let mut unarchive = div()
            .id(("arch-unarchive", row_ix))
            .flex_none()
            .px_3()
            .py_1p5()
            .rounded_md()
            .text_xs()
            .bg(theme::hover())
            .text_color(theme::fg())
            .child("Unarchive");
        let mut delete = div()
            .id(("arch-delete", row_ix))
            .flex_none()
            .px_3()
            .py_1p5()
            .rounded_md()
            .text_xs()
            .child(if armed { "Confirm delete" } else { "Delete" });
        if pending {
            unarchive = unarchive.opacity(0.45);
            delete = delete.text_color(danger()).opacity(0.45);
        } else {
            unarchive = unarchive
                .cursor_pointer()
                .hover(|s| s.bg(theme::selected()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        cx.stop_propagation();
                        this.archived_unarchive(unarchive_id.clone(), cx);
                    }),
                );
            delete = delete
                .cursor_pointer()
                .text_color(if armed { theme::fg() } else { danger() })
                .when(armed, |s| s.bg(rgb(0x7a2e2e)))
                .when(!armed, |s| s.hover(|h| h.bg(theme::hover())))
                .when(armed, |s| s.hover(|h| h.bg(rgb(0x8f3a3a))))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        cx.stop_propagation();
                        this.archived_delete(delete_id.clone(), cx);
                    }),
                );
        }

        div()
            .id(("archived-row", row_ix))
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded_md()
            .hover(|s| s.bg(theme::hover()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::fg())
                            .whitespace_nowrap()
                            .overflow_x_hidden()
                            .text_ellipsis()
                            .child(thread.title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(format!("Archived {archived_rel} · Created {created_rel}")),
                    ),
            )
            .child(unarchive)
            .child(delete)
    }

    pub fn render_archived_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Auto-load on first render of this section.
        self.maybe_load_archived(cx);

        let state = &self.archived_state;
        let loading = state.loading.get();

        let mut section = div().flex().flex_col().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .pb_2()
                .child(
                    div()
                        .id("archived-refresh")
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .bg(theme::hover())
                        .text_color(theme::fg())
                        .when(loading, |s| s.opacity(0.5))
                        .when(!loading, |s| {
                            s.cursor_pointer()
                                .hover(|h| h.bg(theme::selected()))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _ev, _window, cx| {
                                        this.refresh_archived(cx);
                                    }),
                                )
                        })
                        .child(if loading { "Refreshing…" } else { "Refresh" }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child("Unarchive returns a thread to the sidebar; delete is permanent."),
                ),
        );

        // Group threads under their project (only projects present in the
        // snapshot get a section — mirrors the web panel).
        let mut groups_el = div().flex().flex_col();
        let mut row_ix = 0usize;
        for project in &state.projects {
            let threads: Vec<&ArchivedThread> = state
                .threads
                .iter()
                .filter(|t| t.project_id == project.id)
                .collect();
            if threads.is_empty() {
                continue;
            }
            let mut group = div().flex().flex_col().child(group_label(project.title.clone()));
            for thread in threads {
                group = group.child(self.render_archived_row(thread, row_ix, cx));
                row_ix += 1;
            }
            groups_el = groups_el.child(group);
        }

        if row_ix == 0 {
            // Single status row for the no-data cases, mirroring the web
            // panel's loading / error / empty SettingsRow.
            let (title, detail) = if loading && state.error.is_none() {
                (
                    "Loading archived threads".to_string(),
                    "Checking the server…".to_string(),
                )
            } else if let Some(err) = &state.error {
                ("Could not load archived threads".to_string(), err.clone())
            } else if self.commands.is_none() {
                (
                    "No server connected".to_string(),
                    "Archived threads live on a connected environment.".to_string(),
                )
            } else {
                (
                    "No archived threads".to_string(),
                    "Archived threads will appear here.".to_string(),
                )
            };
            section.child(
                div()
                    .px_2()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().text_color(theme::fg()).child(title))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(detail),
                    ),
            )
        } else {
            // A failed refresh keeps the stale list visible with a warning.
            if let Some(err) = &state.error {
                section = section.child(
                    div()
                        .px_2()
                        .pb_1()
                        .text_xs()
                        .text_color(danger())
                        .child(format!("Could not refresh — {err}")),
                );
            }
            section.child(groups_el)
        }
    }
}
