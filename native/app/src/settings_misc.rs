//! Settings sections: Notifications, Chat defaults, Projects.
//!
//! - **Notifications** mirrors `NotificationSettings.tsx` + the
//!   `inAppNotificationsEnabled` row from `GeneralSettingsPanel`: a
//!   `notificationMode` select (off / notifications / sound /
//!   notifications-and-sound) and an in-app toast switch. The web persists
//!   both as *client* settings (device-local), so they live on `State` here
//!   until `ClientConfig` grows the fields — see the INTEGRATE notes.
//! - **Chat defaults** mirrors the web "New threads" / "Default permissions"
//!   rows (`ProjectDefaultsSettings`): `defaultModelSelection`,
//!   `fallbackModelSelection`, `defaultRuntimeMode`, `defaultThreadEnvMode`,
//!   `defaultAutoPull`. All are ServerSettings written through
//!   `server.updateSettings {patch:{…}}`; the mirror loads lazily from
//!   `server.getConfig → settings` (Cell-flag lazy-load like archived.rs).
//! - **Projects** lists `state.projects` with inline rename
//!   (`project.update`), two-stage delete (`project.delete`), add-existing-
//!   folder (`project.create`) and scaffold-new (`projects.createNew`, which
//!   makes the folder + git + icon server-side). The shell stream's
//!   project.updated/removed events keep the list live.

use std::cell::Cell;
use std::collections::HashSet;

use gpui::{
    Context, Entity, EntityInputHandler, Focusable, MouseButton, SharedString, Subscription,
    Window, div, prelude::*, px, rgb,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppView;
use crate::composer::MODEL_CATALOG;
use crate::notifications::ToastKind;
use crate::text_field::{PromptSubmitted, TextField};
use crate::theme;

/// `notificationMode` options — same labels as `NOTIFICATION_MODE_LABELS`
/// in apps/web/src/threadNotifications.ts.
const NOTIFICATION_MODES: &[(&str, &str)] = &[
    ("off", "Off"),
    ("notifications", "Notifications only"),
    ("sound", "Sound only"),
    ("notifications-and-sound", "Notifications with sound"),
];

/// `defaultRuntimeMode` options — labels/descriptions from
/// apps/web/src/components/chat/runtimeModeConfig.ts.
const RUNTIME_MODES: &[(&str, &str, &str)] = &[
    (
        "approval-required",
        "Supervised",
        "Ask before commands and file changes.",
    ),
    (
        "auto-accept-edits",
        "Auto-accept edits",
        "Auto-approve edits, ask before other actions.",
    ),
    (
        "auto",
        "Auto",
        "Supported providers approve routine actions; others still ask.",
    ),
    (
        "full-access",
        "Full access",
        "Allow commands and edits without prompts.",
    ),
];

/// `defaultThreadEnvMode` options — labels from
/// `resolveEnvModeLabel` in apps/web/src/components/BranchToolbar.logic.ts.
const ENV_MODES: &[(&str, &str)] = &[
    ("local", "Current checkout"),
    ("worktree", "New worktree"),
];

/// Packed-value sentinel for "no explicit selection" rows.
const AUTO_VALUE: &str = "\u{1f}auto";
/// Model option values pack `instanceId` + `model` with a unit separator.
const PACK_SEP: char = '\u{1f}';

fn pack_model(instance: &str, model: &str) -> String {
    format!("{instance}{PACK_SEP}{model}")
}

/// Which inline option list is expanded (at most one at a time).
#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenList {
    NotificationMode,
    DefaultModel,
    FallbackModel,
    RuntimeMode,
    EnvMode,
}

impl State {
    /// Seed the device-local notification prefs from the persisted client
    /// config (called once from `AppView::new`).
    pub(crate) fn seed_from_config(&mut self, cfg: &crate::config::ClientConfig) {
        self.notification_mode = cfg.notification_mode.clone();
        self.in_app_notifications = cfg.in_app_notifications;
    }
}

impl OpenList {
    /// Element-id prefix + trigger id for this list.
    fn prefix(self) -> &'static str {
        match self {
            OpenList::NotificationMode => "misc-notif-mode",
            OpenList::DefaultModel => "misc-default-model",
            OpenList::FallbackModel => "misc-fallback-model",
            OpenList::RuntimeMode => "misc-runtime-mode",
            OpenList::EnvMode => "misc-env-mode",
        }
    }
}

#[derive(Default)]
pub struct State {
    /// ServerSettings mirror (`server.getConfig → settings`; replaced with
    /// the merged object `server.updateSettings` returns).
    settings: Value,
    /// `Cell` flags because the lazy-load kick runs from `&self` render.
    settings_loaded: Cell<bool>,
    settings_loading: Cell<bool>,
    /// Last settings load/write failure, shown as a status line.
    settings_error: Option<String>,
    /// Expanded option list, if any.
    open_list: Option<OpenList>,

