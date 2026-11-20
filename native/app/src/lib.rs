mod acp;
mod appearance;
mod archived;
mod clients;
mod composer;
mod config;
mod envs;
mod explorer;
mod icons;
mod integrations;
mod markdown;
mod model;
pub mod net;
mod notifications;
mod palette;
mod right_panel;
mod search_dialog;
mod settings;
mod settings_backend;
mod settings_extra;
mod settings_misc;
mod sidebar;
mod terminal_drawer;
mod text_field;
mod theme;
mod timeline;
mod vcs;
mod welcome;

use gpui::{
    App, AppContext, Bounds, Context, Decorations, Entity, Focusable, IntoElement, KeyBinding,
    Render, ScrollHandle, SharedString, Window, WindowBounds, WindowControlArea, WindowDecorations,
    WindowOptions, div, prelude::*, px, size,
};
use gpui_platform::application;
use model::{AppState, ConnectionStatus, Role};
use net::{ClientCommand, ShellEvent};
use text_field::{PromptSubmitted, TextField};

gpui::actions!(
    app,
    [
        TogglePalette,
        ToggleSettings,
        TogglePanel,
        ToggleTerminalDrawer,
        ToggleSearch,
        NewThread,
        CloseOverlay,
        FocusSearch,
        OpenFilePicker,
        ClosePanel,
        PrevThread,
        NextThread,
        TogglePin,
        ToggleModelPicker,
        OpenUsageView,
    ]
);

/// Jump to the Nth thread in sidebar order — the web `thread.jump.N`
/// bindings (`mod+1..9`, gated to the desktop app there too). Bound as
/// `ctrl+1..9`; `no_json` since keymaps here are code, not JSON.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = app, no_json)]
struct ThreadJump {
    /// 0-based index into the sidebar's flat thread order.
    index: usize,
}

/// What the center column shows — the web's `/usage` and pull-request
/// routes flattened into the main window instead of separate pages.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum CenterView {
    #[default]
    Chat,
    Usage,
    PullRequests,
}

/// How the app reaches a server and which provider new threads use.
#[derive(Clone)]
pub struct AppConfig {
    /// `None` runs the offline mock UI.
    pub endpoint: Option<net::ServerEndpoint>,
    /// modelSelection.instanceId for auto-created threads.
    pub provider: String,
    /// modelSelection.model for auto-created threads.
    pub model: String,
}

impl AppConfig {
    /// Env-driven config (standalone `c67-app` binary). Endpoint precedence:
    /// C67_SERVER_URL+C67_TOKEN, then the active saved environment from the
    /// client config, else the offline mock.
    pub fn from_env() -> Self {
        let endpoint = net::ServerEndpoint::from_env().or_else(|| {
            let cfg = config::ClientConfig::load();
            cfg.active_environment
                .and_then(|ix| cfg.environments.get(ix))
                .map(|e| net::ServerEndpoint {
                    http_url: e.http_url.clone(),
                    token: e.token.clone(),
                })
        });
        Self {
            endpoint,
            provider: std::env::var("C67_PROVIDER").unwrap_or_else(|_| "devReplay".to_string()),
            model: std::env::var("C67_MODEL").unwrap_or_else(|_| "showcase".to_string()),
        }
    }
}

struct AppView {
    state: AppState,
    composer: Entity<TextField>,
    search_field: Entity<TextField>,
    commands: Option<std::sync::mpsc::Sender<ClientCommand>>,
    thread_items: Vec<net::TurnItemView>,
    scroll_handle: ScrollHandle,
    provider: String,
    model: String,
    sidebar_state: sidebar::State,
    timeline_state: timeline::State,
    composer_state: composer::State,
    palette_state: palette::State,
    settings_state: settings::State,
    notifications_state: notifications::State,
    appearance_state: appearance::State,
    archived_state: archived::State,
    clients_state: clients::State,
    acp_state: acp::State,
    env_state: envs::State,
    /// Persisted client config (theme mode, saved environments).
    client_config: config::ClientConfig,
    /// Display-only label for the connected server (settings/connection UI).
    endpoint_label: String,
    /// Live endpoint (url + token) for authenticated HTTP calls from
    /// settings surfaces; env switching updates both this and endpoint_label.
    active_endpoint: Option<net::ServerEndpoint>,
    /// First snapshot already auto-selected a thread. `selected == None`
    /// after that means a deliberate new-thread draft — a later snapshot
    /// (reconnect) must not hijack it by re-picking `threads.first()`.
    did_auto_select: bool,
    /// When the selected thread started running, for the "Working for …"
    /// row at the bottom of the timeline. `None` while idle.
    turn_started_at: Option<std::time::Instant>,
    /// Half-second tick counter driving the working row's pulsing dot.
    working_pulse: u64,
    /// First-run onboarding overlay (welcome.rs).
    welcome_state: welcome::State,
    /// Expanded image attachment viewer overlay (composer.rs).
    expanded_image: Option<composer::ExpandedImage>,
    /// Right inspector panel (right_panel.rs).
    panel_state: right_panel::State,
    /// File explorer pane (explorer.rs).
    explorer_state: explorer::State,
    /// Terminal drawer + pane (terminal_drawer.rs).
    terminal_state: terminal_drawer::State,
    /// Source-control panes (vcs.rs).
    vcs_state: vcs::State,
    /// Project content-search dialog (search_dialog.rs).
    search_state: search_dialog::State,
    /// Integrations (MCP apps) settings (integrations.rs).
    integrations_state: integrations::State,
    /// Notifications/Chat/Projects settings (settings_misc.rs).
    misc_settings_state: settings_misc::State,
    /// Tasks/Secrets/Diagnostics/Usage settings (settings_backend.rs).
    backend_settings_state: settings_backend::State,
    /// Dynamic RPC streams opened by surfaces (terminal, vcs, tasks):
    /// stream_id → chunk sink. Chunks arrive via `ShellEvent::StreamChunk`.
    stream_sinks: std::collections::HashMap<String, smol::channel::Sender<serde_json::Value>>,
    /// Counter minting unique stream ids.
    next_stream_id: u64,
    /// Center-column mode: chat timeline vs. standalone usage/PR views.
    center_view: CenterView,
    /// Focus handle parked on the root div (always mounted). When a
    /// non-Chat center view unmounts the composer, the window's focused
    /// node leaves the dispatch tree and every "App"-context binding goes
    /// dead — moving focus here keeps the whole keymap alive.
    app_focus: gpui::FocusHandle,
}

