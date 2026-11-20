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

use std::io::Read as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    BoxShadow, ClipboardEntry, ClipboardItem, Context, Entity, EntityInputHandler, ExternalPaths,
    Focusable, FontWeight, KeystrokeEvent, MouseButton, ObjectFit, RenderImage, SharedString,
    Subscription, Window, div, img, prelude::*, px, rgb, rgba,
};
use serde_json::json;

use crate::AppView;
use crate::config;
use crate::icons::{Icon, icon};
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

    /// Provider rows from `server.getConfig` — feeds the usage banner's
    /// real limit percentages and prompt-link driver names.
    pub config_providers: Vec<serde_json::Value>,
    /// `usageLimitSources` snapshots when the server reports them (accounts
    /// whose limits supplement the provider rows).
    pub usage_limit_sources: Vec<serde_json::Value>,
    config_loading: bool,
    config_fetched_at: Option<Instant>,

    /// Active dictation capture (mic recording), toggled by the footer chip
    /// or the `composer.dictation` chord.
    pub dictation: Option<Dictation>,
    /// `environment.transcribePcm` in flight — the chip reads "…".
    dictation_transcribing: bool,
    /// Half-second counter while recording; alternates the chip's red pulse.
    pub dictation_pulse: u64,

    /// Last `guestShare.create` URL (Share menu shows it with Copy/Open).
    pub share_url: Option<String>,
    /// Thread `share_url` belongs to — a stale link never renders for a
    /// different selection.
    pub share_thread: String,
    /// `guestShare.create` in flight.
    pub share_creating: bool,
    /// Last create error, shown inside the Share menu.
    pub share_error: Option<String>,

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
            config_providers: Vec::new(),
            usage_limit_sources: Vec::new(),
            config_loading: false,
            config_fetched_at: None,
            dictation: None,
            dictation_transcribing: false,
            dictation_pulse: 0,
            share_url: None,
            share_thread: String::new(),
            share_creating: false,
            share_error: None,
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
    /// Guest-share link + hosted prompt link.
    Share,
    /// The `…` overflow menu — web `CompactComposerControlsMenu`: footer
    /// controls with no resting slot (interaction mode, permissions, effort,
    /// terminal context, share).
    Overflow,
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

#[allow(dead_code)] // `Text` is the wire shape for pasted-text attachments.
pub enum AttachmentKind {
    Image { bytes: Vec<u8>, mime: String },
    File { path: String },
    Text { content: String },
}

/// A live microphone capture: a recorder child writing raw s16 mono PCM at
/// 16 kHz to stdout, drained by a reader thread into `pcm`.
pub struct Dictation {
    child: std::process::Child,
    pcm: Arc<Mutex<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    started: Instant,
}

/// Recorder commands tried in order — PipeWire, PulseAudio, then ALSA. All
/// emit raw s16 mono PCM at 16 kHz on stdout, which is what
/// `environment.transcribePcm` expects.
const RECORDERS: &[(&str, &[&str])] = &[
    (
        "pw-record",
        &[
            "--target",
            "0",
            "--rate",
            "16000",
            "--channels",
            "1",
            "--format",
            "s16",
            "-",
        ],
    ),
    (
        "parecord",
        &["--rate=16000", "--channels=1", "--format=s16le", "--raw"],
    ),
    (
        "arecord",
        &["-f", "S16_LE", "-r", "16000", "-c", "1", "-t", "raw", "-"],
    ),
];

/// ~16 minutes of 16 kHz s16 mono audio — the reader stops collecting past
/// this so a forgotten recording cannot grow without bound.
const DICTATION_MAX_BYTES: usize = 32_000_000;

/// Hosted app origin for generated prompt links (`DEFAULT_HOSTED_APP_URL`
/// in packages/shared/src/connectAuth.ts); `C67_HOSTED_APP_URL` overrides.
const DEFAULT_HOSTED_APP_URL: &str = "https://app.67.codes";

/// `PROMPT_LINK_MAX_PROMPT_LENGTH` in packages/shared/src/promptLinks.ts.
const PROMPT_LINK_MAX_PROMPT_LENGTH: usize = 8_000;

/// Percent-encode for a URL query value (unreserved characters only).
fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
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

/// Best-effort 64px preview for a staged file path — image files render as
/// square previews like the web client. Only the preview is decoded; the
/// attachment stays `AttachmentKind::File`, so the dispatch payload is
/// unchanged. Size-capped so a pathological path cannot stall the composer.
fn file_thumbnail(path: &str) -> Option<Arc<RenderImage>> {
    let looks_like_image = path
        .rsplit('.')
        .next()
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif"
            )
        })
        .unwrap_or(false);
    if !looks_like_image {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > 32 * 1024 * 1024 {
        return None;
    }
    decode_thumbnail(&std::fs::read(path).ok()?)
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

/// Failure detail for `environment.transcribePcm` — the `VoiceInputErrorPill`
/// equivalent: a CloudSpeechError key reason appends the settings hint.
fn dictation_error_detail(exit: &serde_json::Value) -> String {
    let message = rpc_error(exit);
    let reason = exit["cause"]["error"]["reason"].as_str().unwrap_or("");
    match reason {
        "missing-key" | "unreadable-key" | "rejected-key" => {
            format!("{message} Configure cloud speech keys in Settings → Voice.")
        }
        _ => message,
    }
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

    // `composer.dictation` — the shared default is mod+shift+d with
    // `when: "!terminalFocus"` (packages/shared/src/keybindings.ts), so a
    // focused terminal keeps the chord for itself (splitVertical there).
    if event.keystroke.key.eq_ignore_ascii_case("d")
        && event.keystroke.modifiers.shift
        && (event.keystroke.modifiers.control || event.keystroke.modifiers.platform)
        && !event.keystroke.modifiers.alt
    {
        let terminal_focus = {
            let this = view.read(cx);
            this.terminal_state.drawer_open
                && window
                    .focused(cx)
                    .is_some_and(|handle| !this.composer_field_focused(&handle, cx))
        };
        if !terminal_focus {
            view.update(cx, |this, cx| this.toggle_dictation(window, cx));
            cx.stop_propagation();
        }
        return;
    }

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

/// The composer card's drop shadow in dark mode
/// (`--shadow-composer-dark: 0 14px 32px -18px black·75%`).
fn composer_shadow() -> BoxShadow {
    BoxShadow::new(px(0.), px(14.), rgba(0x000000bf).into())
        .blur_radius(px(32.))
        .spread_radius(px(-18.))
}

/// The card's inset top-edge highlight
/// (`inset 0 1px 0 var(--composer-highlight)`).
fn composer_highlight_shadow() -> BoxShadow {
    BoxShadow::new(px(0.), px(1.), theme::composer_highlight().into()).inset()
}

/// The send/stop buttons' inset top-edge highlight
/// (`inset 0 1px 0 var(--inset-shadow-control-highlight)`).
fn control_highlight_shadow() -> BoxShadow {
    BoxShadow::new(px(0.), px(1.), theme::control_highlight().into()).inset()
}

/// The dropdown popup's shadow (`0 18px 44px -18px black·80%`).
fn popup_shadow() -> BoxShadow {
    BoxShadow::new(px(0.), px(18.), rgba(0x000000cc).into())
        .blur_radius(px(44.))
        .spread_radius(px(-18.))
}

/// Picker popups above the composer — web `dropdown-glass`: popover fill,
/// contrast-fg·10% outline, rounded-lg, deep drop shadow, p-1 item gutter.
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
        .border_color(theme::popup_border())
        .shadow(vec![popup_shadow()])
        .p_1()
        .max_h(px(280.))
        .overflow_y_hidden()
        .occlude()
}

/// The slash/mention suggestion drawer — an attached `ComposerBanner.Surface`
/// in the web client: the composer glass fill, the 5% composer outline,
/// rounded-2xl, the composer drop shadow, p-2 item gutter.
fn drawer_panel() -> gpui::Div {
    div()
        .absolute()
        .bottom_full()
        .left_0()
        .right_0()
        .mb_2()
        .flex()
        .flex_col()
        .rounded_2xl()
        .bg(theme::composer_surface())
        .border_1()
        .border_color(theme::composer_outline())
        .shadow(vec![composer_shadow()])
        .p_2()
        .max_h(px(288.))
        .overflow_y_hidden()
        .occlude()
}

/// A picker row — web `Menu` item: min-h-7, px-2 py-1, rounded-sm, text-sm;
/// the checked option sits on `bg-foreground/[0.08]` like a menu radio item.
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
        .min_h(px(28.))
        .px_2()
        .py_1()
        .rounded_sm()
        .text_sm()
        .cursor(gpui::CursorStyle::PointingHand)
        .when(active, |s| {
            s.bg(theme::command_highlight()).text_color(theme::fg())
        })
        .when(!active, |s| {
            s.text_color(theme::fg()).hover(|s| s.bg(theme::accent()))
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
                .text_xs()
                .text_color(theme::fg_muted())
                .child(detail),
        );
    }
    row
}

