//! Environment switching (P2): saved server endpoints with add / remove /
//! connect — mirroring the Connections settings + SavedEnvironmentRows in the
//! web UI. Persisted in `config::ClientConfig.environments`; connecting sends
//! `ClientCommand::SwitchEndpoint` (net::run swaps endpoint and reconnects).
//!
//! The add form uses the lazy-entity `TextField` pattern from palette.rs /
//! sidebar.rs: `Entity<TextField>`s need a `cx` (and a window for focus), so
//! they are created on first open of the form rather than in `State::default`.

use gpui::{
    Context, Entity, EntityInputHandler, Focusable, MouseButton, SharedString, Subscription,
    Window, div, prelude::*, px, rgb,
};

use crate::AppView;
use crate::config::SavedEnvironment;
use crate::model::ConnectionStatus;
use crate::net::{ClientCommand, ServerEndpoint};
use crate::text_field::{PromptSubmitted, TextField};
use crate::theme;

#[derive(Default)]
pub struct State {
    /// Add-environment form visible.
    pub add_open: bool,
    /// Mirrors of the add-form fields. `render_envs_section` is `&self`, so
    /// entity observers sync field text into these plain strings for the Add
    /// button's enabled state.
    pub draft_label: String,
    pub draft_url: String,
    pub draft_token: String,
    /// Inline validation message shown under the form.
    pub error: Option<SharedString>,
    /// Saved-environment index armed for a second Remove click (two-stage
    /// arm/confirm like palette.rs `delete_confirm`).
    pub confirm_remove: Option<usize>,
    /// Add-form inputs, created lazily on first open (`cx.new` needs a context).
    label_field: Option<Entity<TextField>>,
    url_field: Option<Entity<TextField>>,
    token_field: Option<Entity<TextField>>,
    subs: Vec<Subscription>,
}

/// Dot + label + optional detail for the live `ConnectionStatus`.
/// Mirrors `connection_status` in settings.rs (module-private there).
fn status_visual(status: &ConnectionStatus) -> (gpui::Rgba, &'static str, Option<SharedString>) {
    match status {
        ConnectionStatus::Mock => (
            theme::fg_muted(),
            "Offline",
            Some("No server configured — showing built-in mock data.".into()),
        ),
        ConnectionStatus::Connecting => (rgb(0xfbbf24), "Connecting…", None),
        ConnectionStatus::Connected => (rgb(0x34d399), "Connected", None),
        ConnectionStatus::Failed(error) => {
            (rgb(0xf87171), "Connection failed", Some(error.clone()))
        }
        ConnectionStatus::Disconnected(reason) => {
            (rgb(0xf87171), "Disconnected", Some(reason.clone()))
        }
    }
}

/// Small caption above a group of rows (mirrors `group_label` in settings.rs).
fn env_group_label(text: &'static str) -> gpui::Div {
    div()
        .pb_1()
        .pt_3()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(text)
}

/// Read a text field's contents when no `&mut Window` is on the update stack
/// (entity observers run with `&mut App` only). Same helper sidebar.rs uses.
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
/// one (entity subscriptions/observers). Listeners that do receive `window`
/// should prefer `field.update(cx, …)` directly — `cx.with_window` silently
/// no-ops when the entity's window lookup fails.
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

fn reset_field(field: &Entity<TextField>, cx: &mut Context<AppView>) {
    edit_field(field, cx, |field, window, cx| field.reset(window, cx));
}

fn set_field_text(field: &Entity<TextField>, text: &str, cx: &mut Context<AppView>) {
    edit_field(field, cx, |field, window, cx| {
        field.replace_text_in_range(Some(0..usize::MAX), text, window, cx)
    });
}

