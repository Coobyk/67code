//! First-run onboarding overlay. Owns `impl AppView` welcome render methods.
//!
//! Auto-opens when no endpoint is configured (`active_endpoint.is_none()`)
//! until the user connects or picks "Skip / explore offline"; the dismissal is
//! persisted in an `onboarded` marker file next to `config.json` (config.rs
//! owns ClientConfig — onboarding state stays out of it).
//!
//! Connect mirrors envs.rs `connect_environment`: save the endpoint as a
//! `SavedEnvironment`, update `active_endpoint`/`endpoint_label`, and send
//! `ClientCommand::SwitchEndpoint` when a net loop exists. When the app
//! started with no endpoint (`commands.is_none()`), there is no net thread to
//! switch — same caveat envs.rs documents — so the selection applies on next
//! launch.

use gpui::{
    Context, Div, Entity, EntityInputHandler, Focusable, MouseButton, SharedString, Subscription,
    Window, div, prelude::*, px, rgb, rgba,
};

use crate::AppView;
use crate::config::SavedEnvironment;
use crate::model::ConnectionStatus;
use crate::net::{ClientCommand, ServerEndpoint};
use crate::notifications::ToastKind;
use crate::text_field::{PromptSubmitted, TextField};
use crate::theme;

/// Pre-filled endpoint — the address `cargo run -p c67-server` binds.
const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:14199";

pub struct State {
    /// Explicitly open (a future menu/palette entry can set this; first-run
    /// auto-open doesn't need it).
    pub open: bool,
    /// Set once the user connects or skips; the `onboarded` marker file
    /// makes it stick across launches so auto-open never fires again.
    dismissed: bool,
    /// Inline validation message shown under the fields.
    error: Option<SharedString>,
    /// Connect form inputs, created lazily on first render while open —
    /// `cx.new` needs a context (envs.rs/palette.rs lazy-entity pattern).
    endpoint_field: Option<Entity<TextField>>,
    token_field: Option<Entity<TextField>>,
    subs: Vec<Subscription>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            dismissed: onboarded(),
            error: None,
            endpoint_field: None,
            token_field: None,
            subs: Vec::new(),
        }
    }
}

/// `~/.config/c67/onboarded` (or `$C67_CONFIG_HOME/onboarded`) — same base dir
/// and pattern as config.rs `config_path()`.
fn onboarded_path() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("C67_CONFIG_HOME") {
        return Some(std::path::PathBuf::from(dir).join("onboarded"));
    }
    let home = std::env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config")
            .join("c67")
            .join("onboarded"),
    )
}

fn onboarded() -> bool {
    onboarded_path().is_some_and(|path| path.exists())
}

fn mark_onboarded() {
    let Some(path) = onboarded_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, "done\n");
}

/// Read a text field's contents when no `&mut Window` is on the update stack
/// (entity observers run with `&mut App` only). Same helper envs.rs uses.
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

/// Read a text field's contents inside an event listener that has a `window`.
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

/// Run `f` on `field` with a `&mut Window` from a context that doesn't carry
/// one (entity subscriptions/observers, deferred spawns). No-ops when no
/// window is reachable — callers treat it as best-effort.
fn edit_field<R>(
    field: &Entity<TextField>,
    cx: &mut Context<AppView>,
    f: impl Fn(&mut TextField, &mut Window, &mut Context<TextField>) -> R,
) -> Option<R> {
    if let Some(result) = cx.with_window(field.entity_id(), |window, app| {
        field.update(app, |field, cx| f(field, window, cx))
    }) {
        return Some(result);
    }
    for handle in cx.windows() {
        if let Ok(result) = handle.update(cx, |_root, window, app| {
            field.update(app, |field, cx| f(field, window, cx))
        }) {
            return Some(result);
        }
    }
    None
}

fn set_field_text(field: &Entity<TextField>, text: &str, cx: &mut Context<AppView>) {
    edit_field(field, cx, |field, window, cx| {
        field.replace_text_in_range(Some(0..usize::MAX), text, window, cx)
    });
}

