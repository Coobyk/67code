//! Toast notifications (P1): transient cards for connection changes, turn
//! completion/failure, and pending approvals. Driven by observe_shell_event,
//! called from AppView::apply_shell_event for every ShellEvent.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, IntoElement, MouseButton, Rgba, SharedString, deferred, div, prelude::*,
    px, rgb,
};
use serde_json::json;
use uuid::Uuid;

use crate::model::ConnectionStatus;
use crate::net::ShellEvent;
use crate::{AppView, theme};

/// Cap on visible toasts; the oldest is dropped when a new one arrives.
const MAX_TOASTS: usize = 4;
/// How long a toast stays on screen before it removes itself.
const AUTO_DISMISS_SECS: u64 = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToastKind {
    /// Reserved for neutral notices; no event arm produces one yet.
    #[allow(dead_code)]
    Info,
    Success,
    Error,
    Approval,
}

impl ToastKind {
    /// Left accent-bar color; mirrors the sidebar status-dot palette.
    fn accent(self) -> Rgba {
        match self {
            ToastKind::Info => theme::primary(),
            ToastKind::Success => rgb(0x34d399),
            ToastKind::Error => rgb(0xf87171),
            ToastKind::Approval => rgb(0xfbbf24),
        }
    }
}

/// Action button on a toast. An enum (not a closure) so the click handler can
/// match and call back into `AppView` methods — the same pattern as
/// `ToastKind`.
#[derive(Clone, Debug)]
pub enum ToastAction {
    /// "Undo" an archive — `thread.unarchive` exists on the wire (delete has
    /// no un-delete command, so delete toasts stay plain notices).
    UnarchiveThread { thread_id: SharedString },
    /// "Undo" a snooze — `thread.unsnooze`.
    UnsnoozeThread { thread_id: SharedString },
}

impl ToastAction {
    /// Button label. Both current actions undo something.
    fn label(&self) -> &'static str {
        "Undo"
    }
}

pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub title: SharedString,
    pub detail: Option<SharedString>,
    /// Thread focused when the toast body is clicked, when the toast is
    /// about one.
    pub thread_id: Option<SharedString>,
    /// Optional action button (e.g. Undo archive).
    pub action: Option<ToastAction>,
    /// Push time; kept for relative-age display or TTL policies.
    #[allow(dead_code)]
    pub created: Instant,
}

#[derive(Default)]
pub struct State {
    /// Visible toasts, oldest first (rendered top to bottom).
    pub toasts: VecDeque<Toast>,
    /// Last shell status seen per thread id, so `thread.updated` events can
    /// be reduced to running→settled transitions.
    pub thread_status: HashMap<String, String>,
    /// Runtime-request turn-item ids already toasted. Approval items update
    /// repeatedly while pending; only the first update should notify.
    pub requests_seen: HashSet<String>,
    next_id: u64,
}

impl State {
    fn push(
        &mut self,
        kind: ToastKind,
        title: impl Into<SharedString>,
        detail: Option<String>,
        thread_id: Option<SharedString>,
        action: Option<ToastAction>,
    ) -> u64 {
        let title: SharedString = title.into();
        let detail: Option<SharedString> = detail.map(Into::into);
        // Reconnect retries and repeated events would otherwise churn the
        // stack: suppress a toast identical to the newest visible one.
        if let Some(newest) = self.toasts.back()
            && newest.kind == kind
            && newest.title == title
            && newest.detail == detail
        {
            return newest.id;
        }
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;
        while self.toasts.len() >= MAX_TOASTS {
            self.toasts.pop_front();
        }
        self.toasts.push_back(Toast {
            id,
            kind,
            title,
            detail,
            thread_id,
            action,
            created: Instant::now(),
        });
        id
    }
}

/// `OrchestrationV2ShellThreadStatus` values that mean a run is in flight
/// (`LIVE_SHELL_STATUSES` in packages/contracts/src/orchestrationV2.ts).
fn is_live_status(status: &str) -> bool {
    matches!(
        status,
        "preparing" | "queued" | "starting" | "running" | "waiting"
    )
}

/// Single-line, `max`-char summary of a wire string for the toast detail.
fn truncate(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat
    }
}

impl AppView {
    pub(crate) fn push_toast(
        &mut self,
        kind: ToastKind,
        title: impl Into<SharedString>,
        detail: Option<String>,
        thread_id: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.push_toast_with_action(kind, title, detail, thread_id, None, cx);
    }