impl AppView {
    /// Lazily create the add-form fields + observers/subscriptions. Called
    /// from `open_env_form`, which has the `window` focus needs.
    fn ensure_env_fields(&mut self, cx: &mut Context<Self>) {
        if self.env_state.label_field.is_some() {
            return;
        }
        let label_field = cx.new(|cx| TextField::new("Label (e.g. Office box)", cx));
        let url_field = cx.new(|cx| TextField::new("http://host:port", cx));
        let token_field = cx.new(|cx| TextField::new("Bearer token", cx));

        // Keep the draft_* mirrors in sync so `&self` render can gate the Add
        // button on a plausible URL.
        self.env_state
            .subs
            .push(cx.observe(&label_field, |this, field, cx| {
                this.env_state.draft_label = field_text(&field, cx);
                cx.notify();
            }));
        self.env_state
            .subs
            .push(cx.observe(&url_field, |this, field, cx| {
                this.env_state.draft_url = field_text(&field, cx);
                cx.notify();
            }));
        self.env_state
            .subs
            .push(cx.observe(&token_field, |this, field, cx| {
                this.env_state.draft_token = field_text(&field, cx);
                cx.notify();
            }));

        // Enter in any field submits the whole form.
        for field in [&label_field, &url_field, &token_field] {
            self.env_state.subs.push(cx.subscribe(
                field,
                |this, field, event: &PromptSubmitted, cx| {
                    this.submit_env_form(&field, &event.0, cx);
                },
            ));
        }

        self.env_state.label_field = Some(label_field);
        self.env_state.url_field = Some(url_field);
        self.env_state.token_field = Some(token_field);
    }

    /// "Add environment" opener: build fields on first use, clear drafts, and
    /// focus the label input.
    fn open_env_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_env_fields(cx);
        self.env_state.add_open = true;
        self.env_state.error = None;
        self.env_state.confirm_remove = None;
        self.env_state.draft_label.clear();
        self.env_state.draft_url.clear();
        self.env_state.draft_token.clear();
        // Listener has `window` — update the field entities directly (the
        // `cx.with_window` fallback in `reset_field` can't be relied on here).
        for field in [
            self.env_state.label_field.clone(),
            self.env_state.url_field.clone(),
            self.env_state.token_field.clone(),
        ]
        .into_iter()
        .flatten()
        {
            field.update(cx, |field, cx| field.reset(window, cx));
        }
        if let Some(field) = self.env_state.label_field.clone() {
            field.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// Enter pressed in an add-form field. `submitted` carries the submitting
    /// field's text (its content is already reset by the time this runs); the
    /// other fields are read out via `field_text`. Same shape as sidebar.rs
    /// `submit_project_form`.
    fn submit_env_form(
        &mut self,
        submitter: &Entity<TextField>,
        submitted: &SharedString,
        cx: &mut Context<Self>,
    ) {
        let (Some(label_f), Some(url_f), Some(token_f)) = (
            self.env_state.label_field.clone(),
            self.env_state.url_field.clone(),
            self.env_state.token_field.clone(),
        ) else {
            return;
        };
        let label = if submitter == &label_f {
            submitted.to_string()
        } else {
            field_text(&label_f, cx)
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
        let restore = if submitter == &label_f {
            (label_f, label.clone())
        } else if submitter == &url_f {
            (url_f, url.clone())
        } else {
            (token_f, token.clone())
        };
        self.add_environment(label, url, token, Some(restore), cx);
    }

    /// "Add" button in the form — reads the fields through the listener's
    /// `window` instead of the observer mirrors.
    fn add_environment_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(label_f), Some(url_f), Some(token_f)) = (
            self.env_state.label_field.clone(),
            self.env_state.url_field.clone(),
            self.env_state.token_field.clone(),
        ) else {
            return;
        };
        let label = field_text_in(&label_f, window, cx);
        let url = field_text_in(&url_f, window, cx);
        let token = field_text_in(&token_f, window, cx);
        self.add_environment(label, url, token, None, cx);
        if !self.env_state.add_open {
            // Added — the field entities persist for reuse, so clear them
            // here where `window` is guaranteed (same path palette.rs uses).
            for field in [&label_f, &url_f, &token_f] {
                field.update(cx, |field, cx| field.reset(window, cx));
            }
        }
    }

