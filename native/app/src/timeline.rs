//! Message timeline: turn item rendering — tool calls, diffs, approvals,
//! user/assistant styling. Owns `impl AppView` timeline render methods and
//! `timeline::State`.
//!
//! Field names come from `OrchestrationV2TurnItem` in
//! `packages/contracts/src/orchestrationV2.ts` (the complete payload arrives
//! on `TurnItemView::raw`); `item.status` is one of `OrchestrationV2TurnItemStatus`
//! (idle, pending, running, waiting, completed, failed, cancelled, interrupted).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use chrono::{DateTime, Datelike, Local};
use gpui::{
    AnyElement, ClipboardItem, Entity, EntityInputHandler, Focusable, FontFeatures, Rgba,
    Subscription, div, prelude::*, px,
};
use serde_json::{Value, json};

use crate::icons::{self, Icon};
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
    /// Fenced-code blocks the user unwrapped (markdown block key → nowrap).
    /// Absent = wrapped, the web's default `data-wrap`.
    pub code_unwrapped: HashSet<u64>,
    /// Fenced-code block key whose header Copy action last ran.
    pub code_copied: Option<u64>,
    /// Checkpoint card directory expansion: checkpoint item id → state.
    pub checkpoint_dirs: HashMap<String, CheckpointDirState>,
    /// Checkpoint item id → turn-diff fetch for the inline "Open diff".
    pub checkpoint_diffs: HashMap<String, CheckpointDiff>,
}

/// Changed-files card directory expansion (web `allDirectoriesExpanded`
/// plus per-directory overrides), defaulting to collapsed.
#[derive(Default)]
pub struct CheckpointDirState {
    /// The expand/collapse-all baseline (web default `false`).
    pub all_expanded: bool,
    /// Directory paths flipped from `all_expanded` by a row click.
    pub overrides: HashSet<String>,
}

impl CheckpointDirState {
    fn is_expanded(&self, path: &str) -> bool {
        self.overrides.contains(path) != self.all_expanded
    }
}

/// A `orchestration.getTurnDiff` result for the changed-files card.
pub enum CheckpointDiff {
    Loading,
    Loaded(String),
    Error(String),
}

/// `text-success` (`--success`) — approved outcomes, "Answered", +N counts.
fn success() -> Rgba {
    theme::success()
}

/// `text-destructive` (`--destructive`) — failure rows and destructive marks.
fn danger() -> Rgba {
    theme::error()
}

