//! Persisted client-side configuration: theme mode and saved environments
//! (server endpoints). Lives at `$C67_CONFIG_HOME/config.json`, defaulting to
//! `~/.config/c67/config.json`. Written through `ClientConfig::save()` after
//! any mutation so a restart restores the same environment + theme.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A server the user can connect to (label + endpoint + bearer token).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedEnvironment {
    pub label: String,
    pub http_url: String,
    pub token: String,
}

/// Submitted prompts kept per project for up-arrow recall.
pub const MAX_PROMPT_HISTORY: usize = 50;
/// Stashed drafts kept per project.
pub const MAX_STASHED_DRAFTS: usize = 20;
/// Sidebar draft threads kept per client.
pub const MAX_DRAFT_THREADS: usize = 50;

/// A file attachment parked with a stashed draft (re-staged on restore;
/// image/text attachments are dropped since they don't serialize cheaply).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StashedFile {
    pub name: String,
    pub path: String,
}

/// A composer draft parked for later — the web client's prompt stash.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StashedDraft {
    pub prompt: String,
    #[serde(default)]
    pub files: Vec<StashedFile>,
}

/// A client-side draft thread — the web client's "Draft threads" sidebar
/// block. Keyed by `thread_id` (a `draft:<uuid>` id); promoted to a real
/// thread on first dispatch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DraftThread {
    pub thread_id: String,
    pub project_id: String,
    pub prompt: String,
    #[serde(default)]
    pub files: Vec<StashedFile>,
    #[serde(default)]
    pub created_at: String,
}

