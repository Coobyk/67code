//! Composer: input field, attachments, provider/model pickers, mode
//! selectors, slash commands. Owns `impl AppView` composer render methods and
//! `composer::State`.
//!
//! `render_composer` is called from `Render for AppView` as `&self`, so every
//! interactive element uses window-level callbacks (`&mut Window` + `&mut App`)
//! and reaches the view through `window.root::<AppView>()` instead of
//! `cx.listener`. Keyboard handling lives in an `App::intercept_keystrokes`
//! hook installed lazily on the first composer interaction — it runs before
//! keymap/action dispatch, which is what lets Enter apply a slash command
//! instead of submitting the raw text.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    ClipboardEntry, Context, Entity, EntityInputHandler, ExternalPaths, Focusable, KeystrokeEvent,
    MouseButton, ObjectFit, RenderImage, SharedString, Subscription, Window, div, img, prelude::*,
    px, rgb, rgba,
};
use serde_json::json;

use crate::AppView;
use crate::config;
use crate::model::{ConnectionStatus, Message, Role};
use crate::notifications::ToastKind;
use crate::text_field::{PromptSubmitted, Submit, TextField};
use crate::theme;

/// Composer-local UI state. Expand freely — this field lives on `AppView`.
pub struct State {
    /// Open picker, if any (model / interaction mode / runtime mode / attach menu).
    pub picker: Option<PickerKind>,
    /// Staged attachments to send with the next message.
    pub attachments: Vec<Attachment>,
    /// Mirror of the composer `TextField`'s content, synced after each keystroke
    /// (drives the slash-command menu, which `&self` rendering cannot read).
    pub draft: String,
    /// Slash-command menu visible (dismissed by Esc until the draft changes).
    pub slash_open: bool,
    /// Highlighted row in the slash menu (arrow keys cycle, Enter applies).
    pub slash_selection: usize,
    /// `default` | `plan` — read by the orchestrator for the `interactionMode`
    /// field of `thread.create` payloads.
    pub interaction_mode: String,
    /// Runtime mode for the `runtimeMode` field of `thread.create` payloads.
    /// One of `full-access`, `approval-required`, `auto`, `auto-accept-edits`.
    pub runtime_mode: String,
    /// Reasoning effort for `modelSelection.options` (`""` = provider
    /// default, emits no option). One of `""`, `low`, `medium`, `high`.
    pub effort: String,
    /// User explicitly picked a model; blocks `defaultModelSelection`
    /// (server settings) from reseeding `provider`/`model`.
    pub model_picked: bool,
    /// User explicitly picked a permission mode; blocks
    /// `defaultRuntimeMode` from reseeding `runtime_mode`.
    pub runtime_mode_picked: bool,
    /// Run-options pick for the next dispatch on a busy thread —
    /// `steer_active`/`restart_active`/`queue_after_active`. Consumed by
    /// `on_prompt_submitted` in lib.rs; None = default queue behavior.
    pub dispatch_override: Option<&'static str>,
    /// Index into the project's persisted prompt history while browsing with
    /// up/down; `None` = live draft, not browsing.
    pub history_index: Option<usize>,
    /// Draft parked when history browsing began, restored when the user
    /// browses back down past the newest entry.
    pub history_draft: String,
    /// The history entry currently shown in the composer — lets
    /// `sync_composer_draft` notice manual edits and exit history mode.
    pub history_recalled: String,
    /// Inline path input staged by "Attach file…" (no native file dialog).
    pub attach_field: Option<Entity<TextField>>,

    /// @mention popup: a trailing `@token` in `draft` (see `mention_token`).
    pub mention_open: bool,
    /// Highlighted row in the mention popup.
    pub mention_selection: usize,
    /// Byte offset of the active token's `@` inside `draft`.
    pub mention_token_start: usize,
    /// Text after the active token's `@` — what candidates filter on.
    pub mention_query: String,
    /// File-path suggestions from the last `projects.searchEntries` reply.
    pub mention_files: Vec<String>,
    /// The query `mention_files` was fetched with (dedupes searches).
    pub mention_searched: String,
    /// Debounce/result generation — stale replies drop.
    mention_generation: u64,

    /// Today's `server.getUsageSummary` value, for the composer banner.
    pub usage_summary: Option<serde_json::Value>,
    usage_loading: bool,
    usage_fetched_at: Option<Instant>,
    /// The UTC day the banner was dismissed for; a new day re-shows it.
    pub usage_dismissed_day: Option<String>,

    /// Selected-thread project's current branch (`vcs.refreshStatus`).
    pub branch: Option<String>,
    /// Whole merged status value (ahead/behind, working-tree counts).
    pub branch_status: Option<serde_json::Value>,
    /// Project id `branch`/`branch_status` were fetched for.
    pub branch_scope: Option<String>,
    branch_loading: bool,
    branch_fetched_at: Option<Instant>,
    /// `vcs.listRefs` rows for the branch picker.
    pub branch_refs: Vec<serde_json::Value>,
    pub branch_refs_loading: bool,
    pub branch_refs_loaded: bool,
    pub branch_refs_error: Option<String>,
    /// Ref a `vcs.switchRef`/`vcs.createRef` call is in flight for.
    pub branch_pending: Option<String>,
    /// Inline input staged by "New branch…" in the branch picker.
    pub branch_field: Option<Entity<TextField>>,

    /// `subscribeTerminalMetadata` stream id while the ctx picker is open.
    term_meta_stream: Option<String>,
    /// Latest terminal metadata rows (all threads; filtered at render).
    pub terminals: Vec<serde_json::Value>,
    /// Terminal id whose scrollback fetch is in flight.
    pub ctx_fetching: Option<String>,
    /// Staged `TerminalContextRecord`s for `message.dispatch.context`.
    pub terminal_contexts: Vec<TerminalContext>,

    /// The 10s background refresh (branch status, usage) is running.
    tick_started: bool,
    attach_subscription: Option<Subscription>,
    branch_subscription: Option<Subscription>,
    key_subscription: Option<Subscription>,
    hooks_installed: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            picker: None,
            attachments: Vec::new(),
            draft: String::new(),
            slash_open: false,
            slash_selection: 0,
            interaction_mode: "default".to_string(),
            runtime_mode: "full-access".to_string(),
            effort: String::new(),
            model_picked: false,
            runtime_mode_picked: false,
            dispatch_override: None,
            history_index: None,
            history_draft: String::new(),
            history_recalled: String::new(),
            attach_field: None,
            mention_open: false,
            mention_selection: 0,
            mention_token_start: 0,
            mention_query: String::new(),
            mention_files: Vec::new(),
            mention_searched: String::new(),
            mention_generation: 0,
            usage_summary: None,
            usage_loading: false,
            usage_fetched_at: None,
            usage_dismissed_day: None,
            branch: None,
            branch_status: None,
            branch_scope: None,
            branch_loading: false,
            branch_fetched_at: None,
            branch_refs: Vec::new(),
            branch_refs_loading: false,
            branch_refs_loaded: false,
            branch_refs_error: None,
            branch_pending: None,
            branch_field: None,
            term_meta_stream: None,
            terminals: Vec::new(),
            ctx_fetching: None,
            terminal_contexts: Vec::new(),
            tick_started: false,
            attach_subscription: None,
            branch_subscription: None,
            key_subscription: None,
            hooks_installed: false,
        }
    }
}

impl State {
    /// Take the staged attachments and serialize them for the `"attachments"`
    /// array of a `message.dispatch` command. Emits the contract
    /// `ChatAttachment` shape (`type`/`id`/`name`/`mimeType`/`sizeBytes`);
    /// `path` and `dataUrl` are extra fields the Rust provider can consume.
    /// The orchestrator should call this while building the dispatch payload
    /// so the composer clears on send.
    pub fn drain_attachments(&mut self) -> Vec<serde_json::Value> {
        std::mem::take(&mut self.attachments)
            .into_iter()
            .map(|attachment| match attachment.kind {
                AttachmentKind::File { path } => {
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    serde_json::json!({
                        "type": "file",
                        "id": uuid::Uuid::new_v4().to_string().replace('-', ""),
                        "name": attachment.name,
                        "mimeType": "application/octet-stream",
                        "sizeBytes": size.max(1),
                        "path": path,
                    })
                }
                AttachmentKind::Image { bytes, mime } => serde_json::json!({
                    "type": "image",
                    "id": uuid::Uuid::new_v4().to_string().replace('-', ""),
                    "name": attachment.name,
                    "mimeType": &mime,
                    "sizeBytes": bytes.len(),
                    "dataUrl": format!("data:{};base64,{}", mime, base64_encode(&bytes)),
                }),
                AttachmentKind::Text { content } => serde_json::json!({
                    "type": "file",
                    "id": uuid::Uuid::new_v4().to_string().replace('-', ""),
                    "name": attachment.name,
                    "mimeType": "text/plain",
                    "sizeBytes": content.len().max(1),
                    "text": content,
                }),
            })
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Model,
    InteractionMode,
    RuntimeMode,
    /// The `+` menu (attach file / paste image).
    Attach,
    /// Busy-thread run options (queue / steer / restart).
    RunOptions,
    /// Project branch list (`vcs.listRefs`).
    Branch,
    /// Running terminals to attach as message context.
    Terminals,
}

/// A staged terminal context — serialized as a `TerminalContextRecord` inside
/// `OrchestrationMessageContext.records` on `message.dispatch`.
pub struct TerminalContext {
    /// `contextId` on the wire record.
    pub context_id: String,
    /// Server-side terminal id (`terminalId` in the record).
    pub terminal_id: String,
    /// Owning thread — staged records are dropped if the selection moves on.
    pub thread_id: String,
    /// Chip/record label, e.g. "t1 · L40-L160".
    pub label: String,
    /// `terminalLabel` on the wire record.
    pub terminal_label: String,
    pub line_start: u64,
    pub line_end: u64,
    /// Captured scrollback text (ANSI stripped).
    pub text: String,
}

impl TerminalContext {
    fn record(&self) -> serde_json::Value {
        json!({
            "version": 1,
            "contextId": self.context_id,
            "kind": "terminal",
            "label": self.label,
            "terminalId": self.terminal_id,
            "terminalLabel": self.terminal_label,
            "lineStart": self.line_start,
            "lineEnd": self.line_end,
            "text": self.text,
        })
    }
}

/// A staged composer attachment (image bytes, file path, or pasted text).
pub struct Attachment {
    pub name: String,
    pub kind: AttachmentKind,
    /// Decoded pixels for the thumbnail strip and the expand overlay
    /// (`Image` kind only; `None` when decode failed → file-icon block).
    pub thumbnail: Option<Arc<RenderImage>>,
}

pub enum AttachmentKind {
    Image { bytes: Vec<u8>, mime: String },
    File { path: String },
    Text { content: String },
}

/// Static provider/model catalog for the model picker (the web client resolves
/// this from the server's provider list; the native client keeps it fixed).
/// Each row is `(instanceId, model, label)`.
pub(crate) const MODEL_CATALOG: &[(&str, &str, &str)] = &[
    ("devReplay", "showcase", "Replay · showcase"),
    ("devin", "swe-2-high", "Devin · SWE-2 High"),
    ("codex", "gpt-6-astra", "Codex · GPT-6 Astra"),
    ("codex", "gpt-5.3-codex", "Codex · GPT-5.3 Codex"),
    ("codex", "o4-mini", "Codex · o4-mini"),
    ("claudeAgent", "claude-opus-5-5", "Claude · Opus 5.5"),
    ("claudeAgent", "claude-sonnet-4-6", "Claude · Sonnet 4.6"),
    ("claudeAgent", "claude-haiku-5-5", "Claude · Haiku 5.5"),
    ("opencode", "openai/gpt-5", "OpenCode · GPT-5"),
    ("cursor", "composer-2", "Cursor · Composer 2"),
];

const INTERACTION_MODES: &[(&str, &str)] = &[("default", "Default"), ("plan", "Plan")];

const RUNTIME_MODES: &[(&str, &str)] = &[
    ("full-access", "Full access"),
    ("auto", "Auto"),
    ("auto-accept-edits", "Auto-accept edits"),
    ("approval-required", "Supervised"),
];

/// Reasoning-effort cycle for the footer chip; `""` emits no option so the
/// provider applies its own default.
const EFFORT_LEVELS: &[&str] = &["", "low", "medium", "high"];

fn effort_label(effort: &str) -> &'static str {
    match effort {
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        _ => "Default",
    }
}

