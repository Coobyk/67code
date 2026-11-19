//! Settings sections: Notifications, Chat defaults, Projects.
//!
//! - **Notifications** mirrors `NotificationSettings.tsx` + the
//!   `inAppNotificationsEnabled` row from `GeneralSettingsPanel`: a
//!   `notificationMode` select (off / notifications / sound /
//!   notifications-and-sound) and an in-app toast switch. The web persists
//!   both as *client* settings (device-local); here they persist on
//!   `ClientConfig.notification_mode` / `.in_app_notifications`.
//! - **Chat defaults** mirrors the web "New threads" / "Default permissions"
//!   rows (`ProjectDefaultsSettings`): `defaultModelSelection`,
//!   `fallbackModelSelection`, `defaultRuntimeMode`, `defaultThreadEnvMode`,
//!   `defaultAutoPull`. All are ServerSettings written through
//!   `server.updateSettings {patch:{…}}`; the mirror loads lazily from
//!   `server.getConfig → settings` (Cell-flag lazy-load like archived.rs).
//! - **Scope** — a Global/Project selector (`scope_project`, persisted on
//!   `ClientConfig.settings_scope_project`) mirrors the web's scoped
//!   settings: `PROJECT_SCOPED_SERVER_SETTING_KEYS` rows write
//!   `settings.projectSettingsOverrides[projectId][key]` via
//!   `update_scoped_setting`/`clear_scoped_override` (merge by project, like
//!   `planScopedSettingsPatch`), and each scoped row shows an
//!   "override"/"inherited" chip in project scope.
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
const ENV_MODES: &[(&str, &str)] = &[("local", "Current checkout"), ("worktree", "New worktree")];

/// `ProjectScript.icon` literals — the enum in
/// packages/contracts/src/project.ts.
pub(crate) const PROJECT_SCRIPT_ICONS: &[&str] =
    &["play", "test", "lint", "configure", "build", "debug"];

/// Project-icon emoji choices — `PROJECT_EMOJIS` in
/// apps/web/src/projectIconOptions.ts (same order).
const PROJECT_EMOJIS: &[&str] = &[
    "💻", "🛠️", "🚀", "🤖", "✨", "⚡", "🌐", "📱", "🖥️", "⌨️", "⚙️", "🗄️", "☁️", "📦", "📚", "🧪",
    "🔒", "🎮", "🎵", "🎬", "🖼️", "🛍️", "🔥", "💡", "🧩", "📊", "🧠", "🦄", "🐙", "🌱",
];

/// Packed-value sentinel for "no explicit selection" rows.
pub(crate) const AUTO_VALUE: &str = "\u{1f}auto";
/// Model option values pack `instanceId` + `model` with a unit separator.
pub(crate) const PACK_SEP: char = '\u{1f}';

pub(crate) fn pack_model(instance: &str, model: &str) -> String {
    format!("{instance}{PACK_SEP}{model}")
}

/// Which inline option list is expanded (at most one at a time).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenList {
    NotificationMode,
    /// Global/Project scope selector rendered above the scoped sections.
    SettingsScope,
    DefaultModel,
    FallbackModel,
    RuntimeMode,
    EnvMode,
    // settings_extra.rs sections (Threads / Source Control):
    DiffLayout,
    DiffFiles,
    WorktreeSubmodules,
    AutoSettleDays,
    TextGenModel,
    ResponseStreaming,
    ScWritingMode,
    ScWriterModel,
}

impl State {
    /// Seed the device-local notification prefs from the persisted client
    /// config (called once from `AppView::new`).
    pub(crate) fn seed_from_config(&mut self, cfg: &crate::config::ClientConfig) {
        self.notification_mode = cfg.notification_mode.clone();
        self.in_app_notifications = cfg.in_app_notifications;
        self.scope_project = cfg.settings_scope_project.clone();
    }
}

impl OpenList {
    /// Element-id prefix + trigger id for this list.
    pub(crate) fn prefix(self) -> &'static str {
        match self {
            OpenList::NotificationMode => "misc-notif-mode",
            OpenList::SettingsScope => "misc-settings-scope",
            OpenList::DefaultModel => "misc-default-model",
            OpenList::FallbackModel => "misc-fallback-model",
            OpenList::RuntimeMode => "misc-runtime-mode",
            OpenList::EnvMode => "misc-env-mode",
            OpenList::DiffLayout => "extra-diff-layout",
            OpenList::DiffFiles => "extra-diff-files",
            OpenList::WorktreeSubmodules => "extra-wt-submodules",
            OpenList::AutoSettleDays => "extra-autosettle-days",
            OpenList::TextGenModel => "extra-textgen-model",
            OpenList::ResponseStreaming => "extra-resp-streaming",
            OpenList::ScWritingMode => "extra-sc-writing-mode",
            OpenList::ScWriterModel => "extra-sc-writer-model",
        }
    }
}

#[derive(Default)]
pub struct State {
    /// ServerSettings mirror (`server.getConfig → settings`; replaced with
    /// the merged object `server.updateSettings` returns). `pub(crate)` so
    /// the settings_extra sections can read the same mirror.
    pub(crate) settings: Value,
    /// `Cell` flags because the lazy-load kick runs from `&self` render.
    pub(crate) settings_loaded: Cell<bool>,
    pub(crate) settings_loading: Cell<bool>,
    /// Last settings load/write failure, shown as a status line.
    pub(crate) settings_error: Option<String>,
    /// Expanded option list, if any.
    open_list: Option<OpenList>,
    /// Settings scope: `Some(project_id)` makes scoped rows write
    /// `settings.projectSettingsOverrides[project_id]` instead of the global
    /// setting. Mirrors the web's Global/Project scope selector
    /// (`PROJECT_SCOPED_SERVER_SETTING_KEYS` decides which rows scope).
    /// Persisted on `ClientConfig.settings_scope_project`.
    pub(crate) scope_project: Option<String>,
    /// `server.getConfig → environment.serverVersion` (About & updates row).
    pub(crate) server_version: Option<String>,
    /// Wave-3 sections (Keybindings / Source Control / Threads) — the
    /// AppView field list lives in lib.rs, so extra-section state hangs off
    /// this struct instead.
    pub(crate) extra: crate::settings_extra::ExtraState,

    // ── Project scripts (`defaultProjectScripts`, scoped) ───────────────
    /// Add/edit form visible in the Projects section.
    script_form_open: bool,
    /// Script id being edited; `None` = the form adds a new script.
    script_editing: Option<String>,
    script_name_draft: String,
    script_command_draft: String,
    /// `ProjectScript.icon` literal for the form.
    script_icon: String,
    script_on_create: bool,
    script_on_settle: bool,
    script_name_field: Option<Entity<TextField>>,
    script_command_field: Option<Entity<TextField>>,
    script_subs: Vec<Subscription>,

    // ── Notification prefs (client/device-local in the web) ─────────
    // Persisted via `ClientConfig.notification_mode` / `.in_app_notifications`
    // (seeded on startup; written back on change).
    notification_mode: String,
    in_app_notifications: bool,
    // `in_app_notifications` gates the "another thread finished" toasts;
    // `notification_mode` drives `os_notify` (notify-send + sound, best-effort)
    // for settle/approval alerts in notifications.rs.

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