impl AppView {
    fn new(window: &mut Window, cx: &mut Context<Self>, config: &AppConfig) -> Self {
        // Apply the persisted theme mode before first paint.
        let client_config = config::ClientConfig::load();
        let system_dark = matches!(
            window.appearance(),
            gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
        );
        theme::set_mode(client_config.theme_mode, system_dark);
        // Follow OS appearance changes while the mode is System.
        cx.observe_window_appearance(window, |this, window, cx| {
            let system_dark = matches!(
                window.appearance(),
                gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
            );
            theme::set_mode(this.client_config.theme_mode, system_dark);
            cx.refresh_windows();
        })
        .detach();

        let composer = cx.new(|cx| TextField::new("Message the agent…", cx));
        composer.read(cx).focus_handle(cx).focus(window, cx);
        cx.subscribe_in(
            &composer,
            window,
            |this, _composer, event: &PromptSubmitted, _window, cx| {
                this.on_prompt_submitted(&event.0, cx);
            },
        )
        .detach();
        let search_field = cx.new(|cx| TextField::new("Search threads…", cx));

        let mut commands = None;
        let state = match config.endpoint.clone() {
            Some(endpoint) => {
                let (events_tx, events_rx) = smol::channel::unbounded::<ShellEvent>();
                let (commands_tx, commands_rx) = std::sync::mpsc::channel::<ClientCommand>();
                std::thread::spawn(move || net::run(endpoint, events_tx, commands_rx));
                commands = Some(commands_tx);

                cx.spawn(async move |this, cx| {
                    while let Ok(event) = events_rx.recv().await {
                        let alive = this.update(cx, |this, cx| {
                            this.apply_shell_event(event, cx);
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                })
                .detach();

                AppState::connecting()
            }
            None => AppState::mock(),
        };

        let mut misc_settings_state = settings_misc::State::default();
        misc_settings_state.seed_from_config(&client_config);

        Self {
            state,
            composer,
            search_field,
            commands,
            thread_items: Vec::new(),
            scroll_handle: ScrollHandle::new(),
            provider: config.provider.clone(),
            model: config.model.clone(),
            sidebar_state: sidebar::State::default(),
            timeline_state: timeline::State::default(),
            composer_state: composer::State::default(),
            palette_state: palette::State::default(),
            settings_state: settings::State::default(),
            notifications_state: notifications::State::default(),
            appearance_state: appearance::State::default(),
            archived_state: archived::State::default(),
            clients_state: clients::State::default(),
            acp_state: acp::State::default(),
            env_state: envs::State::default(),
            client_config,
            endpoint_label: config
                .endpoint
                .as_ref()
                .map(|e| e.http_url.clone())
                .unwrap_or_else(|| "offline".to_string()),
            active_endpoint: config.endpoint.clone(),
            did_auto_select: false,
            turn_started_at: None,
            working_pulse: 0,
            welcome_state: welcome::State::default(),
            expanded_image: None,
            panel_state: right_panel::State::default(),
            explorer_state: explorer::State::default(),
            terminal_state: terminal_drawer::State::default(),
            vcs_state: vcs::State::default(),
            search_state: search_dialog::State::default(),
            integrations_state: integrations::State::default(),
            misc_settings_state,
            backend_settings_state: settings_backend::State::default(),
            stream_sinks: std::collections::HashMap::new(),
            next_stream_id: 0,
            center_view: CenterView::Chat,
            app_focus: cx.focus_handle(),
        }
    }

    fn apply_shell_event(&mut self, event: ShellEvent, cx: &mut Context<Self>) {
        self.observe_shell_event(&event, cx);
        match event {
            ShellEvent::Connected => {
                self.state.connection = ConnectionStatus::Connected;
                // Fresh socket: re-subscribe the open thread so its stream
                // (and a fresh snapshot) resumes after a reconnect.
                if let Some(thread_id) = self.state.selected.clone() {
                    if let Some(commands) = &self.commands {
                        let _ = commands.send(ClientCommand::SubscribeThread {
                            thread_id: thread_id.to_string(),
                        });
                    }
                }
            }
            ShellEvent::Snapshot(snapshot) => {
                self.state.projects = snapshot
                    .projects
                    .into_iter()
                    .map(|p| model::Project {
                        id: p.id.into(),
                        name: p.title.into(),
                        path: p.workspace_root.into(),
                        expanded: true,
                    })
                    .collect();
                self.state.threads = snapshot
                    .threads
                    .iter()
                    .filter(|t| !t.deleted && !t.archived)
                    .map(model::Thread::from_shell)
                    .collect();
                if self.state.selected.is_none()
                    && !self.did_auto_select
                    && let Some(first) = self.state.threads.first()
                {
                    let id = first.id.clone();
                    self.state.selected = Some(id.clone());
                    self.did_auto_select = true;
                    self.thread_items.clear();
                    if let Some(commands) = &self.commands {
                        let _ = commands.send(ClientCommand::SubscribeThread {
                            thread_id: id.to_string(),
                        });
                    }
                }
            }
            ShellEvent::ProjectUpserted(p) => {
                let project = model::Project {
                    id: p.id.clone().into(),
                    name: p.title.into(),
                    path: p.workspace_root.into(),
                    expanded: true,
                };
                match self
                    .state
                    .projects
                    .iter()
                    .position(|e| e.id.as_ref() == p.id)
                {
                    Some(i) => self.state.projects[i] = project,
                    None => self.state.projects.push(project),
                }
            }
            ShellEvent::ProjectRemoved { project_id } => {
                self.state.projects.retain(|p| p.id.as_ref() != project_id);
            }
            ShellEvent::ThreadUpserted { thread } => {
                if thread.deleted || thread.archived {
                    self.state.threads.retain(|t| t.id.as_ref() != thread.id);
                } else {
                    let entry = model::Thread::from_shell(&thread);
                    match self
                        .state
                        .threads
                        .iter()
                        .position(|e| e.id.as_ref() == thread.id)
                    {
                        Some(i) => self.state.threads[i] = entry,
                        None => self.state.threads.push(entry),
                    }
                }
            }
            ShellEvent::ThreadRemoved { thread_id, .. } => {
                self.state.threads.retain(|t| t.id.as_ref() != thread_id);
            }
            ShellEvent::ThreadSnapshot { items } => {
                self.thread_items = items;
                self.scroll_handle.scroll_to_bottom();
            }
            ShellEvent::ThreadItemUpdated { item } => {
                match self.thread_items.iter().position(|i| i.id == item.id) {
                    Some(i) => self.thread_items[i] = item,
                    None => self.thread_items.push(item),
                }
                self.scroll_handle.scroll_to_bottom();
            }
            ShellEvent::ProjectsRefreshed { projects } => {
                for p in projects {
                    let entry = model::Project {
                        id: p.id.clone().into(),
                        name: p.title.into(),
                        path: p.workspace_root.into(),
                        expanded: true,
                    };
                    match self
                        .state
                        .projects
                        .iter()
                        .position(|e| e.id.as_ref() == p.id)
                    {
                        Some(i) => self.state.projects[i] = entry,
                        None => self.state.projects.push(entry),
                    }
                }
            }
            ShellEvent::OlderSettledThreads { threads } => {
                for thread in threads {
                    if thread.deleted
                        || self
                            .state
                            .threads
                            .iter()
                            .any(|t| t.id.as_ref() == thread.id)
                    {
                        continue;
                    }
                    self.state.threads.push(model::Thread::from_shell(&thread));
                }
            }
            ShellEvent::Error(e) => {
                // A failed request/subscription (e.g. an RPC the server
                // doesn't implement) must not downgrade a live connection —
                // the toast already reports it.
                if !matches!(self.state.connection, ConnectionStatus::Connected) {
                    self.state.connection = ConnectionStatus::Failed(e.into());
                }
            }
            ShellEvent::AuthAccessChange { value } => {
                self.observe_auth_access(&value, cx);
            }
            ShellEvent::StreamChunk { stream_id, values } => {
                if let Some(tx) = self.stream_sinks.get(&stream_id) {
                    for v in values {
                        let _ = tx.try_send(v);
                    }
                }
            }
            ShellEvent::StreamEnd { stream_id } => {
                // Dropping the sink closes the surface's receiver.
                self.stream_sinks.remove(&stream_id);
            }
            ShellEvent::Disconnected(reason) => {
                self.state.connection = ConnectionStatus::Disconnected(reason.into());
            }
        }
        cx.notify();
    }

    fn on_prompt_submitted(&mut self, text: &SharedString, cx: &mut Context<Self>) {
        self.state.messages.push(model::Message {
            role: Role::User,
            text: text.clone(),
        });
        self.composer_state.history_index = None;

        if self.state.connection == ConnectionStatus::Connected {
            // The submitted prompt joins the project's recall history.
            let project = self.composer_project_key();
            self.client_config.record_prompt(&project, text.as_ref());
            let thread_id = match self.state.selected_thread() {
                Some(thread) => thread.id.to_string(),
                None => {
                    // No thread selected: create one under the first project.
                    let Some(project) = self.state.projects.first() else {
                        self.state.messages.push(model::Message {
                            role: Role::Agent,
                            text: "No project on this server to create a thread in.".into(),
                        });
                        cx.notify();
                        return;
                    };
                    let thread_id = uuid::Uuid::new_v4().to_string();
                    self.dispatch(serde_json::json!({
                        "type": "thread.create",
                        "commandId": uuid::Uuid::new_v4().to_string(),
                        "threadId": thread_id,
                        "projectId": project.id.to_string(),
                        "title": text.chars().take(60).collect::<String>(),
                        "modelSelection": self.model_selection(),
                        "runtimeMode": self.composer_state.runtime_mode.clone(),
                        "interactionMode": self.composer_state.interaction_mode.clone(),
                        "branch": null,
                        "worktreePath": null,
                        "createdBy": "user",
                        "creationSource": "web",
                    }));
                    self.state.selected = Some(thread_id.clone().into());
                    if let Some(commands) = &self.commands {
                        let _ = commands.send(ClientCommand::SubscribeThread {
                            thread_id: thread_id.clone(),
                        });
                    }
                    thread_id
                }
            };

            let attachments = serde_json::Value::Array(self.composer_state.drain_attachments());
            // A busy thread queues the message behind the active run instead
            // of interrupting it (web parity); `dispatch_override` is the
            // composer's run-options pick (steer/restart/queue).
            let dispatch_mode = match self.composer_state.dispatch_override.take() {
                Some(mode @ ("steer_active" | "restart_active")) => serde_json::json!({
                    "type": mode,
                    "targetRunId": self
                        .state
                        .selected_thread()
                        .and_then(|t| t.latest_run_id.clone()),
                }),
                Some(mode) => serde_json::json!({ "type": mode }),
                None if self.selected_thread_busy() => {
                    serde_json::json!({ "type": "queue_after_active" })
                }
                None => serde_json::json!({ "type": "start_immediately" }),
            };
            self.dispatch(serde_json::json!({
                "type": "message.dispatch",
                "commandId": uuid::Uuid::new_v4().to_string(),
                "threadId": thread_id,
                "messageId": uuid::Uuid::new_v4().to_string(),
                "text": text.to_string(),
                "modelSelection": self.model_selection(),
                "attachments": attachments,
                "dispatchMode": dispatch_mode,
                "createdBy": "user",
                "creationSource": "web",
            }));
        } else {
            self.state.messages.push(model::Message {
                role: Role::Agent,
                text: "Received — the Rust client is not wired to a 67 server yet.".into(),
            });
        }
        cx.notify();
    }

    /// Switch the center column and move focus to a node that stays
    /// mounted: Chat refocuses the composer, Usage/PullRequests park focus
    /// on the root div's handle. Without this, leaving Chat unmounts the
    /// focused TextField, the dispatch path falls back to the view node
    /// (no key context), and every "App" binding goes dead. Deferred so it
    /// runs after the dispatch-time window borrow is released — and after
    /// `close_palette` re-points focus at the now-unmounted composer.
    pub(crate) fn set_center_view(&mut self, view: CenterView, cx: &mut Context<Self>) {
        self.center_view = view;
        let entity_id = cx.entity_id();
        let app_focus = self.app_focus.clone();
        let composer = self.composer.clone();
        let chat = view == CenterView::Chat;
        cx.defer(move |cx| {
            for handle in cx.windows() {
                let _ = handle.update(cx, |root, window, app| {
                    let matches = root
                        .downcast::<AppView>()
                        .is_ok_and(|view| view.entity_id() == entity_id);
                    if !matches {
                        return;
                    }
                    if chat {
                        composer.read(app).focus_handle(app).focus(window, app);
                    } else {
                        app_focus.focus(window, app);
                    }
                });
            }
        });
        cx.notify();
    }

    fn on_new_thread(&mut self, _action: &NewThread, _window: &mut Window, cx: &mut Context<Self>) {
        self.state.selected = None;
        self.thread_items.clear();
        self.set_center_view(CenterView::Chat, cx);
    }

    /// ctrl-1..9 — the web `thread.jump.N` commands.
    fn on_thread_jump(
        &mut self,
        action: &ThreadJump,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.jump_to_thread(action.index, cx);
    }

    /// ctrl-u — the web `usage.open` command.
    fn on_open_usage_view(
        &mut self,
        _action: &OpenUsageView,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_center_view(CenterView::Usage, cx);
    }

    fn on_toggle_panel(
        &mut self,
        _action: &TogglePanel,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.panel_state.tab = if self.panel_state.tab.is_some() {
            None
        } else {
            Some(right_panel::Tab::Files)
        };
        cx.notify();
    }

    fn on_toggle_terminal_drawer(
        &mut self,
        _action: &ToggleTerminalDrawer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = !self.terminal_state.drawer_open;
        self.terminal_drawer_set_open(open, window, cx);
    }

    /// Single door for opening/closing the terminal drawer: closing hands
    /// focus back to the composer so keystrokes never land on a hidden
    /// terminal focus handle.
    pub(crate) fn terminal_drawer_set_open(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.terminal_state.drawer_open = open;
        if !open {
            self.composer.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    fn on_toggle_search(
        &mut self,
        _action: &ToggleSearch,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_state.open = !self.search_state.open;
        cx.notify();
    }

    fn on_close_overlay(
        &mut self,
        _action: &CloseOverlay,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.expanded_image.is_some() {
            self.expanded_image = None;
        } else if self.search_state.open {
            self.search_state.open = false;
        } else if self.palette_state.open {
            self.palette_state.open = false;
        } else if self.settings_state.open {
            self.settings_state.open = false;
        } else if self.composer_state.picker.is_some() {
            self.composer_state.picker = None;
        } else if self.vcs_dismiss_picker(cx) {
            return;
        } else if self.center_view != CenterView::Chat {
            self.set_center_view(CenterView::Chat, cx);
        } else {
            return;
        }
        cx.notify();
    }

    fn on_focus_search(
        &mut self,
        _action: &FocusSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_field
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
    }

    /// Send a request/response RPC (`{"tag": <tag>}`) and get a receiver that
    /// resolves once with the `Exit` frame — `{"_tag":"Success","value":…}` or
    /// a Failure object. None when offline.
    fn send_query(
        &self,
        tag: &str,
        payload: serde_json::Value,
    ) -> Option<smol::channel::Receiver<serde_json::Value>> {
        let (reply, rx) = smol::channel::bounded(1);
        self.commands
            .as_ref()?
            .send(ClientCommand::Query {
                tag: tag.to_string(),
                payload,
                reply,
            })
            .ok()?;
        Some(rx)
    }

    fn dispatch(&mut self, command: serde_json::Value) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(ClientCommand::Dispatch { command });
        }
    }

    /// Open a windowed RPC stream (`terminal.attach`, `subscribeVcsStatus`,
    /// `scheduledTasks.subscribe`, …). Returns the stream id plus a receiver
    /// that yields each raw chunk value; the receiver closes on stream end.
    /// Surfaces drive their own `cx.spawn` loops on the receiver.
    fn open_stream(
        &mut self,
        tag: &str,
        payload: serde_json::Value,
    ) -> Option<(String, smol::channel::Receiver<serde_json::Value>)> {
        self.next_stream_id += 1;
        let stream_id = format!("s{}", self.next_stream_id);
        let (tx, rx) = smol::channel::unbounded();
        self.stream_sinks.insert(stream_id.clone(), tx);
        self.commands
            .as_ref()?
            .send(ClientCommand::Subscribe {
                tag: tag.to_string(),
                payload,
                stream_id: stream_id.clone(),
            })
            .ok()?;
        Some((stream_id, rx))
    }

    /// Interrupt an open dynamic stream (no-op if it already ended).
    fn close_stream(&mut self, stream_id: &str) {
        self.stream_sinks.remove(stream_id);
        if let Some(commands) = &self.commands {
            let _ = commands.send(ClientCommand::InterruptStream {
                stream_id: stream_id.to_string(),
            });
        }
    }

    /// Push the composer's runtime/interaction mode picks onto the selected
    /// thread so the next run uses them (web sends the same set commands).
    pub(crate) fn set_thread_modes(&mut self) {
        let Some(thread) = self.state.selected_thread() else {
            return;
        };
        let thread_id = thread.id.to_string();
        self.dispatch(serde_json::json!({
            "type": "thread.runtime-mode.set",
            "commandId": uuid::Uuid::new_v4().to_string(),
            "threadId": thread_id,
            "runtimeMode": self.composer_state.runtime_mode.clone(),
        }));
        self.dispatch(serde_json::json!({
            "type": "thread.interaction-mode.set",
            "commandId": uuid::Uuid::new_v4().to_string(),
            "threadId": thread_id,
            "interactionMode": self.composer_state.interaction_mode.clone(),
        }));
    }

    pub(crate) fn stop_thread(&mut self, cx: &mut Context<Self>) {
        let Some(thread) = self.state.selected_thread() else {
            return;
        };
        self.dispatch(serde_json::json!({
            "type": "thread.stop",
            "commandId": uuid::Uuid::new_v4().to_string(),
            "threadId": thread.id.to_string(),
        }));
        cx.notify();
    }

    fn select_thread(&mut self, thread_id: SharedString, cx: &mut Context<Self>) {
        self.thread_items.clear();
        self.turn_started_at = None;
        if let Some(commands) = &self.commands {
            if commands
                .send(ClientCommand::SubscribeThread {
                    thread_id: thread_id.to_string(),
                })
                .is_err()
            {
                eprintln!("[ui] command channel closed");
            }
        }
        self.state.selected = Some(thread_id);
        self.set_center_view(CenterView::Chat, cx);
    }

    /// Center-column content for the standalone modes: a back-to-chat
    /// header over the shared usage section / pull-requests pane. Both
    /// bodies self-load against the wire (server.getUsageSummary and the
    /// pullRequests.* calls respectively).
    fn render_center_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (label, body) = match self.center_view {
            CenterView::Usage => ("Usage", self.render_usage_section(cx)),
            CenterView::PullRequests => ("Pull requests", self.render_prs_pane(cx)),
            // render() only calls this for non-chat modes; keep a cheap body
            // anyway so an unexpected Chat can't panic the draw path.
            CenterView::Chat => ("Chat", div()),
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .id("center-back-chat")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .hover(|s| s.bg(theme::hover()))
                            .child("← Chat")
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    this.set_center_view(CenterView::Chat, cx);
                                }),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(label),
                    ),
            )
            .child(
                div()
                    .id("center-standalone")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_4()
                    .py_2()
                    .child(body),
            )
    }
}