/// Decode image bytes into a gpui-renderable image. gpui's software buffers
/// are BGRA, so R and B are swapped like `decode_static_image` does.
fn decode_thumbnail(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let mut data = image::load_from_memory(bytes).ok()?.into_rgba8();
    for pixel in data.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new([image::Frame::new(data)])))
}

struct SlashCommand {
    name: &'static str,
    description: &'static str,
}

const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "new",
        description: "Start a new thread",
    },
    SlashCommand {
        name: "plan",
        description: "Switch to plan mode",
    },
    SlashCommand {
        name: "model",
        description: "Choose a model",
    },
    SlashCommand {
        name: "compact",
        description: "Compact this thread",
    },
    SlashCommand {
        name: "stop",
        description: "Stop the running turn",
    },
    SlashCommand {
        name: "feedback",
        description: "Upload thread feedback to the provider (Codex)",
    },
];

/// Commands whose name matches the text typed after the leading `/`.
fn slash_commands_for(draft: &str) -> Vec<&'static SlashCommand> {
    let Some(rest) = draft.strip_prefix('/') else {
        return Vec::new();
    };
    if rest.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    SLASH_COMMANDS
        .iter()
        .filter(|command| command.name.starts_with(rest))
        .collect()
}

/// The draft's trailing `@token`: byte offset of the `@` and the query after
/// it. The `@` must start a word (BOF or after whitespace) and the token must
/// run to the end of the draft with no whitespace — that is what opens the
/// mention popup and what closes it on space/delete.
fn mention_token(draft: &str) -> Option<(usize, &str)> {
    let start = draft.rfind('@')?;
    if draft[..start]
        .chars()
        .last()
        .is_some_and(|c| !c.is_whitespace())
    {
        return None;
    }
    let query = &draft[start + 1..];
    if query.chars().any(char::is_whitespace) {
        return None;
    }
    Some((start, query))
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `YYYY-MM-DD` (UTC) from unix seconds — civil-from-days, no time dep.
fn day_string(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

/// Compact token count for the usage banner ("1.2k", "3.4M").
fn compact_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn danger() -> gpui::Rgba {
    rgb(0xff5a5a)
}

fn warning() -> gpui::Rgba {
    rgb(0xe8b45a)
}

/// `(total tokens, total costUsd, distinct sessions)` — bucket `sessions`
/// double-counts a session spanning models/days, so `sources` wins when the
/// server reports it.
fn usage_totals(summary: &serde_json::Value) -> (u64, f64, u64) {
    let mut tokens = 0u64;
    let mut cost = 0f64;
    let mut sessions = 0u64;
    for bucket in summary["buckets"].as_array().into_iter().flatten() {
        let t = &bucket["totals"];
        tokens += t["uncachedInputTokens"].as_u64().unwrap_or(0)
            + t["cachedInputTokens"].as_u64().unwrap_or(0)
            + t["cacheCreationTokens"].as_u64().unwrap_or(0)
            + t["outputTokens"].as_u64().unwrap_or(0);
        cost += bucket["costUsd"].as_f64().unwrap_or(0.);
        sessions += bucket["sessions"].as_u64().unwrap_or(0);
    }
    let distinct: u64 = summary["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["distinctSessions"].as_u64())
        .sum();
    (tokens, cost, if distinct > 0 { distinct } else { sessions })
}

/// Readable message out of a Failure `Exit` frame.
fn rpc_error(exit: &serde_json::Value) -> String {
    let error = &exit["cause"]["error"];
    for key in ["requirement", "detail", "message", "reason"] {
        if let Some(s) = error[key].as_str().filter(|s| !s.is_empty()) {
            return s.to_string();
        }
    }
    for candidate in [&exit["cause"]["message"], &exit["error"]["message"]] {
        if let Some(s) = candidate.as_str() {
            return s.to_string();
        }
    }
    "The request failed.".to_string()
}

/// Strip ANSI escape sequences (CSI, OSC, and two-byte ESC sequences) from
/// raw PTY scrollback so the terminal-context record is plain text.
fn strip_terminal_escapes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                // CSI: parameter/intermediate bytes then final @…~.
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC: until BEL or ST (ESC \).
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if n == '\u{7}' || (prev == '\u{1b}' && n == '\\') {
                        break;
                    }
                    prev = n;
                }
            }
            Some(_) | None => {}
        }
    }
    out
}

fn app_view(window: &mut Window) -> Option<Entity<AppView>> {
    window.root::<AppView>().flatten()
}

