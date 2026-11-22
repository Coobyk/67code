//! Sidebar: project grouping, thread rows, status indicators, context menus.
//! Owns `impl AppView` sidebar render methods and `sidebar::State`.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use gpui::{
    AnyElement, ClipboardItem, ElementId, Entity, EntityInputHandler, FocusHandle, Focusable,
    MouseButton, MouseDownEvent, ObjectFit, RenderImage, SharedString, Subscription, Window,
    anchored, deferred, div, img, point, prelude::*, px, rgb,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppView;
use crate::CenterView;
use crate::composer::{Attachment, AttachmentKind};
use crate::config;
use crate::icons::{Icon, hover_icon, icon};
use crate::model::{ConnectionStatus, Project, Thread};
use crate::notifications::{ToastAction, ToastKind};
use crate::text_field::{PromptSubmitted, TextField};
use crate::theme;
use crate::tooltip::tooltip;

/// Sidebar-local UI state. Expand freely — this field lives on `AppView`.
#[derive(Default)]
pub struct State {
    /// Text the user typed into the sidebar search field.
    pub query: String,
    /// Thread id whose context menu is open, plus open position.
    pub context_menu: Option<ContextMenu>,
    /// Thread currently being renamed (inline editor open).
    pub renaming: Option<SharedString>,
    /// Thread id awaiting a second "Delete" click to confirm.
    pub delete_confirm: Option<SharedString>,
    /// Inline new-project form is open.
    pub new_project_open: bool,
    /// Thread id the snooze overlay is open for.
    pub snooze_target: Option<SharedString>,
    /// Parse error shown under the custom "snooze until" field.
    pub snooze_error: Option<SharedString>,
    /// Draft-threads block collapsed.
    pub drafts_collapsed: bool,
    /// Project shell `icon` values keyed by project id. `net::parse_project`
    /// drops `icon`, so `load_project_icons` harvests it out-of-band.
    pub project_icons: HashMap<String, Value>,
    /// Project shell `faviconPath` values keyed by project id, harvested by
    /// the same out-of-band query as `project_icons`.
    pub project_favicons: HashMap<String, String>,
    /// project id → (favicon path loaded, decoded image). Loaded through
    /// `assets.createUrl` (`project-favicon` resource) + bearer GET.
    pub favicon_images: HashMap<String, (String, Arc<RenderImage>)>,
    /// project id → favicon path with a fetch in flight.
    pub favicon_loading: HashMap<String, String>,
    /// project id → favicon path whose fetch/decode failed; not retried
    /// until the harvested path changes.
    pub favicon_failed: HashMap<String, String>,
    /// Whether the project-icon query already ran since the last shell event
    /// that could change it.
    pub icons_queried: bool,
    /// Thread-shell fields `net::parse_thread` drops (`settledOverride`,
    /// `settledAt`, `branch`), harvested out-of-band like `project_icons`.
    /// Keyed by thread id.
    pub thread_meta: HashMap<String, ThreadMeta>,
    /// Whether the metadata shell stream + settled query already ran.
    pub meta_started: bool,
    /// Raw `server.getConfig` value — environmentId, capabilities
    /// (`threadAutoSettleOptOut`, `threadProjectFromScratch`, `threadSnooze`,
    /// `threadSettlement`), `scratchWorkspaceRoot`, machine kind/label.
    pub server_config: Option<Value>,
    /// Whether the config query already ran since the last connection.
    pub config_queried: bool,
    /// The "Working" shelf is expanded (web default: collapsed).
    pub working_open: bool,
    /// The "Snoozed" shelf is expanded (web default: collapsed).
    pub snoozed_open: bool,
    /// The collapsed "Settled" shelf is expanded.
    pub settled_open: bool,
    /// Scratch singleton groups the user collapsed, keyed by thread id.
    /// Each "No project" thread is its own group (web group model), so
    /// `Project.expanded` can't express per-thread collapse.
    pub scratch_collapsed: HashSet<SharedString>,
    /// List-filter toggle: when set, the sidebar lists only threads of this
    /// project (the web's `ListFilter` project-scope button).
    pub project_filter: Option<SharedString>,
    /// Scroll position for the thread list.
    pub scroll: gpui::ScrollHandle,
    rename_field: Option<Entity<TextField>>,
    snooze_field: Option<Entity<TextField>>,
    project_name_field: Option<Entity<TextField>>,
    project_path_field: Option<Entity<TextField>>,
    rename_blur: Option<Subscription>,
    /// Focus handle tracked by the sidebar root. The context menu grabs it on
    /// open so root-level key handling (Escape to dismiss) is reachable while
    /// focus would otherwise stay in the main content area.
    menu_focus: Option<FocusHandle>,
    subs: Vec<Subscription>,
    wired: bool,
}

#[derive(Clone)]
pub struct ContextMenu {
    pub thread_id: SharedString,
    pub position: gpui::Point<gpui::Pixels>,
    /// Parent item whose submenu popover is open, plus the anchor point the
    /// popover was opened from (top-right of that row).
    pub open_submenu: Option<(&'static str, gpui::Point<gpui::Pixels>)>,
}

/// Thread-shell fields the parsed `Thread` model drops — harvested from a
/// second raw `subscribeShell` stream plus `getOlderSettledThreads`.
#[derive(Clone, Default)]
pub struct ThreadMeta {
    /// `settledOverride == "settled"` or the thread appeared in the
    /// older-settled listing.
    pub settled: bool,
    /// ISO timestamp the thread settled — drives the settled-row label.
    pub settled_at: Option<String>,
    /// Git branch the thread's worktree is on (`branch` shell field).
    pub branch: Option<String>,
    /// `worktreePath` — renders the FolderGit2 worktree indicator and the
    /// "Turn into project" gate.
    pub worktree_path: Option<String>,
    /// Title from the raw shell (used when the thread isn't in the parsed
    /// `state.threads`, e.g. older settled rows).
    pub title: String,
    /// Owning project id for settled-row grouping chips.
    pub project_id: String,
    /// `pinnedAt != null`.
    pub pinned: bool,
    /// `pendingRuntimeRequest.kind` — approval-family kinds map to the web
    /// "Approval" top status; `user_input` maps to "Input".
    pub pending_request_kind: Option<String>,
    /// `pendingSubagentRequest` — `"approval"`/`"user_input"`; any value also
    /// excludes the Working shelf like the web's `isThreadWorking`.
    pub pending_subagent_request: Option<String>,
    /// `activeProviderThreadId` — with `latestRunId` it proves a runtime
    /// summary exists (web `shellRuntime`).
    pub active_provider_thread_id: Option<String>,
    /// `pendingGuestMessageCount` — >0 maps to the "Request" top status.
    pub pending_guest_messages: i64,
    /// `blockedOnThreadWait` — with a running runtime this is "Waiting".
    pub blocked_on_thread_wait: bool,
    /// `lastErrorClass` — `usage_limit` maps to the "Limited" top status.
    pub last_error_class: Option<String>,
    /// `goal.status == "active"` swaps the "Working" label for "Goal".
    pub goal_active: bool,
    /// `activityRunStartedAt` — Working/Waiting duration anchor
    /// (`resolveThreadWorkingStartedAt` runtime half).
    pub activity_run_started_at: Option<String>,
    /// `activeRunId` — paired with latestRun fields for the duration anchor.
    pub active_run_id: Option<String>,
    /// `latestRunRequestedAt`/`latestRunCompletedAt` — duration anchors.
    pub latest_run_requested_at: Option<String>,
    pub latest_run_completed_at: Option<String>,
    /// `titleRegeneration != null` — card dims and the menu item disables.
    pub title_regenerating: bool,
    /// `autoSettleDisabledAt != null` — Auto-settle "Never" is checked.
    pub auto_settle_disabled: bool,
    /// `providerInstanceId` + `modelSelection.instanceId` — the top of the
    /// provider stack (`runtime?.providerInstanceId` falls back to the
    /// model selection like the web).
    pub provider_instance_id: Option<String>,
    pub model_instance_id: Option<String>,
    /// `pullRequests` links — the badge switches to a "+N" stack indicator
    /// when more than one PR is linked.
    pub pull_request_count: usize,
}

/// Dot color for a thread's status — the web `resolveSidebarFolderDot`
/// palette (attention → warning, failed → error, unread → success,
/// working → info). `None` for a quiet, read row: web folder rows draw no
/// status affordance for settled threads.
fn status_dot_color(thread: &Thread, unread: bool) -> Option<gpui::Rgba> {
    if thread.has_pending_request || thread.has_actionable_proposed_plan {
        Some(theme::warning())
    } else if matches!(thread.status.as_ref(), "failed" | "error") {
        Some(theme::error())
    } else if thread.running() {
        Some(theme::info())
    } else if unread {
        Some(theme::success())
    } else {
        None
    }
}

/// Seconds since the Unix epoch. The app has no chrono/time dep, so snooze
/// presets are computed in UTC.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Days since epoch -> (year, month, day); Howard Hinnant's civil algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (year, month, day) -> days since epoch; inverse of `civil_from_days`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `YYYY-MM-DDTHH:MM:SS.000Z` — the wire `IsoDateTime` shape.
fn iso_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Parse the `YYYY-MM-DDTHH:MM:SS` prefix of an ISO timestamp back to Unix
/// seconds. Anything after the seconds field (fraction, zone) is ignored —
/// our payloads always write `Z`.
fn unix_from_iso(s: &str) -> Option<i64> {
    if s.len() < 19 {
        return None;
    }
    let num = |i: usize, n: usize| -> Option<i64> { s.get(i..i + n)?.parse().ok() };
    let days = days_from_civil(num(0, 4)?, num(5, 2)?, num(8, 2)?);
    Some(days * 86_400 + num(11, 2)? * 3600 + num(14, 2)? * 60 + num(17, 2)?)
}

/// Now as a wire `IsoDateTime` string.
fn iso_now() -> String {
    iso_from_unix(unix_now())
}

/// Web `formatWorkingDurationLabel`: `<1s`→"0s", `12s`, `3m`, `1h 4m`, `2d 3h`.
fn fmt_duration(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3_600 {
        return format!("{}m", secs / 60);
    }
    if secs < 86_400 {
        return format!("{}h {}m", secs / 3_600, (secs % 3_600) / 60);
    }
    format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3_600)
}

/// Indigo used by the web `input` status label (`text-indigo-*`).
fn indigo_fg() -> gpui::Rgba {
    if theme::effective_dark() {
        gpui::rgba(0xa5b4fcff)
    } else {
        gpui::rgba(0x6366f1ff)
    }
}

/// The derived runtime summary — web `shellRuntime`: a runtime exists while
/// a run or provider session is on record; its status is `activityRunStatus`
/// then the shell `status` (the background-task idle park is approximated
/// away — `pendingBackgroundTasks` isn't harvested).
fn runtime_status<'a>(thread: &'a Thread, meta: Option<&'a ThreadMeta>) -> Option<&'a str> {
    if thread.latest_run_id.is_none() && meta.is_none_or(|m| m.active_provider_thread_id.is_none())
    {
        return None;
    }
    Some(
        thread
            .activity_run_status
            .as_deref()
            .unwrap_or(thread.status.as_ref()),
    )
}

/// The card's right-edge status slot, mirroring web
/// `resolveSidebarThreadStatus` + `topStatus`: icon, color, label, and the
/// ISO timestamp a running duration counts from. `None` renders the
/// relative-time label instead.
fn card_top_status(
    thread: &Thread,
    meta: Option<&ThreadMeta>,
    unread: bool,
) -> Option<(Icon, gpui::Rgba, &'static str, Option<String>)> {
    let meta_field = |f: fn(&ThreadMeta) -> Option<&str>| meta.and_then(f).map(str::to_string);
    let request_kind = meta.and_then(|m| m.pending_request_kind.as_deref());
    let subagent_kind = meta.and_then(|m| m.pending_subagent_request.as_deref());
    // hasPendingApprovals — a pending request that isn't user input or an
    // auth refresh, or a subagent waiting on approval.
    if subagent_kind == Some("approval")
        || (thread.has_pending_request
            && !matches!(request_kind, Some("user_input") | Some("auth_refresh")))
    {
        return Some((Icon::ShieldQuestion, theme::warning_fg(), "Approval", None));
    }
    if subagent_kind == Some("user_input") || request_kind == Some("user_input") {
        return Some((Icon::MessageCircleQuestion, indigo_fg(), "Input", None));
    }
    if meta.is_some_and(|m| m.pending_guest_messages > 0) {
        return Some((Icon::UserRound, theme::guest_fg(), "Request", None));
    }
    let status = runtime_status(thread, meta);
    if status == Some("running") && meta.is_some_and(|m| m.blocked_on_thread_wait) {
        let started = meta_field(|m| m.latest_run_completed_at.as_deref());
        return Some((
            Icon::CircleDotDashed,
            theme::sidebar_fg_muted(),
            "Waiting",
            started,
        ));
    }
    if status == Some("preparing") {
        return Some((
            Icon::CircleDashed,
            theme::info_fg(),
            "Setting up",
            working_duration_start(thread, meta),
        ));
    }
    if matches!(status, Some("queued" | "starting" | "running" | "waiting")) {
        let label = if meta.is_some_and(|m| m.goal_active) {
            "Goal"
        } else {
            "Working"
        };
        return Some((
            Icon::CircleDashed,
            theme::info_fg(),
            label,
            working_duration_start(thread, meta),
        ));
    }
    if status == Some("idle") {
        let started = meta_field(|m| m.latest_run_completed_at.as_deref());
        return Some((
            Icon::CircleDotDashed,
            theme::sidebar_fg_muted(),
            "Waiting",
            started,
        ));
    }
    if status == Some("failed") {
        if meta.and_then(|m| m.last_error_class.as_deref()) == Some("usage_limit") {
            return Some((Icon::CircleAlert, theme::warning_fg(), "Limited", None));
        }
        return Some((Icon::CircleAlert, theme::error_fg(), "Failed", None));
    }
    if unread {
        return Some((Icon::CircleCheck, theme::success_fg(), "Done", None));
    }
    None
}

