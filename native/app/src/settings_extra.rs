//! Extra settings sections: Keybindings, Source Control, Threads.
//!
//! - **Keybindings** edits the server's `keybindings.json` through
//!   `server.upsertKeybinding` / `server.removeKeybinding`
//!   (`{key, command, when?}` rules; the server validates the shortcut and
//!   when-expression). The list reads `server.getConfig →
//!   keybindings.{bindings,issues}` + `keybindingsConfigPath`. The add-rule
//!   form mirrors the web editor: a command picker over
//!   `STATIC_KEYBINDING_COMMANDS` + `script.<id>.run` commands, a "record
//!   keys" capture box that writes `mod+shift+x` notation, and conflict
//!   highlighting for keys bound to more than one command.
//! - **Source Control** renders `server.discoverSourceControl` — VCS tools
//!   (git/jj) with version/installHint rows and provider rows (GitHub) with
//!   auth state — plus the device-flow sign-in (`signInStart` →
//!   `signInWait`, cancel via `signInCancel`). The writing settings
//!   (`sourceControlWritingStyle.mode` + `customInstructions`,
//!   `sourceControlWriterModelSelection`) are scoped ServerSettings written
//!   via `server.updateSettings` / `projectSettingsOverrides`.
//! - **Threads** covers the web Threads/Diffs/Worktrees panels that are
//!   meaningful on a single-environment native client: diff defaults are
//!   client-persisted (`ClientConfig`, like the web's ClientSettings), the
//!   rest are ServerSettings — the scoped keys (`newWorktreesStartFromOrigin`,
//!   `worktreeSubmodules`, auto-settle, text generation,
//!   `threadTitleInstructions`, `responseStreamingMode`, agent access)
//!   honor the Global/Project scope selector. Browser/voice/licenses get
//!   honest explanatory text; an "About this server" group shows
//!   `environment.serverVersion` and checks `server.getNpmRelease`.

use std::cell::Cell;
use std::collections::HashMap;

use gpui::{
    Context, Div, Entity, EntityInputHandler, FocusHandle, Focusable, MouseButton, SharedString,
    Subscription, Window, div, prelude::*, px,
};
use serde_json::{Value, json};

use crate::AppView;
use crate::composer::MODEL_CATALOG;
use crate::icons::{Icon, icon};
use crate::settings_misc::{
    self, AUTO_VALUE, OpenList, danger, edit_field, exit_failure_message, field_text_in,
    group_label, pack_model, setting_row, switch_control,
};
use crate::text_field::{PromptSubmitted, TextField};
use crate::theme;

/// `sourceControlWritingStyle.mode` options — labels/descriptions from
/// `MODE_OPTIONS` in apps/web/…/SourceControlWritingSettings.tsx.
const WRITING_MODES: &[(&str, &str, &str)] = &[
    (
        "repo_conventions",
        "Repository conventions",
        "Match each project's commit and pull request history.",
    ),
    (
        "conventional_commits",
        "Conventional Commits",
        "Conventional Commit prefixes, short PR titles and descriptions.",
    ),
    (
        "custom",
        "Custom instructions",
        "Your instructions shape commit and pull request text.",
    ),
];

/// `worktreeSubmodules` options — `WorktreeSubmodules` literals.
const SUBMODULE_MODES: &[(&str, &str)] = &[
    ("recursive", "Recursive"),
    ("top-level", "Top level only"),
    ("none", "None"),
];

/// `sidebarAutoSettleAfterDays` choices (value string → days; "off" → null).
const AUTO_SETTLE_DAYS: &[(&str, &str)] = &[
    ("off", "Off"),
    ("1", "1 day"),
    ("3", "3 days"),
    ("7", "7 days"),
    ("14", "14 days"),
    ("30", "30 days"),
];

/// `STATIC_KEYBINDING_COMMANDS` — the command picker list, copied verbatim
/// from packages/contracts/src/keybindings.ts (`script.<id>.run` commands
/// are appended dynamically from `defaultProjectScripts`).
const KEYBINDING_COMMANDS: &[&str] = &[
    "workspace.focusNext",
    "workspace.focusPrevious",
    "sidebar.toggle",
    "navigation.back",
    "navigation.forward",
    "terminal.toggle",
    "terminal.split",
    "terminal.splitVertical",
    "terminal.new",
    "terminal.close",
    "rightPanel.toggle",
    "rightPanel.new",
    "threadPanel.toggle",
    "rightPanel.toggleMaximized",
    "rightPanel.close",
    "view.reopenClosed",
    "pullRequest.copyNumber",
    "diff.toggle",
    "preview.toggle",
    "preview.refresh",
    "preview.focusUrl",
    "preview.zoomIn",
    "preview.zoomOut",
    "preview.resetZoom",
    "commandPalette.toggle",
    "filePicker.toggle",
    "projectSearch.toggle",
    "usage.open",
    "theme.select",
    "appearance.cycle",
    "themeEditor.toggle",
    "composer.stash",
    "composer.sendAlternate",
    "composer.sendBackground",
    "composer.sendAndNewThread",
    "composer.dictation",
    "composer.host",
    "composer.cycleHost",
    "composer.effort",
    "composer.increaseEffort",
    "composer.decreaseEffort",
    "composer.mode",
    "composer.workspace",
    "composer.previousWorktree",
    "composer.branch",
    "chat.new",
    "chat.newLocal",
    "chat.newWithoutProject",
    "chat.find",
    "editor.openFavorite",
    "usage.cost",
    "usage.tokens",
    "usage.limits",
    "usage.period.day",
    "usage.period.week",
    "usage.period.month",
    "usage.period.quarter",
    "modelPicker.toggle",
    "modelPicker.previousProvider",
    "modelPicker.nextProvider",
    "modelPicker.jump.1",
    "modelPicker.jump.2",
    "modelPicker.jump.3",
    "modelPicker.jump.4",
    "modelPicker.jump.5",
    "modelPicker.jump.6",
    "modelPicker.jump.7",
    "modelPicker.jump.8",
    "modelPicker.jump.9",
    "thread.stop",
    "thread.steerQueuedMessage",
    "thread.editQueuedMessage",
    "thread.previous",
    "thread.next",
    "thread.copyReference",
    "thread.settle",
    "thread.pin",
    "thread.regenerateTitle",
    "thread.undo",
    "thread.jump.1",
    "thread.jump.2",
    "thread.jump.3",
    "thread.jump.4",
    "thread.jump.5",
    "thread.jump.6",
    "thread.jump.7",
    "thread.jump.8",
    "thread.jump.9",
];

/// `responseStreamingMode` options.
const STREAMING_MODES: &[(&str, &str, &str)] = &[
    (
        "turn",
        "After each turn",
        "Update the transcript when a turn finishes.",
    ),
    (
        "paragraph",
        "Live paragraphs",
        "Stream paragraphs as the agent writes them.",
    ),
];

/// State for the sections in this file. It hangs off
/// `settings_misc::State::extra` — the AppView field list lives in lib.rs.
#[derive(Default)]
pub(crate) struct ExtraState {
    /// TextField entities + subscriptions are created lazily on first
    /// section render (`cx.new` needs a context; the deferred-open pattern
    /// from settings_backend.rs).
    prepared: Cell<bool>,

    // ── Keybindings ─────────────────────────────────────────────────────
    kb_loaded: Cell<bool>,
    kb_loading: Cell<bool>,
    /// Raw rules from `server.getConfig → keybindings.bindings`
    /// (`{key, command, when?}`).
    kb_bindings: Vec<Value>,
    /// `keybindings.issues` from the same snapshot.
    kb_issues: Vec<Value>,
    /// `keybindingsConfigPath` for display.
    kb_path: Option<String>,
    kb_error: Option<String>,
    kb_add_open: bool,
    kb_saving: bool,
    kb_command_draft: String,
    kb_key_draft: String,
    kb_when_draft: String,
    kb_command_field: Option<Entity<TextField>>,
    kb_key_field: Option<Entity<TextField>>,
    kb_when_field: Option<Entity<TextField>>,
    kb_subs: Vec<Subscription>,
    /// Command picker list open inside the add-rule form.
    kb_command_picker: bool,
    /// "Record keystroke" mode: the capture box owns `kb_capture_focus` and
    /// the next `KeyDownEvent` becomes `kb_key_draft`.
    kb_capturing: bool,
    kb_capture_focus: Option<FocusHandle>,