/// Install the app-wide keystroke hook once, from the first composer
/// interaction (key or click). The interceptor runs before action dispatch so
/// menu keys can be consumed instead of reaching the text field.
fn ensure_composer_hooks(window: &mut Window, cx: &mut gpui::App) {
    let Some(view) = app_view(window) else {
        return;
    };
    view.update(cx, |this, cx| {
        if !this.composer_state.tick_started {
            this.composer_state.tick_started = true;
            // First load now (branch chip, usage banner), then a slow tick.
            this.composer_periodic(cx);
            cx.spawn(async move |this, cx| {
                loop {
                    smol::Timer::after(Duration::from_secs(10)).await;
                    let alive = this
                        .update(cx, |this, cx| {
                            this.composer_periodic(cx);
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
        if this.composer_state.hooks_installed {
            return;
        }
        this.composer_state.hooks_installed = true;
        let subscription = cx.intercept_keystrokes(|event, window, cx| {
            on_composer_keystroke(event, window, cx);
        });
        this.composer_state.key_subscription = Some(subscription);
    });
}

fn on_composer_keystroke(event: &KeystrokeEvent, window: &mut Window, cx: &mut gpui::App) {
    // The text field applies this keystroke later in dispatch; re-read the
    // draft on the next frame so the mirror (and slash menu) stays current.
    window.on_next_frame(|window, cx| {
        let Some(view) = app_view(window) else {
            return;
        };
        view.update(cx, |this, cx| this.sync_composer_draft(window, cx));
    });

    let Some(view) = app_view(window) else {
        return;
    };
    let (
        slash_open,
        picker_open,
        attach_open,
        branch_open,
        browsing,
        overlay_open,
        draft_empty,
        mention_open,
        ctx_staged,
        feedback_draft,
    ) = {
        let this = view.read(cx);
        let state = &this.composer_state;
        (
            state.slash_open,
            state.picker.is_some(),
            state.attach_field.is_some(),
            state.branch_field.is_some(),
            state.history_index.is_some(),
            this.expanded_image.is_some(),
            state.draft.is_empty(),
            state.mention_open,
            this.staged_ctx_count() > 0,
            state.draft == "/feedback" || state.draft.starts_with("/feedback "),
        )
    };
    if !(slash_open
        || picker_open
        || attach_open
        || branch_open
        || browsing
        || overlay_open
        || draft_empty
        || mention_open
        || ctx_staged
        || feedback_draft)
    {
        return;
    }

    match event.keystroke.key.as_str() {
        "escape" => {
            // Only consume Esc when something is actually open — the plain
            // Escape keybinding (CloseOverlay) handles palette/settings when
            // the composer has nothing to dismiss.
            let consumed = view.update(cx, |this, cx| {
                if this.expanded_image.is_some() {
                    this.expanded_image = None;
                } else if this.composer_state.mention_open
                    || this.composer_state.slash_open
                    || this.composer_state.picker.is_some()
                    || this.composer_state.attach_field.is_some()
                    || this.composer_state.branch_field.is_some()
                {
                    this.composer_state.mention_open = false;
                    this.composer_state.slash_open = false;
                    this.composer_state.picker = None;
                    this.composer_state.attach_field = None;
                    this.composer_state.attach_subscription = None;
                    this.composer_state.branch_field = None;
                    this.composer_state.branch_subscription = None;
                    this.close_terminal_meta();
                } else {
                    return false;
                }
                cx.notify();
                true
            });
            if consumed {
                cx.stop_propagation();
            }
        }
        "enter" if mention_open => {
            let index = view.read(cx).composer_state.mention_selection;
            let applied = view.update(cx, |this, cx| {
                if this.mention_candidates().is_empty() {
                    return false;
                }
                this.apply_mention(index, window, cx);
                true
            });
            if applied {
                cx.stop_propagation();
            }
        }
        // "/feedback" or "/feedback <reason>" uploads thread feedback
        // (`provider.uploadFeedback`) instead of sending a message. Before
        // the slash-menu arm: a bare "/feedback" is a complete command.
        "enter" if feedback_draft => {
            let reason = view
                .read(cx)
                .composer_state
                .draft
                .trim_start_matches("/feedback")
                .trim()
                .to_string();
            view.update(cx, |this, cx| this.send_thread_feedback(reason, window, cx));
            cx.stop_propagation();
        }
        "enter" if slash_open => {
            let applied = view.update(cx, |this, cx| {
                let commands = slash_commands_for(&this.composer_state.draft);
                let index = this
                    .composer_state
                    .slash_selection
                    .min(commands.len().saturating_sub(1));
                match commands.get(index) {
                    Some(command) => {
                        this.apply_slash_command(command.name, window, cx);
                        true
                    }
                    None => false,
                }
            });
            if applied {
                cx.stop_propagation();
            }
        }
        // Staged terminal contexts ride `message.dispatch.context`, which the
        // `PromptSubmitted` path in lib.rs predates — the composer sends the
        // dispatch itself when records are staged. Not while a picker or an
        // inline field (attach path, branch name) owns Enter.
        "enter" if ctx_staged && !draft_empty && !picker_open && !attach_open && !branch_open => {
            view.update(cx, |this, cx| this.submit_with_context(window, cx));
            cx.stop_propagation();
        }
        // Click-only pickers don't select on Enter — swallow it so a stray
        // Enter doesn't fire a prompt while a menu is open. The branch-name
        // field needs its own Enter (`PromptSubmitted` → `vcs.createRef`).
        "enter" if picker_open && !branch_open => {
            view.update(cx, |this, cx| {
                this.composer_state.picker = None;
                this.composer_state.branch_field = None;
                this.composer_state.branch_subscription = None;
                this.close_terminal_meta();
                cx.notify();
            });
            cx.stop_propagation();
        }
        key @ ("up" | "down") if mention_open => {
            view.update(cx, |this, cx| {
                let count = this.mention_candidates().len();
                if count > 0 {
                    let selection = this.composer_state.mention_selection;
                    this.composer_state.mention_selection = if key == "down" {
                        (selection + 1) % count
                    } else {
                        selection.checked_sub(1).unwrap_or(count - 1)
                    };
                    cx.notify();
                }
            });
            cx.stop_propagation();
        }
        key @ ("up" | "down") if slash_open => {
            view.update(cx, |this, cx| {
                let count = slash_commands_for(&this.composer_state.draft).len();
                if count > 0 {
                    let selection = this.composer_state.slash_selection;
                    this.composer_state.slash_selection = if key == "down" {
                        (selection + 1) % count
                    } else {
                        selection.checked_sub(1).unwrap_or(count - 1)
                    };
                    cx.notify();
                }
            });
            cx.stop_propagation();
        }
        // ctrl/cmd-shift-s: stash the current draft (the web client's
        // composer.stash command).
        key if key.eq_ignore_ascii_case("s")
            && (event.keystroke.modifiers.control || event.keystroke.modifiers.platform)
            && event.keystroke.modifiers.shift =>
        {
            view.update(cx, |this, cx| this.stash_draft(window, cx));
            cx.stop_propagation();
        }
        // Prompt history: up from an empty composer walks backward through
        // submitted prompts; down steps forward and returns to the draft.
        key @ ("up" | "down") if (browsing || draft_empty) && !attach_open && !branch_open => {
            let backward = key == "up";
            view.update(cx, |this, cx| {
                this.step_prompt_history(backward, window, cx)
            });
            cx.stop_propagation();
        }
        _ => {}
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(TABLE[(word >> 18) as usize & 0x3f] as char);
        out.push(TABLE[(word >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(word >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[word as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

fn interaction_mode_label(mode: &str) -> &'static str {
    INTERACTION_MODES
        .iter()
        .find(|(value, _)| *value == mode)
        .map(|(_, label)| *label)
        .unwrap_or("Default")
}

fn runtime_mode_label(mode: &str) -> &'static str {
    RUNTIME_MODES
        .iter()
        .find(|(value, _)| *value == mode)
        .map(|(_, label)| *label)
        .unwrap_or("Full access")
}

/// Shared styling for the floating menus above the composer.
fn menu_panel() -> gpui::Div {
    div()
        .absolute()
        .bottom_full()
        .left_0()
        .right_0()
        .mb_2()
        .flex()
        .flex_col()
        .rounded_lg()
        .bg(theme::raised())
        .border_1()
        .border_color(theme::border())
        .py_1()
        .max_h(px(280.))
        .overflow_y_hidden()
        .occlude()
}

fn menu_row(
    id: impl Into<gpui::ElementId>,
    title: impl Into<SharedString>,
    detail: Option<SharedString>,
    active: bool,
) -> gpui::Stateful<gpui::Div> {
    let mut row = div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1p5()
        .text_xs()
        .cursor(gpui::CursorStyle::PointingHand)
        .when(active, |s| s.bg(theme::selected()).text_color(theme::fg()))
        .when(!active, |s| {
            s.text_color(theme::fg()).hover(|s| s.bg(theme::hover()))
        })
        .child(
            div()
                .min_w(px(120.))
                .text_color(theme::fg())
                .child(title.into()),
        );
    if let Some(detail) = detail {
        row = row.child(
            div()
                .flex_1()
                .truncate()
                .text_color(theme::fg_muted())
                .child(detail),
        );
    }
    row
}

/// Small footer control button (pill) that opens a picker.
fn footer_chip(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    kind: PickerKind,
    open: Option<PickerKind>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .text_xs()
        .cursor(gpui::CursorStyle::PointingHand)
        .when(open == Some(kind), |s| {
            s.bg(theme::selected()).text_color(theme::fg())
        })
        .when(open != Some(kind), |s| {
            s.text_color(theme::fg_muted())
                .hover(|s| s.bg(theme::hover()))
        })
        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
            ensure_composer_hooks(window, cx);
            let Some(view) = app_view(window) else {
                return;
            };
            view.update(cx, |this, cx| this.toggle_composer_picker(kind, cx));
        })
        .child(label.into())
        .child(div().text_color(theme::fg_muted()).child("▾"))
}

impl AppView {
    /// Submit the composer draft exactly like pressing Enter in the input —
    /// routes the `Submit` action to the composer's focusable node, which emits
    /// `PromptSubmitted` through the existing subscription in `lib.rs`.
    pub fn send_composer(&self, window: &mut Window, cx: &mut gpui::App) {
        self.composer
            .read(cx)
            .focus_handle(cx)
            .dispatch_action(&Submit, window, cx);
    }

    /// Re-read the composer text field and refresh the mirrored draft plus
    /// slash-menu visibility. Only notifies when the draft actually changed.
    fn sync_composer_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.update(cx, |field, cx| {
            let len = field.text_length_utf16(window, cx).unwrap_or(0);
            field
                .text_for_range(0..len, &mut None, window, cx)
                .unwrap_or_default()
        });
        let mut mention_search = false;
        {
            let state = &mut self.composer_state;
            if state.draft != text {
                state.draft = text;
                // Slash menu only while the draft is a bare "/prefix" —
                // "/feedback <reason>" is a complete command, not a menu.
                state.slash_open =
                    state.draft.starts_with('/') && !state.draft.chars().any(char::is_whitespace);
                state.slash_selection = 0;
                // A manual edit while browsing history drops out of history mode.
                if state.history_index.is_some() && state.draft != state.history_recalled {
                    state.history_index = None;
                }
                // @mention popup: a trailing "@token" keeps it open while the
                // query edits; anything else (space, cursor elsewhere) closes.
                if let Some((start, query)) =
                    mention_token(&state.draft).map(|(s, q)| (s, q.to_string()))
                {
                    if state.mention_query != query {
                        state.mention_query = query;
                        state.mention_selection = 0;
                        mention_search = true;
                    }
                    state.mention_open = true;
                    state.mention_token_start = start;
                } else {
                    state.mention_open = false;
                }
                cx.notify();
            }
        }
        if mention_search {
            self.schedule_mention_search(cx);
        }
    }

    /// Project key that prompt history and the draft stash hang off: the
    /// selected thread's project, else the project a new thread would be
    /// created in, else a shared "" bucket for the empty state.
    pub(crate) fn composer_project_key(&self) -> String {
        self.state
            .selected_thread()
            .map(|thread| thread.project_id.to_string())
            .or_else(|| self.state.projects.first().map(|p| p.id.to_string()))
            .unwrap_or_default()
    }

    /// `modelSelection` payload for `thread.create` / `message.dispatch`:
    /// instance + model, plus a reasoning-effort provider option when the
    /// effort chip picked one.
    pub(crate) fn model_selection(&self) -> serde_json::Value {
        let mut selection = serde_json::json!({
            "instanceId": self.provider,
            "model": self.model,
        });
        if !self.composer_state.effort.is_empty() {
            selection["options"] = serde_json::json!([{
                "id": "effort",
                "value": self.composer_state.effort,
            }]);
        }
        selection
    }

    /// True while the selected thread has a run in flight — a submit then
    /// queues behind it (`queue_after_active`) instead of interrupting.
    pub(crate) fn selected_thread_busy(&self) -> bool {
        self.state.selected_thread().is_some_and(|t| t.running())
    }

    /// Staged terminal-context records belonging to the selected thread.
    fn staged_ctx_count(&self) -> usize {
        let Some(id) = self.state.selected_thread().map(|t| t.id.to_string()) else {
            return 0;
        };
        self.composer_state
            .terminal_contexts
            .iter()
            .filter(|c| c.thread_id == id)
            .count()
    }

    /// Replace the composer field's content (reset + insert at caret 0).
    fn set_composer_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |field, cx| {
            field.reset(window, cx);
            field.replace_text_in_range(None, text, window, cx);
        });
    }

    /// Up/down through the persisted prompt history (web parity). `up`
    /// enters history mode only from an empty composer and parks the live
    /// draft; `down` past the newest entry restores it.
    fn step_prompt_history(&mut self, backward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.composer_project_key();
        let history_len = self.client_config.history_for(&project).len();
        if history_len == 0 {
            self.composer_state.history_index = None;
            return;
        }
        let next = match (backward, self.composer_state.history_index) {
            // First step back from a clean composer: park the live draft and
            // land on the newest entry.
            (true, None) if self.composer_state.draft.is_empty() => {
                self.composer_state.history_draft = self.composer_state.draft.clone();
                Some(history_len - 1)
            }
            (true, Some(index)) => Some(index.saturating_sub(1)),
            (false, Some(index)) if index + 1 < history_len => Some(index + 1),
            // Down past the newest entry: back to the parked draft.
            (false, Some(_)) => None,
            _ => return,
        };
        let text = match next {
            Some(index) => {
                self.composer_state.history_index = Some(index);
                self.client_config
                    .history_for(&project)
                    .get(index)
                    .cloned()
                    .unwrap_or_default()
            }
            None => {
                self.composer_state.history_index = None;
                std::mem::take(&mut self.composer_state.history_draft)
            }
        };
        self.composer_state.history_recalled = text.clone();
        self.composer_state.draft = text.clone();
        self.set_composer_text(&text, window, cx);
        cx.notify();
    }

    /// Park the current draft + file attachments and clear the composer —
    /// `ctrl-shift-s` or the Stash chip (the web client's prompt stash).
    fn stash_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.composer_state.draft.trim().to_string();
        let files: Vec<config::StashedFile> = self
            .composer_state
            .attachments
            .iter()
            .filter_map(|attachment| match &attachment.kind {
                AttachmentKind::File { path } => Some(config::StashedFile {
                    name: attachment.name.clone(),
                    path: path.clone(),
                }),
                _ => None,
            })
            .collect();
        if prompt.is_empty() && files.is_empty() {
            return;
        }
        let project = self.composer_project_key();
        self.client_config
            .push_stash(&project, config::StashedDraft { prompt, files });
        self.composer
            .update(cx, |field, cx| field.reset(window, cx));
        let state = &mut self.composer_state;
        state.draft.clear();
        state.attachments.clear();
        state.history_index = None;
        self.push_toast(ToastKind::Info, "Draft stashed", None, None, cx);
        cx.notify();
    }

    /// Restore the newest stashed draft — the "Stashed (n)" footer chip.
    fn restore_stash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.composer_project_key();
        let Some(entry) = self.client_config.pop_stash(&project) else {
            return;
        };
        self.set_composer_text(&entry.prompt, window, cx);
        self.composer_state.draft = entry.prompt;
        for file in entry.files {
            self.composer_state.attachments.push(Attachment {
                name: file.name,
                kind: AttachmentKind::File { path: file.path },
                thumbnail: None,
            });
        }
        cx.notify();
    }

    /// Stage files dropped on the composer (gpui delivers an external file
    /// drop as `ExternalPaths` to `on_drop` listeners under the pointer).
    fn stage_dropped_paths(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        for path in paths {
            let display = path.display().to_string();
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| display.clone());
            self.composer_state.attachments.push(Attachment {
                name,
                kind: AttachmentKind::File { path: display },
                thumbnail: None,
            });
        }
        cx.notify();
    }

    fn apply_slash_command(
        &mut self,
        name: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer
            .update(cx, |field, cx| field.reset(window, cx));
        self.composer_state.draft.clear();
        self.composer_state.slash_open = false;
        self.composer_state.slash_selection = 0;
        match name {
            // `/new`: the next message creates a fresh thread, like the web UI's
            // draft route. Local-only — nothing is dispatched.
            "new" => {
                self.composer_state.attachments.clear();
                self.state.selected = None;
            }
            "plan" => {
                self.composer_state.interaction_mode = "plan".to_string();
            }
            "model" => {
                self.composer_state.picker = Some(PickerKind::Model);
            }
            // The server treats a bare "/compact" user message as a compaction
            // request (IDLE_COMPACTION_TEXT in the contracts).
            "compact" => {
                if let Some(thread) = self.state.selected_thread() {
                    self.dispatch(serde_json::json!({
                        "type": "message.dispatch",
                        "commandId": uuid::Uuid::new_v4().to_string(),
                        "threadId": thread.id.to_string(),
                        "messageId": uuid::Uuid::new_v4().to_string(),
                        "text": "/compact",
                        "attachments": [],
                        "dispatchMode": { "type": "start_immediately" },
                        "createdBy": "user",
                        "creationSource": "web",
                    }));
                }
            }
            "stop" => {
                if let Some(thread) = self.state.selected_thread() {
                    self.dispatch(serde_json::json!({
                        "type": "thread.stop",
                        "commandId": uuid::Uuid::new_v4().to_string(),
                        "threadId": thread.id.to_string(),
                    }));
                }
            }
            // `/feedback` is not dispatched from the menu — it seeds the
            // draft so a reason can be typed; Enter then uploads it.
            "feedback" => {
                self.set_composer_text("/feedback ", window, cx);
                self.composer_state.draft = "/feedback ".to_string();
            }
            _ => {}
        }
        cx.notify();
    }

    /// Stage a clipboard item as attachments: images become image attachments,
    /// external paths become file attachments (matches the web client's paste
    /// behavior). Text clipboard content is left for the input's own paste.
    fn stage_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        for entry in &item.entries {
            match entry {
                ClipboardEntry::Image(image) => {
                    self.composer_state.attachments.push(Attachment {
                        name: format!("pasted-image.{}", image.format.extension()),
                        thumbnail: decode_thumbnail(&image.bytes),
                        kind: AttachmentKind::Image {
                            bytes: image.bytes.clone(),
                            mime: image.format.mime_type().to_string(),
                        },
                    });
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    for path in paths.paths() {
                        let display = path.display().to_string();
                        let name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| display.clone());
                        self.composer_state.attachments.push(Attachment {
                            name,
                            kind: AttachmentKind::File { path: display },
                            thumbnail: None,
                        });
                    }
                }
                ClipboardEntry::String(_) => {}
            }
        }
        cx.notify();
    }

    /// Open the inline path input staged by "Attach file…". Its `PromptSubmitted`
    /// (Enter) becomes a file attachment instead of a message.
    fn open_attach_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let field = cx.new(|cx| TextField::new("File path…", cx));
        let subscription =
            window.subscribe(&field, cx, |_field, event: &PromptSubmitted, window, cx| {
                let Some(view) = app_view(window) else {
                    return;
                };
                view.update(cx, |this, cx| {
                    let path = event.0.trim().to_string();
                    if !path.is_empty() {
                        let name = path
                            .rsplit(['/', '\\'])
                            .next()
                            .unwrap_or(path.as_str())
                            .to_string();
                        this.composer_state.attachments.push(Attachment {
                            name,
                            kind: AttachmentKind::File { path },
                            thumbnail: None,
                        });
                    }
                    this.composer_state.attach_field = None;
                    this.composer_state.attach_subscription = None;
                    cx.notify();
                });
            });
        field.read(cx).focus_handle(cx).focus(window, cx);
        self.composer_state.attach_field = Some(field);
        self.composer_state.attach_subscription = Some(subscription);
    }

    /// `(project_id, workspace_root)` for the selected thread's project —
    /// falling back to the first project so the empty-thread composer still
    /// has a scope for mention search.
    fn composer_project(&self) -> Option<(String, String)> {
        let project = self
            .state
            .selected_thread()
            .and_then(|thread| {
                self.state
                    .projects
                    .iter()
                    .find(|p| p.id == thread.project_id)
            })
            .or_else(|| self.state.projects.first())?;
        Some((project.id.to_string(), project.path.to_string()))
    }

    // ------------------------------------------------------------------
    // @mentions
    // ------------------------------------------------------------------

    /// Debounce file lookups ~200 ms behind the latest query edit.
    fn schedule_mention_search(&mut self, cx: &mut Context<Self>) {
        self.composer_state.mention_generation =
            self.composer_state.mention_generation.wrapping_add(1);
        let generation = self.composer_state.mention_generation;
        cx.spawn(async move |this, cx| {
            smol::Timer::after(Duration::from_millis(200)).await;
            let _ = this.update(cx, |this, cx| this.run_mention_search(generation, cx));
        })
        .detach();
    }

    /// `projects.searchEntries` for the current token, if the popup is still
    /// open on this generation and the query changed since the last fetch.
    fn run_mention_search(&mut self, generation: u64, cx: &mut Context<Self>) {
        let state = &self.composer_state;
        if state.mention_generation != generation || !state.mention_open {
            return;
        }
        let query = state.mention_query.clone();
        if query == state.mention_searched {
            return;
        }
        let Some((project_id, cwd)) = self.composer_project() else {
            return;
        };
        let Some(rx) = self.send_query(
            "projects.searchEntries",
            json!({ "projectId": project_id, "cwd": cwd, "query": query, "limit": 8, "kind": "file" }),
        ) else {
            return;
        };
        self.composer_state.mention_searched = query;
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.composer_state;
                if s.mention_generation != generation || !s.mention_open {
                    return;
                }
                s.mention_files = if exit["_tag"].as_str() == Some("Success") {
                    exit["value"]["entries"]
                        .as_array()
                        .map(|entries| {
                            entries
                                .iter()
                                .filter_map(|e| e["path"].as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// `(insert_text, title, kind)` rows for the mention popup — matching
    /// thread titles first, then server file hits.
    fn mention_candidates(&self) -> Vec<(String, String, &'static str)> {
        let query = self.composer_state.mention_query.to_lowercase();
        let mut out: Vec<(String, String, &'static str)> = Vec::new();
        for thread in &self.state.threads {
            if out.len() >= 3 {
                break;
            }
            let title = thread.title.to_string();
            if !query.is_empty() && !title.to_lowercase().contains(&query) {
                continue;
            }
            out.push((format!("@{title}"), format!("@{title}"), "thread"));
        }
        for path in &self.composer_state.mention_files {
            out.push((path.clone(), path.clone(), "file"));
        }
        out
    }

    /// Replace the trailing `@token` with the picked candidate + a space.
    fn apply_mention(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((insert, ..)) = self.mention_candidates().get(index).cloned() else {
            return;
        };
        let start = self
            .composer_state
            .mention_token_start
            .min(self.composer_state.draft.len());
        let draft = format!("{}{} ", &self.composer_state.draft[..start], insert);
        self.composer_state.draft = draft.clone();
        self.composer_state.mention_open = false;
        self.set_composer_text(&draft, window, cx);
        cx.notify();
    }

    fn render_mention_menu(&self) -> impl IntoElement {
        let candidates = self.mention_candidates();
        let selection = self.composer_state.mention_selection;
        let mut panel = menu_panel();
        if candidates.is_empty() {
            panel = panel.child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No matches"),
            );
        } else {
            for (index, (_insert, title, kind)) in candidates.iter().enumerate() {
                panel = panel.child(
                    menu_row(
                        ("mention", index),
                        title.clone(),
                        Some((*kind).into()),
                        index == selection,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        move |_event, window, cx| {
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| this.apply_mention(index, window, cx));
                        },
                    ),
                );
            }
        }
        panel
    }

    // ------------------------------------------------------------------
    // Footer pickers
    // ------------------------------------------------------------------

    /// Toggle a footer picker, running open/close side effects (load refs,
    /// open the terminal-metadata stream).
    fn toggle_composer_picker(&mut self, kind: PickerKind, cx: &mut Context<Self>) {
        let opening = self.composer_state.picker != Some(kind);
        self.composer_state.picker = if opening { Some(kind) } else { None };
        if self.composer_state.picker != Some(PickerKind::Branch) {
            self.composer_state.branch_field = None;
            self.composer_state.branch_subscription = None;
        }
        if opening {
            match kind {
                PickerKind::Branch => self.load_branch_refs(cx),
                PickerKind::Terminals => self.open_terminal_meta(cx),
                _ => {}
            }
        } else if kind == PickerKind::Terminals {
            self.close_terminal_meta();
        }
        cx.notify();
    }

    /// Slow background refresh driven by the composer tick: branch status
    /// (~20 s), usage summary (~5 min), and a stray terminal-meta stream
    /// if the ctx picker closed without its own teardown.
    fn composer_periodic(&mut self, cx: &mut Context<Self>) {
        if self.state.connection != ConnectionStatus::Connected {
            return;
        }
        let scope = self.composer_project().map(|(id, _)| id);
        if scope != self.composer_state.branch_scope {
            let s = &mut self.composer_state;
            s.branch_scope = scope.clone();
            s.branch = None;
            s.branch_status = None;
            s.branch_refs.clear();
            s.branch_refs_loaded = false;
            s.branch_refs_error = None;
            s.branch_pending = None;
            s.branch_fetched_at = None;
        }
        if scope.is_some()
            && !self.composer_state.branch_loading
            && self
                .composer_state
                .branch_fetched_at
                .is_none_or(|t| t.elapsed() > Duration::from_secs(20))
        {
            self.fetch_branch_status(cx);
        }
        if !self.composer_state.usage_loading
            && self
                .composer_state
                .usage_fetched_at
                .is_none_or(|t| t.elapsed() > Duration::from_secs(300))
        {
            self.fetch_usage_summary(cx);
        }
        if self.composer_state.picker != Some(PickerKind::Terminals)
            && self.composer_state.term_meta_stream.is_some()
        {
            self.close_terminal_meta();
        }
        if self.composer_state.picker != Some(PickerKind::Branch)
            && self.composer_state.branch_field.is_some()
        {
            self.composer_state.branch_field = None;
            self.composer_state.branch_subscription = None;
        }
    }

    // ------------------------------------------------------------------
    // Usage banner
    // ------------------------------------------------------------------

    /// `server.getUsageSummary` for today (UTC day resolution).
    fn fetch_usage_summary(&mut self, cx: &mut Context<Self>) {
        if self.composer_state.usage_loading {
            return;
        }
        let today = day_string(unix_now());
        let Some(rx) = self.send_query(
            "server.getUsageSummary",
            json!({
                "sinceDay": today,
                "untilDay": today,
                "timeZone": "UTC",
                "resolution": "day",
            }),
        ) else {
            return;
        };
        self.composer_state.usage_loading = true;
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.composer_state;
                s.usage_loading = false;
                s.usage_fetched_at = Some(Instant::now());
                if exit["_tag"].as_str() == Some("Success") {
                    s.usage_summary = Some(exit["value"].clone());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Compact banner over the composer when today's buckets carry usage.
    /// `getUsageSummary` exposes token totals, not provider limit
    /// percentages — the web's usage-limit bars have no wire equivalent —
    /// so this shows a totals notice that warns on heavy use.
    fn render_usage_banner(&self) -> gpui::Div {
        let mut banner = div();
        let Some(summary) = &self.composer_state.usage_summary else {
            return banner;
        };
        let (tokens, cost, sessions) = usage_totals(summary);
        if tokens == 0 {
            return banner;
        }
        let today = day_string(unix_now());
        if self.composer_state.usage_dismissed_day.as_deref() == Some(today.as_str()) {
            return banner;
        }
        let heavy = tokens >= 2_000_000 || cost >= 5.0;
        banner = banner
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .px_3()
            .py_1p5()
            .border_1()
            .text_xs()
            .when(heavy, |s| {
                s.border_color(warning())
                    .bg(rgba(0xe8b45a14))
                    .text_color(warning())
            })
            .when(!heavy, |s| {
                s.border_color(theme::border())
                    .bg(theme::raised())
                    .text_color(theme::fg_muted())
            })
            .child("◔")
            .child(div().flex_1().child(format!(
                "Usage today: {} · ${cost:.2} · {sessions} session{}",
                compact_tokens(tokens),
                if sessions == 1 { "" } else { "s" },
            )))
            .child(
                div()
                    .id("usage-dismiss")
                    .px_1()
                    .rounded_sm()
                    .cursor(gpui::CursorStyle::PointingHand)
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.composer_state.usage_dismissed_day = Some(today.clone());
                            cx.notify();
                        });
                    })
                    .child("×"),
            );
        banner
    }

    // ------------------------------------------------------------------
    // Branch chip + picker
    // ------------------------------------------------------------------

    /// `vcs.refreshStatus` — the footer chip's current-branch readout. The
    /// VCS pane owns its own copy (`vcs_state` is module-private), so the
    /// composer keeps a lightweight mirror.
    fn fetch_branch_status(&mut self, cx: &mut Context<Self>) {
        if self.composer_state.branch_loading {
            return;
        }
        let Some((_pid, cwd)) = self.composer_project() else {
            return;
        };
        let Some(rx) = self.send_query("vcs.refreshStatus", json!({ "cwd": cwd })) else {
            return;
        };
        self.composer_state.branch_loading = true;
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.composer_state;
                s.branch_loading = false;
                s.branch_fetched_at = Some(Instant::now());
                if exit["_tag"].as_str() == Some("Success") {
                    let merged = &exit["value"];
                    s.branch = merged["refName"]
                        .as_str()
                        .or_else(|| merged["branch"].as_str())
                        .map(str::to_owned);
                    s.branch_status = Some(merged.clone());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `vcs.listRefs` — fills the branch picker. Scoped to the project the
    /// chip was rendered for; replies under a stale scope drop.
    fn load_branch_refs(&mut self, cx: &mut Context<Self>) {
        if self.composer_state.branch_refs_loading {
            return;
        }
        let Some((project_id, cwd)) = self.composer_project() else {
            return;
        };
        let Some(rx) = self.send_query("vcs.listRefs", json!({ "cwd": cwd, "limit": 100 })) else {
            self.composer_state.branch_refs_error = Some("Not connected to a server.".to_string());
            cx.notify();
            return;
        };
        self.composer_state.branch_refs_loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.composer_state;
                if s.branch_scope.as_deref() != Some(project_id.as_str()) {
                    return;
                }
                s.branch_refs_loading = false;
                s.branch_refs_loaded = true;
                if exit["_tag"].as_str() == Some("Success") {
                    s.branch_refs = exit["value"]["refs"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    s.branch_refs_error = None;
                } else {
                    s.branch_refs_error = Some(rpc_error(&exit));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `vcs.switchRef` — check out the picked ref; the next status refresh
    /// updates the chip.
    fn switch_branch(&mut self, ref_name: String, cx: &mut Context<Self>) {
        if self.composer_state.branch_pending.is_some() {
            return;
        }
        let Some((_pid, cwd)) = self.composer_project() else {
            return;
        };
        let Some(rx) = self.send_query("vcs.switchRef", json!({ "cwd": cwd, "refName": ref_name }))
        else {
            self.push_toast(
                ToastKind::Error,
                "Branch switch failed",
                Some("Not connected to a server.".to_string()),
                None,
                cx,
            );
            return;
        };
        self.composer_state.branch_pending = Some(ref_name.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.composer_state;
                s.branch_pending = None;
                if exit["_tag"].as_str() == Some("Success") {
                    let name = exit["value"]["refName"]
                        .as_str()
                        .unwrap_or(&ref_name)
                        .to_string();
                    s.picker = None;
                    s.branch_fetched_at = None;
                    s.branch_refs_loaded = false;
                    this.push_toast(
                        ToastKind::Success,
                        format!("Switched to {name}"),
                        None,
                        None,
                        cx,
                    );
                    this.fetch_branch_status(cx);
                    this.load_branch_refs(cx);
                } else {
                    let message = rpc_error(&exit);
                    s.branch_refs_error = Some(message.clone());
                    this.push_toast(
                        ToastKind::Error,
                        "Branch switch failed",
                        Some(message),
                        None,
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Open the inline input staged by "New branch…" in the branch picker.
    /// Its `PromptSubmitted` (Enter) becomes a `vcs.createRef` call.
    fn open_branch_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let field = cx.new(|cx| TextField::new("New branch name…", cx));
        let subscription =
            window.subscribe(&field, cx, |_field, event: &PromptSubmitted, window, cx| {
                let Some(view) = app_view(window) else {
                    return;
                };
                view.update(cx, |this, cx| {
                    let name = event.0.trim().to_string();
                    this.composer_state.branch_field = None;
                    this.composer_state.branch_subscription = None;
                    if !name.is_empty() {
                        this.create_branch(name, cx);
                    } else {
                        cx.notify();
                    }
                });
            });
        field.read(cx).focus_handle(cx).focus(window, cx);
        self.composer_state.branch_field = Some(field);
        self.composer_state.branch_subscription = Some(subscription);
    }

    /// `vcs.createRef` — create the named ref and check it out
    /// (`switchRef: true`), then refresh the chip and the picker list.
    fn create_branch(&mut self, ref_name: String, cx: &mut Context<Self>) {
        if self.composer_state.branch_pending.is_some() {
            return;
        }
        let Some((_pid, cwd)) = self.composer_project() else {
            return;
        };
        let Some(rx) = self.send_query(
            "vcs.createRef",
            json!({ "cwd": cwd, "refName": ref_name, "switchRef": true }),
        ) else {
            self.push_toast(
                ToastKind::Error,
                "Branch create failed",
                Some("Not connected to a server.".to_string()),
                None,
                cx,
            );
            return;
        };
        self.composer_state.branch_pending = Some(ref_name.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.composer_state;
                s.branch_pending = None;
                if exit["_tag"].as_str() == Some("Success") {
                    s.picker = None;
                    s.branch_fetched_at = None;
                    s.branch_refs_loaded = false;
                    this.push_toast(
                        ToastKind::Success,
                        format!("Created {ref_name}"),
                        None,
                        None,
                        cx,
                    );
                    this.fetch_branch_status(cx);
                    this.load_branch_refs(cx);
                } else {
                    let message = rpc_error(&exit);
                    s.branch_refs_error = Some(message.clone());
                    this.push_toast(
                        ToastKind::Error,
                        "Branch create failed",
                        Some(message),
                        None,
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ------------------------------------------------------------------
    // Feedback + terminal context
    // ------------------------------------------------------------------

    /// `/feedback [reason]` — `provider.uploadFeedback` for the selected
    /// thread (Codex threads only, like the web composer).
    fn send_thread_feedback(
        &mut self,
        reason: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Clear the field + mirrors like a submit.
        self.composer
            .update(cx, |field, cx| field.reset(window, cx));
        self.composer_state.draft.clear();
        self.composer_state.slash_open = false;
        let Some(thread) = self.state.selected_thread() else {
            self.push_toast(
                ToastKind::Error,
                "No thread selected",
                Some("/feedback needs an open thread.".to_string()),
                None,
                cx,
            );
            return;
        };
        // Codex-only on the web; judge by the providers that ran this thread,
        // falling back to the connection default before any run exists.
        let is_codex = if thread.provider_instances.is_empty() {
            self.provider == "codex"
        } else {
            thread.provider_instances.iter().any(|p| p == "codex")
        };
        if !is_codex {
            self.push_toast(
                ToastKind::Info,
                "Feedback upload is only supported for Codex threads",
                None,
                None,
                cx,
            );
            return;
        }
        let mut payload = json!({ "threadId": thread.id.to_string() });
        if !reason.is_empty() {
            payload["reason"] = json!(reason);
        }
        let Some(rx) = self.send_query("provider.uploadFeedback", payload) else {
            self.push_toast(
                ToastKind::Error,
                "Feedback upload failed",
                Some("Not connected to a server.".to_string()),
                None,
                cx,
            );
            return;
        };
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                if exit["_tag"].as_str() == Some("Success") {
                    this.push_toast(
                        ToastKind::Success,
                        "Feedback uploaded to provider",
                        None,
                        None,
                        cx,
                    );
                } else {
                    this.push_toast(
                        ToastKind::Error,
                        "Feedback upload failed",
                        Some(rpc_error(&exit)),
                        None,
                        cx,
                    );
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Open `subscribeTerminalMetadata` while the ctx picker is open and
    /// collect rows into `composer_state.terminals` (the drawer keeps its
    /// own private copy; this is the composer-local view).
    fn open_terminal_meta(&mut self, cx: &mut Context<Self>) {
        if self.composer_state.term_meta_stream.is_some() {
            return;
        }
        let Some((stream_id, rx)) = self.open_stream("subscribeTerminalMetadata", json!({})) else {
            return;
        };
        self.composer_state.term_meta_stream = Some(stream_id.clone());
        cx.spawn(async move |this, cx| {
            while let Ok(chunk) = rx.recv().await {
                let alive = this
                    .update(cx, |this, cx| {
                        let s = &mut this.composer_state;
                        match chunk["type"].as_str() {
                            Some("snapshot") => {
                                s.terminals =
                                    chunk["terminals"].as_array().cloned().unwrap_or_default();
                            }
                            Some("upsert") => {
                                let t = &chunk["terminal"];
                                let tid = t["threadId"].as_str().unwrap_or("");
                                let eid = t["terminalId"].as_str().unwrap_or("");
                                match s.terminals.iter().position(|e| {
                                    e["threadId"].as_str() == Some(tid)
                                        && e["terminalId"].as_str() == Some(eid)
                                }) {
                                    Some(i) => s.terminals[i] = t.clone(),
                                    None => s.terminals.push(t.clone()),
                                }
                            }
                            Some("remove") => {
                                let tid = chunk["threadId"].as_str().unwrap_or("");
                                let eid = chunk["terminalId"].as_str().unwrap_or("");
                                s.terminals.retain(|e| {
                                    !(e["threadId"].as_str() == Some(tid)
                                        && e["terminalId"].as_str() == Some(eid))
                                });
                            }
                            _ => {}
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.composer_state.term_meta_stream.as_deref() == Some(stream_id.as_str()) {
                    this.composer_state.term_meta_stream = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Close the ctx picker's metadata stream, if open.
    fn close_terminal_meta(&mut self) {
        if let Some(id) = self.composer_state.term_meta_stream.take() {
            self.close_stream(&id);
        }
    }

    /// `terminal.observe` one-shot → stage the scrollback tail as a
    /// `TerminalContextRecord` for the next message.
    fn stage_terminal_context(
        &mut self,
        thread_id: String,
        terminal_id: String,
        label: String,
        cx: &mut Context<Self>,
    ) {
        if self.composer_state.ctx_fetching.is_some() {
            return;
        }
        self.composer_state.picker = None;
        self.composer_state.ctx_fetching = Some(terminal_id.clone());
        cx.notify();
        // `terminal.observe` is a stream RPC: open it, take the leading
        // `snapshot` chunk's scrollback, then interrupt the stream.
        let Some((stream_id, rx)) = self.open_stream(
            "terminal.observe",
            json!({ "threadId": thread_id, "terminalId": terminal_id }),
        ) else {
            self.composer_state.ctx_fetching = None;
            self.push_toast(
                ToastKind::Error,
                "Terminal context failed",
                Some("Not connected to a server.".to_string()),
                None,
                cx,
            );
            return;
        };
        cx.spawn(async move |this, cx| {
            let mut snapshot = None;
            while let Ok(chunk) = rx.recv().await {
                if chunk["type"].as_str() == Some("snapshot") {
                    snapshot = Some(chunk["snapshot"].clone());
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.close_stream(&stream_id);
                let s = &mut this.composer_state;
                s.ctx_fetching = None;
                let Some(snapshot) = snapshot else {
                    this.push_toast(
                        ToastKind::Error,
                        "Could not read terminal output",
                        None,
                        None,
                        cx,
                    );
                    return;
                };
                let raw = strip_terminal_escapes(snapshot["history"].as_str().unwrap_or(""));
                let mut lines: Vec<&str> = raw.lines().collect();
                while lines.last().is_some_and(|l| l.trim().is_empty()) {
                    lines.pop();
                }
                let line_start = lines.len().saturating_sub(120);
                let line_end = lines.len();
                let mut text = lines[line_start..].join("\n");
                if text.len() > 64_000 {
                    // Keep the tail — the most recent output — on a char boundary.
                    let mut start = text.len() - 64_000;
                    while !text.is_char_boundary(start) {
                        start += 1;
                    }
                    text = text[start..].to_string();
                }
                let record = TerminalContext {
                    context_id: format!("ctx-{}", uuid::Uuid::new_v4()),
                    terminal_id: terminal_id.clone(),
                    thread_id: thread_id.clone(),
                    label: format!("{label} · L{}-L{}", line_start + 1, line_end),
                    terminal_label: label.clone(),
                    line_start: line_start as u64,
                    line_end: line_end as u64,
                    text,
                };
                // One staged record per terminal — recapture replaces.
                s.terminal_contexts.retain(|c| c.terminal_id != terminal_id);
                s.terminal_contexts.push(record);
                cx.notify();
            });
        })
        .detach();
    }

    /// `message.dispatch` with staged terminal contexts in `context.records`
    /// — `on_prompt_submitted` predates the `context` field, so the composer
    /// sends this dispatch itself (mirroring its thread/mode bookkeeping).
    fn submit_with_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .composer
            .update(cx, |field, cx| {
                let len = field.text_length_utf16(window, cx).unwrap_or(0);
                field
                    .text_for_range(0..len, &mut None, window, cx)
                    .unwrap_or_default()
            })
            .trim()
            .to_string();
        if text.is_empty() || self.composer_state.terminal_contexts.is_empty() {
            return;
        }
        let Some(thread_id) = self.state.selected_thread().map(|t| t.id.to_string()) else {
            self.push_toast(
                ToastKind::Error,
                "No thread selected",
                Some("Terminal context needs an open thread.".to_string()),
                None,
                cx,
            );
            return;
        };
        // Staged records are scoped to their thread — a context captured on
        // thread A must not ride a dispatch to thread B.
        self.composer_state
            .terminal_contexts
            .retain(|c| c.thread_id == thread_id);
        // Mirror `TextField::submit` + `on_prompt_submitted` bookkeeping.
        self.composer
            .update(cx, |field, cx| field.reset(window, cx));
        self.state.messages.push(Message {
            role: Role::User,
            text: text.clone().into(),
        });
        self.composer_state.history_index = None;
        if self.state.connection != ConnectionStatus::Connected {
            self.state.messages.push(Message {
                role: Role::Agent,
                text: "Received — the Rust client is not wired to a 67 server yet.".into(),
            });
            cx.notify();
            return;
        }
        let project = self.composer_project_key();
        self.client_config.record_prompt(&project, &text);
        let attachments = serde_json::Value::Array(self.composer_state.drain_attachments());
        let dispatch_mode = match self.composer_state.dispatch_override.take() {
            Some(mode @ ("steer_active" | "restart_active")) => json!({
                "type": mode,
                "targetRunId": self
                    .state
                    .selected_thread()
                    .and_then(|t| t.latest_run_id.clone()),
            }),
            Some(mode) => json!({ "type": mode }),
            None if self.selected_thread_busy() => json!({ "type": "queue_after_active" }),
            None => json!({ "type": "start_immediately" }),
        };
        let records: Vec<_> = self
            .composer_state
            .terminal_contexts
            .iter()
            .map(TerminalContext::record)
            .collect();
        self.composer_state.terminal_contexts.clear();
        let mut payload = json!({
            "type": "message.dispatch",
            "commandId": uuid::Uuid::new_v4().to_string(),
            "threadId": thread_id,
            "messageId": uuid::Uuid::new_v4().to_string(),
            "text": text,
            "modelSelection": self.model_selection(),
            "attachments": attachments,
            "dispatchMode": dispatch_mode,
            "createdBy": "user",
            "creationSource": "web",
        });
        if !records.is_empty() {
            payload["context"] = json!({ "version": 1, "records": records });
        }
        self.dispatch(payload);
        self.composer_state.draft.clear();
        cx.notify();
    }

    fn render_slash_menu(&self) -> impl IntoElement {
        let commands = slash_commands_for(&self.composer_state.draft);
        let selection = self.composer_state.slash_selection;
        let mut panel = menu_panel();
        if commands.is_empty() {
            panel = panel.child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No matching command"),
            );
        } else {
            for (index, command) in commands.iter().enumerate() {
                let name = command.name;
                panel = panel.child(
                    menu_row(
                        ("slash", index),
                        format!("/{name}"),
                        Some(command.description.into()),
                        index == selection,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        move |_event, window, cx| {
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| this.apply_slash_command(name, window, cx));
                        },
                    ),
                );
            }
        }
        panel
    }

    fn render_picker(&self, kind: PickerKind) -> impl IntoElement {
        let mut panel = menu_panel();
        match kind {
            PickerKind::Model => {
                for (index, (instance, model, label)) in MODEL_CATALOG.iter().enumerate() {
                    let active = self.provider == *instance && self.model == *model;
                    panel = panel.child(
                        menu_row(
                            ("model", index),
                            *label,
                            Some(format!("{instance}/{model}").into()),
                            active,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            move |_event, window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| {
                                    this.provider = (*instance).to_string();
                                    this.model = (*model).to_string();
                                    this.composer_state.model_picked = true;
                                    this.composer_state.picker = None;
                                    cx.notify();
                                });
                            },
                        ),
                    );
                }
            }
            PickerKind::InteractionMode => {
                for (index, (value, label)) in INTERACTION_MODES.iter().enumerate() {
                    let active = self.composer_state.interaction_mode == *value;
                    panel = panel.child(
                        menu_row(("interaction", index), *label, None, active).on_mouse_down(
                            MouseButton::Left,
                            move |_event, window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| {
                                    this.composer_state.interaction_mode = (*value).to_string();
                                    this.composer_state.picker = None;
                                    this.set_thread_modes();
                                    cx.notify();
                                });
                            },
                        ),
                    );
                }
            }
            PickerKind::RuntimeMode => {
                for (index, (value, label)) in RUNTIME_MODES.iter().enumerate() {
                    let active = self.composer_state.runtime_mode == *value;
                    panel = panel.child(
                        menu_row(("runtime", index), *label, None, active).on_mouse_down(
                            MouseButton::Left,
                            move |_event, window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| {
                                    this.composer_state.runtime_mode = (*value).to_string();
                                    this.composer_state.runtime_mode_picked = true;
                                    this.composer_state.picker = None;
                                    this.set_thread_modes();
                                    cx.notify();
                                });
                            },
                        ),
                    );
                }
            }
            PickerKind::Attach => {
                panel = panel.child(
                    menu_row(
                        "attach-file",
                        "Attach file…",
                        Some("Type or paste a path".into()),
                        false,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        move |_event, window, cx| {
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| {
                                this.composer_state.picker = None;
                                this.open_attach_field(window, cx);
                                cx.notify();
                            });
                        },
                    ),
                );
                panel = panel.child(
                    menu_row(
                        "attach-image",
                        "Paste image",
                        Some("From the clipboard".into()),
                        false,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        move |_event, window, cx| {
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| {
                                this.composer_state.picker = None;
                                this.stage_clipboard(cx);
                            });
                        },
                    ),
                );
            }
            PickerKind::RunOptions => {
                // Busy-thread submit behavior → `dispatch_override`, consumed
                // by `on_prompt_submitted` into `dispatchMode`.
                const RUN_OPTIONS: &[(&str, &str, &str)] = &[
                    ("queue_after_active", "Queue", "Send after the current run"),
                    ("steer_active", "Steer", "Inject into the running turn"),
                    ("restart_active", "Restart", "Stop this run and start over"),
                ];
                let current = self
                    .composer_state
                    .dispatch_override
                    .unwrap_or("queue_after_active");
                for (index, (value, label, detail)) in RUN_OPTIONS.iter().enumerate() {
                    panel = panel.child(
                        menu_row(
                            ("runopt", index),
                            *label,
                            Some((*detail).into()),
                            *value == current,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            move |_event, window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| {
                                    this.composer_state.dispatch_override = Some(*value);
                                    this.composer_state.picker = None;
                                    cx.notify();
                                });
                            },
                        ),
                    );
                }
            }
            PickerKind::Branch => {
                let s = &self.composer_state;
                if let Some(field) = &s.branch_field {
                    panel = panel.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .border_b_1()
                            .border_color(theme::border())
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("Name:")
                            .child(div().flex_1().text_color(theme::fg()).child(field.clone()))
                            .child("↵ to create"),
                    );
                }
                if s.branch_refs_loading && s.branch_refs.is_empty() {
                    panel = panel.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("Loading refs…"),
                    );
                }
                if let Some(error) = &s.branch_refs_error {
                    panel = panel.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_xs()
                            .text_color(danger())
                            .child(error.clone()),
                    );
                }
                if s.branch_refs_loaded && s.branch_refs.is_empty() && !s.branch_refs_loading {
                    panel = panel.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("No refs"),
                    );
                }
                let mut list = div()
                    .id("branch-refs")
                    .flex()
                    .flex_col()
                    .max_h(px(240.))
                    .overflow_y_scroll();
                for (index, r) in s.branch_refs.iter().enumerate() {
                    let name = r["name"].as_str().unwrap_or("").to_string();
                    if name.is_empty() {
                        continue;
                    }
                    let current = r["current"].as_bool() == Some(true);
                    let mut detail = String::new();
                    if current {
                        detail.push_str("current");
                    } else if r["isDefault"].as_bool() == Some(true) {
                        detail.push_str("default");
                    }
                    if r["isRemote"].as_bool() == Some(true) {
                        if !detail.is_empty() {
                            detail.push_str(" · ");
                        }
                        detail.push_str("remote");
                    }
                    if s.branch_pending.as_deref() == Some(name.as_str()) {
                        detail = "switching…".to_string();
                    }
                    let ref_name = name.clone();
                    list = list.child(
                        menu_row(("bref", index), name, Some(detail.into()), current)
                            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| {
                                    this.switch_branch(ref_name.clone(), cx)
                                });
                            }),
                    );
                }
                panel = panel.child(list);
                // "New branch…" → inline name input → `vcs.createRef`.
                panel = panel.child(
                    div()
                        .id("branch-new")
                        .px_3()
                        .py_2()
                        .border_t_1()
                        .border_color(theme::border())
                        .text_xs()
                        .cursor(gpui::CursorStyle::PointingHand)
                        .text_color(theme::fg_muted())
                        .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                        .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| this.open_branch_field(window, cx));
                        })
                        .child("+ New branch…"),
                );
            }
            PickerKind::Terminals => {
                let s = &self.composer_state;
                let thread_id = self
                    .state
                    .selected_thread()
                    .map(|t| t.id.to_string())
                    .unwrap_or_default();
                let running: Vec<&serde_json::Value> = s
                    .terminals
                    .iter()
                    .filter(|t| {
                        t["status"].as_str() == Some("running")
                            && t["threadId"].as_str() == Some(thread_id.as_str())
                    })
                    .collect();
                if running.is_empty() {
                    panel = panel.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(if s.terminals.is_empty() {
                                "No terminals found"
                            } else {
                                "No running terminals on this thread"
                            }),
                    );
                }
                for (index, t) in running.iter().enumerate() {
                    let terminal_id = t["terminalId"].as_str().unwrap_or("").to_string();
                    let thread = t["threadId"].as_str().unwrap_or("").to_string();
                    let label = t["label"].as_str().unwrap_or(&terminal_id).to_string();
                    let cwd = t["cwd"].as_str().unwrap_or("").to_string();
                    let pending = s.ctx_fetching.as_deref() == Some(terminal_id.as_str());
                    panel = panel.child(
                        menu_row(
                            ("term", index),
                            label.clone(),
                            Some(if pending {
                                "capturing…".into()
                            } else {
                                cwd.into()
                            }),
                            false,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            move |_event, window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| {
                                    this.stage_terminal_context(
                                        thread.clone(),
                                        terminal_id.clone(),
                                        label.clone(),
                                        cx,
                                    );
                                });
                            },
                        ),
                    );
                }
            }
        }
        panel
    }

    fn render_attachment_strip(&self) -> impl IntoElement {
        let mut strip = div().flex().flex_wrap().gap_1().pb_2();
        for (index, attachment) in self.composer_state.attachments.iter().enumerate() {
            let mut chip = div()
                .flex()
                .items_center()
                .gap_1()
                .pl_2()
                .pr_1()
                .py_0p5()
                .rounded_md()
                .bg(theme::user_bubble())
                .border_1()
                .border_color(theme::border())
                .text_xs();
            match (&attachment.kind, &attachment.thumbnail) {
                (AttachmentKind::Image { .. }, Some(thumbnail)) => {
                    let thumbnail = thumbnail.clone();
                    let for_overlay = thumbnail.clone();
                    let name = attachment.name.clone();
                    chip = chip.child(
                        div()
                            .id(("attach-thumb", index))
                            .w(px(28.))
                            .h(px(28.))
                            .flex_none()
                            .rounded_sm()
                            .overflow_hidden()
                            .cursor(gpui::CursorStyle::PointingHand)
                            // Click expands into the image overlay.
                            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| {
                                    this.expanded_image = Some(ExpandedImage {
                                        name: name.clone(),
                                        image: for_overlay.clone(),
                                    });
                                    cx.notify();
                                });
                            })
                            .child(
                                img(gpui::ImageSource::Render(thumbnail))
                                    .size_full()
                                    .object_fit(ObjectFit::Cover),
                            ),
                    );
                }
                (AttachmentKind::Image { .. }, None) => {
                    // Decode failed — neutral block, no expand target.
                    chip = chip.child(
                        div()
                            .w(px(28.))
                            .h(px(28.))
                            .flex_none()
                            .rounded_sm()
                            .bg(theme::selected())
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme::fg_muted())
                            .child("img"),
                    );
                }
                _ => {}
            }
            strip = strip.child(
                chip.child(attachment.name.clone()).child(
                    div()
                        .id(("attach-remove", index))
                        .px_1()
                        .rounded_sm()
                        .cursor(gpui::CursorStyle::PointingHand)
                        .text_color(theme::fg_muted())
                        .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| {
                                if index < this.composer_state.attachments.len() {
                                    this.composer_state.attachments.remove(index);
                                }
                                cx.notify();
                            });
                        })
                        .child("×"),
                ),
            );
        }
        // Staged terminal-context records — the "ctx" footer picker stages
        // them; they ship in `message.dispatch.context.records`. Chips are
        // scoped to their owning thread (records are too, on submit).
        let selected_thread_id = self
            .state
            .selected_thread()
            .map(|t| t.id.to_string())
            .unwrap_or_default();
        for (index, ctx) in self
            .composer_state
            .terminal_contexts
            .iter()
            .enumerate()
            .filter(|(_, c)| c.thread_id == selected_thread_id)
        {
            strip = strip.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .pl_2()
                    .pr_1()
                    .py_0p5()
                    .rounded_md()
                    .bg(theme::selected())
                    .border_1()
                    .border_color(theme::border())
                    .text_xs()
                    .child("❯_")
                    .child(ctx.label.clone())
                    .child(
                        div()
                            .id(("ctx-remove", index))
                            .px_1()
                            .rounded_sm()
                            .cursor(gpui::CursorStyle::PointingHand)
                            .text_color(theme::fg_muted())
                            .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| {
                                    if index < this.composer_state.terminal_contexts.len() {
                                        this.composer_state.terminal_contexts.remove(index);
                                    }
                                    cx.notify();
                                });
                            })
                            .child("×"),
                    ),
            );
        }
        strip
    }

    pub fn render_composer(&self) -> impl IntoElement {
        let state = &self.composer_state;
        let composer = self.composer.clone();
        // While a turn runs, the send button becomes a stop square (web parity).
        let running = self.selected_thread_busy();
        let project_key = self.composer_project_key();
        let stashed = self.client_config.stash_count(&project_key);

        let mut root = div()
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .relative()
            .key_context("Composer")
            // External file drops stage as File attachments; gpui translates
            // the platform drop into a MouseUp carrying `ExternalPaths`.
            .can_drop(|value, _window, _cx| value.is::<ExternalPaths>())
            .drag_over::<ExternalPaths>(|style, _paths, _window, _cx| style.bg(theme::hover()))
            .on_drop::<ExternalPaths>(|paths, window, cx| {
                let Some(view) = app_view(window) else {
                    return;
                };
                view.update(cx, |this, cx| this.stage_dropped_paths(paths.paths(), cx));
            })
            .on_key_down(|_event, window, cx| {
                ensure_composer_hooks(window, cx);
            })
            .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                ensure_composer_hooks(window, cx);
            });

        // Floating menus above the composer.
        if state.mention_open {
            root = root.child(self.render_mention_menu());
        } else if state.slash_open {
            root = root.child(self.render_slash_menu());
        }
        if let Some(kind) = state.picker {
            root = root.child(self.render_picker(kind));
        }

        // Usage-summary notice (tokens/cost today) when the server has data.
        root = root.child(self.render_usage_banner());

        let mut card = div()
            .flex()
            .flex_col()
            .rounded_lg()
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border())
            .px_3()
            .py_2();

        if !state.attachments.is_empty() || self.staged_ctx_count() > 0 {
            card = card.child(self.render_attachment_strip());
        }
        if let Some(field) = &state.attach_field {
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pb_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Path:")
                    .child(div().flex_1().text_color(theme::fg()).child(field.clone()))
                    .child("↵ to attach · esc to cancel"),
            );
        }

        card = card.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("composer-attach")
                        .px_1()
                        .rounded_md()
                        .cursor(gpui::CursorStyle::PointingHand)
                        .text_sm()
                        .text_color(theme::fg_muted())
                        .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                        .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                            ensure_composer_hooks(window, cx);
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| {
                                this.toggle_composer_picker(PickerKind::Attach, cx);
                            });
                        })
                        .child("+"),
                )
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(theme::fg())
                        .child(composer.clone()),
                )
                .child(if running {
                    div()
                        .id("stop")
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(0x7a2e2e))
                        .cursor(gpui::CursorStyle::PointingHand)
                        .text_xs()
                        .text_color(gpui::white())
                        .hover(|s| s.bg(rgb(0x9a3a3a)))
                        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| this.stop_thread(cx));
                        })
                        .child("Stop")
                } else {
                    div()
                        .id("send")
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(theme::primary())
                        .cursor(gpui::CursorStyle::PointingHand)
                        .text_xs()
                        .text_color(gpui::white())
                        .hover(|s| s.bg(gpui::rgb(0x6d8cff)))
                        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                            ensure_composer_hooks(window, cx);
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| {
                                this.send_composer(window, cx);
                            });
                            // `PromptSubmitted` clears the field — re-sync the
                            // draft mirror now since no keystroke fired.
                            window.on_next_frame(|window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| this.sync_composer_draft(window, cx));
                            });
                        })
                        .child("Send")
                }),
        );

        root = root.child(card);

        // Footer control row: model pill + mode chips + empty-thread hint.
        let mut footer = div()
            .flex()
            .items_center()
            .gap_2()
            .px_1()
            .child(footer_chip(
                "composer-model",
                format!("{}/{}", self.provider, self.model),
                PickerKind::Model,
                state.picker,
            ))
            .child(footer_chip(
                "composer-interaction",
                interaction_mode_label(&state.interaction_mode),
                PickerKind::InteractionMode,
                state.picker,
            ))
            .child(footer_chip(
                "composer-runtime",
                runtime_mode_label(&state.runtime_mode),
                PickerKind::RuntimeMode,
                state.picker,
            ))
            // Reasoning-effort cycle: Default → low → medium → high, sent as
            // modelSelection.options[{id:"effort"}] when not Default.
            .child(
                div()
                    .id("composer-effort")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .text_xs()
                    .cursor(gpui::CursorStyle::PointingHand)
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::hover()))
                    .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            let current = EFFORT_LEVELS
                                .iter()
                                .position(|level| *level == this.composer_state.effort)
                                .unwrap_or(0);
                            this.composer_state.effort =
                                EFFORT_LEVELS[(current + 1) % EFFORT_LEVELS.len()].to_string();
                            cx.notify();
                        });
                    })
                    .child(format!("{} effort", effort_label(&state.effort))),
            );

        // Branch chip — selected thread's project; opens the ref picker.
        // Hidden until `vcs.refreshStatus` confirms the root is a git repo.
        if self.composer_project().is_some() {
            let is_repo = state
                .branch_status
                .as_ref()
                .and_then(|v| v["isRepo"].as_bool())
                == Some(true);
            if is_repo {
                let branch_label = state
                    .branch
                    .clone()
                    .filter(|b| !b.is_empty())
                    .unwrap_or_else(|| "(detached)".to_string());
                footer = footer.child(footer_chip(
                    "composer-branch",
                    format!("⎇ {branch_label}"),
                    PickerKind::Branch,
                    state.picker,
                ));
            }
        }

        // ctx chip — attach running terminals' scrollback as message
        // context (`context.records`, kind "terminal").
        if self.state.selected_thread().is_some()
            && self.state.connection == ConnectionStatus::Connected
        {
            let ctx_count = self.staged_ctx_count();
            footer = footer.child(footer_chip(
                "composer-ctx",
                if ctx_count == 0 {
                    "ctx".to_string()
                } else {
                    format!("ctx ({ctx_count})")
                },
                PickerKind::Terminals,
                state.picker,
            ));
        }

        // Busy-thread run options → `dispatch_override` (Queue default).
        if running {
            let run_label = match state.dispatch_override {
                Some("steer_active") => "Steer",
                Some("restart_active") => "Restart",
                _ => "Queue",
            };
            footer = footer.child(footer_chip(
                "composer-runopt",
                format!("⏵ {run_label}"),
                PickerKind::RunOptions,
                state.picker,
            ));
        }

        footer = footer.child(div().flex_1());

        // Draft stash chips: Stash while there's content, Stashed (n) while
        // entries wait to be restored.
        if !state.draft.is_empty() || !state.attachments.is_empty() {
            footer = footer.child(
                div()
                    .id("composer-stash")
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .cursor(gpui::CursorStyle::PointingHand)
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                    .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                        ensure_composer_hooks(window, cx);
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| this.stash_draft(window, cx));
                    })
                    .child("Stash"),
            );
        }
        if stashed > 0 {
            footer = footer.child(
                div()
                    .id("composer-stashed")
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .text_xs()
                    .cursor(gpui::CursorStyle::PointingHand)
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                    .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                        ensure_composer_hooks(window, cx);
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| this.restore_stash(window, cx));
                    })
                    .child(format!("Stashed ({stashed})")),
            );
        }

        if running {
            footer = footer.child(div().text_xs().text_color(theme::fg_muted()).child(
                match state.dispatch_override {
                    Some("steer_active") => "⏎ steers the running turn",
                    Some("restart_active") => "⏎ restarts the run",
                    _ => "⏎ queues behind the current run",
                },
            ));
        }
        if self.state.selected.is_none() {
            footer = footer.child(
                div()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No thread selected — this will start a new thread"),
            );
        }

        root.child(footer)
    }
}