    // ── Folder browser (filesystem.browse) ──────────────────────────────
    /// The browse panel inside the add-project form is open.
    browse_open: bool,
    /// Directory whose children `browse_entries` lists (absolute, no
    /// trailing slash except the filesystem root).
    browse_dir: String,
    /// `(name, fullPath)` directory entries from `filesystem.browse`.
    browse_entries: Vec<(String, String)>,
    /// `Cell` flags because the fetch kick runs from `&self` render.
    browse_loading: Cell<bool>,
    /// `browse_dir` changed since the last fetch — re-browse on next render.
    browse_dirty: Cell<bool>,
    browse_error: Option<String>,

    // ── Project icon picker ─────────────────────────────────────────────
    /// Project id whose emoji-picker row is open (`project.update`
    /// `{projectIcon:{kind:"emoji",…}}` — the wire accepts it; ShellProject
    /// doesn't carry the icon back, so there is no current-icon display).
    icon_picker: Option<String>,
}

// ─── small render helpers (mirroring settings.rs section style) ───────────

/// Small caption above a group of rows.
pub(crate) fn group_label(text: &'static str) -> gpui::Div {
    div()
        .pb_1()
        .pt_3()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(text)
}

/// Destructive accent used across the app (palette/sidebar danger).
pub(crate) fn danger() -> gpui::Rgba {
    rgb(0xf87171)
}