/// Appearance mode: `"dark"`, `"light"`, or `"system"`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
    System,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientConfig {
    #[serde(default)]
    pub theme_mode: ThemeMode,
    /// Accent hue override reserved for future use; None = default accent.
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub environments: Vec<SavedEnvironment>,
    /// Index into `environments` of the last-connected endpoint.
    #[serde(default)]
    pub active_environment: Option<usize>,
    /// Submitted prompts per project key (oldest first) — composer up-arrow
    /// recall. Keyed by project id; see `AppView::composer_project_key`.
    #[serde(default)]
    pub prompt_history: HashMap<String, Vec<String>>,
    /// Parked composer drafts per project key (newest first).
    #[serde(default)]
    pub stashed_drafts: HashMap<String, Vec<StashedDraft>>,
    /// System-notification mode for thread events ("off" default) — mirrors
    /// the web's device-local ClientSettings.notificationMode.
    #[serde(default)]
    pub notification_mode: String,
    /// Show in-app toasts when a background thread settles (default true).
    #[serde(default = "default_in_app_notifications")]
    pub in_app_notifications: bool,
    /// Client-side draft threads (sidebar block) — persisted so drafts
    /// survive restart, same as the web's localStorage drafts.
    #[serde(default)]
    pub draft_threads: Vec<DraftThread>,
    /// Diff layout for diff/PR views ("stacked" | "split") — mirrors the
    /// web's device-local `ClientSettings.diffLayout` (default "stacked").
    #[serde(default = "default_diff_layout")]
    pub diff_layout: String,
    /// Ignore whitespace-only changes in diffs by default — mirrors the
    /// web's `ClientSettings.diffIgnoreWhitespace` (default true).
    #[serde(default = "default_true")]
    pub diff_ignore_whitespace: bool,
    /// Start diff files collapsed — mirrors the web's
    /// `ClientSettings.diffFilesCollapsed` (default true).
    #[serde(default = "default_true")]
    pub diff_files_collapsed: bool,
    /// Open linked pull requests / changes panels proactively — mirrors the
    /// web's `ClientSettings.proactivePanelsEnabled` (default false).
    #[serde(default)]
    pub proactive_panels: bool,
    /// Settings-scope selection: `Some(project_id)` shows the project-override
    /// axis in the scoped settings sections; `None` edits global server
    /// settings. Validated against the live project list on use — a stale id
    /// falls back to Global.
    #[serde(default)]
    pub settings_scope_project: Option<String>,
    /// Sidebar "Project grouping" — the web's
    /// `ClientSettings.sidebarProjectGroupingMode` reduced to its toggle
    /// (`separate` ⇄ enabled mode; default enabled like `repository`).
    #[serde(default = "default_true")]
    pub sidebar_project_grouping: bool,
    /// Sidebar "Project order" — the web's
    /// `ClientSettings.sidebarProjectSortOrder` ("updated_at" default).
    #[serde(default = "default_project_sort_order")]
    pub sidebar_project_sort_order: String,
    /// Sidebar "Working section" — the web's
    /// `ClientSettings.sidebarWorkingShelfEnabled` (default true).
    #[serde(default = "default_true")]
    pub sidebar_working_shelf: bool,
    /// Confirmations — the web's `confirmThreadUnpin` (default false).
    #[serde(default)]
    pub confirm_thread_unpin: bool,
    /// The web's `confirmThreadArchive` (default false).
    #[serde(default)]
    pub confirm_thread_archive: bool,
    /// The web's `confirmThreadDelete` (default true).
    #[serde(default = "default_true")]
    pub confirm_thread_delete: bool,
    /// Display "Time format" — the web's `ClientSettings.timestampFormat`
    /// ("locale" | "12-hour" | "24-hour"; empty = locale default).
    #[serde(default)]
    pub timestamp_format: String,
    /// Chat "Send shortcut" — the web's `ClientSettings.sendShortcut`
    /// ("enter" | "mod-enter-multiline" | "mod-enter"; default "enter").
    #[serde(default = "send_shortcut")]
    pub send_shortcut: String,
    /// Chat "Rich text composer" — `ClientSettings.composerRichTextEnabled`
    /// (default true).
    #[serde(default = "default_true")]
    pub composer_rich_text: bool,
    /// Chat "Skills in slash menu" — `ClientSettings.showSkillsInSlashMenu`
    /// (default true).
    #[serde(default = "default_true")]
    pub show_skills_in_slash_menu: bool,
    /// Chat "Follow-up behavior" — `ClientSettings.followUpBehavior`
    /// ("queue" | "steer"; default "queue").
    #[serde(default = "follow_up_behavior")]
    pub follow_up_behavior: String,
    /// Activity "Tool calls in chat" — `ClientSettings.toolCallVisibility`
    /// ("showAll" | "hideSuccessful" | "hideAll" | "zen"; default "showAll").
    #[serde(default = "tool_call_visibility")]
    pub tool_call_visibility: String,
    /// Activity "Screen reader announcements" —
    /// `ClientSettings.screenReaderAnnouncements`
    /// ("status" | "messages" | "everything"; default "status").
    #[serde(default = "screen_reader_announcements")]
    pub screen_reader_announcements: String,
    /// Diffs "Diff colors" — `ClientSettings.diffColorScheme`
    /// ("red-green" | "blue-orange"; default "red-green").
    #[serde(default = "diff_color_scheme")]
    pub diff_color_scheme: String,
    /// Diffs "Open pull request links in T3 Code" —
    /// `ClientSettings.pullRequestLinksOpenInApp` (default true).
    #[serde(default = "default_true")]
    pub pull_request_links_open_in_app: bool,
    /// Voice "Dictation shortcut" — `ClientSettings.voiceShortcutMode`
    /// ("auto" | "hold" | "toggle"; default "auto").
    #[serde(default = "voice_shortcut_mode")]
    pub voice_shortcut_mode: String,
    /// Voice "Language" — `ClientSettings.speechLanguage` ("" / "auto" =
    /// auto-detect; BCP-47 code otherwise).
    #[serde(default = "speech_language")]
    pub speech_language: String,
    /// Voice "Translate to English" — `ClientSettings.speechTranslateToEnglish`.
    #[serde(default)]
    pub speech_translate_to_english: bool,
    /// Voice "Silence filtering" — `ClientSettings.speechSilenceFiltering`
    /// (default true).
    #[serde(default = "default_true")]
    pub speech_silence_filtering: bool,
    /// Voice "Remove filler words" — `ClientSettings.speechRemoveFillerWords`
    /// (default true).
    #[serde(default = "default_true")]
    pub speech_remove_filler_words: bool,
    /// Voice "Dictionary" — `ClientSettings.speechCustomWords`, comma-separated.
    #[serde(default)]
    pub speech_custom_words: String,
    /// Voice "Correction cue" — `ClientSettings.speechCorrectionWord`.
    #[serde(default)]
    pub speech_correction_word: String,
    /// SnapShots "SnapShots" — `ClientSettings.snapShotEnabled`.
    #[serde(default)]
    pub snap_shot_enabled: bool,
    /// SnapShots "Include accessibility text" —
    /// `ClientSettings.snapShotIncludeAccessibility` (default true).
    #[serde(default = "default_true")]
    pub snap_shot_include_accessibility: bool,
    /// SnapShots "Capture shortcut" — `ClientSettings.snapShotShortcut`
    /// reduced to a label string ("" = platform default).
    #[serde(default = "snap_shot_shortcut")]
    pub snap_shot_shortcut: String,
    /// SnapShots "Play sound" — `ClientSettings.snapShotPlaySound`
    /// (default true).
    #[serde(default = "default_true")]
    pub snap_shot_play_sound: bool,
    /// SnapShots "Capture sound" — `ClientSettings.snapShotSound`
    /// ("soft-pop" | "camera-shutter"; default "soft-pop").
    #[serde(default = "snap_shot_sound")]
    pub snap_shot_sound: String,
    /// SnapShots "Flash" — `ClientSettings.snapShotFlash` (default true).
    #[serde(default = "default_true")]
    pub snap_shot_flash: bool,
    /// SnapShots "Animations" — `ClientSettings.snapShotAnimations`
    /// (default true).
    #[serde(default = "default_true")]
    pub snap_shot_animations: bool,
    /// Browser "Open links" — `ClientSettings.browserLinkTarget`
    /// ("system" | "app"; default "system" — the native client has no embedded
    /// browser, so links always open in the system browser either way).
    #[serde(default = "browser_link_target")]
    pub browser_link_target: String,
    /// Browser "Search engine" — `ClientSettings.browserSearchEngine`
    /// (default "duckduckgo" upstream; "" = default).
    #[serde(default = "browser_search_engine")]
    pub browser_search_engine: String,
    /// Browser "Show floating preview" —
    /// `ClientSettings.browserAutoShowFloatingPreview` (default true).
    #[serde(default = "default_true")]
    pub browser_auto_show_floating_preview: bool,
    /// Labs "Boring sidebar" — `ClientSettings.boringSidebarEnabled`.
    #[serde(default)]
    pub boring_sidebar: bool,
    /// Labs "Two-line thread rows" —
    /// `ClientSettings.boringSidebarMultilineRows`.
    #[serde(default)]
    pub boring_sidebar_multiline: bool,
}