/// Linux/Windows client-decoration buttons mirroring Electron's
/// `titleBarOverlay` (minimize / maximize-restore / close).
fn window_controls() -> impl IntoElement {
    fn btn(
        id: &'static str,
        glyph: &'static str,
        area: WindowControlArea,
        close_danger: bool,
        handler: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let mut b = div()
            .id(id)
            .w_7()
            .h_5()
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .cursor_pointer()
            .text_xs()
            .text_color(theme::fg_muted())
            .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
            .child(glyph)
            .window_control_area(area)
            .on_mouse_down(gpui::MouseButton::Left, handler);
        if close_danger {
            b = b.hover(|s| s.bg(theme::error()).text_color(gpui::white()));
        }
        b
    }
    div()
        .flex()
        .items_center()
        .gap_1()
        .ml_2()
        .child(btn(
            "win-min",
            "\u{2013}",
            WindowControlArea::Min,
            false,
            |_ev, w, _cx| w.minimize_window(),
        ))
        .child(btn(
            "win-max",
            "\u{25a1}",
            WindowControlArea::Max,
            false,
            |_ev, w, _cx| w.zoom_window(),
        ))
        .child(btn(
            "win-close",
            "\u{00d7}",
            WindowControlArea::Close,
            true,
            |_ev, w, _cx| w.remove_window(),
        ))
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Client-decorated windows need an inset so the compositor still
        // offers invisible resize borders.
        let client_decorations = matches!(window.window_decorations(), Decorations::Client { .. });
        if client_decorations {
            window.set_client_inset(px(5.0));
        }
        let selected_thread = self.state.selected_thread();
        let title = selected_thread
            .map(|t| t.title.clone())
            .unwrap_or_else(|| "67 Code".into());
        let running = selected_thread.is_some_and(|t| {
            !matches!(
                t.status.as_ref(),
                "idle" | "completed" | "failed" | "stopped" | "interrupted"
            )
        });
        // Drive the "Working for …" row: remember when the selected thread
        // started running and tick a half-second repaint while it does.
        match (running, self.turn_started_at) {
            (true, None) => {
                let start = std::time::Instant::now();
                self.turn_started_at = Some(start);
                self.working_pulse = 0;
                cx.spawn(async move |this, cx| {
                    loop {
                        smol::Timer::after(std::time::Duration::from_millis(500)).await;
                        let alive = this
                            .update(cx, |this, cx| {
                                if this.turn_started_at != Some(start) {
                                    return false;
                                }
                                this.working_pulse = this.working_pulse.wrapping_add(1);
                                cx.notify();
                                true
                            })
                            .unwrap_or(false);
                        if !alive {
                            break;
                        }
                    }
                })
                .detach();
            }
            (false, Some(_)) => self.turn_started_at = None,
            _ => {}
        }
        div()
            .key_context("App")
            .track_focus(&self.app_focus)
            .flex()
            .size_full()
            .font_family(theme::FONT_SANS)
            .bg(theme::canvas())
            .text_color(theme::fg())
            .on_action(cx.listener(AppView::on_toggle_palette))
            .on_action(cx.listener(AppView::on_toggle_settings))
            .on_action(cx.listener(AppView::on_toggle_panel))
            .on_action(cx.listener(AppView::on_toggle_terminal_drawer))
            .on_action(cx.listener(AppView::on_toggle_search))
            .on_action(cx.listener(AppView::on_new_thread))
            .on_action(cx.listener(AppView::on_close_overlay))
            .on_action(cx.listener(AppView::on_focus_search))
            // Parameterized — element-level so the dispatched index lands in
            // the window that saw the keystroke.
            .on_action(cx.listener(AppView::on_thread_jump))
            // Parity bindings stay element-level: a `cx.on_action` global has
            // no `&mut Window`, and re-borrowing the window during dispatch
            // fails — these listeners must not move back to `app_handler!`.
            .on_action(
                cx.listener(|this: &mut AppView, _: &OpenFilePicker, _window, cx| {
                    this.open_file_picker(cx)
                }),
            )
            .on_action(
                cx.listener(|this: &mut AppView, _: &ClosePanel, window, cx| {
                    this.close_panel_or_drawer(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this: &mut AppView, _: &PrevThread, _window, cx| {
                    this.step_thread_selection(-1, cx)
                }),
            )
            .on_action(
                cx.listener(|this: &mut AppView, _: &NextThread, _window, cx| {
                    this.step_thread_selection(1, cx)
                }),
            )
            .on_action(
                cx.listener(|this: &mut AppView, _: &TogglePin, _window, _cx| {
                    this.toggle_selected_thread_pin()
                }),
            )
            .on_action(
                cx.listener(|this: &mut AppView, _: &ToggleModelPicker, _window, cx| {
                    this.toggle_model_picker(cx)
                }),
            )
            .on_action(cx.listener(AppView::on_open_usage_view))
            .child(self.render_sidebar(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        div()
                            .h(px(theme::TOPBAR_H))
                            .px_4()
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(theme::border())
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            // Drag region for client-side decorations; no-op
                            // under server decorations.
                            .window_control_area(WindowControlArea::Drag)
                            .child(title)
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .id("terminal-drawer-toggle")
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .cursor_pointer()
                                            .text_xs()
                                            .text_color(theme::fg_muted())
                                            .hover(|s| s.bg(theme::hover()))
                                            .child("❯_")
                                            .on_mouse_down(
                                                gpui::MouseButton::Left,
                                                cx.listener(|this, _ev, w, cx| {
                                                    let open = !this.terminal_state.drawer_open;
                                                    this.terminal_drawer_set_open(open, w, cx);
                                                }),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("right-panel-toggle")
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .cursor_pointer()
                                            .text_xs()
                                            .text_color(theme::fg_muted())
                                            .hover(|s| s.bg(theme::hover()))
                                            .child("▤")
                                            .on_mouse_down(
                                                gpui::MouseButton::Left,
                                                cx.listener(|this, _ev, _w, cx| {
                                                    this.panel_state.tab =
                                                        if this.panel_state.tab.is_some() {
                                                            None
                                                        } else {
                                                            Some(right_panel::Tab::Files)
                                                        };
                                                    cx.notify();
                                                }),
                                            ),
                                    )
                                    .when(client_decorations, |el| el.child(window_controls())),
                            ),
                    )
                    .child(match self.center_view {
                        CenterView::Chat => div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .child(self.render_timeline())
                            .child(self.render_terminal_drawer(cx))
                            .child(self.render_composer())
                            .into_any_element(),
                        _ => self.render_center_view(cx).into_any_element(),
                    }),
            )
            .child(self.render_right_panel(cx))
            .child(self.render_palette(cx))
            .child(self.render_settings(cx))
            .child(self.render_notifications(cx))
            .child(self.render_welcome(cx))
            .child(self.render_snooze_overlay(cx))
            .child(self.render_image_overlay(cx))
            .child(self.render_search_dialog(cx))
    }
}