    // ── Notification prefs (client/device-local in the web) ─────────
    // Persisted via `ClientConfig.notification_mode` / `.in_app_notifications`
    // (seeded on startup; written back on change).
    notification_mode: String,
    in_app_notifications: bool,
    // INTEGRATE: notifications.rs — consume `notification_mode` to gate the
    //   native alert backend (OS toast / sound) once one exists, and
    //   `in_app_notifications` to gate the "another thread finished" toasts
    //   while the window is focused.

    // ── Projects section ────────────────────────────────────────────
    /// Add-project form visible.
    add_open: bool,
    /// Draft mirrors — render is `&self`, so observers sync field text here
    /// (same pattern as envs.rs `draft_*`).
    draft_name: String,
    draft_path: String,
    draft_rename: String,
    /// Project id whose row shows the inline rename field.
    renaming: Option<String>,
    /// Project id armed for a second Delete click.
    confirm_delete: Option<String>,
    /// Project ids with a mutation in flight (row buttons disabled).
    pending: HashSet<String>,
    /// A create/add-folder request is in flight.
    creating: bool,
    /// Last project mutation failure, shown inline.
    projects_error: Option<String>,
    /// Lazily built fields (`cx.new` needs a context — envs.rs pattern).
    name_field: Option<Entity<TextField>>,
    path_field: Option<Entity<TextField>>,
    rename_field: Option<Entity<TextField>>,
    subs: Vec<Subscription>,
}

// ─── small render helpers (mirroring settings.rs section style) ───────────

/// Small caption above a group of rows.
fn group_label(text: &'static str) -> gpui::Div {
    div()
        .pb_1()
        .pt_3()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(text)
}

/// Destructive accent used across the app (palette/sidebar danger).
fn danger() -> gpui::Rgba {
    rgb(0xf87171)
}

/// A settings row: title + muted description on the left, control on the
/// right — the compact shape `SettingsRow` gives the web.
fn setting_row(
    title: &'static str,
    description: &'static str,
    control: impl IntoElement,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_4()
        .py_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::fg())
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(description),
                ),
        )
        .child(control)
}

/// A simple switch (web `Switch`): pill with a knob on the on/off side.
fn switch_control(on: bool) -> gpui::Div {
    div()
        .w(px(32.))
        .h(px(18.))
        .flex_none()
        .rounded_full()
        .px(px(2.))
        .bg(if on { theme::primary() } else { theme::border() })
        .flex()
        .items_center()
        .when(on, |d| d.justify_end())
        .child(
            div()
                .w(px(14.))
                .h(px(14.))
                .rounded_full()
                .bg(gpui::white()),
        )
}

/// A readable message out of a Failure `Exit` frame's cause (same shape
/// `exit_failure_message` in archived.rs digs through).
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

// ─── TextField helpers (copied pattern from envs.rs — they're private
//     there). Listeners with a `window` prefer `field.update` directly. ────

/// Read a text field's contents when no `&mut Window` is on the update stack.
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
/// one (entity subscriptions/observers).
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

// ─── settings mirror plumbing ─────────────────────────────────────────────

impl AppView {
    /// Whether background-thread settle toasts are enabled (Notifications
    /// settings → "In-app notifications").
    pub(crate) fn in_app_notifications_enabled(&self) -> bool {
        self.misc_settings_state.in_app_notifications
    }