fn default_in_app_notifications() -> bool {
    true
}

fn default_project_sort_order() -> String {
    "updated_at".to_string()
}

fn default_diff_layout() -> String {
    "stacked".to_string()
}

fn send_shortcut() -> String {
    "enter".to_string()
}
fn follow_up_behavior() -> String {
    "queue".to_string()
}
fn tool_call_visibility() -> String {
    "showAll".to_string()
}
fn screen_reader_announcements() -> String {
    "status".to_string()
}
fn diff_color_scheme() -> String {
    "red-green".to_string()
}
fn voice_shortcut_mode() -> String {
    "auto".to_string()
}
fn speech_language() -> String {
    "auto".to_string()
}
fn snap_shot_shortcut() -> String {
    "both-shift-keys".to_string()
}
fn snap_shot_sound() -> String {
    "soft-pop".to_string()
}
fn browser_link_target() -> String {
    "system".to_string()
}
fn browser_search_engine() -> String {
    "duckduckgo".to_string()
}

fn default_true() -> bool {
    true
}

fn config_path() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("C67_CONFIG_HOME") {
        return Some(std::path::PathBuf::from(dir).join("config.json"));
    }
    let home = std::env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config")
            .join("c67")
            .join("config.json"),
    )
}

impl ClientConfig {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = config_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, text);
        }
    }

    /// Record a submitted prompt under `project`, deduping consecutive
    /// repeats and capping at `MAX_PROMPT_HISTORY` (oldest first).
    pub fn record_prompt(&mut self, project: &str, prompt: &str) {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return;
        }
        let entries = self.prompt_history.entry(project.to_string()).or_default();
        if entries.last().is_some_and(|last| last == prompt) {
            return;
        }
        entries.push(prompt.to_string());
        while entries.len() > MAX_PROMPT_HISTORY {
            entries.remove(0);
        }
        self.save();
    }

    /// A project's submitted prompts, oldest first.
    pub fn history_for(&self, project: &str) -> &[String] {
        self.prompt_history
            .get(project)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Park a draft under `project` (newest first), capped at
    /// `MAX_STASHED_DRAFTS`.
    pub fn push_stash(&mut self, project: &str, draft: StashedDraft) {
        let entries = self.stashed_drafts.entry(project.to_string()).or_default();
        entries.insert(0, draft);
        entries.truncate(MAX_STASHED_DRAFTS);
        self.save();
    }

    /// Take the newest stashed draft for `project`, if any.
    pub fn pop_stash(&mut self, project: &str) -> Option<StashedDraft> {
        let entries = self.stashed_drafts.get_mut(project)?;
        if entries.is_empty() {
            return None;
        }
        let draft = entries.remove(0);
        self.save();
        Some(draft)
    }

    /// How many drafts are stashed under `project`.
    pub fn stash_count(&self, project: &str) -> usize {
        self.stashed_drafts.get(project).map_or(0, Vec::len)
    }

    /// Park a sidebar draft thread (appended in creation order), capped at
    /// `MAX_DRAFT_THREADS` (oldest dropped).
    pub fn push_draft_thread(&mut self, draft: DraftThread) {
        self.draft_threads.push(draft);
        while self.draft_threads.len() > MAX_DRAFT_THREADS {
            self.draft_threads.remove(0);
        }
        self.save();
    }

    /// Remove and return the draft thread with `thread_id` — opening a draft
    /// consumes it (the live composer takes over as the editable copy).
    pub fn take_draft_thread(&mut self, thread_id: &str) -> Option<DraftThread> {
        let index = self
            .draft_threads
            .iter()
            .position(|d| d.thread_id == thread_id)?;
        let draft = self.draft_threads.remove(index);
        self.save();
        Some(draft)
    }

    /// Delete a draft thread without consuming it (the row's ✕). Returns
    /// whether one was removed.
    pub fn remove_draft_thread(&mut self, thread_id: &str) -> bool {
        let before = self.draft_threads.len();
        self.draft_threads.retain(|d| d.thread_id != thread_id);
        let removed = self.draft_threads.len() != before;
        if removed {
            self.save();
        }
        removed
    }
}