/// `text-warning` (`--warning`) — "Waiting for approval", usage-limit rows.
fn warning() -> Rgba {
    theme::warning()
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

/// The lucide icon the web maps each entry kind to (`workEntryIconName` in
/// TimelineWorkEntryRow.logic.ts → `WorkEntryIcon`). Where the web's glyph
/// isn't bundled, the closest lucide substitute is noted in the comment.
fn work_entry_icon(kind: &str) -> Icon {
    match kind {
        // terminal
        "command_execution" => Icon::Terminal,
        // square-pen → closest bundled pen
        "file_change" => Icon::Pencil,
        // brain → closest bundled "AI thinking" glyph
        "reasoning" => Icon::Sparkles,
        // bot → closest bundled
        "subagent" => Icon::Users,
        // globe
        "web_search" => Icon::Globe,
        // search
        "file_search" => Icon::Search,
        // list-todo
        "todo_list" => Icon::ListTodo,
        // message-circle → closest bundled
        "approval_request" | "user_input_request" => Icon::MessageSquare,
        // wrench (and the `hammer` fallback)
        _ => Icon::Wrench,
    }
}

/// `system_notice`/`notification` icon — mirrors `workEntryIconName`:
/// a notification resolves from `source.kind` (failed → circle-alert);
/// a plain system notice has `info` tone → the check icon.
fn notice_icon(item: &TurnItemView, failed: bool) -> Icon {
    if failed {
        return Icon::CircleAlert;
    }
    if item.kind == "notification" {
        return match item.raw["source"]["kind"].as_str().unwrap_or_default() {
            // bot
            "subagent" | "delegated_task" => Icon::Users,
            "command" => Icon::Terminal,
            "monitor" => Icon::Eye,
            // t3-code wordmark → closest bundled
            "system" => Icon::Sparkles,
            // background_task / unknown
            _ => Icon::Zap,
        };
    }
    Icon::Check
}

/// `approval_request`/`user_input_request` icon per `requestKind` — mirrors
/// `APPROVAL_PRESENTATION` in TimelinePendingApprovalRow.tsx.
fn approval_icon(kind: &str) -> Icon {
    match kind {
        // terminal
        "command" => Icon::Terminal,
        // square-pen
        "file-change" => Icon::Pencil,
        // eye
        "file-read" => Icon::Eye,
        // wrench
        "permission" => Icon::Wrench,
        // hammer ("App access") → closest bundled access glyph
        "mcp-elicitation" => Icon::Shield,
        _ => Icon::MessageSquare,
    }
}

/// The `w-6` icon slot: a `size-4` (16-px) lucide icon centered in 24 px.
/// Work rows render it `text-icon-muted` at `muted` opacity (the web's
/// `opacity-70 light:brightness-60`); `dim: false` skips the fade for
/// full-strength destructive/warning icons.
fn glyph_slot(icon: Icon, color: Rgba, dim: bool) -> gpui::Div {
    div()
        .w(px(24.))
        .h(px(24.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .when(dim, |d| d.opacity(0.7))
        .child(icons::icon(icon, 16.).text_color(color))
}

fn work_icon(icon: Icon, failed: bool) -> gpui::Div {
    // `text-icon-muted` normally; failed calls tint it
    // `text-tool-error-icon/40` like `failedToolIconClassName` does.
    glyph_slot(
        icon,
        if failed {
            theme::tool_error_icon()
        } else {
            theme::icon_muted()
        },
        true,
    )
}

/// Trailing `size-3` chevron at `text-icon-muted opacity-70` inside a size-4
/// slot — ChevronRight collapsed, ChevronDown expanded (the web rotates the
/// right chevron 90°; a down chevron is the same affordance without a
/// transform).
fn work_chevron(expanded: bool) -> gpui::Div {
    div()
        .w(px(16.))
        .h(px(24.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .opacity(0.7)
        .child(
            icons::icon(
                if expanded {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                12.,
            )
            .text_color(theme::icon_muted()),
        )
}

/// `tabular-nums` — enabled on timestamps and the working timer like the
/// web's `tabular-nums` class.
fn tabular_nums() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

/// `h:mm AM` in the viewer's local zone — matches the web's en-US
/// `Intl.DateTimeFormat` output for the default 12-hour timestamp format.
fn hhmm_ampm(dt: &DateTime<Local>) -> String {
    format!("{} {}", dt.format("%-I:%M"), dt.format("%p"))
}

/// `formatDayAwareTimestamp` — `h:mm AM` today, `yesterday at h:mm AM`,
/// `M/D h:mm AM` this year, `M/D/YYYY h:mm AM` older.
fn day_aware_label(dt: &DateTime<Local>) -> String {
    let now = Local::now();
    let hhmm = hhmm_ampm(dt);
    let days = now.date_naive() - dt.date_naive();
    match days.num_days() {
        d if d <= 0 => hhmm,
        1 => format!("yesterday at {hhmm}"),
        _ if dt.year() == now.year() => format!("{} {hhmm}", dt.format("%-m/%-d")),
        _ => format!("{} {hhmm}", dt.format("%-m/%-d/%Y")),
    }
}

/// `formatUpcomingTimestamp` — the forward-looking twin: `h:mm AM` today,
/// `tomorrow at h:mm AM`, `M/D h:mm AM` / `M/D/YYYY h:mm AM` further out;
/// past stamps fall back to `day_aware_label`. The usage-limit row appends
/// the local zone name (`(UTC)`), like the web's `Intl` `timeZone` output.
fn upcoming_label(iso: &str) -> Option<String> {
    let dt = DateTime::parse_from_rfc3339(iso).ok()?.with_timezone(&Local);
    let now = Local::now();
    let hhmm = hhmm_ampm(&dt);
    let days = dt.date_naive() - now.date_naive();
    let label = match days.num_days() {
        d if d < 0 => day_aware_label(&dt),
        0 => hhmm,
        1 => format!("tomorrow at {hhmm}"),
        _ if dt.year() == now.year() => format!("{} {hhmm}", dt.format("%-m/%-d")),
        _ => format!("{} {hhmm}", dt.format("%-m/%-d/%Y")),
    };
    let tz = Local::now().format("%Z").to_string();
    Some(if tz.is_empty() {
        label
    } else {
        format!("{label} ({tz})")
    })
}

/// `TimelineRowTimestamp` — `text-xs text-muted-foreground tabular-nums`,
/// `opacity-0` until the `.group("timeline-row")` ancestor is hovered unless
/// `always_visible` (the web's `alwaysVisible`, used on error rows and the
/// under-bubble meta).
fn timestamp_div(iso: &str, always_visible: bool, hover_group: &'static str) -> Option<gpui::Div> {
    let dt = DateTime::parse_from_rfc3339(iso)
        .ok()?
        .with_timezone(&Local);
    let label = day_aware_label(&dt);
    // The enclosing row's `.group(...)` gates the reveal: "timeline-row" on
    // work/fold rows, "msg" on the under-bubble meta rows. A div holds only
    // one group-hover style, so the caller names its ancestor.
    Some(
        div()
            .flex_shrink_0()
            .text_xs()
            .font_features(tabular_nums())
            .text_color(theme::fg_muted())
            .when(!always_visible, |d| {
                d.opacity(0.).group_hover(hover_group, |s| s.opacity(1.))
            })
            .child(label),
    )
}

/// Item timestamps read `updatedAt` (the web passes `createdAt`, which the
/// wire carries as `updatedAt` on our `TurnItemView`s).
fn item_timestamp(
    item: &TurnItemView,
    always_visible: bool,
    hover_group: &'static str,
) -> Option<gpui::Div> {
    timestamp_div(raw_str(item, "updatedAt")?, always_visible, hover_group)
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
    icon: Icon,
    label: gpui::Div,
) -> gpui::Stateful<gpui::Div> {
    let failed = item.status == "failed";
    work_row_line()
        .id(("head", index))
        .group("timeline-row")
        .cursor_pointer()
        .on_click(toggle_expanded(item.id.clone()))
        .child(work_icon(icon, failed))
        .child(label.flex_1().min_w_0().truncate())
}

/// `text-tool-error-icon/40` `size-3` XIcon — the web's failed-indicator
/// mark on tool-ish work rows.
fn failed_x() -> gpui::Div {
    div()
        .flex_shrink_0()
        .child(icons::icon(Icon::X, 12.).text_color(theme::tool_error_icon()))
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

// ── Turn folds (web `deriveTurnFolds` → "Worked for 1m 17s ›") ──────
//
// Once a turn settles, its work entries (and earlier assistant narration)
// collapse behind one bordered row; the terminal assistant message stays
// visible. Compactions, checkpoint cards, notices, errors and answered
// user-input cards never fold, and a segment containing live items is
// left alone.

/// Kinds that may hide inside a turn fold (the web's `foldedWorkEntry`).
fn is_fold_work_kind(kind: &str) -> bool {
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
    )
}

struct TurnFold {
    /// `expanded` key — `fold:{boundary user_message id}`.
    key: String,
    label: String,
    /// The boundary message's `createdAt` — the fold row's hover timestamp.
    created_at: Option<String>,
}

/// One row in the rendered timeline after fold collapsing.
enum TimelineRow {
    Item(usize),
    Fold {
        key: String,
        label: String,
        created_at: Option<String>,
        anchor: usize,
        open: bool,
    },
}

/// Map `item index → fold key` for every foldable item; the anchor row
/// renders at the smallest index of each fold's set.
fn derive_turn_folds(items: &[TurnItemView]) -> (HashMap<usize, TurnFold>, HashMap<usize, String>) {
    let mut anchors: HashMap<usize, TurnFold> = HashMap::new();
    let mut hidden: HashMap<usize, String> = HashMap::new();
    // A turn segment runs from one user message to the next — the web keys
    // groups by runId, which the boundary message stands in for here.
    let bounds: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| it.kind == "user_message")
        .map(|(i, _)| i)
        .collect();
    for (bi, &b) in bounds.iter().enumerate() {
        let end = bounds.get(bi + 1).copied().unwrap_or(items.len());
        let seg = (b + 1)..end;
        if items[seg.clone()].iter().any(is_live) {
            continue;
        }
        // The terminal assistant message (the turn's final reply) stays.
        let terminal = items[seg.clone()]
            .iter()
            .rposition(|it| it.kind == "assistant_message")
            .map(|p| seg.start + p);
        let mut members: Vec<usize> = Vec::new();
        for i in seg.clone() {
            let it = &items[i];
            let k = it.kind.as_str();
            let foldable = if k == "assistant_message" {
                Some(i) != terminal
            } else if is_fold_work_kind(k) {
                // Answered user-input cards stay (web `questionAnswer`).
                !(k == "user_input_request" && !it.raw["questionAnswer"].is_null())
            } else {
                false
            };
            if !foldable {
                continue;
            }
            // Work trailing the final reply folds only when it settled
            // cleanly — failures after the reply stay visible (web
            // `isFoldableTrailingActivity`).
            if terminal.is_some_and(|t| i > t) && it.status == "failed" {
                continue;
            }
            members.push(i);
        }
        if members.is_empty() {
            continue;
        }
        // Duration: boundary message → the segment's newest timestamp, in
        // the working-timer's "Ns" / "Hm Ss" / "Hh Mm Ss" form.
        fn iso<'a>(it: &'a TurnItemView, keys: [&str; 2]) -> Option<&'a str> {
            keys.iter().find_map(|k| raw_str(it, k))
        }
        let start_iso = iso(&items[b], ["createdAt", "updatedAt"]);
        let end_iso = items[seg.clone()]
            .iter()
            .rev()
            .find_map(|it| iso(it, ["updatedAt", "createdAt"]));
        let secs = match (start_iso, end_iso) {
            (Some(a), Some(z)) => DateTime::parse_from_rfc3339(a)
                .ok()
                .zip(DateTime::parse_from_rfc3339(z).ok())
                .map(|(a, z)| (z - a).num_seconds().max(0) as u64),
            _ => None,
        };
        let stopped = items[seg.clone()]
            .iter()
            .any(|it| it.status == "interrupted");
        let label = match (stopped, secs.map(format_work_duration)) {
            (true, Some(d)) => format!("You stopped after {d}"),
            (true, None) => "You stopped this response".to_string(),
            (false, Some(d)) => format!("Worked for {d}"),
            (false, None) => "Worked".to_string(),
        };
        let key = format!("fold:{}", items[b].id);
        for &i in &members {
            hidden.insert(i, key.clone());
        }
        anchors.insert(
            members[0],
            TurnFold {
                key,
                label,
                created_at: iso(&items[b], ["createdAt", "updatedAt"]).map(str::to_owned),
            },
        );
    }
    (anchors, hidden)
}

/// `formatDuration` — "Ns" under a minute, then "Hm Ss" / "Hh Mm Ss" with
/// zero parts dropped (never zero-padded). Shared by the working timer and
/// turn-fold labels.
fn format_work_duration(elapsed: u64) -> String {
    let (h, m, s) = (elapsed / 3600, (elapsed % 3600) / 60, elapsed % 60);
    let mut parts = Vec::new();
    if elapsed >= 60 {
        if h > 0 {
            parts.push(format!("{h}h"));
        }
        if m > 0 {
            parts.push(format!("{m}m"));
        }
    }
    if elapsed < 60 || s > 0 || m > 0 {
        parts.push(format!("{s}s"));
    }
    parts.join(" ")
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

/// Ghost icon button — the web's `ghost-muted` icon-xs Button: a `size-6`
/// rounded target with a `size-3` (12-px) muted icon, `bg-accent` +
/// `text-foreground` on hover, revealed on row hover (callers may set a
/// starting opacity for "already sent" states).
fn ghost_icon_chip(id: impl Into<gpui::ElementId>, icon: Icon) -> gpui::Stateful<gpui::Div> {
    ghost_icon_chip_colored(id, icon, theme::fg_muted(), theme::fg())
}

/// `icon_color` — resting glyph ink; `hover_color` — the glyph ink under the
/// chip's `bg-accent` hover (kept distinct so the primary "copied" check
/// stays primary on hover like the web's `CheckIcon text-primary`).
fn ghost_icon_chip_colored(
    id: impl Into<gpui::ElementId>,
    icon: Icon,
    icon_color: Rgba,
    hover_color: Rgba,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(24.))
        .h(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .cursor_pointer()
        .hover(move |s| s.bg(theme::accent()).text_color(hover_color))
        .child(icons::icon(icon, 12.).text_color(icon_color))
}

/// "Copy" ghost chip revealed when its `.group("msg")` ancestor is hovered;
/// writes `text` to the clipboard and flips to the web's primary check.
fn copy_chip(
    key: &'static str,
    index: usize,
    copied: bool,
    item_id: String,
    text: String,
) -> gpui::Stateful<gpui::Div> {
    let (icon, color, hover_color) = if copied {
        // `CheckIcon size-3 text-primary` — stays primary under the hover bg.
        (Icon::Check, theme::primary(), theme::primary())
    } else {
        (Icon::Copy, theme::fg_muted(), theme::fg())
    };
    ghost_icon_chip_colored((key, index), icon, color, hover_color).on_click(
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

/// The web's `ContextChip kind="citation"`: an inline `font-medium` span in
/// the `--context-chip-citation` accent — a `size-[1em]` QuoteIcon plus the
/// label (comment or quoted text), which underlines on hover. Clicking
/// navigates to the cited thread — the web routes to the source thread and
/// reveals the quoted range; the native timeline can only land on the thread.
fn citation_chip(
    index: usize,
    ci: usize,
    citation: &AssistantCitation,
) -> gpui::Stateful<gpui::Div> {
    let label = citation_label(citation);
    let thread_id = citation.thread_id.clone();
    div()
        .id(("cite", index * 64 + ci))
        .flex()
        .items_center()
        .gap(px(3.)) // gap-[0.2em] at text-sm ≈ 3px
        .cursor_pointer()
        .text_sm()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::citation_accent())
        .on_click(move |_event, window, cx| {
            let thread_id = thread_id.clone();
            update_view(window, cx, move |this, cx| {
                this.select_thread(thread_id.into(), cx);
            });
        })
        .child(
            div()
                .flex_shrink_0()
                .child(icons::icon(Icon::Quote, 14.).text_color(theme::citation_accent())),
        )
        .child(
            div()
                .truncate()
                .max_w(px(224.)) // max-w-[16em] at 14px
                .hover(|s| s.underline())
                .child(label),
        )
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

/// The web's `CheckboxMark variant="contrast"`: a `size-4` (16px) rounded-[4px]
/// box — `border-foreground/55` on the input surface; checked fills
/// `bg-primary` with a primary-foreground check.
fn checkbox_mark(checked: bool) -> gpui::Div {
    let mut box_ = div()
        .size(px(16.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .border_color(if checked {
            theme::primary()
        } else {
            theme::option_mark_border()
        });
    box_ = if checked {
        box_.bg(theme::primary())
            .child(icons::icon(Icon::Check, 12.).text_color(theme::primary_fg()))
    } else {
        box_.bg(theme::field_surface())
    };
    box_
}

/// The web's `OptionRadio`: a `size-4` circle — `border-foreground/55` on the
/// input surface; checked fills `bg-primary` with a `size-1.5`
/// primary-foreground center dot.
fn radio_mark(checked: bool) -> gpui::Div {
    let mut ring = div()
        .size(px(16.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(theme::option_mark_border())
        .bg(theme::field_surface());
    if checked {
        ring = ring.child(
            div()
                .size(px(14.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme::primary())
                .child(div().size(px(6.)).rounded_full().bg(theme::primary_fg())),
        );
    }
    ring
}

/// One selectable option row inside a pending user-input card — the web's
/// `group … gap-2.5 rounded-md py-1.5 ps-2` row: the CheckboxMark/OptionRadio
/// mark in a `h-5` slot, a `text-sm font-medium` label with the `text-2xs`
/// `text-secondary-label` description under it, `text-foreground/70` until
/// selected, `hover:bg-muted/30`. Clicking toggles the value in
/// `timeline_state.user_input_answers` — multi-select questions accumulate,
/// single-select replaces (matching `togglePendingUserInputOptionSelection`).
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
    let mark = if multi_select {
        checkbox_mark(selected)
    } else {
        radio_mark(selected)
    };
    let mut label_col = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(px(2.))
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(label.to_string()),
        );
    // The contract makes `description` required; the web hides it when it
    // just repeats the label.
    if !description.is_empty() && description != label {
        label_col = label_col.child(
            div()
                .text_size(theme::text_size::XS_2)
                .text_color(theme::secondary_label())
                .child(description.to_string()),
        );
    }
    div()
        .id((
            "ui-opt",
            item_index * 4096 + question_index * 64 + option_index,
        ))
        .flex()
        .items_start()
        .gap(px(10.)) // gap-2.5
        .px_2()
        .py_1p5()
        .rounded_md()
        .cursor_pointer()
        .text_color(if selected {
            theme::fg()
        } else {
            theme::fg_dim()
        })
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
        // The `flex h-5 items-center` slot keeps the mark on the label's
        // first line (text-sm at 22.75px leading ≈ h-5).
        .child(
            div()
                .h(px(20.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .child(mark),
        )
        .child(label_col)
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
        // `--diff-addition-foreground`
        theme::diff_add_fg()
    } else if line.starts_with('-') {
        // `--diff-deletion-foreground`
        theme::diff_del_fg()
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

// ── Changed-files tree (web `buildTurnDiffTree`) ─────────────────────
//
// Checkpoint `files` become a directory tree: dirs sort before files,
// everything name-sorted, and single-child directory chains compact into
// one `a/b` row.

enum FileTreeNode {
    Dir {
        name: String,
        path: String,
        additions: u64,
        deletions: u64,
        children: Vec<FileTreeNode>,
    },
    File {
        name: String,
        additions: Option<u64>,
        deletions: Option<u64>,
    },
}

#[derive(Default)]
struct DirBuilder {
    dirs: std::collections::BTreeMap<String, DirBuilder>,
    files: Vec<FileTreeNode>,
    additions: u64,
    deletions: u64,
}

impl DirBuilder {
    fn into_nodes(self, prefix: &str) -> Vec<FileTreeNode> {
        let mut out: Vec<FileTreeNode> = Vec::new();
        for (name, dir) in self.dirs {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            // Compact single-child directory chains into one row.
            let mut name = name;
            let mut path = path;
            let mut dir = dir;
            while dir.dirs.len() == 1 && dir.files.is_empty() {
                let Some((child_name, child)) = dir.dirs.pop_first() else {
                    break;
                };
                name = format!("{name}/{child_name}");
                path = format!("{path}/{child_name}");
                dir = child;
            }
            let additions = dir.additions;
            let deletions = dir.deletions;
            let children = dir.into_nodes(&path);
            out.push(FileTreeNode::Dir {
                name,
                path,
                additions,
                deletions,
                children,
            });
        }
        out.extend(self.files);
        // Case-insensitive name sort, directories first (approximates the
        // web's `Intl.Collator numeric/base` ordering).
        fn node_name(node: &FileTreeNode) -> &str {
            match node {
                FileTreeNode::Dir { name, .. } | FileTreeNode::File { name, .. } => name,
            }
        }
        out.sort_by(|a, b| {
            let rank = |n: &FileTreeNode| matches!(n, FileTreeNode::Dir { .. });
            rank(b).cmp(&rank(a)).then_with(|| {
                node_name(a)
                    .to_lowercase()
                    .cmp(&node_name(b).to_lowercase())
            })
        });
        out
    }
}

/// Stable element-id hash for tree paths (ids must survive re-renders).
fn fnv_node(s: &str) -> usize {
    let mut h = 0xcbf29ce484222325u64;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h & (u64::MAX >> 1)) as usize
}

fn build_file_tree(files: &[Value]) -> Vec<FileTreeNode> {
    let mut root = DirBuilder::default();
    for file in files {
        let Some(path) = file["path"].as_str() else {
            continue;
        };
        let segments: Vec<String> = path
            .split(['/', '\\'])
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let Some(name) = segments.last() else {
            continue;
        };
        let additions = file["additions"].as_u64();
        let deletions = file["deletions"].as_u64();
        // Each ancestor directory aggregates the file's stats.
        let mut dir = &mut root;
        dir.additions += additions.unwrap_or(0);
        dir.deletions += deletions.unwrap_or(0);
        for segment in &segments[..segments.len().saturating_sub(1)] {
            dir = dir.dirs.entry(segment.clone()).or_default();
            dir.additions += additions.unwrap_or(0);
            dir.deletions += deletions.unwrap_or(0);
        }
        dir.files.push(FileTreeNode::File {
            name: name.clone(),
            additions,
            deletions,
        });
    }
    root.into_nodes("")
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
        let list = |lane: gpui::Div| {
            div()
                .id("messages")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll_handle)
                .px(px(48.))
                .child(lane)
        };
        // Top/bottom spacers ≈ chat-list-header/footer (h-3 / h-4).
        lane = lane.child(div().h(px(12.)).flex_shrink_0());

        // Web `TimelineHistoryControl`: a `gap-1.5 pb-2` lane row whose
        // full-width button centers the label between `h-px flex-1
        // bg-border/70` hairlines; the error note sits below it.
        if self.has_more_history || self.history_error {
            lane = lane.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .pb_2()
                    .when(self.has_more_history, |col| {
                        col.child(
                            div()
                                .id("load-earlier-turns")
                                .w_full()
                                .flex()
                                .items_center()
                                .gap_3()
                                .py_1p5()
                                .cursor_pointer()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .hover(|s| s.text_color(theme::fg()))
                                .child(
                                    div().h(px(1.)).flex_1().bg(theme::hairline().opacity(0.7)),
                                )
                                .child(
                                    div().flex_none().child(if self.history_loading {
                                        "Loading earlier turns…"
                                    } else {
                                        "Load earlier turns"
                                    }),
                                )
                                .child(
                                    div().h(px(1.)).flex_1().bg(theme::hairline().opacity(0.7)),
                                )
                                .on_click(|_event, window, cx| {
                                    update_view(window, cx, |this, cx| {
                                        this.load_earlier_turns(cx);
                                        cx.notify();
                                    });
                                }),
                        )
                    })
                    .when(self.history_error, |col| {
                        col.child(
                            div()
                                .flex()
                                .justify_center()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child("Couldn't load earlier turns. Try again."),
                        )
                    }),
            );
        }

        // The web's empty-thread placeholder is a standalone `h-full` flex
        // that centers the text in the viewport (with bottom padding for the
        // composer inset), not a row inside the scroll lane.
        let empty_state = || -> gpui::Stateful<gpui::Div> {
            div()
                .id("messages-empty")
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .pb(px(90.))
                .text_color(theme::fg_muted())
                .text_sm()
                .child("Send a message to start the conversation. ")
        };

        if self.state.connection != ConnectionStatus::Mock {
            if self.thread_items.is_empty() {
                if self.turn_started_at.is_some() {
                    lane = lane.child(self.render_working_row());
                    return list(lane);
                }
                return empty_state();
            }
            // Turn folds: settled work between two user messages collapses
            // behind a "Worked for …" row; expanding it unhides the items in
            // their original positions.
            let (fold_anchors, fold_hidden) = derive_turn_folds(&self.thread_items);
            let mut rows: Vec<TimelineRow> = Vec::new();
            for i in 0..self.thread_items.len() {
                if let Some(fold) = fold_anchors.get(&i) {
                    let open = self.timeline_state.expanded.contains(&fold.key);
                    rows.push(TimelineRow::Fold {
                        key: fold.key.clone(),
                        label: fold.label.clone(),
                        created_at: fold.created_at.clone(),
                        anchor: i,
                        open,
                    });
                    if !open {
                        continue;
                    }
                } else if let Some(key) = fold_hidden.get(&i) {
                    if !self.timeline_state.expanded.contains(key) {
                        continue;
                    }
                }
                rows.push(TimelineRow::Item(i));
            }
            for (ri, row) in rows.iter().enumerate() {
                let next_is_work = match rows.get(ri + 1) {
                    Some(TimelineRow::Item(j)) => is_work_kind(&self.thread_items[*j].kind),
                    _ => false,
                };
                // Row rhythm: pb-4 under messages/plans, pb-2 under work-log
                // entries and events; consecutive work entries touch (the
                // web groups them at py-0 inside group-content).
                let (pad, element) = match row {
                    TimelineRow::Fold {
                        key,
                        label,
                        created_at,
                        anchor,
                        open,
                    } => (
                        6.,
                        self.turn_fold_row(key, label, created_at.as_deref(), *anchor, *open),
                    ),
                    TimelineRow::Item(i) => {
                        let item = &self.thread_items[*i];
                        let pad = if matches!(
                            item.kind.as_str(),
                            "user_message" | "assistant_message" | "proposed_plan" | "checkpoint"
                        ) {
                            16.
                        } else if is_work_kind(&item.kind) && next_is_work {
                            0.
                        } else {
                            8.
                        };
                        (pad, self.render_turn_item(*i, item))
                    }
                };
                lane = lane.child(
                    div()
                        .flex()
                        .flex_col()
                        .max_w_full()
                        .pb(px(pad))
                        .child(element),
                );
            }
            if self.turn_started_at.is_some() {
                lane = lane.child(self.render_working_row());
            }
            lane = lane.child(div().h(px(16.)).flex_shrink_0());
            return list(lane);
        }

        if self.state.messages.is_empty() {
            return empty_state();
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
        list(lane)
    }

    /// `WorkingTimelineRow`: `border-b border-border/60 pb-2 pt-1` around a
    /// `h-6 items-baseline gap-2 px-1 text-sm text-muted-foreground
    /// tabular-nums` "Working for Ns" line.
    fn render_working_row(&self) -> impl IntoElement {
        let elapsed = self
            .turn_started_at
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        let label = format_work_duration(elapsed);
        div()
            .border_b_1()
            .border_color(theme::table_rule())
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
                    .font_features(tabular_nums())
                    .text_color(theme::fg_muted())
                    .child(format!("Working for {label}")),
            )
    }

    /// `TurnFoldRow` — the same `border-b border-border/60 pb-2 pt-1` row as
    /// the working timer, but a button: "Worked for 1m 17s" plus a
    /// `size-3.5` chevron that toggles the turn's hidden work entries; a
    /// `ms-auto` `TimelineRowTimestamp` reveals on row hover.
    fn turn_fold_row(
        &self,
        key: &str,
        label: &str,
        created_at: Option<&str>,
        anchor: usize,
        open: bool,
    ) -> AnyElement {
        div()
            .id(("fold", anchor))
            .group("timeline-row")
            .border_b_1()
            .border_color(theme::table_rule())
            .pb_2()
            .pr(px(2.))
            .pt_1()
            .cursor_pointer()
            .text_color(theme::fg_muted())
            .hover(|s| s.text_color(theme::fg()))
            .on_click(toggle_expanded(key.to_string()))
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .text_sm()
                    .line_height(px(22.75))
                    .font_features(tabular_nums())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_1()
                            .rounded_md()
                            .child(label.to_string())
                            .child(
                                div().flex_shrink_0().child(
                                    icons::icon(
                                        if open {
                                            Icon::ChevronDown
                                        } else {
                                            Icon::ChevronRight
                                        },
                                        14.,
                                    )
                                    .text_color(theme::fg_muted()),
                                ),
                            ),
                    )
                    .when_some(created_at.and_then(|iso| timestamp_div(iso, false, "timeline-row")), |d, ts| {
                        d.child(ts.ml_auto())
                    }),
            )
            .into_any_element()
    }

    /// Per-code-block state + click wiring for `render_markdown_ctx` —
    /// wrap toggle and copy are stored on `timeline_state` keyed by the
    /// block's content hash so they survive re-renders.
    fn markdown_code_ui(&self, key: u64) -> crate::markdown::CodeBlockUi {
        crate::markdown::CodeBlockUi {
            wrapped: !self.timeline_state.code_unwrapped.contains(&key),
            copied: self.timeline_state.code_copied == Some(key),
            on_toggle_wrap: Some(Rc::new(move |_event, window, cx| {
                update_view(window, cx, move |this, cx| {
                    if !this.timeline_state.code_unwrapped.remove(&key) {
                        this.timeline_state.code_unwrapped.insert(key);
                    }
                    cx.notify();
                });
            })),
            on_copied: Some(Rc::new(move |_event, window, cx| {
                update_view(window, cx, move |this, cx| {
                    this.timeline_state.code_copied = Some(key);
                    cx.notify();
                });
            })),
        }
    }

    fn render_md(&self, text: &str) -> Vec<AnyElement> {
        crate::markdown::render_markdown_ctx(text, &mut |key| self.markdown_code_ui(key))
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
            "checkpoint" => self.checkpoint_row(index, item),
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
            .children(self.render_md(&text));
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
        if let Some(ts) = item_timestamp(item, false, "msg") {
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
            .children(self.render_md(&text));
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
        // The item's own threadId is canonical; fall back to the selected
        // thread (this row only renders in it anyway).
        let thread_id = raw_str(item, "threadId")
            .map(str::to_owned)
            .or_else(|| self.state.selected.as_ref().map(|s| s.to_string()))
            .unwrap_or_default();
        // `AssistantForkButton` — a ghost GitFork chip on every completed
        // assistant message with a runId (web `canForkProjectedAssistantItem`
        // keeps it whenever capability evidence is absent).
        if !item.streaming
            && item.status == "completed"
            && let Some(run_id) = raw_str(item, "runId").map(str::to_owned)
        {
            let source = thread_id.clone();
            let title = self
                .state
                .selected_thread()
                .map(|t| format!("{} fork", t.title));
            buttons = buttons.child(
                ghost_icon_chip(("fork", index), Icon::GitFork).on_click(
                    move |_event, window, cx| {
                        let source = source.clone();
                        let run_id = run_id.clone();
                        let title = title.clone();
                        update_view(window, cx, move |this, cx| {
                            let target = uuid::Uuid::new_v4().to_string();
                            // Same payload as the web's `forkThreadFromRun`
                            // (`commands.ts`), then navigate to the new shell.
                            let mut command = json!({
                                "type": "thread.fork",
                                "commandId": uuid::Uuid::new_v4().to_string(),
                                "createdBy": "user",
                                "creationSource": "web",
                                "sourceThreadId": source,
                                "targetThreadId": target,
                                "sourcePoint": {"type": "run", "runId": run_id},
                            });
                            if let Some(title) = &title {
                                command["title"] = json!(title);
                            }
                            this.dispatch(command);
                            this.select_thread(target.into(), cx);
                        });
                    },
                ),
            );
        }
        // Hover feedback thumbs on every settled assistant message; a click
        // on a non-Codex thread explains provider.uploadFeedback isn't
        // available there (same message as the web's /feedback gate).
        if !item.streaming {
            let sent = self.timeline_state.feedback_sent.contains(&item.id);
            let busy = self.timeline_state.feedback_in_flight.contains(&item.id);
            for (up, icon, eid) in [
                (true, Icon::ThumbsUp, 0usize),
                (false, Icon::ThumbsDown, 1usize),
            ] {
                let tid = thread_id.clone();
                let iid = item.id.clone();
                let shown = if sent {
                    Icon::Check
                } else if busy {
                    Icon::LoaderCircle
                } else {
                    icon
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
            if let Some(ts) = item_timestamp(item, false, "msg") {
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
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(theme::fg_muted())
                    .child(icons::icon(Icon::ThumbsDown, 12.).text_color(theme::fg_muted())),
            )
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
                    .text_color(theme::primary_fg())
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
                    .cursor_pointer()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.text_color(theme::fg()))
                    .on_click(move |_event, window, cx| {
                        update_view(window, cx, move |this, cx| {
                            this.timeline_state.feedback_comment = None;
                            cx.notify();
                        });
                    })
                    .child(icons::icon(Icon::X, 12.).text_color(theme::fg_muted())),
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
            work_entry_icon("reasoning"),
            div()
                .text_color(self.live_label_color(live))
                .child(label_text),
        );
        header = header
            .when_some(item_timestamp(item, false, "timeline-row"), |d, ts| d.child(ts))
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
            work_entry_icon("command_execution"),
            div()
                .text_color(self.live_label_color(live))
                .child(format!("{verb} {input}")),
        );
        if failed {
            header = header.child(failed_x());
        }
        header = header
            .when_some(item_timestamp(item, false, "timeline-row"), |d, ts| d.child(ts))
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
            work_entry_icon("dynamic_tool"),
            div().text_color(self.live_label_color(live)).child(label),
        );
        if failed {
            header = header.child(failed_x());
        }
        header = header
            .when_some(item_timestamp(item, false, "timeline-row"), |d, ts| d.child(ts))
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
            work_entry_icon("file_change"),
            div()
                .text_color(self.live_label_color(live))
                .child(format!("{verb} {file_label}")),
        );
        header = header
            .child(stats)
            .when_some(item_timestamp(item, false, "timeline-row"), |d, ts| d.child(ts))
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
            .child(work_icon(approval_icon(kind), item.status == "failed"))
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
        header = header.when_some(item_timestamp(item, false, "timeline-row"), |d, ts| d.child(ts));
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
                    .child(
                        div()
                            .flex_shrink_0()
                            .child(icons::icon(Icon::Shield, 14.).text_color(warning())),
                    )
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
                        .when(*has_warning, |d| {
                            d.child(div().child(
                                icons::icon(Icon::TriangleAlert, 12.).text_color(theme::warning()),
                            ))
                        })
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
                    .child(work_icon(Icon::MessageSquare, false))
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
                        .text_color(theme::primary_fg())
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
            theme::error_rule()
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
                    .when(failed, |d| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .text_color(danger())
                                .child(icons::icon(Icon::TriangleAlert, 12.).text_color(danger())),
                        )
                    })
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
            work_entry_icon("todo_list"),
            div().text_color(self.live_label_color(live)).child(label),
        );
        header = header
            .when_some(item_timestamp(item, false, "timeline-row"), |d, ts| d.child(ts))
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
                let (icon, color, done) = match status {
                    // check
                    "completed" | "done" => (Icon::Check, success(), true),
                    // circle-dot (in progress)
                    "running" | "in_progress" | "in-progress" => {
                        (Icon::CircleDot, theme::primary(), false)
                    }
                    // circle
                    _ => (Icon::Circle, theme::fg_muted(), false),
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
                        .child(
                            div()
                                .w(px(16.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(color)
                                .child(icons::icon(icon, 14.).text_color(color)),
                        )
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
            .child(work_icon(notice_icon(item, failed), failed))
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
            .when_some(item_timestamp(item, false, "timeline-row"), |d, ts| d.child(ts))
            .into_any_element()
    }

    fn error_row(&self, item: &TurnItemView) -> AnyElement {
        let failure = &item.raw["failure"];
        // Provider usage-limit failures (`failure.class: "usage_limit"`)
        // render as a warning-toned WorkLogRow — `text-warning` icon +
        // `text-sm font-medium` "Usage limit reached. Retry after …"
        // label, no card chrome, and no detail paragraph.
        let usage_limit =
            item.status != "completed" && failure["class"].as_str() == Some("usage_limit");
        if usage_limit {
            let reset = failure["resetAt"]
                .as_str()
                .and_then(upcoming_label)
                .map(|t| format!(" Retry after {t}."))
                .unwrap_or_default();
            return work_row_line()
                .group("timeline-row")
                .child(glyph_slot(Icon::CircleAlert, warning(), true))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(warning())
                        .child(format!("Usage limit reached.{reset}")),
                )
                .when_some(item_timestamp(item, true, "timeline-row"), |d, ts| d.child(ts))
                .into_any_element();
        }
        let title = raw_str(item, "title")
            .map(str::to_owned)
            .or_else(|| failure["class"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "Error".to_string());
        let message = failure["message"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| item.body.clone())
            .unwrap_or_else(|| title.clone());
        let code = failure["code"].as_str().map(str::to_owned);
        // Plain provider errors are work entries with the destructive
        // row style — ⚠ icon + `text-destructive` heading, no card.
        work_row_line()
            .group("timeline-row")
            .child(work_icon(Icon::CircleAlert, true))
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
            .when_some(item_timestamp(item, true, "timeline-row"), |d, ts| d.child(ts))
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
                    .children(self.render_md(markdown)),
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
            work_entry_icon("subagent"),
            div()
                .text_color(self.live_label_color(live))
                .child(if status_label.is_empty() {
                    title.to_string()
                } else {
                    format!("{title} · {status_label}")
                }),
        );
        header = header
            .when_some(item_timestamp(item, false, "timeline-row"), |d, ts| d.child(ts))
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
                        .border_color(theme::warning_border())
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
                                .text_color(warning())
                                .flex_shrink_0()
                                .child(icons::icon(Icon::TriangleAlert, 12.).text_color(warning())),
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
            work_entry_icon(item.kind.as_str()),
            div().text_color(self.live_label_color(live)).child(text),
        );
        header = header
            .when_some(item_timestamp(item, false, "timeline-row"), |d, ts| d.child(ts))
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
            row = row.child(
                div()
                    .flex_shrink_0()
                    .text_color(theme::primary())
                    .child(icons::icon(Icon::Ellipsis, 14.).text_color(theme::primary())),
            );
        }
        row = row.child(div().flex_1().h(px(1.)).bg(theme::hairline()));
        row.into_any_element()
    }

    /// `checkpoint` turn item — the web's `ChangedFilesCard`: an `mt-4
    /// rounded-lg` card with a "N changed files +A -D" header, an
    /// expand-all toggle, an "Open diff" action (fetches
    /// `orchestration.getTurnDiff` and shows the patch inline), and the
    /// file tree (`h-7` mono rows, directories collapsible).
    fn checkpoint_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        let files: &[Value] = item.raw["files"].as_array().map_or(&[], Vec::as_slice);
        let totals = &item.raw["filesTotal"];
        let count = totals["count"].as_u64().unwrap_or(files.len() as u64);
        let sum = |key: &str| -> u64 { files.iter().filter_map(|f| f[key].as_u64()).sum() };
        let total_adds = totals["additions"]
            .as_u64()
            .unwrap_or_else(|| sum("additions"));
        let total_dels = totals["deletions"]
            .as_u64()
            .unwrap_or_else(|| sum("deletions"));

        let tree = build_file_tree(files);
        let has_dirs = tree.iter().any(|n| matches!(n, FileTreeNode::Dir { .. }));
        let all_expanded = self
            .timeline_state
            .checkpoint_dirs
            .get(&item.id)
            .is_some_and(|s| s.all_expanded);

        // "Open diff" needs the turn range — the checkpoint's own turn
        // count is approximated by the number of turn-start user messages
        // up to this item (each starts an app run).
        let mut run_ids: HashSet<String> = HashSet::new();
        let mut unrun_boundaries = 0u64;
        for it in &self.thread_items[..=index.min(self.thread_items.len().saturating_sub(1))] {
            if it.kind != "user_message" {
                continue;
            }
            if matches!(
                raw_str(it, "inputIntent"),
                Some("steer" | "promoted_queued_to_steer")
            ) {
                continue;
            }
            match raw_str(it, "runId") {
                Some(run) => {
                    run_ids.insert(run.to_string());
                }
                None => unrun_boundaries += 1,
            }
        }
        let to_turn = run_ids.len() as u64 + unrun_boundaries;
        let from_turn = to_turn.saturating_sub(1);
        let thread_id = raw_str(item, "threadId")
            .map(str::to_owned)
            .or_else(|| self.state.selected.as_ref().map(|s| s.to_string()));
        let diff_open = self
            .timeline_state
            .expanded
            .contains(&format!("cdiff:{}", item.id));

        // Header: "2 changed files" + inline DiffStatLabel, then the
        // expand-all icon and the "Open diff" ghost button.
        let mut title = div()
            .flex()
            .items_center()
            .gap_3()
            .min_w_0()
            .text_xs()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme::fg())
            .child(format!(
                "{count} changed file{}",
                if count == 1 { "" } else { "s" }
            ));
        if total_adds > 0 {
            title = title.child(
                div()
                    .text_xs()
                    .text_color(success())
                    .child(format!("+{total_adds}")),
            );
        }
        if total_dels > 0 {
            title = title.child(
                div()
                    .text_xs()
                    .text_color(danger())
                    .child(format!("-{total_dels}")),
            );
        }

        let mut actions = div().flex().items_center().gap_1().flex_shrink_0();
        if has_dirs {
            let item_id = item.id.clone();
            actions = actions.child(
                div()
                    .id(("cpdirs", index))
                    .w(px(24.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::accent()))
                    .on_click(move |_event, window, cx| {
                        let item_id = item_id.clone();
                        update_view(window, cx, move |this, cx| {
                            let state = this
                                .timeline_state
                                .checkpoint_dirs
                                .entry(item_id)
                                .or_default();
                            state.all_expanded = !state.all_expanded;
                            state.overrides.clear();
                            cx.notify();
                        });
                    })
                    .child(
                        icons::icon(
                            if all_expanded {
                                Icon::ChevronsDownUp
                            } else {
                                Icon::ChevronsUpDown
                            },
                            12.,
                        )
                        .text_color(theme::fg_muted()),
                    ),
            );
        }
        if let Some(tid) = thread_id.clone()
            && to_turn > 0
        {
            let item_id = item.id.clone();
            actions = actions.child(
                div()
                    .id(("cpdiff-btn", index))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1p5()
                    .h(px(24.))
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::accent()).text_color(theme::fg()))
                    .on_click(move |_event, window, cx| {
                        let item_id = item_id.clone();
                        let tid = tid.clone();
                        update_view(window, cx, move |this, cx| {
                            this.open_checkpoint_diff(item_id, tid, from_turn, to_turn, cx);
                        });
                    })
                    .child(icons::icon(Icon::FileDiff, 12.).text_color(theme::fg_muted()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("Open diff"),
                    ),
            );
        }

        let mut card = div()
            .mt(px(16.))
            .rounded_lg()
            .bg(theme::changed_files_bg())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(title)
                    .child(actions),
            );

        // File tree rows — `h-7 gap-2 pr-2`, `paddingLeft: 8 + depth*14`.
        let dir_state = self.timeline_state.checkpoint_dirs.get(&item.id);
        let mut rows = div().flex().flex_col().p_2();
        fn stat_spans(additions: Option<u64>, deletions: Option<u64>) -> gpui::Div {
            let mut d = div()
                .ml_auto()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_1()
                .font_family(theme::FONT_MONO)
                .text_size(theme::text_size::XS_3)
                .font_features(tabular_nums());
            if let Some(a) = additions
                && a > 0
            {
                d = d.child(div().text_color(success()).child(format!("+{a}")));
            }
            if let Some(dels) = deletions
                && dels > 0
            {
                d = d.child(div().text_color(danger()).child(format!("-{dels}")));
            }
            d
        }
        fn file_rows<'a>(
            nodes: &'a [FileTreeNode],
            depth: usize,
            dir_state: Option<&'a CheckpointDirState>,
            has_dirs: bool,
            item_id: &'a str,
            index: usize,
            rows: gpui::Div,
        ) -> gpui::Div {
            let mut rows = rows;
            for node in nodes {
                match node {
                    FileTreeNode::Dir {
                        name,
                        path,
                        additions,
                        deletions,
                        children,
                    } => {
                        let expanded = dir_state.is_some_and(|s| s.is_expanded(path));
                        let row_id = ("cpdir", fnv_node(&format!("{index}:{path}")));
                        let item_id_c = item_id.to_string();
                        let path_c = path.clone();
                        rows = rows.child(
                            div()
                                .id(row_id)
                                .flex()
                                .items_center()
                                .gap_2()
                                .h(px(28.))
                                .pl(px(8. + 14. * depth as f32))
                                .pr_2()
                                .rounded_md()
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::accent()))
                                .on_click(move |_event, window, cx| {
                                    let item_id = item_id_c.clone();
                                    let path = path_c.clone();
                                    update_view(window, cx, move |this, cx| {
                                        let state = this
                                            .timeline_state
                                            .checkpoint_dirs
                                            .entry(item_id)
                                            .or_default();
                                        if !state.overrides.remove(&path) {
                                            state.overrides.insert(path);
                                        }
                                        cx.notify();
                                    });
                                })
                                .child(
                                    div().w(px(14.)).flex_shrink_0().child(
                                        icons::icon(
                                            if expanded {
                                                Icon::ChevronDown
                                            } else {
                                                Icon::ChevronRight
                                            },
                                            14.,
                                        )
                                        .text_color(theme::icon_muted()),
                                    ),
                                )
                                .child(
                                    div().w(px(14.)).flex_shrink_0().child(
                                        icons::icon(
                                            if expanded {
                                                Icon::FolderOpen
                                            } else {
                                                Icon::Folder
                                            },
                                            14.,
                                        )
                                        .text_color(theme::icon_muted()),
                                    ),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(theme::FONT_MONO)
                                        .text_size(theme::text_size::XS_2)
                                        .text_color(theme::fg_muted())
                                        .child(name.clone()),
                                )
                                .child(stat_spans(Some(*additions), Some(*deletions))),
                        );
                        if expanded {
                            rows = file_rows(
                                children,
                                depth + 1,
                                dir_state,
                                has_dirs,
                                item_id,
                                index,
                                rows,
                            );
                        }
                    }
                    FileTreeNode::File {
                        name,
                        additions,
                        deletions,
                    } => {
                        rows = rows.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .h(px(28.))
                                .pl(px(8. + 14. * depth as f32))
                                .pr_2()
                                .rounded_md()
                                .when(has_dirs || depth > 0, |d| {
                                    d.child(div().w(px(14.)).flex_shrink_0())
                                })
                                .child(div().w(px(14.)).flex_shrink_0().child(
                                    icons::icon(Icon::File, 14.).text_color(theme::icon_muted()),
                                ))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(theme::FONT_MONO)
                                        .text_xs()
                                        .text_color(theme::chat_fg())
                                        .child(name.clone()),
                                )
                                .child(stat_spans(*additions, *deletions)),
                        );
                    }
                }
            }
            rows
        }
        rows = file_rows(&tree, 0, dir_state, has_dirs, &item.id, index, rows);
        card = card.child(rows);

        // "N more files in the full diff" when `files` is a wire prefix.
        let hidden_files = count.saturating_sub(files.len() as u64);
        if hidden_files > 0 {
            card = card.child(div().px_3().pb_2().child(
                div().text_xs().text_color(theme::fg_muted()).child(format!(
                    "{hidden_files} more file{} in the full diff",
                    if hidden_files == 1 { "" } else { "s" }
                )),
            ));
        }

        // The expanded inline patch below the tree (`detail_panel` mono).
        if diff_open {
            let state = self.timeline_state.checkpoint_diffs.get(&item.id);
            let mut panel = detail_panel("cpdiff", index);
            match state {
                Some(CheckpointDiff::Loaded(diff)) if !diff.is_empty() => {
                    for line in diff.lines().take(2000) {
                        panel = panel.child(
                            div()
                                .text_color(diff_line_color(line))
                                .child(line.to_string()),
                        );
                    }
                }
                Some(CheckpointDiff::Loaded(_)) => {
                    panel = panel.child(div().italic().child("Empty diff."));
                }
                Some(CheckpointDiff::Error(e)) => {
                    panel = panel.child(div().text_color(danger()).child(e.clone()));
                }
                _ => {
                    panel = panel.child(div().italic().child("Loading diff…"));
                }
            }
            card = card.child(div().px_2().pb_2().child(panel));
        }
        card.into_any_element()
    }

    /// Toggle the checkpoint card's inline turn diff; fetches
    /// `orchestration.getTurnDiff` on first open.
    fn open_checkpoint_diff(
        &mut self,
        item_id: String,
        thread_id: String,
        from_turn: u64,
        to_turn: u64,
        cx: &mut gpui::Context<Self>,
    ) {
        let key = format!("cdiff:{item_id}");
        if self.timeline_state.expanded.remove(&key) {
            cx.notify();
            return;
        }
        self.timeline_state.expanded.insert(key);
        if self.timeline_state.checkpoint_diffs.contains_key(&item_id) {
            cx.notify();
            return;
        }
        let Some(rx) = self.send_query(
            "orchestration.getTurnDiff",
            json!({
                "threadId": thread_id,
                "fromTurnCount": from_turn,
                "toTurnCount": to_turn,
            }),
        ) else {
            self.timeline_state.checkpoint_diffs.insert(
                item_id,
                CheckpointDiff::Error("Not connected to a server.".to_string()),
            );
            cx.notify();
            return;
        };
        self.timeline_state
            .checkpoint_diffs
            .insert(item_id.clone(), CheckpointDiff::Loading);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let state = if exit["_tag"].as_str() == Some("Success") {
                    CheckpointDiff::Loaded(
                        exit["value"]["diff"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    )
                } else {
                    CheckpointDiff::Error(rpc_error_text(&exit))
                };
                this.timeline_state.checkpoint_diffs.insert(item_id, state);
                cx.notify();
            });
        })
        .detach();
    }
}
