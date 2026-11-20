//! Toast notifications (P1): transient cards for connection changes, turn
//! completion/failure, and pending approvals. Driven by observe_shell_event,
//! called from AppView::apply_shell_event for every ShellEvent.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, IntoElement, MouseButton, Rgba, SharedString, Svg, deferred, div,
    prelude::*, px, rgb, svg,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::icons::{Icon, icon};
use crate::model::ConnectionStatus;
use crate::net::ShellEvent;
use crate::{AppView, theme};

/// Cap on visible toasts; the oldest is dropped when a new one arrives.
const MAX_TOASTS: usize = 4;
/// How long a toast stays on screen before it removes itself.
const AUTO_DISMISS_SECS: u64 = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToastKind {
    Info,
    Success,
    Error,
    Approval,
}

/// Lucide `circle-check` — not in the bundled icon set; the web toast's
/// success glyph (CircleCheckIcon).
const CIRCLE_CHECK_SVG: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><circle cx=\"12\" cy=\"12\" r=\"10\"/><path d=\"m9 12 2 2 4-4\"/></svg>";

impl ToastKind {
    /// Semantic icon tint — the web toast's `--info`/`--success`/etc.
    /// foreground colors (sonner-style leading glyph).
    fn icon_color(self) -> Rgba {
        match self {
            ToastKind::Info => theme::info(),
            ToastKind::Success => theme::success_fg(),
            ToastKind::Error => rgb(0xf87171),
            ToastKind::Approval => rgb(0xfbbf24),
        }
    }