    /// Kick off `server.getConfig` on first render of a settings-backed
    /// section. With no command channel there is nothing to fetch — mark it
    /// loaded so defaults show instead of retrying every frame.
    fn maybe_load_misc_settings(&self, cx: &mut Context<Self>) {
        let state = &self.misc_settings_state;
        if state.settings_loaded.get() || state.settings_loading.get() {
            return;
        }
        match self.send_query("server.getConfig", json!({})) {
            Some(rx) => {
                state.settings_loading.set(true);
                cx.spawn(async move |this, cx| {
                    let reply = rx.recv().await;
                    let _ = this.update(cx, |this, cx| {
                        let state = &mut this.misc_settings_state;
                        state.settings_loading.set(false);
                        state.settings_loaded.set(true);
                        match reply {
                            Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                                state.settings = exit["value"]["settings"].clone();
                                state.settings_error = None;
                            }
                            Ok(exit) => {
                                state.settings_error =
                                    Some(format!("Settings failed to load: {}", exit_failure_message(&exit)));
                            }
                            Err(_) => {
                                state.settings_error =
                                    Some("Lost connection before the reply arrived.".into());
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            None => state.settings_loaded.set(true),
        }
    }

    /// `server.updateSettings {patch}` — optimistic top-level merge into the
    /// mirror; the reply carries the full merged object and replaces it. A
    /// failure marks the mirror stale so the next render refetches.
    fn update_server_settings(&mut self, patch: Value, cx: &mut Context<Self>) {
        let Some(rx) = self.send_query("server.updateSettings", json!({ "patch": patch })) else {
            self.misc_settings_state.settings_error =
                Some("Not connected to a server.".into());
            cx.notify();
            return;
        };
        if let Value::Object(settings) = &mut self.misc_settings_state.settings
            && let Value::Object(patch) = &patch
        {
            for (key, value) in patch {
                settings.insert(key.clone(), value.clone());
            }
        }
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.misc_settings_state;
                match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        state.settings = exit["value"].clone();
                        state.settings_error = None;
                    }
                    Ok(exit) => {
                        state.settings_error = Some(format!(
                            "Setting not saved: {}",
                            exit_failure_message(&exit)
                        ));
                        state.settings_loaded.set(false);
                    }
                    Err(_) => {
                        state.settings_error =
                            Some("Lost connection before the reply arrived.".into());
                        state.settings_loaded.set(false);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Fold an expanded select list's pick into state / server settings.
    fn misc_pick(&mut self, list: OpenList, value: String, cx: &mut Context<Self>) {
        self.misc_settings_state.open_list = None;
        match list {
            OpenList::NotificationMode => {
                self.misc_settings_state.notification_mode = value.clone();
                self.client_config.notification_mode = value;
                self.client_config.save();
            }
            OpenList::DefaultModel => {
                let patch = if value == AUTO_VALUE {
                    json!({ "defaultModelSelection": null })
                } else {
                    let (instance, model) = value.split_once(PACK_SEP).unwrap_or(("", ""));
                    json!({ "defaultModelSelection": { "instanceId": instance, "model": model } })
                };
                self.update_server_settings(patch, cx);
            }
            OpenList::FallbackModel => {
                let patch = if value == AUTO_VALUE {
                    json!({ "fallbackModelSelection": null })
                } else {
                    let (instance, model) = value.split_once(PACK_SEP).unwrap_or(("", ""));
                    json!({ "fallbackModelSelection": { "instanceId": instance, "model": model } })
                };
                self.update_server_settings(patch, cx);
            }
            OpenList::RuntimeMode => {
                self.update_server_settings(json!({ "defaultRuntimeMode": value }), cx);
            }
            OpenList::EnvMode => {
                let mode = if value == "inherit" {
                    Value::Null
                } else {
                    Value::String(value)
                };
                self.update_server_settings(json!({ "defaultThreadEnvMode": mode }), cx);
            }
        }
        cx.notify();
    }

    /// Value-label pair for a stored `ModelSelection` (`{instanceId, model}`,
    /// tolerating the legacy `provider` key).
    fn selection_label(selection: &Value) -> String {
        let instance = selection["instanceId"]
            .as_str()
            .or_else(|| selection["provider"].as_str())
            .unwrap_or_default();
        let model = selection["model"].as_str().unwrap_or_default();
        if instance.is_empty() && model.is_empty() {
            return "Automatic".to_string();
        }
        MODEL_CATALOG
            .iter()
            .find(|(i, m, _)| *i == instance && *m == model)
            .map(|(_, _, label)| (*label).to_string())
            .unwrap_or_else(|| format!("{instance}/{model}"))
    }

    /// The trigger control of a select row — current value + ▾; click
    /// toggles the option list open/closed.
    fn select_trigger(
        &self,
        list: OpenList,
        current: SharedString,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(list.prefix())
            .px_3()
            .py_1p5()
            .rounded_md()
            .text_xs()
            .cursor_pointer()
            .bg(theme::hover())
            .text_color(theme::fg())
            .hover(|s| s.bg(theme::selected()))
            .child(format!("{current} ▾"))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _window, cx| {
                    let state = &mut this.misc_settings_state;
                    state.open_list = if state.open_list == Some(list) {
                        None
                    } else {
                        Some(list)
                    };
                    cx.notify();
                }),
            )
    }

    /// One select setting: row + (when open) the option list underneath.
    fn select_setting(
        &self,
        title: &'static str,
        description: &'static str,
        list: OpenList,
        current: SharedString,
        current_value: &str,
        options: Vec<(String, String, Option<String>)>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let open = self.misc_settings_state.open_list == Some(list);
        let mut out = div().flex().flex_col().child(setting_row(
            title,
            description,
            self.select_trigger(list, current, cx),
        ));
        if open {
            let mut list_div = div().flex().flex_col().pb_1();
            for (index, (value, label, detail)) in options.iter().enumerate() {
                let value = value.clone();
                let selected = value == current_value;
                list_div = list_div.child(
                    div()
                        .id((list.prefix(), index))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .text_sm()
                        .cursor_pointer()
                        .text_color(theme::fg())
                        .when(selected, |s| s.bg(theme::selected()))
                        .when(!selected, |s| s.hover(|s| s.bg(theme::hover())))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.misc_pick(list, value.clone(), cx);
                            }),
                        )
                        .child(
                            div()
                                .w(px(14.))
                                .flex_none()
                                .text_color(theme::primary())
                                .child(if selected { "✓" } else { "" }),
                        )
                        .child(div().flex_none().child(label.clone()))
                        .when_some(detail.clone(), |s, detail| {
                            s.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme::fg_muted())
                                    .child(detail),
                            )
                        }),
                );
            }
            out = out.child(list_div);
        }
        out
    }