/// The timestamp a "Working"/"Setting up" duration counts from — web
/// `resolveThreadWorkingStartedAt`: `activityRunStartedAt`, else a still-open
/// latest run that is the runtime's active run (`startedAt ?? requestedAt`).
fn working_duration_start(thread: &Thread, meta: Option<&ThreadMeta>) -> Option<String> {
    meta.and_then(|m| m.activity_run_started_at.clone())
        .or_else(|| {
            let still_open = meta.is_some_and(|m| m.latest_run_completed_at.is_none())
                && thread.latest_run_id.is_some()
                && meta.and_then(|m| m.active_run_id.as_deref())
                    == thread.latest_run_id.as_deref();
            if still_open {
                thread
                    .latest_run_started_at
                    .clone()
                    .or_else(|| {
                        meta.and_then(|m| m.latest_run_requested_at.clone())
                            .map(SharedString::from)
                    })
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
}

/// `providerInstanceInitials` — the instance's one- or two-letter mark when
/// no bundled brand icon exists ("Replay (dev)" → "RD"). Same word-splitting
/// as the composer chip.
fn provider_initials_for(name: &str) -> String {
    let mut words = name
        .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '\u{2019}'))
        .filter(|w| !w.trim_matches(|c: char| c == '\'' || c == '\u{2019}').is_empty());
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

/// Provider-instance glyph for the title-line stack — the web
/// `ProviderInstanceIcon` mapping. Codex/OpenAI-family drivers render the
/// OpenAI mark; Anthropic-family render Claude; anything else falls back to
/// the instance's first two letters.
fn provider_glyph_icon(driver: Option<&str>) -> Option<Icon> {
    match driver? {
        "codex" | "openai" | "openaiCompatible" => Some(Icon::ProviderOpenAi),
        "claudeAgent" | "anthropic" => Some(Icon::ProviderClaude),
        _ => None,
    }
}

/// The rendered provider stack — web `SidebarProviderStack`. Entries are
/// `(brand glyph | initials, is_current)` with the current instance last:
/// history glyphs draw 12px at 35% opacity, the current glyph 14px at 60%.
fn provider_stack_element(stack: &[(Option<Icon>, String, bool)]) -> gpui::Div {
    let mut row = div().flex_none().flex().items_center().gap_1p5();
    for (glyph, initials, current) in stack {
        let (size, opacity) = if *current { (14., 0.6) } else { (12., 0.35) };
        let mark: AnyElement = match glyph {
            Some(glyph) => icon(*glyph, size)
                .text_color(theme::sidebar_fg())
                .into_any_element(),
            None => div()
                .flex_none()
                .text_size(px(9.))
                .text_color(theme::sidebar_fg_muted())
                .child(initials.clone())
                .into_any_element(),
        };
        row = row.child(div().flex_none().opacity(opacity).child(mark));
    }
    row
}

/// Pull-request chip data for the meta line: icon, color, count text.
/// Mirrors web `prBadge`: stacks (+N) use `LayersIcon`; single PRs pick
/// merge-state color and `#number`.
fn pr_badge(thread: &Thread, meta: Option<&ThreadMeta>) -> Option<(Icon, gpui::Rgba, String)> {
    let linked = thread.linked_pull_request.as_ref()?;
    let stack = meta.map(|m| m.pull_request_count).unwrap_or(0);
    if stack > 1 {
        return Some((Icon::Layers, theme::info_fg(), format!("+{stack}")));
    }
    let number = linked["number"]
        .as_i64()
        .or_else(|| linked["pullNumber"].as_i64())
        .or_else(|| {
            // Derive from the URL tail: `…/pull/123`, `…/merge_requests/12`.
            linked["url"]
                .as_str()
                .and_then(|u| u.rsplit('/').next())
                .and_then(|tail| tail.parse::<i64>().ok())
        });
    let state = linked["state"].as_str().unwrap_or("open").to_lowercase();
    let draft = linked["isDraft"].as_bool() == Some(true);
    let color = if state == "merged" {
        gpui::rgba(0xa78bfaff)
    } else if state == "closed" {
        theme::error_fg()
    } else if draft {
        theme::sidebar_fg_muted()
    } else {
        theme::success_fg()
    };
    let text = number.map(|n| format!("#{n}")).unwrap_or_default();
    Some((Icon::GitPullRequest, color, text))
}

/// Snooze presets mirroring the web defaults
/// (`packages/contracts/settings.ts` DEFAULT_SNOOZE_PRESETS): durations plus
/// fixed wake times. With no timezone database in the app, fixed times
/// resolve in UTC.
fn snooze_presets(now: i64) -> Vec<(&'static str, i64, String)> {
    const DAY: i64 = 86_400;
    let today = now.div_euclid(DAY);
    // Weekday of `today`: 0=Sun…6=Sat. Day 0 (1970-01-01) was a Thursday.
    let weekday = (today + 4).rem_euclid(7);
    // Days until next Monday — a Monday counts as next week's, like the web.
    let days_to_monday = (1 - weekday).rem_euclid(7);
    let monday_ahead = if days_to_monday == 0 {
        7
    } else {
        days_to_monday
    };
    let mut presets = vec![
        ("In 1 hour", now + 3_600, false),
        ("In 3 hours", now + 10_800, false),
        ("Tomorrow", (today + 1) * DAY + 9 * 3_600, true),
        (
            "Next week",
            (today + monday_ahead) * DAY + 9 * 3_600,
            true,
        ),
    ];
    // "This evening" (today 18:00) only while it's still ahead, like the web.
    let tonight = today * DAY + 18 * 3_600;
    if tonight > now {
        presets.push(("This evening", tonight, true));
    }
    presets.sort_by_key(|(_, until, _)| *until);
    presets
        .into_iter()
        .map(|(label, until, names_day)| {
            (label, until, snooze_when_label(now, until, names_day))
        })
        .collect()
}

/// Web `whenLabel`: the wake time, prefixed with the weekday when the label
/// doesn't name the day and the wake falls on another day (UTC).
fn snooze_when_label(now: i64, until: i64, names_day: bool) -> String {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let rem = until.rem_euclid(86_400);
    let time = format!("{:02}:{:02}", rem / 3_600, (rem % 3_600) / 60);
    let same_day = until.div_euclid(86_400) == now.div_euclid(86_400);
    if names_day || same_day {
        time
    } else {
        let weekday = (until.div_euclid(86_400) + 4).rem_euclid(7);
        format!("{} {}", WEEKDAYS[weekday as usize], time)
    }
}

/// Web `hostedSegment`: percent-encode everything outside `[A-Za-z0-9_-]`
/// (encodeURIComponent's unescaped set minus the punctuation it re-escapes),
/// so hosted thread links match `app.t3.codes/threads/…`.
fn hosted_segment(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for &b in id.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `a`-`z` fraction digits for order keys — the same keyspace the web uses
/// (`client-runtime/state/threadSort.ts` pinOrderKeyBetween).
const PIN_ORDER_DIGITS: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";

/// Spread slot key for position `i` in a run of `count` rows — a single
/// entry of `generateSpreadPinOrderKeys`. Used by the reorder fallback that
/// rewrites a pinned block whose keys are missing or not fraction keys.
fn spread_slot_key(i: usize, count: usize) -> String {
    let mut width = 2usize;
    let mut space = PIN_ORDER_DIGITS.len() * PIN_ORDER_DIGITS.len();
    while space <= (count + 1) * 2 {
        width += 1;
        space *= PIN_ORDER_DIGITS.len();
    }
    let step = space / (count + 1);
    let mut value = step * (i + 1);
    if value % PIN_ORDER_DIGITS.len() == 0 {
        value += 1;
    }
    let mut chars = vec!['a'; width];
    for digit in (0..width).rev() {
        chars[digit] = PIN_ORDER_DIGITS[value % PIN_ORDER_DIGITS.len()] as char;
        value /= PIN_ORDER_DIGITS.len();
    }
    chars.into_iter().collect()
}

/// Midpoint of two a-z digit strings interpreted as fractions in (0,1);
/// "" stands for the open bound on either side and requires a < b.
/// Port of `pinOrderMidpoint` (keys here are ASCII digits, so byte math is
/// equivalent to the JS char math).
fn pin_order_midpoint(a: &str, b: &str) -> String {
    if !b.is_empty() {
        let mut n = 0;
        while n < b.len() && a.as_bytes().get(n).copied().unwrap_or(b'a') == b.as_bytes()[n] {
            n += 1;
        }
        if n > 0 {
            let a_rest = if n < a.len() { &a[n..] } else { "" };
            return format!("{}{}", &b[..n], pin_order_midpoint(a_rest, &b[n..]));
        }
    }
    let digit_a = if a.is_empty() {
        0i64
    } else {
        (a.as_bytes()[0] - b'a') as i64
    };
    let digit_b = if b.is_empty() {
        PIN_ORDER_DIGITS.len() as i64
    } else {
        (b.as_bytes()[0] - b'a') as i64
    };
    if digit_b - digit_a > 1 {
        return ((b'a' + ((digit_a + digit_b + 1) / 2) as u8) as char).to_string();
    }
    if b.len() > 1 {
        return (b.as_bytes()[0] as char).to_string();
    }
    let head = a.as_bytes().first().copied().unwrap_or(b'a');
    let rest = if a.len() > 1 { &a[1..] } else { "" };
    format!("{}{}", head as char, pin_order_midpoint(rest, ""))
}

/// Key that sorts strictly between two neighbor keys; `None` is an open
/// bound (top/bottom of the keyed run).
fn order_key_between(before: Option<&str>, after: Option<&str>) -> String {
    pin_order_midpoint(before.unwrap_or(""), after.unwrap_or(""))
}

/// Web `isValidPinOrderKey`: every char a-z and no trailing `a` (a trailing
/// minimum digit leaves no room to sort immediately before it).
fn is_valid_order_key(key: &str) -> bool {
    !key.is_empty()
        && key.bytes().all(|b| PIN_ORDER_DIGITS.contains(&b))
        && key.bytes().last() != Some(b'a')
}

/// Read a text field's contents during effects (observers/subscribers), when
/// the `&mut Window` isn't on the update stack but isn't passed in either.
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

fn focus_field(field: &Entity<TextField>, cx: &mut Context<AppView>) {
    let focus = field.read(cx).focus_handle(cx);
    let _ = cx.with_window(field.entity_id(), |window, app| {
        focus.focus(window, app);
    });
}

fn reset_field(field: &Entity<TextField>, cx: &mut Context<AppView>) {
    let _ = cx.with_window(field.entity_id(), |window, app| {
        field.update(app, |field, cx| field.reset(window, cx));
    });
}

fn set_field_text(field: &Entity<TextField>, text: &str, cx: &mut Context<AppView>) {
    let _ = cx.with_window(field.entity_id(), |window, app| {
        field.update(app, |field, cx| {
            field.replace_text_in_range(Some(0..usize::MAX), text, window, cx)
        });
    });
}

/// Parse a custom snooze target into Unix seconds: relative durations
/// ("30m", "2h", "1d", "1w") or a UTC datetime "YYYY-MM-DD[ T]HH:MM[:SS]"
/// (a bare date means 09:00, matching the fixed-wake presets). With no
/// timezone database in the app, datetimes resolve in UTC.
fn parse_custom_snooze(input: &str, now: i64) -> Option<i64> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    // "<digits><unit>" durations.
    if let Some(unit) = s.chars().last().filter(|c| c.is_ascii_alphabetic()) {
        let digits = &s[..s.len() - unit.len_utf8()];
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            let n: i64 = digits.parse().ok()?;
            let secs = match unit.to_ascii_lowercase() {
                'm' => 60,
                'h' => 3_600,
                'd' => 86_400,
                'w' => 604_800,
                _ => return None,
            };
            return now.checked_add(n.checked_mul(secs)?);
        }
    }
    // Datetime: "YYYY-MM-DD" plus optional "HH:MM[:SS]" separated by space or T.
    let normalized = s.replacen('T', " ", 1);
    let mut parts = normalized.split_whitespace();
    let date = parts.next()?;
    let time = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let date_bits: Vec<&str> = date.split('-').collect();
    if date_bits.len() != 3 {
        return None;
    }
    let year: i64 = date_bits[0].parse().ok()?;
    let month: i64 = date_bits[1].parse().ok()?;
    let day: i64 = date_bits[2].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (hour, minute, second) = match time {
        Some(t) => {
            let bits: Vec<&str> = t.split(':').collect();
            if !(2..=3).contains(&bits.len()) {
                return None;
            }
            (
                bits[0].parse().ok()?,
                bits[1].parse().ok()?,
                match bits.get(2) {
                    Some(b) => b.parse().ok()?,
                    None => 0,
                },
            )
        }
        None => (9, 0, 0),
    };
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=59).contains(&second) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// ProjectIconColor name → RGB (tailwind-500 palette, same swatches the web
/// picker's `projectIconColors.ts` uses).
fn project_icon_color(name: &str) -> gpui::Rgba {
    rgb(match name {
        "red" => 0xef4444,
        "orange" => 0xf97316,
        "amber" => 0xf59e0b,
        "yellow" => 0xeab308,
        "lime" => 0x84cc16,
        "green" => 0x22c55e,
        "emerald" => 0x10b981,
        "teal" => 0x14b8a6,
        "cyan" => 0x06b6d4,
        "sky" => 0x0ea5e9,
        "blue" => 0x3b82f6,
        "indigo" => 0x6366f1,
        "violet" => 0x8b5cf6,
        "purple" => 0xa855f7,
        "fuchsia" => 0xd946ef,
        "pink" => 0xec4899,
        "rose" => 0xf43f5e,
        _ => 0x6b7280, // "gray" + unknown
    })
}

/// Sidebar chip for a project shell `icon` (`{kind: emoji|monogram|lucide}`).
/// GPUI has no lucide font, so lucide icons fall back to a colored monogram
/// chip built from the icon name — matching the web's FolderCode fallback in
/// spirit. Returns None for missing/unknown icons (header renders no chip).
fn project_icon_chip(icon: &Value, project_name: &str) -> Option<AnyElement> {
    match icon.get("kind").and_then(Value::as_str)? {
        "emoji" => {
            let emoji = icon.get("emoji").and_then(Value::as_str)?;
            Some(
                div()
                    .flex_none()
                    .w(px(16.))
                    .text_xs()
                    .child(emoji.to_string())
                    .into_any_element(),
            )
        }
        kind @ ("monogram" | "lucide") => {
            let (text, color) = if kind == "monogram" {
                (
                    icon.get("text").and_then(Value::as_str),
                    icon.get("color").and_then(Value::as_str),
                )
            } else {
                (
                    icon.get("name").and_then(Value::as_str),
                    icon.get("color").and_then(Value::as_str),
                )
            };
            // Lucide icon kinds render the real glyph when the bundled set
            // has it (the web shows the icon itself, not a monogram).
            if kind == "lucide" {
                let glyph = match text.unwrap_or_default() {
                    "message-square-dashed" | "message-square" => Some(Icon::MessageSquare),
                    "messages-square" => Some(Icon::MessagesSquare),
                    "folder" | "folder-code" => Some(Icon::Folder),
                    "terminal" => Some(Icon::Terminal),
                    "bot" => Some(Icon::Bot),
                    "zap" => Some(Icon::Zap),
                    "code" | "code-2" => Some(Icon::FileCode),
                    "git-branch" => Some(Icon::GitBranch),
                    "server" => Some(Icon::Server),
                    "globe" => Some(Icon::Globe),
                    "rocket" => Some(Icon::Zap),
                    _ => None,
                };
                if let Some(glyph) = glyph {
                    // Svg doesn't inherit the parent's text color — carry
                    // the ink onto the glyph itself.
                    let ink = project_icon_color(color.unwrap_or("gray"));
                    return Some(
                        div()
                            .flex_none()
                            .w(px(16.))
                            .h(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(crate::icons::icon(glyph, 13.).text_color(ink))
                            .into_any_element(),
                    );
                }
            }
            let label: String = text
                .unwrap_or(project_name)
                .chars()
                .take(2)
                .collect::<String>()
                .to_uppercase();
            Some(
                div()
                    .flex_none()
                    .w(px(16.))
                    .h(px(16.))
                    .rounded_sm()
                    .bg(project_icon_color(color.unwrap_or("gray")))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(gpui::white())
                    .child(div().text_size(px(10.)).child(label))
                    .into_any_element(),
            )
        }
        _ => None,
    }
}

/// The sidebar brand aurora — a 2x prerender of the web's `DevBlueprintArt`
/// SVG under `sidebar-stage-backdrop` masking (see the render comment in
/// `render_sidebar`). Baked per theme since the web resolves different
/// `--stage-art-*` pigments and `--stage-fade` for dark vs light.
fn aurora_image() -> Option<Arc<RenderImage>> {
    static DARK: OnceLock<Option<Arc<RenderImage>>> = OnceLock::new();
    static LIGHT: OnceLock<Option<Arc<RenderImage>>> = OnceLock::new();
    let (slot, bytes) = if theme::effective_dark() {
        (&DARK, &include_bytes!("../assets/aurora-dark.png")[..])
    } else {
        (&LIGHT, &include_bytes!("../assets/aurora-light.png")[..])
    };
    slot.get_or_init(|| crate::explorer::decode_image(bytes))
        .clone()
}

/// 16px image chip for a decoded project favicon — same slot as
/// `project_icon_chip`, shown when the project record carries a
/// `faviconPath` that fetched and decoded cleanly.
fn favicon_chip(image: &Arc<RenderImage>) -> AnyElement {
    img(gpui::ImageSource::Render(image.clone()))
        .flex_none()
        .w(px(16.))
        .h(px(16.))
        .rounded_sm()
        .object_fit(ObjectFit::Contain)
        .into_any_element()
}

/// Web `formatRelativeElapsed`-style sidebar label: "now" / "Ns" / "Nm" /
/// "Nh" / "Nd" (web `RelativeTimeLabel` slot on rows + folder headers).
fn rel_time_label(iso: &str) -> String {
    let Some(then) = unix_from_iso(iso) else {
        return String::new();
    };
    let diff = unix_now() - then;
    if diff <= 0 {
        return "now".to_string();
    }
    if diff < 60 {
        return "now".to_string();
    }
    let minutes = diff / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    format!("{}d", hours / 24)
}

/// Web `projectIdentity.ts` monogram: first letter of the first word plus
/// the first digit it contains, else the last word's first letter, else the
/// first word's last letter — uppercased, two glyphs max.
fn project_monogram(name: &str) -> String {
    let words: Vec<Vec<char>> = name
        .split(|c: char| !(c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .map(|w| w.chars().collect())
        .collect();
    let Some(first_word) = words.first() else {
        return "PR".to_string();
    };
    let first = first_word.first().copied().unwrap_or('P');
    let second = first_word
        .iter()
        .skip(1)
        .find(|c| c.is_numeric())
        .copied()
        .or_else(|| {
            if words.len() > 1 {
                words.last().and_then(|w| w.first()).copied()
            } else {
                first_word.last().copied()
            }
        })
        .unwrap_or(first);
    format!("{first}{second}")
        .to_uppercase()
        .chars()
        .take(2)
        .collect()
}

/// Web `projectIdentity.ts` color: `index = index*31 + codepoint` folded
/// over the lowercased name, modulo the 18-entry tailwind palette.
fn project_derived_color(name: &str) -> gpui::Rgba {
    // Same order as `PROJECT_ICON_COLORS`; palette is tailwind-400 (the web
    // tiles color dark-mode text + 14% tinted backgrounds with the -400
    // swatches).
    const PALETTE: [u32; 18] = [
        0x9ca3af, 0xf87171, 0xfb923c, 0xfbbf24, 0xfacc15, 0xa3e635, 0x4ade80, 0x34d399, 0x2dd4bf,
        0x22d3ee, 0x38bdf8, 0x60a5fa, 0x818cf8, 0xa78bfa, 0xc084fc, 0xe879f9, 0xf472b6, 0xfb7185,
    ];
    let mut index: usize = 0;
    for c in name.to_lowercase().chars() {
        index = (index * 31 + c as usize) % PALETTE.len();
    }
    rgb(PALETTE[index])
}

/// The web `ProjectMonogram` tile: 16px, 25% corner radius, glyph color at
/// 14% tint on the background, bold 2-letter monogram in the glyph color.
pub(crate) fn project_monogram_chip(name: &str) -> AnyElement {
    let color = project_derived_color(name);
    let mut tint = color;
    tint.a = 0.14;
    div()
        .flex_none()
        .w(px(16.))
        .h(px(16.))
        .rounded(px(4.))
        .bg(tint)
        .flex()
        .items_center()
        .justify_center()
        .text_color(color)
        .child(
            div()
                .text_size(px(8.5))
                .font_weight(gpui::FontWeight::BOLD)
                .child(project_monogram(name)),
        )
        .into_any_element()
}

/// Drag payload for pointer-reordering pinned threads (dropped on another
/// pinned row of the same project).
struct SidebarThreadDrag {
    thread_id: SharedString,
    project_id: SharedString,
}

/// Floating ghost rendered under the pointer during a thread drag.
struct ThreadDragGhost {
    title: SharedString,
}

impl Render for ThreadDragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2p5()
            .py_1()
            .rounded_md()
            .bg(theme::sidebar_active())
            .shadow_lg()
            .text_sm()
            .text_color(theme::sidebar_fg())
            .child(self.title.clone())
    }
}

impl AppView {
    /// Lazily create the sidebar's text fields and subscriptions. Called once
    /// from `render_sidebar` because entities need a `cx` to be built.
    fn wire_sidebar(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_state.wired {
            return;
        }
        self.sidebar_state.wired = true;

        self.sidebar_state
            .subs
            .push(cx.observe(&self.search_field, |this, field, cx| {
                let query = field_text(&field, cx);
                if this.sidebar_state.query != query {
                    this.sidebar_state.query = query;
                    cx.notify();
                }
            }));

        let rename_field = cx.new(|cx| TextField::new("Thread title…", cx));
        self.sidebar_state.subs.push(cx.subscribe(
            &rename_field,
            |this, _field, event: &PromptSubmitted, cx| {
                this.commit_rename(&event.0, cx);
            },
        ));
        self.sidebar_state.rename_field = Some(rename_field);

        let name_field = cx.new(|cx| TextField::new("Project name", cx));
        let path_field = cx.new(|cx| TextField::new("/path/to/workspace", cx));
        self.sidebar_state.subs.push(cx.subscribe(
            &name_field,
            |this, field, event: &PromptSubmitted, cx| {
                this.submit_project_form(&field, &event.0, cx);
            },
        ));
        self.sidebar_state.subs.push(cx.subscribe(
            &path_field,
            |this, field, event: &PromptSubmitted, cx| {
                this.submit_project_form(&field, &event.0, cx);
            },
        ));
        self.sidebar_state.project_name_field = Some(name_field);
        self.sidebar_state.project_path_field = Some(path_field);

        let snooze_field =
            cx.new(|cx| TextField::new("30m, 2h, 1d, or YYYY-MM-DD HH:MM (UTC)", cx));
        self.sidebar_state.subs.push(cx.subscribe(
            &snooze_field,
            |this, _field, event: &PromptSubmitted, cx| {
                this.submit_custom_snooze(&event.0, cx);
            },
        ));
        self.sidebar_state.snooze_field = Some(snooze_field);
        self.sidebar_state.menu_focus = Some(cx.focus_handle());
    }

    fn commit_rename(&mut self, title: &SharedString, cx: &mut Context<Self>) {
        let Some(thread_id) = self.sidebar_state.renaming.take() else {
            return;
        };
        let title = title.trim().to_string();
        if !title.is_empty() {
            if let Some(thread) = self.state.threads.iter_mut().find(|t| t.id == thread_id) {
                thread.title = title.clone().into();
            }
            self.dispatch(json!({
                "type": "thread.metadata.update",
                "commandId": Uuid::new_v4().to_string(),
                "threadId": thread_id.to_string(),
                "title": title,
            }));
        }
        cx.notify();
    }

    /// Open the inline rename editor for `thread_id`. Called from the context
    /// menu listener, which has a `window` for prefill + focus + blur-cancel.
    fn start_rename(
        &mut self,
        thread_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(field) = self.sidebar_state.rename_field.clone() else {
            return;
        };
        let title = self
            .state
            .threads
            .iter()
            .find(|t| t.id == thread_id)
            .map(|t| t.title.to_string())
            .unwrap_or_default();
        field.update(cx, |field, cx| {
            field.reset(window, cx);
            field.replace_text_in_range(None, &title, window, cx);
        });
        let focus = field.read(cx).focus_handle(cx);
        self.sidebar_state.rename_blur = Some(cx.on_blur(&focus, window, |this, _window, cx| {
            this.sidebar_state.renaming = None;
            cx.notify();
        }));
        self.sidebar_state.renaming = Some(thread_id);
        focus.focus(window, cx);
        cx.notify();
    }

    /// Enter pressed in either new-project field. `submitted` carries the
    /// submitting field's text (its content is already reset by the time this
    /// runs); the other field is read out via `field_text`.
    fn submit_project_form(
        &mut self,
        submitter: &Entity<TextField>,
        submitted: &SharedString,
        cx: &mut Context<Self>,
    ) {
        let (Some(name_field), Some(path_field)) = (
            self.sidebar_state.project_name_field.clone(),
            self.sidebar_state.project_path_field.clone(),
        ) else {
            return;
        };
        let name = if submitter == &name_field {
            submitted.to_string()
        } else {
            field_text(&name_field, cx)
        };
        let path = if submitter == &path_field {
            submitted.to_string()
        } else {
            field_text(&path_field, cx)
        };
        if path.trim().is_empty() {
            // Name submitted with no path yet: restore the name (its field was
            // cleared on submit) and move focus to the path field.
            set_field_text(&name_field, name.trim(), cx);
            focus_field(&path_field, cx);
            return;
        }
        self.create_project(name, path, cx);
    }

    /// "Create" button in the new-project form — same as `submit_project_form`
    /// but reads both fields through the listener's `window`.
    fn submit_project_form_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(name_field), Some(path_field)) = (
            self.sidebar_state.project_name_field.clone(),
            self.sidebar_state.project_path_field.clone(),
        ) else {
            return;
        };
        let name = field_text_in(&name_field, window, cx);
        let path = field_text_in(&path_field, window, cx);
        self.create_project(name, path, cx);
    }

    fn create_project(&mut self, name: String, path: String, cx: &mut Context<Self>) {
        let path = path.trim().to_string();
        if path.is_empty() {
            cx.notify();
            return;
        }
        let name = {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                path.rsplit('/')
                    .find(|segment| !segment.is_empty())
                    .unwrap_or("project")
                    .to_string()
            } else {
                trimmed.to_string()
            }
        };
        self.dispatch(json!({
            "type": "project.create",
            "commandId": Uuid::new_v4().to_string(),
            "title": name,
            "workspaceRoot": path,
        }));
        // The server assigns the project id, so an optimistic entry would
        // duplicate the upcoming upsert — only do it in offline mock mode.
        if self.state.connection == ConnectionStatus::Mock {
            self.state.projects.push(Project {
                id: Uuid::new_v4().to_string().into(),
                name: name.clone().into(),
                path: path.clone().into(),
                expanded: true,
            });
        }
        if let (Some(name_field), Some(path_field)) = (
            self.sidebar_state.project_name_field.clone(),
            self.sidebar_state.project_path_field.clone(),
        ) {
            reset_field(&name_field, cx);
            reset_field(&path_field, cx);
        }
        self.sidebar_state.new_project_open = false;
        cx.notify();
    }

    fn new_thread(&mut self, project_id: SharedString, cx: &mut Context<Self>) {
        let thread_id = Uuid::new_v4().to_string();
        self.dispatch(json!({
            "type": "thread.create",
            "commandId": Uuid::new_v4().to_string(),
            "threadId": thread_id,
            "projectId": project_id.to_string(),
            "title": "New thread",
            "modelSelection": {
                "instanceId": self.provider.clone(),
                "model": self.model.clone(),
            },
            "runtimeMode": "full-access",
            "interactionMode": "default",
            "branch": null,
            "worktreePath": null,
            "createdBy": "user",
            "creationSource": "web",
        }));
        // Optimistic entry; the server's ThreadUpserted carries our threadId
        // and replaces it in place.
        self.state.threads.push(Thread {
            id: thread_id.clone().into(),
            project_id,
            title: "New thread".into(),
            status: "idle".into(),
            activity_run_status: None,
            has_pending_request: false,
            has_actionable_proposed_plan: false,
            provider_instances: Vec::new(),
            pinned: false,
            updated_at: "".into(),
            snoozed_until: None,
            marked_unread_at: None,
            visited_at: None,
            latest_run_started_at: None,
            linked_pull_request: None,
            pin_order_key: None,
            active_order_key: None,
            runtime_mode: None,
            interaction_mode: None,
            latest_run_id: None,
        });
        self.select_and_visit(thread_id.into(), cx);
    }

    /// Select a thread and tell the server it was opened — `thread.visit`
    /// moves the read watermark (`visitedAt`) and clears `markedUnreadAt`
    /// server-side, so the optimistic update does both.
    fn select_and_visit(&mut self, thread_id: SharedString, cx: &mut Context<Self>) {
        let now = iso_now();
        if let Some(thread) = self.state.threads.iter_mut().find(|t| t.id == thread_id) {
            thread.visited_at = Some(now.clone().into());
            thread.marked_unread_at = None;
        }
        self.dispatch(json!({
            "type": "thread.visit",
            "commandId": Uuid::new_v4().to_string(),
            "threadId": thread_id.to_string(),
            "visitedAt": now,
            "opened": true,
        }));
        self.select_thread(thread_id, cx);
    }

    /// "Mark unread" menu action — `thread.mark-unread`.
    fn mark_unread(&mut self, thread_id: SharedString, cx: &mut Context<Self>) {
        if let Some(thread) = self.state.threads.iter_mut().find(|t| t.id == thread_id) {
            thread.marked_unread_at = Some(iso_now().into());
        }
        self.dispatch(json!({
            "type": "thread.mark-unread",
            "commandId": Uuid::new_v4().to_string(),
            "threadId": thread_id.to_string(),
        }));
        cx.notify();
    }

    /// Unread state, matching the web row: an explicit `markedUnreadAt` or a
    /// run that started after the last visit. ISO strings compare
    /// lexicographically.
    fn is_unread(&self, thread: &Thread) -> bool {
        thread.marked_unread_at.is_some()
            || thread
                .latest_run_started_at
                .as_ref()
                .is_some_and(|started| {
                    thread
                        .visited_at
                        .as_ref()
                        .is_none_or(|visited| started.as_ref() > visited.as_ref())
                })
    }

    /// True while `snoozedUntil` is still ahead of the clock — the web's
    /// `effectiveSnoozed` check (an expired snooze is already awake).
    fn is_snoozed(&self, thread: &Thread) -> bool {
        thread
            .snoozed_until
            .as_ref()
            .is_some_and(|until| unix_from_iso(until.as_ref()).is_some_and(|t| t > unix_now()))
    }

    /// Dispatch `thread.snooze` and optimistically mark the row.
    fn snooze_thread(&mut self, thread_id: SharedString, until: String, cx: &mut Context<Self>) {
        if let Some(thread) = self.state.threads.iter_mut().find(|t| t.id == thread_id) {
            thread.snoozed_until = Some(until.clone().into());
        }
        self.sidebar_state.snooze_target = None;
        self.sidebar_state.snooze_error = None;
        self.dispatch(json!({
            "type": "thread.snooze",
            "commandId": Uuid::new_v4().to_string(),
            "threadId": thread_id.to_string(),
            "snoozedUntil": until,
        }));
        self.push_toast_with_action(
            ToastKind::Info,
            "Thread snoozed",
            Some(format!("wakes {}", until.get(..16).unwrap_or(&until))),
            Some(thread_id.clone()),
            Some(ToastAction::UnsnoozeThread { thread_id }),
            cx,
        );
    }

    /// Dispatch `thread.unsnooze` (`reason:"user"`, per the contract).
    fn unsnooze_thread(&mut self, thread_id: SharedString, cx: &mut Context<Self>) {
        if let Some(thread) = self.state.threads.iter_mut().find(|t| t.id == thread_id) {
            thread.snoozed_until = None;
        }
        self.sidebar_state.snooze_target = None;
        self.sidebar_state.snooze_error = None;
        self.dispatch(json!({
            "type": "thread.unsnooze",
            "commandId": Uuid::new_v4().to_string(),
            "threadId": thread_id.to_string(),
            "reason": "user",
        }));
        cx.notify();
    }

    /// Open the snooze overlay for `thread_id`: reset the custom-until field
    /// and focus it so typing a duration works immediately. Focus is deferred
    /// one frame — the field's input handler only registers once the overlay
    /// has painted, so an immediate `focus` lands nowhere.
    fn open_snooze_overlay(
        &mut self,
        thread_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_state.snooze_target = Some(thread_id);
        self.sidebar_state.snooze_error = None;
        if let Some(field) = self.sidebar_state.snooze_field.clone() {
            reset_field(&field, cx);
            let focus = field.read(cx).focus_handle(cx);
            window.on_next_frame(move |window, cx| focus.focus(window, cx));
        }
        cx.notify();
    }

    /// Enter in the snooze overlay's custom-until field. Parses durations
    /// ("30m") and UTC datetimes ("YYYY-MM-DD HH:MM"); on a bad or past time
    /// the field text is restored (PromptSubmitted clears it) and an error
    /// line shows instead of closing the overlay.
    fn submit_custom_snooze(&mut self, input: &SharedString, cx: &mut Context<Self>) {
        let Some(thread_id) = self.sidebar_state.snooze_target.clone() else {
            return;
        };
        let now = unix_now();
        match parse_custom_snooze(input.as_ref(), now) {
            Some(until) if until > now => {
                if let Some(field) = self.sidebar_state.snooze_field.clone() {
                    reset_field(&field, cx);
                }
                self.snooze_thread(thread_id, iso_from_unix(until), cx);
            }
            _ => {
                self.sidebar_state.snooze_error =
                    Some("Use 30m/2h/1d or YYYY-MM-DD HH:MM (UTC), in the future".into());
                if let Some(field) = self.sidebar_state.snooze_field.clone() {
                    set_field_text(&field, input.as_ref(), cx);
                }
                cx.notify();
            }
        }
    }

    /// Park the current composer draft + file attachments as a sidebar draft
    /// thread and clear the composer. Only meaningful in new-thread mode —
    /// a draft is the prompt a not-yet-created thread would carry.
    fn save_current_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.selected.is_some() {
            self.push_toast(
                ToastKind::Info,
                "No draft to save",
                Some("Drafts live on the new-thread composer (⌘N)".to_string()),
                None,
                cx,
            );
            return;
        }
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
            self.push_toast(
                ToastKind::Info,
                "Nothing to save",
                Some("Type a prompt or attach files first".to_string()),
                None,
                cx,
            );
            return;
        }
        self.client_config.push_draft_thread(config::DraftThread {
            thread_id: format!("draft:{}", Uuid::new_v4()),
            project_id: self.composer_project_key(),
            prompt,
            files,
            created_at: iso_now(),
        });
        self.composer
            .update(cx, |field, cx| field.reset(window, cx));
        self.composer_state.draft.clear();
        self.composer_state.attachments.clear();
        self.composer_state.history_index = None;
        self.push_toast(ToastKind::Info, "Draft saved", None, None, cx);
        cx.notify();
    }

    /// Open a saved draft: park any composer content first (opening a draft
    /// replaces the composer, so the current text is kept as a draft of its
    /// own instead of being lost), then load the draft into the composer and
    /// consume the stored entry.
    fn open_draft_thread(&mut self, draft_id: String, window: &mut Window, cx: &mut Context<Self>) {
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
        if !prompt.is_empty() || !files.is_empty() {
            self.client_config.push_draft_thread(config::DraftThread {
                thread_id: format!("draft:{}", Uuid::new_v4()),
                project_id: self.composer_project_key(),
                prompt,
                files,
                created_at: iso_now(),
            });
        }
        let Some(draft) = self.client_config.take_draft_thread(&draft_id) else {
            cx.notify();
            return;
        };
        self.state.selected = None;
        self.thread_items.clear();
        self.has_more_history = false;
        self.history_cursor = None;
        self.history_loading = false;
        self.history_error = false;
        self.composer.update(cx, |field, cx| {
            field.reset(window, cx);
            field.replace_text_in_range(None, &draft.prompt, window, cx);
        });
        self.composer_state.draft = draft.prompt;
        self.composer_state.attachments = draft
            .files
            .into_iter()
            .map(|file| Attachment {
                name: file.name,
                kind: AttachmentKind::File { path: file.path },
                thumbnail: None,
            })
            .collect();
        self.composer_state.history_index = None;
        self.composer.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Delete a saved draft (its row's ✕).
    fn delete_draft_thread(&mut self, draft_id: String, cx: &mut Context<Self>) {
        self.client_config.remove_draft_thread(&draft_id);
        cx.notify();
    }

    /// Fetch `icon` for all live projects once per shell event that can
    /// change it (`icons_queried` resets in `observe_shell_event`). Project
    /// shells in this RPC response carry the same `icon` the subscribeShell
    /// snapshot emits — `net::parse_project` just doesn't surface it.
    fn load_project_icons(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_state.icons_queried {
            return;
        }
        self.sidebar_state.icons_queried = true;
        let Some(rx) = self.send_query("orchestration.getArchivedShellSnapshot", json!({})) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let Ok(exit) = rx.recv().await else {
                // Failed query (dropped connection, wedged server): leave
                // `icons_queried` false so the next render retries.
                let _ = this.update(cx, |this, _cx| {
                    this.sidebar_state.icons_queried = false;
                });
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if exit["_tag"].as_str() != Some("Success") {
                    this.sidebar_state.icons_queried = false;
                    cx.notify();
                    return;
                }
                let value = &exit["value"];
                let mut icons = HashMap::new();
                let mut favicons = HashMap::new();
                if let Some(projects) = value.get("projects").and_then(Value::as_array) {
                    for project in projects {
                        let id = project.get("id").and_then(Value::as_str);
                        // `icon` is the legacy alias; the shell contract
                        // field is `projectIcon`.
                        let icon = project
                            .get("icon")
                            .or_else(|| project.get("projectIcon"))
                            .filter(|v| !v.is_null());
                        if let (Some(id), Some(icon)) = (id, icon) {
                            icons.insert(id.to_string(), icon.clone());
                        }
                        let favicon = project
                            .get("faviconPath")
                            .and_then(Value::as_str)
                            .filter(|p| !p.is_empty());
                        if let (Some(id), Some(path)) = (id, favicon) {
                            favicons.insert(id.to_string(), path.to_string());
                        }
                    }
                }
                this.sidebar_state.project_icons = icons;
                // Drop cached favicon state whose path changed or went away;
                // surviving entries keep their loaded image.
                let favicon_state = &mut this.sidebar_state;
                favicon_state
                    .favicon_images
                    .retain(|id, (path, _)| favicons.get(id) == Some(path));
                favicon_state
                    .favicon_loading
                    .retain(|id, path| favicons.get(id) == Some(path));
                favicon_state
                    .favicon_failed
                    .retain(|id, path| favicons.get(id) == Some(path));
                favicon_state.project_favicons = favicons;
                cx.notify();
            });
        })
        .detach();
    }

    /// Merge one raw thread-shell JSON into `thread_meta` (used by the
    /// metadata shell stream and the older-settled listing).
    fn harvest_thread_meta(meta: &mut HashMap<String, ThreadMeta>, shell: &Value) {
        let Some(id) = shell.get("id").and_then(Value::as_str) else {
            return;
        };
        let entry = meta.entry(id.to_string()).or_default();
        if let Some(override_) = shell.get("settledOverride").and_then(Value::as_str) {
            entry.settled = override_ == "settled";
        }
        if let Some(at) = shell.get("settledAt").and_then(Value::as_str) {
            entry.settled_at = Some(at.to_string());
        }
        if let Some(branch) = shell.get("branch") {
            entry.branch = branch
                .as_str()
                .filter(|b| !b.is_empty())
                .map(str::to_string);
        }
        if let Some(worktree) = shell.get("worktreePath") {
            entry.worktree_path = worktree
                .as_str()
                .filter(|w| !w.is_empty())
                .map(str::to_string);
        }
        if let Some(title) = shell.get("title").and_then(Value::as_str) {
            entry.title = title.to_string();
        }
        if let Some(project) = shell.get("projectId").and_then(Value::as_str) {
            entry.project_id = project.to_string();
        }
        if shell.get("pinnedAt").is_some_and(|p| !p.is_null()) {
            entry.pinned = true;
        }
        // Status + card fields the parsed Thread model does not carry —
        // the card's topStatus/meta-line resolution mirrors
        // `resolveSidebarThreadStatus` on these.
        if let Some(kind) = shell["pendingRuntimeRequest"]["kind"].as_str() {
            entry.pending_request_kind = Some(kind.to_string());
        } else if shell.get("pendingRuntimeRequest").is_some_and(|v| v.is_null()) {
            entry.pending_request_kind = None;
        }
        if let Some(subagent) = shell.get("pendingSubagentRequest") {
            entry.pending_subagent_request = subagent.as_str().map(str::to_string);
        }
        if let Some(id) = shell.get("activeProviderThreadId") {
            entry.active_provider_thread_id = id.as_str().map(str::to_string);
        }
        if let Some(count) = shell["pendingGuestMessageCount"].as_i64() {
            entry.pending_guest_messages = count;
        }
        if let Some(blocked) = shell["blockedOnThreadWait"].as_bool() {
            entry.blocked_on_thread_wait = blocked;
        }
        if let Some(class) = shell.get("lastErrorClass") {
            entry.last_error_class = class.as_str().map(str::to_string);
        }
        if let Some(status) = shell["goal"]["status"].as_str() {
            entry.goal_active = status == "active";
        }
        if let Some(at) = shell.get("activityRunStartedAt") {
            entry.activity_run_started_at = at.as_str().map(str::to_string);
        }
        if let Some(id) = shell.get("activeRunId") {
            entry.active_run_id = id.as_str().map(str::to_string);
        }
        if let Some(at) = shell.get("latestRunRequestedAt") {
            entry.latest_run_requested_at = at.as_str().map(str::to_string);
        }
        if let Some(at) = shell.get("latestRunCompletedAt") {
            entry.latest_run_completed_at = at.as_str().map(str::to_string);
        }
        if let Some(regen) = shell.get("titleRegeneration") {
            entry.title_regenerating = !regen.is_null();
        }
        if let Some(disabled) = shell.get("autoSettleDisabledAt") {
            entry.auto_settle_disabled = !disabled.is_null();
        }
        if let Some(id) = shell.get("providerInstanceId") {
            entry.provider_instance_id = id.as_str().map(str::to_string);
        }
        if let Some(id) = shell["modelSelection"]["instanceId"].as_str() {
            entry.model_instance_id = Some(id.to_string());
        }
        if let Some(prs) = shell["pullRequests"].as_array() {
            entry.pull_request_count = prs.len();
        }
    }

    /// Harvest `settledOverride`/`settledAt`/`branch`/`pinnedAt` that
    /// `net::parse_thread` drops: a second raw `subscribeShell` stream keeps
    /// them live, and `orchestration.getOlderSettledThreads` covers settled
    /// shells outside the bounded snapshot — the same out-of-band pattern as
    /// `load_project_icons`.
    fn load_thread_meta(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_state.meta_started {
            return;
        }
        self.sidebar_state.meta_started = true;

        if let Some((_, rx)) = self.open_stream(
            "orchestration.subscribeShell",
            json!({ "requestCompletionMarker": true }),
        ) {
            cx.spawn(async move |this, cx| {
                while let Ok(value) = rx.recv().await {
                    let _ = this.update(cx, |this, cx| {
                        let meta = &mut this.sidebar_state.thread_meta;
                        let mut changed = false;
                        match value.get("kind").and_then(Value::as_str) {
                            Some("snapshot") | Some("bootstrap") => {
                                if let Some(threads) = value["snapshot"]["threads"].as_array() {
                                    for shell in threads {
                                        Self::harvest_thread_meta(meta, shell);
                                    }
                                    changed = true;
                                }
                            }
                            Some("thread.updated") | Some("thread.upserted") => {
                                Self::harvest_thread_meta(meta, &value["thread"]);
                                changed = true;
                            }
                            Some("thread.removed") => {
                                if let Some(id) = value["threadId"].as_str().map(str::to_string) {
                                    changed = meta.remove(&id).is_some();
                                }
                            }
                            _ => {}
                        }
                        if changed {
                            cx.notify();
                        }
                    });
                }
            })
            .detach();
        }

        if let Some(rx) = self.send_query(
            "orchestration.getOlderSettledThreads",
            json!({ "before": { "settledAt": "2999-01-01T00:00:00.000Z" }, "limit": 200 }),
        ) {
            cx.spawn(async move |this, cx| {
                let Ok(exit) = rx.recv().await else {
                    return;
                };
                let _ = this.update(cx, |this, cx| {
                    if exit["_tag"].as_str() != Some("Success") {
                        return;
                    }
                    let Some(threads) = exit["value"]["threads"].as_array() else {
                        return;
                    };
                    let meta = &mut this.sidebar_state.thread_meta;
                    for shell in threads {
                        Self::harvest_thread_meta(meta, shell);
                        if let Some(id) = shell.get("id").and_then(Value::as_str)
                            && let Some(entry) = meta.get_mut(id)
                        {
                            entry.settled = true;
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// `server.getConfig` once per connection — supplies the environment id
    /// (thread links), the capabilities that gate context-menu items
    /// (`threadAutoSettleOptOut`, `threadProjectFromScratch`, `threadSnooze`,
    /// `threadSettlement`), `scratchWorkspaceRoot` for the "No project"
    /// treatment, and the machine descriptor for remote-env icons.
    fn load_server_config(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_state.config_queried {
            return;
        }
        self.sidebar_state.config_queried = true;
        let Some(rx) = self.send_query("server.getConfig", json!({})) else {
            self.sidebar_state.config_queried = false;
            return;
        };
        cx.spawn(async move |this, cx| {
            let Ok(exit) = rx.recv().await else {
                let _ = this.update(cx, |this, _cx| {
                    this.sidebar_state.config_queried = false;
                });
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if exit["_tag"].as_str() == Some("Success") {
                    this.sidebar_state.server_config = Some(exit["value"].clone());
                } else {
                    this.sidebar_state.config_queried = false;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// One capability flag from the harvested `server.getConfig` payload.
    fn server_capability(&self, name: &str) -> bool {
        self.sidebar_state
            .server_config
            .as_ref()
            .is_some_and(|c| c["environment"]["capabilities"][name].as_bool() == Some(true))
    }

    /// The connected environment's id (for hosted thread links).
    fn environment_id(&self) -> Option<String> {
        self.sidebar_state
            .server_config
            .as_ref()
            .and_then(|c| c["environment"]["environmentId"].as_str())
            .map(str::to_string)
    }

    /// `scratchWorkspaceRoot` — a project whose workspace root sits under it
    /// is a "No project" scratch project (web `isScratchProject`).
    fn is_scratch_project(&self, project: &Project) -> bool {
        let Some(root) = self
            .sidebar_state
            .server_config
            .as_ref()
            .and_then(|c| c["scratchWorkspaceRoot"].as_str())
        else {
            // Without config we can't prove a scratch root; "No project"
            // threads still get the dashed chip via their own project icon.
            return project.name.as_ref() == "No project";
        };
        !project.path.is_empty() && project.path.starts_with(root)
    }

    /// Web `resolveThreadProviderStack` + `SidebarProviderStack`: prior
    /// provider-instance ids first (deduped, last two), the current
    /// `runtime.providerInstanceId ?? modelSelection.instanceId` last.
    /// Returns `(glyph, initials, is_current)` entries; an unknown current
    /// instance collapses the stack to nothing, like the web's `null`.
    fn provider_stack(
        &self,
        thread: &Thread,
        meta: Option<&ThreadMeta>,
    ) -> Vec<(Option<Icon>, String, bool)> {
        // Web `resolveThreadProviderStack`: the current instance is
        // `runtime?.providerInstanceId ?? modelSelection.instanceId` — the
        // provider id only counts while a runtime summary exists.
        let current = meta
            .and_then(|m| {
                if runtime_status(thread, meta).is_some() {
                    m.provider_instance_id
                        .clone()
                        .or_else(|| m.model_instance_id.clone())
                } else {
                    m.model_instance_id.clone()
                }
            })
            .or_else(|| thread.provider_instances.last().map(|id| id.to_string()));
        let Some(current) = current else {
            return Vec::new();
        };
        let entry_for = |id: &str| {
            self.composer_state
                .config_providers
                .iter()
                .find(|p| p["instanceId"].as_str() == Some(id))
        };
        let Some(current_entry) = entry_for(&current) else {
            return Vec::new();
        };
        let initials_of = |entry: &serde_json::Value, id: &str| -> String {
            let name = entry["name"]
                .as_str()
                .or_else(|| entry["displayName"].as_str())
                .unwrap_or(id);
            provider_initials_for(name)
        };
        let mut history: Vec<(Option<Icon>, String, bool)> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for id in &thread.provider_instances {
            let id = id.as_ref();
            if id == current || seen.contains(&id) {
                continue;
            }
            let Some(entry) = entry_for(id) else {
                continue;
            };
            seen.push(id);
            history.push((
                provider_glyph_icon(entry["driver"].as_str()),
                initials_of(entry, id),
                false,
            ));
        }
        // Web caps the stack at the most recent three instances.
        let mut history = history.into_iter().rev().take(2).rev().collect::<Vec<_>>();
        history.push((
            provider_glyph_icon(current_entry["driver"].as_str()),
            initials_of(current_entry, &current),
            true,
        ));
        history
    }

    /// Resolve the 16px project avatar the way the web header does: decoded
    /// favicon, else configured icon chip, else the derived monogram tile.
    /// Shared by the folder header, settled rows, and the topbar breadcrumb.
    pub(crate) fn project_chip(&self, project: &Project) -> AnyElement {
        self.sidebar_state
            .favicon_images
            .get(project.id.as_ref())
            .map(|(_, image)| favicon_chip(image))
            .or_else(|| {
                self.sidebar_state
                    .project_icons
                    .get(project.id.as_ref())
                    .and_then(|icon| project_icon_chip(icon, project.name.as_ref()))
            })
            .unwrap_or_else(|| {
                // Scratch projects default to the gray dashed-square mark
                // (`SCRATCH_PROJECT_ICON` in the web settings defaults).
                if self.is_scratch_project(project) {
                    div()
                        .size_4()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            icon(Icon::MessageSquareDashed, 14.)
                                .text_color(theme::sidebar_icon()),
                        )
                        .into_any_element()
                } else {
                    project_monogram_chip(project.name.as_ref())
                }
            })
    }

    /// Web `isThreadWorking` — the Working shelf predicate: no pending
    /// request or subagent call, an active-or-idle runtime, and a latest run
    /// that hasn't settled (unless a plan proposal is waiting on it).
    fn is_working_thread(&self, thread: &Thread) -> bool {
        let meta = self.sidebar_state.thread_meta.get(thread.id.as_ref());
        if thread.has_pending_request || meta.is_some_and(|m| m.pending_subagent_request.is_some()) {
            return false;
        }
        let Some(status) = runtime_status(thread, meta) else {
            return false;
        };
        if !matches!(
            status,
            "idle" | "preparing" | "queued" | "starting" | "running" | "waiting"
        ) {
            return false;
        }
        // The latest run is settled when it isn't in an active status and
        // the runtime's live run isn't it.
        let run_settled = thread.latest_run_id.is_some()
            && !matches!(
                status,
                "preparing" | "queued" | "starting" | "running" | "waiting"
            )
            && meta.and_then(|m| m.active_run_id.as_deref())
                != thread.latest_run_id.as_deref();
        if thread.interaction_mode.as_deref() == Some("plan")
            && thread.has_actionable_proposed_plan
            && run_settled
        {
            return false;
        }
        true
    }

    /// Whether the harvested shell says this thread sits on the settled
    /// shelf (`settledOverride == "settled"` or an older-settled row).
    fn is_settled(&self, thread_id: &str) -> bool {
        self.sidebar_state
            .thread_meta
            .get(thread_id)
            .is_some_and(|m| m.settled)
    }

    /// Dispatch `thread.settle` and optimistically move the row to the
    /// settled shelf — same shape as `snooze_thread`.
    fn settle_thread(&mut self, thread_id: SharedString, cx: &mut Context<Self>) {
        let now = iso_now();
        {
            let entry = self
                .sidebar_state
                .thread_meta
                .entry(thread_id.to_string())
                .or_default();
            entry.settled = true;
            if entry.settled_at.is_none() {
                entry.settled_at = Some(now);
            }
        }
        self.dispatch(json!({
            "type": "thread.settle",
            "commandId": Uuid::new_v4().to_string(),
            "threadId": thread_id.to_string(),
        }));
        cx.notify();
    }

    /// Kick off favicon fetches for projects whose harvested `faviconPath`
    /// isn't cached, in flight, or known-failed. Each load is the web's
    /// `project-favicon` asset flow: `assets.createUrl {cwd, path}` then a
    /// bearer-authed GET decoded through explorer's image pipeline. Runs
    /// from `render_sidebar` (which has `&mut self`).
    fn load_project_favicons(&mut self, cx: &mut Context<Self>) {
        let Some(endpoint) = self.active_endpoint.clone() else {
            return;
        };
        for project in &self.state.projects {
            let id = project.id.to_string();
            let Some(path) = self.sidebar_state.project_favicons.get(&id).cloned() else {
                continue;
            };
            if self
                .sidebar_state
                .favicon_images
                .get(&id)
                .is_some_and(|(p, _)| p == &path)
                || self.sidebar_state.favicon_loading.get(&id) == Some(&path)
                || self.sidebar_state.favicon_failed.get(&id) == Some(&path)
            {
                continue;
            }
            self.sidebar_state
                .favicon_loading
                .insert(id.clone(), path.clone());
            let Some(rx) = self.send_query(
                "assets.createUrl",
                json!({
                    "resource": {
                        "_tag": "project-favicon",
                        "cwd": project.path.to_string(),
                        "path": path,
                    }
                }),
            ) else {
                self.sidebar_state.favicon_loading.remove(&id);
                continue;
            };
            let ep = endpoint.clone();
            cx.spawn(async move |this, cx| {
                let exit = rx
                    .recv()
                    .await
                    .unwrap_or_else(|_| json!({"_tag": "Failure"}));
                let result = if exit["_tag"].as_str() == Some("Success") {
                    match exit["value"]["relativeUrl"].as_str() {
                        Some(rel) => {
                            let url = format!("{}{}", ep.http_url.trim_end_matches('/'), rel);
                            smol::unblock(move || crate::explorer::fetch_image(&ep, &url))
                                .await
                                .map(|(image, _)| image)
                                .map_err(|_| ())
                        }
                        None => Err(()),
                    }
                } else {
                    Err(())
                };
                let _ = this.update(cx, |this, cx| {
                    // A newer harvest may have changed the path or removed
                    // the project — only land the image when the in-flight
                    // marker still matches what we fetched.
                    if this.sidebar_state.favicon_loading.get(&id) == Some(&path) {
                        this.sidebar_state.favicon_loading.remove(&id);
                        match result {
                            Ok(image) => {
                                this.sidebar_state.favicon_images.insert(id, (path, image));
                            }
                            Err(()) => {
                                this.sidebar_state.favicon_failed.insert(id, path);
                            }
                        }
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// Pinned threads of `project_id` in display order, mirroring the web's
    /// `sortPinnedThreadsByOrderKey`: keyed rows by `pinOrderKey` ascending,
    /// then keyless rows by recency.
    fn pinned_order(&self, project_id: &str) -> Vec<Thread> {
        let mut pinned: Vec<Thread> = self
            .state
            .threads
            .iter()
            .filter(|t| t.pinned && t.project_id.as_ref() == project_id)
            .cloned()
            .collect();
        pinned.sort_by_key(|t| {
            (
                t.pin_order_key.is_none(),
                t.pin_order_key.clone().unwrap_or_default(),
                Reverse(t.updated_at.clone()),
            )
        });
        pinned
    }

    /// Optimistically set a `pinOrderKey` and send `thread.pin.reorder`.
    fn set_pin_key(&mut self, thread_id: &str, order_key: String) {
        if let Some(thread) = self
            .state
            .threads
            .iter_mut()
            .find(|t| t.id.as_ref() == thread_id)
        {
            thread.pin_order_key = Some(order_key.clone().into());
        }
        self.dispatch(json!({
            "type": "thread.pin.reorder",
            "commandId": Uuid::new_v4().to_string(),
            "threadId": thread_id,
            "orderKey": order_key,
        }));
    }

    /// Persist a pinned block's final order: `ordered` is the display order
    /// after the move and `moved_index` the slot of the row whose position
    /// changed. With usable neighbor keys the moved row takes the midpoint
    /// key; with missing/corrupt keys the whole block is respread — the same
    /// fallback as the web's `planPinnedReorder`. Shared by `move_pinned`
    /// (context menu) and `drop_pinned` (pointer drag).
    fn commit_pinned_order(&mut self, ordered: Vec<Thread>, moved_index: usize) {
        let n = ordered.len();
        // Neighbors of the moved row in the *final* order; an existing
        // neighbor is only usable when it carries a valid fraction key,
        // missing bounds are open ends.
        let before_index = moved_index.checked_sub(1);
        let after_index = (moved_index + 1 < n).then_some(moved_index + 1);
        let usable = |index: Option<usize>| {
            index.is_none_or(|i| {
                ordered[i]
                    .pin_order_key
                    .as_deref()
                    .is_some_and(is_valid_order_key)
            })
        };
        if usable(before_index) && usable(after_index) {
            let before =
                before_index.and_then(|i| ordered[i].pin_order_key.as_deref().map(str::to_owned));
            let after =
                after_index.and_then(|i| ordered[i].pin_order_key.as_deref().map(str::to_owned));
            let order_key = order_key_between(before.as_deref(), after.as_deref());
            let moved_id = ordered[moved_index].id.clone();
            self.set_pin_key(moved_id.as_ref(), order_key);
        } else {
            // Respread: fresh slot keys across the whole pinned block in its
            // new order — also heals non-fraction (e.g. ISO) legacy keys.
            for (i, row) in ordered.iter().enumerate() {
                let order_key = spread_slot_key(i, n);
                if row.pin_order_key.as_deref() != Some(order_key.as_str()) {
                    self.set_pin_key(row.id.as_ref(), order_key);
                }
            }
        }
    }

    /// Drop handler for pointer-dragged pinned rows: dropping a row onto
    /// another pinned row of the same project inserts it at that row's slot —
    /// after the target when dragging down, before when dragging up (a
    /// post-removal `insert` at the target index lands both ways).
    fn drop_pinned(
        &mut self,
        drag_id: SharedString,
        target_id: SharedString,
        cx: &mut Context<Self>,
    ) {
        if drag_id == target_id {
            return;
        }
        let Some(project_id) = self
            .state
            .threads
            .iter()
            .find(|t| t.id == drag_id)
            .map(|t| t.project_id.clone())
        else {
            return;
        };
        let mut ordered = self.pinned_order(project_id.as_ref());
        let (Some(drag_ix), Some(target_ix)) = (
            ordered.iter().position(|t| t.id == drag_id),
            ordered.iter().position(|t| t.id == target_id),
        ) else {
            return;
        };
        let row = ordered.remove(drag_ix);
        ordered.insert(target_ix, row);
        self.commit_pinned_order(ordered, target_ix);
        cx.notify();
    }

    /// A context-menu row — web `ContextMenuItem`: `min-h-7 gap-2 rounded-sm
    /// px-2 py-1 text-sm`, `bg-accent` + `text-accent-foreground` on
    /// highlight, a `size-4` `text-muted-foreground` leading icon column,
    /// error tint for destructive items, muted + inert when disabled, and a
    /// trailing `chevron-right` for items with children.
    fn menu_item(
        &self,
        id: &'static str,
        leading: Icon,
        label: impl Into<SharedString>,
        danger: bool,
        submenu: bool,
        disabled: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &MouseDownEvent, &mut Window, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        let mut item = div()
            .id(id)
            .min_h(px(28.))
            .flex()
            .items_center()
            .gap_2()
            .rounded_sm()
            .px_2()
            .py_1()
            .when(danger, |el| el.text_color(theme::error_fg()))
            .when(disabled, |el| {
                el.text_color(theme::fg_muted_soft()).opacity(0.5)
            })
            .when(!disabled, |el| {
                el.cursor_pointer().hover(move |s| {
                    if danger {
                        s.bg(theme::destructive_tint())
                            .text_color(theme::error_fg())
                    } else {
                        s.bg(theme::accent()).text_color(theme::accent_fg())
                    }
                })
            })
            // The svg keeps its own ink so the accent-highlighted row text
            // doesn't drag it along (web icons stay muted on highlight).
            .child(icon(leading, 16.).flex_none().text_color(if danger {
                theme::error_fg()
            } else {
                theme::fg_muted()
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_x_hidden()
                    .text_ellipsis()
                    .child(label.into()),
            );
        if !disabled {
            item = item.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev, window, cx| {
                    cx.stop_propagation();
                    on_click(this, ev, window, cx);
                }),
            );
        }
        if submenu {
            item = item.child(
                icon(Icon::ChevronRight, 16.)
                    .flex_none()
                    .text_color(theme::fg_muted()),
            );
        }
        item
    }

    /// A submenu row — web `ContextMenuItem` children: a `checked` row leads
    /// with a size-4 Check (or a blank slot), other rows lead with their
    /// `size-4` icon; `detail` is the muted right caption (snooze wake
    /// labels).
    fn submenu_item(
        &self,
        id: impl Into<ElementId>,
        leading: Option<Icon>,
        checked: Option<bool>,
        label: impl Into<SharedString>,
        detail: Option<String>,
        disabled: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &MouseDownEvent, &mut Window, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        let mut item = div()
            .id(id)
            .min_h(px(28.))
            .flex()
            .items_center()
            .gap_2()
            .rounded_sm()
            .px_2()
            .py_1()
            .when(disabled, |el| el.opacity(0.5))
            .when(!disabled, |el| {
                el.cursor_pointer()
                    .hover(|s| s.bg(theme::accent()).text_color(theme::accent_fg()))
            })
            // The leading column holds the 16px slot every row takes.
            .child(
                div()
                    .w_4()
                    .h_4()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if let Some(glyph) = leading {
                        icon(glyph, 14.)
                            .text_color(theme::fg_muted())
                            .into_any_element()
                    } else if checked == Some(true) {
                        icon(Icon::Check, 14.)
                            .text_color(theme::fg())
                            .into_any_element()
                    } else {
                        icon(Icon::Check, 14.)
                            .text_color(gpui::transparent_black())
                            .into_any_element()
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_x_hidden()
                    .text_ellipsis()
                    .child(label.into()),
            );
        if let Some(detail) = detail {
            item = item.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(detail),
            );
        }
        if !disabled {
            item = item.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev, window, cx| {
                    cx.stop_propagation();
                    on_click(this, ev, window, cx);
                }),
            );
        }
        item
    }

    /// A sidebar thread row. `in_folder` indents the title under the project
    /// name like the web's folder variant (`ps-9`); search results render
    /// flush (`px-2.5`) with the project name trailing.
    fn render_thread_row(
        &self,
        thread: &Thread,
        row_id: usize,
        project_name: Option<SharedString>,
        in_folder: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.state.selected.as_ref() == Some(&thread.id);
        let renaming = self.sidebar_state.renaming.as_ref() == Some(&thread.id);
        let unread = self.is_unread(thread);
        let snoozed = self.is_snoozed(thread);
        // The web's `shouldRecedeSidebarThread`: working/waiting rows and
        // settled read rows dim; active, unread and attention-seeking rows
        // stay at medium weight.
        let receded = !active
            && !thread.has_pending_request
            && !thread.has_actionable_proposed_plan
            && (thread.running() || !unread);
        // Web slim-row title ink: receded → secondary-label, unread →
        // muted-foreground, quiet → secondary-label/70, else foreground.
        let title_color = if receded {
            theme::sidebar_secondary_label()
        } else if active || thread.has_pending_request || thread.has_actionable_proposed_plan {
            theme::sidebar_fg()
        } else if unread {
            theme::sidebar_fg_muted()
        } else {
            theme::sidebar_faint()
        };
        let select_id = thread.id.clone();
        let menu_id = thread.id.clone();
        let settle_id = thread.id.clone();
        let meta = self.sidebar_state.thread_meta.get(thread.id.as_ref());
        let branch = meta
            .and_then(|m| m.branch.clone())
            .filter(|b| !b.is_empty());
        let rel_time = rel_time_label(thread.updated_at.as_ref());

        let mut row = div()
            .id(("thread", row_id))
            .group("thread-row")
            .relative()
            .when(thread.pinned, |row| {
                // Pinned rows are pointer-draggable and accept drops from
                // other pinned rows of the same project.
                let drag = SidebarThreadDrag {
                    thread_id: thread.id.clone(),
                    project_id: thread.project_id.clone(),
                };
                let title = thread.title.clone();
                let drop_project = thread.project_id.clone();
                let drop_target = thread.id.clone();
                row.on_drag(drag, move |_drag, _offset, _window, cx| {
                    cx.new(|_cx| ThreadDragGhost {
                        title: title.clone(),
                    })
                })
                .can_drop(move |value, _window, _cx| {
                    value
                        .downcast_ref::<SidebarThreadDrag>()
                        .is_some_and(|d| d.project_id == drop_project)
                })
                .drag_over::<SidebarThreadDrag>(|style, _drag, _window, _cx| {
                    style.bg(theme::sidebar_selected())
                })
                .on_drop::<SidebarThreadDrag>(move |drag, window, cx| {
                    let Some(view) = window.root::<AppView>().flatten() else {
                        return;
                    };
                    let target_id = drop_target.clone();
                    let drag_id = drag.thread_id.clone();
                    view.update(cx, |this, cx| {
                        this.drop_pinned(drag_id, target_id, cx);
                    });
                })
            })
            .flex()
            .items_center()
            .when(branch.is_some(), |s| s.h_8())
            .when(branch.is_none(), |s| s.h_9())
            .gap_2p5()
            .px_2p5()
            .when(in_folder, |row| row.pl_9())
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .when(active, |s| {
                s.bg(theme::sidebar_active())
                    .text_color(theme::sidebar_fg())
            })
            .when(!active, |s| {
                s.text_color(theme::sidebar_fg())
                    // Restores the receded row's opacity on hover, like
                    // `hover:opacity-100` on the web.
                    .hover(|s| s.bg(theme::sidebar_hover()).opacity(1.))
            })
            // Receded working rows render at 70% opacity on the web.
            .when(receded && thread.running(), |s| s.opacity(0.7))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _window, cx| {
                    if this.sidebar_state.renaming.as_ref() != Some(&select_id) {
                        this.sidebar_state.renaming = None;
                    }
                    this.select_and_visit(select_id.clone(), cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &gpui::MouseDownEvent, window, cx| {
                    this.sidebar_state.context_menu = Some(ContextMenu {
                        thread_id: menu_id.clone(),
                        position: ev.position,
                        open_submenu: None,
                    });
                    this.sidebar_state.delete_confirm = None;
                    if let Some(focus) = this.sidebar_state.menu_focus.clone() {
                        focus.focus(window, cx);
                    }
                    this.select_and_visit(menu_id.clone(), cx);
                }),
            );

        if let Some(color) = status_dot_color(thread, unread) {
            let dot = div().w(px(6.)).h(px(6.)).rounded_full().bg(color);
            row = row.child(if in_folder {
                // Folder rows have no leading icon in the web; the dot floats
                // in the `ps-9` gutter, centered under the project favicon.
                dot.absolute().left(px(15.)).top(px(15.))
            } else {
                dot.flex_none()
            });
        }

        if renaming {
            if let Some(field) = self.sidebar_state.rename_field.clone() {
                row = row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(theme::sidebar_fg())
                        .child(field),
                );
            }
        } else {
            row = row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_x_hidden()
                    .text_ellipsis()
                    .when(!receded, |s| s.font_weight(gpui::FontWeight::MEDIUM))
                    .text_color(title_color)
                    .child(thread.title.clone()),
            );
        }

        if let Some(name) = project_name {
            row = row.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme::sidebar_secondary_label())
                    .child(name),
            );
        }
        if snoozed {
            // Web's snoozed affordance is an alarm-clock glyph + wake label
            // in info ink; the clock is the closest bundled lucide.
            row = row.child(
                icon(Icon::Clock, 12.)
                    .flex_none()
                    .text_color(theme::info_fg()),
            );
        }
        if thread
            .linked_pull_request
            .as_ref()
            .and_then(|pr| pr.get("url"))
            .and_then(|url| url.as_str())
            .is_some()
        {
            // GPUI rows have no tooltip support — the glyph only.
            row = row.child(
                icon(Icon::GitPullRequest, 12.)
                    .flex_none()
                    .text_color(theme::sidebar_secondary_label()),
            );
        }
        if thread.has_pending_request {
            row = row.child(
                icon(Icon::TriangleAlert, 12.)
                    .flex_none()
                    .text_color(theme::warning_fg()),
            );
        }
        if thread.has_actionable_proposed_plan {
            row = row.child(
                div()
                    .flex_none()
                    .px_1()
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(0xa78bfa))
                    .text_xs()
                    .text_color(rgb(0xa78bfa))
                    .child("Plan"),
            );
        }
        if thread.pinned {
            row = row.child(
                icon(Icon::Pin, 12.)
                    .flex_none()
                    .text_color(theme::sidebar_faint()),
            );
        }

        // Right-side status glyph — web `SidebarThreadStatusIcon` order:
        // pending request > proposed plan > working > failed > done/idle.
        let (status_icon, status_color): (Icon, gpui::Rgba) = if thread.has_pending_request {
            (Icon::CircleHelp, theme::warning_fg())
        } else if thread.has_actionable_proposed_plan {
            (Icon::Shield, rgb(0xa78bfa))
        } else if thread.running() {
            (Icon::CircleDot, theme::info_fg())
        } else if matches!(thread.status.as_ref(), "failed" | "error") {
            (Icon::CircleAlert, theme::error_fg())
        } else if matches!(thread.status.as_ref(), "completed") {
            (Icon::CircleCheck, theme::sidebar_icon())
        } else {
            (Icon::CircleDashed, theme::sidebar_faint())
        };
        row = row.child(icon(status_icon, 12.).flex_none().text_color(status_color));
        // Web `RelativeTimeLabel`: fixed-width muted day/time stamp at the
        // row's right edge.
        if !rel_time.is_empty() {
            row = row.child(
                div()
                    .flex_none()
                    .min_w(px(26.))
                    .text_right()
                    .text_xs()
                    .text_color(theme::sidebar_fg_muted())
                    .child(rel_time),
            );
        }

        // Hover/selected "Settle" pill — web's `SidebarSweepButton`
        // (check + "Settle"), covering the right-side stamps on hover.
        if !self.is_settled(thread.id.as_ref()) {
            row = row.child(
                div()
                    .id(("settle", row_id))
                    .group("settle-pill")
                    .absolute()
                    .right(px(6.))
                    .top(px(6.))
                    .flex_none()
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded_md()
                    .px_2()
                    .bg(theme::sidebar_active())
                    .border_1()
                    .border_color(theme::border())
                    .text_xs()
                    .text_color(theme::sidebar_fg_muted())
                    .cursor_pointer()
                    .opacity(if active { 1.0 } else { 0.0 })
                    .group_hover("thread-row", |s| s.opacity(1.))
                    .hover(|s| s.text_color(theme::sidebar_fg()).bg(theme::sidebar_hover()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            cx.stop_propagation();
                            this.settle_thread(settle_id.clone(), cx);
                        }),
                    )
                    .child(hover_icon(
                        Icon::Check,
                        10.,
                        theme::sidebar_fg_muted(),
                        theme::sidebar_fg(),
                        "settle-pill",
                    ))
                    .child("Settle"),
            );
        }

        // The web row grows a second line (`GitBranchIcon` + branch) when
        // the thread's worktree reports a branch.
        if let Some(branch) = branch {
            let mut stack = div()
                .flex()
                .flex_col()
                // Selected two-line rows highlight as one card.
                .when(active, |s| s.bg(theme::sidebar_active()).rounded_md())
                .child(row);
            stack = stack.child(
                div()
                    .flex()
                    .items_center()
                    .h_5()
                    .gap_1p5()
                    .px_2p5()
                    .when(in_folder, |el| el.pl_9())
                    .text_xs()
                    .text_color(theme::sidebar_fg_muted())
                    .child(
                        icon(Icon::GitBranch, 11.)
                            .flex_none()
                            .text_color(theme::sidebar_fg_muted()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .whitespace_nowrap()
                            .overflow_x_hidden()
                            .text_ellipsis()
                            .child(branch),
                    ),
            );
            return stack.into_any_element();
        }
        row.into_any_element()
    }

    /// Web default (`data-testid="sidebar-row-card"`): every active thread is
    /// its own rounded card — `[favicon][pin?] project-name · rel-time` on the
    /// first h-5 line, `title · status` on the second (`mt-1`), an optional
    /// `GitBranch` meta line, and a hover-only snooze + "✓ Settle" cluster
    /// covering the time slot.
    /// A bottom-shelf header — web `CollapsibleSectionHeader` in a
    /// `SidebarSectionHeader` (`mx-0.5 h-8`, `px-2 gap-2 text-xs
    /// font-medium` muted label, hairline rule swapped for the count, a
    /// size-3 chevron). `mt-auto` on the first shelf pins the stack to the
    /// list bottom like the web markers.
    fn render_shelf_header(
        &self,
        kind: &'static str,
        label: &'static str,
        count: usize,
        open: bool,
        mt_auto: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("shelf-{kind}")))
            .mx(px(2.))
            .when(mt_auto, |s| s.mt_auto())
            .h_8()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .rounded_md()
            .cursor_pointer()
            .text_xs()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme::sidebar_fg_muted())
            .hover(|s| s.bg(theme::sidebar_hover()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _window, cx| {
                    match kind {
                        "working" => {
                            this.sidebar_state.working_open =
                                !this.sidebar_state.working_open;
                        }
                        "snoozed" => {
                            this.sidebar_state.snoozed_open =
                                !this.sidebar_state.snoozed_open;
                        }
                        _ => {
                            this.sidebar_state.settled_open =
                                !this.sidebar_state.settled_open;
                        }
                    }
                    cx.notify();
                }),
            )
            .child(label)
            // The hairline rule goes invisible when the count accessory
            // renders, per the web header.
            .child(div().flex_1().min_w(px(8.)).h(px(1.)))
            .child(
                div()
                    .flex_none()
                    .font_weight(gpui::FontWeight::NORMAL)
                    .child(format!("{count}")),
            )
            .child(
                if open {
                    icon(Icon::ChevronDown, 12.)
                } else {
                    icon(Icon::ChevronRight, 12.)
                }
                .flex_none()
                .text_color(theme::sidebar_fg_muted()),
            )
    }

    fn render_card_row(
        &self,
        thread: &Thread,
        row_id: usize,
        project: &Project,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.state.selected.as_ref() == Some(&thread.id);
        let renaming = self.sidebar_state.renaming.as_ref() == Some(&thread.id);
        let unread = self.is_unread(thread);
        let snoozed = self.is_snoozed(thread);
        // Web `shouldRecedeSidebarThread` — same inputs as the slim row.
        let receded = !active
            && !thread.has_pending_request
            && !thread.has_actionable_proposed_plan
            && (thread.running() || !unread);
        // Card title ink: `font-medium text-foreground`; receded rows drop to
        // `font-normal text-foreground/75` until the card is hovered.
        let mut receded_title = theme::sidebar_fg();
        receded_title.a *= 0.75;
        let title_color = if receded {
            receded_title
        } else {
            theme::sidebar_fg()
        };
        let meta = self.sidebar_state.thread_meta.get(thread.id.as_ref());
        let branch = meta
            .and_then(|m| m.branch.clone())
            .filter(|b| !b.is_empty());
        // `pinned` lands on the thread shell; `pinnedAt` arrives on the
        // metadata harvest — either marks the card's pin.
        let pinned = thread.pinned || meta.is_some_and(|m| m.pinned);
        let rel_time = rel_time_label(thread.updated_at.as_ref());
        let select_id = thread.id.clone();
        let menu_id = thread.id.clone();
        let settle_id = thread.id.clone();
        let snooze_id = thread.id.clone();
        let unpin_id = thread.id.clone();

        // Right-edge status slot — web `topStatus` (icon + label + optional
        // working duration); the relative time fills the slot when quiet.
        let top_status = card_top_status(thread, meta, unread);
        let pr = pr_badge(thread, meta);
        // Web `metaLineEmpty`: single-environment builds never see the
        // scratch-machine label (`scratchMachineLabelFor` requires
        // `showProjectEnvironments`), so branch/PR/diff decide the line.
        let meta_line_empty = branch.is_none() && pr.is_none();
        // `SidebarProviderStack`: prior instance marks first (smaller,
        // grayscale), current instance last; absent config entry → no stack.
        let provider_stack = self.provider_stack(thread, meta);
        let duration_suffix = top_status
            .as_ref()
            .and_then(|(_, _, _, started)| started.clone())
            .and_then(|iso| unix_from_iso(&iso))
            .map(|started| fmt_duration(unix_now() - started));

        let mut line1 = div()
            .flex()
            .items_center()
            .h_5()
            .gap_1p5()
            .child(self.project_chip(project));
        if pinned {
            // Web `pinIndicator`: a muted size-3 Pin that flips to an unpin
            // affordance on hover — the bundled set has no pin-off, so the
            // pin keeps its glyph and only brightens.
            line1 = line1.child(
                div()
                    .id(("unpin", row_id))
                    .flex_none()
                    .cursor_pointer()
                    .tooltip(tooltip("Unpin thread"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            cx.stop_propagation();
                            if let Some(thread) =
                                this.state.threads.iter_mut().find(|t| t.id == unpin_id)
                            {
                                thread.pinned = false;
                                thread.pin_order_key = None;
                            }
                            this.dispatch(json!({
                                "type": "thread.unpin",
                                "commandId": Uuid::new_v4().to_string(),
                                "threadId": unpin_id,
                            }));
                            cx.notify();
                        }),
                    )
                    .child(hover_icon(
                        Icon::Pin,
                        12.,
                        theme::sidebar_faint(),
                        theme::sidebar_fg(),
                        "unpin",
                    )),
            );
        }
        line1 = line1
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_x_hidden()
                    .text_ellipsis()
                    .text_xs()
                    .text_color(theme::sidebar_secondary_label())
                    .child(project.name.clone()),
            )
            // Status slot: web `topStatus` (size-4 glyph + medium label +
            // working/waiting duration), else the relative time — snoozed
            // rows lead it with the info clock — hidden on card hover, when
            // the snooze/settle cluster covers it.
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .group_hover("thread-row", |s| s.opacity(0.))
                    .when(snoozed && top_status.is_none(), |el| {
                        el.child(
                            icon(Icon::Clock, 12.)
                                .flex_none()
                                .text_color(theme::info_fg()),
                        )
                    })
                    .child(match top_status {
                        Some((status_icon, status_color, status_label, _)) => div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(status_color)
                            .child(icon(status_icon, 16.).flex_none().text_color(status_color))
                            .child(status_label)
                            .when_some(duration_suffix.clone(), |el, duration| {
                                el.child(
                                    div()
                                        .flex_none()
                                        .font_weight(gpui::FontWeight::NORMAL)
                                        .child(duration),
                                )
                            }),
                        None => div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme::sidebar_secondary_label())
                            .child(rel_time),
                    }),
            );

        let mut line2 = div().mt_1().flex().items_center().h_5().gap_1p5();
        if renaming {
            if let Some(field) = self.sidebar_state.rename_field.clone() {
                line2 = line2.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(theme::sidebar_fg())
                        .child(field),
                );
            }
        } else {
            line2 = line2.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_x_hidden()
                    .text_ellipsis()
                    .text_sm()
                    .when(!receded, |s| s.font_weight(gpui::FontWeight::MEDIUM))
                    .text_color(title_color)
                    .child(thread.title.clone()),
            );
        }
        // Web `titleLineIcons` — the provider stack lands on the title line
        // only when the card has no meta line to hold it.
        if meta_line_empty && !provider_stack.is_empty() {
            line2 = line2.child(provider_stack_element(&provider_stack));
        }

        let mut card = div()
            .id(("thread", row_id))
            .group("thread-row")
            .relative()
            .w_full()
            .rounded_md()
            .overflow_hidden()
            .px(px(10.))
            .py(px(8.))
            .flex()
            .flex_col()
            .cursor_pointer()
            .when(active, |s| {
                // Web `bg-sidebar-row-active` + `light:ring-sidebar-border/70`.
                s.bg(theme::sidebar_active()).when(
                    !theme::effective_dark(),
                    |s| s.border_1().border_color(theme::sidebar_border_soft()),
                )
            })
            .when(!active, |s| s.hover(|s| s.bg(theme::sidebar_hover())))
            .when(receded && thread.running(), |s| s.opacity(0.7))
            // Web `h-[4.875rem]` while a meta line exists; `h-auto` otherwise.
            .when(!meta_line_empty && expanded, |s| s.h(px(78.)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _window, cx| {
                    if this.sidebar_state.renaming.as_ref() != Some(&select_id) {
                        this.sidebar_state.renaming = None;
                    }
                    this.select_and_visit(select_id.clone(), cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &gpui::MouseDownEvent, window, cx| {
                    this.sidebar_state.context_menu = Some(ContextMenu {
                        thread_id: menu_id.clone(),
                        position: ev.position,
                        open_submenu: None,
                    });
                    this.sidebar_state.delete_confirm = None;
                    if let Some(focus) = this.sidebar_state.menu_focus.clone() {
                        focus.focus(window, cx);
                    }
                    this.select_and_visit(menu_id.clone(), cx);
                }),
            )
            .child(line1)
            .when(expanded, |card| {
                let card = card.child(line2);
                // Web `metaLine` (`mt-0.5 flex items-center gap-1.5 text-xs
                // text-secondary-label`): worktree marker + middle-truncated
                // branch on the left; PR chip and the provider stack pinned
                // right (`ml-auto`).
                if !meta_line_empty {
                    let mut meta_line = div()
                        .mt(px(2.))
                        .flex()
                        .items_center()
                        .h_5()
                        .gap_1p5()
                        .text_xs()
                        .text_color(theme::sidebar_secondary_label());
                    if let Some(branch) = branch {
                        let branch_icon = if meta
                            .is_some_and(|m| m.worktree_path.is_some())
                        {
                            Icon::FolderGit2
                        } else {
                            Icon::GitBranch
                        };
                        meta_line = meta_line
                            .child(
                                icon(branch_icon, 12.)
                                    .flex_none()
                                    .text_color(theme::sidebar_secondary_label()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_nowrap()
                                    .overflow_x_hidden()
                                    .text_ellipsis()
                                    .child(branch),
                            );
                    } else {
                        meta_line = meta_line.child(div().flex_1());
                    }
                    if let Some((pr_icon, pr_color, pr_text)) = pr {
                        meta_line = meta_line.child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_0p5()
                                .text_color(pr_color)
                                .child(icon(pr_icon, 12.).flex_none().text_color(pr_color))
                                .when(!pr_text.is_empty(), |el| {
                                    el.child(
                                        div().flex_none().text_xs().child(pr_text),
                                    )
                                }),
                        );
                    }
                    if !provider_stack.is_empty() {
                        meta_line = meta_line.child(provider_stack_element(&provider_stack));
                    }
                    card.child(meta_line)
                } else {
                    card
                }
            });

        if !self.is_settled(thread.id.as_ref()) {
            // Hover-only controls covering the time slot — web's ghost
            // snooze button + "✓ Settle" (both `text-muted-foreground`,
            // brightening on their own hover).
            let mut controls = div()
                .absolute()
                .top(px(8.))
                .right(px(6.))
                .h_5()
                .flex()
                .items_center()
                .opacity(0.)
                .group_hover("thread-row", |s| s.opacity(1.));
            if snoozed {
                // Web `unsnooze` affordance — a clock button that wakes the
                // thread now.
                controls = controls.child(
                    div()
                        .id(("unsnooze", row_id))
                        .group("unsnooze")
                        .flex_none()
                        .w_5()
                        .h_5()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .text_color(theme::sidebar_fg_muted())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::sidebar_fg()))
                        .tooltip(tooltip("Wake thread now"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                cx.stop_propagation();
                                this.unsnooze_thread(snooze_id.clone(), cx);
                            }),
                        )
                        .child(hover_icon(
                            Icon::Clock,
                            14.,
                            theme::sidebar_fg_muted(),
                            theme::sidebar_fg(),
                            "unsnooze",
                        )),
                );
            } else {
                controls = controls.child(
                    div()
                        .id(("snooze", row_id))
                        .group("snooze")
                        .flex_none()
                        .w_5()
                        .h_5()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .text_color(theme::sidebar_fg_muted())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::sidebar_fg()))
                        .tooltip(tooltip("Snooze"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                cx.stop_propagation();
                                if let Some((_, until, _)) =
                                    snooze_presets(unix_now()).first()
                                {
                                    this.snooze_thread(
                                        snooze_id.clone(),
                                        iso_from_unix(*until),
                                        cx,
                                    );
                                }
                            }),
                        )
                        .child(hover_icon(
                            Icon::Clock,
                            14.,
                            theme::sidebar_fg_muted(),
                            theme::sidebar_fg(),
                            "snooze",
                        )),
                );
                controls = controls.child(
                    div()
                        .id(("settle", row_id))
                        .group("settle")
                        .flex_none()
                        .h_5()
                        .flex()
                        .items_center()
                        .gap_1()
                        .rounded_md()
                        .px_1p5()
                        .text_xs()
                        .text_color(theme::sidebar_fg_muted())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::sidebar_fg()))
                        .tooltip(tooltip("Settle thread"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                cx.stop_propagation();
                                this.settle_thread(settle_id.clone(), cx);
                            }),
                        )
                        .child(hover_icon(
                            Icon::Check,
                            14.,
                            theme::sidebar_fg_muted(),
                            theme::sidebar_fg(),
                            "settle",
                        ))
                        .child("Settle"),
                );
            }
            card = card.child(controls);
        }

        // Web `li.py-0.5` — 2px top/bottom air between cards.
        div().py(px(2.)).child(card).into_any_element()
    }

    /// The thread context menu — item-for-item parity with the web's
    /// `buildThreadActionMenuItems` sidebar variant (no `find`): new-thread-
    /// on-branch, pin, settle, snooze submenu, rename, regenerate title,
    /// mark unread, guest share, project filter, turn-into-project,
    /// auto-settle submenu, copy submenu, project settings, archive, delete.
    /// Submenus open as a second anchored popup right of the parent, keyed by
    /// `menu.open_submenu` ("snooze" | "auto-settle" | "copy").
    fn render_context_menu(&self, menu: &ContextMenu, cx: &mut Context<Self>) -> impl IntoElement {
        let thread = self
            .state
            .threads
            .iter()
            .find(|t| t.id == menu.thread_id)
            .cloned();
        let meta = self
            .sidebar_state
            .thread_meta
            .get(menu.thread_id.as_ref())
            .cloned();
        let pinned = thread.as_ref().is_some_and(|t| t.pinned);
        let snoozed = thread.as_ref().is_some_and(|t| self.is_snoozed(t));
        let settled = self.is_settled(menu.thread_id.as_ref());
        let confirming = self.sidebar_state.delete_confirm.as_ref() == Some(&menu.thread_id);
        let config_loaded = self.sidebar_state.server_config.is_some();
        let branch = meta
            .as_ref()
            .and_then(|m| m.branch.clone())
            .filter(|b| !b.is_empty());
        let worktree = meta.as_ref().and_then(|m| m.worktree_path.clone());
        let project = thread
            .as_ref()
            .and_then(|t| {
                self.state
                    .projects
                    .iter()
                    .find(|p| p.id == t.project_id)
                    .cloned()
            });
        let workspace_path = worktree
            .clone()
            .or_else(|| project.as_ref().map(|p| p.path.to_string()));
        // Web `threadRuntimeCanArchive`: preparing/starting/running block
        // archive + turn-into-project, as does a queued runtime with a live
        // active run.
        let is_running = thread.as_ref().is_some_and(|t| {
            let m = self.sidebar_state.thread_meta.get(t.id.as_ref());
            let status = runtime_status(t, m);
            matches!(status, Some("preparing" | "starting" | "running"))
                || (status == Some("queued") && m.is_some_and(|mm| mm.active_run_id.is_some()))
        });
        // Web `canSnooze`: no pending approval, no non-input runtime request,
        // no pending subagent call, no waiting guest message.
        let can_snooze_now = thread.as_ref().is_some_and(|t| {
            !t.has_pending_request
                && meta
                    .as_ref()
                    .and_then(|m| m.pending_request_kind.as_deref())
                    .is_none_or(|k| k == "user_input")
                && meta
                    .as_ref()
                    .is_none_or(|m| m.pending_subagent_request.is_none())
                && meta
                    .as_ref()
                    .is_none_or(|m| m.pending_guest_messages <= 0)
        });
        let regenerating_title = meta.as_ref().is_some_and(|m| m.title_regenerating);
        let auto_settle_enabled = !meta.as_ref().is_some_and(|m| m.auto_settle_disabled);
        let can_turn_into_project = worktree.is_some()
            && self.server_capability("threadProjectFromScratch")
            && project.as_ref().is_some_and(|p| self.is_scratch_project(p));
        let can_share_guest = self
            .sidebar_state
            .server_config
            .as_ref()
            .is_some_and(|c| c["settings"]["guestSharingEnabled"].as_bool() == Some(true));
        let filter_active = thread
            .as_ref()
            .is_some_and(|t| self.sidebar_state.project_filter.as_ref() == Some(&t.project_id));
        let project_label = project
            .as_ref()
            .map(|p| p.name.to_string())
            .unwrap_or_else(|| "project".to_string());

        let separator = || div().h(px(1.)).mx_2().my_1().bg(theme::border());

        // Web `MenuPopup`/`dropdown-glass`: popover surface, rounded-lg, fg 10%
        // border, drop shadow; items live inside a `p-1` content box.
        let mut menu_el = div()
            .flex()
            .flex_col()
            .w(px(200.))
            .p_1()
            .rounded_lg()
            .bg(theme::raised())
            .border_1()
            .border_color(theme::popover_border())
            .shadow_lg()
            .text_sm()
            .text_color(theme::fg())
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _ev, _window, cx| cx.stop_propagation()),
            );

        // `new-thread-on-branch` — web `handleNewThread` on the thread's own
        // branch/worktree. Native dispatches `thread.create` directly.
        if let Some(branch) = branch.clone() {
            let branch_name = branch.clone();
            let worktree_path = worktree.clone();
            let project_id = thread.as_ref().map(|t| t.project_id.clone());
            menu_el = menu_el.child(self.menu_item(
                "mi-new-branch",
                Icon::MessageSquarePlus,
                format!("New thread on {branch_name}"),
                false,
                false,
                false,
                cx,
                move |this, _ev, _window, cx| {
                    this.sidebar_state.context_menu = None;
                    let Some(project_id) = project_id.clone() else {
                        return;
                    };
                    let new_id = Uuid::new_v4().to_string();
                    this.dispatch(json!({
                        "type": "thread.create",
                        "commandId": Uuid::new_v4().to_string(),
                        "threadId": new_id,
                        "projectId": project_id.to_string(),
                        "title": format!("Thread on {branch_name}"),
                        "modelSelection": this.model_selection(),
                        "runtimeMode": this.composer_state.runtime_mode.clone(),
                        "interactionMode": this.composer_state.interaction_mode.clone(),
                        "branch": branch_name.clone(),
                        "worktreePath": worktree_path.clone(),
                        "createdBy": "user",
                        "creationSource": "web",
                    }));
                    this.select_thread(new_id.clone().into(), cx);
                },
            ));
        }

        // `pin`/`unpin` — pinning exists once the server config lands, like
        // the web's `supports.pinning = configLoaded`.
        if config_loaded {
            let pin_id = menu.thread_id.clone();
            menu_el = menu_el.child(self.menu_item(
                if pinned { "mi-unpin" } else { "mi-pin" },
                if pinned { Icon::PinOff } else { Icon::Pin },
                if pinned { "Unpin thread" } else { "Pin thread" },
                false,
                false,
                false,
                cx,
                move |this, _ev, _window, cx| {
                    this.sidebar_state.context_menu = None;
                    if let Some(thread) =
                        this.state.threads.iter_mut().find(|t| t.id == pin_id)
                    {
                        thread.pinned = !pinned;
                        if pinned {
                            thread.pin_order_key = None;
                        }
                    }
                    this.dispatch(json!({
                        "type": if pinned { "thread.unpin" } else { "thread.pin" },
                        "commandId": Uuid::new_v4().to_string(),
                        "threadId": pin_id.to_string(),
                    }));
                    cx.notify();
                },
            ));
        }

        // `settle`/`unsettle` — `supports.settlement = configLoaded`.
        if config_loaded {
            let settle_id = menu.thread_id.clone();
            menu_el = menu_el.child(self.menu_item(
                if settled {
                    "mi-unsettle"
                } else {
                    "mi-settle"
                },
                Icon::CircleCheck,
                if settled {
                    "Un-settle thread"
                } else {
                    "Settle thread"
                },
                false,
                false,
                false,
                cx,
                move |this, _ev, _window, cx| {
                    this.sidebar_state.context_menu = None;
                    if settled {
                        if let Some(entry) =
                            this.sidebar_state.thread_meta.get_mut(settle_id.as_ref())
                        {
                            entry.settled = false;
                        }
                        this.dispatch(json!({
                            "type": "thread.unsettle",
                            "commandId": Uuid::new_v4().to_string(),
                            "threadId": settle_id.to_string(),
                            "reason": "user",
                        }));
                    } else {
                        this.settle_thread(settle_id.clone(), cx);
                    }
                    cx.notify();
                },
            ));
        }

        // `snooze`/`unsnooze` — `supports.snooze = configLoaded`; the snooze
        // row carries the preset submenu.
        if config_loaded {
            if snoozed {
                let unsnooze_id = menu.thread_id.clone();
                menu_el = menu_el.child(self.menu_item(
                    "mi-unsnooze",
                    Icon::Clock,
                    "Wake thread",
                    false,
                    false,
                    false,
                    cx,
                    move |this, _ev, _window, cx| {
                        this.sidebar_state.context_menu = None;
                        this.unsnooze_thread(unsnooze_id.clone(), cx);
                    },
                ));
            } else {
                menu_el = menu_el.child(self.menu_item(
                    "mi-snooze",
                    Icon::Clock,
                    "Snooze",
                    false,
                    true,
                    !can_snooze_now,
                    cx,
                    move |this, ev, _window, cx| {
                        if let Some(m) = &mut this.sidebar_state.context_menu {
                            m.open_submenu = Some(("snooze", ev.position));
                        }
                        cx.notify();
                    },
                ));
            }
        }

        // `rename` — separator before (web `separatorBefore`).
        let rename_id = menu.thread_id.clone();
        menu_el = menu_el.child(separator()).child(self.menu_item(
            "mi-rename",
            Icon::Pencil,
            "Rename thread",
            false,
            false,
            false,
            cx,
            move |this, _ev, window, cx| {
                this.sidebar_state.context_menu = None;
                this.start_rename(rename_id.clone(), window, cx);
            },
        ));

        // `regenerate-title` — `supports.titleRegeneration = configLoaded`;
        // reads "Regenerating…" and sits inert while one is in flight.
        if config_loaded {
            let regen_id = menu.thread_id.clone();
            menu_el = menu_el.child(self.menu_item(
                "mi-regen-title",
                Icon::RefreshCw,
                if regenerating_title {
                    "Regenerating…"
                } else {
                    "Regenerate title"
                },
                false,
                false,
                regenerating_title,
                cx,
                move |this, _ev, _window, cx| {
                    this.sidebar_state.context_menu = None;
                    let entry = this
                        .sidebar_state
                        .thread_meta
                        .entry(regen_id.to_string())
                        .or_default();
                    entry.title_regenerating = true;
                    this.dispatch(json!({
                        "type": "thread.metadata.update",
                        "commandId": Uuid::new_v4().to_string(),
                        "threadId": regen_id.to_string(),
                        "regenerateTitle": true,
                    }));
                    cx.notify();
                },
            ));
        }

        // `mark-unread` — always present (read markers work read-only).
        let unread_id = menu.thread_id.clone();
        menu_el = menu_el.child(self.menu_item(
            "mi-mark-unread",
            Icon::MailOpen,
            "Mark unread",
            false,
            false,
            false,
            cx,
            move |this, _ev, _window, cx| {
                this.sidebar_state.context_menu = None;
                this.mark_unread(unread_id.clone(), cx);
            },
        ));

        // `share-guest` — the server's Guest links beta flag.
        if can_share_guest {
            let share_id = menu.thread_id.clone();
            menu_el = menu_el.child(self.menu_item(
                "mi-share-guest",
                Icon::UserPlus,
                "Share with guest",
                false,
                false,
                false,
                cx,
                move |this, _ev, _window, cx| {
                    this.sidebar_state.context_menu = None;
                    let Some(rx) = this.send_query(
                        "guestShare.create",
                        json!({ "threadId": share_id.to_string() }),
                    ) else {
                        return;
                    };
                    cx.spawn(async move |this, cx| {
                        let Ok(exit) = rx.recv().await else {
                            return;
                        };
                        let _ = this.update(cx, |this, cx| {
                            if exit["_tag"].as_str() == Some("Success") {
                                let url = exit["value"]["url"].as_str().unwrap_or_default();
                                if !url.is_empty() {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        url.to_string(),
                                    ));
                                    this.push_toast(
                                        ToastKind::Info,
                                        "Guest link copied",
                                        Some(url.to_string()),
                                        None,
                                        cx,
                                    );
                                }
                            } else {
                                this.push_toast(
                                    ToastKind::Error,
                                    "Could not share with guest",
                                    exit["value"]["_tag"]
                                        .as_str()
                                        .map(str::to_string),
                                    None,
                                    cx,
                                );
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                },
            ));
        }

        // `filter-by-project` — the thread's non-archived project group.
        if project.is_some() {
            let filter_id = thread.as_ref().map(|t| t.project_id.clone());
            let filter_label = if filter_active {
                "Show all projects".to_string()
            } else {
                format!("Filter by {project_label}")
            };
            menu_el = menu_el.child(self.menu_item(
                "mi-filter-project",
                Icon::FolderTree,
                filter_label,
                false,
                false,
                false,
                cx,
                move |this, _ev, _window, cx| {
                    this.sidebar_state.context_menu = None;
                    this.sidebar_state.project_filter = if this.sidebar_state.project_filter
                        == filter_id
                    {
                        None
                    } else {
                        filter_id.clone()
                    };
                    cx.notify();
                },
            ));
        }

        // `turn-into-project` — a scratch ("No project") thread with its own
        // worktree on a server that can promote it.
        if can_turn_into_project {
            let turn_id = menu.thread_id.clone();
            let turn_title = thread.as_ref().map(|t| t.title.to_string());
            menu_el = menu_el.child(self.menu_item(
                "mi-turn-project",
                Icon::FolderPlus,
                "Turn into project",
                false,
                false,
                is_running,
                cx,
                move |this, _ev, _window, cx| {
                    this.sidebar_state.context_menu = None;
                    let Some(rx) = this.send_query(
                        "projects.createFromThread",
                        json!({ "threadId": turn_id.to_string() }),
                    ) else {
                        return;
                    };
                    let title = turn_title.clone();
                    cx.spawn(async move |this, cx| {
                        let Ok(exit) = rx.recv().await else {
                            return;
                        };
                        let _ = this.update(cx, |this, cx| {
                            if exit["_tag"].as_str() == Some("Success") {
                                this.push_toast(
                                    ToastKind::Info,
                                    "Project created",
                                    title.clone(),
                                    None,
                                    cx,
                                );
                            } else {
                                this.push_toast(
                                    ToastKind::Error,
                                    "Could not turn thread into a project",
                                    None,
                                    None,
                                    cx,
                                );
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                },
            ));
        }

        // `auto-settle` — `supports.autoSettleOptOut` capability; checked
        // children pick Follow-settings vs Never.
        if self.server_capability("threadAutoSettleOptOut") {
            menu_el = menu_el.child(self.menu_item(
                "mi-auto-settle",
                Icon::Timer,
                "Auto-settle behavior",
                false,
                true,
                false,
                cx,
                move |this, ev, _window, cx| {
                    if let Some(m) = &mut this.sidebar_state.context_menu {
                        m.open_submenu = Some(("auto-settle", ev.position));
                    }
                    cx.notify();
                },
            ));
        }

        // `copy` — separator before; children copy path/branch/thread/id.
        menu_el = menu_el.child(separator()).child(self.menu_item(
            "mi-copy",
            Icon::Copy,
            "Copy",
            false,
            true,
            false,
            cx,
            move |this, ev, _window, cx| {
                if let Some(m) = &mut this.sidebar_state.context_menu {
                    m.open_submenu = Some(("copy", ev.position));
                }
                cx.notify();
            },
        ));

        // `project-settings` — opens Settings on the Projects section.
        menu_el = menu_el.child(self.menu_item(
            "mi-project-settings",
            Icon::Settings,
            "Project settings",
            false,
            false,
            false,
            cx,
            move |this, _ev, _window, cx| {
                this.sidebar_state.context_menu = None;
                this.settings_state.open = true;
                this.settings_state.section = crate::settings::Section::Projects;
                cx.notify();
            },
        ));

        // `archive` — separator before, inert while the runtime is live.
        let archive_id = menu.thread_id.clone();
        let archive_title = thread.as_ref().map(|t| t.title.to_string());
        menu_el = menu_el.child(separator()).child(self.menu_item(
            "mi-archive",
            Icon::Archive,
            "Archive thread",
            false,
            false,
            is_running,
            cx,
            move |this, _ev, _window, cx| {
                this.sidebar_state.context_menu = None;
                this.state.threads.retain(|t| t.id != archive_id);
                this.dispatch(json!({
                    "type": "thread.archive",
                    "commandId": Uuid::new_v4().to_string(),
                    "threadId": archive_id.to_string(),
                }));
                this.push_toast_with_action(
                    ToastKind::Info,
                    "Thread archived",
                    archive_title.clone(),
                    None,
                    Some(ToastAction::UnarchiveThread {
                        thread_id: archive_id.clone(),
                    }),
                    cx,
                );
                cx.notify();
            },
        ));

        // `delete` — destructive; first click arms the confirm label like
        // before (the web's `confirmThreadDelete` dialog).
        let delete_id = menu.thread_id.clone();
        let delete_title = thread.as_ref().map(|t| t.title.to_string());
        let delete_label = if confirming {
            "Confirm delete"
        } else {
            "Delete"
        };
        menu_el = menu_el.child(self.menu_item(
            "mi-delete",
            Icon::Trash2,
            delete_label,
            true,
            false,
            false,
            cx,
            move |this, _ev, _window, cx| {
                if confirming {
                    this.sidebar_state.context_menu = None;
                    this.sidebar_state.delete_confirm = None;
                    this.state.threads.retain(|t| t.id != delete_id);
                    if this.state.selected.as_ref() == Some(&delete_id) {
                        this.state.selected = None;
                    }
                    this.dispatch(json!({
                        "type": "thread.delete",
                        "commandId": Uuid::new_v4().to_string(),
                        "threadId": delete_id.to_string(),
                    }));
                    // No un-delete exists on the wire (the command set has
                    // only thread.unarchive) — a plain notice, no Undo.
                    this.push_toast(
                        ToastKind::Info,
                        "Thread deleted",
                        delete_title.clone(),
                        None,
                        cx,
                    );
                } else {
                    this.sidebar_state.delete_confirm = Some(delete_id.clone());
                }
                cx.notify();
            },
        ));

        let mut popup = div().child(
            deferred(
                anchored()
                    .position(point(menu.position.x, menu.position.y))
                    .child(menu_el),
            )
            .with_priority(1),
        );

        // Submenus ride as a second deferred popup anchored right of the
        // parent at the click row (`open_submenu` records the click point).
        if let Some((which, click)) = &menu.open_submenu {
            let sub_pos = point(
                menu.position.x + px(204.),
                click.y - px(8.),
            );
            let mut sub_el = div()
                .flex()
                .flex_col()
                .min_w(px(160.))
                .p_1()
                .rounded_lg()
                .bg(theme::raised())
                .border_1()
                .border_color(theme::popover_border())
                .shadow_lg()
                .text_sm()
                .text_color(theme::fg())
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_this, _ev, _window, cx| cx.stop_propagation()),
                );
            match *which {
                "snooze" => {
                    let presets = snooze_presets(unix_now());
                    for (ix, (label, until, when)) in presets.iter().enumerate() {
                        let snooze_id = menu.thread_id.clone();
                        let until_iso = iso_from_unix(*until);
                        sub_el = sub_el.child(self.submenu_item(
                            ("snooze-preset", ix),
                            None,
                            None,
                            *label,
                            Some(when.clone()),
                            false,
                            cx,
                            move |this, _ev, _window, cx| {
                                this.sidebar_state.context_menu = None;
                                this.snooze_thread(
                                    snooze_id.clone(),
                                    until_iso.clone(),
                                    cx,
                                );
                            },
                        ));
                    }
                    if !presets.is_empty() {
                        sub_el = sub_el.child(separator());
                    }
                    let snooze_id = menu.thread_id.clone();
                    sub_el = sub_el.child(self.submenu_item(
                        "snooze-custom",
                        None,
                        None,
                        "Custom…",
                        None,
                        false,
                        cx,
                        move |this, _ev, window, cx| {
                            this.sidebar_state.context_menu = None;
                            this.open_snooze_overlay(snooze_id.clone(), window, cx);
                        },
                    ));
                }
                "auto-settle" => {
                    for (ix, (label, checked, enabled)) in
                        [("Follow settings", auto_settle_enabled, true),
                         ("Never", !auto_settle_enabled, false)]
                        .into_iter()
                        .enumerate()
                    {
                        let set_id = menu.thread_id.clone();
                        sub_el = sub_el.child(self.submenu_item(
                            ("auto-settle-opt", ix),
                            None,
                            Some(checked),
                            label,
                            None,
                            false,
                            cx,
                            move |this, _ev, _window, cx| {
                                this.sidebar_state.context_menu = None;
                                let entry = this
                                    .sidebar_state
                                    .thread_meta
                                    .entry(set_id.to_string())
                                    .or_default();
                                entry.auto_settle_disabled = !enabled;
                                this.dispatch(json!({
                                    "type": "thread.auto-settle.set",
                                    "commandId": Uuid::new_v4().to_string(),
                                    "threadId": set_id.to_string(),
                                    "enabled": enabled,
                                }));
                                cx.notify();
                            },
                        ));
                    }
                }
                _ => {
                    // "copy" — Path / Branch / Thread / Thread ID.
                    if let Some(path) = workspace_path.clone() {
                        sub_el = sub_el.child(self.submenu_item(
                            "copy-path",
                            Some(Icon::Folder),
                            None,
                            "Path",
                            None,
                            false,
                            cx,
                            move |this, _ev, _window, cx| {
                                this.sidebar_state.context_menu = None;
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    path.clone(),
                                ));
                                this.push_toast(
                                    ToastKind::Info,
                                    "Path copied",
                                    Some(path.clone()),
                                    None,
                                    cx,
                                );
                            },
                        ));
                    }
                    if let Some(branch) = branch.clone() {
                        sub_el = sub_el.child(self.submenu_item(
                            "copy-branch",
                            Some(Icon::GitBranch),
                            None,
                            "Branch",
                            None,
                            false,
                            cx,
                            move |this, _ev, _window, cx| {
                                this.sidebar_state.context_menu = None;
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    branch.clone(),
                                ));
                                this.push_toast(
                                    ToastKind::Info,
                                    "Branch copied",
                                    Some(branch.clone()),
                                    None,
                                    cx,
                                );
                            },
                        ));
                    }
                    let env_id = self.environment_id();
                    let copy_id = menu.thread_id.clone();
                    let copy_title = thread.as_ref().map(|t| t.title.to_string());
                    sub_el = sub_el.child(self.submenu_item(
                        "copy-thread",
                        Some(Icon::MessageSquare),
                        None,
                        "Thread",
                        None,
                        false,
                        cx,
                        move |this, _ev, _window, cx| {
                            this.sidebar_state.context_menu = None;
                            // Web `hostedThreadLink`: the hosted app opens
                            // the thread from anywhere.
                            let url = match env_id.clone() {
                                Some(env) => format!(
                                    "https://app.t3.codes/threads/{}/{}",
                                    hosted_segment(&env),
                                    hosted_segment(copy_id.as_ref()),
                                ),
                                // v1 links name no environment.
                                None => format!("t3-thread://v1/{}", copy_id),
                            };
                            cx.write_to_clipboard(ClipboardItem::new_string(url));
                            this.push_toast(
                                ToastKind::Info,
                                "Thread copied",
                                copy_title.clone(),
                                None,
                                cx,
                            );
                        },
                    ));
                    let id_text = menu.thread_id.clone();
                    sub_el = sub_el.child(self.submenu_item(
                        "copy-thread-id",
                        Some(Icon::Hash),
                        None,
                        "Thread ID",
                        None,
                        false,
                        cx,
                        move |this, _ev, _window, cx| {
                            this.sidebar_state.context_menu = None;
                            cx.write_to_clipboard(ClipboardItem::new_string(
                                id_text.to_string(),
                            ));
                            this.push_toast(
                                ToastKind::Info,
                                "Thread ID copied",
                                Some(id_text.to_string()),
                                None,
                                cx,
                            );
                        },
                    ));
                }
            }
            popup = popup.child(
                deferred(anchored().position(sub_pos).child(sub_el)).with_priority(2),
            );
        }

        popup
    }

    fn render_project_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let name_field = self.sidebar_state.project_name_field.clone();
        let path_field = self.sidebar_state.project_path_field.clone();
        div()
            .mx_2()
            .mb_1()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .rounded_lg()
            .bg(theme::sidebar_control())
            .border_1()
            .border_color(theme::border())
            .when_some(name_field, |el, field| {
                el.child(
                    div()
                        .rounded_sm()
                        .bg(theme::canvas())
                        .border_1()
                        .border_color(theme::border())
                        .px_2()
                        .py_1()
                        .text_sm()
                        .text_color(theme::fg())
                        .child(field),
                )
            })
            .when_some(path_field, |el, field| {
                el.child(
                    div()
                        .rounded_sm()
                        .bg(theme::canvas())
                        .border_1()
                        .border_color(theme::border())
                        .px_2()
                        .py_1()
                        .text_sm()
                        .text_color(theme::fg())
                        .child(field),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pt_1()
                    .child(
                        div()
                            .id("create-project")
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .bg(theme::primary())
                            .text_xs()
                            .text_color(gpui::white())
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.85))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, window, cx| {
                                    cx.stop_propagation();
                                    this.submit_project_form_click(window, cx);
                                }),
                            )
                            .child("Create"),
                    )
                    .child(
                        div()
                            .id("cancel-project")
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.sidebar_state.new_project_open = false;
                                    cx.notify();
                                }),
                            )
                            .child("Cancel"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("Enter to create"),
                    ),
            )
    }

    /// One saved-draft row in the Drafts block: prompt preview + project
    /// name, ✕ to discard, body click opens it in the composer.
    fn render_draft_row(
        &self,
        draft: &config::DraftThread,
        row_id: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let preview = draft
            .prompt
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                let n = draft.files.len();
                format!("{n} attachment{}", if n == 1 { "" } else { "s" })
            });
        let project_name = self
            .state
            .projects
            .iter()
            .find(|p| p.id.as_ref() == draft.project_id)
            .map(|p| p.name.clone());
        let open_id = draft.thread_id.clone();
        let delete_id = draft.thread_id.clone();
        // Web `SidebarDraftRow`: a `h-[4.875rem]` card on the warning-tinted
        // draft surface — pen + project favicon + name on the h-5 first
        // line, medium preview text on the second, a discard × trailing the
        // first line (revealed on row hover, like the web).
        div()
            .id(("draft", row_id))
            .group("sidebar-draft")
            .flex()
            .flex_col()
            .h(px(78.))
            .px_2p5()
            .py_2()
            .my_0p5()
            .rounded_md()
            .cursor_pointer()
            .bg(theme::draft_surface())
            .text_color(theme::sidebar_fg())
            .hover(|s| s.bg(theme::draft_surface_hover()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, window, cx| {
                    this.open_draft_thread(open_id.clone(), window, cx);
                }),
            )
            .child(
                div()
                    .h_5()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        icon(Icon::Pencil, 12.)
                            .flex_none()
                            .text_color(theme::warning_fg()),
                    )
                    // Web shows the project's favicon before its name; same
                    // favicon > icon-chip fallback the folder header uses.
                    .when_some(
                        self.sidebar_state
                            .favicon_images
                            .get(draft.project_id.as_str())
                            .map(|(_, image)| favicon_chip(image))
                            .or_else(|| {
                                self.sidebar_state
                                    .project_icons
                                    .get(draft.project_id.as_str())
                                    .and_then(|ic| {
                                        project_icon_chip(
                                            ic,
                                            project_name.as_deref().unwrap_or_default(),
                                        )
                                    })
                            }),
                        |row, chip| row.child(chip),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .overflow_x_hidden()
                            .text_ellipsis()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::sidebar_secondary_label())
                            .child(project_name.unwrap_or_default()),
                    )
                    .child(
                        div()
                            .id(("draft-delete", row_id))
                            .flex_none()
                            .w_5()
                            .h_5()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .text_color(theme::sidebar_fg_muted())
                            .cursor_pointer()
                            .opacity(0.)
                            .group_hover("sidebar-draft", |s| s.opacity(1.))
                            .hover(|s| s.text_color(theme::sidebar_fg()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.delete_draft_thread(delete_id.clone(), cx);
                                }),
                            )
                            // Svg doesn't inherit the parent's ink — carry it.
                            .child(icon(Icon::X, 12.).text_color(theme::sidebar_fg_muted())),
                    ),
            )
            .child(
                div()
                    .mt_0p5()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_x_hidden()
                    .text_ellipsis()
                    .text_sm()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::sidebar_fg_soft())
                    .child(preview),
            )
    }

    /// The "Drafts" block at the top of the list — the web client's
    /// `SidebarDraftBlock`: client-side draft threads (persisted in
    /// `client_config`), plus the live new-thread draft while the composer
    /// has content. None when there is nothing to show.
    fn render_drafts_block(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let live = self.state.selected.is_none()
            && (!self.composer_state.draft.trim().is_empty()
                || !self.composer_state.attachments.is_empty());
        let mut drafts = self.client_config.draft_threads.clone();
        if drafts.is_empty() && !live {
            return None;
        }
        drafts.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let collapsed = self.sidebar_state.drafts_collapsed;
        // Web `CollapsibleSectionHeader`: a size-3 ChevronDown rotated
        // -90° while collapsed — i.e. a right-pointing chevron.
        let chevron = if collapsed {
            icon(Icon::ChevronRight, 12.)
        } else {
            icon(Icon::ChevronDown, 12.)
        };
        let count = drafts.len() + usize::from(live);

        // Header matches the web `CollapsibleSectionHeader`: h-8, xs medium
        // muted label, hairline rule, trailing count, size-3 chevron.
        let mut block = div().flex().flex_col().child(
            div()
                .id("drafts-header")
                .flex()
                .items_center()
                .h_8()
                .gap_2()
                .px_2()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(theme::sidebar_hover()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _window, cx| {
                        this.sidebar_state.drafts_collapsed = !this.sidebar_state.drafts_collapsed;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::sidebar_fg_muted())
                        .child("Drafts"),
                )
                .child(
                    div()
                        .h(px(1.))
                        .min_w_2()
                        .flex_1()
                        .bg(theme::sidebar_border_soft()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme::sidebar_fg_muted())
                        .child(format!("{count}")),
                )
                .child(chevron.flex_none().text_color(theme::sidebar_fg_muted())),
        );

        if !collapsed {
            if live {
                let preview = self
                    .composer_state
                    .draft
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        let n = self.composer_state.attachments.len();
                        format!("{n} attachment{}", if n == 1 { "" } else { "s" })
                    });
                // Same card as a saved draft; the live draft shows the active
                // row surface and keeps its "Save" affordance.
                block = block.child(
                    div()
                        .id("draft-live")
                        .flex()
                        .flex_col()
                        .h(px(78.))
                        .px_2p5()
                        .py_2()
                        .my_0p5()
                        .rounded_md()
                        .cursor_pointer()
                        .bg(theme::sidebar_active())
                        .text_color(theme::sidebar_fg())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, window, cx| {
                                this.composer.read(cx).focus_handle(cx).focus(window, cx);
                            }),
                        )
                        .child(
                            div()
                                .h_5()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    icon(Icon::Pencil, 12.)
                                        .flex_none()
                                        .text_color(theme::warning_fg()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .whitespace_nowrap()
                                        .overflow_x_hidden()
                                        .text_ellipsis()
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme::sidebar_secondary_label())
                                        .child("Unsaved draft"),
                                )
                                .child(
                                    div()
                                        .id("draft-save")
                                        .flex_none()
                                        .px_1()
                                        .rounded_md()
                                        .text_xs()
                                        .text_color(theme::primary())
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme::accent()))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _ev, window, cx| {
                                                cx.stop_propagation();
                                                this.save_current_draft(window, cx);
                                            }),
                                        )
                                        .child("Save"),
                                ),
                        )
                        .child(
                            div()
                                .mt_0p5()
                                .min_w_0()
                                .whitespace_nowrap()
                                .overflow_x_hidden()
                                .text_ellipsis()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::sidebar_fg_soft())
                                .child(preview),
                        ),
                );
            }
            for (row_id, draft) in drafts.iter().enumerate() {
                block = block.child(self.render_draft_row(draft, row_id, cx));
            }
        }
        // Web `sidebar-draft-divider`: `mx-2.5 my-1.5 h-px bg-sidebar-border/60`.
        block = block.child(
            div()
                .mx_2p5()
                .my_1p5()
                .h(px(1.))
                .bg(theme::sidebar_border_soft()),
        );
        Some(block.into_any_element())
    }

    pub fn render_sidebar(&mut self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        self.wire_sidebar(cx);
        self.load_project_icons(cx);
        self.load_project_favicons(cx);
        self.load_thread_meta(cx);
        self.load_server_config(cx);

        let query = self.sidebar_state.query.trim().to_lowercase();
        let filtering = !query.is_empty();

        let status_color = match &self.state.connection {
            ConnectionStatus::Connected => theme::primary(),
            ConnectionStatus::Failed(_) | ConnectionStatus::Disconnected(_) => theme::error(),
            _ => theme::fg_muted(),
        };

        // The web's "Settled" shelf: every settled thread, newest settle
        // first, rendered below the scrollable project list.
        let mut settled: Vec<(SharedString, SharedString, SharedString, String, bool)> = Vec::new();
        for thread in &self.state.threads {
            if self.is_settled(thread.id.as_ref()) {
                let meta = self.sidebar_state.thread_meta.get(thread.id.as_ref());
                settled.push((
                    thread.id.clone(),
                    thread.title.clone(),
                    thread.project_id.clone(),
                    meta.and_then(|m| m.settled_at.clone())
                        .unwrap_or_else(|| thread.updated_at.to_string()),
                    meta.is_some_and(|m| m.pinned),
                ));
            }
        }
        // Older settled shells the bounded snapshot didn't include in
        // `state.threads` — render them from the harvested metadata.
        for (id, meta) in &self.sidebar_state.thread_meta {
            if !meta.settled {
                continue;
            }
            let id: SharedString = id.clone().into();
            if self.state.threads.iter().any(|t| t.id == id) {
                continue;
            }
            if meta.title.is_empty() {
                continue;
            }
            settled.push((
                id,
                meta.title.clone().into(),
                meta.project_id.clone().into(),
                meta.settled_at.clone().unwrap_or_default(),
                meta.pinned,
            ));
        }
        settled.sort_by(|a, b| b.3.cmp(&a.3));

        let mut list = div()
            .id("sidebar-list")
            .flex()
            .flex_col()
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&self.sidebar_state.scroll)
            .px_2()
            .pb_2();

        // The web keeps settled threads off the active list — they live on
        // the bottom "Settled" shelf.
        let filter_project = self.sidebar_state.project_filter.clone();
        let live_thread = |t: &&Thread| {
            !self.is_settled(t.id.as_ref())
                && filter_project.as_ref().is_none_or(|p| t.project_id == *p)
        };
        if filtering {
            let mut hits: Vec<Thread> = self
                .state
                .threads
                .iter()
                .filter(|t| t.title.to_lowercase().contains(&query))
                .filter(&live_thread)
                .cloned()
                .collect();
            hits.sort_by_key(|t| (self.is_snoozed(t), Reverse(t.updated_at.clone())));
            if hits.is_empty() {
                // Web copy + metrics: `px-2 py-6 text-center text-xs
                // text-sidebar-muted-foreground`.
                list = list.child(
                    div()
                        .px_2()
                        .py_6()
                        .flex()
                        .flex_col()
                        .items_center()
                        .text_xs()
                        .text_color(theme::sidebar_fg_muted())
                        .child("No threads found"),
                );
            }
            for (row_id, thread) in hits.iter().enumerate() {
                let project_name = self
                    .state
                    .projects
                    .iter()
                    .find(|p| p.id == thread.project_id)
                    .map(|p| p.name.clone());
                list = list.child(self.render_thread_row(thread, row_id, project_name, false, cx));
            }
        } else {
            let drafts_block = self.render_drafts_block(cx);
            let no_drafts = drafts_block.is_none();
            if let Some(block) = drafts_block {
                list = list.child(block);
            }
            let projects = self.state.projects.clone();
            if projects.is_empty() && no_drafts {
                // Web's whole-list empty state: centered muted copy plus a
                // bordered "Add project" button.
                list = list.child(
                    div()
                        .px_2()
                        .py_6()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(theme::sidebar_dim())
                        .child("No projects yet")
                        .child(
                            div()
                                .id("empty-add-project")
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .rounded_md()
                                .border_1()
                                .border_color(theme::border())
                                .px_2p5()
                                .py_1()
                                .text_size(theme::text_size::XS_2)
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::sidebar_fg_muted())
                                .cursor_pointer()
                                .hover(|s| {
                                    s.bg(theme::sidebar_hover()).text_color(theme::sidebar_fg())
                                })
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _ev, window, cx| {
                                        cx.stop_propagation();
                                        this.sidebar_state.new_project_open = true;
                                        if let Some(field) =
                                            this.sidebar_state.project_name_field.clone()
                                        {
                                            let focus = field.read(cx).focus_handle(cx);
                                            focus.focus(window, cx);
                                        }
                                        cx.notify();
                                    }),
                                )
                                .child(icon(Icon::Plus, 12.).text_color(theme::sidebar_fg_muted()))
                                .child("Add project"),
                        ),
                );
            }
            // Web `partitionSidebarThreads` (flat sidebar, Working shelf on):
            // resolveSidebarThreadSection puts snoozed ahead of pinned;
            // remaining threads split between the flat active list and the
            // bottom Working shelf via `isThreadWorking`.
            let config_loaded = self.sidebar_state.server_config.is_some();
            let is_pinned = |t: &Thread| {
                t.pinned
                    || self
                        .sidebar_state
                        .thread_meta
                        .get(t.id.as_ref())
                        .is_some_and(|m| m.pinned)
            };
            let mut pinned: Vec<Thread> = Vec::new();
            let mut active: Vec<Thread> = Vec::new();
            let mut working: Vec<Thread> = Vec::new();
            let mut snoozed: Vec<Thread> = Vec::new();
            for thread in self.state.threads.iter().filter(|t| live_thread(t)) {
                if config_loaded && self.is_snoozed(thread) {
                    snoozed.push(thread.clone());
                } else if is_pinned(thread) {
                    pinned.push(thread.clone());
                } else if self.is_working_thread(thread) {
                    working.push(thread.clone());
                } else {
                    active.push(thread.clone());
                }
            }
            // `sortPinnedThreadsForSidebar` — pinOrderKey order, unkeyed last.
            pinned.sort_by_key(|t| {
                (
                    t.pin_order_key.is_none(),
                    t.pin_order_key.clone().unwrap_or_default(),
                )
            });
            // `sortActiveThreadsByOrderKey`: activeOrderKey carriers first,
            // the rest by newest activity anchor.
            active.sort_by_key(|t| match &t.active_order_key {
                Some(key) => (false, key.to_string(), Reverse(SharedString::from(""))),
                None => (true, String::new(), Reverse(t.updated_at.clone())),
            });
            // `sortWorkingThreadsBySend`: newest send/run anchor first.
            working.sort_by_key(|t| {
                Reverse(
                    self.sidebar_state
                        .thread_meta
                        .get(t.id.as_ref())
                        .and_then(|m| m.latest_run_requested_at.clone())
                        .filter(|at| at.as_str() > t.updated_at.as_ref())
                        .map(SharedString::from)
                        .unwrap_or_else(|| t.updated_at.clone()),
                )
            });
            // Snoozed rows order by wake time ascending.
            snoozed.sort_by_key(|t| t.snoozed_until.clone().unwrap_or_default());

            let project_of = |t: &Thread| {
                self.state
                    .projects
                    .iter()
                    .find(|p| p.id == t.project_id)
                    .cloned()
            };
            let mut row_id = 0usize;
            for thread in pinned.iter().chain(active.iter()) {
                let Some(project) = project_of(thread) else {
                    continue;
                };
                // Cards have no fold on the web; the collapse reads stay so a
                // previously-collapsed scratch row can only grow detail lines.
                let expanded = !self
                    .sidebar_state
                    .scratch_collapsed
                    .contains(thread.id.as_ref());
                list = list.child(self.render_card_row(
                    thread, row_id, &project, expanded, cx,
                ));
                row_id += 1;
            }

            // Bottom shelves — `SidebarSectionHeader` rows (`mx-0.5 h-8`,
            // `mt-auto` on the first one present) with slim/card rows under
            // each expanded header.
            let mut first_shelf = true;
            if !working.is_empty() {
                list = list.child(self.render_shelf_header(
                    "working",
                    "Working",
                    working.len(),
                    self.sidebar_state.working_open,
                    std::mem::take(&mut first_shelf),
                    cx,
                ));
                if self.sidebar_state.working_open {
                    for thread in &working {
                        let Some(project) = project_of(thread) else {
                            continue;
                        };
                        let expanded = !self
                            .sidebar_state
                            .scratch_collapsed
                            .contains(thread.id.as_ref());
                        list = list.child(self.render_card_row(
                            thread, row_id, &project, expanded, cx,
                        ));
                        row_id += 1;
                    }
                }
            }
            if !snoozed.is_empty() {
                list = list.child(self.render_shelf_header(
                    "snoozed",
                    "Snoozed",
                    snoozed.len(),
                    self.sidebar_state.snoozed_open,
                    std::mem::take(&mut first_shelf),
                    cx,
                ));
                if self.sidebar_state.snoozed_open {
                    for thread in &snoozed {
                        let project_name = project_of(thread).map(|p| p.name);
                        list = list.child(
                            self.render_thread_row(thread, row_id, project_name, false, cx),
                        );
                        row_id += 1;
                    }
                }
            }
            if !settled.is_empty() {
                list = list.child(self.render_shelf_header(
                    "settled",
                    "Settled",
                    settled.len(),
                    self.sidebar_state.settled_open,
                    std::mem::take(&mut first_shelf),
                    cx,
                ));
                if self.sidebar_state.settled_open {
                    for (i, (id, title, project_id, settled_at, pinned)) in
                        settled.iter().enumerate()
                    {
                        let chip = self
                            .state
                            .projects
                            .iter()
                            .find(|p| p.id == *project_id)
                            .map(|p| self.project_chip(p))
                            .unwrap_or_else(|| project_monogram_chip("No project"));
                        let label = rel_time_label(settled_at);
                        let select_id = id.clone();
                        list = list.child(
                            div()
                                .id(("settled-row", i))
                                .flex()
                                .items_center()
                                .h_9()
                                .gap_2p5()
                                .px_2p5()
                                .rounded_md()
                                .text_sm()
                                .cursor_pointer()
                                .text_color(theme::sidebar_fg_muted())
                                .hover(|s| {
                                    s.bg(theme::sidebar_hover())
                                        .text_color(theme::sidebar_fg())
                                })
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _ev, _window, cx| {
                                        this.select_and_visit(select_id.clone(), cx);
                                    }),
                                )
                                .child(chip)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .whitespace_nowrap()
                                        .overflow_x_hidden()
                                        .text_ellipsis()
                                        .child(title.clone()),
                                )
                                .when(*pinned, |row| {
                                    row.child(
                                        icon(Icon::Pin, 12.)
                                            .flex_none()
                                            .text_color(theme::sidebar_faint()),
                                    )
                                })
                                .when(!label.is_empty(), |row| {
                                    row.child(
                                        div()
                                            .flex_none()
                                            .min_w(px(26.))
                                            .text_right()
                                            .text_xs()
                                            .text_color(theme::sidebar_fg_muted())
                                            .child(label),
                                    )
                                }),
                        );
                    }
                }
            }
        }

        // Web `SidebarHeaderIconButton`: `size-7` ghost icon button on the
        // sidebar-icon color, `bg-sidebar-row-hover` on hover plus the
        // `hover:[&>svg]:text-sidebar-foreground` brighten — carried on the
        // svg via `hover_icon` + the button's `.group(id)` since a parent's
        // `.hover(text_color)` can't reach the glyph — and the `TooltipPopup`
        // the web mounts on each trigger.
        let header_icon_button = |id: &'static str, leading: Icon, tip: &'static str| {
            div()
                .id(id)
                .group(id)
                .flex_none()
                .w_7()
                .h_7()
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(theme::sidebar_hover()))
                .tooltip(tooltip(tip))
                .child(hover_icon(
                    leading,
                    16.,
                    theme::sidebar_icon(),
                    theme::sidebar_fg(),
                    id,
                ))
        };
        // Project the header "new thread" button targets — the web writes to
        // the current project; the closest native analogue is the selected
        // thread's project, then the first project.
        let header_thread_project = self
            .state
            .selected
            .as_ref()
            .and_then(|id| self.state.threads.iter().find(|t| t.id == *id))
            .map(|t| t.project_id.clone())
            .or_else(|| self.state.projects.first().map(|p| p.id.clone()));

        let mut root = div()
            .w(px(256.))
            .h_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme::sidebar())
            .border_r_1()
            .border_color(theme::border())
            .when_some(self.sidebar_state.menu_focus.clone(), |el, focus| {
                el.track_focus(&focus)
            })
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _window, cx| {
                if ev.keystroke.key == "escape" {
                    this.sidebar_state.context_menu = None;
                    this.sidebar_state.delete_confirm = None;
                    this.sidebar_state.renaming = None;
                    this.sidebar_state.new_project_open = false;
                    this.sidebar_state.snooze_target = None;
                    this.sidebar_state.snooze_error = None;
                    cx.notify();
                }
            }))
            // Web `SidebarStageBackdrop` (dev "blueprint" variant): the exact
            // SVG artwork behind the brand header, prerendered offline from
            // `DevBlueprintArt` + the `sidebar-stage-backdrop` mask/fade
            // (apps/web) — GPUI has no SVG patterns/gradient transforms, so
            // the baked PNG is the primitive. Alpha fades into the sidebar
            // surface like the web's mask + `--stage-fade` overlay.
            .when_some(aurora_image(), |el, image| {
                el.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(80.))
                        .child(img(gpui::ImageSource::Render(image)).size_full()),
                )
            })
            .child(
                // Web `SidebarChromeHeader`: the 52px workspace topbar with the
                // h-7 brand. With the stage backdrop up the web flips the brand
                // to `text-white` / `text-white/70` (`onBackdrop`).
                div()
                    .h(px(theme::TOPBAR_H))
                    .pl_3()
                    .pr_3()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .h_7()
                            .flex()
                            .items_baseline()
                            .gap_1()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(div().text_color(gpui::white()).child("67"))
                            .child(div().text_color(gpui::rgba(0xffffffb3)).child("Code")),
                    ),
            )
            .child(
                // First `SidebarGroup` (`p-2`) holds the `SidebarThreadHeader`
                // row: h-8 search container + two size-7 icon buttons.
                div()
                    .p_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .h_8()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_md()
                            .px_2()
                            .py_1p5()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::sidebar_fg_muted())
                            .hover(|s| s.bg(theme::sidebar_hover()))
                            .child(
                                icon(Icon::Search, 16.)
                                    .flex_none()
                                    .text_color(theme::sidebar_icon()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(theme::sidebar_fg())
                                    .child(self.search_field.clone()),
                            )
                            // Web's clear-search affordance (icon-micro
                            // ghost button, X at size-3) — only while a
                            // query is present.
                            .when(filtering, |el| {
                                el.child(
                                    div()
                                        .id("clear-search")
                                        .group("clear-search")
                                        .flex_none()
                                        .w_5()
                                        .h_5()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_sm()
                                        .text_color(theme::sidebar_icon())
                                        .cursor_pointer()
                                        .hover(|s| {
                                            s.bg(theme::sidebar_hover())
                                                .text_color(theme::sidebar_fg())
                                        })
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _ev, _window, cx| {
                                                cx.stop_propagation();
                                                reset_field(&this.search_field, cx);
                                                focus_field(&this.search_field, cx);
                                                cx.notify();
                                            }),
                                        )
                                        .child(hover_icon(
                                            Icon::X,
                                            12.,
                                            theme::sidebar_icon(),
                                            theme::sidebar_fg(),
                                            "clear-search",
                                        )),
                                )
                            }),
                    )
                    // Web `SidebarThreadHeader` trailing controls, in order:
                    // project list-filter, new project, new thread.
                    .child(
                        header_icon_button(
                            "list-filter",
                            Icon::ListFilter,
                            "Filter threads by project",
                        )
                        .when(self.sidebar_state.project_filter.is_some(), |b| {
                            b.bg(theme::sidebar_active())
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _window, cx| {
                                cx.stop_propagation();
                                this.sidebar_state.project_filter =
                                    if this.sidebar_state.project_filter.is_some() {
                                        None
                                    } else {
                                        this.state
                                            .selected
                                            .as_ref()
                                            .and_then(|id| {
                                                this.state.threads.iter().find(|t| t.id == *id)
                                            })
                                            .map(|t| t.project_id.clone())
                                            .or_else(|| {
                                                this.state.projects.first().map(|p| p.id.clone())
                                            })
                                    };
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        header_icon_button("new-project", Icon::FolderPlus, "Add project")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, window, cx| {
                                    cx.stop_propagation();
                                    this.sidebar_state.new_project_open =
                                        !this.sidebar_state.new_project_open;
                                    if this.sidebar_state.new_project_open
                                        && let Some(field) =
                                            this.sidebar_state.project_name_field.clone()
                                    {
                                        let focus = field.read(cx).focus_handle(cx);
                                        focus.focus(window, cx);
                                    }
                                    cx.notify();
                                }),
                            ),
                    )
                    .when_some(header_thread_project, |el, project_id| {
                        el.child(
                            header_icon_button("new-thread-header", Icon::SquarePen, "New thread")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _ev, _window, cx| {
                                        cx.stop_propagation();
                                        this.new_thread(project_id.clone(), cx);
                                    }),
                                ),
                        )
                    }),
            )
            .when(self.sidebar_state.new_project_open, |el| {
                el.child(self.render_project_form(cx))
            })
            .child(list)
            .child(
                // `SidebarFooter`: `px-2 pt-1 pb-2`, utility icon buttons —
                // settings, pull requests, usage, then the update/status
                // pill at the far right.
                div()
                    .px_2()
                    .pt_1()
                    .pb_2()
                    .flex()
                    .items_center()
                    .child(
                        // Web `SidebarUtilityItem` — `size="icon"` button
                        // (size-8) carrying a size-4 lucide glyph, wrapped in
                        // the web's `Tooltip`.
                        div()
                            .id("settings-gear")
                            .group("settings-gear")
                            .flex_none()
                            .w_8()
                            .h_8()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .text_color(theme::sidebar_icon())
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::sidebar_fg()).bg(theme::sidebar_hover()))
                            .tooltip(tooltip("Settings"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.settings_state.open = !this.settings_state.open;
                                    cx.notify();
                                }),
                            )
                            .child(hover_icon(
                                Icon::Settings,
                                16.,
                                theme::sidebar_icon(),
                                theme::sidebar_fg(),
                                "settings-gear",
                            )),
                    )
                    .child(
                        div()
                            .id("pull-requests")
                            .group("pull-requests")
                            .flex_none()
                            .w_8()
                            .h_8()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .text_color(theme::sidebar_icon())
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::sidebar_fg()).bg(theme::sidebar_hover()))
                            .tooltip(tooltip("Pull requests"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.set_center_view(CenterView::PullRequests, cx);
                                }),
                            )
                            .child(hover_icon(
                                Icon::GitPullRequest,
                                16.,
                                theme::sidebar_icon(),
                                theme::sidebar_fg(),
                                "pull-requests",
                            )),
                    )
                    .child(
                        div()
                            .id("usage-view")
                            .group("usage-view")
                            .flex_none()
                            .w_8()
                            .h_8()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .text_color(theme::sidebar_icon())
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::sidebar_fg()).bg(theme::sidebar_hover()))
                            .tooltip(tooltip("Usage"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.set_center_view(CenterView::Usage, cx);
                                }),
                            )
                            .child(hover_icon(
                                Icon::ChartNoAxesColumn,
                                16.,
                                theme::sidebar_icon(),
                                theme::sidebar_fg(),
                                "usage-view",
                            )),
                    )
                    .child(div().flex_1().min_w_0())
                    .child(
                        // Far-right circular update/status pill — the web's
                        // `DesktopUpdateStatusIcon` "everything" state is
                        // `CircleArrowUpIcon` on the control surface; the
                        // bundled set has no circled arrow, so it composes
                        // one from a border ring + ArrowUp.
                        div()
                            .id("update-status")
                            .flex_none()
                            .w_7()
                            .h_7()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(theme::sidebar_control())
                            .text_xs()
                            .text_color(theme::sidebar_fg())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::sidebar_hover()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.sidebar_state.context_menu = None;
                                    cx.notify();
                                }),
                            )
                            .child(
                                if matches!(self.state.connection, ConnectionStatus::Connected)
                                {
                                    div()
                                        .w(px(16.))
                                        .h(px(16.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .border_1()
                                        .border_color(theme::sidebar_fg())
                                        .child(
                                            icon(Icon::ArrowUp, 9.)
                                                .text_color(theme::sidebar_fg()),
                                        )
                                        .into_any_element()
                                } else {
                                    icon(Icon::RefreshCw, 14.)
                                        .text_color(status_color)
                                        .into_any_element()
                                },
                            ),
                    ),
            );

        if self.sidebar_state.context_menu.is_some() {
            root = root.child(
                deferred(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(px(10000.))
                        .h(px(10000.))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _window, cx| {
                                this.sidebar_state.context_menu = None;
                                this.sidebar_state.delete_confirm = None;
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, _ev, _window, cx| {
                                this.sidebar_state.context_menu = None;
                                this.sidebar_state.delete_confirm = None;
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        ),
                )
                .with_priority(0),
            );
        }
        if let Some(menu) = self.sidebar_state.context_menu.clone() {
            root = root.child(self.render_context_menu(&menu, cx));
        }

        root
    }
}