    /// Leading glyph — the web toast's per-kind lucide icon (error →
    /// CircleAlert, info → Info, success → CircleCheck, warning →
    /// TriangleAlert).
    fn icon(self) -> Svg {
        let color = self.icon_color();
        match self {
            ToastKind::Info => icon(Icon::Info, 15.),
            ToastKind::Success => svg().data(CIRCLE_CHECK_SVG).w(px(15.)).h(px(15.)),
            ToastKind::Error => icon(Icon::CircleAlert, 15.),
            ToastKind::Approval => icon(Icon::TriangleAlert, 15.),
        }
        .text_color(color)
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
    /// Open an external URL via `xdg-open` (GitHub repo, release notes).
    OpenUrl {
        url: SharedString,
        label: SharedString,
    },
}

impl ToastAction {
    /// Button label. The undo actions share one; URL actions carry their own.
    fn label(&self) -> SharedString {
        match self {
            ToastAction::OpenUrl { label, .. } => label.clone(),
            _ => "Undo".into(),
        }
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
    /// `has_pending_request` last seen per thread — the shell stream is the
    /// only way a non-selected (e.g. delegated child) thread's pending
    /// approval reaches this client, so edges are tracked here.
    pub pending_requests: HashSet<String>,
    /// `bedtimeReminder.subscribe` stream bookkeeping.
    pub bedtime_stream_started: bool,
    pub bedtime_stream_id: Option<String>,
    /// `nightStartedAt` values already toasted (dedupe across reconnects).
    pub bedtime_reminded: HashSet<String>,
    /// Once-per-session prompts (no persisted flag — `config.rs` belongs to
    /// another workstream; matches the web's once-per-session part).
    pub star_prompt_done: bool,
    pub update_toast_done: bool,
    pub legacy_fold_toasted: bool,
    /// Reentrancy guard for the `server.getConfig` probe.
    pub config_probe_running: bool,
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

/// Deliver an OS-level alert per `ClientConfig.notification_mode`
/// (`off`/`notifications`/`sound`/`notifications-and-sound`). Best-effort:
/// silently no-ops when the platform helpers are missing (e.g. headless).
fn os_notify(mode: &str, title: &str, body: Option<&str>) {
    if mode.contains("notifications") {
        let mut cmd = std::process::Command::new("notify-send");
        cmd.arg(title);
        if let Some(b) = body {
            cmd.arg(b);
        }
        let _ = cmd.spawn();
    }
    if mode.contains("sound") {
        for (bin, args) in [
            ("canberra-gtk-play", vec!["-i", "complete"]),
            (
                "paplay",
                vec!["/usr/share/sounds/freedesktop/stereo/complete.oga"],
            ),
            (
                "aplay",
                vec!["/usr/share/sounds/freedesktop/stereo/complete.wav"],
            ),
        ] {
            if std::process::Command::new(bin).args(&args).spawn().is_ok() {
                break;
            }
        }
    }
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

    /// A toast that stays until dismissed — for calls to action the user
    /// must be able to click (web parity: star/update prompts never
    /// auto-expire).
    pub(crate) fn push_sticky_toast_with_action(
        &mut self,
        kind: ToastKind,
        title: impl Into<SharedString>,
        detail: Option<String>,
        thread_id: Option<SharedString>,
        action: Option<ToastAction>,
        cx: &mut Context<Self>,
    ) {
        self.notifications_state
            .push(kind, title, detail, thread_id, action);
        cx.notify();
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
            ToastAction::OpenUrl { url, .. } => {
                crate::explorer::open_external(url);
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
                // (Re)opened socket: reconnect the reminder stream and re-run
                // the one-shot config probes; the guards inside keep them
                // once-per-session / deduped across reconnects.
                self.ensure_bedtime_stream(cx);
                self.probe_server_config(cx);
                self.maybe_prompt_github_star(cx);
            }
            ShellEvent::Disconnected(reason) => {
                // Dynamic stream subscriptions die with the socket; drop the
                // sink so the recv loop ends and a reconnect can reopen it.
                if let Some(sid) = self.notifications_state.bedtime_stream_id.take() {
                    self.stream_sinks.remove(&sid);
                }
                self.notifications_state.bedtime_stream_started = false;
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
                    // Pending requests present at snapshot are the baseline,
                    // not something new — seed without toasting.
                    if thread.has_pending_request {
                        self.notifications_state
                            .pending_requests
                            .insert(thread.id.clone());
                    } else {
                        self.notifications_state.pending_requests.remove(&thread.id);
                    }
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
                self.notifications_state.pending_requests.remove(thread_id);
            }
            ShellEvent::ThreadUpserted { thread } => {
                let previous = self
                    .notifications_state
                    .thread_status
                    .insert(thread.id.clone(), thread.status.clone());
                // Track pending-request edges regardless of selection: a
                // delegated child's approval only reaches us via its own
                // shell row, never a turn item on the parent's stream.
                let was_pending = self
                    .notifications_state
                    .pending_requests
                    .contains(thread.id.as_str());
                if thread.deleted || !thread.has_pending_request {
                    self.notifications_state.pending_requests.remove(&thread.id);
                } else {
                    self.notifications_state
                        .pending_requests
                        .insert(thread.id.clone());
                }
                if thread.deleted
                    || self
                        .state
                        .selected
                        .as_ref()
                        .is_some_and(|id| id.as_ref() == thread.id)
                {
                    return;
                }
                if !was_pending && thread.has_pending_request {
                    // `pendingRuntimeRequest.kind` isn't in the shell view;
                    // a subagent item pointing at this thread as
                    // `childThreadId` marks it a delegated-task request.
                    let delegated = self.thread_items.iter().any(|i| {
                        i.kind == "subagent"
                            && i.raw["childThreadId"].as_str() == Some(thread.id.as_str())
                    });
                    let title = if delegated {
                        "Approval needed in delegated task"
                    } else {
                        "Needs attention"
                    };
                    let detail = truncate(&thread.title, 60);
                    os_notify(&self.client_config.notification_mode, title, Some(&detail));
                    if self.in_app_notifications_enabled() {
                        self.push_toast(
                            ToastKind::Approval,
                            title,
                            Some(detail),
                            Some(thread.id.clone().into()),
                            cx,
                        );
                    }
                    return;
                }
                // The user is already watching the selected thread; only a
                // live→settled transition in the background is worth alerting.
                if !previous.as_deref().is_some_and(is_live_status) {
                    return;
                }
                let (title, kind) = match thread.status.as_str() {
                    "completed" => ("Turn completed", ToastKind::Success),
                    "failed" => ("Turn failed", ToastKind::Error),
                    _ => return,
                };
                let detail = truncate(&thread.title, 60);
                os_notify(&self.client_config.notification_mode, title, Some(&detail));
                if !self.in_app_notifications_enabled() {
                    return;
                }
                self.push_toast(
                    kind,
                    title,
                    Some(detail),
                    Some(thread.id.clone().into()),
                    cx,
                );
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
                os_notify(
                    &self.client_config.notification_mode,
                    if kind == "approval_request" {
                        "Approval needed"
                    } else {
                        "Input needed"
                    },
                    thread_title.as_deref().map(|t| truncate(t, 60)).as_deref(),
                );
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

    /// Open `bedtimeReminder.subscribe` once per connection. The stream
    /// pushes `BedtimeReminderState` (off | scheduled | reminding); only the
    /// "reminding" state toasts, deduped by `nightStartedAt` across
    /// reconnects.
    fn ensure_bedtime_stream(&mut self, cx: &mut Context<Self>) {
        if self.notifications_state.bedtime_stream_started
            && self.notifications_state.bedtime_stream_id.is_some()
        {
            return;
        }
        let Some((stream_id, rx)) = self.open_stream("bedtimeReminder.subscribe", json!({})) else {
            return;
        };
        self.notifications_state.bedtime_stream_started = true;
        self.notifications_state.bedtime_stream_id = Some(stream_id.clone());
        cx.spawn(async move |this, cx| {
            while let Ok(chunk) = rx.recv().await {
                if chunk["kind"].as_str() != Some("reminding") {
                    continue;
                }
                let night = chunk["nightStartedAt"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| chunk["nightStartedAt"].as_i64().map(|v| v.to_string()))
                    .unwrap_or_default();
                let _ = this.update(cx, |this, cx| {
                    if this.notifications_state.bedtime_reminded.insert(night) {
                        os_notify(
                            &this.client_config.notification_mode,
                            "Bedtime reminder",
                            Some("It's getting late — wrap up tonight's runs."),
                        );
                        if this.in_app_notifications_enabled() {
                            // Body click focuses the current thread — the
                            // reminder isn't tied to a specific one.
                            this.push_toast(
                                ToastKind::Info,
                                "Bedtime reminder",
                                Some("It's bedtime — consider pausing tonight's runs.".to_string()),
                                this.state.selected.clone(),
                                cx,
                            );
                        }
                    }
                    cx.notify();
                });
            }
            // Stream ended (server exit frame) or the sink was dropped on
            // disconnect — clear the id so a reconnect reopens it, but only
            // if a newer subscription hasn't already taken over.
            let _ = this.update(cx, |this, _cx| {
                if this.notifications_state.bedtime_stream_id.as_deref() == Some(stream_id.as_str())
                {
                    this.notifications_state.bedtime_stream_id = None;
                    this.notifications_state.bedtime_stream_started = false;
                }
            });
        })
        .detach();
    }

    /// One `server.getConfig` round trip per connect — feeds the
    /// update-available check and the legacy-settings-fold notice.
    fn probe_server_config(&mut self, cx: &mut Context<Self>) {
        if self.notifications_state.config_probe_running {
            return;
        }
        let Some(rx) = self.send_query("server.getConfig", json!({})) else {
            return;
        };
        self.notifications_state.config_probe_running = true;
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                this.notifications_state.config_probe_running = false;
                if exit["_tag"].as_str() == Some("Success") {
                    let value = exit["value"].clone();
                    this.handle_server_config(&value, cx);
                }
            });
        })
        .detach();
    }

