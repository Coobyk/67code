mod acp;
mod appearance;
mod archived;
mod clients;
mod composer;
mod config;
mod envs;
mod markdown;
mod model;
pub mod net;
mod explorer;
mod notifications;
mod palette;
mod right_panel;
mod search_dialog;
mod settings;
mod settings_backend;
mod settings_misc;
mod sidebar;
mod terminal_drawer;
mod text_field;
mod theme;
mod timeline;
mod vcs;
mod welcome;

use gpui::{AppContext, Focusable, prelude::*,
    App, Bounds, Context, Entity, IntoElement, KeyBinding, Render,
    ScrollHandle, SharedString, Window, WindowBounds, WindowOptions, div, px, size,
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
        NewThread,
        CloseOverlay,
        FocusSearch,
    ]
);

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
            provider: std::env::var("C67_PROVIDER")
                .unwrap_or_else(|_| "devReplay".to_string()),
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
    /// Notifications/Chat/Projects settings (settings_misc.rs).
    misc_settings_state: settings_misc::State,
    /// Tasks/Secrets/Diagnostics/Usage settings (settings_backend.rs).
    backend_settings_state: settings_backend::State,
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
            turn_started_at: None,
            working_pulse: 0,
            welcome_state: welcome::State::default(),
            expanded_image: None,
            panel_state: right_panel::State::default(),
            explorer_state: explorer::State::default(),
            terminal_state: terminal_drawer::State::default(),
            vcs_state: vcs::State::default(),
            search_state: search_dialog::State::default(),
            misc_settings_state: settings_misc::State::default(),
            backend_settings_state: settings_backend::State::default(),
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
                    && let Some(first) = self.state.threads.first()
                {
                    let id = first.id.clone();
                    self.state.selected = Some(id.clone());
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
                match self.state.projects.iter().position(|e| e.id.as_ref() == p.id) {
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
                    match self.state.threads.iter().position(|e| e.id.as_ref() == thread.id) {
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
                    match self.state.projects.iter().position(|e| e.id.as_ref() == p.id) {
                        Some(i) => self.state.projects[i] = entry,
                        None => self.state.projects.push(entry),
                    }
                }
            }
            ShellEvent::OlderSettledThreads { threads } => {
                for thread in threads {
                    if thread.deleted
                        || self.state.threads.iter().any(|t| t.id.as_ref() == thread.id)
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
                    self.dispatch(
                        serde_json::json!({
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
                        }),
                    );
                    self.state.selected = Some(thread_id.clone().into());
                    if let Some(commands) = &self.commands {
                        let _ = commands.send(ClientCommand::SubscribeThread {
                            thread_id: thread_id.clone(),
                        });
                    }
                    thread_id
                }
            };

            let attachments = serde_json::Value::Array(
                self.composer_state.drain_attachments(),
            );
            // A busy thread queues the message behind the active run instead
            // of interrupting it (web parity).
            let dispatch_mode = if self.selected_thread_busy() {
                serde_json::json!({ "type": "queue_after_active" })
            } else {
                serde_json::json!({ "type": "start_immediately" })
            };
            self.dispatch(
                serde_json::json!({
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
                }),
            );
        } else {
            self.state.messages.push(model::Message {
                role: Role::Agent,
                text: "Received — the Rust client is not wired to a 67 server yet.".into(),
            });
        }
        cx.notify();
    }

    fn on_new_thread(&mut self, _action: &NewThread, _window: &mut Window, cx: &mut Context<Self>) {
        self.state.selected = None;
        self.thread_items.clear();
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
        } else if self.palette_state.open {
            self.palette_state.open = false;
        } else if self.settings_state.open {
            self.settings_state.open = false;
        } else if self.composer_state.picker.is_some() {
            self.composer_state.picker = None;
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
        cx.notify();
    }
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .flex()
            .size_full()
            .bg(theme::canvas())
            .text_color(theme::fg())
            .on_action(cx.listener(AppView::on_toggle_palette))
            .on_action(cx.listener(AppView::on_toggle_settings))
            .on_action(cx.listener(AppView::on_new_thread))
            .on_action(cx.listener(AppView::on_close_overlay))
            .on_action(cx.listener(AppView::on_focus_search))
            .child(self.render_sidebar(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .h_full()
                    .child(
                        div()
                            .h(px(44.))
                            .px_4()
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(theme::border())
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(title),
                    )
                    .child(self.render_timeline())
                    .child(self.render_terminal_drawer(cx))
                    .child(self.render_composer()),
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
            KeyBinding::new("escape", CloseOverlay, Some("App")),
        ]);
        let bounds = Bounds::centered(None, size(px(1200.0), px(760.0)), cx);
        cx.open_window(
            WindowOptions {
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("67 Code".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| AppView::new(window, cx, &config)),
        )
        .unwrap();
        cx.activate(true);
    });
}