    /// `push_toast` plus an action button (e.g. "Undo" for archive/snooze).
    pub(crate) fn push_toast_with_action(
        &mut self,
        kind: ToastKind,
        title: impl Into<SharedString>,
        detail: Option<String>,
        thread_id: Option<SharedString>,
        action: Option<ToastAction>,
        cx: &mut Context<Self>,
    ) {
        let id = self
            .notifications_state
            .push(kind, title, detail, thread_id, action);
        cx.notify();
        // Auto-dismiss: remove this id after the timeout (a no-op if the
        // toast was already dismissed or evicted).
        cx.spawn(async move |this, cx| {
            smol::Timer::after(Duration::from_secs(AUTO_DISMISS_SECS)).await;
            let _ = this.update(cx, |this, cx| {
                this.notifications_state.toasts.retain(|t| t.id != id);
                cx.notify();
            });
        })
        .detach();
    }

    fn dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        self.notifications_state.toasts.retain(|t| t.id != id);
        cx.notify();
    }

    /// Run a toast's action button. Each arm dispatches the matching
    /// orchestration command and applies the same optimistic update as the
    /// sidebar paths (the server's `thread.updated` is the source of truth).
    fn run_toast_action(&mut self, action: &ToastAction, cx: &mut Context<Self>) {
        match action {
            ToastAction::UnarchiveThread { thread_id } => {
                self.dispatch(json!({
                    "type": "thread.unarchive",
                    "commandId": Uuid::new_v4().to_string(),
                    "threadId": thread_id.to_string(),
                }));
            }
            ToastAction::UnsnoozeThread { thread_id } => {
                if let Some(thread) = self
                    .state
                    .threads
                    .iter_mut()
                    .find(|t| t.id.as_ref() == thread_id.as_ref())
                {
                    thread.snoozed_until = None;
                }
                self.sidebar_state.snooze_target = None;
                self.dispatch(json!({
                    "type": "thread.unsnooze",
                    "commandId": Uuid::new_v4().to_string(),
                    "threadId": thread_id.to_string(),
                    "reason": "user",
                }));
            }
        }
        cx.notify();
    }

    /// Called for every ShellEvent before the main match in
    /// `apply_shell_event`. `self.state` still holds the pre-event values, so
    /// connection/status checks here see transitions rather than snapshots.
    pub fn observe_shell_event(&mut self, event: &ShellEvent, cx: &mut Context<Self>) {
        match event {
            ShellEvent::Connected => {
                if !matches!(self.state.connection, ConnectionStatus::Connected) {
                    self.push_toast(ToastKind::Success, "Connected", None, None, cx);
                }
            }
            ShellEvent::Disconnected(reason) => {
                self.push_toast(
                    ToastKind::Error,
                    "Connection lost",
                    Some(truncate(reason, 80)),
                    None,
                    cx,
                );
            }
            ShellEvent::Error(e) => {
                self.push_toast(ToastKind::Error, "Error", Some(truncate(e, 80)), None, cx);
            }
            // Seed the transition tracker without toasting; snapshots and
            // backfills describe history, not something that just happened.
            ShellEvent::Snapshot(snapshot) => {
                self.sidebar_state.icons_queried = false;
                for thread in &snapshot.threads {
                    self.notifications_state
                        .thread_status
                        .insert(thread.id.clone(), thread.status.clone());
                }
            }
            // Project shells carry `icon` (net::parse_project drops it); the
            // sidebar re-harvests icons after any event that can change one.
            ShellEvent::ProjectUpserted(_) | ShellEvent::ProjectsRefreshed { .. } => {
                self.sidebar_state.icons_queried = false;
            }
            ShellEvent::OlderSettledThreads { threads } => {
                for thread in threads {
                    self.notifications_state
                        .thread_status
                        .entry(thread.id.clone())
                        .or_insert_with(|| thread.status.clone());
                }
            }
            ShellEvent::ThreadRemoved { thread_id } => {
                self.notifications_state.thread_status.remove(thread_id);
            }
            ShellEvent::ThreadUpserted { thread } => {
                let previous = self
                    .notifications_state
                    .thread_status
                    .insert(thread.id.clone(), thread.status.clone());
                if thread.deleted
                    || self
                        .state
                        .selected
                        .as_ref()
                        .is_some_and(|id| id.as_ref() == thread.id)
                {
                    return;
                }
                // The user is already watching the selected thread; only a
                // live→settled transition in the background is worth a toast.
                if !previous.as_deref().is_some_and(is_live_status)
                    || !self.in_app_notifications_enabled()
                {
                    return;
                }
                match thread.status.as_str() {
                    "completed" => self.push_toast(
                        ToastKind::Success,
                        "Turn completed",
                        Some(truncate(&thread.title, 60)),
                        Some(thread.id.clone().into()),
                        cx,
                    ),
                    "failed" => self.push_toast(
                        ToastKind::Error,
                        "Turn failed",
                        Some(truncate(&thread.title, 60)),
                        Some(thread.id.clone().into()),
                        cx,
                    ),
                    _ => {}
                }
            }
            ShellEvent::ThreadItemUpdated { item } => {
                let kind = item.kind.as_str();
                if kind != "approval_request" && kind != "user_input_request" {
                    return;
                }
                // Resolved/echoed updates still arrive; only notify while the
                // request is actually unanswered, and only once per item.
                if !matches!(item.status.as_str(), "waiting" | "pending" | "running") {
                    return;
                }
                if !self
                    .notifications_state
                    .requests_seen
                    .insert(item.id.clone())
                {
                    return;
                }
                let thread_id = item.raw["threadId"].as_str().unwrap_or_default();
                if self
                    .state
                    .selected
                    .as_ref()
                    .is_some_and(|id| id.as_ref() == thread_id)
                {
                    return;
                }
                let thread_title = self
                    .state
                    .threads
                    .iter()
                    .find(|t| t.id.as_ref() == thread_id)
                    .map(|t| t.title.to_string())
                    .filter(|t| !t.is_empty());
                self.push_toast(
                    ToastKind::Approval,
                    if kind == "approval_request" {
                        "Approval needed"
                    } else {
                        "Input needed"
                    },
                    thread_title.map(|t| truncate(&t, 60)),
                    if thread_id.is_empty() {
                        None
                    } else {
                        Some(thread_id.to_string().into())
                    },
                    cx,
                );
            }
            _ => {}
        }
    }

    /// Transient toast stack rendered above the composer, bottom-right.
    pub fn render_notifications(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let toasts: Vec<AnyElement> = self
            .notifications_state
            .toasts
            .iter()
            .map(|toast| self.render_toast(toast, cx))
            .collect();
        deferred(
            div()
                .absolute()
                .right_4()
                .bottom(px(140.))
                .flex()
                .flex_col()
                .items_end()
                .gap_2()
                .children(toasts),
        )
        .with_priority(1)
    }

    fn render_toast(&self, toast: &Toast, cx: &mut Context<Self>) -> AnyElement {
        let dismiss_id = toast.id;
        let mut card = div()
            .id(("toast", toast.id as usize))
            .w(px(320.))
            .flex()
            .rounded_md()
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border())
            .overflow_hidden()
            .occlude()
            .child(div().w(px(3.)).flex_shrink_0().bg(toast.kind.accent()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::fg())
                            .truncate()
                            .child(toast.title.clone()),
                    )
                    .when_some(toast.detail.clone(), |d, detail| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .truncate()
                                .child(detail),
                        )
                    })
                    .when_some(toast.action.clone(), |d, action| {
                        let action_label = action.label().to_string();
                        d.child(
                            div()
                                .id(("toast-action", toast.id as usize))
                                .px_2()
                                .py_0p5()
                                .mt_0p5()
                                .self_start()
                                .rounded_sm()
                                .text_xs()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::primary())
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::hover()))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _event, _window, cx| {
                                        cx.stop_propagation();
                                        this.run_toast_action(&action, cx);
                                        this.dismiss_toast(dismiss_id, cx);
                                    }),
                                )
                                .child(action_label),
                        )
                    }),
            )
            .child(
                div()
                    .id(("toast-dismiss", toast.id as usize))
                    .px_2()
                    .py_1()
                    .flex_shrink_0()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.text_color(theme::fg()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _event, _window, cx| {
                            cx.stop_propagation();
                            this.dismiss_toast(dismiss_id, cx);
                        }),
                    )
                    .child("×"),
            );

        // Toasts about a thread navigate to it when the body is clicked.
        if let Some(thread_id) = toast.thread_id.clone() {
            card = card.cursor_pointer().on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event, _window, cx| {
                    this.dismiss_toast(dismiss_id, cx);
                    this.select_thread(thread_id.clone(), cx);
                }),
            );
        }
        card.into_any_element()
    }
}
