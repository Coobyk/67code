//! Message timeline: turn item rendering — tool calls, diffs, approvals,
//! user/assistant styling. Owns `impl AppView` timeline render methods and
//! `timeline::State`.
//!
//! Field names come from `OrchestrationV2TurnItem` in
//! `packages/contracts/src/orchestrationV2.ts` (the complete payload arrives
//! on `TurnItemView::raw`); `item.status` is one of `OrchestrationV2TurnItemStatus`
//! (idle, pending, running, waiting, completed, failed, cancelled, interrupted).

use std::collections::{HashMap, HashSet};

use gpui::{
    AnyElement, ClipboardItem, Entity, EntityInputHandler, Focusable, Rgba, Subscription, div,
    prelude::*, px, rgb, rgba,
};
use serde_json::{Value, json};

use crate::model::{ConnectionStatus, Role};
use crate::net::TurnItemView;
use crate::notifications::ToastKind;
use crate::text_field::{PromptSubmitted, TextField};
use crate::{AppView, theme};

/// Expanded tool/command output is capped so a multi-megabyte result can't
/// flood the timeline's text layout.
const OUTPUT_CAP_CHARS: usize = 4096;

/// Timeline-local UI state. Expand freely — this field lives on `AppView`.
#[derive(Default)]
pub struct State {
    /// Turn item ids whose expandable detail (tool output, diff) is open.
    pub expanded: HashSet<String>,
    /// Approval `requestId`s already answered from this client; keeps the
    /// buttons disabled until the server echoes the resolved status back.
    pub responded: HashSet<String>,
    /// Item id whose Copy chip last wrote to the clipboard ("Copied" label).
    pub copied: Option<String>,
    /// Option selections for pending user-input cards, before Submit:
    /// requestId -> questionId -> selected option values in click order.
    pub user_input_answers: HashMap<String, HashMap<String, Vec<String>>>,
    /// Assistant items whose 👍/👎 upload was confirmed by the server.
    pub feedback_sent: HashSet<String>,
    /// Assistant items with a `provider.uploadFeedback` call in flight.
    pub feedback_in_flight: HashSet<String>,
    /// While the thumbs-down comment input is open: (item id, thread id,
    /// field). Created on first use — `cx.new` needs a context.
    pub feedback_comment: Option<(String, String, Entity<TextField>)>,
    /// Keeps the feedback field's `PromptSubmitted` (Enter) subscription.
    pub feedback_sub: Option<Subscription>,
}

fn success() -> Rgba {
    rgb(0x4cc38a)
}

fn danger() -> Rgba {
    rgb(0xff5a5a)
}

fn danger_border() -> Rgba {
    rgba(0xff5a5a55)
}

fn warning() -> Rgba {
    rgb(0xe8b45a)
}

fn warning_border() -> Rgba {
    rgba(0xe8b45a55)
}

/// The item is still producing content (streaming or actively running).
/// `waiting` means it is blocked on the user, not that it is working.
fn is_live(item: &TurnItemView) -> bool {
    item.streaming || matches!(item.status.as_str(), "pending" | "running")
}

fn raw_str<'a>(item: &'a TurnItemView, key: &str) -> Option<&'a str> {
    item.raw
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

// ── Work-log rows (apps/web WorkLog.tsx) ─────────────────────────
//
// Tool-ish turn items render as flat `WorkLogRow` lines — a 24-px icon
// slot, a truncated `text-secondary-label` 14-px label, and a trailing
// hover-revealed timestamp plus disclosure chevron. No card chrome.

/// Kinds that take the flat work-log treatment rather than a card.
fn is_work_kind(kind: &str) -> bool {
    matches!(
        kind,
        "reasoning"
            | "command_execution"
            | "dynamic_tool"
            | "file_change"
            | "subagent"
            | "web_search"
            | "file_search"
            | "todo_list"
            | "approval_request"
            | "user_input_request"
            | "system_notice"
            | "notification"
    )
}

/// DejaVu-covered stand-ins for the lucide `WorkEntryIconName` set the web
/// maps each entry kind to (`toolIconFor` in TimelineWorkEntryRow.logic.ts).
fn work_glyph(kind: &str) -> &'static str {
    match kind {
        // terminal
        "command_execution" => "❯",
        // square-pen
        "file_change" => "✎",
        // brain
        "reasoning" => "☁",
        // bot
        "subagent" => "⌘",
        // globe
        "web_search" => "⊚",
        // search
        "file_search" => "◍",
        // list-todo
        "todo_list" => "☑",
        // message-circle / shield
        "approval_request" | "user_input_request" => "?",
        // info
        "system_notice" | "notification" => "ℹ",
        // hammer / wrench
        _ => "⚒",
    }
}

/// `approval_request`/`user_input_request` icon per `requestKind` — mirrors
/// `APPROVAL_PRESENTATION` in TimelinePendingApprovalRow.tsx.
fn approval_glyph(kind: &str) -> &'static str {
    match kind {
        "command" => "❯",
        "file-change" => "✎",
        "file-read" => "◉",
        "permission" | "mcp-elicitation" => "⚒",
        _ => "?",
    }
}

/// The `w-6` icon slot: a `size-4` (16-px) glyph centered in 24 px.
/// `text-icon-muted` normally; failed calls tint it `text-tool-error-icon/40`
/// like `failedToolIconClassName` does for tintable icons.
fn work_icon(glyph: &'static str, failed: bool) -> gpui::Div {
    div()
        .w(px(24.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(15.))
        .text_color(if failed {
            theme::tool_error_icon()
        } else {
            theme::icon_muted()
        })
        .child(glyph)
}

/// Trailing `size-3` ChevronRight at `text-icon-muted opacity-70`, rotated
/// 90° while expanded — rendered as ▸/▾ since there is no icon font.
fn work_chevron(expanded: bool) -> gpui::Div {
    div()
        .w(px(16.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .opacity(0.7)
        .text_color(theme::icon_muted())
        .child(if expanded { "▾" } else { "▸" })
}

/// Days since the Unix epoch for a civil `Y-M-D` (Howard Hinnant's
/// days_from_civil) — lets timestamps pick today/yesterday/older like the
/// web's `formatDayAwareTimestamp`.
fn civil_days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * if m > 2 { m - 3 } else { m + 9 } + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `TimelineRowTimestamp` — `text-xs text-muted-foreground tabular-nums`,
/// `opacity-0` until the `.group("timeline-row")` ancestor is hovered.
/// Rendered UTC (the native build has no timezone database).
fn item_timestamp(item: &TurnItemView) -> Option<gpui::Div> {
    let iso = raw_str(item, "updatedAt")?;
    let (date, time) = iso.split_once('T')?;
    let (y, m, d) = (
        date.get(..4)?.parse::<i64>().ok()?,
        date.get(5..7)?.parse::<i64>().ok()?,
        date.get(8..10)?.parse::<i64>().ok()?,
    );
    let (mut h, min) = (time.get(..2)?.parse::<u32>().ok()?, time.get(3..5)?);
    // Locale default ≈ 12-hour for en-US.
    let ampm = if h < 12 { "AM" } else { "PM" };
    h %= 12;
    if h == 0 {
        h = 12;
    }
    let hhmm = format!("{h}:{min} {ampm}");
    let days = civil_days(y, m, d);
    let today = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs()
        / 86_400) as i64;
    let label = match today - days {
        diff if diff <= 0 => hhmm,
        1 => format!("yesterday at {hhmm}"),
        _ => format!("{m}/{d}/{y} {hhmm}"),
    };
    // Both ancestors gate the reveal: "timeline-row" on work rows,
    // "msg" on the under-bubble meta rows of user/assistant messages.
    Some(
        div()
            .flex_shrink_0()
            .text_xs()
            .text_color(theme::fg_muted())
            .opacity(0.)
            .group_hover("timeline-row", |s| s.opacity(1.))
            .group_hover("msg", |s| s.opacity(1.))
            .child(label),
    )
}

/// Row shell: `group/timeline-row … rounded-md px-0.5 py-0(.5)` over a
/// `WorkLogLine` (`min-h-6 gap-1.5 text-sm`). Clickable rows toggle.
fn work_row_line() -> gpui::Div {
    div()
        .flex()
        .items_center()
        .min_h(px(24.))
        .w_full()
        .min_w_0()
        .gap(px(6.))
        .px(px(2.))
        .py(px(2.))
        .text_sm()
}

/// `WorkLogDetails kind="panel"`: the expanded body under a work row —
/// `mt-0.5 mb-1.5 pb-2`, a mono `pre` at `--font-size-code` (13px) in
/// `--secondary-label`, `leading-relaxed`, `whitespace-pre-wrap`, capped at
/// `max-h-64` (256px).
fn detail_panel(namespace: &'static str, index: usize) -> gpui::Stateful<gpui::Div> {
    div()
        .id((namespace, index))
        .w_full()
        .mt(px(2.))
        .mb(px(6.))
        .pb_2()
        .overflow_y_scroll()
        .max_h(px(256.))
        .font_family(theme::FONT_MONO)
        .text_size(theme::code_text_size())
        .line_height(px(21.))
        .text_color(theme::secondary_label())
}

/// `WorkLogDetails kind="text"`: indented (`ms-7` = 28px) prose detail,
/// `px-0.5 py-1`, capped at `max-h-96` (384px).
fn detail_text(namespace: &'static str, index: usize) -> gpui::Stateful<gpui::Div> {
    div()
        .id((namespace, index))
        .w_full()
        .ml(px(28.))
        .px(px(2.))
        .py_1()
        .overflow_y_scroll()
        .max_h(px(384.))
        .text_sm()
        .line_height(px(22.75))
        .text_color(theme::secondary_label())
}

/// One-line `WorkLogRow` header: icon slot + truncated secondary-label +
/// trailing slots. `index` seeds the toggle target's element id.
fn work_header(
    index: usize,
    item: &TurnItemView,
    glyph: &'static str,
    label: gpui::Div,
) -> gpui::Stateful<gpui::Div> {
    let failed = item.status == "failed";
    work_row_line()
        .id(("head", index))
        .group("timeline-row")
        .cursor_pointer()
        .on_click(toggle_expanded(item.id.clone()))
        .child(work_icon(glyph, failed))
        .child(label.flex_1().min_w_0().truncate())
}

/// `render_timeline` takes `&self`, so click handlers reach the view through
/// the window root instead of `cx.listener`.
fn update_view(
    window: &mut gpui::Window,
    cx: &mut gpui::App,
    f: impl FnOnce(&mut AppView, &mut gpui::Context<AppView>),
) {
    if let Some(Some(view)) = window.root::<AppView>() {
        view.update(cx, f);
    }
}

fn toggle_expanded(
    item_id: String,
) -> impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) {
    move |_event, window, cx| {
        let item_id = item_id.clone();
        update_view(window, cx, move |this, cx| {
            if !this.timeline_state.expanded.remove(&item_id) {
                this.timeline_state.expanded.insert(item_id);
            }
            cx.notify();
        });
    }
}

/// Pretty-print a JSON detail value; strings pass through verbatim.
fn pretty_json(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        other => serde_json::to_string_pretty(other).ok(),
    }
}

