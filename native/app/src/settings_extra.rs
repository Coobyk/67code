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
//! - **Threads** mirrors the web `/settings/threads` page: a scope
//!   sentence + selector, "New projects" (base directory, ghq layout),
//!   "New threads" (model, permissions, workspace, fallback, auto-pull),
//!   editable "Snooze" presets, "Usage limits", "Auto-settle", and "Text
//!   generation" — the scoped keys write `projectSettingsOverrides` when a
//!   project is picked.
//! - The remaining canonical pages live here too: **Chat** / **Diffs &
//!   pull requests** / **Voice** / **SnapShots** (client-local
//!   `ClientConfig` mirrors of the web's ClientSettings), **Browser** /
//!   **Devices** (agent access + link prefs, with honest notes where the
//!   native client has no embedded surface), **Worktrees**, **Labs**,
//!   **Maintenance** (updates, background activity, plus the native
//!   Diagnostics and Usage folds), and **Providers** (instance
//!   list/config/env, usage providers, text generators, `idleCompaction`,
//!   and the ACP sessions fold).

use std::cell::Cell;
use std::collections::HashMap;

use gpui::{
    Context, Div, Entity, EntityInputHandler, FocusHandle, Focusable, MouseButton, SharedString,
    Subscription, Window, deferred, div, prelude::*, px,
};
use serde_json::{Value, json};