/// Run the GPUI app on the calling (main) thread. Blocks until the window closes.
pub fn run(config: AppConfig) {
    application().run(move |cx: &mut App| {
        use text_field::*;
        // Inter (OFL) — the web's --font-sans. Bundled TTFs so the UI doesn't
        // depend on the host fontconfig.
        let _ = cx.text_system().add_fonts(vec![
            std::borrow::Cow::Borrowed(&include_bytes!("../fonts/Inter-Regular.ttf")[..]),
            std::borrow::Cow::Borrowed(&include_bytes!("../fonts/Inter-Medium.ttf")[..]),
            std::borrow::Cow::Borrowed(&include_bytes!("../fonts/Inter-SemiBold.ttf")[..]),
            std::borrow::Cow::Borrowed(&include_bytes!("../fonts/Inter-Bold.ttf")[..]),
            std::borrow::Cow::Borrowed(&include_bytes!("../fonts/Inter-Italic.ttf")[..]),
        ]);
        cx.bind_keys([
            KeyBinding::new("backspace", Backspace, Some("TextField")),
            KeyBinding::new("delete", Delete, Some("TextField")),
            KeyBinding::new("left", Left, Some("TextField")),
            KeyBinding::new("right", Right, Some("TextField")),
            KeyBinding::new("shift-left", SelectLeft, Some("TextField")),
            KeyBinding::new("shift-right", SelectRight, Some("TextField")),
            KeyBinding::new("ctrl-a", SelectAll, Some("TextField")),
            KeyBinding::new("ctrl-v", Paste, Some("TextField")),
            KeyBinding::new("ctrl-c", Copy, Some("TextField")),
            KeyBinding::new("ctrl-x", Cut, Some("TextField")),
            KeyBinding::new("home", Home, Some("TextField")),
            KeyBinding::new("end", End, Some("TextField")),
            KeyBinding::new("enter", Submit, Some("TextField")),
        ]);
        cx.bind_keys([
            KeyBinding::new("cmd-k", TogglePalette, Some("App")),
            KeyBinding::new("cmd-,", ToggleSettings, Some("App")),
            KeyBinding::new("ctrl-,", ToggleSettings, Some("App")),
            KeyBinding::new("cmd-n", NewThread, Some("App")),
            KeyBinding::new("cmd-f", FocusSearch, Some("App")),
            KeyBinding::new("ctrl-shift-f", ToggleSearch, Some("App")),
            KeyBinding::new("ctrl-`", ToggleTerminalDrawer, Some("App")),
            KeyBinding::new("ctrl-b", TogglePanel, Some("App")),
            KeyBinding::new("escape", CloseOverlay, Some("App")),
            // Web-parity additions (web `mod` ≈ native `ctrl`):
            KeyBinding::new("ctrl-j", ToggleTerminalDrawer, Some("App")),
            KeyBinding::new("ctrl-alt-b", TogglePanel, Some("App")),
            KeyBinding::new("ctrl-p", OpenFilePicker, Some("App")),
            KeyBinding::new("ctrl-w", ClosePanel, Some("App")),
            KeyBinding::new("ctrl-{", PrevThread, Some("App")),
            KeyBinding::new("ctrl-}", NextThread, Some("App")),
            KeyBinding::new("ctrl-shift-p", TogglePin, Some("App")),
            KeyBinding::new("ctrl-shift-m", ToggleModelPicker, Some("App")),
            KeyBinding::new("ctrl-shift-o", NewThread, Some("App")),
            KeyBinding::new("ctrl-shift-n", NewThread, Some("App")),
            // Web `chat.new` is mod+n (already above); ctrl-t additionally
            // opens a fresh thread draft like the browser's new-tab chord.
            KeyBinding::new("ctrl-t", NewThread, Some("App")),
            // Web `usage.open` is mod+u.
            KeyBinding::new("ctrl-u", OpenUsageView, Some("App")),
            // Web `thread.jump.N` is mod+N (desktop-only there too).
            KeyBinding::new("ctrl-1", ThreadJump { index: 0 }, Some("App")),
            KeyBinding::new("ctrl-2", ThreadJump { index: 1 }, Some("App")),
            KeyBinding::new("ctrl-3", ThreadJump { index: 2 }, Some("App")),
            KeyBinding::new("ctrl-4", ThreadJump { index: 3 }, Some("App")),
            KeyBinding::new("ctrl-5", ThreadJump { index: 4 }, Some("App")),
            KeyBinding::new("ctrl-6", ThreadJump { index: 5 }, Some("App")),
            KeyBinding::new("ctrl-7", ThreadJump { index: 6 }, Some("App")),
            KeyBinding::new("ctrl-8", ThreadJump { index: 7 }, Some("App")),
            KeyBinding::new("ctrl-9", ThreadJump { index: 8 }, Some("App")),
            // Web `navigation.back`/`navigation.forward` (mod+[/mod+]) —
            // closest native equivalent is stepping the thread order.
            KeyBinding::new("ctrl-[", PrevThread, Some("App")),
            KeyBinding::new("ctrl-]", NextThread, Some("App")),
        ]);
        // Electron MIN_MAIN_WINDOW_SIZE/DEFAULT_MAIN_WINDOW_SIZE.
        let bounds = Bounds::centered(None, size(px(1100.0), px(780.0)), cx);
        cx.open_window(
            WindowOptions {
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("67 Code".into()),
                    ..Default::default()
                }),
                // Electron uses titleBarStyle "hidden" + overlay controls on
                // Linux/Windows; client decorations give the same borderless
                // look where the compositor supports it.
                window_decorations: Some(WindowDecorations::Client),
                window_min_size: Some(size(px(400.0), px(360.0))),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| AppView::new(window, cx, &config)),
        )
        .unwrap();
        cx.activate(true);
    });
}