fn focus_field(field: &Entity<TextField>, cx: &mut Context<AppView>) {
    let focus = field.read(cx).focus_handle(cx);
    if cx
        .with_window(field.entity_id(), |window, app| {
            focus.focus(window, app);
        })
        .is_some()
    {
        return;
    }
    for handle in cx.windows() {
        if handle
            .update(cx, |_root, window, app| focus.focus(window, app))
            .is_ok()
        {
            return;
        }
    }
}

/// One numbered step row: muted index on the left, title + body on the right.
fn step(number: &'static str, title: &'static str, body: Div) -> Div {
    div()
        .flex()
        .gap_3()
        .pt_3()
        .child(
            div()
                .w(px(20.))
                .flex_none()
                .text_sm()
                .text_color(theme::fg_muted())
                .child(number),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::fg())
                        .child(title),
                )
                .child(body),
        )
}

impl AppView {
    /// Lazily create the endpoint/token fields + submit subscriptions.
    /// Called from `render_welcome` when the overlay is about to paint —
    /// `cx.new`/`cx.subscribe` are app-context calls, safe during render.
    /// Focus needs a window, so it's deferred one tick.
    fn ensure_welcome_fields(&mut self, cx: &mut Context<Self>) {
        if self.welcome_state.endpoint_field.is_some() {
            return;
        }
        let endpoint_field = cx.new(|cx| TextField::new(DEFAULT_ENDPOINT, cx));
        let token_field = cx.new(|cx| TextField::new("Bearer token (optional)", cx));

        // Enter in either field submits the whole form (envs.rs pattern).
        for field in [&endpoint_field, &token_field] {
            self.welcome_state.subs.push(cx.subscribe(
                field,
                |this, field, event: &PromptSubmitted, cx| {
                    this.submit_welcome_form(&field, &event.0, cx);
                },
            ));
        }

        self.welcome_state.endpoint_field = Some(endpoint_field);
        self.welcome_state.token_field = Some(token_field);

        // Rendering holds the window borrow, so focus on the next tick —
        // best-effort; a no-op if no window update is reachable.
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |this, cx| {
                if let Some(field) = this.welcome_state.endpoint_field.clone() {
                    focus_field(&field, cx);
                }
            });
        })
        .detach();
    }

    /// Enter pressed in a welcome field. The submitting field's text arrives
    /// in `submitted` (it is already reset by then); the other field is read
    /// out via `field_text`. Same shape as envs.rs `submit_env_form`.
    fn submit_welcome_form(
        &mut self,
        submitter: &Entity<TextField>,
        submitted: &SharedString,
        cx: &mut Context<Self>,
    ) {
        let (Some(url_f), Some(token_f)) = (
            self.welcome_state.endpoint_field.clone(),
            self.welcome_state.token_field.clone(),
        ) else {
            return;
        };
        let url = if submitter == &url_f {
            submitted.to_string()
        } else {
            field_text(&url_f, cx)
        };
        let token = if submitter == &token_f {
            submitted.to_string()
        } else {
            field_text(&token_f, cx)
        };
        // The submitter's field was cleared on Enter — remember its text so a
        // failed validation can restore it.
        let restore = if submitter == &url_f {
            (url_f, url.clone())
        } else {
            (token_f, token.clone())
        };
        self.welcome_connect(url, token, Some(restore), cx);
    }

    /// "Connect" button — reads the fields through the listener's `window`.
    fn welcome_connect_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(url_f), Some(token_f)) = (
            self.welcome_state.endpoint_field.clone(),
            self.welcome_state.token_field.clone(),
        ) else {
            return;
        };
        let url = field_text_in(&url_f, window, cx);
        let token = field_text_in(&token_f, window, cx);
        self.welcome_connect(url, token, None, cx);
    }

    /// Validate + persist the endpoint as a saved environment, then switch —
    /// the same flow envs.rs `add_environment` + `connect_environment` run.
    fn welcome_connect(
        &mut self,
        url: String,
        token: String,
        restore: Option<(Entity<TextField>, String)>,
        cx: &mut Context<Self>,
    ) {
        let mut url = url.trim().to_string();
        if url.is_empty() {
            url = DEFAULT_ENDPOINT.to_string();
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.welcome_state.error = Some("URL must start with http:// or https://".into());
            if let Some((field, text)) = restore {
                set_field_text(&field, &text, cx);
            }
            if let Some(field) = self.welcome_state.endpoint_field.clone() {
                focus_field(&field, cx);
            }
            cx.notify();
            return;
        }
        let token = token.trim().to_string();

        // Reuse a saved env on URL match (update its token); else append.
        let ix = match self
            .client_config
            .environments
            .iter()
            .position(|e| e.http_url == url)
        {
            Some(ix) => {
                self.client_config.environments[ix].token = token.clone();
                ix
            }
            None => {
                self.client_config.environments.push(SavedEnvironment {
                    label: url
                        .trim_start_matches("http://")
                        .trim_start_matches("https://")
                        .to_string(),
                    http_url: url.clone(),
                    token: token.clone(),
                });
                self.client_config.environments.len() - 1
            }
        };
        self.client_config.active_environment = Some(ix);
        self.client_config.save();

        self.endpoint_label = url.clone();
        self.active_endpoint = Some(ServerEndpoint {
            http_url: url.clone(),
            token: token.clone(),
        });
        self.finish_onboarding(cx);

        if let Some(commands) = &self.commands {
            let _ = commands.send(ClientCommand::SwitchEndpoint {
                http_url: url.clone(),
                token,
            });
            self.state.connection = ConnectionStatus::Connecting;
            self.push_toast(
                ToastKind::Info,
                "Connecting",
                Some(format!("Switching to {url}")),
                None,
                cx,
            );
        } else {
            // Started with no endpoint: no net thread exists to switch (the
            // same path envs.rs documents). The saved selection connects on
            // next launch.
            self.push_toast(
                ToastKind::Info,
                "Environment saved",
                Some(format!("{url} — connects on next launch")),
                None,
                cx,
            );
        }
        cx.notify();
    }

    /// "Reconnect to <name>" — switch back to a previously saved environment.
    /// Identical mechanism to envs.rs `connect_environment`.
    fn welcome_reconnect(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(env) = self.client_config.environments.get(ix).cloned() else {
            return;
        };
        self.client_config.active_environment = Some(ix);
        self.client_config.save();
        self.endpoint_label = env.http_url.clone();
        self.active_endpoint = Some(ServerEndpoint {
            http_url: env.http_url.clone(),
            token: env.token.clone(),
        });
        self.finish_onboarding(cx);
        if let Some(commands) = &self.commands {
            let _ = commands.send(ClientCommand::SwitchEndpoint {
                http_url: env.http_url.clone(),
                token: env.token.clone(),
            });
            self.state.connection = ConnectionStatus::Connecting;
        }
        self.push_toast(
            ToastKind::Info,
            "Reconnecting",
            Some(format!("Switching to {}", env.label)),
            None,
            cx,
        );
        cx.notify();
    }

    /// "Skip / explore offline" — dismiss and persist so it never auto-opens.
    fn welcome_skip(&mut self, cx: &mut Context<Self>) {
        self.finish_onboarding(cx);
        cx.notify();
    }

    /// Shared close-out for connect/reconnect/skip: drop the open flag, arm
    /// the persisted dismissal, and clear any validation error.
    fn finish_onboarding(&mut self, cx: &mut Context<Self>) {
        self.welcome_state.open = false;
        self.welcome_state.dismissed = true;
        self.welcome_state.error = None;
        mark_onboarded();
        cx.notify();
    }

    /// The saved environment "Reconnect to <name>" targets: the persisted
    /// active selection when valid, else the first saved env — and only when
    /// it isn't already the live endpoint.
    fn reconnect_candidate(&self) -> Option<(usize, SavedEnvironment)> {
        let ix = self
            .client_config
            .active_environment
            .filter(|ix| *ix < self.client_config.environments.len())
            .unwrap_or(0);
        let env = self.client_config.environments.get(ix)?.clone();
        if self
            .active_endpoint
            .as_ref()
            .is_some_and(|e| e.http_url == env.http_url)
        {
            return None;
        }
        Some((ix, env))
    }

    pub(crate) fn render_welcome(&mut self, cx: &mut Context<Self>) -> Div {
        // First run: no endpoint configured and onboarding never dismissed.
        let first_run = self.active_endpoint.is_none() && !self.welcome_state.dismissed;
        if !(self.welcome_state.open || first_run) {
            return div();
        }
        self.ensure_welcome_fields(cx);

        // ── Step 1: run a server ─────────────────────────────────────
        let step_run = step(
            "1",
            "Run a server",
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child("Start one on this machine, or point at a remote T3 environment."),
                )
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::canvas())
                        .text_xs()
                        .font_family("monospace")
                        .text_color(theme::fg())
                        .child("cargo run -p c67-server"),
                ),
        );

        // ── Step 2: endpoint + token ─────────────────────────────────
        let mut fields = div().flex().flex_col().gap_2();
        for (caption, field) in [
            ("Endpoint", self.welcome_state.endpoint_field.clone()),
            ("Token", self.welcome_state.token_field.clone()),
        ] {
            if let Some(field) = field {
                fields = fields.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_xs().text_color(theme::fg_muted()).child(caption))
                        .child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(theme::border())
                                .bg(theme::canvas())
                                .text_sm()
                                .text_color(theme::fg())
                                .child(field),
                        ),
                );
            }
        }
        let step_endpoint = step("2", "Enter its address", fields);

        // ── Step 3: connect ──────────────────────────────────────────
        // With a saved environment available, "Reconnect to <name>" is the
        // primary action and Connect drops to secondary styling.
        let reconnect = self.reconnect_candidate();
        let mut actions = div().flex().items_center().gap_2().pt_1();

        let mut connect_button = div()
            .id("welcome-connect")
            .px_3()
            .py_1p5()
            .rounded_md()
            .text_xs()
            .cursor_pointer()
            .child("Connect");
        if reconnect.is_some() {
            connect_button = connect_button
                .bg(theme::hover())
                .text_color(theme::fg())
                .hover(|s| s.bg(theme::selected()));
        } else {
            connect_button = connect_button
                .bg(theme::primary())
                .text_color(gpui::white())
                .hover(|s| s.opacity(0.9));
        }
        actions = actions.child(connect_button.on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _ev, window, cx| {
                this.welcome_connect_click(window, cx);
            }),
        ));

        if let Some((ix, env)) = reconnect {
            actions = actions.child(
                div()
                    .id("welcome-reconnect")
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .bg(theme::primary())
                    .text_color(gpui::white())
                    .hover(|s| s.opacity(0.9))
                    .child(format!("Reconnect to {}", env.label))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.welcome_reconnect(ix, cx);
                        }),
                    ),
            );
        }

        actions = actions.child(div().flex_1()).child(
            div()
                .id("welcome-skip")
                .px_2()
                .py_1p5()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .text_color(theme::fg_muted())
                .hover(|s| s.text_color(theme::fg()))
                .child("Skip — explore offline")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _window, cx| {
                        this.welcome_skip(cx);
                    }),
                ),
        );

        let mut step_connect = step("3", "Connect", actions);
        if let Some(error) = self.welcome_state.error.clone() {
            step_connect = step_connect.child(
                div()
                    .pt_1()
                    .text_xs()
                    .text_color(rgb(0xf87171))
                    .child(error),
            );
        }
        if self.commands.is_none() {
            step_connect = step_connect.child(
                div()
                    .pt_1()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No live connection yet — Connect saves this environment and it is used on next launch."),
            );
        }

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000b3))
            .on_mouse_down(
                MouseButton::Left,
                // Only an explicit open can be dismissed on the backdrop;
                // first-run auto-open re-renders until Connect or Skip.
                cx.listener(|this, _ev, _window, cx| {
                    this.welcome_state.open = false;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("welcome-panel")
                    .flex()
                    .flex_col()
                    .w(px(540.))
                    .rounded_lg()
                    .bg(theme::raised())
                    .border_1()
                    .border_color(theme::border())
                    .occlude()
                    .px_6()
                    .py_5()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_this, _ev, _window, cx| cx.stop_propagation()),
                    )
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::fg())
                            .child("Welcome to 67 Code"),
                    )
                    .child(
                        div()
                            .pt_1()
                            .text_sm()
                            .text_color(theme::fg_muted())
                            .child("A native client for T3 Code environments. Point it at a server to get started."),
                    )
                    .child(step_run)
                    .child(step_endpoint)
                    .child(step_connect)
                    .child(
                        div()
                            .pt_3()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("You can manage environments later in Settings → Environments."),
                    ),
            )
    }
}