/// A suggestion row in the slash/mention drawer — web `CommandItem` in the
/// composer menu: min-h-7, px-2 py-1.5, rounded-lg; an optional leading icon
/// (16px secondary-label ink), the label column is medium xs, the description
/// is xs secondary-label.
fn drawer_row(
    id: impl Into<gpui::ElementId>,
    leading: Option<Icon>,
    title: impl Into<SharedString>,
    detail: Option<SharedString>,
    active: bool,
) -> gpui::Stateful<gpui::Div> {
    let mut row = div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .min_h(px(28.))
        .px_2()
        .py_1p5()
        .rounded_lg()
        .cursor(gpui::CursorStyle::PointingHand)
        .when(active, |s| {
            s.bg(theme::accent()).text_color(theme::accent_fg())
        })
        .when(!active, |s| s.text_color(theme::fg()));
    if let Some(glyph) = leading {
        row = row.child(
            div()
                .flex_none()
                .w(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .child(icon(glyph, 16.).text_color(theme::secondary_label())),
        );
    }
    row = row.child(
        div()
            .w(px(160.))
            .flex_none()
            .truncate()
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .child(title.into()),
    );
    if let Some(detail) = detail {
        row = row.child(
            div()
                .flex_1()
                .truncate()
                .text_xs()
                .text_color(theme::fg_muted())
                .child(detail),
        );
    }
    row
}

/// Empty-state line inside the slash/mention drawer (web `px-5 pt-3.5 pb-7`
/// `text-xs text-secondary-label`).
fn drawer_empty(label: &'static str) -> gpui::Div {
    div()
        .px_5()
        .pt_3p5()
        .pb_7()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(label)
}

/// ComposerControl base (apps/web): h-7, px-2, gap-1.5, borderless, rounded
/// control radius, text-sm medium — the footer chip look.
fn control_chip(id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .gap_1p5()
        .h(px(28.))
        .px_2()
        .rounded(px(theme::CONTROL_RADIUS))
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .cursor(gpui::CursorStyle::PointingHand)
}

/// The xs control used under the card (context strip) — `ComposerControl
/// size="xs"`: h-6, px-1.75, gap-1, text-xs, muted ink.
fn context_chip(id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_1()
        .h(px(24.))
        .px(px(7.))
        .rounded(px(theme::CONTROL_RADIUS))
        .text_xs()
        .whitespace_nowrap()
        .cursor(gpui::CursorStyle::PointingHand)
        .text_color(theme::fg_muted())
}

/// A 28px ghost icon button — `Button size="icon-sm" variant="ghost"`.
fn ghost_icon_button(id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .w(px(28.))
        .h(px(28.))
        .rounded(px(theme::CONTROL_RADIUS))
        .text_color(theme::fg_muted())
        .cursor(gpui::CursorStyle::PointingHand)
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

    /// True when `handle` is the focus of one of the composer's own text
    /// fields (input, attach path, branch name) — the `composer.dictation`
    /// chord's `!terminalFocus` guard treats anything else under an open
    /// terminal drawer as terminal focus.
    fn composer_field_focused(&self, handle: &gpui::FocusHandle, cx: &gpui::App) -> bool {
        [
            Some(&self.composer),
            self.composer_state.attach_field.as_ref(),
            self.composer_state.branch_field.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|field| field.read(cx).focus_handle(cx) == *handle)
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
            let thumbnail = file_thumbnail(&file.path);
            self.composer_state.attachments.push(Attachment {
                name: file.name,
                kind: AttachmentKind::File { path: file.path },
                thumbnail,
            });
        }
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Dictation
    // ------------------------------------------------------------------

    /// Mic chip / `composer.dictation` chord: first click records, second
    /// click stops and sends the PCM to `environment.transcribePcm`.
    fn toggle_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_state.dictation.is_some() {
            self.finish_dictation(window, cx);
        } else {
            self.start_dictation(cx);
        }
    }

    /// Spawn the first available recorder (`pw-record` → `parecord` →
    /// `arecord`) and drain its PCM stdout on a reader thread. The button
    /// stays enabled with no recorder installed — the user gets a toast
    /// naming the packages instead of a silently dead control.
    fn start_dictation(&mut self, cx: &mut Context<Self>) {
        if self.composer_state.dictation.is_some() || self.composer_state.dictation_transcribing {
            return;
        }
        if self.state.connection != ConnectionStatus::Connected {
            self.push_toast(
                ToastKind::Error,
                "Dictation needs a connected server",
                Some("The transcript is sent to the server for speech-to-text.".to_string()),
                None,
                cx,
            );
            return;
        }
        let mut spawned = None;
        for (bin, args) in RECORDERS {
            if let Ok(child) = std::process::Command::new(bin)
                .args(*args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                spawned = Some(child);
                break;
            }
        }
        let Some(mut child) = spawned else {
            self.push_toast(
                ToastKind::Error,
                "No audio recorder found",
                Some(
                    "Install pipewire-utils (pw-record), pulseaudio-utils (parecord), or alsa-utils (arecord)."
                        .to_string(),
                ),
                None,
                cx,
            );
            return;
        };
        let pcm: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let reader = child.stdout.take().map(|mut stdout| {
            let sink = pcm.clone();
            std::thread::spawn(move || {
                let mut buffer = [0u8; 8192];
                loop {
                    match stdout.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Ok(mut data) = sink.lock() {
                                if data.len() < DICTATION_MAX_BYTES {
                                    data.extend_from_slice(&buffer[..n]);
                                }
                            }
                        }
                    }
                }
            })
        });
        self.composer_state.dictation = Some(Dictation {
            child,
            pcm,
            reader,
            started: Instant::now(),
        });
        self.composer_state.dictation_pulse = 0;
        cx.notify();
        // Half-second repaint for the pulsing chip + elapsed readout.
        cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(Duration::from_millis(500)).await;
                let recording = this
                    .update(cx, |this, cx| {
                        this.composer_state.dictation_pulse =
                            this.composer_state.dictation_pulse.wrapping_add(1);
                        cx.notify();
                        this.composer_state.dictation.is_some()
                    })
                    .unwrap_or(false);
                if !recording {
                    break;
                }
            }
        })
        .detach();
    }

    /// Stop the recorder, base64 the collected PCM, and send
    /// `environment.transcribePcm`. A Success transcript is appended to the
    /// draft; a Failure toasts the server message plus a settings hint when
    /// the failure looks like an unconfigured cloud key.
    fn finish_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut dictation) = self.composer_state.dictation.take() else {
            return;
        };
        if let Err(error) = dictation.child.kill() {
            eprintln!("[dictation] recorder kill failed: {error}");
        }
        if let Err(error) = dictation.child.wait() {
            eprintln!("[dictation] recorder wait failed: {error}");
        }
        if let Some(reader) = dictation.reader.take() {
            if reader.join().is_err() {
                eprintln!("[dictation] reader thread panicked");
            }
        }
        let bytes = std::mem::take(&mut *dictation.pcm.lock().unwrap_or_else(|e| e.into_inner()));
        cx.notify();
        if bytes.is_empty() {
            self.push_toast(
                ToastKind::Error,
                "No audio captured",
                Some("Check microphone permissions and the default input source.".to_string()),
                None,
                cx,
            );
            return;
        }
        // The wire also accepts `provider`/`model`; the server resolves its
        // own cloud-speech defaults when they are omitted (this client has
        // no voice-destination settings yet).
        let Some(rx) = self.send_query(
            "environment.transcribePcm",
            json!({ "pcm": base64_encode(&bytes) }),
        ) else {
            self.push_toast(
                ToastKind::Error,
                "Dictation failed",
                Some("Not connected to a server.".to_string()),
                None,
                cx,
            );
            return;
        };
        self.composer_state.dictation_transcribing = true;
        cx.spawn_in(window, async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update_in(cx, |this, window, cx| {
                this.composer_state.dictation_transcribing = false;
                if exit["_tag"].as_str() == Some("Success") {
                    let text = exit["value"]["text"]
                        .as_str()
                        .or_else(|| exit["value"]["transcript"].as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if text.is_empty() {
                        this.push_toast(ToastKind::Info, "No speech detected", None, None, cx);
                    } else {
                        let mut draft = this.composer_state.draft.clone();
                        if !draft.is_empty() && !draft.ends_with(char::is_whitespace) {
                            draft.push(' ');
                        }
                        draft.push_str(&text);
                        this.composer_state.draft = draft.clone();
                        this.composer_state.slash_open = false;
                        this.composer_state.mention_open = false;
                        this.set_composer_text(&draft, window, cx);
                    }
                } else {
                    this.push_toast(
                        ToastKind::Error,
                        "Dictation failed",
                        Some(dictation_error_detail(&exit)),
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
    // Server config (provider usage limits)
    // ------------------------------------------------------------------

    /// `server.getConfig` — `providers[].usageLimits` feeds the banner's
    /// real "X% of limit" numbers; `instanceId → driver` feeds prompt
    /// links. Same ~5 min cadence as `getUsageSummary`.
    fn fetch_provider_config(&mut self, cx: &mut Context<Self>) {
        if self.composer_state.config_loading {
            return;
        }
        let Some(rx) = self.send_query("server.getConfig", json!({})) else {
            return;
        };
        self.composer_state.config_loading = true;
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.composer_state;
                s.config_loading = false;
                s.config_fetched_at = Some(Instant::now());
                if exit["_tag"].as_str() == Some("Success") {
                    if let Some(providers) = exit["value"]["providers"].as_array() {
                        s.config_providers = providers.clone();
                    }
                    // `usageLimitSources` is stream-only opt-in on the real
                    // server; servers that include it still populate it.
                    if let Some(sources) = exit["value"]["usageLimitSources"].as_array() {
                        s.usage_limit_sources = sources.clone();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `server.getConfig → providers[].driver` for an instanceId, when the
    /// config stream has delivered providers.
    fn provider_driver(&self, instance_id: &str) -> Option<String> {
        self.composer_state
            .config_providers
            .iter()
            .find(|p| p["instanceId"].as_str() == Some(instance_id))
            .and_then(|p| p["driver"].as_str())
            .map(str::to_string)
    }

    /// `showProviderUnavailable` (`ComposerControlsBar`): the config stream
    /// delivered providers and the selected instance has no driver → the
    /// footer swaps the model chip for a settings shortcut and the editor and
    /// send button read disabled.
    fn provider_unavailable(&self) -> bool {
        self.composer_state.config_fetched_at.is_some()
            && !self.composer_state.config_providers.is_empty()
            && self.provider_driver(&self.provider).is_none()
    }

    /// The 16px provider glyph on the model chip — web `ProviderInstanceIcon`:
    /// bundled brand marks where the registry has one, otherwise `None` and
    /// the chip falls back to the instance's initials.
    fn provider_icon(&self) -> Option<Icon> {
        let driver = self
            .provider_driver(&self.provider)
            .unwrap_or_else(|| self.provider.clone());
        match driver.as_str() {
            "codex" | "openai" | "openaiCompatible" => Some(Icon::ProviderOpenAi),
            "claudeAgent" | "anthropic" => Some(Icon::ProviderClaude),
            _ => None,
        }
    }

    /// `providerInstanceInitials` — the instance's one- or two-letter mark
    /// when no bundled brand icon exists ("Replay (dev)" → "RD").
    fn provider_initials(&self) -> String {
        let name = self
            .composer_state
            .config_providers
            .iter()
            .find(|p| p["instanceId"].as_str() == Some(self.provider.as_str()))
            .and_then(|p| p["name"].as_str().or_else(|| p["displayName"].as_str()))
            .unwrap_or(self.provider.as_str());
        let mut words = name
            .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '\u{2019}'))
            .filter(|w| {
                !w.trim_matches(|c: char| c == '\'' || c == '\u{2019}')
                    .is_empty()
            });
        match (words.next(), words.next()) {
            (Some(only), None) => only.chars().take(2).collect::<String>().to_uppercase(),
            (first, second) => [first, second]
                .into_iter()
                .flatten()
                .filter_map(|word| word.chars().next())
                .map(|c| c.to_ascii_uppercase())
                .collect(),
        }
    }

    /// The model chip's label — web `getTriggerDisplayModelName`: the server
    /// model row's `shortName`/`name`, the static catalog's display half, then
    /// the raw model id.
    fn model_display_name(&self) -> String {
        if let Some(row) = self
            .composer_state
            .config_providers
            .iter()
            .find(|p| p["instanceId"].as_str() == Some(self.provider.as_str()))
            .and_then(|p| p["models"].as_array())
            .and_then(|models| {
                models.iter().find(|m| {
                    m["slug"].as_str() == Some(self.model.as_str())
                        || m["id"].as_str() == Some(self.model.as_str())
                })
            })
        {
            if let Some(name) = row["shortName"].as_str().or_else(|| row["name"].as_str()) {
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
        MODEL_CATALOG
            .iter()
            .find(|(instance, model, _)| *instance == self.provider && *model == self.model)
            .and_then(|(_, _, label)| {
                label
                    .split('\u{00b7}')
                    .nth(1)
                    .map(str::trim)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| self.model.clone())
    }

    // ------------------------------------------------------------------
    // Share menu (guest link + hosted prompt link)
    // ------------------------------------------------------------------

    /// `guestShare.create` for the selected thread — the result URL is
    /// copied to the clipboard and kept in `share_url` so the menu offers
    /// Copy/Open afterwards.
    fn create_guest_share(&mut self, cx: &mut Context<Self>) {
        if self.composer_state.share_creating {
            return;
        }
        let Some(thread_id) = self.state.selected_thread().map(|t| t.id.to_string()) else {
            return;
        };
        let Some(rx) = self.send_query("guestShare.create", json!({ "threadId": thread_id }))
        else {
            self.push_toast(
                ToastKind::Error,
                "Guest link failed",
                Some("Not connected to a server.".to_string()),
                None,
                cx,
            );
            return;
        };
        self.composer_state.share_creating = true;
        self.composer_state.share_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.composer_state;
                s.share_creating = false;
                if exit["_tag"].as_str() == Some("Success") {
                    let url = exit["value"]["url"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    if url.is_empty() {
                        s.share_error = Some("The server returned an empty link.".to_string());
                    } else {
                        cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                        s.share_url = Some(url);
                        s.share_thread = thread_id;
                        s.share_error = None;
                        this.push_toast(
                            ToastKind::Success,
                            "Guest link copied",
                            Some(
                                "Anyone with the link can watch this thread for 24 hours."
                                    .to_string(),
                            ),
                            None,
                            cx,
                        );
                    }
                } else {
                    let message = rpc_error(&exit);
                    s.share_error = Some(message.clone());
                    this.push_toast(
                        ToastKind::Error,
                        "Guest link failed",
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

    /// Build the hosted `app.67.codes/new?prompt=…` link for the current
    /// draft (the web client's `buildNewThreadLink`, including its
    /// provider/model params when the config stream names the driver) and
    /// copy it to the clipboard.
    fn copy_prompt_link(&mut self, cx: &mut Context<Self>) {
        let prompt = self.composer_state.draft.trim().to_string();
        if prompt.is_empty() {
            self.push_toast(
                ToastKind::Info,
                "Type a prompt first",
                Some("A prompt link shares the composer draft.".to_string()),
                None,
                cx,
            );
            return;
        }
        if prompt.chars().count() > PROMPT_LINK_MAX_PROMPT_LENGTH {
            self.push_toast(
                ToastKind::Error,
                "Prompt is too long to link",
                Some(format!(
                    "Prompt links carry at most {PROMPT_LINK_MAX_PROMPT_LENGTH} characters."
                )),
                None,
                cx,
            );
            return;
        }
        let origin = std::env::var("C67_HOSTED_APP_URL")
            .ok()
            .filter(|o| !o.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_HOSTED_APP_URL.to_string());
        let mut url = format!("{origin}/new?prompt={}", url_encode(&prompt));
        // `provider` names the driver, not the instance; the web looks the
        // driver up by instanceId and drops both params when unknown.
        if let Some(driver) = self.provider_driver(&self.provider) {
            url.push_str(&format!(
                "&provider={}&model={}",
                url_encode(&driver),
                url_encode(&self.model)
            ));
        }
        cx.write_to_clipboard(ClipboardItem::new_string(url));
        self.composer_state.picker = None;
        self.push_toast(ToastKind::Success, "Prompt link copied", None, None, cx);
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
            let thumbnail = file_thumbnail(&display);
            self.composer_state.attachments.push(Attachment {
                name,
                kind: AttachmentKind::File { path: display },
                thumbnail,
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
                        let thumbnail = file_thumbnail(&display);
                        self.composer_state.attachments.push(Attachment {
                            name,
                            kind: AttachmentKind::File { path: display },
                            thumbnail,
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
                        let thumbnail = file_thumbnail(&path);
                        this.composer_state.attachments.push(Attachment {
                            name,
                            kind: AttachmentKind::File { path },
                            thumbnail,
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
    /// thread titles first, then server file hits. `kind` selects the row's
    /// leading icon (web: MessagesSquare for threads, the file-type icon for
    /// paths).
    fn mention_candidates(&self) -> Vec<(String, String, Icon)> {
        let query = self.composer_state.mention_query.to_lowercase();
        let mut out: Vec<(String, String, Icon)> = Vec::new();
        for thread in &self.state.threads {
            if out.len() >= 3 {
                break;
            }
            let title = thread.title.to_string();
            if !query.is_empty() && !title.to_lowercase().contains(&query) {
                continue;
            }
            out.push((
                format!("@{title}"),
                format!("@{title}"),
                Icon::MessageSquare,
            ));
        }
        for path in &self.composer_state.mention_files {
            out.push((path.clone(), path.clone(), Icon::File));
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
        let mut panel = drawer_panel();
        if candidates.is_empty() {
            panel = panel.child(drawer_empty("No matching files or folders."));
        } else {
            for (index, (_insert, title, kind)) in candidates.iter().enumerate() {
                panel = panel.child(
                    drawer_row(
                        ("mention", index),
                        Some(*kind),
                        title.clone(),
                        None,
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

    /// Web placeholder ladder (ChatComposer.tsx): provider-unavailable →
    /// project-required → the default call-to-action. The web also swaps in
    /// approval/question prompts; native has no pending-question composer
    /// state yet, so those variants fall through to the default.
    fn composer_placeholder(&self) -> &'static str {
        if self.provider_unavailable() {
            "Enable a provider in Settings to send a message"
        } else if self.state.selected.is_none() && self.state.projects.is_empty() {
            "Choose a project above to start a thread"
        } else {
            "Ask anything. @ files, $ skills, / commands"
        }
    }

    /// Slow background refresh driven by the composer tick: branch status
    /// (~20 s), usage summary (~5 min), the server-config stream behind the
    /// usage banner, and a stray terminal-meta stream if the ctx picker
    /// closed without its own teardown.
    fn composer_periodic(&mut self, cx: &mut Context<Self>) {
        let placeholder = self.composer_placeholder();
        self.composer.update(cx, |field, cx| {
            field.set_placeholder(placeholder, cx);
        });
        if self.state.connection != ConnectionStatus::Connected {
            return;
        }
        if !self.composer_state.config_loading
            && self
                .composer_state
                .config_fetched_at
                .is_none_or(|t| t.elapsed() > Duration::from_secs(300))
        {
            self.fetch_provider_config(cx);
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

    /// A `usageLimits` row that is actually usable: no `unavailable`
    /// marker and at least one window (`limitsNotice` in
    /// packages/shared/src/usageLimits.ts treats both as "no data").
    fn usable_limits(limits: &serde_json::Value) -> Option<&serde_json::Value> {
        if limits.is_null()
            || !limits["unavailable"].is_null()
            || limits["windows"].as_array().is_none_or(|w| w.is_empty())
        {
            return None;
        }
        Some(limits)
    }

    /// "(label, usedPercent, resetsAt)" for the fullest reported window on
    /// the selected provider — its own `usageLimits` first, then a matching
    /// `usageLimitSources` account (mirrors `collectProviderUsageLimits`).
    fn usage_limit_window(&self) -> Option<(String, f64, Option<String>)> {
        let provider = self
            .composer_state
            .config_providers
            .iter()
            .find(|p| p["instanceId"].as_str() == Some(self.provider.as_str()))?;
        let limits = Self::usable_limits(&provider["usageLimits"]).or_else(|| {
            let driver = provider["driver"].as_str().unwrap_or_default();
            self.composer_state
                .usage_limit_sources
                .iter()
                .flat_map(|source| source["accounts"].as_array().into_iter().flatten())
                .find(|account| account["driver"].as_str() == Some(driver))
                .and_then(|account| Self::usable_limits(&account["usageLimits"]))
        })?;
        let window = limits["windows"]
            .as_array()?
            .iter()
            .filter_map(|w| w["usedPercent"].as_f64().map(|pct| (w, pct)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))?;
        let label = window.0["label"].as_str().unwrap_or("usage").to_string();
        let resets = window.0["resetsAt"].as_str().map(str::to_string);
        Some((label, window.1, resets))
    }

    /// Compact banner over the composer when the wire has usage data.
    /// `subscribeServerConfig → providers[].usageLimits.windows` gives real
    /// "X% of limit" numbers; when no provider reports limits the banner
    /// falls back to `getUsageSummary` token/cost totals and warns on the
    /// same heavy-use threshold as before.
    fn render_usage_banner(&self) -> gpui::Div {
        let mut banner = div();
        let today = day_string(unix_now());
        if self.composer_state.usage_dismissed_day.as_deref() == Some(today.as_str()) {
            return banner;
        }
        let limit = self.usage_limit_window();
        let (tokens, cost, sessions) = self
            .composer_state
            .usage_summary
            .as_ref()
            .map(usage_totals)
            .unwrap_or_default();
        if limit.is_none() && tokens == 0 {
            return banner;
        }
        let heavy = limit
            .as_ref()
            .map(|(_, pct, _)| *pct >= 80.)
            .unwrap_or_else(|| tokens >= 2_000_000 || cost >= 5.0);
        let mut text = String::new();
        if let Some((label, pct, resets)) = &limit {
            text.push_str(&format!("{pct:.0}% of {label} limit"));
            // ISO timestamps arrive as `…THH:MM…Z`; show the clock time.
            if let Some(hm) = resets.as_deref().and_then(|iso| iso.get(11..16)) {
                text.push_str(&format!(" · resets {hm}"));
            }
            if tokens > 0 {
                text.push_str(" · ");
            }
        }
        if tokens > 0 {
            text.push_str(&format!(
                "{} · ${cost:.2} · {sessions} session{} today",
                compact_tokens(tokens),
                if sessions == 1 { "" } else { "s" },
            ));
        }
        // Attached `ComposerBanner` drawer — inset 1.375rem from the card's
        // edges, top corners rounded, bottom tucked under the card's edge.
        banner = banner
            .mx(px(22.))
            .flex()
            .items_center()
            .gap_2()
            .rounded_tl_2xl()
            .rounded_tr_2xl()
            .px_3()
            .py_1()
            .border_1()
            .border_b_0()
            .text_xs()
            .when(heavy, |s| {
                // warning variant: warning·28% outline + warning·8% tint.
                s.border_color(rgba(0xf59e0b47))
                    .bg(rgba(0xf59e0b14))
                    .text_color(theme::fg())
            })
            .when(!heavy, |s| {
                s.border_color(theme::composer_outline())
                    .bg(theme::composer_surface())
                    .text_color(theme::fg_muted())
            })
            .child(
                div()
                    .w(px(24.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    // Banner icon column stays muted-foreground in every
                    // variant (web `composer-banner-icon`), size-3.
                    .child(
                        icon(
                            if heavy {
                                Icon::TriangleAlert
                            } else {
                                Icon::Activity
                            },
                            12.,
                        )
                        .text_color(theme::fg_muted()),
                    ),
            )
            .child(div().flex_1().child(text))
            .child(
                div()
                    .id("usage-dismiss")
                    .w(px(24.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(theme::CONTROL_RADIUS))
                    .cursor(gpui::CursorStyle::PointingHand)
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::accent()).text_color(theme::fg()))
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.composer_state.usage_dismissed_day = Some(today.clone());
                            cx.notify();
                        });
                    })
                    .child(icon(Icon::X, 14.).text_color(theme::fg_muted())),
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
        let mut panel = drawer_panel();
        if commands.is_empty() {
            panel = panel.child(drawer_empty("No matching command."));
        } else {
            for (index, command) in commands.iter().enumerate() {
                let name = command.name;
                panel = panel.child(
                    drawer_row(
                        ("slash", index),
                        None,
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
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        icon(Icon::CornerDownLeft, 11.)
                                            .text_color(theme::fg_muted()),
                                    )
                                    .child("to create"),
                            ),
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
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(icon(Icon::Plus, 12.).text_color(theme::fg_muted()))
                                .child("New branch…"),
                        ),
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
            PickerKind::Share => {
                let s = &self.composer_state;
                let thread_id = self.state.selected_thread().map(|t| t.id.to_string());
                // Guest share — `guestShare.create` makes a 24 h link that
                // lets an unauthenticated browser watch the thread and
                // propose messages (web: GuestShareDialog).
                match &thread_id {
                    Some(tid) => {
                        let share_here = s
                            .share_url
                            .as_ref()
                            .filter(|_| s.share_thread == *tid)
                            .cloned();
                        if let Some(url) = share_here {
                            panel = panel.child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme::fg_muted())
                                    .child(url.clone()),
                            );
                            panel = panel.child(
                                menu_row(
                                    "share-copy",
                                    "Copy guest link",
                                    Some("Copied when the link was created".into()),
                                    false,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    move |_event, window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            url.clone(),
                                        ));
                                        let Some(view) = app_view(window) else {
                                            return;
                                        };
                                        view.update(cx, |this, cx| {
                                            this.push_toast(
                                                ToastKind::Success,
                                                "Guest link copied",
                                                None,
                                                None,
                                                cx,
                                            );
                                        });
                                    },
                                ),
                            );
                            let open_url = s.share_url.clone().unwrap_or_default();
                            panel = panel.child(
                                menu_row(
                                    "share-open",
                                    "Open in browser",
                                    Some("Opens the guest link".into()),
                                    false,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    move |_event, _window, cx| {
                                        cx.open_url(&open_url);
                                    },
                                ),
                            );
                            panel = panel.child(
                                menu_row(
                                    "share-new",
                                    "Create new link",
                                    Some("A new link ends the current one".into()),
                                    false,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    move |_event, window, cx| {
                                        let Some(view) = app_view(window) else {
                                            return;
                                        };
                                        view.update(cx, |this, cx| this.create_guest_share(cx));
                                    },
                                ),
                            );
                        } else {
                            panel = panel.child(
                                menu_row(
                                    "share-create",
                                    if s.share_creating {
                                        "Creating guest link…"
                                    } else {
                                        "Create guest link"
                                    },
                                    Some(
                                        "Guests watch this thread and propose messages · 24 h"
                                            .into(),
                                    ),
                                    false,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    move |_event, window, cx| {
                                        let Some(view) = app_view(window) else {
                                            return;
                                        };
                                        view.update(cx, |this, cx| this.create_guest_share(cx));
                                    },
                                ),
                            );
                        }
                        if let Some(error) = &s.share_error {
                            panel = panel.child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .text_xs()
                                    .text_color(danger())
                                    .child(error.clone()),
                            );
                        }
                    }
                    None => {
                        panel = panel.child(
                            div()
                                .px_3()
                                .py_2()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child("Select a thread to create a guest link"),
                        );
                    }
                }
                // Hosted prompt link — built client-side like the web's
                // `buildNewThreadLink`; carries the draft plus provider/model
                // when the config stream names the driver.
                panel = panel.child(div().h(px(1.)).mx_1().bg(theme::border()));
                panel = panel.child(
                    menu_row(
                        "share-prompt",
                        "Copy prompt link",
                        Some("app.67.codes link that opens a new thread with this draft".into()),
                        false,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        move |_event, window, cx| {
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| this.copy_prompt_link(cx));
                        },
                    ),
                );
            }
            PickerKind::Overflow => {
                // Web `CompactComposerControlsMenu`: the controls that don't
                // fit the resting footer live behind the ellipsis — here the
                // interaction mode, permissions, effort cycle, terminal
                // context, and share rows.
                let s = &self.composer_state;
                panel = panel.child(
                    menu_row(
                        "ov-mode",
                        "Interaction mode",
                        Some(interaction_mode_label(&s.interaction_mode).into()),
                        false,
                    )
                    .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.toggle_composer_picker(PickerKind::InteractionMode, cx)
                        });
                    }),
                );
                panel = panel.child(
                    menu_row(
                        "ov-permissions",
                        "Permissions",
                        Some(runtime_mode_label(&s.runtime_mode).into()),
                        false,
                    )
                    .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.toggle_composer_picker(PickerKind::RuntimeMode, cx)
                        });
                    }),
                );
                // Same Default → low → medium → high cycle as the model
                // picker's effort column; the row's detail shows the pick.
                panel = panel.child(
                    menu_row(
                        "ov-effort",
                        "Effort",
                        Some(effort_label(&s.effort).into()),
                        false,
                    )
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
                            this.composer_state.picker = None;
                            cx.notify();
                        });
                    }),
                );
                if self.state.selected_thread().is_some()
                    && self.state.connection == ConnectionStatus::Connected
                {
                    panel = panel.child(
                        menu_row(
                            "ov-ctx",
                            "Attach terminal context",
                            Some("Running terminals on this thread".into()),
                            false,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            |_event, window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| {
                                    this.toggle_composer_picker(PickerKind::Terminals, cx)
                                });
                            },
                        ),
                    );
                }
                if self.state.connection == ConnectionStatus::Connected {
                    panel = panel.child(
                        menu_row(
                            "ov-share",
                            "Share…",
                            Some("Guest link or hosted prompt link".into()),
                            false,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            |_event, window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| {
                                    this.toggle_composer_picker(PickerKind::Share, cx)
                                });
                            },
                        ),
                    );
                }
            }
        }
        panel
    }

    /// Small remove affordance used on staged chips — an 18px muted hit
    /// target with an X glyph that brightens on hover like the web file-row
    /// remove button.
    fn chip_remove(
        id: impl Into<gpui::ElementId>,
        index: usize,
        ctx_chip: bool,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .w(px(18.))
            .h(px(18.))
            .flex()
            .items_center()
            .justify_center()
            .flex_none()
            .rounded_sm()
            .cursor(gpui::CursorStyle::PointingHand)
            .text_color(theme::fg_muted())
            .hover(|s| s.bg(theme::accent()).text_color(theme::fg()))
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                let Some(view) = app_view(window) else {
                    return;
                };
                view.update(cx, |this, cx| {
                    if ctx_chip {
                        if index < this.composer_state.terminal_contexts.len() {
                            this.composer_state.terminal_contexts.remove(index);
                        }
                    } else if index < this.composer_state.attachments.len() {
                        this.composer_state.attachments.remove(index);
                    }
                    cx.notify();
                });
            })
            .child(icon(Icon::X, 12.).text_color(theme::fg_muted()))
    }

    fn render_attachment_strip(&self) -> impl IntoElement {
        // Web `composerAttachmentMediaRow`: mb-3, gap-2, 64px rounded-lg
        // thumbs; files render as `gap-2 py-1 text-sm` rows.
        let mut strip = div().flex().flex_wrap().gap_2().pb_3();
        for (index, attachment) in self.composer_state.attachments.iter().enumerate() {
            match (&attachment.kind, &attachment.thumbnail) {
                // Any attachment with a decoded preview — pasted images and
                // image-looking file paths alike — renders as a 64px thumb.
                (_, Some(thumbnail)) => {
                    let thumbnail = thumbnail.clone();
                    let for_overlay = thumbnail.clone();
                    let name = attachment.name.clone();
                    strip = strip.child(
                        div()
                            .id(("attach-thumb", index))
                            .w(px(64.))
                            .h(px(64.))
                            .flex_none()
                            .relative()
                            .rounded_lg()
                            .overflow_hidden()
                            .border_1()
                            .border_color(theme::border())
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
                            )
                            // Name strip pinned to the thumb's bottom edge —
                            // web `bg-background/85 text-3xs`.
                            .child(
                                div()
                                    .absolute()
                                    .bottom_0()
                                    .left_0()
                                    .right_0()
                                    .bg(rgba(0x000000d9))
                                    .px_1()
                                    .text_center()
                                    .text_size(gpui::px(10.))
                                    .text_color(theme::fg())
                                    .truncate()
                                    .child(attachment.name.clone()),
                            )
                            // Remove control — web `media-close`: black·65%
                            // rounded chip in the top corner.
                            .child(
                                div()
                                    .id(("attach-remove", index))
                                    .absolute()
                                    .top(px(4.))
                                    .right(px(4.))
                                    .w(px(18.))
                                    .h(px(18.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(4.))
                                    .bg(theme::media_close())
                                    .cursor(gpui::CursorStyle::PointingHand)
                                    .text_xs()
                                    .text_color(gpui::white())
                                    .hover(|s| s.bg(rgba(0x000000cc)))
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
                                    .child(icon(Icon::X, 12.).text_color(gpui::white())),
                            ),
                    );
                }
                (AttachmentKind::Image { .. }, None) => {
                    // Decode failed — neutral 64px block, no expand target.
                    strip = strip.child(
                        div()
                            .w(px(64.))
                            .h(px(64.))
                            .flex_none()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::muted())
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme::fg_muted())
                            .child(icon(Icon::Image, 20.).text_color(theme::fg_muted())),
                    );
                }
                _ => {
                    // Staged file — web file row: rounded-lg muted fill,
                    // border/80, text-sm name + a small remove affordance.
                    strip = strip.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .h(px(28.))
                            .pl_2()
                            .pr_1()
                            .rounded_lg()
                            .bg(theme::muted())
                            .border_1()
                            .border_color(theme::border())
                            .text_sm()
                            .text_color(theme::fg())
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .child(icon(Icon::File, 14.).text_color(theme::fg_muted())),
                            )
                            .child(div().truncate().child(attachment.name.clone()))
                            .child(Self::chip_remove(("attach-remove", index), index, false)),
                    );
                }
            }
        }
        // Staged terminal-context records — the "ctx" footer picker stages
        // them; they ship in `message.dispatch.context.records`. Chips are
        // scoped to their owning thread (records are too, on submit). Web
        // context records render as muted chips with a terminal glyph.
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
                    .gap_1p5()
                    .h(px(28.))
                    .pl_2()
                    .pr_1()
                    .rounded_lg()
                    .bg(theme::muted())
                    .border_1()
                    .border_color(theme::border())
                    .text_sm()
                    .text_color(theme::fg())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(icon(Icon::Terminal, 14.).text_color(theme::ctx_terminal())),
                    )
                    .child(div().truncate().child(ctx.label.clone()))
                    .child(Self::chip_remove(("ctx-remove", index), index, true)),
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

        // `.chat-composer-lane` — 3rem side gutters on desktop, the composer
        // overlay's `pt-1.5 sm:pt-2`, and the fixed 20px safe-area spacer
        // (`h-[calc(env(safe-area-inset-bottom)+1.25rem)]`) under the card.
        let root = div()
            .w_full()
            .px(px(48.))
            .pt_2()
            .pb(px(20.))
            .flex()
            .flex_col()
            .key_context("Composer")
            // External file drops stage as File attachments; gpui translates
            // the platform drop into a MouseUp carrying `ExternalPaths`.
            .can_drop(|value, _window, _cx| value.is::<ExternalPaths>())
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

        // `.chat-content-lane` — the card and its menus center on
        // `--chat-content-max-width` (46rem). Absolute menus anchor here so
        // they track the card, not the padded gutter.
        let mut lane = div()
            .mx_auto()
            .w_full()
            .max_w(px(theme::CHAT_MAX_W))
            .flex()
            .flex_col()
            .relative();

        // Floating menus above the composer.
        if state.mention_open {
            lane = lane.child(self.render_mention_menu());
        } else if state.slash_open {
            lane = lane.child(self.render_slash_menu());
        }
        if let Some(kind) = state.picker {
            lane = lane.child(self.render_picker(kind));
        }

        // Attached `ComposerBanner` dock — the usage notice sits flush on the
        // card's top edge (its bottom corners tuck under the card outline).
        lane = lane.child(self.render_usage_banner());

        // `ComposerSurface.Main`: rounded-2xl glass card, 5%-white outline,
        // the composer drop shadow plus the inset top-edge highlight.
        // The footer lives inside the card. File drops tint the surface,
        // matching the web drop target's `bg-accent/45` ring.
        let mut card = div()
            .flex()
            .flex_col()
            .rounded_2xl()
            .bg(theme::composer_surface())
            .border_1()
            .border_color(theme::composer_outline())
            .shadow(vec![composer_highlight_shadow(), composer_shadow()])
            .drag_over::<ExternalPaths>(|style, _paths, _window, _cx| style.bg(theme::accent()));

        // `data-chat-composer-body`: px-4 pt-4 pb-2 around the prompt editor.
        let mut body = div().flex().flex_col().px_4().pt_4().pb_2();

        // The periodic tick only refreshes the placeholder after the first
        // composer interaction, so the field's constructor string would
        // linger. A zero-size canvas writes the current ladder rung in during
        // every prepaint instead.
        {
            let placeholder = self.composer_placeholder();
            let field = composer.clone();
            body = body.child(
                gpui::canvas(
                    move |_bounds, _window, cx| {
                        field.update(cx, |field, cx| field.set_placeholder(placeholder, cx));
                    },
                    |_bounds, _state, _window, _cx| {},
                )
                .w_0()
                .h_0(),
            );
        }

        if !state.attachments.is_empty() || self.staged_ctx_count() > 0 {
            body = body.child(self.render_attachment_strip());
        }
        if let Some(field) = &state.attach_field {
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pb_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Path:")
                    .child(div().flex_1().text_color(theme::fg()).child(field.clone()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(icon(Icon::CornerDownLeft, 11.).text_color(theme::fg_muted()))
                            .child("to attach · esc to cancel"),
                    ),
            );
        }

        // `composer-tiptap`: text-sm foreground, leading-relaxed (22.75px at
        // 14px), min-h of two lines + p-1, max-h-52 scroll. The field's own
        // hitbox is one line tall; clicks in the wrapper's dead space below
        // forward focus like a real textarea.
        body = body.child(
            div()
                .min_h(px(54.))
                .max_h(px(208.))
                .text_sm()
                .line_height(px(22.75))
                .text_color(theme::fg())
                .on_mouse_down(MouseButton::Left, {
                    let composer = composer.clone();
                    move |_event, window, cx| {
                        composer.read(cx).focus_handle(cx).focus(window, cx);
                    }
                })
                .child(composer.clone()),
        );

        card = card.child(body);

        // `data-chat-composer-footer`: px-2 pb-2; attach button on the left,
        // the control chips right-aligned before the primary actions.
        let provider_unavailable = self.provider_unavailable();
        let mut card_footer = div().flex().items_center().px_2().pb_2().child(
            div().mr_2().child(
                ghost_icon_button("composer-attach")
                    .hover(|s| s.bg(theme::accent()).text_color(theme::fg()))
                    .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                        ensure_composer_hooks(window, cx);
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.toggle_composer_picker(PickerKind::Attach, cx);
                        });
                    })
                    .child(icon(Icon::Paperclip, 16.).text_color(theme::fg_muted())),
            ),
        );

        // `data-chat-composer-actions=right` — `flex-1 justify-end gap-2`:
        // the footer controls (`gap-0.5`) then the mic/send cluster.
        let mut right = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .justify_end()
            .gap_2();

        // `data-chat-composer-controls=footer` — `flex-1 justify-end gap-0.5`.
        let mut controls = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .justify_end()
            .gap_0p5();

        if provider_unavailable {
            // `showProviderUnavailable` — a ComposerControl with the alert
            // glyph; when a setup target exists it reads "Open provider
            // settings" and jumps to Settings → Providers.
            controls = controls.child(
                control_chip("composer-provider-settings")
                    .text_color(theme::control_fg())
                    .hover(|s| s.bg(theme::accent()).text_color(theme::fg()))
                    .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.settings_state.open = true;
                            this.settings_state.section = crate::settings::Section::Providers;
                            cx.notify();
                        });
                    })
                    .child(icon(Icon::CircleAlert, 16.).text_color(theme::fg_muted()))
                    .child("Open provider settings"),
            );
        } else {
            // `ComposerModelPicker` trigger — ComposerControl chrome (h-7,
            // px-2, gap-1.5): a 16px provider glyph (brand mark or instance
            // initials), the model's display name, and the muted effort
            // suffix when it isn't the provider default. No caret.
            let model_open = state.picker == Some(PickerKind::Model);
            let mut model_chip = control_chip("composer-model")
                .when(model_open, |s| {
                    s.bg(theme::accent()).text_color(theme::accent_fg())
                })
                .when(!model_open, |s| {
                    s.text_color(theme::control_fg())
                        .hover(|s| s.bg(theme::accent()).text_color(theme::fg()))
                })
                .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                    ensure_composer_hooks(window, cx);
                    let Some(view) = app_view(window) else {
                        return;
                    };
                    view.update(cx, |this, cx| {
                        this.toggle_composer_picker(PickerKind::Model, cx)
                    });
                });
            if let Some(glyph) = self.provider_icon() {
                let color = match glyph {
                    // Claude's mark keeps its brand orange (web `fill-[#d97757]`);
                    // the OpenAI knot is white/black like web's text token.
                    Icon::ProviderClaude => rgb(0xd97757),
                    _ => theme::fg(),
                };
                model_chip = model_chip.child(icon(glyph, 16.).text_color(color));
            } else {
                let initials = self.provider_initials();
                if !initials.is_empty() {
                    model_chip = model_chip.child(
                        div()
                            .text_size(theme::text_size::XS_3)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::fg_muted())
                            .child(initials),
                    );
                }
            }
            model_chip = model_chip.child(self.model_display_name());
            if !state.effort.is_empty() {
                model_chip = model_chip.child(
                    div()
                        .text_color(theme::fg_muted())
                        .child(effort_label(&state.effort)),
                );
            }
            controls = controls.child(model_chip);

            // `ComposerFooterModeControls` — the permissions chip only rests
            // when the mode differs from the default (native has no
            // `defaultRuntimeMode` setting yet; "full-access" seeds it).
            if state.runtime_mode != "full-access" {
                let mode_glyph = match state.runtime_mode.as_str() {
                    "approval-required" => Icon::Lock,
                    "auto-accept-edits" => Icon::PenLine,
                    "auto" => Icon::ShieldCheck,
                    _ => Icon::ShieldOff,
                };
                let permissions_open = state.picker == Some(PickerKind::RuntimeMode);
                controls = controls.child(
                    control_chip("composer-permissions")
                        .when(permissions_open, |s| {
                            s.bg(theme::accent()).text_color(theme::accent_fg())
                        })
                        .when(!permissions_open, |s| {
                            s.text_color(theme::control_fg())
                                .hover(|s| s.bg(theme::accent()).text_color(theme::fg()))
                        })
                        .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                            ensure_composer_hooks(window, cx);
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| {
                                this.toggle_composer_picker(PickerKind::RuntimeMode, cx)
                            });
                        })
                        .child(icon(mode_glyph, 16.).text_color(theme::fg_muted()))
                        .child(runtime_mode_label(&state.runtime_mode)),
                );
            }

            // `CompactComposerControlsMenu` — the icon-only ellipsis control
            // behind which the controls with no resting slot live.
            let overflow_open = state.picker == Some(PickerKind::Overflow);
            controls = controls.child(
                ghost_icon_button("composer-overflow")
                    .when(overflow_open, |s| {
                        s.bg(theme::accent()).text_color(theme::accent_fg())
                    })
                    .when(!overflow_open, |s| {
                        s.hover(|s| s.bg(theme::accent()).text_color(theme::fg()))
                    })
                    .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                        ensure_composer_hooks(window, cx);
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.toggle_composer_picker(PickerKind::Overflow, cx)
                        });
                    })
                    .child(icon(Icon::Ellipsis, 16.).text_color(theme::fg_muted())),
            );
        }
        right = right.child(controls);

        // Primary actions — web `ComposerFooterPrimaryActions`: gap-2 between
        // the controls and the mic/send cluster.
        let mut actions = div().flex().items_center().gap_2();

        // Dictation toggle — `ComposerSpeechButton` is a 28px ghost icon;
        // while recording it pulses destructive, matching the recording pill.
        {
            let recording = state.dictation.is_some();
            let transcribing = state.dictation_transcribing;
            let pulse = state.dictation_pulse % 2 == 0;
            actions = actions.child(if recording {
                let elapsed = state
                    .dictation
                    .as_ref()
                    .map(|d| d.started.elapsed().as_secs())
                    .unwrap_or(0);
                control_chip("composer-dictation")
                    .text_color(danger())
                    .bg(if pulse {
                        rgba(0xff5a5a14)
                    } else {
                        rgba(0xff5a5a2e)
                    })
                    .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                        ensure_composer_hooks(window, cx);
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| this.toggle_dictation(window, cx));
                    })
                    .child(icon(Icon::CircleDot, 12.).text_color(danger()))
                    .child(format!("{}:{:02}", elapsed / 60, elapsed % 60))
            } else {
                ghost_icon_button("composer-dictation")
                    .hover(|s| s.bg(theme::accent()).text_color(theme::fg()))
                    .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                        ensure_composer_hooks(window, cx);
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| this.toggle_dictation(window, cx));
                    })
                    .child(
                        icon(
                            if transcribing {
                                Icon::LoaderCircle
                            } else {
                                Icon::Mic
                            },
                            16.,
                        )
                        .text_color(theme::fg_muted()),
                    )
            });
        }

        // Busy-thread follow-up pill → `dispatch_override` (Queue default),
        // the `aria-pressed` accent control beside the primary button.
        if running {
            let run_label = match state.dispatch_override {
                Some("steer_active") => "Steer",
                Some("restart_active") => "Restart",
                _ => "Queue",
            };
            actions = actions.child(
                control_chip("composer-runopt")
                    .bg(theme::accent_soft())
                    .text_color(theme::fg())
                    .hover(|s| s.bg(theme::accent()))
                    .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                        ensure_composer_hooks(window, cx);
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.toggle_composer_picker(PickerKind::RunOptions, cx);
                        });
                    })
                    .child(run_label)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(icon(Icon::ChevronDown, 12.).text_color(theme::fg_muted())),
                    ),
            );
        }

        // `message-action` primary button: 32px circle, primary fill, white
        // glyph; disabled state keeps the blue circle dimmed (the reference's
        // `disabled:opacity-64` stage-backdrop button), not the fg·12% fill.
        let sendable =
            (!state.draft.is_empty() || !state.attachments.is_empty()) && !provider_unavailable;
        actions = actions.child(if running {
            div()
                .id("stop")
                .w(px(32.))
                .h(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme::destructive_90())
                .shadow(vec![control_highlight_shadow()])
                .cursor(gpui::CursorStyle::PointingHand)
                .text_sm()
                .text_color(gpui::white())
                .hover(|s| s.bg(theme::error()))
                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                    let Some(view) = app_view(window) else {
                        return;
                    };
                    view.update(cx, |this, cx| this.stop_thread(cx));
                })
                .child(icon(Icon::Square, 12.).text_color(gpui::white()))
        } else {
            div()
                .id("send")
                .w(px(32.))
                .h(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .shadow(vec![control_highlight_shadow()])
                .cursor(gpui::CursorStyle::PointingHand)
                .text_sm()
                .when(sendable, |s| {
                    s.bg(theme::primary())
                        .text_color(gpui::white())
                        .hover(|s| s.bg(theme::primary_hover()))
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
                })
                .when(!sendable, |s| {
                    s.bg(theme::primary_dim())
                        .text_color(theme::primary_fg_dim())
                        .cursor(gpui::CursorStyle::Arrow)
                })
                .child(icon(Icon::ArrowUp, 14.).text_color(if sendable {
                    gpui::white()
                } else {
                    theme::primary_fg_dim().into()
                }))
        });

        right = right.child(actions);
        card_footer = card_footer.child(right);
        card = card.child(card_footer);

        lane = lane.child(card);

        // `ComposerSurface.ContextStrip`: a bare px-2 pt-2 row under the card
        // — the branch selector (right-aligned), the draft-stash affordance,
        // and the muted hints; xs controls, no surface of its own.
        let show_branch = self.composer_project().is_some()
            && state
                .branch_status
                .as_ref()
                .and_then(|v| v["isRepo"].as_bool())
                == Some(true);
        let can_stash = !state.draft.is_empty() || !state.attachments.is_empty();
        let no_thread = self.state.selected.is_none();
        if show_branch || can_stash || stashed > 0 || running || no_thread {
            let mut strip = div()
                .flex()
                .w_full()
                .items_center()
                .gap_2()
                .px_2()
                .pt_2()
                .text_xs()
                .text_color(theme::fg_muted());

            if running {
                strip = strip.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_color(theme::fg_muted())
                        .child(icon(Icon::CornerDownLeft, 11.).text_color(theme::fg_muted()))
                        .child(match state.dispatch_override {
                            Some("steer_active") => "steers the running turn",
                            Some("restart_active") => "restarts the run",
                            _ => "queues behind the current run",
                        }),
                );
            } else if no_thread {
                strip = strip.child(
                    div()
                        .text_color(theme::fg_muted())
                        .child("No thread selected — this will start a new thread"),
                );
            }

            // Right-aligned cluster, web `@xl/composer-surface:ml-auto`.
            strip = strip.child(div().flex_1());

            // Draft stash affordance: "Stash" while there's content,
            // `ComposerStashBadge` (bookmark + count) once entries wait.
            if can_stash {
                strip = strip.child(
                    context_chip("composer-stash")
                        .hover(|s| s.bg(theme::accent()).text_color(theme::fg()))
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
                strip = strip.child(
                    context_chip("composer-stashed")
                        .hover(|s| s.bg(theme::accent()).text_color(theme::fg()))
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

            // Branch chip — selected thread's project; opens the ref picker.
            if show_branch {
                let branch_label = state
                    .branch
                    .clone()
                    .filter(|b| !b.is_empty())
                    .unwrap_or_else(|| "(detached)".to_string());
                let open = state.picker == Some(PickerKind::Branch);
                strip = strip.child(
                    context_chip("composer-branch")
                        .when(open, |s| {
                            s.bg(theme::accent()).text_color(theme::accent_fg())
                        })
                        .when(!open, |s| {
                            s.hover(|s| s.bg(theme::accent()).text_color(theme::fg()))
                        })
                        .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                            ensure_composer_hooks(window, cx);
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| {
                                this.toggle_composer_picker(PickerKind::Branch, cx);
                            });
                        })
                        // Web branch chip: `GitBranchIcon` size-3 muted + the
                        // branch name, no caret.
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .child(icon(Icon::GitBranch, 12.).text_color(theme::fg_muted())),
                        )
                        .child(branch_label),
                );
            }

            lane = lane.child(strip);
        }

        root.child(lane)
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