use crate::AppView;
use crate::composer::MODEL_CATALOG;
use crate::icons::{Icon, icon};
use crate::settings_misc::{
    self, AUTO_VALUE, OpenList, danger, edit_field, exit_failure_message, field_text_in,
    group_label, pack_model, setting_row, settings_card, settings_section_title, switch_control,
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

// ─── Snooze presets (`snoozePresets`) ───────────────────────────────────────

/// `MAX_SNOOZE_PRESETS` from packages/contracts/src/settings.ts.
const MAX_SNOOZE_PRESETS: usize = 8;

/// `WHEN_LABELS` — `SnoozePresetsSettings.tsx`.
const SNOOZE_WHEN: &[(&str, &str)] = &[
    ("after", "In"),
    ("today", "Today"),
    ("tomorrow", "Tomorrow"),
    ("monday", "Monday"),
    ("tuesday", "Tuesday"),
    ("wednesday", "Wednesday"),
    ("thursday", "Thursday"),
    ("friday", "Friday"),
    ("saturday", "Saturday"),
    ("sunday", "Sunday"),
];

/// Curated `after` durations (minutes → label) — the web's number+unit
/// fields collapse into one select natively.
const SNOOZE_DURATIONS: &[(u64, &str)] = &[
    (15, "15 minutes"),
    (30, "30 minutes"),
    (60, "1 hour"),
    (120, "2 hours"),
    (180, "3 hours"),
    (240, "4 hours"),
    (480, "8 hours"),
    (720, "12 hours"),
    (1440, "1 day"),
    (2880, "2 days"),
    (4320, "3 days"),
    (10080, "7 days"),
];

/// Hourly `at` times — the web's `HH:MM` input collapses into a select.
const SNOOZE_TIMES: &[&str] = &[
    "06:00", "07:00", "08:00", "09:00", "10:00", "11:00", "12:00", "13:00", "14:00", "15:00",
    "16:00", "17:00", "18:00", "19:00", "20:00", "21:00", "22:00", "23:00", "00:00",
];

/// `DEFAULT_SNOOZE_PRESETS` from packages/contracts/src/settings.ts.
fn default_snooze_presets() -> Vec<Value> {
    vec![
        json!({ "kind": "after", "minutes": 60 }),
        json!({ "kind": "after", "minutes": 180 }),
        json!({ "kind": "at", "day": "today", "time": "18:00" }),
        json!({ "kind": "at", "day": "tomorrow", "time": "09:00" }),
        json!({ "kind": "at", "day": "monday", "time": "09:00" }),
    ]
}

/// Human label for an `after` duration ("90 minutes" → "1.5 hours" stays
/// minute-precise: "90 minutes" — `splitMinutes` picks whole units only).
fn format_minutes(minutes: u64) -> String {
    if minutes % 1440 == 0 {
        let days = minutes / 1440;
        if days == 1 { "1 day".into() } else { format!("{days} days") }
    } else if minutes % 60 == 0 {
        let hours = minutes / 60;
        if hours == 1 {
            "1 hour".into()
        } else {
            format!("{hours} hours")
        }
    } else {
        format!("{minutes} minutes")
    }
}

/// `nextCandidate` — the first `ADD_CANDIDATES` preset whose identity isn't
/// already in the list (defaults first, then the web's two extras).
fn next_snooze_candidate(presets: &[Value]) -> Option<Value> {
    let identity = |preset: &Value| -> String {
        if preset["kind"].as_str() == Some("at") {
            format!(
                "at:{}:{}",
                preset["day"].as_str().unwrap_or_default(),
                preset["time"].as_str().unwrap_or_default()
            )
        } else {
            format!("after:{}", preset["minutes"].as_u64().unwrap_or_default())
        }
    };
    let mut candidates = default_snooze_presets();
    candidates.push(json!({ "kind": "after", "minutes": 30 }));
    candidates.push(json!({ "kind": "at", "day": "saturday", "time": "09:00" }));
    for day in ["tuesday", "wednesday", "thursday", "friday", "sunday"] {
        candidates.push(json!({ "kind": "at", "day": day, "time": "09:00" }));
    }
    let used: Vec<String> = presets.iter().map(identity).collect();
    if let Some(candidate) = candidates
        .into_iter()
        .find(|candidate| !used.contains(&identity(candidate)))
    {
        return Some(candidate);
    }
    // Everything canonical is taken — append a longer `after` than any
    // row already offers.
    let longest = presets
        .iter()
        .filter(|preset| preset["kind"].as_str() == Some("after"))
        .filter_map(|preset| preset["minutes"].as_u64())
        .max()
        .unwrap_or(0);
    Some(json!({ "kind": "after", "minutes": (longest + 60).min(43200) }))
}

// ─── Client-local option tables (web ClientSettings literals) ───────────────

/// `sendShortcut` options — `sendShortcutOptions` in `ChatSettingsPanel`
/// ("Ctrl" — the native client ships on non-Apple platforms).
const SEND_SHORTCUTS: &[(&str, &str)] = &[
    ("enter", "Enter"),
    (
        "mod-enter-multiline",
        "Ctrl + Enter for multiline prompts",
    ),
    ("mod-enter", "Ctrl + Enter always"),
];

/// `followUpBehavior` options.
const FOLLOW_UP_MODES: &[(&str, &str)] = &[("queue", "Queue"), ("steer", "Steer")];

/// `toolCallVisibility` — `TOOL_CALL_VISIBILITY_LABELS`.
const TOOL_CALL_MODES: &[(&str, &str)] = &[
    ("showAll", "Show all"),
    ("hideSuccessful", "Hide successful"),
    ("hideAll", "Hide all"),
    ("zen", "Zen"),
];

/// `screenReaderAnnouncements` — `SCREEN_READER_ANNOUNCEMENTS_LABELS`.
const SCREEN_READER_MODES: &[(&str, &str)] = &[
    ("status", "Status only"),
    ("messages", "Status and messages"),
    ("everything", "Everything"),
];

/// `diffColorScheme` — `DiffColorsSettingsRow` options.
const DIFF_COLOR_SCHEMES: &[(&str, &str)] = &[
    ("red-green", "Red & green (default)"),
    ("blue-orange", "Blue & orange"),
];

/// `voiceShortcutMode` — `SHORTCUT_MODE_LABELS` in `VoiceSettingsPanel`.
const VOICE_SHORTCUT_MODES: &[(&str, &str)] = &[
    ("auto", "Auto: press to toggle, hold to record"),
    ("hold", "Hold: record while held"),
    ("toggle", "Toggle: press to start, press again to stop"),
];

/// `speechLanguage` choices — the contract accepts any BCP-47 code and the
/// web lists the recognizer's supported set dynamically; native offers the
/// common set plus Auto-detect.
const SPEECH_LANGUAGES: &[(&str, &str)] = &[
    ("auto", "Auto-detect"),
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh", "Chinese"),
    ("hi", "Hindi"),
];

/// `snapShotShortcut` choices — the `SnapShotShortcut` modifier-pair
/// variants (free key-chord capture isn't implemented natively).
const SNAPSHOT_SHORTCUTS: &[(&str, &str)] = &[
    ("both-shift-keys", "Shift + Shift"),
    ("pair:control", "Ctrl + Ctrl"),
    ("pair:alt", "Alt + Alt"),
    ("pair:meta", "Super + Super"),
];

/// `snapShotSound` menu values — `MenuRadioItem`s in `SnapShotSettings`
/// ("off" clears `snapShotPlaySound`).
const SNAPSHOT_SOUNDS: &[(&str, &str)] = &[
    ("off", "Off"),
    ("soft-pop", "Soft pop"),
    ("camera-shutter", "Camera shutter"),
];

/// `browserLinkTarget` — `LINK_TARGET_LABELS`.
const BROWSER_LINK_TARGETS: &[(&str, &str)] = &[
    ("system", "Your default browser"),
    ("app", "T3 Code"),
];

/// `browserSearchEngine` — `SEARCH_ENGINE_LABELS` minus "custom" (the
/// custom-URL field is web-only).
const BROWSER_SEARCH_ENGINES: &[(&str, &str)] = &[
    ("duckduckgo", "DuckDuckGo"),
    ("google", "Google"),
    ("bing", "Bing"),
    ("brave", "Brave Search"),
    ("kagi", "Kagi"),
    ("ecosia", "Ecosia"),
];

/// `idleCompaction` — `IDLE_COMPACTION_OPTIONS` in `SettingsPanels.tsx`.
const IDLE_COMPACTION_MODES: &[(&str, &str, &str)] = &[
    (
        "onReturn",
        "When you return",
        "Compact when you come back to the thread.",
    ),
    (
        "beforeCacheExpires",
        "Before the cache expires (beta)",
        "Compact just before the prompt cache expires.",
    ),
    ("off", "Never", "Leave idle threads untouched."),
];

/// `branchNamingMode` — `BranchNamingSettings` options.
const BRANCH_NAMING_MODES: &[(&str, &str, &str)] = &[
    ("static", "Static", "Branches use the prefix plus a slug."),
    (
        "semantic",
        "Semantic",
        "The model names the branch from your first message.",
    ),
    ("custom", "Custom", "Your instructions shape the name."),
];

/// `worktreeCleanup` select values — null inherits the project default.
const WORKTREE_CLEANUP_MODES: &[(&str, &str)] = &[
    ("inherit", "Inherit"),
    ("off", "Off — never clean up"),
    ("custom", "Custom rules"),
];

/// `backgroundActivity.profile` — `BackgroundActivityProfile` literals.
const BACKGROUND_PROFILES: &[(&str, &str)] = &[
    ("balanced", "Balanced"),
    ("performance", "Performance"),
    ("battery-saver", "Battery saver"),
    ("custom", "Custom"),
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

    // ── Voice (client-local fields) ─────────────────────────────────────
    /// `speechCustomWords` — comma-separated dictionary entries.
    speech_words_field: Option<Entity<TextField>>,
    speech_words_draft: String,
    speech_words_seeded: bool,
    /// `speechCorrectionWord` — spoken cue that opens correction.
    speech_correction_field: Option<Entity<TextField>>,
    speech_correction_draft: String,
    speech_correction_seeded: bool,

    // ── Worktrees ───────────────────────────────────────────────────────
    /// `worktreesDirectory` (server key) — where new worktrees are written.
    worktrees_dir_field: Option<Entity<TextField>>,
    worktrees_dir_draft: String,
    worktrees_dir_seeded: bool,
    /// `branchNamePrefix` (scoped key).
    branch_prefix_field: Option<Entity<TextField>>,
    branch_prefix_draft: String,
    branch_prefix_seeded: bool,
    /// `branchNameInstructions` (scoped key).
    branch_instructions_field: Option<Entity<TextField>>,
    branch_instructions_draft: String,
    branch_instructions_seeded: bool,

    // ── Maintenance (end of the page) ───────────────────────────────────
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
        // Client-local voice fields — PromptSubmitted writes ClientConfig.
        if state.speech_words_field.is_none() {
            let field = cx.new(|cx| {
                TextField::new("Words the recognizer should learn, comma separated", cx)
            });
            state.extra_subs.push(cx.observe(&field, |this, field, cx| {
                this.misc_settings_state.extra.speech_words_draft =
                    settings_misc::field_text(&field, cx);
                cx.notify();
            }));
            state.extra_subs.push(cx.subscribe(
                &field,
                |this, _field, event: &PromptSubmitted, _cx| {
                    this.client_config.speech_custom_words = event.0.trim().to_string();
                    this.client_config.save();
                },
            ));
            state.speech_words_field = Some(field);
        }
        if state.speech_correction_field.is_none() {
            let field = cx.new(|cx| TextField::new("e.g. scratch that", cx));
            state
                .extra_subs
                .push(cx.observe(&field, |this, field, cx| {
                    this.misc_settings_state.extra.speech_correction_draft =
                        settings_misc::field_text(&field, cx);
                    cx.notify();
                }));
            state.extra_subs.push(cx.subscribe(
                &field,
                |this, _field, event: &PromptSubmitted, _cx| {
                    this.client_config.speech_correction_word = event.0.trim().to_string();
                    this.client_config.save();
                },
            ));
            state.speech_correction_field = Some(field);
        }
        // `worktreesDirectory` — server-global key.
        if state.worktrees_dir_field.is_none() {
            let field = cx.new(|cx| TextField::new("Default (~/.t3/worktrees)", cx));
            state.extra_subs.push(cx.observe(&field, |this, field, cx| {
                this.misc_settings_state.extra.worktrees_dir_draft =
                    settings_misc::field_text(&field, cx);
                cx.notify();
            }));
            state.extra_subs.push(cx.subscribe(
                &field,
                |this, _field, event: &PromptSubmitted, cx| {
                    this.update_server_settings(
                        json!({ "worktreesDirectory": event.0.trim() }),
                        cx,
                    );
                },
            ));
            state.worktrees_dir_field = Some(field);
        }
        // Scoped branch-naming fields.
        if state.branch_prefix_field.is_none() {
            let field = cx.new(|cx| TextField::new("e.g. t3/", cx));
            state.extra_subs.push(cx.observe(&field, |this, field, cx| {
                this.misc_settings_state.extra.branch_prefix_draft =
                    settings_misc::field_text(&field, cx);
                cx.notify();
            }));
            state.extra_subs.push(cx.subscribe(
                &field,
                |this, _field, event: &PromptSubmitted, cx| {
                    this.update_scoped_setting(
                        "branchNamePrefix",
                        json!(event.0.trim()),
                        cx,
                    );
                },
            ));
            state.branch_prefix_field = Some(field);
        }
        if state.branch_instructions_field.is_none() {
            let field = cx.new(|cx| TextField::new("Extra branch name guidance (optional)", cx));
            state.extra_subs.push(cx.observe(&field, |this, field, cx| {
                this.misc_settings_state.extra.branch_instructions_draft =
                    settings_misc::field_text(&field, cx);
                cx.notify();
            }));
            state.extra_subs.push(cx.subscribe(
                &field,
                |this, _field, event: &PromptSubmitted, cx| {
                    this.update_scoped_setting(
                        "branchNameInstructions",
                        json!(event.0.trim()),
                        cx,
                    );
                },
            ));
            state.branch_instructions_field = Some(field);
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
        let branch_prefix_value = self
            .scoped_value("branchNamePrefix")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let branch_instructions_value = self
            .scoped_value("branchNameInstructions")
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
        // Client-local fields seed from ClientConfig.
        if !state.speech_words_seeded
            && let Some(field) = state.speech_words_field.clone()
        {
            let value = self.client_config.speech_custom_words.clone();
            if !value.is_empty() {
                state.speech_words_draft = value.clone();
                edit_field(&field, cx, |field, window, cx| {
                    field.reset(window, cx);
                    field.replace_text_in_range(Some(0..0), &value, window, cx);
                });
            }
            state.speech_words_seeded = true;
        }
        if !state.speech_correction_seeded
            && let Some(field) = state.speech_correction_field.clone()
        {
            let value = self.client_config.speech_correction_word.clone();
            if !value.is_empty() {
                state.speech_correction_draft = value.clone();
                edit_field(&field, cx, |field, window, cx| {
                    field.reset(window, cx);
                    field.replace_text_in_range(Some(0..0), &value, window, cx);
                });
            }
            state.speech_correction_seeded = true;
        }
        if !state.worktrees_dir_seeded
            && let Some(field) = state.worktrees_dir_field.clone()
        {
            let value = settings["worktreesDirectory"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if !value.is_empty() {
                state.worktrees_dir_draft = value.clone();
                edit_field(&field, cx, |field, window, cx| {
                    field.reset(window, cx);
                    field.replace_text_in_range(Some(0..0), &value, window, cx);
                });
            }
            state.worktrees_dir_seeded = true;
        }
        if !state.branch_prefix_seeded
            && let Some(field) = state.branch_prefix_field.clone()
        {
            if !branch_prefix_value.is_empty() {
                state.branch_prefix_draft = branch_prefix_value.clone();
                edit_field(&field, cx, |field, window, cx| {
                    field.reset(window, cx);
                    field.replace_text_in_range(
                        Some(0..0),
                        &branch_prefix_value,
                        window,
                        cx,
                    );
                });
            }
            state.branch_prefix_seeded = true;
        }
        if !state.branch_instructions_seeded
            && let Some(field) = state.branch_instructions_field.clone()
        {
            if !branch_instructions_value.is_empty() {
                state.branch_instructions_draft = branch_instructions_value.clone();
                edit_field(&field, cx, |field, window, cx| {
                    field.reset(window, cx);
                    field.replace_text_in_range(
                        Some(0..0),
                        &branch_instructions_value,
                        window,
                        cx,
                    );
                });
            }
            state.branch_instructions_seeded = true;
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

        // Default pull request merge method (web `ProjectDefaultsSettings`
        // "Repositories") — `pullRequestMergeMethod`, null = last selected.
        let merge_method = self.scoped_value("pullRequestMergeMethod");
        let merge_value = merge_method.as_str().unwrap_or("last");
        let merge_label = match merge_value {
            "merge" => "Merge commit",
            "squash" => "Squash and merge",
            "rebase" => "Rebase and merge",
            _ => "Last selected",
        };
        section = section.child(self.scoped_select_setting(
            "Default merge method",
            "How the merge button lands pull requests by default.",
            OpenList::MergeMethod,
            "pullRequestMergeMethod",
            merge_label.into(),
            merge_value,
            vec![
                ("last".to_string(), "Last selected".to_string(), None),
                ("merge".to_string(), "Merge commit".to_string(), None),
                (
                    "squash".to_string(),
                    "Squash and merge".to_string(),
                    None,
                ),
                (
                    "rebase".to_string(),
                    "Rebase and merge".to_string(),
                    None,
                ),
            ],
            cx,
        ));

        section
    }

    // ─── Threads (web /settings/threads) ────────────────────────────────

    /// `snoozePresets` — the array from the settings mirror, falling back to
    /// the contract's `DEFAULT_SNOOZE_PRESETS` when the server stored none.
    fn snooze_presets(&self) -> Vec<Value> {
        self.misc_settings_state.settings["snoozePresets"]
            .as_array()
            .cloned()
            .filter(|list| !list.is_empty())
            .unwrap_or_else(default_snooze_presets)
    }

    /// Write the whole `snoozePresets` array via `server.updateSettings`.
    fn snooze_write(&mut self, presets: Vec<Value>, cx: &mut Context<Self>) {
        self.update_server_settings(json!({ "snoozePresets": presets }), cx);
    }

    /// `OpenList::SnoozeWhen` pick — swap the row's `after` ↔ `at(day)`
    /// shape, preserving the duration/time it already carries.
    pub(crate) fn snooze_pick_when(&mut self, row: usize, value: &str, cx: &mut Context<Self>) {
        let mut presets = self.snooze_presets();
        let Some(preset) = presets.get_mut(row) else {
            return;
        };
        if value == "after" {
            if preset["kind"].as_str() != Some("after") {
                *preset = json!({ "kind": "after", "minutes": 60 });
            }
        } else if preset["kind"].as_str() != Some("at")
            || preset["day"].as_str() != Some(value)
        {
            let time = preset["time"].as_str().unwrap_or("09:00").to_string();
            *preset = json!({ "kind": "at", "day": value, "time": time });
        } else {
            return;
        }
        self.snooze_write(presets, cx);
    }

    /// `OpenList::SnoozeUnit` pick — `"min:<n>"` edits an `after` row's
    /// duration, `"time:HH:MM"` edits an `at` row's time.
    pub(crate) fn snooze_pick_unit(&mut self, row: usize, value: &str, cx: &mut Context<Self>) {
        let mut presets = self.snooze_presets();
        let Some(preset) = presets.get_mut(row) else {
            return;
        };
        if let Some(minutes) = value.strip_prefix("min:")
            && preset["kind"].as_str() == Some("after")
            && let Ok(minutes) = minutes.parse::<u64>()
        {
            preset["minutes"] = json!(minutes);
        } else if let Some(time) = value.strip_prefix("time:")
            && preset["kind"].as_str() == Some("at")
        {
            preset["time"] = json!(time);
        } else {
            return;
        }
        self.snooze_write(presets, cx);
    }

    /// Row-scoped select for the snooze list: the shared
    /// `select_trigger`/`select_options_list` derive element ids from the
    /// list prefix alone, which collides across rows — these carry the row
    /// index in the id.
    fn snooze_select(
        &self,
        row: usize,
        list: OpenList,
        current: SharedString,
        current_value: &str,
        options: &[(String, String, Option<String>)],
        cx: &mut Context<Self>,
    ) -> Div {
        let which = list.prefix();
        let open = self.misc_settings_state.open_list == Some(list);
        let mut cell = div()
            .relative()
            .flex_none()
            .child(
                div()
                    .id(format!("{which}-{row}"))
                    .min_w(px(104.))
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
                            .flex()
                            .items_center()
                            .child(icon(Icon::ChevronDown, 12.).text_color(theme::icon_muted())),
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
                    ),
            );
        if open {
            let mut popup = div()
                .flex()
                .flex_col()
                .min_w(px(150.))
                .max_h(px(260.))
                .overflow_hidden()
                .p_1()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::raised())
                .shadow_2xl();
            for (index, (value, label, _)) in options.iter().enumerate() {
                let value = value.clone();
                let selected = value == current_value;
                popup = popup.child(
                    div()
                        .id(format!("{which}-{row}-option-{index}"))
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
                        .when(selected, |s| s.bg(theme::selected()))
                        .when(!selected, |s| s.hover(|s| s.bg(theme::accent())))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.misc_pick(list, value.clone(), cx);
                            }),
                        )
                        .child(div().flex_none().child(label.clone())),
                );
            }
            cell = cell.child(deferred(
                div()
                    .absolute()
                    .top_full()
                    .left_0()
                    .pt_1()
                    .child(popup),
            ));
        }
        cell
    }

    pub(crate) fn render_threads_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_misc_settings(cx);
        self.maybe_prepare_extra(cx);
        let settings = &self.misc_settings_state.settings;
        let connected = self.commands.is_some();
        let mut page = div().flex().flex_col().gap_8();

        if let Some(error) = &self.misc_settings_state.settings_error {
            page = page.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(error.clone()),
            );
        }

        if connected {
            // Web `ThreadsSettingsPanel` scope sentence + selector — scoped
            // rows below write `projectSettingsOverrides[project]` when a
            // project is picked (PROJECT_SCOPED_SERVER_SETTING_KEYS).
            page = page.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::fg_muted())
                            .child("These defaults apply to new projects and threads. Pick a project below to override them per project."),
                    )
                    .child(self.render_scope_selector(cx)),
            );
        }

        // ── New projects (ServerSettings) ────────────────────────────────
        let mut new_project_rows: Vec<Div> = Vec::new();
        if let Some(field) = self.misc_settings_state.extra.base_dir_field.clone() {
            new_project_rows.push(setting_row(
                "Add project starts in",
                "Leave empty to use \"~/\" when the Add Project browser opens.",
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
        new_project_rows.push(setting_row(
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
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("New projects"))
                .child(settings_card(new_project_rows)),
        );

        // ── New threads — the web `ProjectDefaultsSettings` "general" set:
        // model, permissions, workspace, fallback model, auto-pull. ──
        let mut new_thread_rows: Vec<Div> = Vec::new();
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
        new_thread_rows.push(self.scoped_select_setting(
            "Model",
            "Default model for new threads. Projects can override it.",
            OpenList::DefaultModel,
            "defaultModelSelection",
            default_label,
            &default_value,
            model_options,
            cx,
        ));

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
        new_thread_rows.push(self.select_setting(
            "Fallback model",
            "Used for new threads when the default model's provider is out of usage.",
            OpenList::FallbackModel,
            fallback_label,
            &fallback_value,
            fallback_options,
            cx,
        ));

        let runtime_mode = self
            .scoped_value("defaultRuntimeMode")
            .as_str()
            .unwrap_or("full-access")
            .to_string();
        let runtime_label = settings_misc::RUNTIME_MODES
            .iter()
            .find(|(value, _, _)| *value == runtime_mode.as_str())
            .map(|(_, label, _)| (*label).to_string())
            .unwrap_or_else(|| runtime_mode.clone());
        new_thread_rows.push(self.scoped_select_setting(
            "Default permissions",
            "Default permissions for new threads. Projects can override them.",
            OpenList::RuntimeMode,
            "defaultRuntimeMode",
            runtime_label.into(),
            &runtime_mode,
            settings_misc::RUNTIME_MODES
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
        for (value, label) in settings_misc::ENV_MODES {
            env_options.push((value.to_string(), label.to_string(), None));
        }
        new_thread_rows.push(self.scoped_select_setting(
            "Workspace",
            "Where new threads start. Projects and their t3.json can override it.",
            OpenList::EnvMode,
            "defaultThreadEnvMode",
            env_label.into(),
            env_value,
            env_options,
            cx,
        ));

        new_thread_rows.push(self.scoped_switch_row(
            "Automatically pull",
            "Keeps the default branch current when the checkout has no local changes or commits. Projects can override it.",
            "defaultAutoPull",
            "threads-autopull-switch",
            cx,
        ));

        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("New threads"))
                .child(settings_card(new_thread_rows)),
        );

        // ── Snooze (global scope only — `snoozePresets` isn't
        //    project-scoped) — web `SnoozePresetsRow`. ──
        let presets = self.snooze_presets();
        let mut snooze_rows: Vec<Div> = Vec::new();
        for (row, preset) in presets.iter().enumerate() {
            let kind = preset["kind"].as_str().unwrap_or("after");
            let (when_value, when_label) = if kind == "at" {
                let day = preset["day"].as_str().unwrap_or("tomorrow");
                (
                    day.to_string(),
                    SNOOZE_WHEN
                        .iter()
                        .find(|(value, _)| *value == day)
                        .map(|(_, label)| *label)
                        .unwrap_or(day)
                        .to_string(),
                )
            } else {
                ("after".to_string(), "In".to_string())
            };
            let mut controls = div().flex().items_center().gap_2().flex_wrap();
            controls = controls.child(self.snooze_select(
                row,
                OpenList::SnoozeWhen(row),
                when_label.into(),
                &when_value,
                &SNOOZE_WHEN
                    .iter()
                    .map(|(value, label)| (value.to_string(), label.to_string(), None))
                    .collect::<Vec<_>>(),
                cx,
            ));
            if kind == "at" {
                let time = preset["time"].as_str().unwrap_or("09:00").to_string();
                let mut time_options: Vec<(String, String, Option<String>)> = SNOOZE_TIMES
                    .iter()
                    .map(|value| {
                        (
                            format!("time:{value}"),
                            (*value).to_string(),
                            None,
                        )
                    })
                    .collect();
                if !SNOOZE_TIMES.contains(&time.as_str()) {
                    time_options.insert(0, (format!("time:{time}"), time.clone(), None));
                }
                controls = controls.child(self.snooze_select(
                    row,
                    OpenList::SnoozeUnit(row),
                    format!("at {time}").into(),
                    &format!("time:{time}"),
                    &time_options,
                    cx,
                ));
            } else {
                let minutes = preset["minutes"].as_u64().unwrap_or(60);
                let mut duration_options: Vec<(String, String, Option<String>)> =
                    SNOOZE_DURATIONS
                        .iter()
                        .map(|(value, label)| {
                            (format!("min:{value}"), (*label).to_string(), None)
                        })
                        .collect();
                let in_options = SNOOZE_DURATIONS
                    .iter()
                    .any(|(value, _)| *value == minutes);
                if !in_options {
                    duration_options.insert(
                        0,
                        (
                            format!("min:{minutes}"),
                            format_minutes(minutes),
                            None,
                        ),
                    );
                }
                controls = controls.child(self.snooze_select(
                    row,
                    OpenList::SnoozeUnit(row),
                    format_minutes(minutes).into(),
                    &format!("min:{minutes}"),
                    &duration_options,
                    cx,
                ));
            }
            controls = controls.child(
                div()
                    .id(format!("snooze-remove-{row}"))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                    .child("Remove")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            let mut presets = this.snooze_presets();
                            if row < presets.len() {
                                presets.remove(row);
                                this.snooze_write(presets, cx);
                            }
                        }),
                    ),
            );
            snooze_rows.push(div().px_4().py_2p5().child(controls));
        }
        let mut snooze_card = settings_card(snooze_rows);
        if presets.len() < MAX_SNOOZE_PRESETS {
            snooze_card = snooze_card.child(
                div()
                    .id("snooze-add")
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(theme::card_divider())
                    .cursor_pointer()
                    .text_xs()
                    .text_color(theme::primary())
                    .hover(|s| s.bg(theme::hover()))
                    .child("+ Add preset")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            let mut presets = this.snooze_presets();
                            if let Some(candidate) = next_snooze_candidate(&presets) {
                                presets.push(candidate);
                                this.snooze_write(presets, cx);
                            }
                        }),
                    ),
            );
        }
        let snooze_section = div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(settings_section_title("Snooze")),
                    )
                    .child(
                        div()
                            .id("snooze-reset")
                            .cursor_pointer()
                            .pb_2()
                            .text_xs()
                            .text_color(theme::primary())
                            .hover(|s| s.text_color(theme::fg()))
                            .child("Reset to defaults")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    this.snooze_write(default_snooze_presets(), cx);
                                }),
                            ),
                    ),
            )
            .child(snooze_card)
            .child(
                div()
                    .pt_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Options the thread's snooze menu offers, in the order they appear."),
            );
        page = page.child(snooze_section);

        // ── Usage limits (ServerSettings) ────────────────────────────────
        let auto_resume = settings["autoResumeLimitedThreads"].as_bool() == Some(true);
        let snooze_limited = settings["snoozeLimitedThreads"].as_bool() == Some(true);
        let usage_rows: Vec<Div> = vec![
            setting_row(
                "Resume after usage limits",
                "Automatically resume threads when their usage limits reset. You can cancel auto-resume in each thread.",
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
            ),
            setting_row(
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
            ),
        ];
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Usage limits"))
                .child(settings_card(usage_rows)),
        );
        // Cost/pricing metadata from the composer's `getUsageSummary`
        // fetch — shown only when the server reported it.
        if let Some(pricing_line) = self.usage_pricing_line() {
            page = page.child(
                div()
                    .pt_0p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(pricing_line),
            );
        }

        // ── Auto-settle (scoped ServerSettings) ──────────────────────────
        if connected {
            let mut settle_rows: Vec<Div> = vec![self.scoped_switch_row(
                "Settle merged threads",
                "Settle a thread when its pull request merges. Closed pull requests still settle automatically.",
                "sidebarAutoSettleOnMerge",
                "threads-settle-merge",
                cx,
            )];
            let settle_days = self.scoped_value("sidebarAutoSettleAfterDays").as_i64();
            let days_value = settle_days
                .map(|d| d.to_string())
                .unwrap_or_else(|| "off".into());
            let days_label = AUTO_SETTLE_DAYS
                .iter()
                .find(|(v, _)| *v == days_value)
                .map(|(_, label)| *label)
                .unwrap_or("Off");
            settle_rows.push(self.scoped_select_setting(
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
            ));
            if settle_days.is_some() {
                settle_rows.push(self.scoped_switch_row(
                    "Keep threads with a PR",
                    "Inactivity does not settle threads with a pull request. Merge and close still do.",
                    "sidebarAutoSettleKeepPullRequests",
                    "threads-keep-prs",
                    cx,
                ));
            }
            page = page.child(
                div()
                    .flex()
                    .flex_col()
                    .child(settings_section_title("Auto-settle"))
                    .child(settings_card(settle_rows)),
            );
        }

        // ── Text generation (scoped ServerSettings) ──────────────────────
        if connected {
            let mut text_rows: Vec<Div> = Vec::new();
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
            text_rows.push(self.scoped_select_setting(
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
                text_rows.push(setting_row(
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
            text_rows.push(self.scoped_select_setting(
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
            ));
            page = page.child(
                div()
                    .flex()
                    .flex_col()
                    .child(settings_section_title("Text generation"))
                    .child(settings_card(text_rows)),
            );
        }
        page
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

    // ─── small shared pieces for the new canonical pages ────────────────

    /// A device-local toggle row — flips a `ClientConfig` field and saves
    /// (same write path as `client_switch_row` in settings_misc).
    fn client_switch(
        &self,
        title: &'static str,
        description: &'static str,
        element_id: &'static str,
        on: bool,
        write: fn(&mut AppView, bool),
        cx: &mut Context<Self>,
    ) -> Div {
        setting_row(
            title,
            description,
            div()
                .id(element_id)
                .cursor_pointer()
                .child(switch_control(on))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        write(this, !on);
                        this.client_config.save();
                        cx.notify();
                    }),
                ),
        )
    }

    /// A server-global toggle row — writes `{key: !on}` through
    /// `server.updateSettings`.
    fn server_switch(
        &self,
        title: &'static str,
        description: &'static str,
        element_id: &'static str,
        key: &'static str,
        on: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        setting_row(
            title,
            description,
            div()
                .id(element_id)
                .cursor_pointer()
                .child(switch_control(on))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        this.update_server_settings(json!({ key: !on }), cx);
                    }),
                ),
        )
    }

    /// Wraps a TextField in the bordered box the rows use.
    fn boxed_field(&self, field: Entity<TextField>, width: f32) -> Div {
        div()
            .w(px(width))
            .px_2()
            .py_1()
            .rounded_lg()
            .border_1()
            .border_color(theme::input())
            .bg(theme::field_surface())
            .text_sm()
            .text_color(theme::fg())
            .child(field)
    }

    // ─── Chat (web /settings/chat — ClientSettings) ─────────────────────

    pub(crate) fn render_chat_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_prepare_extra(cx);
        let cfg = &self.client_config;

        // ── Composer ──
        let mut composer_rows: Vec<Div> = vec![self.client_switch(
            "Rich text composer",
            "Show formatted Markdown as you type.",
            "chat-rich-text",
            cfg.composer_rich_text,
            |this, on| this.client_config.composer_rich_text = on,
            cx,
        )];
        let send = cfg.send_shortcut.clone();
        let send_label = SEND_SHORTCUTS
            .iter()
            .find(|(value, _)| *value == send)
            .map(|(_, label)| *label)
            .unwrap_or("Enter");
        composer_rows.push(self.select_setting(
            "Send shortcut",
            "Choose when Enter sends a prompt or inserts a new line.",
            OpenList::SendShortcut,
            send_label.into(),
            &send,
            SEND_SHORTCUTS
                .iter()
                .map(|(value, label)| (value.to_string(), label.to_string(), None))
                .collect(),
            cx,
        ));
        composer_rows.push(self.client_switch(
            "Show skills in slash menu",
            "Also include skills in the / command menu. Skills always appear when you type $.",
            "chat-skills-slash",
            cfg.show_skills_in_slash_menu,
            |this, on| this.client_config.show_skills_in_slash_menu = on,
            cx,
        ));

        // ── Follow-ups ──
        let follow_up = cfg.follow_up_behavior.clone();
        let follow_label = if follow_up == "steer" {
            "Steer"
        } else {
            "Queue"
        };
        let follow_rows: Vec<Div> = vec![self.select_setting(
            "Follow-up behavior",
            "Queue follow-ups while the agent runs or steer the current run. Press Ctrl + Enter to do the opposite for one message.",
            OpenList::FollowUpBehavior,
            follow_label.into(),
            &follow_up,
            FOLLOW_UP_MODES
                .iter()
                .map(|(value, label)| (value.to_string(), label.to_string(), None))
                .collect(),
            cx,
        )];

        // ── Activity ──
        let visibility = cfg.tool_call_visibility.clone();
        let visibility_label = TOOL_CALL_MODES
            .iter()
            .find(|(value, _)| *value == visibility)
            .map(|(_, label)| *label)
            .unwrap_or("Show all");
        let announcements = cfg.screen_reader_announcements.clone();
        let announcements_label = SCREEN_READER_MODES
            .iter()
            .find(|(value, _)| *value == announcements)
            .map(|(_, label)| *label)
            .unwrap_or("Status only");
        let activity_rows: Vec<Div> = vec![
            self.select_setting(
                "Tool calls in chat",
                "Hide the commands, edits, and searches agents run. Hide successful keeps failures. Zen shows only final answers. Approvals, questions, and errors always show.",
                OpenList::ToolCallVisibility,
                visibility_label.into(),
                &visibility,
                TOOL_CALL_MODES
                    .iter()
                    .map(|(value, label)| (value.to_string(), label.to_string(), None))
                    .collect(),
                cx,
            ),
            self.select_setting(
                "Screen reader announcements",
                "What a screen reader hears from the open thread: a turn starting, needing you, and ending. Status and messages adds what the agent writes before its last message. Everything adds each step.",
                OpenList::ScreenReader,
                announcements_label.into(),
                &announcements,
                SCREEN_READER_MODES
                    .iter()
                    .map(|(value, label)| (value.to_string(), label.to_string(), None))
                    .collect(),
                cx,
            ),
        ];

        div()
            .flex()
            .flex_col()
            .gap_8()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(settings_section_title("Composer"))
                    .child(settings_card(composer_rows)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(settings_section_title("Follow-ups"))
                    .child(settings_card(follow_rows)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(settings_section_title("Activity"))
                    .child(settings_card(activity_rows)),
            )
    }

    // ─── Diffs & pull requests (web /settings/diffs) ────────────────────

    pub(crate) fn render_diffs_section(&self, cx: &mut Context<Self>) -> Div {
        let cfg = &self.client_config;
        let mut diff_rows: Vec<Div> = Vec::new();

        let layout = cfg.diff_layout.clone();
        let layout_label = if layout == "split" {
            "Split"
        } else {
            "Stacked"
        };
        diff_rows.push(self.select_setting(
            "Diff layout",
            "Show diffs stacked or side by side. The toggle in the diff toolbar changes this too.",
            OpenList::DiffLayout,
            layout_label.into(),
            &layout,
            vec![
                ("stacked".to_string(), "Stacked".to_string(), None),
                ("split".to_string(), "Split".to_string(), None),
            ],
            cx,
        ));
        diff_rows.push(self.client_switch(
            "Hide whitespace changes",
            "Set whether the diff panel ignores whitespace-only edits by default.",
            "diffs-ignore-ws",
            cfg.diff_ignore_whitespace,
            |this, on| this.client_config.diff_ignore_whitespace = on,
            cx,
        ));
        let collapsed = cfg.diff_files_collapsed;
        diff_rows.push(self.select_setting(
            "Default diff file state",
            "Start with files expanded or collapsed when opening diffs or a pull request's Code tab.",
            OpenList::DiffFiles,
            if collapsed { "Collapsed" } else { "Expanded" }.into(),
            if collapsed { "collapsed" } else { "expanded" },
            vec![
                ("expanded".to_string(), "Expanded".to_string(), None),
                ("collapsed".to_string(), "Collapsed".to_string(), None),
            ],
            cx,
        ));
        let colors = cfg.diff_color_scheme.clone();
        let colors_label = if colors == "blue-orange" {
            "Blue & orange"
        } else {
            "Red & green (default)"
        };
        diff_rows.push(self.select_setting(
            "Diff colors",
            "The palette added/removed lines use in diffs.",
            OpenList::DiffColors,
            colors_label.into(),
            &colors,
            DIFF_COLOR_SCHEMES
                .iter()
                .map(|(value, label)| (value.to_string(), label.to_string(), None))
                .collect(),
            cx,
        ));

        let pr_rows: Vec<Div> = vec![
            self.client_switch(
                "Open pull requests and changes panels proactively",
                "Open linked pull requests first. Otherwise, open Changes for edits to at least 3 files or 50 lines.",
                "diffs-proactive",
                cfg.proactive_panels,
                |this, on| this.client_config.proactive_panels = on,
                cx,
            ),
            self.client_switch(
                "Open pull requests in T3 Code",
                "Clicking a pull request link opens it in T3 Code. Turn off to open pull request links like any other link, on your git host.",
                "diffs-pr-links",
                cfg.pull_request_links_open_in_app,
                |this, on| this.client_config.pull_request_links_open_in_app = on,
                cx,
            ),
        ];

        div()
            .flex()
            .flex_col()
            .gap_8()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(settings_section_title("Diffs"))
                    .child(settings_card(diff_rows)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(settings_section_title("Pull requests"))
                    .child(settings_card(pr_rows)),
            )
    }

    // ─── Voice (web /settings/voice — client-local rows) ────────────────

    pub(crate) fn render_voice_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_prepare_extra(cx);
        let cfg = &self.client_config;
        let mut page = div().flex().flex_col().gap_8();
        page = page.child(
            div()
                .text_sm()
                .text_color(theme::fg_muted())
                .child("Voice dictation lives in the composer — these preferences shape how recordings are transcribed and cleaned up. Recognition runs on the connected environment; the web additionally manages downloadable speech models."),
        );

        let mode = cfg.voice_shortcut_mode.clone();
        let mode_label = VOICE_SHORTCUT_MODES
            .iter()
            .find(|(value, _)| *value == mode)
            .map(|(_, label)| *label)
            .unwrap_or("Auto: press to toggle, hold to record");
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Shortcut"))
                .child(settings_card(vec![self.select_setting(
                    "Voice shortcut",
                    "How the microphone key starts and stops dictation.",
                    OpenList::VoiceShortcut,
                    mode_label.into(),
                    &mode,
                    VOICE_SHORTCUT_MODES
                        .iter()
                        .map(|(value, label)| (value.to_string(), label.to_string(), None))
                        .collect(),
                    cx,
                )])),
        );

        let language = cfg.speech_language.clone();
        let language_label = SPEECH_LANGUAGES
            .iter()
            .find(|(value, _)| *value == language)
            .map(|(_, label)| *label)
            .unwrap_or("Auto-detect");
        let transcription_rows: Vec<Div> = vec![
            self.select_setting(
                "Speech language",
                "Language the recognizer listens for. Auto-detect follows what it hears.",
                OpenList::SpeechLanguage,
                language_label.into(),
                &language,
                SPEECH_LANGUAGES
                    .iter()
                    .map(|(value, label)| (value.to_string(), label.to_string(), None))
                    .collect(),
                cx,
            ),
            self.client_switch(
                "Translate to English",
                "Translate dictated speech into English before it reaches the composer.",
                "voice-translate",
                cfg.speech_translate_to_english,
                |this, on| this.client_config.speech_translate_to_english = on,
                cx,
            ),
            self.client_switch(
                "Filter silence",
                "Drop silent stretches from recordings before transcription.",
                "voice-silence",
                cfg.speech_silence_filtering,
                |this, on| this.client_config.speech_silence_filtering = on,
                cx,
            ),
            self.client_switch(
                "Remove filler words",
                "Strip um, uh, and similar fillers from the transcript.",
                "voice-fillers",
                cfg.speech_remove_filler_words,
                |this, on| this.client_config.speech_remove_filler_words = on,
                cx,
            ),
        ];
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Transcription"))
                .child(settings_card(transcription_rows)),
        );

        let mut dictionary_rows: Vec<Div> = Vec::new();
        if let Some(field) = self.misc_settings_state.extra.speech_words_field.clone() {
            dictionary_rows.push(setting_row(
                "Shared dictionary",
                "Words the recognizer should learn — names, jargon, commands. Comma separated; Enter saves.",
                self.boxed_field(field, 220.),
            ));
        }
        if let Some(field) = self.misc_settings_state.extra.speech_correction_field.clone()
        {
            dictionary_rows.push(setting_row(
                "Correction cue",
                "Say this word to correct the last phrase — e.g. “scratch that”. Enter saves.",
                self.boxed_field(field, 160.),
            ));
        }
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Dictionary"))
                .child(settings_card(dictionary_rows)),
        );

        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Cleanup"))
                .child(settings_card(vec![setting_row(
                    "Cleanup model",
                    "Transcript cleanup uses the text generation model configured on the Threads page.",
                    div()
                        .text_sm()
                        .text_color(theme::fg_muted())
                        .child("follows text generation model"),
                )])),
        );
        page
    }

    // ─── SnapShots (web /settings/snapshots — client-local rows) ────────

    pub(crate) fn render_snapshots_section(&self, cx: &mut Context<Self>) -> Div {
        let cfg = &self.client_config;
        let mut rows: Vec<Div> = vec![self.client_switch(
            "Enable SnapShots",
            "Capture windows as screenshots attached to your next message.",
            "snap-enabled",
            cfg.snap_shot_enabled,
            |this, on| this.client_config.snap_shot_enabled = on,
            cx,
        )];
        let shortcut = cfg.snap_shot_shortcut.clone();
        let shortcut_label = SNAPSHOT_SHORTCUTS
            .iter()
            .find(|(value, _)| *value == shortcut)
            .map(|(_, label)| *label)
            .unwrap_or("Shift + Shift");
        rows.push(self.select_setting(
            "Capture shortcut",
            "The key pair that captures the focused window.",
            OpenList::SnapShortcut,
            shortcut_label.into(),
            &shortcut,
            SNAPSHOT_SHORTCUTS
                .iter()
                .map(|(value, label)| (value.to_string(), label.to_string(), None))
                .collect(),
            cx,
        ));
        rows.push(self.client_switch(
            "Include app text in snapshots",
            "Include text and controls when the app makes them available.",
            "snap-accessibility",
            cfg.snap_shot_include_accessibility,
            |this, on| this.client_config.snap_shot_include_accessibility = on,
            cx,
        ));
        rows.push(self.client_switch(
            "Play a sound",
            "Play a sound when capture starts.",
            "snap-play-sound",
            cfg.snap_shot_play_sound,
            |this, on| this.client_config.snap_shot_play_sound = on,
            cx,
        ));
        if cfg.snap_shot_play_sound {
            let sound = cfg.snap_shot_sound.clone();
            let sound_label = if sound == "camera-shutter" {
                "Camera shutter"
            } else {
                "Soft pop"
            };
            rows.push(self.select_setting(
                "Snapshot sound",
                "Choose the sound played when capture starts.",
                OpenList::SnapSound,
                sound_label.into(),
                &sound,
                SNAPSHOT_SOUNDS
                    .iter()
                    .filter(|(value, _)| *value != "off")
                    .map(|(value, label)| (value.to_string(), label.to_string(), None))
                    .collect(),
                cx,
            ));
        }
        rows.push(self.client_switch(
            "Flash captured window",
            "Show a gentle cue on the captured window.",
            "snap-flash",
            cfg.snap_shot_flash,
            |this, on| this.client_config.snap_shot_flash = on,
            cx,
        ));
        rows.push(self.client_switch(
            "Animate snapshots",
            "Animate captured windows into your draft.",
            "snap-animations",
            cfg.snap_shot_animations,
            |this, on| this.client_config.snap_shot_animations = on,
            cx,
        ));

        div()
            .flex()
            .flex_col()
            .gap_8()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(settings_section_title("SnapShots"))
                    .child(settings_card(rows)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Capture bindings register in the desktop client; these preferences persist on this device for the clients that drive them."),
            )
    }

    // ─── Browser (web /settings/browser) ────────────────────────────────

    pub(crate) fn render_browser_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_misc_settings(cx);
        let cfg = &self.client_config;
        let mut page = div().flex().flex_col().gap_8();

        let mut agent_rows: Vec<Div> = Vec::new();
        if self.commands.is_some() {
            agent_rows.push(self.scoped_switch_row(
                "Agent browser access",
                "Let agents open and drive a browser on the connected environment — page fetch, automation, screenshots.",
                "enableAgentBrowserAccess",
                "browser-agent-access",
                cx,
            ));
        }
        agent_rows.push(self.client_switch(
            "Auto-show floating preview",
            "Show the floating preview when an agent opens a browser or device unless the agent says otherwise.",
            "browser-auto-preview",
            cfg.browser_auto_show_floating_preview,
            |this, on| this.client_config.browser_auto_show_floating_preview = on,
            cx,
        ));
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Agent access"))
                .child(settings_card(agent_rows)),
        );

        let target = cfg.browser_link_target.clone();
        let target_label = if target == "app" {
            "T3 Code"
        } else {
            "Your default browser"
        };
        let engine = cfg.browser_search_engine.clone();
        let engine_label = BROWSER_SEARCH_ENGINES
            .iter()
            .find(|(value, _)| *value == engine)
            .map(|(_, label)| *label)
            .unwrap_or("DuckDuckGo");
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Links"))
                .child(settings_card(vec![
                    self.select_setting(
                        "Open links in",
                        "Where links in the chat and terminal open. Hold Ctrl while clicking a link to open it in your default browser either way.",
                        OpenList::BrowserLink,
                        target_label.into(),
                        &target,
                        BROWSER_LINK_TARGETS
                            .iter()
                            .map(|(value, label)| {
                                (value.to_string(), label.to_string(), None)
                            })
                            .collect(),
                        cx,
                    ),
                    self.select_setting(
                        "Search engine",
                        "What the address bar searches when you type something that isn't a URL.",
                        OpenList::BrowserEngine,
                        engine_label.into(),
                        &engine,
                        BROWSER_SEARCH_ENGINES
                            .iter()
                            .map(|(value, label)| {
                                (value.to_string(), label.to_string(), None)
                            })
                            .collect(),
                        cx,
                    ),
                ])),
        );

        page = page.child(
            div()
                .text_xs()
                .text_color(theme::fg_muted())
                .child("The native client has no embedded browser — in-app tabs and recordings open in the desktop or web client; these preferences still apply to sessions this client starts."),
        );

        // MCP-app surfaces fold — the old native-only "Integrations" page.
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("MCP apps"))
                .child(self.render_integrations_section(cx)),
        );
        page
    }

    // ─── Devices (web /settings/devices) ────────────────────────────────

    pub(crate) fn render_devices_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_misc_settings(cx);
        let settings = &self.misc_settings_state.settings;
        let mut page = div().flex().flex_col().gap_8();

        let hub = settings["enableDeviceSupport"].as_bool() == Some(true);
        let mut device_rows: Vec<Div> = vec![self.server_switch(
            "Device hub",
            "Let this environment host companion devices (phones, tablets, other desktops) that agents can see and drive.",
            "devices-hub",
            "enableDeviceSupport",
            hub,
            cx,
        )];
        if self.commands.is_some() {
            device_rows.push(self.scoped_switch_row(
                "Agent device access",
                "New agent sessions can open and drive these devices via the agent-device tool.",
                "enableAgentDeviceAccess",
                "devices-agent-access",
                cx,
            ));
        }
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Devices"))
                .child(settings_card(device_rows)),
        );
        page = page.child(
            div()
                .text_xs()
                .text_color(theme::fg_muted())
                .child("Paired devices appear once the hub is enabled — pairing and per-device controls live in the desktop and web clients."),
        );
        page
    }

    // ─── Worktrees (web /settings/worktrees) ────────────────────────────

    pub(crate) fn render_worktrees_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_misc_settings(cx);
        self.maybe_prepare_extra(cx);
        let mut page = div().flex().flex_col().gap_8();
        if let Some(error) = &self.misc_settings_state.settings_error {
            page = page.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(error.clone()),
            );
        }
        if self.commands.is_some() {
            page = page.child(self.render_scope_selector(cx));
        }

        // ── Creation ──
        let mut creation_rows: Vec<Div> = Vec::new();
        if self.commands.is_some() {
            creation_rows.push(self.scoped_switch_row(
                "Start new worktrees from remote",
                "Uses the latest commit of the base branch's tracked remote branch, even if its name differs. Otherwise uses the same-named branch on origin. Uses your local branch if that remote branch is missing.",
                "newWorktreesStartFromOrigin",
                "wt-origin",
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
            creation_rows.push(self.scoped_select_setting(
                "Submodules",
                "How new worktrees check out git submodules.",
                OpenList::WorktreeSubmodules,
                "worktreeSubmodules",
                submodules_label.into(),
                submodules_value,
                submodule_options,
                cx,
            ));
        }
        if let Some(field) = self.misc_settings_state.extra.worktrees_dir_field.clone() {
            creation_rows.push(setting_row(
                "Worktrees directory",
                "Where new worktrees are written on the server. Enter saves; empty uses the default.",
                self.boxed_field(field, 220.),
            ));
        }
        if self.commands.is_some() {
            // `worktreeCleanup`: null inherits the default rules, "off"
            // disables, "custom" writes the contract's default rule set.
            let cleanup = self.scoped_value("worktreeCleanup");
            let cleanup_mode = cleanup["mode"].as_str();
            let cleanup_value = match cleanup_mode {
                Some(mode) => mode.to_string(),
                None if cleanup.is_null() => "inherit".to_string(),
                None => "custom".to_string(),
            };
            let cleanup_label = WORKTREE_CLEANUP_MODES
                .iter()
                .find(|(value, _)| *value == cleanup_value)
                .map(|(_, label)| *label)
                .unwrap_or("Inherit");
            // `misc_pick` translates the string select into the nullable
            // {mode,rules} union the contract stores.
            creation_rows.push(self.scoped_select_setting(
                "Cleanup rules",
                "When the server removes old worktrees — on merge, on delete, after inactivity.",
                OpenList::WorktreeCleanup,
                "worktreeCleanup",
                cleanup_label.into(),
                &cleanup_value,
                WORKTREE_CLEANUP_MODES
                    .iter()
                    .map(|(value, label)| (value.to_string(), label.to_string(), None))
                    .collect(),
                cx,
            ));
        }
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Creation"))
                .child(settings_card(creation_rows)),
        );

        // ── Branches ──
        let mut branch_rows: Vec<Div> = Vec::new();
        if self.commands.is_some() {
            let naming = self
                .scoped_value("branchNamingMode")
                .as_str()
                .unwrap_or("static")
                .to_string();
            let naming_label = BRANCH_NAMING_MODES
                .iter()
                .find(|(value, _, _)| *value == naming)
                .map(|(_, label, _)| *label)
                .unwrap_or("Static");
            branch_rows.push(self.scoped_select_setting(
                "Branch naming",
                "Choose how new worktree branches are named from your first message.",
                OpenList::BranchNamingMode,
                "branchNamingMode",
                naming_label.into(),
                &naming,
                BRANCH_NAMING_MODES
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
        }
        if let Some(field) = self.misc_settings_state.extra.branch_prefix_field.clone() {
            branch_rows.push(setting_row(
                "Branch prefix",
                "For example, t3 or t3/ produces t3/add-search. Enter saves; empty means no prefix.",
                self.boxed_field(field, 160.),
            ));
        }
        if let Some(field) = self
            .misc_settings_state
            .extra
            .branch_instructions_field
            .clone()
        {
            branch_rows.push(setting_row(
                "Branch naming instructions",
                "Appended to the naming prompt. The model returns the complete branch name; no prefix or suffix is added. Enter saves.",
                self.boxed_field(field, 220.),
            ));
        }
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Branches"))
                .child(settings_card(branch_rows)),
        );
        page
    }

    // ─── Labs (web /settings/labs) ──────────────────────────────────────

    pub(crate) fn render_labs_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_misc_settings(cx);
        let settings = &self.misc_settings_state.settings;
        let cfg = &self.client_config;
        let mut rows: Vec<Div> = Vec::new();

        rows.push(self.client_switch(
            "Boring sidebar (group by project)",
            "Group sidebar threads into project folders. Pinned threads stay on top, and Snoozed and Settled stay below the folders.",
            "labs-boring-sidebar",
            cfg.boring_sidebar,
            |this, on| this.client_config.boring_sidebar = on,
            cx,
        ));
        if cfg.boring_sidebar {
            rows.push(setting_row(
                "Two-line folder rows",
                "Show folder threads on two lines: the title, then the branch, pull request, and agent.",
                div()
                    .id("labs-boring-multiline")
                    .cursor_pointer()
                    .child(switch_control(cfg.boring_sidebar_multiline))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            this.client_config.boring_sidebar_multiline =
                                !this.client_config.boring_sidebar_multiline;
                            this.client_config.save();
                            cx.notify();
                        }),
                    ),
            ));
        }
        let guest = settings["guestSharingEnabled"].as_bool() == Some(true);
        rows.push(self.server_switch(
            "Guest links",
            "Share a thread with a guest who watches it live in a browser and sends messages you approve. Needs T3 Connect.",
            "labs-guest-links",
            "guestSharingEnabled",
            guest,
            cx,
        ));
        let linked = settings["linkedEnvironmentsEnabled"].as_bool() == Some(true);
        rows.push(self.server_switch(
            "Linked environments",
            "Link environments so threads and projects can span machines.",
            "labs-linked-envs",
            "linkedEnvironmentsEnabled",
            linked,
            cx,
        ));
        let suggestions = settings["promptSuggestionsEnabled"].as_bool() == Some(true);
        rows.push(self.server_switch(
            "Suggest next prompt",
            "Show a predicted next message in the empty composer after a Claude or Codex turn. Each is an extra cached request on your plan.",
            "labs-prompt-suggest",
            "promptSuggestionsEnabled",
            suggestions,
            cx,
        ));
        let simlock = settings["simlockDeviceHostsEnabled"].as_bool() == Some(true);
        rows.push(self.server_switch(
            "Simlock device hosts",
            "Let devices attach simulators through Simlock for agent device sessions.",
            "labs-simlock",
            "simlockDeviceHostsEnabled",
            simlock,
            cx,
        ));
        let fast_checkout = settings["fastWorktreeCheckout"].as_bool() == Some(true);
        rows.push(self.server_switch(
            "Fast worktree checkout",
            "On a Mac, clone new worktrees from a checkout cached in the T3 home folder instead of writing every file. Uses Git's normal checkout when it can't. Turning it off deletes the cache.",
            "labs-fast-worktree",
            "fastWorktreeCheckout",
            fast_checkout,
            cx,
        ));

        div()
            .flex()
            .flex_col()
            .gap_8()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(settings_section_title("Beta"))
                    .child(settings_card(rows)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Features still settling in. Boring-sidebar rows are device-local; the rest write server settings."),
            )
    }

    // ─── Maintenance (web /settings/maintenance + native folds) ─────────

    pub(crate) fn render_maintenance_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_misc_settings(cx);
        let settings = &self.misc_settings_state.settings;
        let mut page = div().flex().flex_col().gap_8();

        if let Some(error) = &self.misc_settings_state.settings_error {
            page = page.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(error.clone()),
            );
        }
        if self.commands.is_some() {
            page = page.child(self.render_scope_selector(cx));
        }

        // ── Updates (moved from the old combined Threads page) ──────────
        let version = self
            .misc_settings_state
            .server_version
            .clone()
            .unwrap_or_else(|| "unknown".into());
        let update_checking = self.misc_settings_state.extra.update_checking;
        let mut update_control = div()
            .id("maintenance-check-update")
            .px_3()
            .py_1p5()
            .rounded_md()
            .text_xs()
            .cursor_pointer()
            .bg(theme::hover())
            .text_color(theme::fg())
            .hover(|s| s.bg(theme::selected()))
            .child(if update_checking {
                "Checking…"
            } else {
                "Check for updates"
            });
        if update_checking {
            update_control = update_control.opacity(0.4);
        }
        update_control = update_control.on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _ev, _window, cx| {
                this.check_server_update(cx);
            }),
        );
        let mut update_rows: Vec<Div> = vec![
            setting_row(
                "Server version",
                "Reported by server.getConfig on this environment.",
                div().text_sm().text_color(theme::fg_muted()).child(version),
            ),
            setting_row(
                "Updates",
                "Ask the npm registry for the latest t3 release.",
                update_control,
            ),
        ];
        let provider_checks = settings["enableProviderUpdateChecks"].as_bool() != Some(false);
        update_rows.push(self.server_switch(
            "Provider update checks",
            "Check whether installed provider CLIs have newer versions available.",
            "maintenance-provider-checks",
            "enableProviderUpdateChecks",
            provider_checks,
            cx,
        ));
        if self.commands.is_some() {
            update_rows.push(self.scoped_switch_row(
                "Continue threads after server update",
                "Resume running threads after the server restarts for an update.",
                "continueThreadsAfterServerUpdate",
                "maintenance-continue-threads",
                cx,
            ));
        }
        let mut updates_section = div()
            .flex()
            .flex_col()
            .child(settings_section_title("Updates"))
            .child(settings_card(update_rows));
        if let Some(status) = &self.misc_settings_state.extra.update_status {
            let status = status.clone();
            let upgrade_link = status.strip_prefix("LATEST:");
            updates_section = updates_section.child(
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
                                .id("maintenance-open-npm")
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
        page = page.child(updates_section);

        // ── Background activity — `backgroundActivity.profile`, merged so
        //    `baseProfile`/`overrides` survive the patch. ──
        let activity = settings["backgroundActivity"].clone();
        let profile = activity["profile"]
            .as_str()
            .unwrap_or("balanced")
            .to_string();
        let profile_label = BACKGROUND_PROFILES
            .iter()
            .find(|(value, _)| *value == profile)
            .map(|(_, label)| *label)
            .unwrap_or("Balanced");
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Background activity"))
                .child(settings_card(vec![self.select_setting(
                    "Background activity",
                    "How hard the server works while you are away — git fetches, provider health checks, power monitoring.",
                    OpenList::BackgroundProfile,
                    profile_label.into(),
                    &profile,
                    BACKGROUND_PROFILES
                        .iter()
                        .map(|(value, label)| {
                            (
                                value.to_string(),
                                label.to_string(),
                                None,
                            )
                        })
                        .collect(),
                    cx,
                )])),
        );

        // ── Native folds: Diagnostics + Usage limits panels. ────────────
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Diagnostics"))
                .child(self.render_diagnostics_section(cx)),
        );
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Usage"))
                .child(self.render_usage_section(cx)),
        );
        page
    }

    // ─── Providers (web /settings/providers) ────────────────────────────

    /// Provider-instance enabled state — `resolveProviderInstanceEnabled`
    /// semantics: an explicit `enabled` flag in the settings override or the
    /// stored config wins; default instances (codex/claudeAgent) and drivers
    /// the web enables by default resolve true.
    fn provider_instance_enabled(&self, instance_id: &str, provider: &Value) -> bool {
        let configured =
            &self.misc_settings_state.settings["providerInstances"][instance_id];
        if let Some(enabled) = configured["enabled"].as_bool() {
            return enabled;
        }
        if let Some(enabled) = provider["enabled"].as_bool() {
            return enabled;
        }
        // `DEFAULT_DISABLED_PROVIDER_DRIVERS` in settings.ts — everything
        // else defaults on.
        let driver = provider["driver"].as_str().unwrap_or(instance_id);
        !matches!(
            driver,
            "cursor" | "grok" | "fx" | "kiro" | "muse" | "pi" | "opencode" | "antigravity"
        )
    }

    /// Toggle one instance — `providerInstances` merges per entry on the
    /// server, so a `{instanceId: {enabled}}` patch can't drop siblings.
    fn toggle_provider_instance(&mut self, instance_id: String, on: bool, cx: &mut Context<Self>) {
        self.update_server_settings(
            json!({ "providerInstances": { instance_id: { "enabled": on } } }),
            cx,
        );
    }

    /// Toggle a `usageLimitSources` / `textGenerators` entry — same
    /// per-entry merge semantics (`{id: {enabled}}`).
    fn toggle_settings_record(
        &mut self,
        record: &'static str,
        id: String,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        self.update_server_settings(json!({ record: { id: { "enabled": on } } }), cx);
    }

    /// The 16px provider glyph — same driver mapping as the composer chip.
    fn provider_mark(&self, driver: &str) -> Option<Icon> {
        match driver {
            "codex" | "openai" | "openaiCompatible" => Some(Icon::ProviderOpenAi),
            "claudeAgent" | "anthropic" => Some(Icon::ProviderClaude),
            _ => None,
        }
    }

    pub(crate) fn render_providers_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_misc_settings(cx);
        let settings = &self.misc_settings_state.settings;
        let connected = self.commands.is_some();
        let mut page = div().flex().flex_col().gap_8();

        if let Some(error) = &self.misc_settings_state.settings_error {
            page = page.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(error.clone()),
            );
        }

        // ── Provider list — `server.getConfig → providers` (mirrored into
        //    both misc state and the composer's config stream). ──
        let mut providers: Vec<Value> = self.misc_settings_state.providers.clone();
        if providers.is_empty() {
            providers = self.composer_state.config_providers.clone();
        }
        let mut provider_card = div().flex().flex_col().rounded_lg();
        let mut provider_count = 0usize;
        for (index, provider) in providers.iter().enumerate() {
            let instance_id = provider["instanceId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if instance_id.is_empty() {
                continue;
            }
            provider_count += 1;
            let name = provider["name"]
                .as_str()
                .or_else(|| provider["displayName"].as_str())
                .unwrap_or(&instance_id)
                .to_string();
            let driver = provider["driver"].as_str().unwrap_or(&instance_id).to_string();
            let enabled = self.provider_instance_enabled(&instance_id, provider);
            let mut detail = driver.clone();
            if let Some(version) = provider["version"].as_str()
                && !version.is_empty()
            {
                detail = format!("{detail} · v{version}");
            }
            let status = provider["status"].as_str().unwrap_or_default();
            let availability = provider["availability"].as_str().unwrap_or_default();
            if !status.is_empty() && status != "ready" {
                detail = format!("{detail} · {status}");
            }
            if availability == "unavailable" {
                detail = format!("{detail} · unavailable");
            }
            let mark = self.provider_mark(&driver);
            let iid = instance_id.clone();
            let mut row = div()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_3()
                .when(index > 0, |r| {
                    r.border_t_1().border_color(theme::card_divider())
                })
                .child(
                    div()
                        .w(px(28.))
                        .h(px(28.))
                        .flex_none()
                        .rounded_md()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme::hover())
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::fg_muted())
                        .child(if let Some(mark) = mark {
                            icon(mark, 14.).text_color(theme::fg()).into_any_element()
                        } else {
                            SharedString::from(
                                provider["name"]
                                    .as_str()
                                    .or_else(|| provider["displayName"].as_str())
                                    .unwrap_or(&instance_id)
                                    .chars()
                                    .next()
                                    .map(|c| c.to_ascii_uppercase().to_string())
                                    .unwrap_or_else(|| "?".into()),
                            )
                            .into_any_element()
                        }),
                )
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
                                .child(name),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .truncate()
                                .child(detail),
                        ),
                );
            if connected {
                row = row.child(
                    div()
                        .id(format!("provider-toggle-{index}"))
                        .cursor_pointer()
                        .child(switch_control(enabled))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.toggle_provider_instance(iid.clone(), !enabled, cx);
                            }),
                        ),
                );
            } else {
                row = row.child(
                    div()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(if enabled { "enabled" } else { "disabled" }),
                );
            }
            provider_card = provider_card.child(row);

            // Per-instance configuration — stored `providerInstances[id]`
            // rows: driver config fields the schema describes (binaryPath,
            // homePath, launchArguments, setupMode…) plus the environment
            // list, rendered read-only because secret values round-trip
            // server-side.
            let configured =
                &self.misc_settings_state.settings["providerInstances"][&instance_id];
            let config = &configured["config"];
            let mut config_rows: Vec<(String, String)> = Vec::new();
            for key in [
                "binaryPath",
                "homePath",
                "shadowHomePath",
                "launchArguments",
                "setupMode",
            ] {
                let value = &config[key];
                let text = match value {
                    Value::String(s) if !s.is_empty() => s.clone(),
                    Value::Array(items) if !items.is_empty() => items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => continue,
                };
                config_rows.push((key.to_string(), text));
            }
            let environment = configured["environment"].as_array().cloned().unwrap_or_else(
                || {
                    provider["environment"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                },
            );
            if !config_rows.is_empty() || !environment.is_empty() {
                let mut detail_card = div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_4()
                    .pb_3()
                    .pt_1();
                for (key, text) in config_rows {
                    detail_card = detail_card.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .child(
                                div()
                                    .w(px(120.))
                                    .flex_none()
                                    .text_color(theme::fg_muted())
                                    .child(key),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme::fg())
                                    .child(text),
                            ),
                    );
                }
                for variable in &environment {
                    let name = variable["name"].as_str().unwrap_or_default().to_string();
                    if name.is_empty() {
                        continue;
                    }
                    let sensitive = variable["sensitive"].as_bool() == Some(true)
                        || variable["valueRedacted"].as_bool() == Some(true)
                        || variable["secret"].is_object();
                    let value = if sensitive {
                        "••••••••".to_string()
                    } else {
                        variable["value"].as_str().unwrap_or_default().to_string()
                    };
                    detail_card = detail_card.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .child(
                                div()
                                    .w(px(120.))
                                    .flex_none()
                                    .text_color(theme::fg_muted())
                                    .child(name),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme::fg())
                                    .child(if value.is_empty() {
                                        "(unset)".to_string()
                                    } else {
                                        value
                                    }),
                            ),
                    );
                }
                provider_card = provider_card.child(detail_card);
            }
        }
        if provider_count == 0 {
            provider_card = provider_card.child(
                div()
                    .px_4()
                    .py_3()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(if connected {
                        "No provider instances reported yet — the list fills in when the server's provider config arrives."
                    } else {
                        "Connect to a server to see provider instances."
                    }),
            );
        }
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Providers"))
                .child(
                    provider_card
                        .border_1()
                        .border_color(theme::card_border())
                        .bg(theme::card_surface_40()),
                ),
        );

        // ── Usage providers — `usageLimitSources` merges per entry. ─────
        let mut usage_rows: Vec<Div> = Vec::new();
        if let Some(sources) = settings["usageLimitSources"].as_object() {
            for (id, source) in sources {
                let enabled = source["enabled"].as_bool() != Some(false);
                let label = source["label"]
                    .as_str()
                    .map(str::to_string)
                    .filter(|label| !label.is_empty())
                    .unwrap_or_else(|| id.clone());
                let url = source["url"].as_str().unwrap_or_default().to_string();
                let kind = source["kind"].as_str().unwrap_or("cliproxy");
                let record_id = id.clone();
                usage_rows.push(setting_row(
                    "Usage provider",
                    "Reports provider usage so limits appear in the UI.",
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
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
                                        .child(label),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::fg_muted())
                                        .truncate()
                                        .child(format!("{kind} · {url}")),
                                ),
                        )
                        .child(
                            div()
                                .id(format!("usage-source-{record_id}"))
                                .cursor_pointer()
                                .child(switch_control(enabled))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _ev, _window, cx| {
                                        this.toggle_settings_record(
                                            "usageLimitSources",
                                            record_id.clone(),
                                            !enabled,
                                            cx,
                                        );
                                    }),
                                ),
                        ),
                ));
            }
        }
        if usage_rows.is_empty() {
            usage_rows.push(setting_row(
                "Usage providers",
                "cliproxy-style usage sources configured on this environment.",
                div()
                    .text_sm()
                    .text_color(theme::fg_muted())
                    .child("None configured"),
            ));
        }
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Usage providers"))
                .child(settings_card(usage_rows)),
        );

        // ── Text generators — `textGenerators` merges per entry. ────────
        let mut generator_rows: Vec<Div> = Vec::new();
        if let Some(generators) = settings["textGenerators"].as_object() {
            for (id, generator) in generators {
                let enabled = generator["enabled"].as_bool() != Some(false);
                let label = generator["label"]
                    .as_str()
                    .map(str::to_string)
                    .filter(|label| !label.is_empty())
                    .unwrap_or_else(|| id.clone());
                let base_url = generator["baseUrl"].as_str().unwrap_or_default();
                let kind = generator["kind"]
                    .as_str()
                    .unwrap_or("openaiCompatible");
                let record_id = id.clone();
                generator_rows.push(setting_row(
                    "Text generator",
                    "A model API that only generates text — thread titles, cleanup.",
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
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
                                        .child(label),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::fg_muted())
                                        .truncate()
                                        .child(format!("{kind} · {base_url}")),
                                ),
                        )
                        .child(
                            div()
                                .id(format!("text-gen-{record_id}"))
                                .cursor_pointer()
                                .child(switch_control(enabled))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _ev, _window, cx| {
                                        this.toggle_settings_record(
                                            "textGenerators",
                                            record_id.clone(),
                                            !enabled,
                                            cx,
                                        );
                                    }),
                                ),
                        ),
                ));
            }
        }
        if generator_rows.is_empty() {
            generator_rows.push(setting_row(
                "Text generators",
                "OpenAI-compatible or Anthropic text endpoints used for generated text.",
                div()
                    .text_sm()
                    .text_color(theme::fg_muted())
                    .child("None configured"),
            ));
        }
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Text generators"))
                .child(settings_card(generator_rows)),
        );

        // ── Advanced — `idleCompaction` (`idleCompactionPatch` writes the
        //    mode plus the legacy boolean). ──
        if connected {
            let idle = settings["idleCompaction"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| {
                    if settings["compactIdleThreads"].as_bool() == Some(true) {
                        "beforeCacheExpires".into()
                    } else {
                        "onReturn".into()
                    }
                });
            let idle_label = IDLE_COMPACTION_MODES
                .iter()
                .find(|(value, _, _)| *value == idle)
                .map(|(_, label, _)| *label)
                .unwrap_or("When you return");
            page = page.child(
                div()
                    .flex()
                    .flex_col()
                    .child(settings_section_title("Advanced"))
                    .child(settings_card(vec![self.select_setting(
                        "Compact idle threads",
                        "When to compact an idle Claude thread over 200,000 tokens: after you return, just before its prompt cache expires, or never. Each Claude provider can override this.",
                        OpenList::IdleCompaction,
                        idle_label.into(),
                        &idle,
                        IDLE_COMPACTION_MODES
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
                    )])),
            );
        }

        // ── ACP sessions fold — the old native-only "ACP Sessions" page. ─
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("ACP sessions"))
                .child(self.render_acp_section(cx)),
        );
        page
    }
}
