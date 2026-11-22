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
    Window, div, prelude::*, rgb,
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
    /// Mirrors of the add-form fields. `render_env_add_form` is `&self`, so
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
    /// focus the label input. Called from the Environments settings page
    /// header (settings.rs).
    pub(crate) fn open_env_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(crate) fn is_env_active(&self, ix: usize) -> bool {
        let Some(env) = self.client_config.environments.get(ix) else {
            return false;
        };
        self.active_endpoint
            .as_ref()
            .is_some_and(|e| e.http_url == env.http_url)
    }

    /// Point the app at saved env `ix`: persist the selection, swap the live
    /// endpoint labels other surfaces read, and ask the net loop to reconnect.
    pub(crate) fn connect_environment(&mut self, ix: usize, cx: &mut Context<Self>) {
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
    pub(crate) fn remove_environment(&mut self, ix: usize, cx: &mut Context<Self>) {
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

    /// The inline add-environment form — the web `AddEnvironmentDialog`
    /// flattened into the Environments settings page. Rendered by
    /// settings.rs below the env list while `env_state.add_open`; the
    /// "+ Add environment" header button calls `open_env_form`.
    pub(crate) fn render_env_add_form(&self, cx: &mut Context<Self>) -> gpui::Div {
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

        form.child(
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
        )
    }
}