/// Some providers wrap command output as `{"stdout","stderr","interrupted"}`;
/// join the streams like the web client does.
fn command_output_text(output: &str) -> String {
    let trimmed = output.trim_start();
    if !trimmed.starts_with("{\"stdout\"") {
        return output.to_string();
    }
    let Ok(parsed) = serde_json::from_str::<Value>(trimmed) else {
        return output.to_string();
    };
    [parsed["stdout"].as_str(), parsed["stderr"].as_str()]
        .into_iter()
        .flatten()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Tool `output` is either a plain string or a provider content-block array
/// (`[{type: "content", content: {type: "text", text: "…"}}]`).
fn tool_output_text(output: &Value) -> Option<String> {
    match output {
        Value::Array(parts) => {
            let texts: Vec<&str> = parts
                .iter()
                .filter_map(|part| {
                    part["content"]["text"]
                        .as_str()
                        .or_else(|| part["text"].as_str())
                })
                .collect();
            if texts.is_empty() {
                pretty_json(output)
            } else {
                Some(texts.join("\n\n"))
            }
        }
        other => pretty_json(other),
    }
}

/// Native codex tool items carry the MCP/exec result object under `result`:
/// `{content: [{type:"text",text}…]}` for tools, `{output, exitCode}` for
/// command executions. `output` stays the contract spelling.
fn tool_result_text(result: &Value) -> Option<String> {
    match result {
        Value::Null => None,
        Value::Object(map) => match map.get("content") {
            Some(content) => tool_output_text(content),
            None => pretty_json(result),
        },
        other => tool_output_text(other),
    }
}

/// First `OUTPUT_CAP_CHARS` of a detail body plus a truncation notice.
fn cap_detail(text: String) -> String {
    if text.chars().count() <= OUTPUT_CAP_CHARS {
        return text;
    }
    let kept: String = text.chars().take(OUTPUT_CAP_CHARS).collect();
    format!(
        "{kept}\n… output truncated ({} more bytes)",
        text.len() - kept.len()
    )
}

/// Ghost icon button — the web's `MessageCopyButton variant="ghost"` / thumbs
/// glyphs: a `size-6` muted target revealed on row hover (callers may set a
/// starting opacity for "already sent" states).
fn ghost_icon_chip(
    id: impl Into<gpui::ElementId>,
    glyph: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(24.))
        .h(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .text_sm()
        .cursor_pointer()
        .text_color(theme::fg_muted())
        .opacity(0.)
        .group_hover("msg", |s| s.opacity(1.))
        .group_hover("timeline-row", |s| s.opacity(1.))
        .hover(|s| s.text_color(theme::fg()))
        .child(glyph)
}

/// "Copy" ghost chip revealed when its `.group("msg")` ancestor is hovered;
/// writes `text` to the clipboard and flips to a check.
fn copy_chip(
    key: &'static str,
    index: usize,
    copied: bool,
    item_id: String,
    text: String,
) -> gpui::Stateful<gpui::Div> {
    ghost_icon_chip((key, index), if copied { "✓" } else { "❐" }).on_click(
        move |_event, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            let item_id = item_id.clone();
            update_view(window, cx, move |this, cx| {
                this.timeline_state.copied = Some(item_id);
                cx.notify();
            });
        },
    )
}

// ---------------------------------------------------------------------------
// Assistant citations (user-authored quote annotations)
// ---------------------------------------------------------------------------
//
// There is no provider-emitted citation turn-item kind: the
// `OrchestrationV2TurnItem` union has no citation member. The only citations
// on the wire are user-authored `AssistantCitation`s, embedded in message
// text as `[Assistant quote](t3-citation://v1/<env>/<thread>/<msg>?…)` links
// (`packages/shared/src/assistantCitations.ts`). ChatMarkdown renders them as
// `AssistantCitationChip`s — a quote chip that navigates to the source
// thread. Below is the same treatment.

/// A parsed `t3-citation:` link — only the fields the chip needs.
struct AssistantCitation {
    thread_id: String,
    text: String,
    comment: Option<String>,
}

const CITATION_PREFIX: &str = "t3-citation://v1/";
const CITATION_LINK_LABEL: &str = "[Assistant quote](";

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `application/x-www-form-urlencoded` decode: `%XX` bytes, and `+` → space
/// when `plus` (JS `URLSearchParams` encodes query spaces as `+` while
/// `encodeURIComponent` path segments use `%20`).
fn pct_decode(s: &str, plus: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => match (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                }
                _ => {
                    out.push(b[i]);
                    i += 1;
                }
            },
            b'+' if plus => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Mirrors `parseAssistantCitationHref`: three path segments, exactly the
/// five required query keys plus optional `comment`, numeric start/end.
fn parse_citation_href(href: &str) -> Option<AssistantCitation> {
    let rest = href.strip_prefix(CITATION_PREFIX)?;
    let (path, query) = rest.split_once('?')?;
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let mut seen: HashMap<String, String> = HashMap::new();
    for kv in query.split('&') {
        let (k, v) = kv.split_once('=')?;
        if seen.insert(k.to_string(), pct_decode(v, true)).is_some() {
            return None;
        }
    }
    for k in ["text", "start", "end", "prefix", "suffix"] {
        if !seen.contains_key(k) {
            return None;
        }
    }
    if seen.keys().any(|k| {
        !matches!(
            k.as_str(),
            "text" | "start" | "end" | "prefix" | "suffix" | "comment"
        )
    }) {
        return None;
    }
    if !(seen["start"].chars().all(|c| c.is_ascii_digit())
        && seen["end"].chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    Some(AssistantCitation {
        thread_id: pct_decode(parts[1], false),
        text: seen.remove("text").unwrap_or_default(),
        comment: seen.remove("comment"),
    })
}

/// Pull `[Assistant quote](t3-citation://…)` links out of message text.
/// Returns the text without the links plus the parsed citations — unparseable
/// links degrade to their label like the web's anchor fallback.
fn extract_assistant_citations(text: &str) -> (String, Vec<AssistantCitation>) {
    let mut cleaned = String::with_capacity(text.len());
    let mut citations = Vec::new();
    let mut rest = text;
    while let Some(pos) = rest.find(CITATION_LINK_LABEL) {
        cleaned.push_str(&rest[..pos]);
        let href_start = pos + CITATION_LINK_LABEL.len();
        match rest[href_start..].find(')') {
            Some(len) => {
                let href = &rest[href_start..href_start + len];
                match parse_citation_href(href) {
                    Some(c) => citations.push(c),
                    None => cleaned.push_str("[Assistant quote]"),
                }
                rest = &rest[href_start + len + 1..];
            }
            None => {
                cleaned.push_str(&rest[pos..]);
                rest = "";
                break;
            }
        }
    }
    cleaned.push_str(rest);
    (cleaned.trim().to_string(), citations)
}

/// `assistantCitationLabel`: comment (trimmed) or quoted text, whitespace
/// squashed, capped at 64 chars.
fn citation_label(c: &AssistantCitation) -> String {
    let base = c
        .comment
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&c.text);
    let flat = base.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 64 {
        flat
    } else {
        format!("{}…", flat.chars().take(64).collect::<String>())
    }
}

/// Compact quote chip replacing a `t3-citation:` link. Clicking navigates to
/// the cited thread — the web routes to the source thread and reveals the
/// quoted range; the native timeline can only land on the thread.
fn citation_chip(
    index: usize,
    ci: usize,
    citation: &AssistantCitation,
) -> gpui::Stateful<gpui::Div> {
    let label = citation_label(citation);
    let quote = citation
        .comment
        .as_ref()
        .map(|_| {
            citation
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|q| !q.is_empty());
    let thread_id = citation.thread_id.clone();
    let mut chip = div()
        .id(("cite", index * 64 + ci))
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::canvas())
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover()))
        .on_click(move |_event, window, cx| {
            let thread_id = thread_id.clone();
            update_view(window, cx, move |this, cx| {
                this.select_thread(thread_id.into(), cx);
            });
        })
        .child(
            div()
                .text_xs()
                .text_color(theme::fg_muted())
                .flex_shrink_0()
                .child("❝"),
        )
        .child(
            div()
                .text_xs()
                .truncate()
                .max_w(px(320.))
                .text_color(theme::fg())
                .child(label),
        );
    if let Some(quote) = quote {
        chip = chip.child(
            div()
                .text_xs()
                .truncate()
                .max_w(px(220.))
                .text_color(theme::fg_muted())
                .child(format!("· “{quote}”")),
        );
    }
    chip
}

