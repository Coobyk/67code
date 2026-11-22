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
use std::collections::{HashMap, HashSet};

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
        if days == 1 {
            "1 day".into()
        } else {
            format!("{days} days")
        }
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
    ("mod-enter-multiline", "Ctrl + Enter for multiline prompts"),
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
const BROWSER_LINK_TARGETS: &[(&str, &str)] =
    &[("system", "Your default browser"), ("app", "T3 Code")];

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

/// Browser "Default viewport" select values — `browserDefaultViewport`
/// flattened to a string for the native mirror: `"fill"` / `"responsive"`
/// (a freeform size the web edits with W×H fields) / a preset id from
/// `PREVIEW_VIEWPORT_PRESET_DEFINITIONS`.
const BROWSER_VIEWPORTS: &[(&str, &str, &str)] = &[
    ("fill", "Fill panel", ""),
    ("responsive", "Responsive", ""),
    ("iphone-se", "iPhone SE", "375 × 667"),
    ("iphone-xr", "iPhone XR", "414 × 896"),
    ("iphone-12-pro", "iPhone 12 Pro", "390 × 844"),
    ("iphone-14-pro-max", "iPhone 14 Pro Max", "430 × 932"),
    ("pixel-7", "Pixel 7", "412 × 915"),
    ("samsung-galaxy-s8-plus", "Samsung Galaxy S8+", "360 × 740"),
    (
        "samsung-galaxy-s20-ultra",
        "Samsung Galaxy S20 Ultra",
        "412 × 915",
    ),
    ("ipad-mini", "iPad Mini", "768 × 1024"),
    ("ipad-air", "iPad Air", "820 × 1180"),
    ("ipad-pro", "iPad Pro", "1024 × 1366"),
    ("surface-pro-7", "Surface Pro 7", "912 × 1368"),
    ("surface-duo", "Surface Duo", "540 × 720"),
    ("galaxy-z-fold-5", "Galaxy Z Fold 5", "344 × 882"),
    ("nest-hub", "Nest Hub", "1024 × 600"),
    ("nest-hub-max", "Nest Hub Max", "1280 × 800"),
];

/// Browser "Default zoom" — `PREVIEW_ZOOM_LEVELS` as percent strings.
const BROWSER_ZOOMS: &[(&str, &str)] = &[
    ("50", "50%"),
    ("67", "67%"),
    ("75", "75%"),
    ("80", "80%"),
    ("90", "90%"),
    ("100", "100%"),
    ("110", "110%"),
    ("125", "125%"),
    ("150", "150%"),
    ("175", "175%"),
    ("200", "200%"),
    ("250", "250%"),
];

/// Browser "Default appearance" — `PreviewAppearancePreference` literals.
const BROWSER_APPEARANCES: &[(&str, &str)] =
    &[("system", "System"), ("light", "Light"), ("dark", "Dark")];

/// Browser "Recording frame rate" — `BROWSER_RECORDING_FRAME_RATES`.
const BROWSER_FRAME_RATES: &[(&str, &str)] = &[("30", "30 fps"), ("60", "60 fps")];

/// One runtime/config text field in the provider detail pane — derived
/// from the `providerSettingsForm` annotations on each driver's settings
/// schema (contracts `CodexSettings`/`ClaudeSettings`/`AntigravitySettings`
/// and the `provider-*/src/settings.ts` files).
struct ProviderFieldDef {
    /// `providerInstances.{id}.config.{key}` the field writes.
    key: &'static str,
    title: &'static str,
    description: &'static str,
    placeholder: &'static str,
    /// `control: "password"` — value shown masked; the stored string may
    /// already be redacted server-side.
    secret: bool,
}

const PROVIDER_CODEX_FIELDS: &[ProviderFieldDef] = &[
    ProviderFieldDef {
        key: "binaryPath",
        title: "Binary path",
        description: "Path to the Codex binary used by this instance.",
        placeholder: "codex",
        secret: false,
    },
    ProviderFieldDef {
        key: "homePath",
        title: "CODEX_HOME path",
        description: "Custom Codex home and config directory.",
        placeholder: "~/.codex",
        secret: false,
    },
    ProviderFieldDef {
        key: "shadowHomePath",
        title: "Shadow home path",
        description: "Account-specific Codex home. Keeps auth.json separate while sharing state from CODEX_HOME.",
        placeholder: "~/.codex-t3/personal",
        secret: false,
    },
    ProviderFieldDef {
        key: "launchArgs",
        title: "Launch arguments",
        description: "Additional CLI arguments passed to codex app-server on session start.",
        placeholder: "",
        secret: false,
    },
];

const PROVIDER_CLAUDE_FIELDS: &[ProviderFieldDef] = &[
    ProviderFieldDef {
        key: "binaryPath",
        title: "Binary path",
        description: "Path to the Claude binary used by this instance.",
        placeholder: "claude",
        secret: false,
    },
    ProviderFieldDef {
        key: "homePath",
        title: "CLAUDE_CONFIG_DIR path",
        description: "Custom Claude home and config directory. Keeps .claude.json and .claude separate.",
        placeholder: "~/.claude",
        secret: false,
    },
    ProviderFieldDef {
        key: "autoCompactWindow",
        title: "Auto-compact after",
        description: "Compact after 100,000 to 1,000,000 tokens. Leave empty to use Claude's default.",
        placeholder: "e.g. 300000",
        secret: false,
    },
    // `idleCompaction` renders as a select after this list — the schema
    // order interleaves it before launchArgs.
    ProviderFieldDef {
        key: "launchArgs",
        title: "Launch arguments",
        description: "Additional CLI arguments passed on session start.",
        placeholder: "e.g. --chrome",
        secret: false,
    },
];

const PROVIDER_ANTIGRAVITY_FIELDS: &[ProviderFieldDef] = &[
    ProviderFieldDef {
        key: "apiKey",
        title: "API key",
        description: "Gemini or Vertex AI express key. Stored separately on the host.",
        placeholder: "Optional",
        secret: true,
    },
    ProviderFieldDef {
        key: "gcpProject",
        title: "GCP project",
        description: "Required for Gemini Enterprise. Agent Platform uses it when no API key is set.",
        placeholder: "my-project-id",
        secret: false,
    },
    ProviderFieldDef {
        key: "gcpLocation",
        title: "GCP location",
        description: "Region for Gemini Enterprise or Agent Platform.",
        placeholder: "us-central1",
        secret: false,
    },
    ProviderFieldDef {
        key: "binaryPath",
        title: "Binary path",
        description: "Custom ACP executable. Leave empty to select automatically.",
        placeholder: "Automatic",
        secret: false,
    },
];

/// `authMethod` select options — `ANTIGRAVITY_AUTH_METHODS`.
const ANTIGRAVITY_AUTH_METHODS: &[(&str, &str)] = &[
    ("oauth-personal", "Google account"),
    ("oauth-business", "Gemini Enterprise"),
    ("gemini-api-key", "Gemini API key"),
    ("agent-platform", "Agent Platform (Vertex AI)"),
];

/// Per-instance `idleCompaction` select options — `INSTANCE_IDLE_COMPACTION_MODES`.
const PROVIDER_IDLE_COMPACTION_MODES: &[(&str, &str)] = &[
    ("default", "Use default"),
    ("onReturn", "When you return"),
    ("beforeCacheExpires", "Before the cache expires (beta)"),
    ("off", "Never"),
];

/// Drivers offered by "+ Add provider" — `providerClients` order in the
/// web panel, minus ACP-registry entries (managed separately).
const PROVIDER_DRIVER_OPTIONS: &[(&str, &str)] = &[
    ("codex", "Codex"),
    ("claudeAgent", "Claude"),
    ("opencode", "OpenCode"),
    ("antigravity", "Antigravity"),
    ("cursor", "Cursor"),
    ("grok", "Grok"),
    ("kiro", "Kiro"),
    ("fx", "fx"),
    ("muse", "Muse"),
    ("pi", "Pi"),
    ("kilo", "Kilo"),
];

/// `driverKind !== "antigravity"` — custom-model add is disabled for
/// drivers the web excludes.
fn driver_allows_custom(driver: &str) -> bool {
    driver != "antigravity"
}