    /// Validate + persist a new saved environment. On failure the submitter's
    /// cleared text is restored and focus moves to the URL field.
    fn add_environment(
        &mut self,
        label: String,
        url: String,
        token: String,
        restore: Option<(Entity<TextField>, String)>,
        cx: &mut Context<Self>,
    ) {
        let url = url.trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.env_state.error = Some("URL must start with http:// or https://".into());
            if let Some((field, text)) = restore {
                set_field_text(&field, &text, cx);
            }
            if let Some(url_field) = self.env_state.url_field.clone() {
                focus_field(&url_field, cx);
            }
            cx.notify();
            return;
        }
        let label = {
            let trimmed = label.trim();
            if trimmed.is_empty() {
                url.trim_start_matches("http://")
                    .trim_start_matches("https://")
                    .to_string()
            } else {
                trimmed.to_string()
            }
        };
        self.client_config.environments.push(SavedEnvironment {
            label,
            http_url: url,
            token: token.trim().to_string(),
        });
        self.client_config.save();
        for field in [
            self.env_state.label_field.clone(),
            self.env_state.url_field.clone(),
            self.env_state.token_field.clone(),
        ]
        .into_iter()
        .flatten()
        {
            reset_field(&field, cx);
        }
        self.env_state.add_open = false;
        self.env_state.error = None;
        self.env_state.confirm_remove = None;
        cx.notify();
    }

    /// True while saved env `ix` is the endpoint the net loop is pointed at.
    /// URL match (not just `active_environment`) so a stale persisted index —
    /// e.g. env vars winning over the saved selection at launch — can't show
    /// a false "Active" badge.
    fn is_env_active(&self, ix: usize) -> bool {
        let Some(env) = self.client_config.environments.get(ix) else {
            return false;
        };
        self.active_endpoint
            .as_ref()
            .is_some_and(|e| e.http_url == env.http_url)
    }

    /// Point the app at saved env `ix`: persist the selection, swap the live
    /// endpoint labels other surfaces read, and ask the net loop to reconnect.
    fn connect_environment(&mut self, ix: usize, cx: &mut Context<Self>) {
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
        if let Some(commands) = &self.commands {
            let _ = commands.send(ClientCommand::SwitchEndpoint {
                http_url: env.http_url,
                token: env.token,
            });
            self.state.connection = ConnectionStatus::Connecting;
        }
        // No `commands` means the app started with no endpoint (offline mock)
        // and no net thread exists to switch — the saved selection still
        // applies on next launch.
        self.env_state.confirm_remove = None;
        cx.notify();
    }

    /// Two-stage remove: first click arms the row, second confirms. Refuses
    /// while the row is the live endpoint (removing it would strand the
    /// connection).
    fn remove_environment(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.is_env_active(ix) {
            return;
        }
        if self.env_state.confirm_remove != Some(ix) {
            self.env_state.confirm_remove = Some(ix);
            cx.notify();
            return;
        }
        self.env_state.confirm_remove = None;
        if ix < self.client_config.environments.len() {
            self.client_config.environments.remove(ix);
            // Keep the persisted selection index pointing at the same env.
            if let Some(active) = self.client_config.active_environment {
                self.client_config.active_environment = match active.cmp(&ix) {
                    std::cmp::Ordering::Greater => Some(active - 1),
                    std::cmp::Ordering::Equal => None,
                    std::cmp::Ordering::Less => Some(active),
                };
            }
            self.client_config.save();
        }
        cx.notify();
    }

    pub fn render_envs_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (dot_color, status_label, status_detail) = status_visual(&self.state.connection);

        let mut section = div().flex().flex_col();

        // ── This app: live endpoint + status, mirroring the Connection
        // section's dot + label + detail. ──────────────────────────────
        section = section.child(env_group_label("This app")).child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .py_1p5()
                .text_sm()
                .child(
                    div()
                        .w(px(8.))
                        .h(px(8.))
                        .flex_none()
                        .rounded_full()
                        .bg(dot_color),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(theme::fg())
                        .child(status_label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(self.endpoint_label.clone()),
                ),
        );
        if let Some(detail) = status_detail {
            section = section.child(
                div()
                    .pb_1()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(detail),
            );
        }
        if self.commands.is_none() && self.active_endpoint.is_some() {
            section = section.child(
                div()
                    .pb_1()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Selected environment is saved and will be used on next launch."),
            );
        }

        // ── Saved environments ───────────────────────────────────────
        section = section.child(env_group_label("Saved environments"));

        if self.client_config.environments.is_empty() {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No saved environments yet — add one below to switch servers without restarting."),
            );
        }

        for (ix, env) in self.client_config.environments.iter().enumerate() {
            let active = self.is_env_active(ix);
            let armed = self.env_state.confirm_remove == Some(ix);

            let mut row = div()
                .id(("env-row", ix))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded_md()
                .text_sm();
            if active {
                row = row.bg(theme::selected());
            }
            row = row
                .child(
                    div()
                        .w(px(140.))
                        .flex_none()
                        .truncate()
                        .text_color(theme::fg())
                        .child(env.label.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(env.http_url.clone()),
                );

            if active {
                row = row.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme::primary())
                        .child("Active"),
                );
            } else {
                row = row.child(
                    div()
                        .id(("env-connect", ix))
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .bg(theme::hover())
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::selected()))
                        .child("Connect")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.connect_environment(ix, cx);
                            }),
                        ),
                );
            }

            // Remove: disabled on the live row, two-stage arm/confirm
            // everywhere else.
            if active {
                row = row.child(
                    div()
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .opacity(0.4)
                        .child("Remove"),
                );
            } else if armed {
                row = row.child(
                    div()
                        .id(("env-remove", ix))
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .bg(rgb(0x7a2e2e))
                        .text_color(gpui::white())
                        .hover(|s| s.bg(rgb(0x9a3a3a)))
                        .child("Confirm remove")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.remove_environment(ix, cx);
                            }),
                        ),
                );
            } else {
                row = row.child(
                    div()
                        .id(("env-remove", ix))
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .bg(theme::hover())
                        .text_color(theme::fg_muted())
                        .hover(|s| s.bg(theme::selected()))
                        .child("Remove")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.remove_environment(ix, cx);
                            }),
                        ),
                );
            }
            section = section.child(row);
        }

        // A live endpoint that no saved env covers — e.g. supplied via
        // C67_SERVER_URL/C67_TOKEN — is shown read-only.
        let live_url = self.active_endpoint.as_ref().map(|e| e.http_url.clone());
        let live_is_saved = live_url.as_ref().is_some_and(|url| {
            self.client_config
                .environments
                .iter()
                .any(|e| &e.http_url == url)
        });
        if let Some(url) = live_url.filter(|_| !live_is_saved) {
            let from_env = ServerEndpoint::from_env().is_some_and(|e| e.http_url == url);
            let label = if from_env {
                "Current (from environment)"
            } else {
                "Current"
            };
            section = section.child(
                div()
                    .id("env-row-current")
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .text_sm()
                    .bg(theme::selected())
                    .child(
                        div()
                            .w(px(140.))
                            .flex_none()
                            .truncate()
                            .text_color(theme::fg())
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(url),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme::primary())
                            .child("Active"),
                    ),
            );
        }

        // ── Add environment ──────────────────────────────────────────
        if self.env_state.add_open {
            let mut form = div().flex().flex_col().gap_2();
            for (caption, field) in [
                ("Label", self.env_state.label_field.clone()),
                ("URL", self.env_state.url_field.clone()),
                ("Token", self.env_state.token_field.clone()),
            ] {
                if let Some(field) = field {
                    form = form.child(
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

            let draft_url = self.env_state.draft_url.trim();
            let url_ok = draft_url.starts_with("http://") || draft_url.starts_with("https://");
            let mut add_button = div()
                .id("env-add-submit")
                .px_3()
                .py_1p5()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .bg(theme::primary())
                .text_color(gpui::white())
                .child("Add environment");
            if !url_ok {
                add_button = add_button.opacity(0.4);
            }
            add_button = add_button.on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| {
                    this.add_environment_click(window, cx);
                }),
            );

            form = form.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pt_1()
                    .child(add_button)
                    .child(
                        div()
                            .id("env-add-cancel")
                            .px_3()
                            .py_1p5()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .bg(theme::hover())
                            .text_color(theme::fg())
                            .hover(|s| s.bg(theme::selected()))
                            .child("Cancel")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    this.env_state.add_open = false;
                                    this.env_state.error = None;
                                    cx.notify();
                                }),
                            ),
                    )
                    .when_some(self.env_state.error.clone(), |s, error| {
                        s.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(rgb(0xf87171))
                                .child(error),
                        )
                    }),
            );
            section = section
                .child(env_group_label("New environment"))
                .child(form);
        } else {
            section = section.child(
                div().pt_3().flex().child(
                    div()
                        .id("env-add-open")
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .bg(theme::hover())
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::selected()))
                        .child("+ Add environment")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, window, cx| {
                                this.open_env_form(window, cx);
                            }),
                        ),
                ),
            );
        }

        section
    }
}