    // ── Source Control ──────────────────────────────────────────────────
    /// `server.discoverSourceControl` snapshot (`versionControlSystems` +
    /// `sourceControlProviders`) — `Cell` flags for the lazy load.
    sc_discovery: Option<Value>,
    sc_disc_loaded: Cell<bool>,
    sc_disc_loading: Cell<bool>,
    /// Device-flow prompt returned by `sourceControl.signInStart`
    /// (`userCode`, `verificationUri`) while `signInWait` runs.
    sc_signin_prompt: Option<(String, String)>,
    /// Busy flag for the Sign-in round trip.
    sc_busy: bool,
    /// Status line under the provider row (sign-in outcome or load error).
    sc_note: Option<String>,
    sc_instructions_field: Option<Entity<TextField>>,
    sc_instructions_draft: String,
    /// True once the field was seeded from the settings mirror (don't
    /// clobber an in-progress edit on refresh).
    sc_instructions_seeded: bool,

    // ── Threads ─────────────────────────────────────────────────────────
    base_dir_field: Option<Entity<TextField>>,
    base_dir_draft: String,
    base_dir_seeded: bool,
    /// `threadTitleInstructions` (scoped key) — extra guidance appended to
    /// generated thread titles.
    title_instructions_field: Option<Entity<TextField>>,
    title_instructions_draft: String,
    title_instructions_seeded: bool,
    extra_subs: Vec<Subscription>,

    // ── About & updates (end of the Threads section) ────────────────────
    /// Result of the last `server.getNpmRelease` check, rendered inline.
    update_status: Option<String>,
    update_checking: bool,
}

impl ExtraState {
    fn reset_form(&mut self) {
        self.kb_add_open = false;
        self.kb_command_picker = false;
        self.kb_capturing = false;
        self.kb_command_draft.clear();
        self.kb_key_draft.clear();
        self.kb_when_draft.clear();
    }
}

impl AppView {
    // ─── lazy loads ─────────────────────────────────────────────────────

    /// First-render `server.getConfig` for the keybindings mirror.
    /// (The misc mirror extracts `settings`; this one extracts
    /// `keybindings.*` — same payload, separate flag so sections stay
    /// independent.)
    fn maybe_load_keybindings(&self, cx: &mut Context<Self>) {
        let state = &self.misc_settings_state.extra;
        if state.kb_loaded.get() || state.kb_loading.get() {
            return;
        }
        match self.send_query("server.getConfig", json!({})) {
            Some(rx) => {
                state.kb_loading.set(true);
                cx.spawn(async move |this, cx| {
                    let reply = rx.recv().await;
                    let _ = this.update(cx, |this, cx| {
                        this.apply_keybindings_reply(reply);
                        cx.notify();
                    });
                })
                .detach();
            }
            None => state.kb_loaded.set(true),
        }
    }

    /// Fold a `server.getConfig` Exit into the keybindings mirror.
    fn apply_keybindings_reply(&mut self, reply: Result<Value, smol::channel::RecvError>) {
        let state = &mut self.misc_settings_state.extra;
        state.kb_loading.set(false);
        state.kb_loaded.set(true);
        match reply {
            Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                let value = &exit["value"];
                state.kb_bindings = value["keybindings"]["bindings"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                state.kb_issues = value["keybindings"]["issues"]
                    .as_array()
                    .or_else(|| value["issues"].as_array())
                    .cloned()
                    .unwrap_or_default();
                state.kb_path = value["keybindingsConfigPath"].as_str().map(str::to_string);
                state.kb_error = None;
            }
            Ok(exit) => {
                state.kb_error = Some(format!(
                    "Keybindings failed to load: {}",
                    exit_failure_message(&exit)
                ));
            }
            Err(_) => {
                state.kb_error = Some("Lost connection before the reply arrived.".into());
            }
        }
    }

    /// Refetch after a keybinding write — the upsert/remove replies carry
    /// resolved rules, not the raw `bindings` this list renders.
    fn reload_keybindings(&mut self, cx: &mut Context<Self>) {
        self.misc_settings_state.extra.kb_loaded.set(false);
        self.maybe_load_keybindings(cx);
    }