/// Source-control provider `kind` → brand mark; VCS `kind` uses the same
/// map ("git"/"jj").
fn source_control_icon(kind: &str) -> Icon {
    match kind {
        "git" => Icon::ScGit,
        "jj" => Icon::ScJujutsu,
        "github" => Icon::ScGithub,
        "cursor-origin" => Icon::ScCursorOrigin,
        "gitlab" => Icon::ScGitlab,
        "gitcafe" => Icon::ScGitcafe,
        "forgejo" => Icon::ScForgejo,
        "bitbucket" => Icon::ScBitbucket,
        "azure-devops" => Icon::ScAzureDevops,
        _ => Icon::ScGit,
    }
}

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
    /// Expanded detail folds for discovery rows (web `hasDetails` chevron)
    /// — keyed by provider `kind` ("github", "gitcafe", …).
    sc_expanded: HashSet<String>,
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

    // ── Providers page (web two-pane list + detail) ─────────────────────
    /// Selected instance in the right-hand detail pane (defaults to the
    /// first reported instance).
    selected_provider: Option<String>,
    /// One open select/menu on these pages at a time — string keys route
    /// `extra_pick` ("scope:project", "browser:frame-rate",
    /// "browser:profile-menu:<id>", "provider:add",
    /// "provider:<iid>:idleCompaction", …). Kept separate from the shared
    /// `OpenList` enum because settings_misc.rs is not editable here.
    extra_open: Option<String>,
    /// Per-instance TextFields keyed `"{instanceId}:{slot}"` — created in
    /// `ensure_provider_fields` (needs `&mut Context`, so they spawn from
    /// the select handler) and seeded once from the settings mirror.
    provider_fields: HashMap<String, Entity<TextField>>,
    provider_field_seeded: HashSet<String>,
    provider_field_drafts: HashMap<String, String>,
    /// Seeds whose `edit_field` write failed because creation ran inside a
    /// window dispatch (listener) — flushed by the render-side spawn.
    provider_field_pending_seeds: HashMap<String, String>,
    provider_field_subs: Vec<Subscription>,
    /// "+ Add variable" fold in the detail's Environment card.
    provider_env_add: bool,
    /// "+ Add custom model" fold in the detail's Models section.
    provider_model_add: bool,

    // ── Browser page ────────────────────────────────────────────────────
    /// "> Advanced" disclosure inside the Browsing card (viewport / zoom /
    /// appearance defaults).
    browser_advanced: bool,

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
            let field = cx
                .new(|cx| TextField::new("Words the recognizer should learn, comma separated", cx));
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
            state.extra_subs.push(cx.observe(&field, |this, field, cx| {
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
                    this.update_scoped_setting("branchNamePrefix", json!(event.0.trim()), cx);
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
                    this.update_scoped_setting("branchNameInstructions", json!(event.0.trim()), cx);
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
                    field.replace_text_in_range(Some(0..0), &branch_prefix_value, window, cx);
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
                    field.replace_text_in_range(Some(0..0), &branch_instructions_value, window, cx);
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

    // ─── Shared extra-page widgets ──────────────────────────────────────

    /// Toggle the string-keyed select/menu (`extra_open`) open/closed.
    fn extra_toggle(&mut self, key: String, cx: &mut Context<Self>) {
        let state = &mut self.misc_settings_state.extra;
        state.extra_open = if state.extra_open.as_deref() == Some(key.as_str()) {
            None
        } else {
            Some(key)
        };
        cx.notify();
    }

    /// Fold an extra-select pick into state — the `key` prefix routes the
    /// write (scope chip, browser ClientConfig fields, profile menu
    /// actions, provider config selects, "+ Add provider").
    fn extra_pick(&mut self, key: String, value: String, cx: &mut Context<Self>) {
        self.misc_settings_state.extra.extra_open = None;
        if key == "scope:project" {
            self.misc_settings_state.scope_project =
                if value.is_empty() { None } else { Some(value) };
            self.client_config.settings_scope_project =
                self.misc_settings_state.scope_project.clone();
            self.client_config.save();
        } else if key == "browser:frame-rate" {
            self.client_config.browser_recording_frame_rate = value.parse().unwrap_or(30);
            self.client_config.save();
        } else if key == "browser:viewport" {
            self.client_config.browser_default_viewport = value;
            self.client_config.save();
        } else if key == "browser:zoom" {
            self.client_config.browser_default_zoom = value;
            self.client_config.save();
        } else if key == "browser:appearance" {
            self.client_config.browser_default_appearance = value;
            self.client_config.save();
        } else if let Some(profile_id) = key.strip_prefix("browser:profile-menu:") {
            self.browser_profile_menu_pick(profile_id, &value, cx);
        } else if key == "provider:add" {
            self.add_provider_instance(&value, cx);
        } else if let Some(rest) = key.strip_prefix("provider:") {
            if let Some(instance_id) = rest.strip_suffix(":idleCompaction") {
                self.provider_config_write(instance_id, "idleCompaction", &value, cx);
            } else if let Some(instance_id) = rest.strip_suffix(":authMethod") {
                self.provider_config_write(instance_id, "authMethod", &value, cx);
            }
        }
        cx.notify();
    }

    /// Browser-profile ⋮ menu picks: make default / remove. Clearing
    /// cookies/cache needs the embedded browser surface the native client
    /// doesn't have, so the menu carries just these entries.
    fn browser_profile_menu_pick(
        &mut self,
        profile_id: &str,
        action: &str,
        cx: &mut Context<Self>,
    ) {
        match action {
            "default" => {
                self.client_config.browser_default_profile_id = profile_id.to_string();
            }
            "remove" => {
                self.client_config
                    .browser_profiles
                    .retain(|profile| profile.id != profile_id);
                if self.client_config.browser_default_profile_id == profile_id {
                    self.client_config.browser_default_profile_id = "default".to_string();
                }
            }
            _ => return,
        }
        self.client_config.save();
        cx.notify();
    }

    /// The select trigger for an `extra_open` list — same look as
    /// `select_trigger` but keyed by string so these pages don't need new
    /// `OpenList` variants.
    fn extra_trigger(
        &self,
        key: &str,
        current: SharedString,
        min_width: f32,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let key = key.to_string();
        div()
            .id(format!("extra-trigger-{key}"))
            .min_w(px(min_width))
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
                    this.extra_toggle(key.clone(), cx);
                }),
            )
    }

    /// The deferred option popup for an `extra_open` list — same chrome as
    /// the shared `select_options_list`; picking runs `extra_pick`.
    fn extra_options_popup(
        &self,
        key: &str,
        current_value: &str,
        options: &[(String, String, Option<String>)],
        cx: &mut Context<Self>,
    ) -> gpui::Deferred {
        let mut list_div = div()
            .flex()
            .flex_col()
            .min_w(px(180.))
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .shadow_2xl();
        for (index, (value, label, detail)) in options.iter().enumerate() {
            let selected = *value == current_value;
            let value = value.clone();
            let key = key.to_string();
            list_div = list_div.child(
                div()
                    .id(format!("extra-opt-{key}-{index}"))
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
                            this.extra_pick(key.clone(), value.clone(), cx);
                        }),
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
        deferred(div().absolute().top_full().right_0().pt_1().child(list_div)).with_priority(2)
    }

    /// Trigger + popup pair for an `extra_open` select.
    fn extra_select_control(
        &self,
        key: &str,
        current: SharedString,
        current_value: &str,
        options: Vec<(String, String, Option<String>)>,
        min_width: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.misc_settings_state.extra.extra_open.as_deref() == Some(key);
        div()
            .relative()
            .flex_none()
            .child(self.extra_trigger(key, current, min_width, cx))
            .when(open, |cell| {
                cell.child(self.extra_options_popup(key, current_value, &options, cx))
            })
    }

    /// A whole `setting_row` whose control is an `extra_select_control`.
    fn extra_select_row(
        &self,
        title: &'static str,
        description: &'static str,
        key: &str,
        current: SharedString,
        current_value: &str,
        options: Vec<(String, String, Option<String>)>,
        cx: &mut Context<Self>,
    ) -> Div {
        div().flex().flex_col().child(setting_row(
            title,
            description,
            self.extra_select_control(key, current, current_value, options, 160., cx),
        ))
    }

    /// The web `SettingsScopeSentence`: "Applying settings for
    /// [project chip] {on|across} [env chip]". The project chip opens the
    /// project list (same write as `OpenList::SettingsScope`); the env chip
    /// is read-only — the native client edits the connected environment.
    fn scope_sentence(&self, on_environment: bool, cx: &mut Context<Self>) -> Div {
        let pid = self.scope_project_id().map(str::to_string);
        let project_label: SharedString = match &pid {
            Some(id) => self
                .state
                .projects
                .iter()
                .find(|p| p.id.as_ref() == id.as_str())
                .map(|p| p.name.to_string())
                .unwrap_or_else(|| id.clone())
                .into(),
            None => "All projects".into(),
        };
        let current = pid.clone().unwrap_or_default();
        let mut options = vec![(
            String::new(),
            "All projects".to_string(),
            Some("Server-wide defaults every project inherits.".to_string()),
        )];
        for project in &self.state.projects {
            options.push((
                project.id.to_string(),
                project.name.to_string(),
                Some("Override the marked rows for this project.".to_string()),
            ));
        }
        let open = self.misc_settings_state.extra.extra_open.as_deref() == Some("scope:project");
        let chip =
            |id: &'static str, label: SharedString, leading: Option<Icon>, interactive: bool| {
                let mut el = div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(theme::field_surface())
                    .border_1()
                    .border_color(theme::input())
                    .text_xs()
                    .text_color(theme::fg());
                if let Some(mark) = leading {
                    el = el.child(icon(mark, 11.).text_color(theme::fg_muted()));
                }
                el = el.child(label);
                if interactive {
                    el = el
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::accent()))
                        .child(icon(Icon::ChevronDown, 11.).text_color(theme::icon_muted()));
                }
                el
            };
        let env_label: SharedString = self
            .client_config
            .active_environment
            .and_then(|ix| self.client_config.environments.get(ix))
            .map(|env| env.label.clone())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| self.endpoint_label.clone())
            .into();
        let env_chip = if on_environment {
            chip("scope-env-chip", env_label, Some(Icon::Monitor), false)
        } else {
            chip(
                "scope-env-chip",
                "All environments".into(),
                Some(Icon::Monitor),
                false,
            )
        };
        let project_chip = chip("scope-project-chip", project_label, None, true).on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _ev, _window, cx| {
                this.extra_toggle("scope:project".to_string(), cx);
            }),
        );
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(theme::fg_muted())
            .child("Applying settings for")
            .child(div().relative().child(project_chip).when(open, |cell| {
                cell.child(self.extra_options_popup("scope:project", &current, &options, cx))
            }))
            .child(if on_environment { "on" } else { "across" })
            .child(env_chip)
    }

    /// Small ghost icon button — `SettingsSection` header actions and the
    /// `SettingResetButton` (↺) affordance.
    fn extra_icon_button(
        &self,
        element_id: String,
        mark: Icon,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut AppView, &mut Context<AppView>) + 'static,
    ) -> gpui::Stateful<Div> {
        div()
            .id(element_id)
            .w(px(24.))
            .h(px(24.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .text_color(theme::fg_muted())
            .hover(|s| s.bg(theme::accent()).text_color(theme::fg()))
            .child(icon(mark, 13.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _window, cx| {
                    on_click(this, cx);
                }),
            )
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

    /// One Repositories scoped switch with the web's `SettingResetButton`
    /// (↺) beside the title while the value differs from `default_on`.
    fn sc_reset_switch_row(
        &self,
        title: &'static str,
        description: &'static str,
        scoped_key: &'static str,
        element_id: &'static str,
        default_on: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let on = self.scoped_value(scoped_key).as_bool() == Some(true);
        let mut title_row = div().flex().items_center().gap_1p5().child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::fg())
                .child(title),
        );
        if on != default_on {
            title_row = title_row.child(self.extra_icon_button(
                format!("{element_id}-reset"),
                Icon::Undo2,
                cx,
                move |this, cx| {
                    this.update_scoped_setting(scoped_key, json!(default_on), cx);
                },
            ));
        }
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
        div()
            .flex()
            .items_center()
            .gap_8()
            .px_4()
            .py_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(title_row)
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(description),
                    ),
            )
            .child(control)
    }

    /// One discovery row — the web `DiscoveryItemRow`: status dot over the
    /// brand mark, label + version code + badges on the first line, the
    /// availability/auth summary on the second, chevron + availability
    /// switch on the right.
    fn sc_discovery_row(&self, item: &Value, is_vcs: bool, cx: &mut Context<Self>) -> Div {
        let kind = item["kind"].as_str().unwrap_or_default().to_string();
        let label = item["label"].as_str().unwrap_or("item").to_string();
        let available = item["status"].as_str() == Some("available");
        let implemented = item["implemented"].as_bool() != Some(false);
        let version = item["version"].as_str().unwrap_or_default().to_string();
        let auth_status = item["auth"]["status"].as_str().unwrap_or("unknown");
        let signed_in = auth_status == "authenticated";
        let unauthenticated = auth_status == "unauthenticated";
        let has_cli = !item["executable"].as_str().unwrap_or_default().is_empty();
        let install_hint = item["installHint"].as_str().unwrap_or_default();
        let expanded = self.misc_settings_state.extra.sc_expanded.contains(&kind);

        // Summary line — `useSourceControlItemSummaries` in the web.
        let summary: String = if is_vcs {
            if !implemented {
                format!("Support for {label} is coming soon.")
            } else if available {
                "Available.".to_string()
            } else {
                format!("Not available on this server: {install_hint}")
            }
        } else if !has_cli && unauthenticated {
            format!("Available. {install_hint}")
        } else if !available {
            format!("Not available on this server: {install_hint}")
        } else if unauthenticated {
            format!(
                "{label} is not authenticated on this server. Sign in or configure credentials using the `{kind}` tool on the server host to enable change request features."
            )
        } else if signed_in {
            match (
                item["auth"]["account"].as_str(),
                item["auth"]["host"].as_str(),
            ) {
                (Some(account), Some(host)) => {
                    format!("Authenticated as {account} on {host}")
                }
                (Some(account), None) => format!("Authenticated as {account}"),
                _ => "Authenticated".to_string(),
            }
        } else {
            "Available. Status unknown.".to_string()
        };

        // Status dot: green when usable+authed, muted otherwise — the web
        // `DiscoveryItemMark` (auth → green, available → foreground,
        // missing → muted).
        let dot = if (is_vcs && available && implemented) || (!is_vcs && available && signed_in) {
            gpui::rgb(0x34d399)
        } else if available {
            theme::fg_muted()
        } else {
            theme::border()
        };

        // The web shows a chevron on rows whose definition carries extra
        // settings (GitHub sign-in / GitCafe token) — `hasDetails`.
        let has_details = !is_vcs && matches!(kind.as_str(), "github" | "gitcafe");

        let mut first_line = div().flex().items_center().gap_2().min_w_0().child(
            div()
                .flex_none()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::fg())
                .child(label.clone()),
        );
        if !version.is_empty() && available {
            first_line = first_line.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(version.clone()),
            );
        }
        if is_vcs && !implemented {
            first_line = first_line.child(
                div()
                    .flex_none()
                    .px_1p5()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .bg(theme::hover())
                    .text_color(theme::fg_muted())
                    .child("Coming Soon"),
            );
        }

        let mark = div()
            .relative()
            .w(px(20.))
            .h(px(20.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(icon(source_control_icon(&kind), 15.).text_color(theme::fg()))
            .child(
                div()
                    .absolute()
                    .top(px(-3.))
                    .left(px(-3.))
                    .w(px(7.))
                    .h(px(7.))
                    .rounded_full()
                    .bg(dot),
            );

        let kind_for_toggle = kind.clone();
        let mut row = div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .child(mark)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(first_line)
                    .child(div().text_xs().text_color(theme::fg_muted()).child(summary)),
            );
        if has_details {
            row = row.child(
                div()
                    .id(format!("sc-expand-{kind_for_toggle}"))
                    .w(px(20.))
                    .h(px(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::accent()))
                    .child(icon(
                        if expanded {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronRight
                        },
                        12.,
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            let expanded = &mut this.misc_settings_state.extra.sc_expanded;
                            if !expanded.remove(&kind_for_toggle) {
                                expanded.insert(kind_for_toggle.clone());
                            }
                            cx.notify();
                        }),
                    ),
            );
        }
        // The availability switch is a read-out in the web (no handler);
        // VCS rows without an implementation omit it entirely.
        if !is_vcs || implemented {
            row = row.child(div().flex_none().child(switch_control(
                available && (is_vcs || !unauthenticated || signed_in || !has_cli),
            )));
        }
        let mut cell = div().flex().flex_col().child(row);

        // Expanded details — GitHub hosts the device-flow sign-in, GitCafe
        // the token hint (token entry is web/desktop-only server-side; the
        // install hint already names the `cafe` flow).
        if expanded && kind == "github" {
            let mut details = div()
                .flex()
                .flex_col()
                .gap_2()
                .px_4()
                .pb_3()
                .pt_1()
                .border_t_1()
                .border_color(theme::card_divider());
            if signed_in {
                details = details.child(
                    div()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child("Signed in through the GitHub CLI on this server."),
                );
            } else {
                let mut sign_in = div()
                    .id("sc-sign-in")
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .bg(theme::hover())
                    .text_color(theme::fg())
                    .hover(|s| s.bg(theme::selected()))
                    .child(if self.misc_settings_state.extra.sc_busy {
                        "Signing in…"
                    } else {
                        "Sign in with GitHub"
                    });
                if self.misc_settings_state.extra.sc_busy {
                    sign_in = sign_in.opacity(0.4);
                } else {
                    sign_in = sign_in.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            this.source_control_sign_in(cx);
                        }),
                    );
                }
                details = details
                    .child(div().text_xs().text_color(theme::fg_muted()).child(
                        "Sign in on this device — the code authorizes the `gh` CLI on the server.",
                    ))
                    .child(div().flex().child(sign_in));
            }
            if let Some((code, uri)) = &self.misc_settings_state.extra.sc_signin_prompt {
                details = details.child(
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
            cell = cell.child(details);
        } else if expanded {
            cell = cell.child(
                div()
                    .px_4()
                    .pb_3()
                    .pt_1()
                    .border_t_1()
                    .border_color(theme::card_divider())
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Token and account settings for this provider are managed in the desktop and web clients."),
            );
        }
        cell
    }

    pub(crate) fn render_source_control_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_source_control(cx);
        self.maybe_load_misc_settings(cx);
        self.maybe_prepare_extra(cx);
        let state = &self.misc_settings_state.extra;
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

        // Scope sentence — the web `SettingsScopeSentence` above the page
        // ("… across All environments" for these server+VCS surfaces).
        page = page.child(self.scope_sentence(false, cx));

        // ── Repositories — web `ProjectDefaultsSettings` repo rows. ────
        let mut repo_rows: Vec<Div> = Vec::new();
        if connected {
            repo_rows.push(self.sc_reset_switch_row(
                "Automatically pull",
                "Keeps the default branch current when the checkout has no local changes or commits. Projects can override it.",
                "defaultAutoPull",
                "sc-auto-pull",
                false,
                cx,
            ));
            repo_rows.push(self.sc_reset_switch_row(
                "Remove agent credits when merging",
                "Remove recognized agent credit lines from merge and squash messages on hosts that support it, keeping human co-authors. Includes auto-merge. Excludes merge queues, stack merges, and existing commits.",
                "removeAgentCreditsOnMerge",
                "sc-agent-credits",
                false,
                cx,
            ));
            let merge_method = self.scoped_value("pullRequestMergeMethod");
            let merge_value = merge_method.as_str().unwrap_or("last");
            let merge_label = match merge_value {
                "merge" => "Merge commit",
                "squash" => "Squash and merge",
                "rebase" => "Rebase and merge",
                _ => "Last selected",
            };
            repo_rows.push(self.scoped_select_setting(
                "Default merge method",
                "Pull requests start with this method. Last selected reuses whatever you chose most recently on this device.",
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
        } else {
            repo_rows.push(setting_row(
                "Repositories",
                "Source control discovery runs on the connected environment.",
                div()
                    .text_sm()
                    .text_color(theme::fg_muted())
                    .child("Not connected"),
            ));
        }
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Repositories"))
                .child(settings_card(repo_rows)),
        );

        // ── Version Control — `server.discoverSourceControl` VCS rows. ─
        let discovery = state.sc_discovery.clone().unwrap_or(Value::Null);
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .min_h(px(28.))
                        .pb_2p5()
                        .px_4()
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_base()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::fg())
                                .child("Version Control"),
                        )
                        .when(connected, |s| {
                            s.child(self.extra_icon_button(
                                "sc-rescan".to_string(),
                                Icon::RefreshCw,
                                cx,
                                |this, cx| this.rescan_source_control(cx),
                            ))
                        }),
                )
                .child({
                    let mut vcs_rows: Vec<Div> = Vec::new();
                    if let Some(vcs) = discovery["versionControlSystems"].as_array() {
                        for item in vcs {
                            vcs_rows.push(self.sc_discovery_row(item, true, cx));
                        }
                    }
                    if vcs_rows.is_empty() {
                        vcs_rows.push(
                            div()
                                .px_4()
                                .py_3()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child(if state.sc_disc_loading.get() {
                                    "Scanning the server for git / jj / CLIs…"
                                } else if connected {
                                    "No discovery result yet — the rescan button checks the server's tools."
                                } else {
                                    "Connect to a server to see version control tools."
                                }),
                        );
                    }
                    settings_card(vcs_rows)
                }),
        );

        // ── Source control providers — provider discovery rows. ────────
        let providers = discovery["sourceControlProviders"].as_array().cloned();
        let mut provider_rows: Vec<Div> = Vec::new();
        if let Some(items) = providers {
            for item in &items {
                provider_rows.push(self.sc_discovery_row(item, false, cx));
            }
        }
        if provider_rows.is_empty() {
            provider_rows.push(
                div()
                    .px_4()
                    .py_3()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(if state.sc_disc_loading.get() {
                        "Scanning the server for source control providers…"
                    } else {
                        "No source control providers detected on the server."
                    }),
            );
        }
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Source control providers"))
                .child(settings_card(provider_rows)),
        );

        if let Some(note) = &state.sc_note {
            page = page.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(note.clone()),
            );
        }

        // ── Commit & pull request writing (scoped ServerSettings). ─────
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Commit & pull request writing"))
                .child(self.render_sc_writing_rows(cx)),
        );

        page
    }

    /// The writing-settings card — `sourceControlWritingStyle` (+ the
    /// custom-instructions field in "custom" mode) and the writer model.
    fn render_sc_writing_rows(&self, cx: &mut Context<Self>) -> Div {
        let state = &self.misc_settings_state.extra;
        let mut rows: Vec<Div> = Vec::new();

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
        rows.push(
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
        if mode == "custom"
            && let Some(field) = state.sc_instructions_field.clone()
        {
            rows.push(setting_row(
                "Custom instructions",
                "Enter saves. Applied to generated commit messages and PR text.",
                self.boxed_field(field, 220.),
            ));
        }

        let writer_selection = self.scoped_value("sourceControlWriterModelSelection");
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
            Self::selection_label(&writer_selection).into()
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
        rows.push(self.scoped_select_setting(
            "Writing model",
            "Model used for generated source control text.",
            OpenList::ScWriterModel,
            "sourceControlWriterModelSelection",
            writer_label,
            &writer_value,
            writer_options,
            cx,
        ));

        settings_card(rows)
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
        } else if preset["kind"].as_str() != Some("at") || preset["day"].as_str() != Some(value) {
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
        let mut cell = div().relative().flex_none().child(
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
                div().absolute().top_full().left_0().pt_1().child(popup),
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
        new_thread_rows.push(
            self.scoped_select_setting(
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
            ),
        );

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
            controls = controls.child(
                self.snooze_select(
                    row,
                    OpenList::SnoozeWhen(row),
                    when_label.into(),
                    &when_value,
                    &SNOOZE_WHEN
                        .iter()
                        .map(|(value, label)| (value.to_string(), label.to_string(), None))
                        .collect::<Vec<_>>(),
                    cx,
                ),
            );
            if kind == "at" {
                let time = preset["time"].as_str().unwrap_or("09:00").to_string();
                let mut time_options: Vec<(String, String, Option<String>)> = SNOOZE_TIMES
                    .iter()
                    .map(|value| (format!("time:{value}"), (*value).to_string(), None))
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
                let mut duration_options: Vec<(String, String, Option<String>)> = SNOOZE_DURATIONS
                    .iter()
                    .map(|(value, label)| (format!("min:{value}"), (*label).to_string(), None))
                    .collect();
                let in_options = SNOOZE_DURATIONS.iter().any(|(value, _)| *value == minutes);
                if !in_options {
                    duration_options
                        .insert(0, (format!("min:{minutes}"), format_minutes(minutes), None));
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
        let snooze_section =
            div()
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
                    div().pt_2().text_xs().text_color(theme::fg_muted()).child(
                        "Options the thread's snooze menu offers, in the order they appear.",
                    ),
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
            settle_rows.push(
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
            text_rows.push(
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
        composer_rows.push(
            self.select_setting(
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
            ),
        );
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
        diff_rows.push(
            self.select_setting(
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
            ),
        );

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
                .child(settings_card(vec![
                    self.select_setting(
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
                    ),
                ])),
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
        if let Some(field) = self
            .misc_settings_state
            .extra
            .speech_correction_field
            .clone()
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
        rows.push(
            self.select_setting(
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
            ),
        );
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
            rows.push(
                self.select_setting(
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
                ),
            );
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

    /// One row in the Browser profiles sub-card — name + "Default" badge +
    /// the ⋮ menu (Set as default / Remove for custom profiles).
    fn browser_profile_row(&self, index: usize, cx: &mut Context<Self>) -> Div {
        let profile = self.client_config.browser_profiles[index].clone();
        let is_default = self.client_config.browser_default_profile_id == profile.id;
        let built_in = profile.id == "default";
        let menu_key = format!("browser:profile-menu:{}", profile.id);
        let menu_open =
            self.misc_settings_state.extra.extra_open.as_deref() == Some(menu_key.as_str());
        let mut options = vec![("default".to_string(), "Set as default".to_string(), None)];
        if !built_in {
            options.push(("remove".to_string(), "Remove profile".to_string(), None));
        }
        let profile_id = profile.id.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .when(index > 0, |row| {
                row.border_t_1().border_color(theme::card_divider())
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .text_sm()
                            .text_color(theme::fg())
                            .truncate()
                            .child(profile.name),
                    )
                    .when(is_default, |name| {
                        name.child(
                            div()
                                .flex_none()
                                .px_1p5()
                                .py_0p5()
                                .rounded_md()
                                .text_xs()
                                .bg(theme::selected())
                                .text_color(theme::primary())
                                .child("Default"),
                        )
                    }),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(
                        div()
                            .id(format!("browser-profile-menu-{profile_id}"))
                            .w(px(22.))
                            .h(px(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(theme::fg_muted())
                            .hover(|s| s.bg(theme::accent()))
                            .child(icon(Icon::Ellipsis, 14.))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    this.extra_toggle(menu_key.clone(), cx);
                                }),
                            ),
                    )
                    .when(menu_open, |cell| {
                        cell.child(self.extra_options_popup(
                            &format!("browser:profile-menu:{profile_id}"),
                            "",
                            &options,
                            cx,
                        ))
                    }),
            )
    }

    pub(crate) fn render_browser_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_misc_settings(cx);
        self.maybe_prepare_extra(cx);
        let cfg = &self.client_config;
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
        page = page.child(self.scope_sentence(false, cx));

        // ── Agent access ──
        let mut agent_rows: Vec<Div> = Vec::new();
        if self.commands.is_some() {
            agent_rows.push(self.scoped_switch_row(
                "Agent browser access",
                "Allow agents to use the shared browser. Projects can override it.",
                "enableAgentBrowserAccess",
                "browser-agent-access",
                cx,
            ));
        } else {
            agent_rows.push(setting_row(
                "Agent browser access",
                "Allow agents to use the shared browser. Projects can override it.",
                div()
                    .text_sm()
                    .text_color(theme::fg_muted())
                    .child("Not connected"),
            ));
        }
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Agent access"))
                .child(settings_card(agent_rows)),
        );

        // ── Browsing ──
        let mut browsing_rows: Vec<Div> = Vec::new();

        // Browser profiles — `ClientSettings.browserProfiles`: a sub-card
        // inside the row's control area listing persistent profiles.
        let mut profiles_list = div()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(theme::card_border())
            .bg(theme::field_surface());
        for index in 0..cfg.browser_profiles.len() {
            profiles_list = profiles_list.child(self.browser_profile_row(index, cx));
        }
        browsing_rows.push(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .px_4()
                .py_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_8()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme::fg())
                                        .child("Browser profiles"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::fg_muted())
                                        .child("Profiles separate cookies and logins. Incognito data is cleared when the app closes."),
                                ),
                        )
                        .child(
                            div()
                                .id("browser-add-profile")
                                .flex_none()
                                .px_3()
                                .py_1p5()
                                .rounded_lg()
                                .border_1()
                                .border_color(theme::input())
                                .text_xs()
                                .cursor_pointer()
                                .text_color(theme::fg())
                                .hover(|s| s.bg(theme::accent()))
                                .child("+ Add profile")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _ev, _window, cx| {
                                        let count = this.client_config.browser_profiles.len();
                                        this.client_config.browser_profiles.push(
                                            crate::config::BrowserProfile {
                                                id: format!(
                                                    "profile-{}",
                                                    uuid::Uuid::new_v4()
                                                ),
                                                name: format!("Profile {count}"),
                                            },
                                        );
                                        this.client_config.save();
                                        cx.notify();
                                    }),
                                ),
                        ),
                )
                .child(profiles_list),
        );

        let target = cfg.browser_link_target.clone();
        let target_label = if target == "app" {
            "T3 Code"
        } else {
            "Your default browser"
        };
        browsing_rows.push(self.select_setting(
            "Open links in",
            "Where links in the chat and terminal open. Hold Ctrl while clicking a link to open it in your default browser either way.",
            OpenList::BrowserLink,
            target_label.into(),
            &target,
            BROWSER_LINK_TARGETS
                .iter()
                .map(|(value, label)| (value.to_string(), label.to_string(), None))
                .collect(),
            cx,
        ));

        browsing_rows.push(self.client_switch(
            "Auto-show floating preview",
            "Show the floating preview when an agent opens a browser or device unless the agent says otherwise.",
            "browser-auto-preview",
            cfg.browser_auto_show_floating_preview,
            |this, on| this.client_config.browser_auto_show_floating_preview = on,
            cx,
        ));

        let engine = cfg.browser_search_engine.clone();
        let engine_label = BROWSER_SEARCH_ENGINES
            .iter()
            .find(|(value, _)| *value == engine)
            .map(|(_, label)| *label)
            .unwrap_or("DuckDuckGo");
        browsing_rows.push(
            self.select_setting(
                "Search engine",
                "What the address bar searches when you type something that isn't a URL.",
                OpenList::BrowserEngine,
                engine_label.into(),
                &engine,
                BROWSER_SEARCH_ENGINES
                    .iter()
                    .map(|(value, label)| (value.to_string(), label.to_string(), None))
                    .collect(),
                cx,
            ),
        );

        // "> Advanced" disclosure — viewport / zoom / appearance defaults.
        let advanced_open = self.misc_settings_state.extra.browser_advanced;
        browsing_rows.push(
            div().flex().flex_col().child(
                div()
                    .id("browser-advanced")
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_2p5()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.text_color(theme::fg()))
                    .child(icon(
                        if advanced_open {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronRight
                        },
                        12.,
                    ))
                    .child("Advanced")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            let open = this.misc_settings_state.extra.browser_advanced;
                            this.misc_settings_state.extra.browser_advanced = !open;
                            cx.notify();
                        }),
                    ),
            ),
        );
        if advanced_open {
            let viewport = cfg.browser_default_viewport.clone();
            let viewport_label = BROWSER_VIEWPORTS
                .iter()
                .find(|(value, _, _)| *value == viewport)
                .map(|(_, label, _)| *label)
                .unwrap_or("Fill panel");
            browsing_rows.push(self.extra_select_row(
                "Default viewport",
                "Tab size for you and agents. Fill fits the panel; other sizes show the device toolbar.",
                "browser:viewport",
                viewport_label.into(),
                &viewport,
                BROWSER_VIEWPORTS
                    .iter()
                    .map(|(value, label, detail)| {
                        (
                            value.to_string(),
                            label.to_string(),
                            if detail.is_empty() {
                                None
                            } else {
                                Some(detail.to_string())
                            },
                        )
                    })
                    .collect(),
                cx,
            ));
            let zoom = cfg.browser_default_zoom.clone();
            let zoom_label = BROWSER_ZOOMS
                .iter()
                .find(|(value, _)| *value == zoom)
                .map(|(_, label)| *label)
                .unwrap_or("100%");
            browsing_rows.push(
                self.extra_select_row(
                    "Default zoom",
                    "Page zoom applied to new browser tabs.",
                    "browser:zoom",
                    zoom_label.into(),
                    &zoom,
                    BROWSER_ZOOMS
                        .iter()
                        .map(|(value, label)| (value.to_string(), label.to_string(), None))
                        .collect(),
                    cx,
                ),
            );
            let appearance = cfg.browser_default_appearance.clone();
            let appearance_label = BROWSER_APPEARANCES
                .iter()
                .find(|(value, _)| *value == appearance)
                .map(|(_, label)| *label)
                .unwrap_or("System");
            browsing_rows.push(
                self.extra_select_row(
                    "Default appearance",
                    "The color scheme pages are told to prefer. System follows your OS setting.",
                    "browser:appearance",
                    appearance_label.into(),
                    &appearance,
                    BROWSER_APPEARANCES
                        .iter()
                        .map(|(value, label)| (value.to_string(), label.to_string(), None))
                        .collect(),
                    cx,
                ),
            );
        }
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Browsing"))
                .child(settings_card(browsing_rows)),
        );

        // ── Recordings ──
        let frame_rate = cfg.browser_recording_frame_rate.to_string();
        let frame_label = BROWSER_FRAME_RATES
            .iter()
            .find(|(value, _)| *value == frame_rate)
            .map(|(_, label)| *label)
            .unwrap_or("30 fps");
        let recording_rows: Vec<Div> = vec![
            self.extra_select_row(
                "Browser recording frame rate",
                "Maximum recording rate. 30 fps saves CPU and storage; 60 fps is smoother.",
                "browser:frame-rate",
                frame_label.into(),
                &frame_rate,
                BROWSER_FRAME_RATES
                    .iter()
                    .map(|(value, label)| (value.to_string(), label.to_string(), None))
                    .collect(),
                cx,
            ),
            self.client_switch(
                "Show key presses in recordings",
                "Show pressed keys and shortcuts in new recordings. Password fields are excluded.",
                "browser-rec-keys",
                cfg.browser_recording_show_key_presses,
                |this, on| this.client_config.browser_recording_show_key_presses = on,
                cx,
            ),
            self.client_switch(
                "Show mouse presses in recordings",
                "Highlight mouse presses and held buttons in new recordings.",
                "browser-rec-mouse",
                cfg.browser_recording_show_mouse_presses,
                |this, on| this.client_config.browser_recording_show_mouse_presses = on,
                cx,
            ),
        ];
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(settings_section_title("Recordings"))
                .child(settings_card(recording_rows)),
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
            branch_rows.push(
                self.scoped_select_setting(
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
                ),
            );
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
    /// The driver's runtime/config text fields — mirrors the
    /// `providerSettingsForm` order of each settings schema.
    fn provider_field_defs(driver: &str) -> &'static [ProviderFieldDef] {
        match driver {
            "codex" => PROVIDER_CODEX_FIELDS,
            "claudeAgent" => PROVIDER_CLAUDE_FIELDS,
            "antigravity" => PROVIDER_ANTIGRAVITY_FIELDS,
            "grok" | "kiro" | "fx" | "muse" | "pi" | "opencode" | "devin" | "devReplay"
            | "kilo" | "cursor" | "acpRegistry" => &[],
            _ => &[],
        }
    }

    /// Driver → simple binaryPath-only field sets (shared shape).
    fn provider_simple_fields(driver: &str) -> &'static [ProviderFieldDef] {
        const BINARY_ONLY: &[(&str, &[ProviderFieldDef])] = &[];
        let _ = BINARY_ONLY;
        match driver {
            "grok" => &[ProviderFieldDef {
                key: "binaryPath",
                title: "Binary path",
                description: "Path to the Grok CLI binary.",
                placeholder: "grok",
                secret: false,
            }],
            "kiro" => &[ProviderFieldDef {
                key: "binaryPath",
                title: "Binary path",
                description: "Path to the Kiro CLI binary.",
                placeholder: "kiro-cli",
                secret: false,
            }],
            "fx" => &[ProviderFieldDef {
                key: "binaryPath",
                title: "Binary path",
                description: "Path to the fx binary. All fx instances on this machine share its fx login.",
                placeholder: "fx",
                secret: false,
            }],
            "muse" => &[ProviderFieldDef {
                key: "binaryPath",
                title: "Binary path",
                description: "Path to the Muse Code binary on this environment. Sign in with muse login on that host.",
                placeholder: "muse",
                secret: false,
            }],
            "pi" => &[
                ProviderFieldDef {
                    key: "binaryPath",
                    title: "Binary path",
                    description: "Path to the Pi coding agent binary.",
                    placeholder: "pi",
                    secret: false,
                },
                ProviderFieldDef {
                    key: "launchArgs",
                    title: "Launch arguments",
                    description: "Additional CLI arguments passed to pi --mode rpc on session start.",
                    placeholder: "",
                    secret: false,
                },
            ],
            "opencode" => &[
                ProviderFieldDef {
                    key: "binaryPath",
                    title: "Binary path",
                    description: "Path to the OpenCode binary.",
                    placeholder: "opencode",
                    secret: false,
                },
                ProviderFieldDef {
                    key: "serverUrl",
                    title: "Server URL",
                    description: "Leave blank to let T3 Code spawn the server when needed.",
                    placeholder: "http://127.0.0.1:4096",
                    secret: false,
                },
                ProviderFieldDef {
                    key: "serverPassword",
                    title: "Server password",
                    description: "Stored separately on the host.",
                    placeholder: "Optional",
                    secret: true,
                },
            ],
            _ => &[],
        }
    }

    /// All runtime fields for a driver (schema order — Claude interleaves
    /// the `idleCompaction` select, Antigravity the `authMethod` select;
    /// both render separately below).
    fn provider_runtime_fields(driver: &str) -> &'static [ProviderFieldDef] {
        let fields = Self::provider_field_defs(driver);
        if !fields.is_empty() {
            return fields;
        }
        Self::provider_simple_fields(driver)
    }

    /// `getProviderVersionLabel` — `v{semver}` plus the Antigravity
    /// `agy_acp_server_YYYY_MM_DD_*` normalization.
    fn provider_version_label(version: &str) -> Option<String> {
        if version.is_empty() {
            return None;
        }
        if let Some(rest) = version.strip_prefix("agy_acp_server_") {
            let mut parts = rest.split('_');
            if let (Some(year), Some(month), Some(day)) = (parts.next(), parts.next(), parts.next())
            {
                let candidate = parts.next().unwrap_or_default();
                return Some(if candidate.is_empty() {
                    format!("{year}-{month}-{day}")
                } else {
                    format!("{year}-{month}-{day} {candidate}")
                });
            }
        }
        Some(
            if version.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                format!("v{version}")
            } else {
                version.to_string()
            },
        )
    }

    /// `getProviderSummary` — headline + optional message + dot color for
    /// the list row and the detail's Status line.
    fn provider_status_summary(
        provider: &Value,
        enabled: bool,
    ) -> (String, Option<String>, gpui::Rgba) {
        let message = provider["message"]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        let status = provider["status"].as_str().unwrap_or_default();
        let installed = provider["installed"].as_bool() != Some(false);
        let auth_status = provider["auth"]["status"].as_str().unwrap_or_default();
        let auth_label = provider["auth"]["label"]
            .as_str()
            .or_else(|| provider["auth"]["type"].as_str())
            .unwrap_or_default();
        let antigravity_unchecked = provider["driver"].as_str() == Some("antigravity")
            && installed
            && status == "warning"
            && (auth_status.is_empty() || auth_status == "unknown")
            && matches!(
                provider["auth"]["type"].as_str(),
                None | Some("oauth-personal") | Some("oauth-business")
            );
        let muted = theme::fg_muted();
        let warning = theme::warning();
        if !enabled || status == "disabled" {
            return (
                "Disabled".to_string(),
                Some(message.unwrap_or_else(|| {
                    "This provider is installed but disabled for new sessions in T3 Code."
                        .to_string()
                })),
                muted,
            );
        }
        if !installed {
            return (
                "Not found".to_string(),
                Some(message.unwrap_or_else(|| "CLI not detected on PATH.".to_string())),
                warning,
            );
        }
        if auth_status == "unauthenticated" {
            return (
                if auth_label.is_empty() {
                    "Not authenticated".to_string()
                } else {
                    format!("Not authenticated · {auth_label}")
                },
                message,
                warning,
            );
        }
        if antigravity_unchecked {
            return (
                "Installed · Sign-in not checked".to_string(),
                message,
                muted,
            );
        }
        if status == "warning" {
            return (
                "Needs attention".to_string(),
                Some(message.unwrap_or_else(|| {
                    "The provider is installed, but the server could not fully verify it."
                        .to_string()
                })),
                warning,
            );
        }
        if status == "error" {
            return (
                "Unavailable".to_string(),
                Some(
                    message
                        .unwrap_or_else(|| "The provider failed its startup checks.".to_string()),
                ),
                danger(),
            );
        }
        if auth_status == "authenticated" {
            return (
                if auth_label.is_empty() {
                    "Authenticated".to_string()
                } else {
                    format!("Authenticated · {auth_label}")
                },
                message,
                theme::success(),
            );
        }
        ("Available".to_string(), message, theme::success())
    }

    // ─── Provider instance writes ───────────────────────────────────────

    /// Merge `patch` into `providerInstances[{instance_id}]` and send the
    /// whole record — the TS server merges per entry, but the native mock
    /// server shallow-merges top-level keys, so the full map is the only
    /// write both honor.
    fn provider_instances_write(
        &mut self,
        instance_id: &str,
        patch: serde_json::Map<String, Value>,
        cx: &mut Context<Self>,
    ) {
        let mut instances = self.misc_settings_state.settings["providerInstances"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut entry = instances
            .get(instance_id)
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        for (key, value) in patch {
            if value.is_null() {
                entry.remove(&key);
            } else {
                entry.insert(key, value);
            }
        }
        instances.insert(instance_id.to_string(), Value::Object(entry));
        self.update_server_settings(json!({ "providerInstances": Value::Object(instances) }), cx);
    }

    /// Write one `providerInstances.{id}.config.{key}` string (empty omits
    /// the key — `clearWhenEmpty: "omit"`).
    fn provider_config_write(
        &mut self,
        instance_id: &str,
        key: &str,
        value: &str,
        cx: &mut Context<Self>,
    ) {
        let mut config =
            self.misc_settings_state.settings["providerInstances"][instance_id]["config"]
                .as_object()
                .cloned()
                .unwrap_or_default();
        if value.is_empty() {
            config.remove(key);
        } else {
            config.insert(key.to_string(), json!(value));
        }
        let mut patch = serde_json::Map::new();
        patch.insert("config".to_string(), Value::Object(config));
        self.provider_instances_write(instance_id, patch, cx);
    }

    /// Replace `providerInstances.{id}.environment` (add/remove variable).
    fn provider_environment_write(
        &mut self,
        instance_id: &str,
        variables: Vec<Value>,
        cx: &mut Context<Self>,
    ) {
        let mut patch = serde_json::Map::new();
        patch.insert("environment".to_string(), json!(variables));
        self.provider_instances_write(instance_id, patch, cx);
    }

    /// "+ Add provider" — create `providerInstances[{driver}]` (or a
    /// `-2`/`-3` suffix when taken) enabled per the driver's default.
    fn add_provider_instance(&mut self, driver: &str, cx: &mut Context<Self>) {
        let existing: HashSet<String> = self.misc_settings_state.settings["providerInstances"]
            .as_object()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default();
        let reported: HashSet<String> = self
            .misc_settings_state
            .providers
            .iter()
            .chain(self.composer_state.config_providers.iter())
            .filter_map(|provider| provider["instanceId"].as_str().map(str::to_string))
            .collect();
        let mut instance_id = driver.to_string();
        let mut suffix = 2;
        while existing.contains(&instance_id) || reported.contains(&instance_id) {
            instance_id = format!("{driver}-{suffix}");
            suffix += 1;
        }
        let enabled = !matches!(
            driver,
            "cursor" | "grok" | "fx" | "kiro" | "muse" | "pi" | "opencode" | "antigravity"
        );
        let mut patch = serde_json::Map::new();
        patch.insert("driver".to_string(), json!(driver));
        patch.insert("enabled".to_string(), json!(enabled));
        patch.insert("config".to_string(), json!({}));
        self.provider_instances_write(&instance_id.clone(), patch, cx);
        self.select_provider_instance(instance_id, cx);
    }

    /// The header reset (↺) — drop the stored `providerInstances` entry so
    /// the instance falls back to its reported defaults.
    fn reset_provider_instance(&mut self, instance_id: String, cx: &mut Context<Self>) {
        let mut instances = self.misc_settings_state.settings["providerInstances"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        if instances.remove(&instance_id).is_none() {
            return;
        }
        self.update_server_settings(json!({ "providerInstances": Value::Object(instances) }), cx);
    }

    // ─── Provider detail TextFields ─────────────────────────────────────

    /// Select an instance in the list + create/seed its detail fields.
    fn select_provider_instance(&mut self, instance_id: String, cx: &mut Context<Self>) {
        self.misc_settings_state.extra.selected_provider = Some(instance_id.clone());
        self.misc_settings_state.extra.provider_env_add = false;
        self.misc_settings_state.extra.provider_model_add = false;
        self.ensure_provider_fields(&instance_id, cx);
        cx.notify();
    }

    /// Create + seed + wire the per-instance fields (`"{iid}|{slot}"` keys —
    /// `|` separates because slots contain `:`).
    fn ensure_provider_fields(&mut self, instance_id: &str, cx: &mut Context<Self>) {
        let provider = self
            .misc_settings_state
            .providers
            .iter()
            .chain(self.composer_state.config_providers.iter())
            .find(|p| p["instanceId"].as_str() == Some(instance_id))
            .cloned();
        let driver = provider
            .as_ref()
            .and_then(|p| p["driver"].as_str())
            .unwrap_or(instance_id)
            .to_string();
        let config =
            self.misc_settings_state.settings["providerInstances"][instance_id]["config"].clone();

        let mut slots: Vec<(String, String, String)> = Vec::new(); // (slot, placeholder, seed)
        let display_seed =
            self.misc_settings_state.settings["providerInstances"][instance_id]["displayName"]
                .as_str()
                .map(str::to_string)
                .or_else(|| {
                    provider.as_ref().and_then(|p| {
                        p["displayName"]
                            .as_str()
                            .or_else(|| p["name"].as_str())
                            .map(str::to_string)
                    })
                })
                .unwrap_or_default();
        slots.push(("display_name".to_string(), "Name".to_string(), display_seed));
        for field in Self::provider_runtime_fields(&driver) {
            // Password fields never seed — secret values are stored
            // separately on the host and come back redacted; typing a new
            // value and pressing Enter replaces it.
            let seed = if field.secret {
                String::new()
            } else {
                config[field.key].as_str().unwrap_or_default().to_string()
            };
            slots.push((
                format!("cfg:{}", field.key),
                field.placeholder.to_string(),
                seed,
            ));
        }
        // Utility fields — drafts only (they write on their Add/Enter
        // handlers, not per-keystroke).
        for (slot, placeholder) in [
            ("model_filter", "Filter models"),
            ("model_add", "model-slug"),
            ("env_name", "VARIABLE_NAME"),
            ("env_value", "Value"),
        ] {
            slots.push((slot.to_string(), placeholder.to_string(), String::new()));
        }

        for (slot, placeholder, seed) in slots {
            let key = format!("{instance_id}|{slot}");
            if self
                .misc_settings_state
                .extra
                .provider_fields
                .contains_key(&key)
            {
                continue;
            }
            let field = cx.new(|cx| TextField::new(placeholder.clone(), cx));
            let draft_key = key.clone();
            self.misc_settings_state
                .extra
                .provider_field_subs
                .push(cx.observe(&field, move |this, field, cx| {
                    this.misc_settings_state
                        .extra
                        .provider_field_drafts
                        .insert(draft_key.clone(), settings_misc::field_text(&field, cx));
                    cx.notify();
                }));
            let submit_key = key.clone();
            self.misc_settings_state
                .extra
                .provider_field_subs
                .push(
                    cx.subscribe(&field, move |this, _field, _event: &PromptSubmitted, cx| {
                        this.provider_field_submit(submit_key.clone(), cx);
                    }),
                );
            if !seed.is_empty() {
                self.misc_settings_state
                    .extra
                    .provider_field_drafts
                    .insert(key.clone(), seed.clone());
                let applied = edit_field(&field, cx, |field, window, cx| {
                    field.reset(window, cx);
                    field.replace_text_in_range(Some(0..0), &seed, window, cx);
                });
                if applied.is_none() {
                    self.misc_settings_state
                        .extra
                        .provider_field_pending_seeds
                        .insert(key.clone(), seed);
                }
            }
            self.misc_settings_state
                .extra
                .provider_fields
                .insert(key.clone(), field);
            self.misc_settings_state
                .extra
                .provider_field_seeded
                .insert(key);
        }
    }

    /// Enter-key handler for the per-instance fields — the slot decides the
    /// write (display name, config value, env add, custom-model add).
    fn provider_field_submit(&mut self, key: String, cx: &mut Context<Self>) {
        let Some((instance_id, slot)) = key.split_once('|') else {
            return;
        };
        let instance_id = instance_id.to_string();
        let slot = slot.to_string();
        let draft = self
            .misc_settings_state
            .extra
            .provider_field_drafts
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .trim()
            .to_string();
        if slot == "display_name" {
            let mut patch = serde_json::Map::new();
            patch.insert(
                "displayName".to_string(),
                if draft.is_empty() {
                    Value::Null
                } else {
                    json!(draft)
                },
            );
            self.provider_instances_write(&instance_id, patch, cx);
        } else if let Some(config_key) = slot.strip_prefix("cfg:") {
            self.provider_config_write(&instance_id, config_key, &draft, cx);
        } else if slot == "env_name" || slot == "env_value" {
            self.provider_env_add_submit(&instance_id, cx);
        } else if slot == "model_add" {
            self.provider_model_add_submit(&instance_id, cx);
        }
    }

    /// `providerInstances.{id}.environment` — stored entries only (the
    /// server merges them with reported ones at runtime).
    fn provider_environment(&self, instance_id: &str) -> Vec<Value> {
        self.misc_settings_state.settings["providerInstances"][instance_id]["environment"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// "+ Add variable" submit — appends `{name, value, sensitive:false}`.
    fn provider_env_add_submit(&mut self, instance_id: &str, cx: &mut Context<Self>) {
        let name_key = format!("{instance_id}|env_name");
        let value_key = format!("{instance_id}|env_value");
        let name = self
            .misc_settings_state
            .extra
            .provider_field_drafts
            .get(&name_key)
            .cloned()
            .unwrap_or_default()
            .trim()
            .to_string();
        if name.is_empty() {
            return;
        }
        let value = self
            .misc_settings_state
            .extra
            .provider_field_drafts
            .get(&value_key)
            .cloned()
            .unwrap_or_default();
        let mut variables = self.provider_environment(instance_id);
        if let Some(existing) = variables
            .iter_mut()
            .find(|v| v["name"].as_str() == Some(name.as_str()))
        {
            existing["value"] = json!(value);
        } else {
            variables.push(json!({ "name": name, "value": value, "sensitive": false }));
        }
        self.provider_environment_write(instance_id, variables, cx);
        self.misc_settings_state.extra.provider_env_add = false;
        for key in [name_key, value_key] {
            if let Some(field) = self
                .misc_settings_state
                .extra
                .provider_fields
                .get(&key)
                .cloned()
            {
                edit_field(&field, cx, |field, window, cx| field.reset(window, cx));
            }
            self.misc_settings_state
                .extra
                .provider_field_drafts
                .insert(key, String::new());
        }
        cx.notify();
    }

    /// `config.customModels` as `{slug, name}` objects (string entries
    /// promote to objects for display).
    fn provider_custom_models(&self, instance_id: &str) -> Vec<Value> {
        self.misc_settings_state.settings["providerInstances"][instance_id]["config"]
            ["customModels"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        if let Some(slug) = item.as_str() {
                            Some(json!({ "slug": slug }))
                        } else if item["slug"].as_str().is_some() {
                            Some(item.clone())
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Write `config.customModels` (object entries keep `{slug, name}`).
    fn provider_custom_models_write(
        &mut self,
        instance_id: &str,
        models: Vec<Value>,
        cx: &mut Context<Self>,
    ) {
        let mut config =
            self.misc_settings_state.settings["providerInstances"][instance_id]["config"]
                .as_object()
                .cloned()
                .unwrap_or_default();
        config.insert("customModels".to_string(), json!(models));
        let mut patch = serde_json::Map::new();
        patch.insert("config".to_string(), Value::Object(config));
        self.provider_instances_write(instance_id, patch, cx);
    }

    /// "+ Add custom model" — `{slug, name, capabilities: null}` per the
    /// web's `handleAdd` (dupes rejected silently like the web's error).
    fn provider_model_add_submit(&mut self, instance_id: &str, cx: &mut Context<Self>) {
        let key = format!("{instance_id}|model_add");
        let slug = self
            .misc_settings_state
            .extra
            .provider_field_drafts
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .trim()
            .to_string();
        if slug.is_empty() {
            return;
        }
        let mut models = self.provider_custom_models(instance_id);
        if models
            .iter()
            .any(|m| m["slug"].as_str() == Some(slug.as_str()))
        {
            return;
        }
        models.push(json!({ "slug": slug, "name": slug }));
        self.provider_custom_models_write(instance_id, models, cx);
        self.misc_settings_state.extra.provider_model_add = false;
        if let Some(field) = self
            .misc_settings_state
            .extra
            .provider_fields
            .get(&key)
            .cloned()
        {
            edit_field(&field, cx, |field, window, cx| field.reset(window, cx));
        }
        self.misc_settings_state
            .extra
            .provider_field_drafts
            .insert(key, String::new());
        cx.notify();
    }

    /// Trash a custom model — removes it from `config.customModels` and
    /// any favorite/hidden preference entry.
    fn provider_model_remove(&mut self, instance_id: &str, slug: String, cx: &mut Context<Self>) {
        let models: Vec<Value> = self
            .provider_custom_models(instance_id)
            .into_iter()
            .filter(|m| m["slug"].as_str() != Some(slug.as_str()))
            .collect();
        self.provider_custom_models_write(instance_id, models, cx);
        let mut prefs = self.provider_model_prefs(instance_id);
        prefs.model_order.retain(|s| s != &slug);
        self.provider_model_prefs_write(instance_id, prefs, cx);
    }

    // ─── Model list preferences (ClientConfig mirrors — device-local) ───

    fn provider_model_prefs(&self, instance_id: &str) -> crate::config::ProviderModelPreferences {
        self.client_config
            .provider_model_preferences
            .get(instance_id)
            .cloned()
            .unwrap_or_default()
    }

    fn provider_model_prefs_write(
        &mut self,
        instance_id: &str,
        prefs: crate::config::ProviderModelPreferences,
        cx: &mut Context<Self>,
    ) {
        self.client_config
            .provider_model_preferences
            .insert(instance_id.to_string(), prefs);
        self.client_config.save();
        cx.notify();
    }

    /// The visibility switch — flips `hiddenModels` membership.
    fn provider_model_set_hidden(
        &mut self,
        instance_id: &str,
        slug: String,
        hidden: bool,
        cx: &mut Context<Self>,
    ) {
        let mut prefs = self.provider_model_prefs(instance_id);
        if hidden && !prefs.hidden_models.contains(&slug) {
            prefs.hidden_models.push(slug);
        } else {
            prefs.hidden_models.retain(|s| s != &slug);
        }
        self.provider_model_prefs_write(instance_id, prefs, cx);
    }

    /// Star toggle — `ClientSettings.favorites` `{provider, model}` pair.
    fn provider_model_toggle_favorite(
        &mut self,
        instance_id: &str,
        slug: String,
        cx: &mut Context<Self>,
    ) {
        let favorite = crate::config::ModelFavorite {
            provider: instance_id.to_string(),
            model: slug.clone(),
        };
        if self.client_config.model_favorites.contains(&favorite) {
            self.client_config
                .model_favorites
                .retain(|f| *f != favorite);
        } else {
            self.client_config.model_favorites.push(favorite);
        }
        self.client_config.save();
        cx.notify();
    }

    /// ↑/↓ reorder — swaps `slug` with its neighbor inside `ordered` and
    /// stores the result as `modelOrder`.
    fn provider_model_move(
        &mut self,
        instance_id: &str,
        slug: &str,
        direction: i32,
        ordered: &[String],
        cx: &mut Context<Self>,
    ) {
        let mut ordered = ordered.to_vec();
        let Some(index) = ordered.iter().position(|s| s == slug) else {
            return;
        };
        let target = index as i32 + direction;
        if target < 0 || target >= ordered.len() as i32 {
            return;
        }
        ordered.swap(index, target as usize);
        let mut prefs = self.provider_model_prefs(instance_id);
        prefs.model_order = ordered;
        self.provider_model_prefs_write(instance_id, prefs, cx);
    }

    /// "Disable all" / "Enable all" — set hidden membership for every
    /// built-in slug at once (`nextHiddenModelsForBulkToggle`).
    fn provider_models_set_all_hidden(
        &mut self,
        instance_id: &str,
        built_in_slugs: &[String],
        hidden: bool,
        cx: &mut Context<Self>,
    ) {
        let mut prefs = self.provider_model_prefs(instance_id);
        if hidden {
            for slug in built_in_slugs {
                if !prefs.hidden_models.contains(slug) {
                    prefs.hidden_models.push(slug.clone());
                }
            }
        } else {
            let set: HashSet<&String> = built_in_slugs.iter().collect();
            prefs.hidden_models.retain(|s| !set.contains(s));
        }
        self.provider_model_prefs_write(instance_id, prefs, cx);
    }

    /// The composite provider list — misc mirror, falling back to the
    /// composer stream, plus instances that exist only as
    /// `providerInstances` config entries (created via "+ Add provider"
    /// before the server reports them).
    fn provider_list(&self) -> Vec<Value> {
        let mut providers = self.misc_settings_state.providers.clone();
        if providers.is_empty() {
            providers = self.composer_state.config_providers.clone();
        }
        let mut seen: HashSet<String> = providers
            .iter()
            .filter_map(|p| p["instanceId"].as_str().map(str::to_string))
            .collect();
        if let Some(configured) = self.misc_settings_state.settings["providerInstances"].as_object()
        {
            for (instance_id, entry) in configured {
                if seen.contains(instance_id) {
                    continue;
                }
                let driver = entry["driver"].as_str().unwrap_or(instance_id);
                providers.push(json!({
                    "instanceId": instance_id,
                    "driver": driver,
                    "displayName": entry["displayName"].as_str(),
                    "enabled": entry["enabled"].as_bool().unwrap_or(false),
                    "installed": false,
                    "status": "warning",
                    "auth": { "status": "unknown" },
                    "models": [],
                }));
                seen.insert(instance_id.clone());
            }
        }
        providers
    }

    /// The instance id shown in the detail pane — the stored selection
    /// when it still exists, else the first provider.
    fn selected_provider_id(&self, providers: &[Value]) -> Option<String> {
        let selected = self.misc_settings_state.extra.selected_provider.clone();
        if let Some(selected) = selected
            && providers
                .iter()
                .any(|p| p["instanceId"].as_str() == Some(selected.as_str()))
        {
            return Some(selected);
        }
        providers
            .iter()
            .find_map(|p| p["instanceId"].as_str().map(str::to_string))
    }

    // ─── Provider page: left list ───────────────────────────────────────

    /// One row in the provider list — icon + name/version + status +
    /// switch; selected rows highlight like the web `ProviderInstanceRow`.
    fn provider_list_row(
        &self,
        provider: &Value,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let instance_id = provider["instanceId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let name = provider["displayName"]
            .as_str()
            .or_else(|| provider["name"].as_str())
            .unwrap_or(&instance_id)
            .to_string();
        let driver = provider["driver"]
            .as_str()
            .unwrap_or(&instance_id)
            .to_string();
        let enabled = self.provider_instance_enabled(&instance_id, provider);
        let (headline, _, _) = Self::provider_status_summary(provider, enabled);
        let version = provider["version"]
            .as_str()
            .and_then(Self::provider_version_label);
        let mark = self.provider_mark(&driver);
        let select_id = instance_id.clone();
        let toggle_id = instance_id.clone();
        let connected = self.commands.is_some();

        let mut name_line = div().flex().items_center().gap_1p5().min_w_0().child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::fg())
                .truncate()
                .child(name.clone()),
        );
        if let Some(version) = version {
            name_line = name_line.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(version),
            );
        }

        let mut row = div()
            .id(format!("provider-row-{instance_id}"))
            .flex()
            .items_center()
            .gap_2p5()
            .px_3()
            .py_3()
            .cursor_pointer()
            .when(selected, |r| r.bg(theme::selected()))
            .when(!selected, |r| r.hover(|s| s.bg(theme::accent())))
            .child(
                div()
                    .w(px(24.))
                    .h(px(24.))
                    .flex_none()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if let Some(mark) = mark {
                        icon(mark, 15.).text_color(theme::fg()).into_any_element()
                    } else {
                        SharedString::from(
                            name.chars()
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
                    .child(name_line)
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .truncate()
                            .child(headline),
                    ),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _window, cx| {
                    this.select_provider_instance(select_id.clone(), cx);
                }),
            );
        if connected {
            row = row.child(
                div()
                    .id(format!("provider-toggle-{toggle_id}"))
                    .flex_none()
                    .cursor_pointer()
                    .child(switch_control(enabled))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.toggle_provider_instance(toggle_id.clone(), !enabled, cx);
                        }),
                    ),
            );
        }
        row
    }

    /// The "+ Add provider" row + driver popup at the bottom of the list.
    fn provider_add_row(&self, cx: &mut Context<Self>) -> Div {
        let open = self.misc_settings_state.extra.extra_open.as_deref() == Some("provider:add");
        let options: Vec<(String, String, Option<String>)> = PROVIDER_DRIVER_OPTIONS
            .iter()
            .map(|(value, label)| (value.to_string(), label.to_string(), None))
            .collect();
        div()
            .relative()
            .child(
                div()
                    .id("provider-add")
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .px_3()
                    .py_3()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::accent()).text_color(theme::fg()))
                    .child(icon(Icon::Plus, 14.))
                    .child("Add provider")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            this.extra_toggle("provider:add".to_string(), cx);
                        }),
                    ),
            )
            .when(open, |cell| {
                cell.child(
                    deferred(
                        div()
                            .absolute()
                            .bottom_full()
                            .left_0()
                            .pb_1()
                            .child(self.extra_options_popup("provider:add", "", &options, cx)),
                    )
                    .with_priority(2),
                )
            })
    }

    // ─── Provider page: detail pane ─────────────────────────────────────

    /// A runtime field row — title + description left, TextField right.
    fn provider_field_row(&self, instance_id: &str, def: &ProviderFieldDef) -> Div {
        let key = format!("{instance_id}|cfg:{}", def.key);
        let control: Div = match self.misc_settings_state.extra.provider_fields.get(&key) {
            Some(field) => self.boxed_field(field.clone(), 200.),
            None => div().w(px(200.)),
        };
        div()
            .flex()
            .items_center()
            .gap_6()
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
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::fg())
                            .child(def.title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(def.description),
                    ),
            )
            .child(control)
    }

    /// The right-hand detail pane — header, Status, Display name, Runtime,
    /// Environment, Models — the web `ProviderInstanceCard` editor mode.
    fn provider_detail_pane(&self, provider: &Value, cx: &mut Context<Self>) -> Div {
        let instance_id = provider["instanceId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let driver = provider["driver"]
            .as_str()
            .unwrap_or(&instance_id)
            .to_string();
        let name = provider["displayName"]
            .as_str()
            .or_else(|| provider["name"].as_str())
            .unwrap_or(&instance_id)
            .to_string();
        let enabled = self.provider_instance_enabled(&instance_id, provider);
        let (headline, status_detail, dot) = Self::provider_status_summary(provider, enabled);
        let mark = self.provider_mark(&driver);
        let configured = &self.misc_settings_state.settings["providerInstances"][&instance_id];
        let has_stored_config = configured.is_object();
        let iid_reset = instance_id.clone();

        let mut pane = div().flex().flex_col().min_w_0().flex_1();

        // ── Header: mark + name (+ reset when the instance has stored
        //    settings — the web shows the reset only while dirty). ──
        let mut header = div()
            .flex()
            .items_center()
            .gap_2p5()
            .px_4()
            .pt_4()
            .pb_3()
            .child(
                div()
                    .w(px(24.))
                    .h(px(24.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if let Some(mark) = mark {
                        icon(mark, 16.).text_color(theme::fg()).into_any_element()
                    } else {
                        SharedString::from(
                            name.chars()
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
                    .text_base()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::fg())
                    .truncate()
                    .child(name.clone()),
            );
        if has_stored_config {
            header = header.child(self.extra_icon_button(
                "provider-reset".to_string(),
                Icon::Undo2,
                cx,
                move |this, cx| {
                    this.reset_provider_instance(iid_reset.clone(), cx);
                },
            ));
        }
        pane = pane.child(header);

        // ── Status + Display name ──
        let mut status_block = div().flex().flex_col().gap_1().px_4();
        status_block = status_block
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::fg())
                    .child("Status"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(7.)).h(px(7.)).flex_none().rounded_full().bg(dot))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(headline),
                    ),
            );
        if let Some(detail) = status_detail
            && detail != "This provider is installed but disabled for new sessions in T3 Code."
        {
            status_block =
                status_block.child(div().text_xs().text_color(theme::fg_muted()).child(detail));
        }
        pane = pane.child(status_block);

        let name_key = format!("{instance_id}|display_name");
        let name_control: Div = match self
            .misc_settings_state
            .extra
            .provider_fields
            .get(&name_key)
        {
            Some(field) => self.boxed_field(field.clone(), 200.),
            None => div().w(px(200.)),
        };
        pane = pane.child(
            div()
                .flex()
                .items_center()
                .gap_6()
                .px_4()
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
                                .child("Display name"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child("The name shown for this provider across T3 Code."),
                        ),
                )
                .child(name_control),
        );

        // ── Runtime ──
        let runtime_fields = Self::provider_runtime_fields(&driver);
        let show_idle_select = driver == "claudeAgent";
        let show_auth_select = driver == "antigravity";
        if !runtime_fields.is_empty() || show_idle_select || show_auth_select {
            pane = pane.child(
                div()
                    .px_4()
                    .pt_2()
                    .pb_1()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::fg())
                    .child("Runtime"),
            );
            for def in runtime_fields {
                pane = pane.child(
                    div()
                        .px_4()
                        .child(self.provider_field_row(&instance_id, def)),
                );
            }
            if show_auth_select {
                let current = configured["config"]["authMethod"]
                    .as_str()
                    .unwrap_or("oauth-personal");
                let label = ANTIGRAVITY_AUTH_METHODS
                    .iter()
                    .find(|(value, _)| *value == current)
                    .map(|(_, label)| *label)
                    .unwrap_or("Google account");
                pane = pane.child(
                    div().px_4().child(self.extra_select_row(
                        "Sign-in method",
                        "Google accounts use your subscription; API keys and Agent Platform bill usage.",
                        &format!("provider:{instance_id}:authMethod"),
                        label.into(),
                        current,
                        ANTIGRAVITY_AUTH_METHODS
                            .iter()
                            .map(|(value, label)| {
                                (value.to_string(), label.to_string(), None)
                            })
                            .collect(),
                        cx,
                    )),
                );
            }
            if show_idle_select {
                let current = configured["config"]["idleCompaction"]
                    .as_str()
                    .unwrap_or("default");
                let label = PROVIDER_IDLE_COMPACTION_MODES
                    .iter()
                    .find(|(value, _)| *value == current)
                    .map(|(_, label)| *label)
                    .unwrap_or("Use default");
                pane = pane.child(
                    div().px_4().child(self.extra_select_row(
                        "Compact idle threads",
                        "When to compact an idle thread over 200,000 tokens: after you return, just before its prompt cache expires, or never. Overrides the default in Providers > Advanced.",
                        &format!("provider:{instance_id}:idleCompaction"),
                        label.into(),
                        current,
                        PROVIDER_IDLE_COMPACTION_MODES
                            .iter()
                            .map(|(value, label)| {
                                (value.to_string(), label.to_string(), None)
                            })
                            .collect(),
                        cx,
                    )),
                );
            }
        }

        // ── Environment ──
        let variables = self.provider_environment(&instance_id);
        let adding_env = self.misc_settings_state.extra.provider_env_add;
        let iid_env = instance_id.clone();
        let mut env_head = div()
            .flex()
            .items_center()
            .gap_4()
            .px_4()
            .pt_4()
            .pb_2()
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
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::fg())
                            .child("Environment"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("API keys, base URLs, and other per-instance CLI settings."),
                    ),
            );
        if !adding_env {
            env_head = env_head.child(
                div()
                    .id("provider-env-add-open")
                    .flex_none()
                    .px_3()
                    .py_1p5()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::input())
                    .text_xs()
                    .cursor_pointer()
                    .text_color(theme::fg())
                    .hover(|s| s.bg(theme::accent()))
                    .child("+ Add variable")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            this.misc_settings_state.extra.provider_env_add = true;
                            cx.notify();
                        }),
                    ),
            );
        }
        pane = pane.child(env_head);

        for (index, variable) in variables.iter().enumerate() {
            let var_name = variable["name"].as_str().unwrap_or_default().to_string();
            let sensitive = variable["sensitive"].as_bool() == Some(true)
                || variable["valueRedacted"].as_bool() == Some(true)
                || variable["secret"].is_object();
            let var_value = if sensitive {
                "••••••••".to_string()
            } else {
                variable["value"].as_str().unwrap_or_default().to_string()
            };
            let iid_remove = instance_id.clone();
            pane = pane.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_1p5()
                    .child(
                        div()
                            .w(px(180.))
                            .flex_none()
                            .text_sm()
                            .text_color(theme::fg())
                            .truncate()
                            .child(var_name),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .truncate()
                            .child(if var_value.is_empty() {
                                "(unset)".to_string()
                            } else {
                                var_value
                            }),
                    )
                    .child(self.extra_icon_button(
                        format!("provider-env-remove-{index}"),
                        Icon::X,
                        cx,
                        move |this, cx| {
                            let mut variables = this.provider_environment(&iid_remove);
                            if index < variables.len() {
                                variables.remove(index);
                            }
                            this.provider_environment_write(&iid_remove, variables, cx);
                        },
                    )),
            );
        }
        if variables.is_empty() && !adding_env {
            pane = pane.child(
                div()
                    .px_4()
                    .pb_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No per-instance variables configured."),
            );
        }
        if adding_env {
            let name_key = format!("{instance_id}|env_name");
            let value_key = format!("{instance_id}|env_value");
            let fields = &self.misc_settings_state.extra.provider_fields;
            let name_field = fields.get(&name_key).cloned();
            let value_field = fields.get(&value_key).cloned();
            let iid_submit = instance_id.clone();
            let mut add_row = div().flex().items_center().gap_2().px_4().pb_3();
            if let Some(field) = name_field {
                add_row = add_row.child(self.boxed_field(field, 160.));
            }
            if let Some(field) = value_field {
                add_row = add_row.child(self.boxed_field(field, 180.));
            }
            add_row = add_row
                .child(
                    div()
                        .id("provider-env-add-submit")
                        .px_3()
                        .py_1p5()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme::input())
                        .text_xs()
                        .cursor_pointer()
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::accent()))
                        .child("Add")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.provider_env_add_submit(&iid_submit, cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .id("provider-env-add-cancel")
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .text_color(theme::fg_muted())
                        .hover(|s| s.bg(theme::accent()))
                        .child("Cancel")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _window, cx| {
                                this.misc_settings_state.extra.provider_env_add = false;
                                cx.notify();
                            }),
                        ),
                );
            pane = pane.child(add_row);
        }
        let _ = iid_env;

        pane = pane.child(self.provider_models_section(&instance_id, provider, cx));

        pane
    }

    /// The Models block inside the detail pane — `ProviderModelsSection`:
    /// filter/counts/add toolbar, Favorites/All/Hidden groups, star +
    /// reorder + visibility controls per row.
    fn provider_models_section(
        &self,
        instance_id: &str,
        provider: &Value,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut block = div().flex().flex_col().px_4().pt_4().pb_4();

        // Build the display list: reported models + custom entries.
        let mut models: Vec<Value> = provider["models"].as_array().cloned().unwrap_or_default();
        let built_in_slugs: Vec<String> = models
            .iter()
            .filter_map(|m| m["slug"].as_str().map(str::to_string))
            .collect();
        for custom in self.provider_custom_models(instance_id) {
            let slug = custom["slug"].as_str().unwrap_or_default().to_string();
            if slug.is_empty()
                || models
                    .iter()
                    .any(|m| m["slug"].as_str() == Some(slug.as_str()))
            {
                continue;
            }
            models.push(json!({
                "slug": slug,
                "name": custom["name"].as_str().unwrap_or(&slug),
                "isCustom": true,
                "capabilities": null,
            }));
        }
        if models.is_empty() {
            return block;
        }

        let prefs = self.provider_model_prefs(instance_id);
        let hidden: HashSet<String> = prefs.hidden_models.iter().cloned().collect();
        let favorites: Vec<String> = self
            .client_config
            .model_favorites
            .iter()
            .filter(|f| f.provider == instance_id)
            .map(|f| f.model.clone())
            .collect();
        let favorite_set: HashSet<String> = favorites.iter().cloned().collect();

        // Order: modelOrder first, then server order (web `orderModels`).
        let order_index = |slug: &str| -> usize {
            prefs
                .model_order
                .iter()
                .position(|s| s == slug)
                .unwrap_or(usize::MAX)
        };
        let mut ordered_slugs: Vec<String> = models
            .iter()
            .filter_map(|m| m["slug"].as_str().map(str::to_string))
            .collect();
        let server_position: HashMap<String, usize> = ordered_slugs
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), i))
            .collect();
        ordered_slugs.sort_by_key(|slug| {
            (
                order_index(slug),
                server_position.get(slug).copied().unwrap_or(usize::MAX),
            )
        });
        let by_slug: HashMap<String, Value> = models
            .iter()
            .map(|m| {
                (
                    m["slug"].as_str().unwrap_or_default().to_string(),
                    m.clone(),
                )
            })
            .collect();

        // Groups: favorites first, then visible, then hidden (built-ins
        // only — custom models can't be hidden).
        let group_of = |model: &Value| -> &'static str {
            let slug = model["slug"].as_str().unwrap_or_default();
            let custom = model["isCustom"].as_bool() == Some(true);
            if favorite_set.contains(slug) {
                "favorite"
            } else if !custom && hidden.contains(slug) {
                "hidden"
            } else {
                "visible"
            }
        };
        let filter = self
            .misc_settings_state
            .extra
            .provider_field_drafts
            .get(&format!("{instance_id}|model_filter"))
            .cloned()
            .unwrap_or_default()
            .to_lowercase();
        let filtered: Vec<Value> = ordered_slugs
            .iter()
            .filter_map(|slug| by_slug.get(slug).cloned())
            .filter(|model| {
                if filter.is_empty() {
                    return true;
                }
                let slug = model["slug"].as_str().unwrap_or_default().to_lowercase();
                let name = model["name"].as_str().unwrap_or_default().to_lowercase();
                slug.contains(&filter) || name.contains(&filter)
            })
            .collect();
        let favorite_models: Vec<&Value> = filtered
            .iter()
            .filter(|m| group_of(m) == "favorite")
            .collect();
        let visible_models: Vec<&Value> = filtered
            .iter()
            .filter(|m| group_of(m) == "visible")
            .collect();
        let hidden_models: Vec<&Value> = filtered
            .iter()
            .filter(|m| group_of(m) == "hidden")
            .collect();
        let favorite_count = favorites.len();
        let hidden_count = models
            .iter()
            .filter(|m| m["isCustom"].as_bool() != Some(true))
            .filter(|m| hidden.contains(m["slug"].as_str().unwrap_or_default()))
            .count();
        let all_built_in_hidden =
            !built_in_slugs.is_empty() && built_in_slugs.iter().all(|slug| hidden.contains(slug));

        // ── Header copy + toolbar ──
        block = block.child(
            div()
                .pb_2()
                .text_xs()
                .text_color(theme::fg_muted())
                .child("Model preferences and custom models are saved on the selected environment and shared across connected devices."),
        );
        let filter_key = format!("{instance_id}|model_filter");
        let filter_field = self
            .misc_settings_state
            .extra
            .provider_fields
            .get(&filter_key)
            .cloned();
        let iid_bulk = instance_id.to_string();
        let bulk_hidden = !all_built_in_hidden;
        let bulk_slugs = built_in_slugs.clone();
        let mut toolbar = div().flex().items_center().gap_2().pb_2();
        if let Some(field) = filter_field {
            toolbar = toolbar.child(self.boxed_field(field, 176.));
        }
        toolbar = toolbar
            .child(div().flex_1().min_w_0())
            .child(
                div()
                    .id("provider-models-bulk")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::accent()))
                    .child(if all_built_in_hidden {
                        "Enable all"
                    } else {
                        "Disable all"
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.provider_models_set_all_hidden(
                                &iid_bulk,
                                &bulk_slugs,
                                bulk_hidden,
                                cx,
                            );
                        }),
                    ),
            )
            .child(div().text_xs().text_color(theme::fg_muted()).child(format!(
                "{} models{}{}",
                models.len(),
                if favorite_count > 0 {
                    format!(" · {favorite_count} favorites")
                } else {
                    String::new()
                },
                if hidden_count > 0 {
                    format!(" · {hidden_count} hidden")
                } else {
                    String::new()
                },
            )));
        let adding_model = self.misc_settings_state.extra.provider_model_add;
        if !adding_model && driver_allows_custom(&provider["driver"].as_str().unwrap_or_default()) {
            toolbar = toolbar.child(
                div()
                    .id("provider-model-add-open")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::accent()))
                    .child("+ Add custom model")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            this.misc_settings_state.extra.provider_model_add = true;
                            cx.notify();
                        }),
                    ),
            );
        }
        block = block.child(toolbar);

        // ── Model rows ──
        let full_order: Vec<String> = ordered_slugs.clone();
        let mut render_rows = |items: &[&Value], first_index: usize, block: Div| -> Div {
            let mut block = block;
            for (offset, model) in items.iter().enumerate() {
                let index = first_index + offset;
                block = block.child(self.provider_model_row(
                    instance_id,
                    model,
                    index,
                    filtered.len(),
                    &full_order,
                    &favorite_set,
                    &hidden,
                    cx,
                ));
            }
            block
        };
        if !favorite_models.is_empty() {
            block = block.child(
                div()
                    .pt_1()
                    .pb_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Favorites"),
            );
        }
        block = render_rows(&favorite_models, 0, block);
        if !favorite_models.is_empty() {
            block = block.child(
                div()
                    .pt_4()
                    .pb_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("All"),
            );
        }
        block = render_rows(&visible_models, favorite_models.len(), block);
        if !hidden_models.is_empty() {
            block = block.child(
                div()
                    .pt_4()
                    .pb_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Hidden from picker"),
            );
        }
        block = render_rows(
            &hidden_models,
            favorite_models.len() + visible_models.len(),
            block,
        );
        if filtered.is_empty() {
            block = block.child(div().py_2().text_xs().text_color(theme::fg_muted()).child(
                if filter.is_empty() {
                    "No models reported for this provider yet."
                } else {
                    "No models match."
                },
            ));
        }

        // ── Add-model fold ──
        if adding_model {
            let add_key = format!("{instance_id}|model_add");
            let field = self
                .misc_settings_state
                .extra
                .provider_fields
                .get(&add_key)
                .cloned();
            let iid_submit = instance_id.to_string();
            let mut add_row = div().flex().items_center().gap_2().pt_2();
            if let Some(field) = field {
                add_row = add_row.child(self.boxed_field(field, 200.));
            }
            add_row = add_row
                .child(
                    div()
                        .id("provider-model-add-submit")
                        .px_3()
                        .py_1p5()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme::input())
                        .text_xs()
                        .cursor_pointer()
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::accent()))
                        .child("Add")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.provider_model_add_submit(&iid_submit, cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .id("provider-model-add-cancel")
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .text_color(theme::fg_muted())
                        .hover(|s| s.bg(theme::accent()))
                        .child("Cancel")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _window, cx| {
                                this.misc_settings_state.extra.provider_model_add = false;
                                cx.notify();
                            }),
                        ),
                );
            block = block.child(add_row);
        }
        block
    }

    /// One model row — star, name + slug, capability hint, ↑/↓ reorder,
    /// visibility switch; custom models get a remove (🗑) instead of the
    /// hidden switch (the web only hides built-ins).
    fn provider_model_row(
        &self,
        instance_id: &str,
        model: &Value,
        index: usize,
        _visible_total: usize,
        full_order: &[String],
        favorite_set: &HashSet<String>,
        hidden: &HashSet<String>,
        cx: &mut Context<Self>,
    ) -> Div {
        let slug = model["slug"].as_str().unwrap_or_default().to_string();
        let name = model["name"].as_str().unwrap_or(&slug).to_string();
        let is_custom = model["isCustom"].as_bool() == Some(true);
        let is_favorite = favorite_set.contains(&slug);
        let is_hidden = !is_custom && hidden.contains(&slug);
        let iid = instance_id.to_string();

        let mut row = div()
            .flex()
            .items_center()
            .gap_2()
            .py_1()
            .min_h(px(28.))
            .child(
                div()
                    .id(format!("model-fav-{iid}-{index}"))
                    .w(px(20.))
                    .h(px(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_color(if is_favorite {
                        theme::warning()
                    } else {
                        theme::fg_muted()
                    })
                    .hover(|s| s.bg(theme::accent()))
                    .child(icon(
                        if is_favorite {
                            Icon::StarFilled
                        } else {
                            Icon::Star
                        },
                        13.,
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener({
                            let iid = iid.clone();
                            let slug = slug.clone();
                            move |this, _ev, _window, cx| {
                                this.provider_model_toggle_favorite(&iid, slug.clone(), cx);
                            }
                        }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(if is_hidden {
                                theme::fg_muted()
                            } else {
                                theme::fg()
                            })
                            .truncate()
                            .child(name.clone()),
                    )
                    .when(name != slug, |s| {
                        s.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .truncate()
                                .child(slug.clone()),
                        )
                    }),
            );
        if model["capabilities"].is_null() && !is_custom {
            row = row.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No reasoning"),
            );
        }
        // ↑/↓ reorder — writes the whole ordered list.
        for (direction, mark) in [(-1, Icon::ArrowUp), (1, Icon::ArrowDown)] {
            let iid = iid.clone();
            let slug = slug.clone();
            let order: Vec<String> = full_order.to_vec();
            row = row.child(
                div()
                    .id(format!("model-move-{iid}-{index}-{direction}"))
                    .w(px(18.))
                    .h(px(18.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::accent()))
                    .child(icon(mark, 11.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.provider_model_move(&iid, &slug, direction, &order, cx);
                        }),
                    ),
            );
        }
        if is_custom {
            let iid = iid.clone();
            let slug = slug.clone();
            row = row.child(
                div()
                    .id(format!("model-remove-{iid}-{index}"))
                    .w(px(20.))
                    .h(px(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::accent()).text_color(danger()))
                    .child(icon(Icon::Trash2, 12.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.provider_model_remove(&iid, slug.clone(), cx);
                        }),
                    ),
            );
        } else {
            let iid = iid.clone();
            let slug = slug.clone();
            row = row.child(
                div()
                    .id(format!("model-visible-{iid}-{index}"))
                    .flex_none()
                    .cursor_pointer()
                    .child(switch_control(!is_hidden))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.provider_model_set_hidden(&iid, slug.clone(), is_hidden, cx);
                        }),
                    ),
            );
        }
        row
    }
    fn provider_instance_enabled(&self, instance_id: &str, provider: &Value) -> bool {
        let configured = &self.misc_settings_state.settings["providerInstances"][instance_id];
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
        let mut patch = serde_json::Map::new();
        patch.insert("enabled".to_string(), json!(on));
        self.provider_instances_write(&instance_id, patch, cx);
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

    /// The provider brand glyph — the driver→icon map used by
    /// `providerClients` in the web (list rows, detail header, composer).
    fn provider_mark(&self, driver: &str) -> Option<Icon> {
        match driver {
            "codex" | "openai" | "openaiCompatible" => Some(Icon::ProviderOpenAi),
            "claudeAgent" | "anthropic" => Some(Icon::ProviderClaude),
            "devin" => Some(Icon::ProviderDevin),
            "grok" => Some(Icon::ProviderGrok),
            "opencode" => Some(Icon::ProviderOpencode),
            "cursor" => Some(Icon::ProviderCursor),
            "kiro" => Some(Icon::ProviderKiro),
            "pi" => Some(Icon::ProviderPi),
            "fx" => Some(Icon::ProviderFx),
            "muse" => Some(Icon::ProviderMuse),
            "kilo" => Some(Icon::ProviderKilo),
            "devReplay" => Some(Icon::Play),
            "antigravity" | "acpRegistry" => Some(Icon::Bot),
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

        // Scope sentence — the providers surface writes to the connected
        // environment ("… on <env>").
        page = page.child(self.scope_sentence(true, cx));

        // ── Providers — `server.getConfig → providers` list + the
        //    selected instance's detail pane (web ProviderSettingsPanel
        //    two-pane editor). ──
        let providers = self.provider_list();
        let selected_id = self.selected_provider_id(&providers);
        let selected_provider = providers
            .iter()
            .find(|p| p["instanceId"].as_str() == selected_id.as_deref())
            .cloned();

        // Auto-selection (no click yet) never ran `select_provider_instance`,
        // so its TextFields don't exist and the detail inputs render empty —
        // spawn the deferred creation; `&self` render can't build entities.
        if let Some(instance_id) = &selected_id
            && !self
                .misc_settings_state
                .extra
                .provider_fields
                .contains_key(&format!("{instance_id}|display_name"))
        {
            let instance_id = instance_id.clone();
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.ensure_provider_fields(&instance_id, cx);
                    cx.notify();
                });
            })
            .detach();
        }
        // Fields created inside listeners can't reach a window for seeding —
        // flush the queued seeds once the spawn runs outside dispatch.
        if !self
            .misc_settings_state
            .extra
            .provider_field_pending_seeds
            .is_empty()
        {
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| {
                    let pending = std::mem::take(
                        &mut this.misc_settings_state.extra.provider_field_pending_seeds,
                    );
                    for (key, seed) in pending {
                        let Some(field) = this
                            .misc_settings_state
                            .extra
                            .provider_fields
                            .get(&key)
                            .cloned()
                        else {
                            continue;
                        };
                        edit_field(&field, cx, |field, window, cx| {
                            field.reset(window, cx);
                            field.replace_text_in_range(Some(0..0), &seed, window, cx);
                        });
                    }
                    cx.notify();
                });
            })
            .detach();
        }

        // "↻ Checked just now" — the latest `checkedAt` across instances.
        let checked_at = providers
            .iter()
            .filter_map(|p| p["checkedAt"].as_str())
            .next()
            .is_some();

        let mut list_pane = div()
            .w(px(220.))
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme::card_divider());
        let mut row_index = 0usize;
        for provider in &providers {
            let instance_id = provider["instanceId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if instance_id.is_empty() {
                continue;
            }
            let selected = selected_id.as_deref() == Some(instance_id.as_str());
            let row = self.provider_list_row(provider, selected, cx);
            list_pane = list_pane.child(row).when(row_index > 0, |pane| pane);
            row_index += 1;
        }
        if row_index == 0 {
            list_pane = list_pane.child(
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
        if connected {
            list_pane = list_pane.child(
                div()
                    .border_t_1()
                    .border_color(theme::card_divider())
                    .child(self.provider_add_row(cx)),
            );
        }

        let detail_pane: Div = match &selected_provider {
            Some(provider) => self.provider_detail_pane(provider, cx),
            None => div()
                .flex_1()
                .px_4()
                .py_8()
                .text_xs()
                .text_color(theme::fg_muted())
                .child("Select a provider to configure it."),
        };

        let providers_card = div()
            .flex()
            .items_stretch()
            .rounded_xl()
            .border_1()
            .border_color(theme::card_border())
            .bg(theme::card_surface_40())
            .overflow_hidden()
            .child(list_pane)
            .child(detail_pane);

        // "Providers +" header with the checked stamp right-aligned — the
        // web `SettingsSection` title + refresh affordance.
        page = page.child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .min_h(px(28.))
                        .pb_2p5()
                        .px_4()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_base()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::fg())
                                .child("Providers")
                                .child(
                                    div()
                                        .id("providers-add-title")
                                        .flex()
                                        .items_center()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .text_color(theme::fg_muted())
                                        .hover(|s| s.text_color(theme::fg()))
                                        .child(icon(Icon::Plus, 14.))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _ev, _window, cx| {
                                                this.extra_toggle("provider:add".to_string(), cx);
                                            }),
                                        ),
                                ),
                        )
                        .when(checked_at, |s| {
                            s.child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_xs()
                                    .text_color(theme::fg_muted())
                                    .child(icon(Icon::RefreshCw, 11.))
                                    .child("Checked just now"),
                            )
                        }),
                )
                .child(providers_card),
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
                                .child(div().text_sm().text_color(theme::fg()).child(label))
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
                let kind = generator["kind"].as_str().unwrap_or("openaiCompatible");
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
                                .child(div().text_sm().text_color(theme::fg()).child(label))
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