impl AppView {
    /// Snooze modal: preset wake times (plus "Unsnooze" while snoozed),
    /// rendered at AppView level above everything else.
    pub(crate) fn render_snooze_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(thread_id) = self.sidebar_state.snooze_target.clone() else {
            return div().into_any_element();
        };
        let now = unix_now();
        let title = self
            .state
            .threads
            .iter()
            .find(|t| t.id == thread_id)
            .map(|t| t.title.to_string())
            .unwrap_or_default();
        let current = self
            .state
            .threads
            .iter()
            .find(|t| t.id == thread_id)
            .and_then(|t| t.snoozed_until.clone())
            .filter(|until| unix_from_iso(until.as_ref()).is_some_and(|t| t > now));

        let mut card = div()
            .flex()
            .flex_col()
            .w(px(320.))
            .p_1()
            .rounded_lg()
            .bg(theme::raised())
            .border_1()
            .border_color(theme::popover_border())
            .shadow_lg()
            .text_sm()
            .text_color(theme::fg())
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _ev, _window, cx| cx.stop_propagation()),
            )
            // Escape inside the card (e.g. with the custom-until field
            // focused) closes the overlay before the App-level CloseOverlay
            // sees it.
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _window, cx| {
                if ev.keystroke.key == "escape" {
                    this.sidebar_state.snooze_target = None;
                    this.sidebar_state.snooze_error = None;
                    if let Some(field) = this.sidebar_state.snooze_field.clone() {
                        reset_field(&field, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(
                div()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::fg_muted())
                            .child("Snooze thread"),
                    )
                    .when(!title.is_empty(), |el| {
                        el.child(
                            div()
                                .text_sm()
                                .text_color(theme::fg())
                                .whitespace_nowrap()
                                .overflow_x_hidden()
                                .text_ellipsis()
                                .child(title),
                        )
                    }),
            )
            .child(div().h(px(1.)).mx_2().my_1().bg(theme::border()));

        if let Some(until) = current {
            let unsnooze_id = thread_id.clone();
            card = card
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(format!(
                            "Snoozed until {}",
                            until.get(..16).unwrap_or(&until)
                        )),
                )
                .child(
                    div()
                        .id("snooze-unsnooze")
                        .min_h(px(28.))
                        .flex()
                        .items_center()
                        .rounded_sm()
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .text_color(theme::primary())
                        .hover(|s| s.bg(theme::accent()).text_color(theme::accent_fg()))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                cx.stop_propagation();
                                this.unsnooze_thread(unsnooze_id.clone(), cx);
                            }),
                        )
                        .child("Unsnooze"),
                )
                .child(div().h(px(1.)).mx_2().my_1().bg(theme::border()));
        }

        for (index, (label, until, when_label)) in
            snooze_presets(now).into_iter().enumerate()
        {
            let until_iso = iso_from_unix(until);
            let when = format!("{when_label} UTC");
            let preset_id = thread_id.clone();
            card = card.child(
                div()
                    .id(("snooze-preset", index))
                    .min_h(px(28.))
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .px_2()
                    .py_1()
                    .justify_between()
                    .gap_2()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::accent()).text_color(theme::accent_fg()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            cx.stop_propagation();
                            this.snooze_thread(preset_id.clone(), until_iso.clone(), cx);
                        }),
                    )
                    .child(div().flex_none().child(label))
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(when),
                    ),
            );
        }

        // Custom "snooze until…" entry — the web's datetime picker, reduced
        // to a text input since GPUI has no calendar widget.
        card = card
            .child(div().h(px(1.)).mx_2().my_1().bg(theme::border()))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .rounded_sm()
                            .bg(theme::field_surface())
                            .border_1()
                            .border_color(theme::input())
                            .px_2()
                            .py_1()
                            .text_sm()
                            .text_color(theme::fg())
                            .when_some(self.sidebar_state.snooze_field.clone(), |el, field| {
                                el.child(field)
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("Enter to snooze until then"),
                    )
                    .when_some(self.sidebar_state.snooze_error.clone(), |el, error| {
                        el.child(div().text_xs().text_color(theme::error_fg()).child(error))
                    }),
            );

        card = card
            .child(div().h(px(1.)).mx_2().my_1().bg(theme::border()))
            .child(
                div()
                    .id("snooze-cancel")
                    .min_h(px(28.))
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::accent()).text_color(theme::accent_fg()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            cx.stop_propagation();
                            this.sidebar_state.snooze_target = None;
                            this.sidebar_state.snooze_error = None;
                            if let Some(field) = this.sidebar_state.snooze_field.clone() {
                                reset_field(&field, cx);
                            }
                            cx.notify();
                        }),
                    )
                    .child("Cancel"),
            );

        div()
            .id("snooze-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::overlay_backdrop())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, _window, cx| {
                    this.sidebar_state.snooze_target = None;
                    this.sidebar_state.snooze_error = None;
                    if let Some(field) = this.sidebar_state.snooze_field.clone() {
                        reset_field(&field, cx);
                    }
                    cx.notify();
                }),
            )
            .child(card)
            .into_any_element()
    }
}