    /// First-render `server.discoverSourceControl` — VCS tools (git/jj) and
    /// forge providers (GitHub CLI auth). This replaces the old
    /// `sourceControl.appAccess` probe, whose input contract requires a
    /// repository and never belonged in settings.
    fn maybe_load_source_control(&self, cx: &mut Context<Self>) {
        let state = &self.misc_settings_state.extra;
        if state.sc_disc_loaded.get() || state.sc_disc_loading.get() {
            return;
        }
        match self.send_query("server.discoverSourceControl", json!({})) {
            Some(rx) => {
                state.sc_disc_loading.set(true);
                cx.spawn(async move |this, cx| {
                    let reply = rx.recv().await;
                    let _ = this.update(cx, |this, cx| {
                        let state = &mut this.misc_settings_state.extra;
                        state.sc_disc_loading.set(false);
                        state.sc_disc_loaded.set(true);
                        match reply {
                            Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                                state.sc_discovery = Some(exit["value"].clone());
                                state.sc_note = None;
                            }
                            Ok(exit) => {
                                state.sc_discovery = None;
                                state.sc_note = Some(format!(
                                    "Discovery failed: {}",
                                    exit_failure_message(&exit)
                                ));
                            }
                            Err(_) => {
                                state.sc_discovery = None;
                                state.sc_note =
                                    Some("Lost connection before the reply arrived.".into());
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            None => state.sc_disc_loaded.set(true),
        }
    }

    /// "Rescan" button — refetch `server.discoverSourceControl`.
    fn rescan_source_control(&mut self, cx: &mut Context<Self>) {
        self.misc_settings_state.extra.sc_disc_loaded.set(false);
        self.maybe_load_source_control(cx);
    }

    /// Lazily build the extra-section TextFields + subscriptions on first
    /// render of a section that needs them (deferred-open pattern — render
    /// is `&self`, so this queues one `this.update` that creates them).
    fn maybe_prepare_extra(&self, cx: &mut Context<Self>) {
        if self.misc_settings_state.extra.prepared.get() {
            return;
        }
        self.misc_settings_state.extra.prepared.set(true);
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |this, cx| {
                this.ensure_extra_fields(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Create the persistent extra-section fields (`server.getConfig`
    /// values are seeded in whenever they arrive or already exist).
    fn ensure_extra_fields(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.misc_settings_state.extra;
        if state.base_dir_field.is_none() {
            let field = cx.new(|cx| TextField::new("~/ (leave empty for home)", cx));
            state.extra_subs.push(cx.observe(&field, |this, field, cx| {
                this.misc_settings_state.extra.base_dir_draft =
                    settings_misc::field_text(&field, cx);
                cx.notify();
            }));
            state.extra_subs.push(cx.subscribe(
                &field,
                |this, _field, event: &PromptSubmitted, cx| {
                    let value = event.0.to_string();
                    this.update_server_settings(
                        json!({ "addProjectBaseDirectory": value.trim() }),
                        cx,
                    );
                },
            ));
            state.base_dir_field = Some(field);
        }
        if state.sc_instructions_field.is_none() {
            let field = cx.new(|cx| TextField::new("Instructions for commit / PR text", cx));
            state.extra_subs.push(cx.observe(&field, |this, field, cx| {
                this.misc_settings_state.extra.sc_instructions_draft =
                    settings_misc::field_text(&field, cx);
                cx.notify();
            }));
            state.extra_subs.push(cx.subscribe(
                &field,
                |this, _field, event: &PromptSubmitted, cx| {
                    let instructions = event.0.to_string();
                    this.save_sc_instructions(instructions, cx);
                },
            ));
            state.sc_instructions_field = Some(field);
        }
        if state.title_instructions_field.is_none() {
            let field = cx.new(|cx| TextField::new("Extra title guidance (optional)", cx));
            state.extra_subs.push(cx.observe(&field, |this, field, cx| {
                this.misc_settings_state.extra.title_instructions_draft =
                    settings_misc::field_text(&field, cx);
                cx.notify();
            }));
            state.extra_subs.push(cx.subscribe(
                &field,
                |this, _field, event: &PromptSubmitted, cx| {
                    // Scoped key — writes land in the override entry when a
                    // project is selected; empty clears to inherit.
                    this.update_scoped_setting(
                        "threadTitleInstructions",
                        json!(event.0.trim()),
                        cx,
                    );
                },
            ));
            state.title_instructions_field = Some(field);
        }
        self.seed_extra_fields(cx);
    }

    /// Push the loaded ServerSettings into the persistent fields once —
    /// either just after `ensure_extra_fields` or when `server.getConfig`
    /// lands (called from `maybe_load_misc_settings`' success arm too).
    pub(crate) fn seed_extra_fields(&mut self, cx: &mut Context<Self>) {
        let settings = self.misc_settings_state.settings.clone();
        // Scoped key — seed the effective value for the current scope.
        let title_value = self
            .scoped_value("threadTitleInstructions")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let state = &mut self.misc_settings_state.extra;
        if !state.base_dir_seeded
            && let Some(field) = state.base_dir_field.clone()
        {
            let value = settings["addProjectBaseDirectory"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if !value.is_empty() {
                state.base_dir_draft = value.clone();
                edit_field(&field, cx, |field, window, cx| {
                    field.reset(window, cx);
                    field.replace_text_in_range(Some(0..0), &value, window, cx);
                });
            }
            state.base_dir_seeded = true;
        }
        if !state.sc_instructions_seeded
            && let Some(field) = state.sc_instructions_field.clone()
            && let Some(instructions) =
                settings["sourceControlWritingStyle"]["customInstructions"].as_str()
        {
            if !instructions.is_empty() {
                state.sc_instructions_draft = instructions.to_string();
                edit_field(&field, cx, |field, window, cx| {
                    field.reset(window, cx);
                    field.replace_text_in_range(Some(0..0), instructions, window, cx);
                });
            }
            state.sc_instructions_seeded = true;
        }
        if !state.title_instructions_seeded
            && let Some(field) = state.title_instructions_field.clone()
        {
            if !title_value.is_empty() {
                state.title_instructions_draft = title_value.clone();
                edit_field(&field, cx, |field, window, cx| {
                    field.reset(window, cx);
                    field.replace_text_in_range(Some(0..0), &title_value, window, cx);
                });
            }
            state.title_instructions_seeded = true;
        }
    }

    /// `sourceControlWritingStyle.customInstructions` — merged into the
    /// style object so `mode`/`followChangeRequestTemplates` survive.
    /// Scoped key — writes land in the project override when one is active.
    fn save_sc_instructions(&mut self, instructions: String, cx: &mut Context<Self>) {
        let mut style = self
            .scoped_value("sourceControlWritingStyle")
            .as_object()
            .cloned()
            .unwrap_or_default();
        style.insert("mode".into(), json!("custom"));
        style.insert("customInstructions".into(), json!(instructions));
        self.update_scoped_setting("sourceControlWritingStyle", Value::Object(style), cx);
    }

    // ─── Keybindings ────────────────────────────────────────────────────

    /// "+ Add rule" — build the three fields once, clear drafts, focus the
    /// command input.
    fn open_kb_add_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        {
            let state = &mut self.misc_settings_state.extra;
            if state.kb_command_field.is_none() {
                let command_field = cx.new(|cx| {
                    TextField::new("Command (e.g. chat.new or script.my-script.run)", cx)
                });
                let key_field = cx.new(|cx| TextField::new("Shortcut (e.g. mod+shift+k)", cx));
                let when_field = cx.new(|cx| TextField::new("When expression (optional)", cx));
                state
                    .kb_subs
                    .push(cx.observe(&command_field, |this, field, cx| {
                        this.misc_settings_state.extra.kb_command_draft =
                            settings_misc::field_text(&field, cx);
                        cx.notify();
                    }));
                state
                    .kb_subs
                    .push(cx.observe(&key_field, |this, field, cx| {
                        this.misc_settings_state.extra.kb_key_draft =
                            settings_misc::field_text(&field, cx);
                        cx.notify();
                    }));
                state
                    .kb_subs
                    .push(cx.observe(&when_field, |this, field, cx| {
                        this.misc_settings_state.extra.kb_when_draft =
                            settings_misc::field_text(&field, cx);
                        cx.notify();
                    }));
                for field in [command_field.clone(), key_field.clone(), when_field.clone()] {
                    state.kb_subs.push(cx.subscribe(
                        &field,
                        |this, _field, _event: &PromptSubmitted, cx| {
                            this.save_keybinding(cx);
                        },
                    ));
                }
                state.kb_command_field = Some(command_field);
                state.kb_key_field = Some(key_field);
                state.kb_when_field = Some(when_field);
            }
            state.reset_form();
            state.kb_add_open = true;
            state.kb_error = None;
        }
        for field in [
            self.misc_settings_state.extra.kb_command_field.clone(),
            self.misc_settings_state.extra.kb_key_field.clone(),
            self.misc_settings_state.extra.kb_when_field.clone(),
        ]
        .into_iter()
        .flatten()
        {
            field.update(cx, |field, cx| field.reset(window, cx));
        }
        if let Some(field) = self.misc_settings_state.extra.kb_command_field.clone() {
            field.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// Save via `server.upsertKeybinding {key, command, when?}` — the
    /// server validates the shortcut grammar and when-expression; failures
    /// surface as `kb_error`.
    fn save_keybinding(&mut self, cx: &mut Context<Self>) {
        if self.misc_settings_state.extra.kb_saving {
            return;
        }
        let (command, key, when) = {
            let state = &self.misc_settings_state.extra;
            (
                state.kb_command_draft.trim().to_string(),
                state.kb_key_draft.trim().to_string(),
                state.kb_when_draft.trim().to_string(),
            )
        };
        if command.is_empty() || key.is_empty() {
            return;
        }
        let mut payload = json!({ "command": command, "key": key });
        if !when.is_empty() {
            payload["when"] = json!(when);
        }
        let Some(rx) = self.send_query("server.upsertKeybinding", payload) else {
            self.misc_settings_state.extra.kb_error = Some("Not connected to a server.".into());
            cx.notify();
            return;
        };
        let state = &mut self.misc_settings_state.extra;
        state.kb_saving = true;
        state.kb_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.misc_settings_state.extra;
                state.kb_saving = false;
                match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        state.kb_error = None;
                        state.reset_form();
                        this.reload_keybindings(cx);
                    }
                    Ok(exit) => {
                        state.kb_error = Some(exit_failure_message(&exit));
                    }
                    Err(_) => {
                        state.kb_error = Some("Lost connection before the reply arrived.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Read the three fields inside a click listener (has `window`), then
    /// save.
    fn save_keybinding_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (field, slot) in [
            self.misc_settings_state
                .extra
                .kb_command_field
                .clone()
                .map(|f| (f, "command")),
            self.misc_settings_state
                .extra
                .kb_key_field
                .clone()
                .map(|f| (f, "key")),
            self.misc_settings_state
                .extra
                .kb_when_field
                .clone()
                .map(|f| (f, "when")),
        ]
        .into_iter()
        .flatten()
        {
            let text = field_text_in(&field, window, cx);
            let state = &mut self.misc_settings_state.extra;
            match slot {
                "command" => state.kb_command_draft = text,
                "key" => state.kb_key_draft = text,
                _ => state.kb_when_draft = text,
            }
        }
        self.save_keybinding(cx);
    }

    /// Per-row delete: `server.removeKeybinding` with the rule's raw
    /// `{key, command, when?}` (the server matches on parsed identity).
    fn remove_keybinding(&mut self, rule: Value, cx: &mut Context<Self>) {
        let Some(rx) = self.send_query("server.removeKeybinding", rule) else {
            self.misc_settings_state.extra.kb_error = Some("Not connected to a server.".into());
            cx.notify();
            return;
        };
        self.misc_settings_state.extra.kb_error = None;
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.misc_settings_state.extra;
                match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        state.kb_error = None;
                        this.reload_keybindings(cx);
                    }
                    Ok(exit) => {
                        state.kb_error = Some(exit_failure_message(&exit));
                    }
                    Err(_) => {
                        state.kb_error = Some("Lost connection before the reply arrived.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Commands the picker offers: `STATIC_KEYBINDING_COMMANDS` plus one
    /// `script.<id>.run` per configured project script (the wire's other
    /// legal command shape — `KeybindingCommand` in keybindings.ts).
    fn known_commands(&self) -> Vec<String> {
        let mut commands: Vec<String> = KEYBINDING_COMMANDS.iter().map(|s| s.to_string()).collect();
        let global = self.misc_settings_state.settings["defaultProjectScripts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut ids = std::collections::HashSet::new();
        for script in global.iter().chain(self.effective_scripts().iter()) {
            if let Some(id) = script["id"].as_str()
                && ids.insert(id.to_string())
            {
                commands.push(format!("script.{id}.run"));
            }
        }
        commands
    }

    /// key → binding count, for conflict highlighting (the web's
    /// `bindingsByKey` conflict rows).
    fn kb_conflicts(&self) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        for rule in &self.misc_settings_state.extra.kb_bindings {
            if let Some(key) = rule["key"].as_str() {
                *counts.entry(key.to_string()).or_insert(0) += 1;
            }
        }
        counts.retain(|_, count| *count > 1);
        counts
    }

    /// "Record" button: arm capture mode and focus the capture box — the
    /// next `KeyDownEvent` becomes `kb_key_draft` in `mod+shift+x` shape.
    fn start_kb_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = &mut self.misc_settings_state.extra;
        let focus = state
            .kb_capture_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        state.kb_capturing = true;
        state.kb_command_picker = false;
        focus.focus(window, cx);
        cx.notify();
    }

    /// `KeyDownEvent` → the `mod+shift+x` grammar the server keymap writes.
    /// Bare modifier presses are ignored (they're just a prefix).
    fn capture_keystroke(
        &mut self,
        ev: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = ev.keystroke.key.to_lowercase();
        if matches!(
            key.as_str(),
            "control" | "alt" | "shift" | "meta" | "super" | "fn" | "capslock"
        ) {
            return;
        }
        let mut parts: Vec<&str> = Vec::new();
        let m = &ev.keystroke.modifiers;
        if m.control || m.platform {
            parts.push("mod");
        }
        if m.alt {
            parts.push("alt");
        }
        if m.shift {
            parts.push("shift");
        }
        parts.push(key.as_str());
        let captured = parts.join("+");
        {
            let state = &mut self.misc_settings_state.extra;
            state.kb_capturing = false;
            state.kb_key_draft = captured.clone();
        }
        if let Some(field) = self.misc_settings_state.extra.kb_key_field.clone() {
            field.update(cx, |field, cx| {
                field.reset(window, cx);
                field.replace_text_in_range(Some(0..0), &captured, window, cx);
            });
        }
        cx.notify();
    }

    /// Pick a command from the picker list: set the draft + field text and
    /// collapse the picker.
    fn pick_kb_command(&mut self, command: String, window: &mut Window, cx: &mut Context<Self>) {
        {
            let state = &mut self.misc_settings_state.extra;
            state.kb_command_picker = false;
            state.kb_command_draft = command.clone();
        }
        if let Some(field) = self.misc_settings_state.extra.kb_command_field.clone() {
            field.update(cx, |field, cx| {
                field.reset(window, cx);
                field.replace_text_in_range(Some(0..0), &command, window, cx);
            });
        }
        cx.notify();
    }

    pub(crate) fn render_keybindings_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_keybindings(cx);
        let state = &self.misc_settings_state.extra;
        let connected = self.commands.is_some();
        let mut section = div().flex().flex_col();

        if !connected {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Connect to a server to edit keybindings."),
            );
            return section;
        }
        if state.kb_loading.get() && state.kb_bindings.is_empty() {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Loading keybindings…"),
            );
        }
        if let Some(path) = &state.kb_path {
            section = section.child(
                div()
                    .pb_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(format!("Editing {path} on the server.")),
            );
        }
        if let Some(error) = &state.kb_error {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(error.clone()),
            );
        }
        for issue in &state.kb_issues {
            let message = issue["message"].as_str().unwrap_or("Invalid rule");
            section = section.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(icon(Icon::TriangleAlert, 11.).text_color(danger()))
                    .child(SharedString::from(message.to_string())),
            );
        }

        let conflicts = self.kb_conflicts();
        section = section.child(group_label("Rules (action → keystroke)"));
        if state.kb_bindings.is_empty() && !state.kb_loading.get() {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No custom keybindings — the server defaults apply."),
            );
        }
        for (index, rule) in state.kb_bindings.iter().enumerate() {
            let command = rule["command"].as_str().unwrap_or("").to_string();
            let key = rule["key"].as_str().unwrap_or("").to_string();
            let conflicted = conflicts.contains_key(&key);
            let when = rule["when"].as_str().map(str::to_string);
            section = section.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .text_sm()
                    .when(conflicted, |s| s.bg(theme::hover()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(if conflicted { danger() } else { theme::fg() })
                            .child(command),
                    )
                    .when_some(when, |s, when| {
                        s.child(
                            div()
                                .flex_none()
                                .max_w(px(140.))
                                .truncate()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child(when),
                        )
                    })
                    .child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(if conflicted {
                                danger()
                            } else {
                                theme::border()
                            })
                            .bg(theme::kbd_bg())
                            .text_xs()
                            .text_color(if conflicted {
                                danger()
                            } else {
                                theme::fg_muted()
                            })
                            .child(key),
                    )
                    .when(conflicted, |s| {
                        s.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(danger())
                                .child("conflict"),
                        )
                    })
                    .child(
                        div()
                            .id(("kb-remove", index))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .bg(theme::hover())
                            .text_color(theme::fg_muted())
                            .hover(|s| s.bg(theme::selected()))
                            .child("Remove")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener({
                                    let rule = rule.clone();
                                    move |this, _ev, _window, cx| {
                                        this.remove_keybinding(rule.clone(), cx);
                                    }
                                }),
                            ),
                    ),
            );
        }

        if state.kb_add_open {
            let mut form = div().flex().flex_col().gap_2().pt_2();

            // ── Command + picker ────────────────────────────────────────
            if let Some(field) = state.kb_command_field.clone() {
                form = form.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::fg_muted())
                                        .child("Command"),
                                )
                                .child(
                                    div()
                                        .id("kb-cmd-picker-toggle")
                                        .cursor_pointer()
                                        .text_xs()
                                        .text_color(theme::primary())
                                        .hover(|s| s.text_color(theme::fg()))
                                        .child(if state.kb_command_picker {
                                            "hide picker"
                                        } else {
                                            "pick a command…"
                                        })
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _ev, _window, cx| {
                                                let state = &mut this.misc_settings_state.extra;
                                                state.kb_command_picker = !state.kb_command_picker;
                                                state.kb_capturing = false;
                                                cx.notify();
                                            }),
                                        ),
                                ),
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
            if state.kb_command_picker {
                let mut picker = div()
                    .flex()
                    .flex_col()
                    .max_h(px(220.))
                    .overflow_hidden()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::raised())
                    .shadow_lg();
                for (index, command) in self.known_commands().iter().enumerate() {
                    let command = command.clone();
                    let picked = state.kb_command_draft.trim() == command;
                    picker = picker.child(
                        div()
                            .id(("kb-cmd-pick", index))
                            .px_2()
                            .py_1()
                            .text_xs()
                            .cursor_pointer()
                            .when(picked, |s| {
                                s.bg(theme::selected()).text_color(theme::primary())
                            })
                            .when(!picked, |s| {
                                s.text_color(theme::fg()).hover(|s| s.bg(theme::hover()))
                            })
                            .child(command.clone())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, window, cx| {
                                    this.pick_kb_command(command.clone(), window, cx);
                                }),
                            ),
                    );
                }
                form = form.child(picker);
            }

            // ── Shortcut + keystroke capture ────────────────────────────
            if let Some(field) = state.kb_key_field.clone() {
                form = form.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::fg_muted())
                                        .child("Shortcut"),
                                )
                                .child(
                                    div()
                                        .id("kb-capture-toggle")
                                        .cursor_pointer()
                                        .text_xs()
                                        .text_color(theme::primary())
                                        .hover(|s| s.text_color(theme::fg()))
                                        .child(if state.kb_capturing {
                                            "recording… press keys (esc to stop)"
                                        } else {
                                            "record keys"
                                        })
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _ev, window, cx| {
                                                if this.misc_settings_state.extra.kb_capturing {
                                                    this.misc_settings_state.extra.kb_capturing =
                                                        false;
                                                    cx.notify();
                                                } else {
                                                    this.start_kb_capture(window, cx);
                                                }
                                            }),
                                        ),
                                ),
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
            if state.kb_capturing {
                // Focused capture box: the next KeyDownEvent fills the
                // shortcut field in `mod+shift+x` shape.
                let capture = div()
                    .id("kb-capture-box")
                    .track_focus(&state.kb_capture_focus.clone().unwrap_or_else(|| cx.focus_handle()))
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::primary())
                    .bg(theme::selected())
                    .text_xs()
                    .text_color(theme::fg())
                    .child("Press the keys for this rule — e.g. Ctrl+Shift+K records as “mod+shift+k”.")
                    .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                        if ev.keystroke.key == "escape" {
                            this.misc_settings_state.extra.kb_capturing = false;
                            cx.notify();
                        } else {
                            this.capture_keystroke(ev, window, cx);
                        }
                    }));
                form = form.child(capture);
            }

            // ── When (optional) ─────────────────────────────────────────
            if let Some(field) = state.kb_when_field.clone() {
                form = form.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child("When (optional)"),
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

            // Conflict warning when the draft key is already bound.
            let draft_key = state.kb_key_draft.trim();
            if !draft_key.is_empty() {
                let taken: Vec<String> = state
                    .kb_bindings
                    .iter()
                    .filter(|r| r["key"].as_str() == Some(draft_key))
                    .filter_map(|r| r["command"].as_str().map(str::to_string))
                    .collect();
                if !taken.is_empty() {
                    form = form.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .py_1()
                            .text_xs()
                            .text_color(danger())
                            .child(icon(Icon::TriangleAlert, 11.).text_color(danger()))
                            .child(format!(
                                "“{draft_key}” is already bound to {} — saving adds another rule for the same keys.",
                                taken.join(", ")
                            )),
                    );
                }
            }

            let ok = !state.kb_command_draft.trim().is_empty()
                && !state.kb_key_draft.trim().is_empty()
                && !state.kb_saving;
            let mut save_btn = div()
                .id("kb-save")
                .px_3()
                .py_1p5()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .bg(theme::primary())
                .text_color(gpui::white())
                .child(if state.kb_saving {
                    "Saving…"
                } else {
                    "Save rule"
                });
            if !ok {
                save_btn = save_btn.opacity(0.4);
            }
            save_btn = save_btn.on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| {
                    this.save_keybinding_click(window, cx);
                }),
            );
            form = form.child(
                div().flex().items_center().gap_2().child(save_btn).child(
                    div()
                        .id("kb-cancel")
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
                                this.misc_settings_state.extra.kb_add_open = false;
                                cx.notify();
                            }),
                        ),
                ),
            );
            section = section.child(group_label("Add rule")).child(form);
        } else {
            section = section.child(
                div().pt_3().flex().child(
                    div()
                        .id("kb-add-open")
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .bg(theme::hover())
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::selected()))
                        .child("+ Add rule")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, window, cx| {
                                this.open_kb_add_form(window, cx);
                            }),
                        ),
                ),
            );
        }

        section = section.child(
            div()
                .pt_3()
                .text_xs()
                .text_color(theme::fg_muted())
                .child("Rules are matched on their full {key, command, when?} identity — editing a rule means removing it and adding the replacement. The web also offers a one-click reset to server defaults."),
        );
        section
    }

    // ─── Source Control ─────────────────────────────────────────────────

    /// GitHub device-flow sign-in: `sourceControl.signInStart` returns the
    /// `SourceControlSignInPrompt` (userCode + verificationUri), the browser
    /// opens the URI, and `signInWait` resolves when the user approves the
    /// code — exactly the web `SourceControlAppSignInSettings` flow.
    fn source_control_sign_in(&mut self, cx: &mut Context<Self>) {
        // Provider/host come from the discovered GitHub item; the kind is
        // `github` and `auth.host` covers enterprise hosts when set.
        let host = self.github_provider_item()["auth"]["host"]
            .as_str()
            .unwrap_or("github.com")
            .to_string();
        let input = json!({ "provider": "github", "host": host });
        let Some(rx) = self.send_query("sourceControl.signInStart", input.clone()) else {
            self.misc_settings_state.extra.sc_note = Some("Not connected to a server.".into());
            cx.notify();
            return;
        };
        let state = &mut self.misc_settings_state.extra;
        state.sc_busy = true;
        state.sc_note = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.misc_settings_state.extra;
                state.sc_busy = false;
                match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        let code = exit["value"]["userCode"].as_str().unwrap_or("").to_string();
                        let uri = exit["value"]["verificationUri"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        state.sc_signin_prompt = Some((code, uri.clone()));
                        if !uri.is_empty() {
                            let _ = std::process::Command::new("xdg-open").arg(&uri).spawn();
                        }
                        this.source_control_sign_in_wait(input, cx);
                    }
                    Ok(exit) => {
                        state.sc_note = Some(exit_failure_message(&exit));
                    }
                    Err(_) => {
                        state.sc_note = Some("Lost connection before the reply arrived.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `sourceControl.signInWait` — resolves when the device-flow sign-in
    /// finishes however long the user takes; the result refreshes the
    /// discovery rows.
    fn source_control_sign_in_wait(&mut self, input: Value, cx: &mut Context<Self>) {
        let Some(rx) = self.send_query("sourceControl.signInWait", input) else {
            self.misc_settings_state.extra.sc_note = Some("Not connected to a server.".into());
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.misc_settings_state.extra;
                state.sc_signin_prompt = None;
                match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        state.sc_note = Some("Signed in.".into());
                    }
                    Ok(exit) => {
                        state.sc_note = Some(exit_failure_message(&exit));
                    }
                    Err(_) => {
                        state.sc_note = Some("Lost connection before the reply arrived.".into());
                    }
                }
                this.rescan_source_control(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Cancel a pending device-flow sign-in (`sourceControl.signInCancel`).
    fn source_control_sign_in_cancel(&mut self, cx: &mut Context<Self>) {
        let host = self.github_provider_item()["auth"]["host"]
            .as_str()
            .unwrap_or("github.com")
            .to_string();
        let _ = self.send_query(
            "sourceControl.signInCancel",
            json!({ "provider": "github", "host": host }),
        );
        self.misc_settings_state.extra.sc_signin_prompt = None;
        cx.notify();
    }

    /// The `sourceControlProviders` entry for GitHub (empty object when the
    /// server reported none).
    fn github_provider_item(&self) -> Value {
        self.misc_settings_state
            .extra
            .sc_discovery
            .as_ref()
            .and_then(|discovery| discovery["sourceControlProviders"].as_array())
            .and_then(|items| {
                items
                    .iter()
                    .find(|i| i["kind"].as_str() == Some("github"))
                    .cloned()
            })
            .unwrap_or(Value::Null)
    }

    pub(crate) fn render_source_control_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_source_control(cx);
        self.maybe_load_misc_settings(cx);
        self.maybe_prepare_extra(cx);
        let state = &self.misc_settings_state.extra;
        let connected = self.commands.is_some();
        let mut section = div().flex().flex_col();

        if !connected {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Connect to a server to see source control providers."),
            );
            return section;
        }

        // ── Discovery (`server.discoverSourceControl`) ──────────────────
        let discovery = state.sc_discovery.clone().unwrap_or(Value::Null);
        section = section.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(group_label("Version control — tools on the server")),
                )
                .child(
                    div()
                        .id("sc-rescan")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .bg(theme::hover())
                        .text_color(theme::fg_muted())
                        .hover(|s| s.bg(theme::selected()))
                        .child(if state.sc_disc_loading.get() {
                            "Scanning…"
                        } else {
                            "Rescan"
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _window, cx| {
                                this.rescan_source_control(cx);
                            }),
                        ),
                ),
        );

        if state.sc_disc_loading.get() && discovery.is_null() {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Scanning the server for git / jj / CLIs…"),
            );
        } else if discovery.is_null() {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No discovery result yet — Rescan to check the server's tools."),
            );
        }

        // VcsDiscoveryItem rows: label, version+executable when available,
        // installHint when missing. `implemented:false` kinds (e.g. jj on a
        // git-only server) get a muted "not supported" tag instead.
        if let Some(vcs) = discovery["versionControlSystems"].as_array() {
            for item in vcs {
                let label = item["label"].as_str().unwrap_or("tool").to_string();
                let available = item["status"].as_str() == Some("available");
                let implemented = item["implemented"].as_bool() != Some(false);
                let detail = if available {
                    let version = item["version"].as_str().unwrap_or_default();
                    let exe = item["executable"].as_str().unwrap_or_default();
                    if exe.is_empty() {
                        version.to_string()
                    } else {
                        format!("{version} · {exe}")
                    }
                } else {
                    item["installHint"]
                        .as_str()
                        .unwrap_or("Not installed on the server.")
                        .to_string()
                };
                section = section.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .py_1p5()
                        .text_sm()
                        .child(div().w(px(8.)).h(px(8.)).flex_none().rounded_full().bg(
                            if available {
                                gpui::rgb(0x34d399)
                            } else {
                                theme::border()
                            },
                        ))
                        .child(
                            div()
                                .w(px(120.))
                                .flex_none()
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
                                .child(detail),
                        )
                        .when(!implemented, |s| {
                            s.child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme::fg_muted())
                                    .child("not supported"),
                            )
                        }),
                );
            }
        }

        // SourceControlProviderDiscoveryItem rows: label + auth state
        // (authenticated account / unauthenticated / unknown) + installHint
        // when the CLI is missing.
        let providers = discovery["sourceControlProviders"].as_array().cloned();
        if let Some(providers) = providers.filter(|p| !p.is_empty()) {
            section = section.child(group_label("Source control providers"));
            for item in providers {
                let label = item["label"].as_str().unwrap_or("provider").to_string();
                let available = item["status"].as_str() == Some("available");
                let auth_status = item["auth"]["status"].as_str().unwrap_or("unknown");
                let account = item["auth"]["account"].as_str();
                let host = item["auth"]["host"].as_str();
                let signed_in = auth_status == "authenticated";
                let mut detail = if !available {
                    item["installHint"]
                        .as_str()
                        .unwrap_or("CLI not installed on the server.")
                        .to_string()
                } else {
                    match (signed_in, account) {
                        (true, Some(account)) => match host {
                            Some(host) => format!("Signed in as {account} on {host}"),
                            None => format!("Signed in as {account}"),
                        },
                        _ => "Not signed in".to_string(),
                    }
                };
                if let Some(extra) = item["detail"].as_str() {
                    detail = format!("{detail} · {extra}");
                }
                let is_github = item["kind"].as_str() == Some("github");
                let mut row =
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .py_1p5()
                        .text_sm()
                        .child(div().w(px(8.)).h(px(8.)).flex_none().rounded_full().bg(
                            if signed_in {
                                gpui::rgb(0x34d399)
                            } else {
                                theme::border()
                            },
                        ))
                        .child(
                            div()
                                .w(px(120.))
                                .flex_none()
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
                                .child(detail),
                        );
                // GitHub device-flow sign-in (appSignIn-capable providers —
                // the web gates the button on `appSignIn === true`).
                if is_github && available && !signed_in {
                    let mut sign_in_btn = div()
                        .id("sc-sign-in")
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .bg(theme::hover())
                        .text_color(theme::fg())
                        .child(if state.sc_busy {
                            "Signing in…"
                        } else {
                            "Sign in"
                        });
                    if state.sc_busy {
                        sign_in_btn = sign_in_btn.opacity(0.4);
                    }
                    sign_in_btn = sign_in_btn.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            this.source_control_sign_in(cx);
                        }),
                    );
                    row = row.child(sign_in_btn);
                }
                section = section.child(row);
            }
        } else if !discovery.is_null() {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No source control providers detected on the server."),
            );
        }

        // Device-flow prompt while `signInWait` runs.
        if let Some((code, uri)) = &state.sc_signin_prompt {
            section = section.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_1p5()
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
                                    .child(format!("Enter code {code}")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::fg_muted())
                                    .truncate()
                                    .child(uri.clone()),
                            ),
                    )
                    .child(
                        div()
                            .id("sc-signin-cancel")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .bg(theme::hover())
                            .text_color(theme::fg_muted())
                            .hover(|s| s.bg(theme::selected()))
                            .child("Cancel")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    this.source_control_sign_in_cancel(cx);
                                }),
                            ),
                    ),
            );
        }

        if let Some(note) = &state.sc_note {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(note.clone()),
            );
        }

        // ── Writing (ServerSettings) ────────────────────────────────────
        if let Some(error) = &self.misc_settings_state.settings_error {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(error.clone()),
            );
        }
        section = section.child(self.render_scope_selector(cx));
        section = section.child(group_label("Commit & pull request writing"));

        // Scoped keys — the mode/model write into projectSettingsOverrides
        // when the scope selector picks a project.
        let style = self.scoped_value("sourceControlWritingStyle");
        let mode = style["mode"]
            .as_str()
            .unwrap_or("repo_conventions")
            .to_string();
        let mode_label = WRITING_MODES
            .iter()
            .find(|(value, _, _)| *value == mode)
            .map(|(_, label, _)| *label)
            .unwrap_or(mode.as_str());
        section = section.child(
            self.scoped_select_setting(
                "Writing style",
                "How generated commit messages and pull request text are shaped.",
                OpenList::ScWritingMode,
                "sourceControlWritingStyle",
                mode_label.into(),
                &mode,
                WRITING_MODES
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
        if mode == "custom" {
            if let Some(field) = state.sc_instructions_field.clone() {
                section = section.child(setting_row(
                    "Custom instructions",
                    "Enter saves. Applied to generated commit messages and PR text.",
                    div()
                        .w(px(220.))
                        .px_2()
                        .py_1()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme::input())
                        .bg(theme::field_surface())
                        .text_sm()
                        .text_color(theme::fg())
                        .child(field),
                ));
            }
        }

        // Writer model — same ModelSelection shape as Chat's defaults
        // (scoped: nullable override).
        let writer_selection = self.scoped_value("sourceControlWriterModelSelection");
        let writer_selection = &writer_selection;
        let writer_value = if writer_selection.is_null() {
            AUTO_VALUE.to_string()
        } else {
            pack_model(
                writer_selection["instanceId"]
                    .as_str()
                    .or_else(|| writer_selection["provider"].as_str())
                    .unwrap_or_default(),
                writer_selection["model"].as_str().unwrap_or_default(),
            )
        };
        let writer_label: SharedString = if writer_selection.is_null() {
            "Automatic".into()
        } else {
            Self::selection_label(writer_selection).into()
        };
        let mut writer_options = vec![(
            AUTO_VALUE.to_string(),
            "Automatic".to_string(),
            Some("Follows the text generation model.".to_string()),
        )];
        for (instance, model, label) in MODEL_CATALOG {
            writer_options.push((
                pack_model(instance, model),
                (*label).to_string(),
                Some(format!("{instance}/{model}")),
            ));
        }
        section = section.child(self.scoped_select_setting(
            "Writing model",
            "Model used for generated source control text.",
            OpenList::ScWriterModel,
            "sourceControlWriterModelSelection",
            writer_label,
            &writer_value,
            writer_options,
            cx,
        ));

        section
    }

    // ─── Threads (web Threads + Diffs + Worktrees panels) ───────────────

    pub(crate) fn render_threads_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_misc_settings(cx);
        self.maybe_prepare_extra(cx);
        let settings = &self.misc_settings_state.settings;
        let connected = self.commands.is_some();
        let mut section = div().flex().flex_col();

        if let Some(error) = &self.misc_settings_state.settings_error {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(error.clone()),
            );
        }

        // ── Diffs (client-persisted — the web's ClientSettings) ─────────
        section = section.child(group_label("Diffs"));
        let layout = self.client_config.diff_layout.clone();
        let layout_label = if layout == "split" {
            "Split"
        } else {
            "Stacked"
        };
        section = section.child(self.select_setting(
            "Diff layout",
            "Show diffs stacked or side by side.",
            OpenList::DiffLayout,
            layout_label.into(),
            &layout,
            vec![
                ("stacked".to_string(), "Stacked".to_string(), None),
                ("split".to_string(), "Split".to_string(), None),
            ],
            cx,
        ));
        let ignore_ws = self.client_config.diff_ignore_whitespace;
        section = section.child(setting_row(
            "Hide whitespace changes",
            "Diffs ignore whitespace-only edits by default.",
            div()
                .id("threads-ignore-ws")
                .cursor_pointer()
                .child(switch_control(ignore_ws))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        this.client_config.diff_ignore_whitespace = !ignore_ws;
                        this.client_config.save();
                        cx.notify();
                    }),
                ),
        ));
        let collapsed = self.client_config.diff_files_collapsed;
        section = section.child(self.select_setting(
            "Default file state",
            "Start with files expanded or collapsed when opening diffs.",
            OpenList::DiffFiles,
            if collapsed { "Collapsed" } else { "Expanded" }.into(),
            if collapsed { "collapsed" } else { "expanded" },
            vec![
                ("expanded".to_string(), "Expanded".to_string(), None),
                ("collapsed".to_string(), "Collapsed".to_string(), None),
            ],
            cx,
        ));
        let proactive = self.client_config.proactive_panels;
        section = section.child(setting_row(
            "Proactive panels",
            "Open linked pull requests first; otherwise open Changes for edits to at least 3 files or 50 lines.",
            div()
                .id("threads-proactive")
                .cursor_pointer()
                .child(switch_control(proactive))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        this.client_config.proactive_panels = !proactive;
                        this.client_config.save();
                        cx.notify();
                    }),
                ),
        ));

        if connected {
            // Scoped rows below write `projectSettingsOverrides[project]`
            // when a project is selected (PROJECT_SCOPED_SERVER_SETTING_KEYS).
            section = section.child(self.render_scope_selector(cx));

            // ── New projects (ServerSettings) ────────────────────────────
            section = section.child(group_label("New projects"));
            let base_dir_field = self.misc_settings_state.extra.base_dir_field.clone();
            if let Some(field) = base_dir_field {
                section = section.child(setting_row(
                    "Add project starts in",
                    "Base folder for the project browser. Enter saves; empty means home.",
                    div()
                        .w(px(200.))
                        .px_2()
                        .py_1()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme::input())
                        .bg(theme::field_surface())
                        .text_sm()
                        .text_color(theme::fg())
                        .child(field),
                ));
            }
            let ghq = settings["ghqCloneLayout"].as_bool() == Some(true);
            section = section.child(setting_row(
                "Clone into host folders",
                "Clones go to github.com/owner/repo under the folder above, like ghq.",
                div()
                    .id("threads-ghq")
                    .cursor_pointer()
                    .child(switch_control(ghq))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.update_server_settings(json!({ "ghqCloneLayout": !ghq }), cx);
                        }),
                    ),
            ));

            // ── Worktrees (scoped ServerSettings) ────────────────────────
            section = section.child(group_label("Worktrees"));
            section = section.child(self.scoped_switch_row(
                "Start new worktrees from remote",
                "Use the base branch's tracked remote branch instead of the local one.",
                "newWorktreesStartFromOrigin",
                "threads-wt-origin",
                cx,
            ));
            let submodules = self.scoped_value("worktreeSubmodules");
            let submodules = submodules.as_str();
            let submodules_value = submodules.unwrap_or("inherit");
            let submodules_label = match submodules {
                None => "Inherit",
                Some("recursive") => "Recursive",
                Some("top-level") => "Top level only",
                Some("none") => "None",
                Some(other) => other,
            };
            let mut submodule_options = vec![(
                "inherit".to_string(),
                "Inherit".to_string(),
                Some("Follow the project's t3.json.".to_string()),
            )];
            for (value, label) in SUBMODULE_MODES {
                submodule_options.push((value.to_string(), label.to_string(), None));
            }
            section = section.child(self.scoped_select_setting(
                "Submodules",
                "How new worktrees check out git submodules.",
                OpenList::WorktreeSubmodules,
                "worktreeSubmodules",
                submodules_label.into(),
                submodules_value,
                submodule_options,
                cx,
            ));

            // ── Usage limits (ServerSettings) ────────────────────────────
            section = section.child(group_label("Usage limits"));
            let auto_resume = settings["autoResumeLimitedThreads"].as_bool() == Some(true);
            section = section.child(setting_row(
                "Resume after usage limits",
                "Automatically resume threads when their usage limits reset.",
                div()
                    .id("threads-auto-resume")
                    .cursor_pointer()
                    .child(switch_control(auto_resume))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.update_server_settings(
                                json!({ "autoResumeLimitedThreads": !auto_resume }),
                                cx,
                            );
                        }),
                    ),
            ));
            let snooze_limited = settings["snoozeLimitedThreads"].as_bool() == Some(true);
            section = section.child(setting_row(
                "Snooze until limits reset",
                "Snooze threads until their usage limits reset. Enable auto-resume to continue the agent after the reset.",
                div()
                    .id("threads-snooze-limited")
                    .cursor_pointer()
                    .child(switch_control(snooze_limited))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.update_server_settings(
                                json!({ "snoozeLimitedThreads": !snooze_limited }),
                                cx,
                            );
                        }),
                    ),
            ));
            // Cost/pricing metadata from the composer's `getUsageSummary`
            // fetch — costUsd + unpricedRecords per bucket plus the pricing
            // status, shown only when the server reported them.
            if let Some(pricing_line) = self.usage_pricing_line() {
                section = section.child(
                    div()
                        .py_1p5()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(pricing_line),
                );
            }

            // ── Auto-settle (scoped ServerSettings) ──────────────────────
            section = section.child(group_label("Auto-settle"));
            section = section.child(self.scoped_switch_row(
                "Settle merged threads",
                "Settle a thread when its pull request merges. Closed pull requests still settle automatically.",
                "sidebarAutoSettleOnMerge",
                "threads-settle-merge",
                cx,
            ));
            let settle_days = self.scoped_value("sidebarAutoSettleAfterDays").as_i64();
            let days_value = settle_days
                .map(|d| d.to_string())
                .unwrap_or_else(|| "off".into());
            let days_label = AUTO_SETTLE_DAYS
                .iter()
                .find(|(v, _)| *v == days_value)
                .map(|(_, label)| *label)
                .unwrap_or("Off");
            section = section.child(
                self.scoped_select_setting(
                    "Settle inactive threads",
                    "Sidebar threads with no activity for this long settle automatically.",
                    OpenList::AutoSettleDays,
                    "sidebarAutoSettleAfterDays",
                    days_label.into(),
                    &days_value,
                    AUTO_SETTLE_DAYS
                        .iter()
                        .map(|(value, label)| (value.to_string(), label.to_string(), None))
                        .collect(),
                    cx,
                ),
            );
            if settle_days.is_some() {
                section = section.child(self.scoped_switch_row(
                    "Keep threads with a PR",
                    "Inactivity does not settle threads with a pull request. Merge and close still do.",
                    "sidebarAutoSettleKeepPullRequests",
                    "threads-keep-prs",
                    cx,
                ));
            }

            // ── Text generation (scoped ServerSettings) ──────────────────
            section = section.child(group_label("Text generation"));
            let textgen_selection = self.scoped_value("textGenerationModelSelection");
            let textgen_selection = &textgen_selection;
            let textgen_value = if textgen_selection.is_null() {
                AUTO_VALUE.to_string()
            } else {
                pack_model(
                    textgen_selection["instanceId"]
                        .as_str()
                        .or_else(|| textgen_selection["provider"].as_str())
                        .unwrap_or_default(),
                    textgen_selection["model"].as_str().unwrap_or_default(),
                )
            };
            let textgen_label: SharedString = if textgen_selection.is_null() {
                "Automatic".into()
            } else {
                Self::selection_label(textgen_selection).into()
            };
            let mut textgen_options = vec![(
                AUTO_VALUE.to_string(),
                "Automatic".to_string(),
                Some("The provider's default text model.".to_string()),
            )];
            for (instance, model, label) in MODEL_CATALOG {
                textgen_options.push((
                    pack_model(instance, model),
                    (*label).to_string(),
                    Some(format!("{instance}/{model}")),
                ));
            }
            section = section.child(self.scoped_select_setting(
                "Text generation model",
                "For thread titles and other generated text.",
                OpenList::TextGenModel,
                "textGenerationModelSelection",
                textgen_label,
                &textgen_value,
                textgen_options,
                cx,
            ));
            if let Some(field) = self
                .misc_settings_state
                .extra
                .title_instructions_field
                .clone()
            {
                let mut control = div().flex().items_center().gap_2();
                if let Some(chip) = self.scope_chip("threadTitleInstructions", cx) {
                    control = control.child(chip);
                }
                control = control.child(
                    div()
                        .w(px(220.))
                        .px_2()
                        .py_1()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme::input())
                        .bg(theme::field_surface())
                        .text_sm()
                        .text_color(theme::fg())
                        .child(field),
                );
                section = section.child(setting_row(
                    "Thread title instructions",
                    "Extra guidance appended when titles are generated. Enter saves; empty clears.",
                    control,
                ));
            }
            let streaming = self
                .scoped_value("responseStreamingMode")
                .as_str()
                .unwrap_or("turn")
                .to_string();
            let streaming_label = STREAMING_MODES
                .iter()
                .find(|(v, _, _)| *v == streaming)
                .map(|(_, label, _)| *label)
                .unwrap_or("After each turn");
            section = section.child(
                self.scoped_select_setting(
                    "Response streaming",
                    "How agent replies appear while a turn is running.",
                    OpenList::ResponseStreaming,
                    "responseStreamingMode",
                    streaming_label.into(),
                    &streaming,
                    STREAMING_MODES
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

            // ── Agent access (scoped ServerSettings) ─────────────────────
            // These are real server keys — they gate what *agents* may do on
            // the server machine, not a native browser/voice feature.
            section = section.child(group_label("Agent access"));
            section = section.child(self.scoped_switch_row(
                "Agent browser access",
                "Let agents drive the server-side browser tool (page fetch/automation). Not a built-in web view.",
                "enableAgentBrowserAccess",
                "threads-agent-browser",
                cx,
            ));
            section = section.child(self.scoped_switch_row(
                "Agent device access",
                "Let agents use device/inputs capabilities the server exposes (e.g. desktop control).",
                "enableAgentDeviceAccess",
                "threads-agent-device",
                cx,
            ));
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("The web's Browser panel embeds a page view — the native app has no embedded browser, so there is nothing to toggle here beyond the agent switches above."),
            );

            // ── About this server ────────────────────────────────────────
            section = section.child(group_label("Server"));
            let version = self
                .misc_settings_state
                .server_version
                .clone()
                .unwrap_or_else(|| "unknown".into());
            section = section.child(setting_row(
                "Server version",
                "Reported by server.getConfig on this environment.",
                div().text_sm().text_color(theme::fg_muted()).child(version),
            ));
            // Secret providers live in `settings.secretProviders`; pending
            // `secret_request`s are answered from chat. There is no client
            // provider-management RPC — say so instead of faking controls.
            let providers = settings["secretProviders"]
                .as_object()
                .map_or(0, |p| p.len());
            section = section.child(setting_row(
                "Secret providers",
                "Configured on the server; pending requests are answered inline in chat.",
                div()
                    .text_sm()
                    .text_color(theme::fg_muted())
                    .child(if providers == 0 {
                        "managed by server".to_string()
                    } else {
                        format!("{providers} configured · managed by server")
                    }),
            ));
            let update_checking = self.misc_settings_state.extra.update_checking;
            let update_label = if update_checking {
                "Checking…"
            } else {
                "Check for updates"
            };
            let mut control = div()
                .id("threads-check-update")
                .px_3()
                .py_1p5()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .bg(theme::hover())
                .text_color(theme::fg())
                .hover(|s| s.bg(theme::selected()))
                .child(update_label);
            if update_checking {
                control = control.opacity(0.4);
            }
            control = control.on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, _window, cx| {
                    this.check_server_update(cx);
                }),
            );
            section = section.child(setting_row(
                "Updates",
                "Ask the npm registry for the latest t3 release.",
                control,
            ));
            if let Some(status) = &self.misc_settings_state.extra.update_status {
                let status = status.clone();
                let upgrade_link = status.strip_prefix("LATEST:");
                section = section.child(
                    div()
                        .py_1p5()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child(
                                    upgrade_link
                                        .map(str::to_string)
                                        .unwrap_or_else(|| status.clone()),
                                ),
                        )
                        .when_some(upgrade_link.map(str::to_string), |s, _| {
                            s.child(
                                div()
                                    .id("threads-open-npm")
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .cursor_pointer()
                                    .bg(theme::hover())
                                    .text_color(theme::primary())
                                    .hover(|s| s.bg(theme::selected()))
                                    .child("npm")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|_this, _ev, _window, _cx| {
                                            let _ = std::process::Command::new("xdg-open")
                                                .arg("https://www.npmjs.com/package/t3")
                                                .spawn();
                                        }),
                                    ),
                            )
                        }),
                );
            }
        }

        section = section.child(div().pt_3().text_xs().text_color(theme::fg_muted()).child(
            "Voice dictation lives in the composer — there is no \
             standalone voice panel. Secret requests are answered inline in chat; providers \
             are managed on the server — no client-side provider RPC exists yet. Licenses \
             ship with the app and are listed in the web client's About page.",
        ));
        section
    }

    /// "Today's usage: $X across N priced records, M unpriced" — built from
    /// the composer's `getUsageSummary` payload (costUsd / unpricedRecords /
    /// pricing.status / knownModels). `None` when nothing has been fetched.
    fn usage_pricing_line(&self) -> Option<String> {
        let summary = self.composer_state.usage_summary.as_ref()?;
        let mut cost = 0f64;
        let mut cost_sources = std::collections::HashSet::new();
        let mut unpriced = 0u64;
        for bucket in summary["buckets"].as_array().into_iter().flatten() {
            cost += bucket["costUsd"].as_f64().unwrap_or(0.);
            if let Some(source) = bucket["costSource"].as_str() {
                cost_sources.insert(source.to_string());
            }
            unpriced += bucket["unpricedRecords"].as_u64().unwrap_or(0);
        }
        let pricing = &summary["pricing"];
        let status = pricing["status"].as_str().unwrap_or("unknown");
        let source = pricing["source"].as_str().unwrap_or_default();
        let known = pricing["knownModels"].as_u64().unwrap_or(0);
        if cost == 0. && unpriced == 0 && status.is_empty() {
            return None;
        }
        Some(format!(
            "Today: ${cost:.2} · {unpriced} unpriced records · pricing {status} ({source}, {known} models known{})",
            if cost_sources.is_empty() {
                String::new()
            } else {
                format!(
                    ", cost via {}",
                    cost_sources.into_iter().collect::<Vec<_>>().join("/")
                )
            }
        ))
    }

    /// "Check for updates" — `server.getNpmRelease {spec:"latest"}` and
    /// compare against `environment.serverVersion`.
    fn check_server_update(&mut self, cx: &mut Context<Self>) {
        if self.misc_settings_state.extra.update_checking {
            return;
        }
        let Some(rx) = self.send_query("server.getNpmRelease", json!({ "spec": "latest" })) else {
            self.misc_settings_state.extra.update_status =
                Some("Not connected to a server.".into());
            cx.notify();
            return;
        };
        let state = &mut self.misc_settings_state.extra;
        state.update_checking = true;
        state.update_status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.misc_settings_state.extra;
                state.update_checking = false;
                match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        let latest = exit["value"]["version"].as_str().map(str::to_string);
                        let current = this.misc_settings_state.server_version.clone();
                        state.update_status = Some(match (latest, current) {
                            (Some(latest), Some(current))
                                if latest.trim_start_matches('v')
                                    == current.trim_start_matches('v') =>
                            {
                                format!("v{current} — up to date")
                            }
                            (Some(latest), Some(current)) => {
                                // The "LATEST:" prefix keeps the npm-link
                                // button attached to update-available lines.
                                format!("LATEST:latest v{latest} — update available (you run v{current})")
                            }
                            (Some(latest), None) => {
                                format!("LATEST:latest v{latest} — update available")
                            }
                            (None, _) => "No release info from the registry.".into(),
                        });
                    }
                    Ok(exit) => {
                        state.update_status = Some(exit_failure_message(&exit));
                    }
                    Err(_) => {
                        state.update_status =
                            Some("Lost connection before the reply arrived.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