/// Run `f` on a TextField from a context that carries no `&mut Window` —
/// same `with_window`/windows fallback as envs.rs `edit_field`.
fn edit_text_field<R>(
    field: &Entity<TextField>,
    cx: &mut gpui::Context<AppView>,
    f: impl Fn(&mut TextField, &mut gpui::Window, &mut gpui::Context<TextField>) -> R,
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

/// Focus a lazily-created TextField from a `&mut Context<AppView>` that has
/// no `&mut Window` (the envs.rs `focus_field` pattern).
fn focus_text_field(field: &Entity<TextField>, cx: &mut gpui::Context<AppView>) {
    let focus = field.read(cx).focus_handle(cx);
    if cx
        .with_window(field.entity_id(), |window, app| focus.focus(window, app))
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

/// Readable message out of a Failure `Exit` frame — same shape as
/// composer.rs `rpc_error` (module-private there, so kept local).
fn rpc_error_text(exit: &Value) -> String {
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

/// One selectable option row inside a pending user-input card. Clicking
/// toggles the value in `timeline_state.user_input_answers` — multi-select
/// questions accumulate, single-select replaces (matching the web's
/// `togglePendingUserInputOptionSelection`).
fn user_input_option(
    item_index: usize,
    question_index: usize,
    option_index: usize,
    request_id: &str,
    question_id: &str,
    value: &str,
    label: &str,
    description: &str,
    multi_select: bool,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    let request_id = request_id.to_string();
    let question_id = question_id.to_string();
    let value = value.to_string();
    let mark = match (multi_select, selected) {
        (true, true) => "☑",
        (true, false) => "☐",
        (false, true) => "●",
        (false, false) => "○",
    };
    let mut row = div()
        .id((
            "ui-opt",
            item_index * 4096 + question_index * 64 + option_index,
        ))
        .flex()
        .items_start()
        .gap_2()
        .px_2()
        .py_1p5()
        .rounded_md()
        .border_1()
        .cursor_pointer()
        .border_color(if selected {
            theme::primary()
        } else {
            theme::border()
        })
        .when(selected, |d| d.bg(theme::selected()))
        .hover(|s| s.bg(theme::hover()))
        .on_click(move |_event, window, cx| {
            let request_id = request_id.clone();
            let question_id = question_id.clone();
            let value = value.clone();
            update_view(window, cx, move |this, cx| {
                let selected = this
                    .timeline_state
                    .user_input_answers
                    .entry(request_id)
                    .or_default()
                    .entry(question_id)
                    .or_default();
                if multi_select {
                    match selected.iter().position(|v| v == &value) {
                        Some(pos) => {
                            selected.remove(pos);
                        }
                        None => selected.push(value),
                    }
                } else {
                    *selected = vec![value];
                }
                cx.notify();
            });
        })
        .child(
            div()
                .text_xs()
                .flex_shrink_0()
                .text_color(if selected {
                    theme::primary()
                } else {
                    theme::fg_muted()
                })
                .child(mark),
        )
        .child(
            div().flex().flex_col().flex_1().min_w_0().gap_1().child(
                div()
                    .text_sm()
                    .text_color(if selected {
                        theme::fg()
                    } else {
                        theme::fg_muted()
                    })
                    .child(label.to_string()),
            ),
        );
    // The contract makes `description` required; the web hides it when it
    // just repeats the label.
    if !description.is_empty() && description != label {
        row = row.child(
            div()
                .flex_shrink_0()
                .max_w(px(280.))
                .text_xs()
                .text_color(theme::fg_muted())
                .child(description.to_string()),
        );
    }
    row
}

/// Display form of a recorded `questionAnswer.answers` value: a string, or a
/// multi-select array joined like the adapters do.
fn answer_display(answer: &Value) -> String {
    match answer {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

/// One-line input summary for a tool call without a `title`.
fn input_summary(input: &Value) -> String {
    let flat = match input {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    let flat = flat.replace('\n', " ");
    flat.chars().take(80).collect()
}

/// `+`/`-` counts from a unified diff when `additions`/`deletions` are absent.
fn diff_stats(diff: &str) -> (usize, usize) {
    let mut additions = 0;
    let mut deletions = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        } else if line.starts_with('+') {
            additions += 1;
        } else if line.starts_with('-') {
            deletions += 1;
        }
    }
    (additions, deletions)
}

fn diff_line_color(line: &str) -> Rgba {
    if line.starts_with("+++") || line.starts_with("---") {
        theme::fg_muted()
    } else if line.starts_with('+') {
        success()
    } else if line.starts_with('-') {
        danger()
    } else if line.starts_with("@@") {
        theme::primary()
    } else {
        theme::fg_muted()
    }
}

/// `RuntimeRequestIcon`/`ToolActivityIconView` colors for a settled request
/// outcome: approved green, declined/failed muted, in-flight warning.
fn outcome_color(item: &TurnItemView) -> Rgba {
    match item.status.as_str() {
        "completed" => success(),
        "cancelled" | "interrupted" | "failed" => theme::fg_muted(),
        _ => warning(),
    }
}

fn request_kind_label(kind: &str) -> &'static str {
    match kind {
        "command" => "Command approval",
        "file-change" => "File change approval",
        "file-read" => "File read approval",
        "permission" => "Permission approval",
        "mcp-elicitation" => "App access approval",
        "user_input" => "Question",
        "dynamic_tool_call" => "Tool approval",
        "auth_refresh" => "Sign-in request",
        _ => "Approval request",
    }
}

impl AppView {
    pub fn render_timeline(&self) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.render_timeline_inner())
    }

    fn render_timeline_inner(&self) -> impl IntoElement {
        // `.chat-scroll` side gutter (3rem ≥ sm, 12px below — fixed 48px keeps
        // the wide-layout look) around a `.chat-content-lane` centered at
        // `--chat-content-max-width` = 736px.
        let mut lane = div()
            .mx_auto()
            .w_full()
            .max_w(px(theme::CHAT_MAX_W))
            .flex()
            .flex_col();
        let list = || {
            div()
                .id("messages")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll_handle)
                .px(px(48.))
        };
        // Top/bottom spacers ≈ chat-list-header/footer (h-3 / h-4).
        lane = lane.child(div().h(px(12.)).flex_shrink_0());

        let empty_state = |lane: gpui::Div| {
            lane.child(
                div()
                    .flex_1()
                    .min_h(px(120.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme::fg_muted())
                    .text_sm()
                    .child("Send a message to start the conversation."),
            )
        };

        if self.state.connection != ConnectionStatus::Mock {
            if self.thread_items.is_empty() {
                if self.turn_started_at.is_some() {
                    lane = lane.child(self.render_working_row());
                    return list().child(lane);
                }
                return list().child(empty_state(lane));
            }
            for (i, item) in self.thread_items.iter().enumerate() {
                // Row rhythm: pb-4 under messages/plans, pb-2 under work-log
                // entries and events; consecutive work entries touch (the
                // web groups them at py-0 inside group-content).
                let next = self.thread_items.get(i + 1);
                let pad = if matches!(
                    item.kind.as_str(),
                    "user_message" | "assistant_message" | "proposed_plan"
                ) {
                    16.
                } else if is_work_kind(&item.kind) && next.is_some_and(|n| is_work_kind(&n.kind)) {
                    0.
                } else {
                    8.
                };
                lane = lane.child(
                    div()
                        .flex()
                        .flex_col()
                        .max_w_full()
                        .pb(px(pad))
                        .child(self.render_turn_item(i, item)),
                );
            }
            if self.turn_started_at.is_some() {
                lane = lane.child(self.render_working_row());
            }
            lane = lane.child(div().h(px(16.)).flex_shrink_0());
            return list().child(lane);
        }

        if self.state.messages.is_empty() {
            return list().child(empty_state(lane));
        }

        for (i, message) in self.state.messages.iter().enumerate() {
            let bubble = div()
                .p_3()
                .rounded(theme::radius::XL2)
                .text_sm()
                .max_w(gpui::relative(0.8))
                .child(message.text.clone());
            let copied = self.timeline_state.copied.as_deref() == Some(message.text.as_ref());
            let chip = copy_chip(
                "copy-m",
                i,
                copied,
                message.text.to_string(),
                message.text.to_string(),
            );
            let row = match message.role {
                Role::User => div()
                    .group("msg")
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap_1()
                    .child(bubble.bg(theme::user_bubble()).text_color(theme::fg()))
                    .child(
                        div()
                            .flex()
                            .w_full()
                            .max_w(gpui::relative(0.8))
                            .justify_end()
                            .pr_1()
                            .child(chip),
                    ),
                Role::Agent => div()
                    .group("msg")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .px_1()
                            .py_0p5()
                            .text_sm()
                            .text_color(theme::chat_fg())
                            .child(message.text.clone()),
                    )
                    .child(div().flex().items_center().gap_0p5().child(chip)),
            };
            lane = lane.child(div().id(("msg", i)).pb_4().child(row));
        }
        lane = lane.child(div().h(px(16.)).flex_shrink_0());
        list().child(lane)
    }

    /// `WorkingTimelineRow`: `border-b border-border/60 pb-2 pt-1` around a
    /// `h-6 items-baseline gap-2 px-1 text-sm text-muted-foreground
    /// tabular-nums` "Working for Ns" line.
    fn render_working_row(&self) -> impl IntoElement {
        let elapsed = self
            .turn_started_at
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        let label = if elapsed < 60 {
            format!("{elapsed}s")
        } else if elapsed < 3600 {
            format!("{}m {:02}s", elapsed / 60, elapsed % 60)
        } else {
            format!("{}h {:02}m", elapsed / 3600, (elapsed % 3600) / 60)
        };
        div()
            .border_b_1()
            .border_color(theme::hairline())
            .pb_2()
            .pt_1()
            .child(
                div()
                    .flex()
                    .h(px(24.))
                    .min_w_0()
                    .items_baseline()
                    .gap_2()
                    .px_1()
                    .text_sm()
                    .line_height(px(22.75))
                    .text_color(theme::fg_muted())
                    .child(format!("Working for {label}")),
            )
    }

    fn render_turn_item(&self, index: usize, item: &TurnItemView) -> AnyElement {
        match item.kind.as_str() {
            "user_message" => self.user_message_row(index, item),
            "assistant_message" => self.assistant_message_row(index, item),
            "reasoning" => self.reasoning_row(index, item),
            "command_execution" => self.command_row(index, item),
            "dynamic_tool" => self.tool_row(index, item),
            "file_change" => self.file_change_row(index, item),
            "approval_request" => self.approval_row(index, item),
            "user_input_request" => self.user_input_row(index, item),
            "todo_list" => self.todo_list_row(index, item),
            "system_notice" | "notification" => self.notice_row(item),
            "error" => self.error_row(item),
            "proposed_plan" => self.proposed_plan_row(item),
            "subagent" => self.subagent_row(index, item),
            "web_search" | "file_search" => self.search_row(index, item),
            "compaction" | "context_compaction" => self.compaction_row(item),
            _ => self.fallback_row(item),
        }
    }

    fn user_message_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        let raw_text = raw_str(item, "text")
            .map(str::to_owned)
            .or_else(|| item.body.clone())
            .unwrap_or_default();
        // `[Assistant quote](t3-citation://…)` links become chips like the
        // web's ChatMarkdown `AssistantCitationChip`; the raw href stays in
        // `raw_text` so Copy still yields the full serialized citation.
        let (text, citations) = extract_assistant_citations(&raw_text);
        // `UserMessageBubble`: `max-w-[80%] rounded-2xl bg-message p-3
        // text-sm` rendered as ChatMarkdown, right-aligned.
        let mut bubble = div()
            .max_w(gpui::relative(0.8))
            .p_3()
            .rounded(theme::radius::XL2)
            .text_sm()
            .line_height(px(22.75))
            .bg(theme::user_bubble())
            .text_color(theme::fg())
            .children(crate::markdown::render_markdown(&text));
        if !citations.is_empty() {
            let mut chips = div().flex().flex_wrap().gap_1().pt_1();
            for (ci, citation) in citations.iter().enumerate() {
                chips = chips.child(citation_chip(index, ci, citation));
            }
            bubble = bubble.child(chips);
        }
        let attachment_count = item.raw["attachments"].as_array().map_or(0, Vec::len);
        if attachment_count > 0 {
            bubble = bubble.child(div().pt_1().text_xs().text_color(theme::fg_muted()).child(
                format!(
                    "{attachment_count} attachment{}",
                    if attachment_count == 1 { "" } else { "s" }
                ),
            ));
        }
        let copied = self.timeline_state.copied.as_deref() == Some(item.id.as_str());
        // Queued turns (web `inputIntent: "queued_turn"`) get a marker chip —
        // they have not run yet, unlike a normal user message.
        let queued = raw_str(item, "inputIntent") == Some("queued_turn");
        // Under-bubble meta row: `max-w-[80%] justify-end pe-1 text-xs` with
        // the day-aware timestamp and ghost icon buttons, revealed on hover.
        let mut meta = div()
            .flex()
            .w_full()
            .max_w(gpui::relative(0.8))
            .items_center()
            .justify_end()
            .gap_0p5()
            .pr_1()
            .text_xs()
            .opacity(0.)
            .group_hover("msg", |s| s.opacity(1.));
        if queued {
            meta = meta.child(
                div()
                    .mr_1()
                    .px_1p5()
                    .py_0p5()
                    .rounded_full()
                    .border_1()
                    .border_color(theme::hairline())
                    .font_family(theme::FONT_MONO)
                    .text_size(theme::text_size::XS_3)
                    .text_color(theme::fg_muted())
                    .child("queued"),
            );
        }
        if let Some(ts) = item_timestamp(item) {
            meta = meta.child(ts);
        }
        meta = meta.child(copy_chip(
            "copy-u",
            index,
            copied,
            item.id.clone(),
            raw_text,
        ));
        div()
            .group("msg")
            .flex()
            .flex_col()
            .items_end()
            .gap_1()
            .child(bubble)
            .child(meta)
            .into_any_element()
    }

    /// `AssistantTimelineRow`: `px-1 py-0.5` unboxed ChatMarkdown (no dim
    /// while streaming), then a `mt-1.5` meta row — status pill, ghost copy,
    /// thumbs, day-aware timestamp — revealed by `group-hover/assistant`.
    fn assistant_message_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        let raw_text = raw_str(item, "text")
            .or(item.body.as_deref())
            .unwrap_or_default()
            .to_string();
        // Quote annotations (`t3-citation:` links) become chips, same as the
        // web's ChatMarkdown `AssistantCitationChip`.
        let (text, citations) = extract_assistant_citations(&raw_text);
        let show_empty = text.is_empty() && !item.streaming;
        let mut col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .px_1()
            .py_0p5()
            .children(crate::markdown::render_markdown(&text));
        if show_empty {
            col = col.child(
                div()
                    .text_sm()
                    .line_height(px(22.75))
                    .text_color(theme::chat_fg())
                    .child("(empty response)"),
            );
        }
        if !citations.is_empty() {
            let mut chips = div().flex().flex_wrap().gap_1();
            for (ci, citation) in citations.iter().enumerate() {
                chips = chips.child(citation_chip(index, ci, citation));
            }
            col = col.child(chips);
        }
        // 👎 opens the optional-comment field inline under the message.
        if let Some((_, _, field)) = self
            .timeline_state
            .feedback_comment
            .as_ref()
            .filter(|(id, _, _)| id == &item.id)
        {
            col = col.child(self.feedback_comment_row(index, item, field));
        }

        // `AssistantMessageMeta`: `flex items-center gap-2 text-xs
        // tabular-nums`, opacity-0 → group-hover reveal.
        let copied = self.timeline_state.copied.as_deref() == Some(item.id.as_str());
        let mut meta = div()
            .flex()
            .items_center()
            .gap_2()
            .mt(px(6.))
            .text_xs()
            .opacity(0.)
            .group_hover("msg", |s| s.opacity(1.));
        // Non-completed projected items lead with a mono-3xs status pill.
        if item.status != "completed" {
            meta = meta.child(
                div()
                    .px_1p5()
                    .py_0p5()
                    .rounded_full()
                    .border_1()
                    .border_color(theme::hairline())
                    .font_family(theme::FONT_MONO)
                    .text_size(theme::text_size::XS_3)
                    .text_color(theme::fg_muted())
                    .child(item.status.clone()),
            );
        }
        let mut buttons = div().flex().items_center().gap_0p5().child(copy_chip(
            "copy-a",
            index,
            copied,
            item.id.clone(),
            raw_text,
        ));
        // Hover feedback thumbs on every settled assistant message; a click
        // on a non-Codex thread explains provider.uploadFeedback isn't
        // available there (same message as the web's /feedback gate).
        if !item.streaming {
            let sent = self.timeline_state.feedback_sent.contains(&item.id);
            let busy = self.timeline_state.feedback_in_flight.contains(&item.id);
            // The item's own threadId is canonical; fall back to the
            // selected thread (this row only renders in it anyway).
            let thread_id = raw_str(item, "threadId")
                .map(str::to_owned)
                .or_else(|| self.state.selected.as_ref().map(|s| s.to_string()))
                .unwrap_or_default();
            for (up, glyph, eid) in [(true, "☝", 0usize), (false, "☟", 1usize)] {
                let tid = thread_id.clone();
                let iid = item.id.clone();
                let shown = if sent {
                    "✓"
                } else if busy {
                    "…"
                } else {
                    glyph
                };
                let mut chip = ghost_icon_chip(("fb", index * 4 + eid), shown);
                if sent || busy {
                    chip = chip.opacity(0.4).group_hover("msg", |s| s.opacity(0.4));
                } else {
                    chip = chip.on_click(move |_event, window, cx| {
                        let tid = tid.clone();
                        let iid = iid.clone();
                        update_view(window, cx, move |this, cx| {
                            if up {
                                this.send_message_feedback(tid, "thumbs up".to_string(), iid, cx);
                            } else {
                                this.open_feedback_comment(iid, tid, cx);
                            }
                        });
                    });
                }
                buttons = buttons.child(chip);
            }
        }
        meta = meta.child(buttons);
        if !item.streaming {
            if let Some(ts) = item_timestamp(item) {
                meta = meta.child(ts);
            }
        }
        div()
            .group("msg")
            .flex()
            .flex_col()
            .child(col)
            .child(meta)
            .into_any_element()
    }

    /// provider.uploadFeedback only exists for the Codex adapter — same
    /// gate the web puts on its `/feedback` composer command.
    fn feedback_upload_allowed(&self) -> bool {
        match self.state.selected_thread() {
            Some(thread) if !thread.provider_instances.is_empty() => thread
                .provider_instances
                .iter()
                .any(|p| p.as_ref() == "codex"),
            _ => self.provider == "codex",
        }
    }

    /// The Codex-only explanation shown when thumbs are clicked on a
    /// non-Codex thread (mirrors composer.rs `/feedback` handling).
    fn feedback_unsupported(&mut self, cx: &mut gpui::Context<Self>) {
        self.push_toast(
            ToastKind::Error,
            "Feedback upload is only supported for Codex threads",
            None,
            None,
            cx,
        );
    }

    /// The 👎 inline comment row: free-text `reason` + Send / ×. Enter
    /// submits through the field's `PromptSubmitted` subscription.
    fn feedback_comment_row(
        &self,
        index: usize,
        item: &TurnItemView,
        field: &Entity<TextField>,
    ) -> gpui::Div {
        let thread_id = raw_str(item, "threadId").unwrap_or_default().to_string();
        let item_id = item.id.clone();
        let field = field.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .pt_1()
            .child(div().text_xs().text_color(theme::fg_muted()).child("☟"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(theme::fg())
                    .child(field.clone()),
            )
            .child({
                let tid = thread_id.clone();
                let iid = item_id.clone();
                let f = field.clone();
                div()
                    .id(("fb-send", index))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .bg(theme::primary())
                    .text_color(gpui::white())
                    .hover(|s| s.opacity(0.9))
                    .on_click(move |_event, window, cx| {
                        let comment = f.update(cx, |field, cx| {
                            let len = field.text_length_utf16(window, cx).unwrap_or(0);
                            field
                                .text_for_range(0..len, &mut None, window, cx)
                                .unwrap_or_default()
                        });
                        let tid = tid.clone();
                        let iid = iid.clone();
                        update_view(window, cx, move |this, cx| {
                            let reason = match comment.trim() {
                                "" => "thumbs down".to_string(),
                                c => format!("thumbs down: {c}"),
                            };
                            this.send_message_feedback(tid, reason, iid, cx);
                        });
                    })
                    .child("Send")
            })
            .child(
                div()
                    .id(("fb-cancel", index))
                    .px_1()
                    .py_0p5()
                    .text_xs()
                    .cursor_pointer()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.text_color(theme::fg()))
                    .on_click(move |_event, window, cx| {
                        update_view(window, cx, move |this, cx| {
                            this.timeline_state.feedback_comment = None;
                            cx.notify();
                        });
                    })
                    .child("×"),
            )
    }

    /// 👎 opens the comment field; the entered text becomes the uploaded
    /// `reason` (the only free field the feedback contract carries).
    fn open_feedback_comment(
        &mut self,
        item_id: String,
        thread_id: String,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.timeline_state.feedback_in_flight.contains(&item_id)
            || self.timeline_state.feedback_sent.contains(&item_id)
        {
            return;
        }
        if !self.feedback_upload_allowed() {
            self.feedback_unsupported(cx);
            return;
        }
        let field = match &self.timeline_state.feedback_comment {
            Some((_, _, field)) => {
                let field = field.clone();
                // Reopening on a different message starts the comment fresh.
                edit_text_field(&field, cx, |field, window, cx| field.reset(window, cx));
                field
            }
            None => {
                let field = cx.new(|cx| TextField::new("Optional — what went wrong?", cx));
                let sub = cx.subscribe(&field, move |this, _field, event: &PromptSubmitted, cx| {
                    let (item_id, thread_id) = match &this.timeline_state.feedback_comment {
                        Some((i, t, _)) => (i.clone(), t.clone()),
                        None => return,
                    };
                    let reason = match event.0.trim() {
                        "" => "thumbs down".to_string(),
                        c => format!("thumbs down: {c}"),
                    };
                    this.send_message_feedback(thread_id, reason, item_id, cx);
                });
                self.timeline_state.feedback_sub = Some(sub);
                field
            }
        };
        focus_text_field(&field, cx);
        self.timeline_state.feedback_comment = Some((item_id, thread_id, field));
        cx.notify();
    }

    /// `provider.uploadFeedback` — payload mirrors the web `/feedback`
    /// command exactly: `{threadId, reason}` (`reason` is the contract's
    /// only free field; there is no rating field on the wire).
    fn send_message_feedback(
        &mut self,
        thread_id: String,
        reason: String,
        item_id: String,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.timeline_state.feedback_in_flight.contains(&item_id)
            || self.timeline_state.feedback_sent.contains(&item_id)
        {
            return;
        }
        if !self.feedback_upload_allowed() {
            self.feedback_unsupported(cx);
            return;
        }
        let mut payload = json!({ "threadId": thread_id });
        if !reason.trim().is_empty() {
            payload["reason"] = json!(reason.trim());
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
        self.timeline_state
            .feedback_in_flight
            .insert(item_id.clone());
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                this.timeline_state.feedback_in_flight.remove(&item_id);
                if exit["_tag"].as_str() == Some("Success") {
                    this.timeline_state.feedback_sent.insert(item_id.clone());
                    // Close the comment row only if it's still this message's.
                    if this
                        .timeline_state
                        .feedback_comment
                        .as_ref()
                        .is_some_and(|(id, _, _)| id == &item_id)
                    {
                        this.timeline_state.feedback_comment = None;
                    }
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
                        Some(rpc_error_text(&exit)),
                        None,
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `live-activity-focus` approximation: while an entry runs, the label
    /// alternates between `--secondary-label` and `--foreground` each
    /// `working_pulse` tick (500ms repaint).
    fn live_label_color(&self, live: bool) -> Rgba {
        if live && self.working_pulse % 2 == 0 {
            theme::fg()
        } else {
            theme::secondary_label()
        }
    }

    fn reasoning_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        // Reasoning stays collapsed once settled; while it streams it is shown.
        let expanded = item.streaming || self.timeline_state.expanded.contains(&item.id);
        let text = raw_str(item, "text")
            .or(item.body.as_deref())
            .unwrap_or_default()
            .to_string();
        // Collapsed rows preview the reasoning text itself (the web flattens
        // the detail into the label); expanded rows show the verb + detail.
        let preview = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let live = is_live(item);
        let label_text = if expanded || preview.is_empty() {
            if live {
                "Thinking".to_string()
            } else {
                "Thought".to_string()
            }
        } else {
            preview
        };
        let mut header = work_header(
            index,
            item,
            work_glyph("reasoning"),
            div()
                .text_color(self.live_label_color(live))
                .child(label_text),
        );
        header = header
            .when_some(item_timestamp(item), |d, ts| d.child(ts))
            .child(work_chevron(expanded));
        let mut col = div().flex().flex_col().child(header);
        if expanded && !text.is_empty() {
            col = col.child(detail_text("reason", index).child(div().child(text)));
        }
        col.into_any_element()
    }

    fn command_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        // Contract spells `input`/`output`/`exitCode`; the native codex
        // provider emits `command` plus `result: {output, exitCode}`.
        let input = raw_str(item, "input")
            .or_else(|| raw_str(item, "command"))
            .unwrap_or("command");
        let output = raw_str(item, "output")
            .or_else(|| item.raw["result"]["output"].as_str())
            .map(command_output_text)
            .filter(|o| !o.trim().is_empty())
            .map(cap_detail);
        let exit_code = item.raw["exitCode"]
            .as_i64()
            .or_else(|| item.raw["result"]["exitCode"].as_i64());
        let failed = item.status == "failed"
            || exit_code.is_some_and(|code| code != 0)
            || item.raw["outputIndicatesFailure"].as_bool() == Some(true);
        let expanded = self.timeline_state.expanded.contains(&item.id);
        let live = is_live(item);
        // `liveWorkEntryLabel` for commands: "Running"/"Failed"/"Stopped"/
        // "Ran" + the command text.
        let verb = if live {
            "Running"
        } else if failed {
            "Failed"
        } else if matches!(item.status.as_str(), "cancelled" | "interrupted") {
            "Stopped"
        } else {
            "Ran"
        };
        let can_expand = output.is_some()
            || item.raw["outputOmitted"].as_bool() == Some(true)
            || exit_code.is_some();
        let mut header = work_header(
            index,
            item,
            work_glyph("command_execution"),
            div()
                .text_color(self.live_label_color(live))
                .child(format!("{verb} {input}")),
        );
        if failed {
            header = header.child(
                div()
                    .text_xs()
                    .text_color(theme::tool_error_icon())
                    .flex_shrink_0()
                    .child("✕"),
            );
        }
        header = header
            .when_some(item_timestamp(item), |d, ts| d.child(ts))
            .child(if can_expand {
                work_chevron(expanded)
            } else {
                div().w(px(16.)).flex_shrink_0()
            });
        let mut col = div().flex().flex_col().child(header);
        if expanded && can_expand {
            let mut detail = detail_panel("cmd-out", index);
            detail = detail.child(
                div()
                    .pb_1()
                    .text_color(theme::fg_muted())
                    .child(format!("$ {input}")),
            );
            detail = match output {
                Some(output) => detail.child(div().child(output)),
                None if item.raw["outputOmitted"].as_bool() == Some(true) => {
                    detail.child(div().italic().child(
                        "Output withheld on the wire — fetch it with orchestration.getTurnItem.",
                    ))
                }
                None => detail.child(div().italic().child("No output.")),
            };
            if let Some(code) = exit_code {
                detail = detail.child(div().pt_1().child(format!("exit {code}")));
            }
            col = col.child(detail);
        }
        col.into_any_element()
    }

    fn tool_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        // Contract spells `toolName`/`input`/`output`; the native codex
        // provider emits `tool`/`server`/`arguments`/`result` instead.
        let tool = raw_str(item, "toolName").or_else(|| raw_str(item, "tool"));
        let name: String = match (raw_str(item, "server"), tool) {
            (Some(server), Some(tool)) if server != tool => format!("{server} · {tool}"),
            (_, Some(tool)) => tool.to_string(),
            _ => "tool".to_string(),
        };
        let input_value = if item.raw["input"].is_null() {
            &item.raw["arguments"]
        } else {
            &item.raw["input"]
        };
        let summary = raw_str(item, "title")
            .map(str::to_owned)
            .unwrap_or_else(|| input_summary(input_value));
        let output = tool_output_text(&item.raw["output"])
            .or_else(|| tool_result_text(&item.raw["result"]))
            .map(cap_detail);
        let expanded = self.timeline_state.expanded.contains(&item.id);
        let live = is_live(item);
        let failed = item.status == "failed";
        // The work-log label reads "name  ·  arg preview" in one muted run —
        // the web puts the display label (often the summary alone) in the
        // secondary-label span.
        let label = if summary.is_empty() {
            name.clone()
        } else {
            format!("{name}  {summary}")
        };
        let can_expand = output.is_some()
            || item.raw["outputOmitted"].as_bool() == Some(true)
            || pretty_json(input_value).is_some();
        let mut header = work_header(
            index,
            item,
            work_glyph("dynamic_tool"),
            div().text_color(self.live_label_color(live)).child(label),
        );
        if failed {
            header = header.child(
                div()
                    .text_xs()
                    .text_color(theme::tool_error_icon())
                    .flex_shrink_0()
                    .child("✕"),
            );
        }
        header = header
            .when_some(item_timestamp(item), |d, ts| d.child(ts))
            .child(if can_expand {
                work_chevron(expanded)
            } else {
                div().w(px(16.)).flex_shrink_0()
            });
        let mut col = div().flex().flex_col().child(header);
        if expanded {
            let mut detail = detail_panel("tool-out", index);
            let mut shown = false;
            if let Some(output) = &output {
                shown = true;
                detail = detail.child(div().child(output.clone()));
            } else if item.raw["outputOmitted"].as_bool() == Some(true) {
                shown = true;
                detail = detail.child(div().italic().child(
                    "Output withheld on the wire — fetch it with orchestration.getTurnItem.",
                ));
            }
            if let Some(input) = pretty_json(input_value) {
                shown = true;
                detail = detail.child(div().pt_2().child(format!("input\n{input}")));
            }
            if !shown {
                detail = detail.child(div().italic().child("No detail."));
            }
            col = col.child(detail);
        }
        col.into_any_element()
    }

    fn file_change_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        // Contract spells `fileName`/`diffStr`; providers may instead carry a
        // `changes` array (`{path, diff?}`, TS spells `{operation, path,
        // oldPath}`) or bare `oldStr`/`newStr` snapshots.
        let changes: &[Value] = item.raw["changes"].as_array().map_or(&[], Vec::as_slice);
        let file_name = raw_str(item, "fileName")
            .or_else(|| changes.first().and_then(|c| c["path"].as_str()))
            .unwrap_or("file");
        let diff = raw_str(item, "diffStr")
            .or_else(|| changes.iter().find_map(|c| c["diff"].as_str()))
            .unwrap_or_default();
        let (additions, deletions) = match (
            item.raw["additions"].as_u64(),
            item.raw["deletions"].as_u64(),
        ) {
            (Some(adds), Some(dels)) => (adds as usize, dels as usize),
            (Some(adds), None) => (adds as usize, 0),
            (None, Some(dels)) => (0, dels as usize),
            (None, None) => diff_stats(diff),
        };
        let expanded = self.timeline_state.expanded.contains(&item.id);
        let file_label = if changes.len() > 1 {
            format!("{file_name}  +{} more", changes.len() - 1)
        } else {
            file_name.to_string()
        };

        let mut stats = div().flex().items_center().gap_1().flex_shrink_0();
        if additions > 0 {
            stats = stats.child(
                div()
                    .text_xs()
                    .text_color(success())
                    .child(format!("+{additions}")),
            );
        }
        if deletions > 0 {
            stats = stats.child(
                div()
                    .text_xs()
                    .text_color(danger())
                    .child(format!("-{deletions}")),
            );
        }

        let live = is_live(item);
        // Web work entries label file edits `verb path` — "Editing"/"Edited"/
        // "Failed" — in the muted secondary-label run.
        let verb = if live {
            "Editing"
        } else if item.status == "failed" {
            "Failed"
        } else if matches!(item.status.as_str(), "cancelled" | "interrupted") {
            "Stopped"
        } else {
            "Edited"
        };
        let mut header = work_header(
            index,
            item,
            work_glyph("file_change"),
            div()
                .text_color(self.live_label_color(live))
                .child(format!("{verb} {file_label}")),
        );
        header = header
            .child(stats)
            .when_some(item_timestamp(item), |d, ts| d.child(ts))
            .child(work_chevron(expanded));

        let mut col = div().flex().flex_col().child(header);

        if expanded {
            let mut detail = detail_panel("diff", index);
            if !diff.is_empty() {
                for line in diff.lines() {
                    detail = detail.child(
                        div()
                            .text_color(diff_line_color(line))
                            .child(line.to_string()),
                    );
                }
            } else {
                // Snapshot providers ship `oldStr`/`newStr` rather than a
                // unified diff — render them as removals then additions.
                let old_str = raw_str(item, "oldStr").unwrap_or_default();
                let new_str = raw_str(item, "newStr").unwrap_or_default();
                if old_str.is_empty() && new_str.is_empty() {
                    detail = detail.child(
                        div()
                            .italic()
                            .text_color(theme::fg_muted())
                            .child("No diff recorded."),
                    );
                } else {
                    for line in old_str.lines() {
                        detail =
                            detail.child(div().text_color(danger()).child(format!("- {line}")));
                    }
                    for line in new_str.lines() {
                        detail =
                            detail.child(div().text_color(success()).child(format!("+ {line}")));
                    }
                }
            }
            col = col.child(detail);
        }
        col.into_any_element()
    }

    fn approval_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        let request_id = raw_str(item, "requestId").unwrap_or_default().to_string();
        let kind = raw_str(item, "requestKind").unwrap_or("request");
        let prompt = raw_str(item, "prompt").map(str::to_owned);
        let reason = raw_str(item, "reason").map(str::to_owned);
        let app_name = raw_str(item, "appName").map(str::to_owned);
        let responded = self.timeline_state.responded.contains(&request_id);
        // Without a requestId the respond command can't be addressed —
        // render the card read-only rather than dead buttons.
        let pending = !request_id.is_empty()
            && matches!(item.status.as_str(), "waiting" | "pending" | "running")
            && !responded;

        let options: Vec<(String, String, bool)> = item.raw["options"]
            .as_array()
            .map(|options| {
                options
                    .iter()
                    .filter_map(|option| {
                        let decision = option["decision"].as_str()?.to_string();
                        let label = option["label"].as_str().unwrap_or(&decision).to_string();
                        Some((decision, label, option["warning"].is_string()))
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|options| !options.is_empty())
            .unwrap_or_else(|| {
                vec![
                    ("accept".to_string(), "Accept".to_string(), false),
                    ("decline".to_string(), "Decline".to_string(), false),
                ]
            });

        // `TimelinePendingApprovalRow` — a flat WorkLogRow (wrapLabel): the
        // `<code>` prompt is the label; kind icon sits in the 24-px slot; the
        // `ms-7.5` detail carries appName/reason and, while pending, the
        // "Waiting for approval" warning plus inline text-link buttons.
        let mut col = div().flex().flex_col();
        let mut header = work_row_line()
            .id(("head", index))
            .group("timeline-row")
            .items_start()
            .child(work_icon(approval_glyph(kind), item.status == "failed"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_family(theme::FONT_MONO)
                    .text_xs()
                    .line_height(px(20.))
                    .text_color(theme::fg())
                    .child(
                        prompt
                            .clone()
                            .unwrap_or_else(|| request_kind_label(kind).to_string()),
                    ),
            );
        header = header.when_some(item_timestamp(item), |d, ts| d.child(ts));
        col = col.child(header);

        // `ms-7.5 flex flex-col gap-1 pb-1 text-xs`
        let mut detail = div().flex().flex_col().gap_1().ml(px(30.)).pb_1().text_xs();
        if let Some(app_name) = app_name {
            detail = detail.child(div().text_color(theme::fg_muted()).child(app_name));
        }
        if let Some(reason) = reason {
            detail = detail.child(div().text_color(theme::fg_muted()).child(reason));
        }

        if pending {
            // "⚠ Waiting for approval · Accept Decline More…" — InlineButton
            // text links (accept foreground/medium, everything else muted).
            let mut actions = div().flex().flex_wrap().items_center().gap_x_2().gap_y_1();
            actions = actions.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_color(warning())
                    .child(div().flex_shrink_0().child("⚠"))
                    .child(div().child("Waiting for approval")),
            );
            actions = actions.child(div().text_color(theme::fg_muted()).child("·"));
            for (option_index, (decision, label, has_warning)) in options.iter().enumerate() {
                let primary = decision == "accept";
                let payload = json!({
                    "type": "runtime-request.respond",
                    "commandId": uuid::Uuid::new_v4().to_string(),
                    "threadId": item.raw["threadId"],
                    "requestId": request_id.clone(),
                    "decision": decision,
                });
                let request_id = request_id.clone();
                let label = label.clone();
                actions = actions.child(
                    div()
                        .id(("approve", index * 64 + option_index))
                        .flex()
                        .items_center()
                        .gap_0p5()
                        .cursor_pointer()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(if primary {
                            theme::fg()
                        } else {
                            theme::fg_muted()
                        })
                        .hover(|s| s.text_color(theme::fg()))
                        .on_click(move |_event, window, cx| {
                            let payload = payload.clone();
                            let request_id = request_id.clone();
                            update_view(window, cx, move |this, cx| {
                                this.timeline_state.responded.insert(request_id);
                                this.dispatch(payload);
                                cx.notify();
                            });
                        })
                        .when(*has_warning, |d| d.child(div().child("⚠")))
                        .child(div().child(label)),
                );
            }
            detail = detail.child(actions);
        } else {
            // Resolved: prefer the option label matching a decision echoed on
            // the item, else map the settled status to an outcome word.
            let outcome = match raw_str(item, "decision") {
                Some(decision) => options
                    .iter()
                    .find(|(d, _, _)| d.as_str() == decision)
                    .map(|(_, label, _)| label.clone())
                    .unwrap_or_else(|| decision.to_string()),
                None => match item.status.as_str() {
                    "completed" => "Approved".to_string(),
                    "cancelled" | "interrupted" => "Declined".to_string(),
                    "failed" => "Failed".to_string(),
                    _ if responded => "Response sent — waiting for the provider…".to_string(),
                    _ => "Pending".to_string(),
                },
            };
            detail = detail.child(div().text_color(outcome_color(item)).child(outcome));
        }
        col = col.child(detail);
        col.into_any_element()
    }

    /// `user_input_request` — the agent's question card. Pending cards list
    /// each question's options; Submit dispatches `runtime-request.respond`
    /// with `answers` keyed by question id (a string, or an array for
    /// `multiSelect`), matching `respondToThreadUserInput` on web.
    /// Message-mode requests (`responseMode: "message"`) are also
    /// dismissible via `thread.user-input.dismiss`.
    fn user_input_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        let request_id = raw_str(item, "requestId").unwrap_or_default().to_string();
        let questions: &[Value] = item.raw["questions"].as_array().map_or(&[], Vec::as_slice);
        let answered = item.raw["questionAnswer"]["answers"].as_object();
        let question_text_by_id = &item.raw["questionAnswer"]["questionTextById"];
        let responded = self.timeline_state.responded.contains(&request_id);
        let message_mode = raw_str(item, "responseMode") == Some("message");
        let pending = !request_id.is_empty()
            && matches!(item.status.as_str(), "waiting" | "pending" | "running")
            && answered.is_none()
            && !responded;

        // Resolve one question's would-be answer: the recorded selection, or
        // the provider's `initialAnswer` suggestion (seeded like the web
        // does, only while the question is untouched), or "" for a skipped
        // `required: false` question.
        let drafts = self.timeline_state.user_input_answers.get(&request_id);
        let answer_for = |question: &Value| -> Option<Value> {
            let qid = question["id"].as_str().unwrap_or_default();
            let recorded = drafts.and_then(|d| d.get(qid));
            let multi = question["multiSelect"].as_bool() == Some(true);
            match recorded {
                Some(values) if !values.is_empty() => {
                    if multi {
                        Some(Value::Array(
                            values.iter().map(|v| json!(v.as_str())).collect(),
                        ))
                    } else {
                        Some(json!(values[0].as_str()))
                    }
                }
                _ => {
                    let custom_ok = question["allowCustomAnswer"].as_bool() != Some(false);
                    if custom_ok {
                        if let Some(initial) = question["initialAnswer"].as_str() {
                            return Some(json!(initial));
                        }
                    }
                    if question["required"].as_bool() == Some(false) {
                        return Some(json!(""));
                    }
                    None
                }
            }
        };
        // Submittable only once every question resolves and at least one
        // answer is non-empty (the server rejects an all-skipped reply).
        let resolved_answers: Option<serde_json::Map<String, Value>> = if pending {
            let mut map = serde_json::Map::new();
            let mut any_answered = false;
            let mut complete = true;
            for question in questions {
                match answer_for(question) {
                    Some(answer) => {
                        if answer != json!("") {
                            any_answered = true;
                        }
                        map.insert(
                            question["id"].as_str().unwrap_or_default().to_string(),
                            answer,
                        );
                    }
                    None => complete = false,
                }
            }
            (complete && any_answered).then_some(map)
        } else {
            None
        };

        // Web renders this as a `ComposerBanner` (rounded-2xl
        // composer-attached surface); the native timeline keeps the
        // interactive questions inline, so it stays a rounded-2xl card.
        let mut card = div()
            .rounded(theme::radius::XL2)
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .min_h(px(24.))
                    .gap(px(6.))
                    .child(work_icon("?", false))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::secondary_label())
                            .child("Question"),
                    )
                    .when(pending, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(warning())
                                .child("Waiting for your answer"),
                        )
                    }),
            );

        if questions.is_empty() {
            // No question schema on the wire — show whatever the item carries.
            if let Some(answers_map) = answered {
                // Resolved without a schema: list recorded answers, using
                // `questionTextById` labels when the server echoed them.
                for (qid, answer) in answers_map {
                    let label = question_text_by_id[qid.as_str()]
                        .as_str()
                        .unwrap_or(qid.as_str());
                    card = card.child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme::fg())
                                    .child(label.to_string()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::fg_muted())
                                    .child(format!("→ {}", answer_display(answer))),
                            ),
                    );
                }
            } else {
                card = card.child(
                    div().text_sm().text_color(theme::fg()).child(
                        item.body
                            .clone()
                            .or_else(|| item.title.clone())
                            .unwrap_or_else(|| "The agent asked a question.".to_string()),
                    ),
                );
                if pending {
                    card = card.child(
                        div()
                            .text_xs()
                            .italic()
                            .text_color(theme::fg_muted())
                            .child(
                                "This question needs a typed answer — respond from another client.",
                            ),
                    );
                }
            }
        }

        for (qi, question) in questions.iter().enumerate() {
            let qid = question["id"].as_str().unwrap_or_default().to_string();
            let header = question["header"].as_str().unwrap_or("");
            let question_text = question["question"].as_str().unwrap_or(header);
            let multi = question["multiSelect"].as_bool() == Some(true);
            let selected: Vec<String> =
                drafts
                    .and_then(|d| d.get(&qid))
                    .cloned()
                    .unwrap_or_else(|| {
                        // Seed `initialAnswer` as the selection until touched.
                        let custom_ok = question["allowCustomAnswer"].as_bool() != Some(false);
                        question["initialAnswer"]
                            .as_str()
                            .filter(|_| custom_ok)
                            .map(|s| vec![s.to_string()])
                            .unwrap_or_default()
                    });

            let mut block = div().flex().flex_col().gap_1().pt_1();
            if !header.is_empty() && header != question_text {
                block = block.child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::fg_muted())
                        .child(header.to_string()),
                );
            }
            block = block.child(
                div()
                    .text_sm()
                    .text_color(theme::fg())
                    .child(question_text.to_string()),
            );

            if pending {
                let custom_ok = question["allowCustomAnswer"].as_bool() != Some(false);
                let mut option_rows = div().flex().flex_col().gap_1();
                let mut option_index = 0usize;
                // The provider's `initialAnswer` suggestion renders first, as
                // a selectable chip — the web seeds it as the draft answer.
                if custom_ok {
                    if let Some(initial) = question["initialAnswer"].as_str() {
                        option_rows = option_rows.child(user_input_option(
                            index,
                            qi,
                            option_index,
                            &request_id,
                            &qid,
                            initial,
                            initial,
                            "",
                            multi,
                            selected.iter().any(|v| v.as_str() == initial),
                        ));
                        option_index += 1;
                    }
                }
                let options: &[Value] = question["options"].as_array().map_or(&[], Vec::as_slice);
                for option in options {
                    let label = option["label"].as_str().unwrap_or("option");
                    let value = option["value"].as_str().unwrap_or(label);
                    let description = option["description"].as_str().unwrap_or("");
                    option_rows = option_rows.child(user_input_option(
                        index,
                        qi,
                        option_index,
                        &request_id,
                        &qid,
                        value,
                        label,
                        description,
                        multi,
                        selected.iter().any(|v| v.as_str() == value),
                    ));
                    option_index += 1;
                }
                if option_index == 0 {
                    option_rows = option_rows.child(
                        div()
                            .text_xs()
                            .italic()
                            .text_color(theme::fg_muted())
                            .child(
                                "This question needs a typed answer — respond from another client.",
                            ),
                    );
                }
                block = block.child(option_rows);
            } else {
                // Resolved: the recorded answer text beside the question.
                let answer_text = answered
                    .and_then(|map| map.get(&qid))
                    .map(answer_display)
                    .unwrap_or_default();
                let label = if answer_text.is_empty() {
                    match item.status.as_str() {
                        "cancelled" | "interrupted" => "Dismissed".to_string(),
                        _ if responded => "Answer sent — waiting for the provider…".to_string(),
                        _ => "No answer recorded".to_string(),
                    }
                } else {
                    answer_text
                };
                block = block.child(
                    div()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(format!("→ {label}")),
                );
            }
            card = card.child(block);
        }

        if pending {
            let mut actions = div().flex().items_center().gap_2().pt_1();
            if let Some(answers_map) = resolved_answers {
                let payload = json!({
                    "type": "runtime-request.respond",
                    "commandId": uuid::Uuid::new_v4().to_string(),
                    "threadId": item.raw["threadId"],
                    "requestId": request_id.clone(),
                    "answers": Value::Object(answers_map),
                });
                let rid = request_id.clone();
                actions = actions.child(
                    div()
                        .id(("ui-submit", index))
                        .px_2p5()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .bg(theme::primary())
                        .text_color(gpui::white())
                        .hover(|s| s.opacity(0.9))
                        .on_click(move |_event, window, cx| {
                            let payload = payload.clone();
                            let rid = rid.clone();
                            update_view(window, cx, move |this, cx| {
                                this.timeline_state.responded.insert(rid);
                                this.dispatch(payload);
                                cx.notify();
                            });
                        })
                        .child("Submit"),
                );
            } else {
                actions = actions.child(
                    div()
                        .px_2p5()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .border_1()
                        .border_color(theme::border())
                        .opacity(0.6)
                        .child("Submit"),
                );
            }
            if message_mode {
                let payload = json!({
                    "type": "thread.user-input.dismiss",
                    "commandId": uuid::Uuid::new_v4().to_string(),
                    "threadId": item.raw["threadId"],
                    "requestId": request_id.clone(),
                });
                let rid = request_id.clone();
                actions = actions.child(
                    div()
                        .id(("ui-dismiss", index))
                        .px_2p5()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .border_1()
                        .border_color(theme::border())
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(move |_event, window, cx| {
                            let payload = payload.clone();
                            let rid = rid.clone();
                            update_view(window, cx, move |this, cx| {
                                this.timeline_state.responded.insert(rid);
                                this.dispatch(payload);
                                cx.notify();
                            });
                        })
                        .child("Dismiss"),
                );
            }
            card = card.child(actions);
        } else if questions.is_empty() {
            card = card.child(
                div()
                    .text_xs()
                    .text_color(match item.status.as_str() {
                        "completed" => success(),
                        "cancelled" | "interrupted" | "failed" => theme::fg_muted(),
                        _ => warning(),
                    })
                    .child(match item.status.as_str() {
                        "completed" => "Answered",
                        "cancelled" | "interrupted" => "Dismissed",
                        "failed" => "Failed",
                        _ if answered.is_some() => "Answered",
                        _ if responded => "Answer sent — waiting for the provider…",
                        _ => "Pending",
                    }),
            );
        }

        card.into_any_element()
    }

    /// `compaction` / `context_compaction` — a muted divider marking where the
    /// context was compacted, with token counts when the provider sent them.
    fn compaction_row(&self, item: &TurnItemView) -> AnyElement {
        let failed = item.status == "failed";
        let live = is_live(item);
        let before = item.raw["beforeTokenCount"].as_u64();
        let after = item.raw["afterTokenCount"].as_u64();
        let mut label = if live {
            "Compacting context…".to_string()
        } else {
            match (before, after) {
                (Some(b), Some(a)) => format!("Context compacted {b} → {a} tokens"),
                _ => "Context compacted".to_string(),
            }
        };
        if let Some(summary) = raw_str(item, "summary") {
            label.push_str(&format!(
                "  · {}",
                summary.chars().take(120).collect::<String>()
            ));
        }
        // `ContextCompactionTimelineRow`: `mx-auto … gap-3 py-1 text-xs
        // text-muted-foreground` flanked by `h-px flex-1 bg-border/70` rules.
        let line_color = if failed {
            danger_border()
        } else {
            theme::hairline()
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .py_1()
            .child(div().flex_1().h(px(1.)).bg(line_color))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .text_xs()
                            .when(failed, |d| d.text_color(danger()))
                            .when(!failed, |d| d.text_color(theme::fg_muted()))
                            .child(if failed { "⚠" } else { "⇥⇤" }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .max_w(px(720.))
                            .when(failed, |d| d.text_color(danger()))
                            .when(!failed, |d| d.text_color(theme::fg_muted()))
                            .child(label),
                    ),
            )
            .child(div().flex_1().h(px(1.)).bg(line_color))
            .into_any_element()
    }

    /// `todo_list` — a `list-todo` work entry; the step checklist sits in the
    /// expanded `ms-7` detail.
    fn todo_list_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        let expanded = self.timeline_state.expanded.contains(&item.id);
        let steps: &[Value] = item.raw["steps"].as_array().map_or(&[], Vec::as_slice);
        let done = steps
            .iter()
            .filter(|s| matches!(s["status"].as_str(), Some("completed" | "done")))
            .count();
        let label = raw_str(item, "title")
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if steps.is_empty() {
                    item.body.clone().unwrap_or_else(|| "Checklist".to_string())
                } else {
                    format!("Todo list — {done} of {} done", steps.len())
                }
            });
        let live = is_live(item);
        let can_expand =
            !steps.is_empty() || raw_str(item, "explanation").is_some() || item.body.is_some();
        let mut header = work_header(
            index,
            item,
            work_glyph("todo_list"),
            div().text_color(self.live_label_color(live)).child(label),
        );
        header = header
            .when_some(item_timestamp(item), |d, ts| d.child(ts))
            .child(if can_expand {
                work_chevron(expanded)
            } else {
                div().w(px(16.)).flex_shrink_0()
            });
        let mut col = div().flex().flex_col().child(header);

        if expanded {
            let mut detail = detail_text("todo", index);
            if let Some(explanation) = raw_str(item, "explanation") {
                detail = detail.child(
                    div()
                        .pb_1()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(explanation.to_string()),
                );
            }
            if steps.is_empty() {
                detail = detail.child(
                    div()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(item.body.clone().unwrap_or_else(|| "Checklist".to_string())),
                );
            }
            for step in steps {
                // Contract: pending | running | completed; accept provider
                // spellings like in_progress/done too.
                let status = step["status"].as_str().unwrap_or("pending");
                let (glyph, color, done) = match status {
                    "completed" | "done" => ("✓", success(), true),
                    "running" | "in_progress" | "in-progress" => ("◐", theme::primary(), false),
                    _ => ("○", theme::fg_muted(), false),
                };
                let text = step["text"]
                    .as_str()
                    .or_else(|| step["content"].as_str())
                    .or_else(|| step["title"].as_str())
                    .unwrap_or("step");
                detail = detail.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_xs().text_color(color).child(glyph))
                        .child(
                            div()
                                .text_sm()
                                .when(done, |d| d.text_color(theme::fg_muted()))
                                .when(!done, |d| d.text_color(theme::secondary_label()))
                                .child(text.to_string()),
                        ),
                );
            }
            col = col.child(detail);
        }
        col.into_any_element()
    }

    /// `system_notice`/`notification` — a non-interactive work-log line:
    /// info icon + `text-secondary-label` (destructive on failure).
    fn notice_row(&self, item: &TurnItemView) -> AnyElement {
        let failed = item.status == "failed" || item.raw["outcome"].as_str() == Some("failed");
        let text = raw_str(item, "summary")
            .or_else(|| raw_str(item, "message"))
            .or_else(|| raw_str(item, "text"))
            .map(str::to_owned)
            .or_else(|| item.body.clone())
            .unwrap_or_else(|| item.kind.clone());
        work_row_line()
            .group("timeline-row")
            .child(work_icon("ℹ", failed))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if failed {
                        danger()
                    } else {
                        theme::secondary_label()
                    })
                    .child(text),
            )
            .when_some(item_timestamp(item), |d, ts| d.child(ts))
            .into_any_element()
    }

    fn error_row(&self, item: &TurnItemView) -> AnyElement {
        let failure = &item.raw["failure"];
        // Provider usage-limit failures (`failure.class: "usage_limit"`)
        // render as the web's usage-limit banner — warning tone, not error.
        let usage_limit =
            item.status != "completed" && failure["class"].as_str() == Some("usage_limit");
        let title = if usage_limit {
            "Usage limit reached".to_string()
        } else {
            raw_str(item, "title")
                .map(str::to_owned)
                .or_else(|| failure["class"].as_str().map(str::to_owned))
                .unwrap_or_else(|| "Error".to_string())
        };
        let mut message = failure["message"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| item.body.clone())
            .unwrap_or_else(|| title.clone());
        if usage_limit {
            if let Some(reset_at) = failure["resetAt"].as_str() {
                // "2026-11-17T20:22:33.666Z" → "2026-11-17 20:22 UTC".
                let short = reset_at.get(..16).unwrap_or(reset_at).replace('T', " ");
                message = format!("{message} Retry after {short} UTC.");
            }
        }
        let code = failure["code"].as_str().map(str::to_owned);
        if !usage_limit {
            // Plain provider errors are work entries with the destructive
            // row style — ⚠ icon + `text-destructive` heading, no card.
            return work_row_line()
                .group("timeline-row")
                .child(work_icon("⚠", true))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .child(
                            div()
                                .truncate()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(danger())
                                .child(if message.is_empty() {
                                    title.clone()
                                } else {
                                    format!("{title}: {message}")
                                }),
                        )
                        .when_some(code, |d, code| {
                            d.child(
                                div()
                                    .text_xs()
                                    .flex_shrink_0()
                                    .text_color(theme::fg_muted())
                                    .child(code),
                            )
                        }),
                )
                .when_some(item_timestamp(item), |d, ts| d.child(ts))
                .into_any_element();
        }
        // Usage-limit failures keep the warning-toned banner.
        div()
            .rounded_lg()
            .border_1()
            .border_color(warning_border())
            .bg(rgba(0xe8b45a14))
            .px_3()
            .py_2()
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
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(warning())
                            .child(title),
                    )
                    .when_some(code, |d, code| {
                        d.child(div().text_xs().text_color(theme::fg_muted()).child(code))
                    }),
            )
            .child(div().text_xs().text_color(theme::fg()).child(message))
            .into_any_element()
    }

    fn proposed_plan_row(&self, item: &TurnItemView) -> AnyElement {
        let markdown = raw_str(item, "markdown")
            .or(item.body.as_deref())
            .unwrap_or_default();
        let actionable = self
            .state
            .selected_thread()
            .is_some_and(|thread| thread.has_actionable_proposed_plan);
        // `ProposedPlanCard`: `rounded-2xl border border-border/80
        // bg-card/70 p-4` — "Plan" badge + title header, markdown at mt-4.
        let mut card = div()
            .rounded(theme::radius::XL2)
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .p_4()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .bg(theme::muted())
                            .text_xs()
                            .text_color(theme::secondary_label())
                            .child("Plan"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::fg())
                            .child("Proposed plan"),
                    ),
            )
            .child(
                div()
                    .mt_4()
                    .flex()
                    .flex_col()
                    .children(crate::markdown::render_markdown(markdown)),
            );
        if actionable {
            card = card.child(
                div()
                    .pt_1()
                    .mt_1()
                    .border_t_1()
                    .border_color(theme::border())
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(warning())
                            .child("Plan awaiting your decision"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("— accept or request edits in the composer."),
                    ),
            );
        }
        card.into_any_element()
    }

    fn subagent_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        let title = raw_str(item, "title").unwrap_or("Subagent");
        let expanded = self.timeline_state.expanded.contains(&item.id);
        let status_label = match item.status.as_str() {
            "pending" => "queued",
            "running" => "running",
            "waiting" => "waiting",
            "completed" => "done",
            "failed" => "failed",
            "cancelled" | "interrupted" => "stopped",
            _ => "",
        };

        let live = is_live(item);
        let mut header = work_header(
            index,
            item,
            work_glyph("subagent"),
            div()
                .text_color(self.live_label_color(live))
                .child(if status_label.is_empty() {
                    title.to_string()
                } else {
                    format!("{title} · {status_label}")
                }),
        );
        header = header
            .when_some(item_timestamp(item), |d, ts| d.child(ts))
            .child(work_chevron(expanded));

        let mut col = div().flex().flex_col().child(header);

        // Delegated-task approval routing: an app-owned subagent's request
        // belongs to its child thread, not the parent — the child's own shell
        // row carries `has_pending_request`. Surface a jump chip here like
        // the web's `pendingSubagentRequest` flag on the parent thread.
        let child_thread_id = raw_str(item, "childThreadId").map(str::to_owned);
        let child_pending = child_thread_id.as_deref().is_some_and(|cid| {
            self.state
                .threads
                .iter()
                .any(|t| t.id.as_ref() == cid && t.has_pending_request)
        });
        if let Some(cid) = &child_thread_id {
            if child_pending || item.status == "waiting" {
                let cid = cid.clone();
                col = col.child(
                    div()
                        .id(("subagent-pending", index))
                        .flex()
                        .items_center()
                        .gap_2()
                        .ml(px(28.))
                        .mb_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(warning_border())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(move |_event, window, cx| {
                            let cid = cid.clone();
                            update_view(window, cx, move |this, cx| {
                                this.select_thread(cid.into(), cx);
                            });
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(warning())
                                .flex_shrink_0()
                                .child("⚠"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::fg())
                                .child("Approval needed in delegated task — open thread"),
                        ),
                );
            }
        }

        if expanded {
            let body = raw_str(item, "result")
                .or_else(|| raw_str(item, "progress"))
                .or_else(|| raw_str(item, "prompt"))
                .unwrap_or("No detail.")
                .to_string();
            col = col.child(detail_panel("subagent", index).child(div().child(body)));
        }
        col.into_any_element()
    }

    fn search_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        let expanded = self.timeline_state.expanded.contains(&item.id);
        let (label, query) = if item.kind == "web_search" {
            let patterns = item.raw["patterns"]
                .as_array()
                .map(|patterns| {
                    patterns
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            ("Web search", patterns)
        } else {
            (
                "File search",
                raw_str(item, "pattern")
                    .map(str::to_owned)
                    .unwrap_or_default(),
            )
        };

        let live = is_live(item);
        let can_expand = item.raw["results"].as_array().is_some();
        let text = if query.is_empty() {
            label.to_string()
        } else {
            format!("{label}  {query}")
        };
        let mut header = work_header(
            index,
            item,
            work_glyph(item.kind.as_str()),
            div().text_color(self.live_label_color(live)).child(text),
        );
        header = header
            .when_some(item_timestamp(item), |d, ts| d.child(ts))
            .child(if can_expand {
                work_chevron(expanded)
            } else {
                div().w(px(16.)).flex_shrink_0()
            });

        let mut col = div().flex().flex_col().child(header);

        if expanded {
            let mut detail = detail_text("search", index);
            let results: &[Value] = item.raw["results"].as_array().map_or(&[], Vec::as_slice);
            if results.is_empty() {
                detail = detail.child(
                    div()
                        .italic()
                        .text_color(theme::fg_muted())
                        .child("No results."),
                );
            }
            for result in results.iter().take(20) {
                let name = result["url"]
                    .as_str()
                    .or_else(|| result["fileName"].as_str())
                    .unwrap_or("result");
                let snippet = result["snippet"]
                    .as_str()
                    .or_else(|| result["preview"].as_str())
                    .map(|s| s.lines().next().unwrap_or_default().to_string());
                let mut row = div()
                    .pb_1()
                    .flex()
                    .flex_col()
                    .child(div().text_color(theme::fg()).child(name.to_string()));
                if let Some(snippet) = snippet {
                    row = row.child(
                        div()
                            .line_clamp(2)
                            .text_color(theme::fg_muted())
                            .child(snippet),
                    );
                }
                detail = detail.child(row);
            }
            if results.len() > 20 {
                detail = detail.child(
                    div()
                        .text_color(theme::fg_muted())
                        .child(format!("…{} more", results.len() - 20)),
                );
            }
            col = col.child(detail);
        }
        col.into_any_element()
    }

    /// Lifecycle rows and unknown kinds — `TimelineSystemDivider`: centered
    /// `text-2xs text-muted-foreground` with `h-px flex-1` rules on each side
    /// and `py-2` vertical spacing.
    fn fallback_row(&self, item: &TurnItemView) -> AnyElement {
        let text = match item.kind.as_str() {
            "checkpoint" => {
                let files = item.raw["files"].as_array().map_or(0, Vec::len);
                if files > 0 {
                    format!("Checkpoint · {files} files")
                } else {
                    "Checkpoint".to_string()
                }
            }
            "run_interrupt_request" => raw_str(item, "message")
                .map(|m| format!("Interrupting run — {m}"))
                .unwrap_or_else(|| "Interrupting run…".to_string()),
            "run_interrupt_result" => raw_str(item, "message")
                .map(|m| format!("Run interrupted — {m}"))
                .unwrap_or_else(|| "Run interrupted".to_string()),
            "handoff" => raw_str(item, "summary")
                .map(str::to_owned)
                .unwrap_or_else(|| "Handoff".to_string()),
            "fork" => "Forked conversation".to_string(),
            "thread_created" => raw_str(item, "title")
                .map(|t| format!("Created thread · {t}"))
                .unwrap_or_else(|| "Created thread".to_string()),
            "link_request" => raw_str(item, "reason")
                .map(|r| format!("Link request — {r}"))
                .unwrap_or_else(|| "Link request".to_string()),
            "secret_request" => raw_str(item, "label")
                .map(|l| format!("Secret request — {l}"))
                .unwrap_or_else(|| "Secret request".to_string()),
            _ => item
                .body
                .clone()
                .or_else(|| item.title.clone())
                .map(|text| format!("{}  {text}", item.kind))
                .unwrap_or_else(|| item.kind.clone()),
        };
        let mut row = div()
            .flex()
            .items_center()
            .gap_3()
            .py_2()
            .when(item.streaming, |d| d.opacity(0.75))
            .child(div().flex_1().h(px(1.)).bg(theme::hairline()))
            .child(
                div()
                    .text_size(theme::text_size::XS_2)
                    .text_color(theme::fg_muted())
                    .truncate()
                    .max_w(px(720.))
                    .child(text),
            );
        if is_live(item) {
            row = row.child(div().text_xs().text_color(theme::primary()).child("…"));
        }
        row = row.child(div().flex_1().h(px(1.)).bg(theme::hairline()));
        row.into_any_element()
    }
}