    // ─── Notifications ────────────────────────────────────────────────────

    pub(crate) fn render_notifications_section(&self, cx: &mut Context<Self>) -> gpui::Div {
        let state = &self.misc_settings_state;
        let mut section = div().flex().flex_col();

        // notificationMode — the web stores this in ClientSettings (device-
        // local persistence); here it lives on misc state (INTEGRATE note in
        // `State` above) and drives the same four-way select.
        let mode = NOTIFICATION_MODES
            .iter()
            .map(|(value, _)| *value)
            .find(|value| *value == state.notification_mode)
            .unwrap_or("off");
        let mode_label = NOTIFICATION_MODES
            .iter()
            .find(|(value, _)| *value == mode)
            .map(|(_, label)| *label)
            .unwrap_or("Off");
        section = section.child(self.select_setting(
            "Thread notifications",
            "System alerts when a thread finishes, fails, or needs input or approval. Applies to this device while the app is open.",
            OpenList::NotificationMode,
            mode_label.into(),
            mode,
            NOTIFICATION_MODES
                .iter()
                .map(|(value, label)| (value.to_string(), label.to_string(), None))
                .collect(),
            cx,
        ));

        let in_app = state.in_app_notifications;
        section = section.child(setting_row(
            "In-app notifications",
            "Show a toast when another thread finishes, fails, or needs input or approval while this app has focus.",
            div()
                .id("misc-inapp-switch")
                .cursor_pointer()
                .child(switch_control(in_app))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _window, cx| {
                        let state = &mut this.misc_settings_state;
                        state.in_app_notifications = !state.in_app_notifications;
                        this.client_config.in_app_notifications = state.in_app_notifications;
                        this.client_config.save();
                        cx.notify();
                    }),
                ),
        ));

        section
    }

    // ─── Chat defaults ────────────────────────────────────────────────────

    pub(crate) fn render_chat_defaults_section(&self, cx: &mut Context<Self>) -> gpui::Div {
        self.maybe_load_misc_settings(cx);
        let state = &self.misc_settings_state;
        let settings = &state.settings;
        let connected = self.commands.is_some();
        let mut section = div().flex().flex_col();

        if !connected {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Connect to a server to change chat defaults."),
            );
        } else if state.settings_loading.get() && !state.settings_loaded.get() {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Loading settings…"),
            );
        }
        if let Some(error) = &state.settings_error {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(error.clone()),
            );
        }

        // ── Default model (defaultModelSelection) ────────────────────────
        // INTEGRATE: lib.rs `on_prompt_submitted` + composer.model_selection()
        //   still pick from self.provider/self.model (env/config); they should
        //   prefer the server's defaultModelSelection for new threads once a
        //   shared accessor lands.
        let default_selection = &settings["defaultModelSelection"];
        let default_value = if default_selection.is_null() {
            AUTO_VALUE.to_string()
        } else {
            pack_model(
                default_selection["instanceId"]
                    .as_str()
                    .or_else(|| default_selection["provider"].as_str())
                    .unwrap_or_default(),
                default_selection["model"].as_str().unwrap_or_default(),
            )
        };
        let mut model_options = vec![(
            AUTO_VALUE.to_string(),
            "Automatic".to_string(),
            Some("Picked from the thread's provider.".to_string()),
        )];
        for (instance, model, label) in MODEL_CATALOG {
            model_options.push((
                pack_model(instance, model),
                (*label).to_string(),
                Some(format!("{instance}/{model}")),
            ));
        }
        let default_label: SharedString = if default_selection.is_null() {
            "Automatic".into()
        } else {
            Self::selection_label(default_selection).into()
        };
        section = section.child(self.select_setting(
            "Model",
            "Default model for new threads. Projects can override it.",
            OpenList::DefaultModel,
            default_label,
            &default_value,
            model_options,
            cx,
        ));

        // ── Fallback model (fallbackModelSelection) ──────────────────────
        let fallback_selection = &settings["fallbackModelSelection"];
        let fallback_value = if fallback_selection.is_null() {
            AUTO_VALUE.to_string()
        } else {
            pack_model(
                fallback_selection["instanceId"]
                    .as_str()
                    .or_else(|| fallback_selection["provider"].as_str())
                    .unwrap_or_default(),
                fallback_selection["model"].as_str().unwrap_or_default(),
            )
        };
        let mut fallback_options = vec![(AUTO_VALUE.to_string(), "None".to_string(), None)];
        for (instance, model, label) in MODEL_CATALOG {
            fallback_options.push((
                pack_model(instance, model),
                (*label).to_string(),
                Some(format!("{instance}/{model}")),
            ));
        }
        let fallback_label: SharedString = if fallback_selection.is_null() {
            "None".into()
        } else {
            Self::selection_label(fallback_selection).into()
        };
        section = section.child(self.select_setting(
            "Fallback model",
            "Used for new threads when the default model's provider is out of usage.",
            OpenList::FallbackModel,
            fallback_label,
            &fallback_value,
            fallback_options,
            cx,
        ));

        // ── Default permissions (defaultRuntimeMode) ─────────────────────
        let runtime_mode = settings["defaultRuntimeMode"]
            .as_str()
            .unwrap_or("full-access");
        let runtime_label = RUNTIME_MODES
            .iter()
            .find(|(value, _, _)| *value == runtime_mode)
            .map(|(_, label, _)| *label)
            .unwrap_or(runtime_mode);
        section = section.child(self.select_setting(
            "Default permissions",
            "Default permissions for new threads. Projects can override them.",
            OpenList::RuntimeMode,
            runtime_label.into(),
            runtime_mode,
            RUNTIME_MODES
                .iter()
                .map(|(value, label, detail)| {
                    (
                        value.to_string(),
                        label.to_string(),
                        Some(detail.to_string()),
                    )
                })
                .collect(),
            cx,
        ));

        // ── Workspace (defaultThreadEnvMode; null inherits) ──────────────
        let env_mode = settings["defaultThreadEnvMode"].as_str();
        let env_value = env_mode.unwrap_or("inherit");
        let env_label = match env_mode {
            None => "Inherit",
            Some("local") => "Current checkout",
            Some("worktree") => "New worktree",
            Some(other) => other,
        };
        let mut env_options = vec![(
            "inherit".to_string(),
            "Inherit".to_string(),
            Some("Follow the project's t3.json, then local.".to_string()),
        )];
        for (value, label) in ENV_MODES {
            env_options.push((value.to_string(), label.to_string(), None));
        }
        section = section.child(self.select_setting(
            "Workspace",
            "Where new threads start. Projects and their t3.json can override it.",
            OpenList::EnvMode,
            env_label.into(),
            env_value,
            env_options,
            cx,
        ));

        // ── Automatically pull (defaultAutoPull) ─────────────────────────
        let auto_pull = settings["defaultAutoPull"].as_bool() == Some(true);
        section = section.child(setting_row(
            "Automatically pull",
            "Keeps the default branch current when the checkout has no local changes or commits. Projects can override it.",
            div()
                .id("misc-autopull-switch")
                .cursor_pointer()
                .child(switch_control(auto_pull))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        this.update_server_settings(
                            json!({ "defaultAutoPull": !auto_pull }),
                            cx,
                        );
                    }),
                ),
        ));

        section
    }

    // ─── Projects ─────────────────────────────────────────────────────────

    /// Lazily create the project fields + observers/subscriptions (the
    /// `ensure_env_fields` pattern: entities need `cx.new` + a window for
    /// focus, so they are built on first open rather than in `State::default`).
    fn ensure_project_fields(&mut self, cx: &mut Context<Self>) {
        if self.misc_settings_state.name_field.is_some() {
            return;
        }
        let name_field = cx.new(|cx| TextField::new("Project name", cx));
        let path_field = cx.new(|cx| TextField::new("/path/to/project", cx));
        let rename_field = cx.new(|cx| TextField::new("Project name", cx));

        self.misc_settings_state.subs.push(cx.observe(
            &name_field,
            |this, field, cx| {
                this.misc_settings_state.draft_name = field_text(&field, cx);
                cx.notify();
            },
        ));
        self.misc_settings_state.subs.push(cx.observe(
            &path_field,
            |this, field, cx| {
                this.misc_settings_state.draft_path = field_text(&field, cx);
                cx.notify();
            },
        ));
        self.misc_settings_state.subs.push(cx.observe(
            &rename_field,
            |this, field, cx| {
                this.misc_settings_state.draft_rename = field_text(&field, cx);
                cx.notify();
            },
        ));

        self.misc_settings_state.subs.push(cx.subscribe(
            &name_field,
            |this, _field, event: &PromptSubmitted, cx| {
                let name = event.0.to_string();
                this.create_new_project(name, cx);
            },
        ));
        self.misc_settings_state.subs.push(cx.subscribe(
            &path_field,
            |this, _field, event: &PromptSubmitted, cx| {
                let path = event.0.to_string();
                this.add_project_folder(path, cx);
            },
        ));
        self.misc_settings_state.subs.push(cx.subscribe(
            &rename_field,
            |this, _field, event: &PromptSubmitted, cx| {
                let title = event.0.to_string();
                this.submit_project_rename(title, cx);
            },
        ));

        let state = &mut self.misc_settings_state;
        state.name_field = Some(name_field);
        state.path_field = Some(path_field);
        state.rename_field = Some(rename_field);
    }

    /// "+ Add project" opener: build fields on first use, clear drafts, and
    /// focus the name input.
    fn open_project_add_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_project_fields(cx);
        let state = &mut self.misc_settings_state;
        state.add_open = true;
        state.projects_error = None;
        state.confirm_delete = None;
        state.renaming = None;
        state.draft_name.clear();
        state.draft_path.clear();
        for field in [state.name_field.clone(), state.path_field.clone()]
            .into_iter()
            .flatten()
        {
            field.update(cx, |field, cx| field.reset(window, cx));
        }
        if let Some(field) = self.misc_settings_state.name_field.clone() {
            field.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// Send a `projects.mutate` request; `pending` (a project id) disables
    /// the row's buttons until the reply lands.
    fn project_mutate(&mut self, payload: Value, pending: Option<String>, cx: &mut Context<Self>) {
        if let Some(id) = &pending {
            self.misc_settings_state.pending.insert(id.clone());
        }
        let Some(rx) = self.send_query("projects.mutate", payload) else {
            self.misc_settings_state.projects_error =
                Some("Not connected to a server.".to_string());
            cx.notify();
            return;
        };
        self.misc_settings_state.projects_error = None;
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.misc_settings_state;
                if let Some(id) = &pending {
                    state.pending.remove(id);
                }
                state.creating = false;
                match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        state.projects_error = None;
                        state.add_open = false;
                        state.renaming = None;
                        state.confirm_delete = None;
                    }
                    Ok(exit) => {
                        state.projects_error = Some(exit_failure_message(&exit));
                    }
                    Err(_) => {
                        state.projects_error =
                            Some("Lost connection before the reply arrived.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// `projects.createNew {name}` — server scaffolds a folder + git + icon
    /// under its projects root and returns `{projectId, workspaceRoot}`.
    fn create_new_project(&mut self, name: String, cx: &mut Context<Self>) {
        let name = name.trim().to_string();
        if name.is_empty() || self.misc_settings_state.creating {
            return;
        }
        let Some(rx) = self.send_query("projects.createNew", json!({ "name": name })) else {
            self.misc_settings_state.projects_error =
                Some("Not connected to a server.".to_string());
            cx.notify();
            return;
        };
        self.misc_settings_state.creating = true;
        self.misc_settings_state.projects_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.misc_settings_state;
                state.creating = false;
                match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        state.projects_error = None;
                        state.add_open = false;
                        state.draft_name.clear();
                        if let Some(field) = state.name_field.clone() {
                            edit_field(&field, cx, |field, window, cx| {
                                field.reset(window, cx)
                            });
                        }
                        let commit_error = exit["value"]["commitError"].as_str();
                        this.push_toast(
                            if commit_error.is_some() {
                                ToastKind::Info
                            } else {
                                ToastKind::Success
                            },
                            "Project created",
                            commit_error.map(str::to_string),
                            None,
                            cx,
                        );
                    }
                    Ok(exit) => {
                        state.projects_error = Some(exit_failure_message(&exit));
                    }
                    Err(_) => {
                        state.projects_error =
                            Some("Lost connection before the reply arrived.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// "Add folder": `project.create` at an existing workspace root. The
    /// folder is created client-side when missing (the web flags
    /// `createWorkspaceRootIfMissing`); the Rust server doesn't interpret
    /// that flag, so the app does it itself — same machine assumption as the
    /// rest of the local workflow.
    // INTEGRATE: a real folder picker wants a `filesystem.browse` RPC; the
    //   path TextField is the documented stand-in.
    fn add_project_folder(&mut self, path: String, cx: &mut Context<Self>) {
        let path = expand_home(&path);
        if path.is_empty() || self.misc_settings_state.creating {
            return;
        }
        // Reject duplicates — a second project on the same root is a UI bug
        // the server won't catch.
        if self
            .state
            .projects
            .iter()
            .any(|p| p.path.as_ref() == path)
        {
            self.misc_settings_state.projects_error =
                Some("That folder is already a project.".to_string());
            cx.notify();
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&path) {
            self.misc_settings_state.projects_error =
                Some(format!("Could not create folder: {e}"));
            cx.notify();
            return;
        }
        let title = std::path::Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .unwrap_or("Project")
            .to_string();
        let project_id = Uuid::new_v4().to_string();
        self.project_mutate(
            json!({
                "type": "project.create",
                "commandId": format!("c67-app:{project_id}"),
                "projectId": project_id,
                "title": title,
                "workspaceRoot": path,
            }),
            None,
            cx,
        );
    }

    /// "Rename" on a row: swap the title for the shared rename field, seeded
    /// with the current name and focused.
    fn start_project_rename(
        &mut self,
        project_id: String,
        current: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_project_fields(cx);
        let state = &mut self.misc_settings_state;
        state.renaming = Some(project_id);
        state.confirm_delete = None;
        state.add_open = false;
        state.projects_error = None;
        state.draft_rename = current.to_string();
        if let Some(field) = state.rename_field.clone() {
            field.update(cx, |field, cx| {
                field.reset(window, cx);
                field.replace_text_in_range(Some(0..0), current, window, cx);
            });
            field.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// Rename via Enter or Save: `project.update {title}`.
    fn submit_project_rename(&mut self, title: String, cx: &mut Context<Self>) {
        let title = title.trim().to_string();
        let Some(project_id) = self.misc_settings_state.renaming.clone() else {
            return;
        };
        if title.is_empty() {
            self.misc_settings_state.renaming = None;
            cx.notify();
            return;
        }
        self.project_mutate(
            json!({
                "type": "project.update",
                "commandId": format!("c67-app:{}", Uuid::new_v4()),
                "projectId": project_id,
                "title": title,
            }),
            Some(project_id),
            cx,
        );
    }

    /// Save button for the rename row — reads the field through the
    /// listener's `window` (the PromptSubmitted path passes the text in).
    fn submit_project_rename_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(field) = self.misc_settings_state.rename_field.clone() else {
            return;
        };
        let title = field_text_in(&field, window, cx);
        self.submit_project_rename(title, cx);
    }

    /// Two-stage delete: first click arms ("Confirm delete"), second sends
    /// `project.delete` with `force` — the web's confirm dialog covers
    /// deleting the project's threads too; the arm click is ours.
    fn delete_project(&mut self, project_id: String, cx: &mut Context<Self>) {
        let state = &mut self.misc_settings_state;
        if state.pending.contains(&project_id) {
            return;
        }
        if state.confirm_delete.as_deref() != Some(project_id.as_str()) {
            state.confirm_delete = Some(project_id);
            state.renaming = None;
            cx.notify();
            return;
        }
        state.confirm_delete = None;
        let pending_id = project_id.clone();
        self.project_mutate(
            json!({
                "type": "project.delete",
                "commandId": format!("c67-app:{}", Uuid::new_v4()),
                "projectId": project_id,
                "force": true,
            }),
            Some(pending_id),
            cx,
        );
    }

    pub(crate) fn render_projects_section(&self, cx: &mut Context<Self>) -> gpui::Div {
        let state = &self.misc_settings_state;
        let connected = self.commands.is_some();
        let active_project = self
            .state
            .selected_thread()
            .map(|t| t.project_id.to_string());
        let mut section = div().flex().flex_col();

        if !connected {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Connect to a server to manage projects."),
            );
        }

        section = section.child(group_label("Projects"));

        if self.state.projects.is_empty() {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No projects on this server yet — add one below."),
            );
        }

        for (index, project) in self.state.projects.iter().enumerate() {
            let project_id = project.id.to_string();
            let active = active_project.as_deref() == Some(project_id.as_str());
            let pending = state.pending.contains(&project_id);
            let renaming = state.renaming.as_deref() == Some(project_id.as_str());
            let armed = state.confirm_delete.as_deref() == Some(project_id.as_str());

            if renaming {
                let mut row = div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .text_sm()
                    .bg(theme::selected());
                if let Some(field) = state.rename_field.clone() {
                    row = row.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::canvas())
                            .text_color(theme::fg())
                            .child(field),
                    );
                }
                row = row
                    .child(
                        div()
                            .id(("project-rename-save", index))
                            .px_3()
                            .py_1p5()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .bg(theme::primary())
                            .text_color(gpui::white())
                            .child("Save")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, window, cx| {
                                    this.submit_project_rename_click(window, cx);
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id(("project-rename-cancel", index))
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
                                    this.misc_settings_state.renaming = None;
                                    cx.notify();
                                }),
                            ),
                    );
                section = section.child(row);
                continue;
            }

            let mut row = div()
                .id(("project-row", index))
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
                        .child(project.name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(project.path.clone()),
                );

            if active {
                row = row.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme::primary())
                        .child("Active"),
                );
            }

            // Rename — disabled while a mutation is in flight on this row.
            let rename_id = project_id.clone();
            let rename_current = project.name.to_string();
            let mut rename_btn = div()
                .id(("project-rename", index))
                .px_3()
                .py_1p5()
                .rounded_md()
                .text_xs()
                .text_color(theme::fg_muted());
            if pending {
                rename_btn = rename_btn.opacity(0.4).child("Rename");
            } else {
                rename_btn = rename_btn
                    .cursor_pointer()
                    .bg(theme::hover())
                    .hover(|s| s.bg(theme::selected()))
                    .child("Rename")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, window, cx| {
                            this.start_project_rename(rename_id.clone(), &rename_current, window, cx);
                        }),
                    );
            }
            row = row.child(rename_btn);

            // Delete — two-stage arm/confirm (palette/envs pattern).
            let delete_id = project_id.clone();
            let mut delete_btn = div()
                .id(("project-delete", index))
                .px_3()
                .py_1p5()
                .rounded_md()
                .text_xs();
            if pending {
                delete_btn = delete_btn
                    .opacity(0.4)
                    .text_color(theme::fg_muted())
                    .child("Delete");
            } else if armed {
                delete_btn = delete_btn
                    .cursor_pointer()
                    .bg(rgb(0x7a2e2e))
                    .text_color(gpui::white())
                    .hover(|s| s.bg(rgb(0x9a3a3a)))
                    .child("Confirm delete")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.delete_project(delete_id.clone(), cx);
                        }),
                    );
            } else {
                delete_btn = delete_btn
                    .cursor_pointer()
                    .bg(theme::hover())
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::selected()))
                    .child("Delete")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.delete_project(delete_id.clone(), cx);
                        }),
                    );
            }
            row = row.child(delete_btn);
            section = section.child(row);
        }

        if let Some(error) = &state.projects_error {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(error.clone()),
            );
        }

        // ── Add project ──────────────────────────────────────────────────
        if state.add_open {
            let mut form = div().flex().flex_col().gap_2();

            // New project — `projects.createNew` scaffolds a folder, git
            // repo and icon under the server's projects root.
            if let Some(field) = state.name_field.clone() {
                form = form.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child("New project (creates a folder on the server)"),
                        )
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
            let name_ok = !state.draft_name.trim().is_empty();
            let mut create_btn = div()
                .id("project-create-submit")
                .px_3()
                .py_1p5()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .bg(theme::primary())
                .text_color(gpui::white())
                .child("Create project");
            if !name_ok || state.creating {
                create_btn = create_btn.opacity(0.4);
            }
            create_btn = create_btn.on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| {
                    let Some(field) = this.misc_settings_state.name_field.clone() else {
                        return;
                    };
                    let name = field_text_in(&field, window, cx);
                    this.create_new_project(name, cx);
                }),
            );
            form = form.child(div().flex().child(create_btn));

            // Existing folder — `project.create` pointing at a workspace
            // root the user types in (no browse RPC yet).
            if let Some(field) = state.path_field.clone() {
                form = form.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .pt_2()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child("Existing folder (absolute path)"),
                        )
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
            let path_ok = !state.draft_path.trim().is_empty();
            let mut add_btn = div()
                .id("project-add-folder-submit")
                .px_3()
                .py_1p5()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .bg(theme::hover())
                .text_color(theme::fg())
                .child("Add folder");
            if !path_ok || state.creating {
                add_btn = add_btn.opacity(0.4);
            }
            add_btn = add_btn.on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| {
                    let Some(field) = this.misc_settings_state.path_field.clone() else {
                        return;
                    };
                    let path = field_text_in(&field, window, cx);
                    this.add_project_folder(path, cx);
                }),
            );
            form = form.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(add_btn)
                    .child(
                        div()
                            .id("project-add-cancel")
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
                                    let state = &mut this.misc_settings_state;
                                    state.add_open = false;
                                    state.projects_error = None;
                                    cx.notify();
                                }),
                            ),
                    ),
            );
            section = section.child(group_label("Add project")).child(form);
        } else {
            section = section.child(
                div().pt_3().flex().child(
                    div()
                        .id("project-add-open")
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .bg(theme::hover())
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::selected()))
                        .child("+ Add project")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, window, cx| {
                                this.open_project_add_form(window, cx);
                            }),
                        ),
                ),
            );
        }

        section
    }
}

/// Expand a leading `~` or `~/…` against `$HOME` for the add-folder field.
fn expand_home(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| trimmed.to_string());
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    trimmed.to_string()
}