/// A settings row: title + muted description on the left, control on the
/// right — the web `SettingsRow` (`px-3 py-3`, `text-sm font-medium` title,
/// `text-xs text-muted-foreground` description).
pub(crate) fn setting_row(
    title: &'static str,
    description: &'static str,
    control: impl IntoElement,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_4()
        .px_3()
        .py_3()
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
                        .font_weight(gpui::FontWeight::MEDIUM)
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

/// A simple switch (web `Switch`): track w-30 h-18 p-2px, thumb 14px.
/// Checked → `bg-primary` + `bg-background` thumb; unchecked → `bg-input`
/// track + `bg-muted-foreground` thumb (dark) / `bg-background` (light).
pub(crate) fn switch_control(on: bool) -> gpui::Div {
    div()
        .w(px(30.))
        .h(px(18.))
        .flex_none()
        .rounded_full()
        .px(px(2.))
        .bg(if on { theme::primary() } else { theme::input() })
        .flex()
        .items_center()
        .when(on, |d| d.justify_end())
        .child(div().w(px(14.)).h(px(14.)).rounded_full().bg(if on {
            theme::switch_thumb_on()
        } else {
            theme::switch_thumb_off()
        }))
}

/// A readable message out of a Failure `Exit` frame's cause (same shape
/// `exit_failure_message` in archived.rs digs through).
pub(crate) fn exit_failure_message(exit: &Value) -> String {
    let cause = &exit["cause"];
    for candidate in [
        &cause["message"],
        &cause["error"]["message"],
        &cause["error"]["detail"],
        &cause["detail"],
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
pub(crate) fn field_text(field: &Entity<TextField>, cx: &mut Context<AppView>) -> String {
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
pub(crate) fn field_text_in(
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
pub(crate) fn edit_field<R>(
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
    pub(crate) fn maybe_load_misc_settings(&self, cx: &mut Context<Self>) {
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
                                state.server_version =
                                    exit["value"]["environment"]["serverVersion"]
                                        .as_str()
                                        .or_else(|| exit["value"]["version"].as_str())
                                        .map(str::to_string);
                                state.settings_error = None;
                                this.apply_server_defaults();
                                this.seed_extra_fields(cx);
                            }
                            Ok(exit) => {
                                state.settings_error = Some(format!(
                                    "Settings failed to load: {}",
                                    exit_failure_message(&exit)
                                ));
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

    /// Push server-side defaults into the composer for fields the user
    /// hasn't explicitly picked: `defaultModelSelection` → provider/model,
    /// `defaultRuntimeMode` → permission mode. Matches the web composer,
    /// where these settings seed the pickers for new threads.
    fn apply_server_defaults(&mut self) {
        let state = &self.misc_settings_state;
        if !self.composer_state.model_picked
            && let Value::Object(sel) = &state.settings["defaultModelSelection"]
        {
            let instance = sel["instanceId"]
                .as_str()
                .or_else(|| sel["provider"].as_str());
            let model = sel["model"].as_str();
            if let (Some(instance), Some(model)) = (instance, model)
                && !instance.is_empty()
                && !model.is_empty()
            {
                self.provider = instance.to_string();
                self.model = model.to_string();
            }
        }
        if !self.composer_state.runtime_mode_picked
            && let Some(mode) = state.settings["defaultRuntimeMode"].as_str()
            && !mode.is_empty()
        {
            self.composer_state.runtime_mode = mode.to_string();
        }
    }

    /// `server.updateSettings {patch}` — optimistic top-level merge into the
    /// mirror; the reply carries the full merged object and replaces it. A
    /// failure marks the mirror stale so the next render refetches.
    pub(crate) fn update_server_settings(&mut self, patch: Value, cx: &mut Context<Self>) {
        let Some(rx) = self.send_query("server.updateSettings", json!({ "patch": patch })) else {
            self.misc_settings_state.settings_error = Some("Not connected to a server.".into());
            cx.notify();
            return;
        };
        if let Value::Object(settings) = &mut self.misc_settings_state.settings
            && let Value::Object(patch) = &patch
        {
            for (key, value) in patch {
                // Per-project override entries merge by project id so one
                // scoped write can't drop a sibling project's overrides.
                if key == "projectSettingsOverrides"
                    && let Value::Object(src) = value
                {
                    let dst = settings.entry(key.clone()).or_insert_with(|| json!({}));
                    if let Value::Object(dst) = dst {
                        for (pid, entry) in src {
                            dst.insert(pid.clone(), entry.clone());
                        }
                    }
                    continue;
                }
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
                        this.apply_server_defaults();
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

    // ─── Settings scope (Global / Project overrides) ──────────────────────

    /// Scoped override keys where `null` is a stored value (not "inherit") —
    /// `NULLABLE_PROJECT_SETTINGS_OVERRIDES` in packages/contracts/settings.ts.
    const NULLABLE_OVERRIDE_KEYS: &[&str] = &[
        "defaultModelSelection",
        "sourceControlWriterModelSelection",
        "reviewGuideModelSelection",
        "pullRequestMergeMethod",
        "sidebarAutoSettleAfterDays",
    ];

    /// The project the scope selector points at, when it still exists.
    pub(crate) fn scope_project_id(&self) -> Option<&str> {
        let id = self.misc_settings_state.scope_project.as_deref()?;
        self.state
            .projects
            .iter()
            .any(|p| p.id.as_ref() == id)
            .then_some(id)
    }

    /// The project's `projectSettingsOverrides` entry (empty object when none).
    fn scope_override_entry(&self, project_id: &str) -> serde_json::Map<String, Value> {
        self.misc_settings_state.settings["projectSettingsOverrides"][project_id]
            .as_object()
            .cloned()
            .unwrap_or_default()
    }

    /// Effective value of a scoped setting: the project override when one is
    /// set (including an explicit `null`), else the global value.
    pub(crate) fn scoped_value(&self, key: &str) -> Value {
        if let Some(pid) = self.scope_project_id()
            && let Some(value) = self.scope_override_entry(pid).get(key)
        {
            return value.clone();
        }
        self.misc_settings_state.settings[key].clone()
    }

    /// Whether the scoped project explicitly overrides `key`.
    pub(crate) fn scope_has_override(&self, key: &str) -> bool {
        self.scope_project_id()
            .map(|pid| self.scope_override_entry(pid).contains_key(key))
            .unwrap_or(false)
    }

    /// Write `key = value` in the active scope. Global scope sends a plain
    /// settings patch; project scope merges into
    /// `projectSettingsOverrides[project_id]` like the web's
    /// `planScopedSettingsPatch` — `null` for a key that cannot store null
    /// removes the override (inherit) instead.
    pub(crate) fn update_scoped_setting(
        &mut self,
        key: &str,
        value: Value,
        cx: &mut Context<Self>,
    ) {
        let Some(pid) = self.scope_project_id().map(str::to_string) else {
            self.update_server_settings(json!({ key: value }), cx);
            return;
        };
        let mut entry = self.scope_override_entry(&pid);
        if value.is_null() && !Self::NULLABLE_OVERRIDE_KEYS.contains(&key) {
            entry.remove(key);
        } else {
            entry.insert(key.to_string(), value);
        }
        self.update_server_settings(
            json!({ "projectSettingsOverrides": { pid: Value::Object(entry) } }),
            cx,
        );
    }

    /// Drop `key` from the project's override entry — inherit the global
    /// value again (web `clearProjectSettingsOverrides` per key).
    pub(crate) fn clear_scoped_override(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(pid) = self.scope_project_id().map(str::to_string) else {
            return;
        };
        let mut entry = self.scope_override_entry(&pid);
        entry.remove(key);
        self.update_server_settings(
            json!({ "projectSettingsOverrides": { pid: Value::Object(entry) } }),
            cx,
        );
    }

    /// "override"/"inherited" chip shown next to a scoped row's control in
    /// project scope. Clicking `override` clears the override back to
    /// inherited; clicking `inherited` pins the effective value as an
    /// explicit override.
    pub(crate) fn scope_chip(
        &self,
        key: &'static str,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        self.scope_project_id()?;
        let overridden = self.scope_has_override(key);
        Some(
            div()
                .id(format!("scope-chip-{key}"))
                .px_1p5()
                .py_0p5()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .when(overridden, |s| {
                    s.bg(theme::selected()).text_color(theme::primary())
                })
                .when(!overridden, |s| {
                    s.bg(theme::hover())
                        .text_color(theme::fg_muted())
                        .hover(|s| s.bg(theme::selected()))
                })
                .child(if overridden { "override" } else { "inherited" })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        if this.scope_has_override(key) {
                            this.clear_scoped_override(key, cx);
                        } else {
                            let value = this.scoped_value(key);
                            this.update_scoped_setting(key, value, cx);
                        }
                    }),
                ),
        )
    }

    /// The Global/Project scope selector rendered above sections whose rows
    /// support `projectSettingsOverrides`.
    pub(crate) fn render_scope_selector(&self, cx: &mut Context<Self>) -> gpui::Div {
        let pid = self.scope_project_id().map(str::to_string);
        let label: SharedString = match &pid {
            Some(id) => {
                let name = self
                    .state
                    .projects
                    .iter()
                    .find(|p| p.id.as_ref() == id.as_str())
                    .map(|p| p.name.to_string())
                    .unwrap_or_else(|| id.clone());
                format!("Project: {name}").into()
            }
            None => "Global".into(),
        };
        let current = pid.clone().unwrap_or_default();
        let mut options = vec![(
            String::new(),
            "Global".to_string(),
            Some("Server-wide defaults every project inherits.".to_string()),
        )];
        for project in &self.state.projects {
            options.push((
                project.id.to_string(),
                project.name.to_string(),
                Some("Override the marked rows for this project.".to_string()),
            ));
        }
        self.select_setting(
            "Apply settings to",
            "Scoped rows show an “override”/“inherited” chip in project scope.",
            OpenList::SettingsScope,
            label,
            &current,
            options,
            cx,
        )
    }

    /// A select row with the scope chip beside the trigger (for
    /// `PROJECT_SCOPED_SERVER_SETTING_KEYS` rows); option lists render the
    /// same as `select_setting`.
    pub(crate) fn scoped_select_setting(
        &self,
        title: &'static str,
        description: &'static str,
        list: OpenList,
        scoped_key: &'static str,
        current: SharedString,
        current_value: &str,
        options: Vec<(String, String, Option<String>)>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let open = self.misc_settings_state.open_list == Some(list);
        let mut control = div().flex().items_center().gap_2();
        if let Some(chip) = self.scope_chip(scoped_key, cx) {
            control = control.child(chip);
        }
        control = control.child(self.select_trigger(list, current, cx));
        let mut out = div()
            .flex()
            .flex_col()
            .child(setting_row(title, description, control));
        if open {
            out = out.child(self.select_options_list(list, current_value, &options, cx));
        }
        out
    }

    /// A boolean scoped row (switch + scope chip) — the same write path as
    /// the select rows.
    pub(crate) fn scoped_switch_row(
        &self,
        title: &'static str,
        description: &'static str,
        scoped_key: &'static str,
        element_id: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let on = self.scoped_value(scoped_key).as_bool() == Some(true);
        let mut control = div().flex().items_center().gap_2();
        if let Some(chip) = self.scope_chip(scoped_key, cx) {
            control = control.child(chip);
        }
        control = control.child(
            div()
                .id(element_id)
                .cursor_pointer()
                .child(switch_control(on))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        this.update_scoped_setting(scoped_key, json!(!on), cx);
                    }),
                ),
        );
        setting_row(title, description, control)
    }

    /// Fold an expanded select list's pick into state / server settings.
    pub(crate) fn misc_pick(&mut self, list: OpenList, value: String, cx: &mut Context<Self>) {
        self.misc_settings_state.open_list = None;
        match list {
            OpenList::NotificationMode => {
                self.misc_settings_state.notification_mode = value.clone();
                self.client_config.notification_mode = value;
                self.client_config.save();
            }
            OpenList::SettingsScope => {
                self.misc_settings_state.scope_project =
                    if value.is_empty() { None } else { Some(value) };
                self.client_config.settings_scope_project =
                    self.misc_settings_state.scope_project.clone();
                self.client_config.save();
            }
            OpenList::DefaultModel => {
                let selection = if value == AUTO_VALUE {
                    Value::Null
                } else {
                    let (instance, model) = value.split_once(PACK_SEP).unwrap_or(("", ""));
                    json!({ "instanceId": instance, "model": model })
                };
                self.update_scoped_setting("defaultModelSelection", selection, cx);
            }
            OpenList::FallbackModel => {
                // Not a project-scoped key — always writes the global setting.
                let patch = if value == AUTO_VALUE {
                    json!({ "fallbackModelSelection": null })
                } else {
                    let (instance, model) = value.split_once(PACK_SEP).unwrap_or(("", ""));
                    json!({ "fallbackModelSelection": { "instanceId": instance, "model": model } })
                };
                self.update_server_settings(patch, cx);
            }
            OpenList::RuntimeMode => {
                self.update_scoped_setting("defaultRuntimeMode", Value::String(value), cx);
            }
            OpenList::EnvMode => {
                let mode = if value == "inherit" {
                    Value::Null
                } else {
                    Value::String(value)
                };
                self.update_scoped_setting("defaultThreadEnvMode", mode, cx);
            }
            // ── settings_extra lists (Threads / Source Control) ────────────
            OpenList::DiffLayout => {
                self.client_config.diff_layout = value;
                self.client_config.save();
            }
            OpenList::DiffFiles => {
                self.client_config.diff_files_collapsed = value == "collapsed";
                self.client_config.save();
            }
            OpenList::WorktreeSubmodules => {
                let submodules = if value == "inherit" {
                    Value::Null
                } else {
                    Value::String(value)
                };
                self.update_scoped_setting("worktreeSubmodules", submodules, cx);
            }
            OpenList::AutoSettleDays => {
                let days = if value == "off" {
                    Value::Null
                } else {
                    value.parse::<i64>().map(Value::from).unwrap_or(Value::Null)
                };
                self.update_scoped_setting("sidebarAutoSettleAfterDays", days, cx);
            }
            OpenList::TextGenModel => {
                let selection = if value == AUTO_VALUE {
                    Value::Null
                } else {
                    let (instance, model) = value.split_once(PACK_SEP).unwrap_or(("", ""));
                    json!({ "instanceId": instance, "model": model })
                };
                self.update_scoped_setting("textGenerationModelSelection", selection, cx);
            }
            OpenList::ResponseStreaming => {
                self.update_scoped_setting("responseStreamingMode", Value::String(value), cx);
            }
            OpenList::ScWritingMode => {
                // `sourceControlWritingStyle` is one object server-side; send
                // the merged value so sibling fields (customInstructions,
                // followChangeRequestTemplates) are not lost.
                let mut style = self
                    .scoped_value("sourceControlWritingStyle")
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                style.insert("mode".into(), json!(value));
                self.update_scoped_setting("sourceControlWritingStyle", Value::Object(style), cx);
            }
            OpenList::ScWriterModel => {
                let selection = if value == AUTO_VALUE {
                    Value::Null
                } else {
                    let (instance, model) = value.split_once(PACK_SEP).unwrap_or(("", ""));
                    json!({ "instanceId": instance, "model": model })
                };
                self.update_scoped_setting("sourceControlWriterModelSelection", selection, cx);
            }
        }
        cx.notify();
    }

    /// Value-label pair for a stored `ModelSelection` (`{instanceId, model}`,
    /// tolerating the legacy `provider` key).
    pub(crate) fn selection_label(selection: &Value) -> String {
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

    /// The trigger control of a select row — the web `SelectTrigger`
    /// default variant: `border-input` + `bg-input/32` surface, `rounded-lg`,
    /// `min-h-8 px-3`, current value left + a muted ▾ chevron right. Click
    /// toggles the option list open/closed.
    pub(crate) fn select_trigger(
        &self,
        list: OpenList,
        current: SharedString,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(list.prefix())
            .min_w(px(160.))
            .h(px(32.))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_3()
            .rounded_lg()
            .border_1()
            .border_color(theme::input())
            .bg(theme::field_surface())
            .text_sm()
            .cursor_pointer()
            .text_color(theme::fg())
            .hover(|s| s.bg(theme::accent()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::fg())
                    .child(current),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme::icon_muted())
                    .child("▾"),
            )
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
    pub(crate) fn select_setting(
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
            out = out.child(self.select_options_list(list, current_value, &options, cx));
        }
        out
    }

    /// The option list under an open select — the web `SelectContent`
    /// popover: `rounded-lg border` popover surface, `p-1`, items
    /// `min-h-7 rounded-sm px-2 py-1 text-sm`, selected `bg-foreground/[0.08]`,
    /// hovered `bg-accent`.
    fn select_options_list(
        &self,
        list: OpenList,
        current_value: &str,
        options: &[(String, String, Option<String>)],
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut list_div = div()
            .flex()
            .flex_col()
            .mt_1()
            .mx_3()
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .shadow_lg();
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
                    .py_1()
                    .min_h(px(28.))
                    .rounded_sm()
                    .text_sm()
                    .cursor_pointer()
                    .text_color(theme::fg())
                    .when(selected, |s| s.bg(theme::hover()))
                    .when(!selected, |s| s.hover(|s| s.bg(theme::accent())))
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
        list_div
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

        if connected {
            section = section.child(self.render_scope_selector(cx));
        }

        // ── Default model (defaultModelSelection, scoped) ────────────────
        // `apply_server_defaults` pushes the global value into the composer
        // whenever the user hasn't explicitly picked a model — the chip then
        // shows the server default for new threads.
        let default_selection = self.scoped_value("defaultModelSelection");
        let default_selection = &default_selection;
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
        section = section.child(self.scoped_select_setting(
            "Model",
            "Default model for new threads. Projects can override it.",
            OpenList::DefaultModel,
            "defaultModelSelection",
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

        // ── Default permissions (defaultRuntimeMode, scoped) ─────────────
        let runtime_mode = self
            .scoped_value("defaultRuntimeMode")
            .as_str()
            .unwrap_or("full-access")
            .to_string();
        let runtime_label = RUNTIME_MODES
            .iter()
            .find(|(value, _, _)| *value == runtime_mode.as_str())
            .map(|(_, label, _)| (*label).to_string())
            .unwrap_or_else(|| runtime_mode.clone());
        section = section.child(
            self.scoped_select_setting(
                "Default permissions",
                "Default permissions for new threads. Projects can override them.",
                OpenList::RuntimeMode,
                "defaultRuntimeMode",
                runtime_label.into(),
                &runtime_mode,
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
            ),
        );

        // ── Workspace (defaultThreadEnvMode; null inherits, scoped) ──────
        let env_mode = self
            .scoped_value("defaultThreadEnvMode")
            .as_str()
            .map(str::to_string);
        let env_mode = env_mode.as_deref();
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
        section = section.child(self.scoped_select_setting(
            "Workspace",
            "Where new threads start. Projects and their t3.json can override it.",
            OpenList::EnvMode,
            "defaultThreadEnvMode",
            env_label.into(),
            env_value,
            env_options,
            cx,
        ));

        // ── Automatically pull (defaultAutoPull, scoped) ─────────────────
        section = section.child(self.scoped_switch_row(
            "Automatically pull",
            "Keeps the default branch current when the checkout has no local changes or commits. Projects can override it.",
            "defaultAutoPull",
            "misc-autopull-switch",
            cx,
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

        self.misc_settings_state
            .subs
            .push(cx.observe(&name_field, |this, field, cx| {
                this.misc_settings_state.draft_name = field_text(&field, cx);
                cx.notify();
            }));
        self.misc_settings_state
            .subs
            .push(cx.observe(&path_field, |this, field, cx| {
                this.misc_settings_state.draft_path = field_text(&field, cx);
                cx.notify();
            }));
        self.misc_settings_state
            .subs
            .push(cx.observe(&rename_field, |this, field, cx| {
                this.misc_settings_state.draft_rename = field_text(&field, cx);
                cx.notify();
            }));

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

    /// Lazily create the script-form fields + observers (same deferred-open
    /// pattern as `ensure_project_fields`).
    fn ensure_script_fields(&mut self, cx: &mut Context<Self>) {
        if self.misc_settings_state.script_name_field.is_some() {
            return;
        }
        let name_field = cx.new(|cx| TextField::new("Action name (e.g. Dev server)", cx));
        let command_field = cx.new(|cx| TextField::new("Command (e.g. vp run dev)", cx));
        self.misc_settings_state
            .script_subs
            .push(cx.observe(&name_field, |this, field, cx| {
                this.misc_settings_state.script_name_draft = field_text(&field, cx);
                cx.notify();
            }));
        self.misc_settings_state
            .script_subs
            .push(cx.observe(&command_field, |this, field, cx| {
                this.misc_settings_state.script_command_draft = field_text(&field, cx);
                cx.notify();
            }));
        for field in [name_field.clone(), command_field.clone()] {
            self.misc_settings_state.script_subs.push(cx.subscribe(
                &field,
                |this, _field, _event: &PromptSubmitted, cx| {
                    this.save_script(cx);
                },
            ));
        }
        let state = &mut self.misc_settings_state;
        state.script_name_field = Some(name_field);
        state.script_command_field = Some(command_field);
    }

    // ─── Project scripts (`defaultProjectScripts`, scoped key) ──────────

    /// Scripts effective for the active scope — project override when set,
    /// else the global `defaultProjectScripts` array. (The web additionally
    /// merges `project.scripts` and t3.json tiers; the native mirror only
    /// sees ServerSettings.)
    pub(crate) fn effective_scripts(&self) -> Vec<Value> {
        self.scoped_value("defaultProjectScripts")
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// Persist the whole scripts array in the active scope.
    fn write_scripts(&mut self, scripts: Vec<Value>, cx: &mut Context<Self>) {
        self.update_scoped_setting("defaultProjectScripts", Value::Array(scripts), cx);
    }

    /// Slug a script name into a `script.<id>.run`-compatible id (the web's
    /// `normalizeScriptId` + `nextProjectScriptId`, max 24 chars).
    fn next_script_id(name: &str, existing: &[Value]) -> String {
        let mut base = String::new();
        let mut last_dash = true; // strips a leading dash
        for ch in name.trim().to_lowercase().chars() {
            if ch.is_ascii_alphanumeric() {
                base.push(ch);
                last_dash = false;
            } else if !last_dash {
                base.push('-');
                last_dash = true;
            }
        }
        let base = base.trim_end_matches('-');
        let base = if base.is_empty() { "script" } else { base };
        let base: String = base.chars().take(24).collect();
        let base = base.trim_end_matches('-');
        let base = if base.is_empty() { "script" } else { base };
        let taken = |id: &str| existing.iter().any(|s| s["id"].as_str() == Some(id));
        if !taken(base) {
            return base.to_string();
        }
        for suffix in 2..10_000u32 {
            let suffix_str = suffix.to_string();
            let keep = 24usize.saturating_sub(suffix_str.len() + 1).max(1);
            let candidate = format!("{}-{suffix}", base.chars().take(keep).collect::<String>());
            if !taken(&candidate) {
                return candidate;
            }
        }
        format!(
            "{}-{}",
            base.chars().take(10).collect::<String>(),
            Uuid::new_v4()
        )
        .chars()
        .take(24)
        .collect()
    }

    /// "+ Add action" / "Edit" opener — seed the form (empty for add, the
    /// script's values for edit) and focus the name field.
    fn open_script_form(
        &mut self,
        script: Option<Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_script_fields(cx);
        let state = &mut self.misc_settings_state;
        state.script_form_open = true;
        state.script_editing = script
            .as_ref()
            .and_then(|s| s["id"].as_str())
            .map(str::to_string);
        state.script_name_draft = script
            .as_ref()
            .and_then(|s| s["name"].as_str())
            .unwrap_or_default()
            .to_string();
        state.script_command_draft = script
            .as_ref()
            .and_then(|s| s["command"].as_str())
            .unwrap_or_default()
            .to_string();
        state.script_icon = script
            .as_ref()
            .and_then(|s| s["icon"].as_str())
            .unwrap_or("play")
            .to_string();
        state.script_on_create = script
            .as_ref()
            .and_then(|s| s["runOnWorktreeCreate"].as_bool())
            .unwrap_or(false);
        state.script_on_settle = script
            .as_ref()
            .and_then(|s| s["runOnSettle"].as_bool())
            .unwrap_or(false);
        for (field, text) in [
            (
                state.script_name_field.clone(),
                state.script_name_draft.clone(),
            ),
            (
                state.script_command_field.clone(),
                state.script_command_draft.clone(),
            ),
        ]
        .into_iter()
        {
            if let Some(field) = field {
                field.update(cx, |field, cx| {
                    field.reset(window, cx);
                    field.replace_text_in_range(Some(0..0), &text, window, cx);
                });
            }
        }
        if let Some(field) = self.misc_settings_state.script_name_field.clone() {
            field.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// Build a `ProjectScript` from the form and write the array (insert or
    /// replace on `script_editing`). `projectScriptOverrides`-style writes
    /// keep sibling keys because `update_scoped_setting` merges the entry.
    fn save_script(&mut self, cx: &mut Context<Self>) {
        let state = &self.misc_settings_state;
        let name = state.script_name_draft.trim().to_string();
        let command = state.script_command_draft.trim().to_string();
        if name.is_empty() || command.is_empty() {
            return;
        }
        let mut scripts = self.effective_scripts();
        let icon = state.script_icon.clone();
        let icon = if PROJECT_SCRIPT_ICONS.contains(&icon.as_str()) {
            icon
        } else {
            "play".to_string()
        };
        // Start from the existing script when editing so optional contract
        // fields the form doesn't cover (stopOnSettle, async, previewUrl,
        // autoOpenPreview) round-trip untouched.
        let mut script = state
            .script_editing
            .as_ref()
            .and_then(|editing| {
                scripts
                    .iter()
                    .find(|s| s["id"].as_str() == Some(editing.as_str()))
                    .cloned()
            })
            .unwrap_or_else(|| json!({}));
        script["name"] = json!(name);
        script["command"] = json!(command);
        script["icon"] = json!(icon);
        script["runOnWorktreeCreate"] = json!(state.script_on_create);
        if state.script_on_settle {
            script["runOnSettle"] = json!(true);
        } else {
            script.as_object_mut().map(|o| o.remove("runOnSettle"));
        }
        if let Some(editing) = state.script_editing.clone() {
            script["id"] = json!(editing);
            let replaced = scripts
                .iter()
                .position(|s| s["id"].as_str() == Some(editing.as_str()));
            match replaced {
                Some(index) => scripts[index] = script,
                None => scripts.push(script),
            }
        } else {
            script["id"] = json!(Self::next_script_id(&name, &scripts));
            scripts.push(script);
        }
        self.misc_settings_state.script_form_open = false;
        self.misc_settings_state.script_editing = None;
        self.write_scripts(scripts, cx);
    }

    /// Read the form fields inside a click listener (has `window`), then save.
    fn save_script_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (field, slot) in [
            self.misc_settings_state
                .script_name_field
                .clone()
                .map(|f| (f, "name")),
            self.misc_settings_state
                .script_command_field
                .clone()
                .map(|f| (f, "command")),
        ]
        .into_iter()
        .flatten()
        {
            let text = field_text_in(&field, window, cx);
            match slot {
                "name" => self.misc_settings_state.script_name_draft = text,
                _ => self.misc_settings_state.script_command_draft = text,
            }
        }
        self.save_script(cx);
    }

    /// Remove a script by id in the active scope.
    fn remove_script(&mut self, script_id: String, cx: &mut Context<Self>) {
        let mut scripts = self.effective_scripts();
        scripts.retain(|s| s["id"].as_str() != Some(script_id.as_str()));
        self.write_scripts(scripts, cx);
    }

    /// Run a script in the selected thread's terminal: `terminal.open` a
    /// dedicated PTY, then `terminal.write` the command, and open the drawer.
    /// Drawer sessions are owned by terminal_drawer.rs, so we open the
    /// session server-side; the drawer's `subscribeTerminalMetadata` stream
    /// picks the new terminal up and attaches on the next render.
    fn run_project_script(
        &mut self,
        name: String,
        command: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scope_pid = self.scope_project_id().map(str::to_string);
        // Prefer the selected thread (when it lives in the scoped project),
        // else any thread of the scoped project, else the selected thread.
        let selected = self.state.selected_thread();
        let thread = selected
            .filter(|t| {
                scope_pid
                    .as_deref()
                    .map(|pid| t.project_id.as_ref() == pid)
                    .unwrap_or(true)
            })
            .or_else(|| {
                scope_pid.as_deref().and_then(|pid| {
                    self.state
                        .threads
                        .iter()
                        .find(|t| t.project_id.as_ref() == pid)
                })
            })
            .or(selected);
        let Some(thread) = thread.cloned() else {
            self.push_toast(
                ToastKind::Info,
                "No thread to run the action in",
                Some("Open a thread in the project first.".into()),
                None,
                cx,
            );
            return;
        };
        let thread_id = thread.id.to_string();
        let terminal_id = format!("script-{}", &Uuid::new_v4().to_string()[..8]);
        // Same payload shape as the drawer's open (terminal_drawer.rs):
        // `cwd` only when the project path is known.
        let cwd = self
            .state
            .projects
            .iter()
            .find(|p| p.id.as_ref() == thread.project_id.as_ref())
            .map(|p| p.path.to_string())
            .filter(|path| !path.is_empty());
        let mut open_payload = json!({
            "threadId": thread_id,
            "terminalId": terminal_id,
            "cols": 120,
            "rows": 30,
        });
        if let Some(cwd) = &cwd {
            open_payload["cwd"] = json!(cwd);
        }
        let Some(rx) = self.send_query("terminal.open", open_payload) else {
            self.push_toast(
                ToastKind::Error,
                "Could not run action",
                Some("Not connected to a server.".into()),
                None,
                cx,
            );
            return;
        };
        let run_name = name.clone();
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        let _ = this.send_query(
                            "terminal.write",
                            json!({
                                "threadId": thread_id,
                                "terminalId": terminal_id,
                                "data": format!("{command}\r"),
                            }),
                        );
                        if this.state.selected.as_deref() != Some(thread_id.as_str()) {
                            this.select_thread(thread_id.clone().into(), cx);
                        }
                        // No `window` inside `cx.spawn`'s update — opening
                        // only flips the flag (focus hand-off only happens on
                        // close, so `terminal_drawer_set_open` isn't needed).
                        this.terminal_state.drawer_open = true;
                        this.push_toast(
                            ToastKind::Success,
                            format!("Running “{run_name}”"),
                            Some("Opened a terminal for the action.".into()),
                            None,
                            cx,
                        );
                    }
                    Ok(exit) => {
                        this.push_toast(
                            ToastKind::Error,
                            "Could not run action",
                            Some(exit_failure_message(&exit)),
                            None,
                            cx,
                        );
                    }
                    Err(_) => {
                        this.push_toast(
                            ToastKind::Error,
                            "Could not run action",
                            Some("Lost connection before the reply arrived.".into()),
                            None,
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
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
        state.browse_open = false;
        state.browse_entries.clear();
        state.browse_error = None;
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
                        state.browse_open = false;
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
                            edit_field(&field, cx, |field, window, cx| field.reset(window, cx));
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
    // A navigable folder picker is a noted gap — `filesystem.browse` exists
    // server-side; the path TextField (with ~ expansion) is the stand-in.
    fn add_project_folder(&mut self, path: String, cx: &mut Context<Self>) {
        let path = expand_home(&path);
        if path.is_empty() || self.misc_settings_state.creating {
            return;
        }
        // Reject duplicates — a second project on the same root is a UI bug
        // the server won't catch.
        if self.state.projects.iter().any(|p| p.path.as_ref() == path) {
            self.misc_settings_state.projects_error =
                Some("That folder is already a project.".to_string());
            cx.notify();
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&path) {
            self.misc_settings_state.projects_error = Some(format!("Could not create folder: {e}"));
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

    // ─── Folder browser (`filesystem.browse`) ────────────────────────────

    /// Open the directory browser inside the add-project form, rooted at
    /// the typed path's directory (else the server setting
    /// `addProjectBaseDirectory`, else `~`). Drives `filesystem.browse`
    /// `{partialPath: "<dir>/"}` which lists subdirectories of `<dir>`.
    fn open_browse(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.misc_settings_state;
        state.browse_open = true;
        state.browse_error = None;
        let seed = if state.draft_path.trim().is_empty() {
            let base = state.settings["addProjectBaseDirectory"]
                .as_str()
                .unwrap_or("");
            if base.trim().is_empty() {
                "~".to_string()
            } else {
                base.to_string()
            }
        } else {
            state.draft_path.clone()
        };
        // The seed may be a not-yet-existing leaf — start at its parent so
        // the browser lands somewhere real and the leaf stays one click away.
        let expanded = expand_home(&seed);
        let path = std::path::Path::new(&expanded);
        state.browse_dir = if path.is_dir() {
            expanded.trim_end_matches('/').to_string()
        } else {
            path.parent()
                .map(|p| p.to_string_lossy().into_owned())
                .filter(|p| !p.is_empty())
                .unwrap_or(expanded)
        };
        if state.browse_dir.is_empty() {
            state.browse_dir = "/".to_string();
        }
        state.browse_dirty.set(true);
        cx.notify();
    }

    /// Kick the `filesystem.browse` fetch when the browser is open and the
    /// current directory changed since the last fetch (render is `&self`,
    /// hence the Cell flags — the maybe_load_misc_settings pattern).
    fn browse_kick(&self, cx: &mut Context<Self>) {
        let state = &self.misc_settings_state;
        if !state.browse_open || !state.browse_dirty.get() || state.browse_loading.get() {
            return;
        }
        let dir = state.browse_dir.clone();
        let partial = if dir == "/" {
            "/".to_string()
        } else {
            format!("{dir}/")
        };
        let Some(rx) = self.send_query("filesystem.browse", json!({ "partialPath": partial }))
        else {
            return;
        };
        state.browse_loading.set(true);
        let requested_dir = dir.clone();
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.misc_settings_state;
                state.browse_loading.set(false);
                // A navigation during the flight re-armed `dirty` — leave it
                // set so the next render fetches the new directory.
                if state.browse_dir == requested_dir {
                    state.browse_dirty.set(false);
                } else {
                    cx.notify();
                    return;
                }
                match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        let value = &exit["value"];
                        state.browse_error = None;
                        if let Some(parent) = value["parentPath"].as_str() {
                            state.browse_dir = parent.to_string();
                        }
                        state.browse_entries = value["entries"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|e| {
                                        let name = e["name"].as_str()?.to_string();
                                        let full = e["fullPath"].as_str()?.to_string();
                                        Some((name, full))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                    }
                    Ok(exit) => {
                        state.browse_entries.clear();
                        state.browse_error = Some(exit_failure_message(&exit));
                    }
                    Err(_) => {
                        state.browse_entries.clear();
                        state.browse_error =
                            Some("Lost connection before the reply arrived.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Navigate into a subdirectory (or up via `..`-style parent handling).
    fn browse_nav(&mut self, dir: String, cx: &mut Context<Self>) {
        let state = &mut self.misc_settings_state;
        let dir = dir.trim_end_matches('/').to_string();
        state.browse_dir = if dir.is_empty() { "/".into() } else { dir };
        state.browse_dirty.set(true);
        cx.notify();
    }

    /// One level up from the current browse directory.
    fn browse_up(&mut self, cx: &mut Context<Self>) {
        let dir = self.misc_settings_state.browse_dir.clone();
        let parent = std::path::Path::new(&dir)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string());
        self.browse_nav(parent, cx);
    }

    /// "Use this folder": copy the browsed directory into the path field
    /// (+ its draft mirror) and close the browser.
    fn browse_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.misc_settings_state.browse_dir.clone();
        let state = &mut self.misc_settings_state;
        state.browse_open = false;
        state.draft_path = dir.clone();
        if let Some(field) = state.path_field.clone() {
            field.update(cx, |field, cx| {
                field.reset(window, cx);
                field.replace_text_in_range(Some(0..0), &dir, window, cx);
            });
            field.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// `project.update {projectIcon:{kind:"emoji",emoji}}` — the wire accepts
    /// the StoredProjectIcon shape; ShellProject doesn't carry it back, so
    /// the pick only shows as a toast here (the web renders it).
    fn set_project_icon(&mut self, project_id: String, emoji: &str, cx: &mut Context<Self>) {
        let state = &mut self.misc_settings_state;
        if state.pending.contains(&project_id) {
            return;
        }
        state.icon_picker = None;
        let pending_id = project_id.clone();
        self.project_mutate(
            json!({
                "type": "project.update",
                "commandId": format!("c67-app:{}", Uuid::new_v4()),
                "projectId": project_id,
                "projectIcon": { "kind": "emoji", "emoji": emoji },
            }),
            Some(pending_id),
            cx,
        );
    }

    pub(crate) fn render_projects_section(&self, cx: &mut Context<Self>) -> gpui::Div {
        // `addProjectBaseDirectory` seeds the browser's starting folder.
        self.maybe_load_misc_settings(cx);
        self.browse_kick(cx);
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

        if connected {
            section = section.child(self.render_scope_selector(cx));
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
                            .rounded_lg()
                            .border_1()
                            .border_color(theme::input())
                            .bg(theme::field_surface())
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
                            this.start_project_rename(
                                rename_id.clone(),
                                &rename_current,
                                window,
                                cx,
                            );
                        }),
                    );
            }
            row = row.child(rename_btn);

            // Icon — opens the emoji picker row below (`project.update`
            // `{projectIcon}` round-trips to other clients; ShellProject
            // doesn't carry it, so the native sidebar stays text-only).
            let icon_id = project_id.clone();
            let picker_open = state.icon_picker.as_deref() == Some(project_id.as_str());
            let mut icon_btn = div()
                .id(("project-icon", index))
                .px_3()
                .py_1p5()
                .rounded_md()
                .text_xs()
                .text_color(theme::fg_muted());
            if pending {
                icon_btn = icon_btn.opacity(0.4).child("Icon");
            } else {
                icon_btn = icon_btn
                    .cursor_pointer()
                    .bg(theme::hover())
                    .hover(|s| s.bg(theme::selected()))
                    .child(if picker_open { "Close" } else { "Icon" })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            let state = &mut this.misc_settings_state;
                            state.icon_picker =
                                if state.icon_picker.as_deref() == Some(icon_id.as_str()) {
                                    None
                                } else {
                                    Some(icon_id.clone())
                                };
                            cx.notify();
                        }),
                    );
            }
            row = row.child(icon_btn);

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

            // Emoji picker — same PROJECT_EMOJIS set as the web's
            // ProjectIconPickerDialog (lucide/monogram kinds are not
            // offered here; the wire would accept them).
            if picker_open {
                let mut picker = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .bg(theme::selected());
                for (emoji_index, emoji) in PROJECT_EMOJIS.iter().enumerate() {
                    let pick_id = project_id.clone();
                    let emoji = *emoji;
                    picker = picker.child(
                        div()
                            .id(("project-emoji", index * 100 + emoji_index))
                            .px_1p5()
                            .py_0p5()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .child(emoji)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    this.set_project_icon(pick_id.clone(), emoji, cx);
                                }),
                            ),
                    );
                }
                section = section.child(picker);
            }
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
                                .rounded_lg()
                                .border_1()
                                .border_color(theme::input())
                                .bg(theme::field_surface())
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
            // root the user types in or picks in the `filesystem.browse`
            // browser below.
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
                                .rounded_lg()
                                .border_1()
                                .border_color(theme::input())
                                .bg(theme::field_surface())
                                .text_sm()
                                .text_color(theme::fg())
                                .child(field),
                        )
                        .child(
                            div()
                                .id("project-browse-toggle")
                                .px_3()
                                .py_1p5()
                                .rounded_md()
                                .text_xs()
                                .cursor_pointer()
                                .bg(theme::hover())
                                .text_color(theme::fg())
                                .hover(|s| s.bg(theme::selected()))
                                .child(if state.browse_open {
                                    "Close browser"
                                } else {
                                    "Browse…"
                                })
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _ev, _window, cx| {
                                        if this.misc_settings_state.browse_open {
                                            this.misc_settings_state.browse_open = false;
                                            cx.notify();
                                        } else {
                                            this.open_browse(cx);
                                        }
                                    }),
                                ),
                        ),
                );
            }

            // Directory browser — `filesystem.browse` lists subdirectories
            // of `browse_dir`; rows navigate in, "↑" goes up, "Use this
            // folder" feeds the path field above.
            if state.browse_open {
                let mut panel = div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::canvas());
                panel = panel.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("project-browse-up")
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .text_xs()
                                .cursor_pointer()
                                .bg(theme::hover())
                                .text_color(theme::fg())
                                .hover(|s| s.bg(theme::selected()))
                                .child("↑ Up")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _ev, _window, cx| {
                                        this.browse_up(cx);
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child(state.browse_dir.clone()),
                        )
                        .child(
                            div()
                                .id("project-browse-select")
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .text_xs()
                                .cursor_pointer()
                                .bg(theme::primary())
                                .text_color(gpui::white())
                                .child("Use this folder")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _ev, window, cx| {
                                        this.browse_select(window, cx);
                                    }),
                                ),
                        ),
                );
                if state.browse_loading.get() {
                    panel = panel.child(
                        div()
                            .py_1()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("Loading…"),
                    );
                } else if let Some(error) = &state.browse_error {
                    panel = panel.child(
                        div()
                            .py_1()
                            .text_xs()
                            .text_color(danger())
                            .child(error.clone()),
                    );
                } else if state.browse_entries.is_empty() {
                    panel = panel.child(
                        div()
                            .py_1()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("No subfolders here."),
                    );
                } else {
                    let mut list = div().flex().flex_col();
                    for (entry_index, (name, full)) in state.browse_entries.iter().enumerate() {
                        let full = full.clone();
                        list = list.child(
                            div()
                                .id(("project-browse-entry", entry_index))
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .text_sm()
                                .cursor_pointer()
                                .text_color(theme::fg())
                                .hover(|s| s.bg(theme::hover()))
                                .child(format!("📁 {name}"))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _ev, _window, cx| {
                                        this.browse_nav(full.clone(), cx);
                                    }),
                                ),
                        );
                    }
                    panel = panel.child(
                        div()
                            .id("project-browse-list")
                            .h(px(140.))
                            .overflow_y_scroll()
                            .child(list),
                    );
                }
                form = form.child(panel);
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
                div().flex().items_center().gap_2().child(add_btn).child(
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
                                state.browse_open = false;
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

        // ── Actions (`defaultProjectScripts`, scoped) ────────────────────
        if connected {
            section = section.child(self.render_scripts_group(cx));
        }

        section
    }

    /// The "Actions" group: a row per `ProjectScript` with Run/Edit/Delete,
    /// the add/edit form, and a scope chip. Mirrors
    /// `ProjectScriptsSettingsSection` in the web minus the t3.json tier
    /// (not part of `ServerSettings`).
    fn render_scripts_group(&self, cx: &mut Context<Self>) -> gpui::Div {
        let state = &self.misc_settings_state;
        let scripts = self.effective_scripts();
        let scope_chip = self.scope_chip("defaultProjectScripts", cx);
        let mut group = div().flex().flex_col();

        group = group.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().min_w_0().child(group_label(
                    "Actions — scripts you can run in a thread's terminal",
                )))
                .when_some(scope_chip, |s, chip| s.child(chip)),
        );

        if scripts.is_empty() {
            group = group.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No actions yet — scripts like “Dev server” or “Tests” show up here."),
            );
        }

        for (index, script) in scripts.iter().enumerate() {
            let id = script["id"].as_str().unwrap_or_default().to_string();
            let name = script["name"].as_str().unwrap_or("script").to_string();
            let command = script["command"].as_str().unwrap_or_default().to_string();
            let icon = script["icon"].as_str().unwrap_or("play");
            let mut detail = format!("{icon} · {command}");
            if script["runOnWorktreeCreate"].as_bool() == Some(true) {
                detail.push_str(" · runs on worktree create");
            }
            if script["runOnSettle"].as_bool() == Some(true) {
                detail.push_str(" · runs on settle");
            }
            if script["stopOnSettle"].as_bool() == Some(true) {
                detail.push_str(" · stops on settle");
            }
            if script["async"].as_bool() == Some(true) {
                detail.push_str(" · async");
            }
            if script["previewUrl"].as_str().is_some() {
                detail.push_str(" · preview");
            }

            let run_name = name.clone();
            let run_command = command.clone();
            let edit_script = script.clone();
            let delete_id = id.clone();
            group = group.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
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
                                    .truncate()
                                    .child(name),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::fg_muted())
                                    .truncate()
                                    .child(detail),
                            ),
                    )
                    .child(script_row_button(
                        ("script-run", index),
                        "Run",
                        cx.listener(move |this, _ev, window, cx| {
                            this.run_project_script(
                                run_name.clone(),
                                run_command.clone(),
                                window,
                                cx,
                            );
                        }),
                    ))
                    .child(script_row_button(
                        ("script-edit", index),
                        "Edit",
                        cx.listener(move |this, _ev, window, cx| {
                            this.open_script_form(Some(edit_script.clone()), window, cx);
                        }),
                    ))
                    .child(
                        div()
                            .id(("script-delete", index))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .text_color(danger())
                            .hover(|s| s.bg(theme::hover()))
                            .child("Delete")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    this.remove_script(delete_id.clone(), cx);
                                }),
                            ),
                    ),
            );
        }

        if state.script_form_open {
            group = group.child(self.render_script_form(cx));
        } else {
            group = group.child(
                div().pt_1().flex().child(
                    div()
                        .id("script-add-open")
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .bg(theme::hover())
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::selected()))
                        .child("+ Add action")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, window, cx| {
                                this.open_script_form(None, window, cx);
                            }),
                        ),
                ),
            );
        }

        group
    }

    /// The add/edit form for one `ProjectScript` (name, command, icon,
    /// runOnWorktreeCreate/runOnSettle switches). The optional contract
    /// fields (`stopOnSettle`, `async`, `previewUrl`, `autoOpenPreview`) are
    /// not editable yet — the note says so rather than faking controls.
    fn render_script_form(&self, cx: &mut Context<Self>) -> gpui::Div {
        let state = &self.misc_settings_state;
        let editing = state.script_editing.is_some();
        let mut form = div().flex().flex_col().gap_2().pt_2();

        if let Some(field) = state.script_name_field.clone() {
            form = form.child(field_box(field));
        }
        if let Some(field) = state.script_command_field.clone() {
            form = form.child(field_box(field));
        }

        // Icon picker — the six ProjectScript.icon literals.
        form = form.child(div().flex().items_center().gap_1().children(
            PROJECT_SCRIPT_ICONS.iter().enumerate().map(|(i, icon)| {
                let icon = icon.to_string();
                let selected = state.script_icon == icon;
                div()
                    .id(("script-icon", i))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .when(selected, |s| {
                        s.bg(theme::selected()).text_color(theme::primary())
                    })
                    .when(!selected, |s| {
                        s.bg(theme::hover())
                            .text_color(theme::fg_muted())
                            .hover(|s| s.bg(theme::selected()))
                    })
                    .child(icon.clone())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.misc_settings_state.script_icon = icon.clone();
                            cx.notify();
                        }),
                    )
            }),
        ));

        for (label, flag, elem) in [
            (
                "Run when a worktree is created",
                state.script_on_create,
                "script-flag-create",
            ),
            (
                "Run when a thread settles",
                state.script_on_settle,
                "script-flag-settle",
            ),
        ] {
            form = form.child(
                div()
                    .id(elem)
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(switch_control(flag))
                    .child(div().text_xs().text_color(theme::fg()).child(label))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            let state = &mut this.misc_settings_state;
                            if elem == "script-flag-create" {
                                state.script_on_create = !state.script_on_create;
                            } else {
                                state.script_on_settle = !state.script_on_settle;
                            }
                            cx.notify();
                        }),
                    ),
            );
        }

        form = form.child(
            div()
                .text_xs()
                .text_color(theme::fg_muted())
                .child("stopOnSettle, async, previewUrl and autoOpenPreview aren't editable here yet — set them in the web app."),
        );

        let can_save = !state.script_name_draft.trim().is_empty()
            && !state.script_command_draft.trim().is_empty();
        form = form.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .pt_1()
                .child(
                    div()
                        .id("script-form-save")
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .when(can_save, |s| {
                            s.cursor_pointer()
                                .bg(theme::primary())
                                .text_color(gpui::white())
                        })
                        .when(!can_save, |s| {
                            s.bg(theme::hover()).text_color(theme::fg_muted())
                        })
                        .child(if editing {
                            "Save changes"
                        } else {
                            "Add action"
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, window, cx| {
                                if can_save {
                                    this.save_script_click(window, cx);
                                }
                            }),
                        ),
                )
                .child(
                    div()
                        .id("script-form-cancel")
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
                                this.misc_settings_state.script_form_open = false;
                                this.misc_settings_state.script_editing = None;
                                cx.notify();
                            }),
                        ),
                ),
        );

        form
    }
}

/// Small bordered action button for script rows.
fn script_row_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    listener: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_md()
        .text_xs()
        .cursor_pointer()
        .bg(theme::hover())
        .text_color(theme::fg())
        .hover(|s| s.bg(theme::selected()))
        .child(label)
        .on_mouse_down(MouseButton::Left, listener)
}

/// Standard bordered field wrapper used across the settings forms — the web
/// `Input`: `rounded-lg border-input`, `bg-input/32` surface, `px-3`.
fn field_box(field: Entity<TextField>) -> gpui::Div {
    div()
        .px_3()
        .py_1()
        .rounded_lg()
        .border_1()
        .border_color(theme::input())
        .bg(theme::field_surface())
        .text_color(theme::fg())
        .text_sm()
        .child(field)
}

/// Expand a leading `~` or `~/…` against `$HOME` for the add-folder field.
pub(crate) fn expand_home(path: &str) -> String {
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