    fn handle_server_config(&mut self, config: &Value, cx: &mut Context<Self>) {
        // Legacy migration notice (one-time): the server folds
        // projectAgentBrowserAccessOverrides / projectAutoPullOverrides /
        // projectScriptOverrides into the new project settings and reports
        // `settings.projectSettingsFolded: true`.
        if config["settings"]["projectSettingsFolded"].as_bool() == Some(true)
            && !self.notifications_state.legacy_fold_toasted
        {
            self.notifications_state.legacy_fold_toasted = true;
            self.push_toast(
                ToastKind::Info,
                "Project settings migrated",
                Some("Legacy per-project overrides were folded into project settings.".to_string()),
                None,
                cx,
            );
        }
        // Update check — the web only asks the registry when the server
        // advertises `capabilities.npmReleaseLookup`.
        if self.notifications_state.update_toast_done {
            return;
        }
        let capable = config["environment"]["capabilities"]["npmReleaseLookup"]
            .as_bool()
            .unwrap_or(false);
        let current = config["environment"]["serverVersion"]
            .as_str()
            .unwrap_or("");
        if !capable || current.is_empty() {
            return;
        }
        let Some(rx) = self.send_query("server.getNpmRelease", json!({"spec": "latest"})) else {
            return;
        };
        let current = current.to_string();
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let latest = exit["value"]["version"].as_str().unwrap_or("").to_string();
            let _ = this.update(cx, |this, cx| {
                if exit["_tag"].as_str() == Some("Success")
                    && semver_newer(&latest, &current)
                    && !this.notifications_state.update_toast_done
                {
                    this.notifications_state.update_toast_done = true;
                    this.push_sticky_toast_with_action(
                        ToastKind::Info,
                        "Update available",
                        Some(format!(
                            "v{current} → v{latest} — run npm/pnpm update on the server"
                        )),
                        None,
                        None,
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// Web `GitHubStarPrompt` parity, best-effort: the web gates on >5
    /// prompts sent, idle, and an unstarred repo check; persists "sent" /
    /// "dismissed" to localStorage. Native has no persisted flag yet
    /// (`config.rs` is another workstream's file), so this is once per
    /// session and the prompt-count gate reads the session's
    /// `prompt_history`.
    fn maybe_prompt_github_star(&mut self, cx: &mut Context<Self>) {
        if self.notifications_state.star_prompt_done {
            return;
        }
        let prompts_sent: usize = self
            .client_config
            .prompt_history
            .values()
            .map(Vec::len)
            .sum();
        if prompts_sent <= 5 {
            return;
        }
        self.notifications_state.star_prompt_done = true;
        self.push_sticky_toast_with_action(
            ToastKind::Info,
            "Enjoying 67 Code?",
            Some("It helps a lot if you star the repo.".to_string()),
            None,
            Some(ToastAction::OpenUrl {
                url: "https://github.com/Coobyk/67code".into(),
                label: "Star on GitHub".into(),
            }),
            cx,
        );
    }

    /// Transient toast stack — web `ToastProvider` default "top-right":
    /// `top = --toast-inset + --workspace-topbar-height` (32 + 52 = 84px),
    /// `right = --toast-inset` (32px), max width 360px.
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
                .top(px(84.))
                .right(px(32.))
                .flex()
                .flex_col()
                .items_end()
                .gap_2()
                .children(toasts),
        )
        .with_priority(1)
    }

    /// One toast — web `Toast.Content`: `dropdown-glass rounded-lg shadow-xl`
    /// card with a semantic leading icon (per-kind lucide, sonner-style),
    /// `font-medium` title, muted description, xs primary action button, and
    /// the corner `bg-popover/92` dismiss orb at `-top-1.5 -right-1.5`.
    fn render_toast(&self, toast: &Toast, cx: &mut Context<Self>) -> AnyElement {
        let dismiss_id = toast.id;
        let mut card = div()
            .id(("toast", toast.id as usize))
            .w(px(360.))
            .relative()
            .flex()
            .rounded_lg()
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border())
            .shadow_xl()
            .occlude()
            .child(
                div()
                    .pl_3p5()
                    .pt_2p5()
                    .flex_none()
                    .text_color(toast.kind.icon_color())
                    .child(toast.kind.icon()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pl_2()
                    .pr_3()
                    .py_2p5()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::fg())
                            .truncate()
                            .child(toast.title.clone()),
                    )
                    .when_some(toast.detail.clone(), |d, detail| {
                        d.child(
                            div()
                                .text_sm()
                                .text_color(theme::fg_muted())
                                .truncate()
                                .child(detail),
                        )
                    })
                    .when_some(toast.action.clone(), |d, action| {
                        let action_label = action.label().to_string();
                        d.child(
                            // Web `Button size=xs` default variant — primary
                            // fill + `hover:bg-primary/90`.
                            div()
                                .id(("toast-action", toast.id as usize))
                                .h(px(24.))
                                .flex()
                                .items_center()
                                .px_2()
                                .mt_1()
                                .self_start()
                                .rounded_md()
                                .bg(theme::primary())
                                .text_xs()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(gpui::white())
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::primary_hover()))
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
                // Web `ToastCloseButton`: 24px popover/92 orb with a
                // border/60 hairline pinned at the card's top-right corner.
                div()
                    .id(("toast-dismiss", toast.id as usize))
                    .absolute()
                    .top(px(-6.))
                    .right(px(-6.))
                    .w(px(24.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(theme::toast_orb_border())
                    .bg(theme::popover_92())
                    .cursor_pointer()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.text_color(theme::fg()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _event, _window, cx| {
                            cx.stop_propagation();
                            this.dismiss_toast(dismiss_id, cx);
                        }),
                    )
                    .child(icon(Icon::X, 11.).text_color(theme::fg_muted())),
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

/// `X.Y.Z[-pre]` → comparable parts. `serverVersion` can be a dev-build
/// label rather than semver — `None` there just means "no nag".
fn semver_parts(v: &str) -> Option<(u64, u64, u64, String)> {
    let (core, pre) = match v.trim().split_once('-') {
        Some((c, p)) => (c, p),
        None => (v.trim(), ""),
    };
    let mut it = core.split('.');
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    let c = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((a, b, c, pre.to_string()))
}

/// `latest` is strictly newer than `current`; a stable release also beats
/// the same version's prerelease (npm `latest` vs a `-dev`/`-nightly` build).
fn semver_newer(latest: &str, current: &str) -> bool {
    match (semver_parts(latest), semver_parts(current)) {
        (Some(l), Some(c)) => {
            (l.0, l.1, l.2) > (c.0, c.1, c.2)
                || ((l.0, l.1, l.2) == (c.0, c.1, c.2) && l.3.is_empty() && !c.3.is_empty())
        }
        _ => false,
    }
}