/// Data for the expanded-image overlay (set when a thumbnail is clicked).
pub struct ExpandedImage {
    pub name: String,
    /// Decoded attachment pixels, shared with the staged thumbnail.
    pub image: Arc<RenderImage>,
}

impl AppView {
    /// Centered modal for a staged image attachment — dim backdrop click or
    /// Esc closes. Rendered last in the root so it sits above other overlays.
    pub(crate) fn render_image_overlay(&self, cx: &mut Context<Self>) -> gpui::Div {
        let Some(expanded) = &self.expanded_image else {
            return div();
        };
        // Fit the image inside a fixed box, preserving aspect ratio.
        let native = expanded.image.size(0);
        let (w, h) = (native.width.0 as f32, native.height.0 as f32);
        let (fit_w, fit_h) = if w > 0. && h > 0. {
            let scale = (680. / w).min(440. / h);
            (px((w * scale).max(1.)), px((h * scale).max(1.)))
        } else {
            (px(320.), px(240.))
        };
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000b3))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, _window, cx| {
                    this.expanded_image = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded_lg()
                    .bg(theme::raised())
                    .border_1()
                    .border_color(theme::border())
                    .p_4()
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_this, _event, _window, cx| cx.stop_propagation()),
                    )
                    .child(
                        img(gpui::ImageSource::Render(expanded.image.clone()))
                            .w(fit_w)
                            .h(fit_h)
                            .object_fit(ObjectFit::Contain),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(expanded.name.clone())
                            .child("esc to close"),
                    ),
            )
    }
}
