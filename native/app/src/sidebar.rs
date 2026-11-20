//! Sidebar: project grouping, thread rows, status indicators, context menus.
//! Owns `impl AppView` sidebar render methods and `sidebar::State`.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyElement, Entity, EntityInputHandler, FocusHandle, Focusable, MouseButton, ObjectFit,
    RenderImage, SharedString, Subscription, Window, anchored, deferred, div, img, point,
    prelude::*, px, rgb,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppView;
use crate::CenterView;
use crate::composer::{Attachment, AttachmentKind};
use crate::config;
use crate::icons::{Icon, icon};
use crate::model::{ConnectionStatus, Project, Thread};
use crate::notifications::{ToastAction, ToastKind};
use crate::text_field::{PromptSubmitted, TextField};
use crate::theme;

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
    /// The collapsed "Settled" shelf is expanded.
    pub settled_open: bool,
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
    /// Title from the raw shell (used when the thread isn't in the parsed
    /// `state.threads`, e.g. older settled rows).
    pub title: String,
    /// Owning project id for settled-row grouping chips.
    pub project_id: String,
    /// `pinnedAt != null`.
    pub pinned: bool,
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

/// Snooze presets mirroring the web defaults
/// (`packages/contracts/settings.ts` DEFAULT_SNOOZE_PRESETS): durations plus
/// fixed wake times. With no timezone database in the app, fixed times
/// resolve in UTC.
fn snooze_presets(now: i64) -> Vec<(&'static str, i64)> {
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
        ("In 1 hour", now + 3_600),
        ("In 4 hours", now + 14_400),
        ("Tomorrow", (today + 1) * DAY + 9 * 3_600),
        ("Next week", (today + monday_ahead) * DAY + 9 * 3_600),
    ];
    // "This evening" (today 18:00) only while it's still ahead, like the web.
    let tonight = today * DAY + 18 * 3_600;
    if tonight > now {
        presets.push(("This evening", tonight));
    }
    presets.sort_by_key(|(_, until)| *until);
    presets
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

    /// "Mark read": `thread.visit` clears `markedUnreadAt`; `opened:false`
    /// keeps it a watermark update rather than a selection.
    fn mark_read(&mut self, thread_id: SharedString, cx: &mut Context<Self>) {
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
            "opened": false,
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
        if let Some(title) = shell.get("title").and_then(Value::as_str) {
            entry.title = title.to_string();
        }
        if let Some(project) = shell.get("projectId").and_then(Value::as_str) {
            entry.project_id = project.to_string();
        }
        if shell.get("pinnedAt").is_some_and(|p| !p.is_null()) {
            entry.pinned = true;
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
            .unwrap_or_else(|| project_monogram_chip(project.name.as_ref()))
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

    /// "Move up/down" on a pinned row (context-menu fallback for pointer
    /// drag).
    fn move_pinned(&mut self, thread_id: SharedString, dir: i32, cx: &mut Context<Self>) {
        let Some(project_id) = self
            .state
            .threads
            .iter()
            .find(|t| t.id == thread_id)
            .map(|t| t.project_id.clone())
        else {
            return;
        };
        let mut ordered = self.pinned_order(project_id.as_ref());
        let Some(ix) = ordered.iter().position(|t| t.id == thread_id) else {
            return;
        };
        let target = ix as i32 + dir;
        if target < 0 || target as usize >= ordered.len() {
            return;
        }
        ordered.swap(ix, target as usize);
        self.commit_pinned_order(ordered, target as usize);
        cx.notify();
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

    /// One context-menu row — shared so every item keeps the same styling.
    /// Metrics mirror the web `MenuItem`: `min-h-7 gap-2 rounded-sm px-2 py-1
    /// text-sm`, `bg-accent` + `text-accent-foreground` on highlight, with a
    /// `size-4` `text-muted-foreground` leading icon column. Danger items are
    /// `text-destructive-foreground` with a `bg-destructive/10` hover tint
    /// and a current-ink icon; `submenu` adds the web's trailing
    /// `chevron-right` affordance.
    fn menu_item(
        &self,
        id: &'static str,
        leading: Icon,
        label: &'static str,
        danger: bool,
        submenu: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
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
            .cursor_pointer()
            .when(danger, |el| el.text_color(theme::error_fg()))
            .hover(move |s| {
                if danger {
                    s.bg(theme::destructive_tint())
                        .text_color(theme::error_fg())
                } else {
                    s.bg(theme::accent()).text_color(theme::accent_fg())
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, window, cx| {
                    cx.stop_propagation();
                    on_click(this, window, cx);
                }),
            )
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
                    .child(label),
            );
        if submenu {
            item = item.child(
                icon(Icon::ChevronRight, 16.)
                    .flex_none()
                    .text_color(theme::fg_muted()),
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
                    .child(icon(Icon::Check, 10.).text_color(theme::sidebar_fg_muted()))
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

    fn render_context_menu(&self, menu: &ContextMenu, cx: &mut Context<Self>) -> impl IntoElement {
        let thread = self
            .state
            .threads
            .iter()
            .find(|t| t.id == menu.thread_id)
            .cloned();
        let pinned = thread.as_ref().is_some_and(|t| t.pinned);
        let snoozed = thread.as_ref().is_some_and(|t| self.is_snoozed(t));
        let unread = thread.as_ref().is_some_and(|t| self.is_unread(t));
        let confirming = self.sidebar_state.delete_confirm.as_ref() == Some(&menu.thread_id);
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

        let rename_id = menu.thread_id.clone();
        menu_el = menu_el.child(self.menu_item(
            "mi-rename",
            Icon::Pencil,
            "Rename",
            false,
            false,
            cx,
            move |this, window, cx| {
                this.sidebar_state.context_menu = None;
                this.start_rename(rename_id.clone(), window, cx);
            },
        ));

        let pin_id = menu.thread_id.clone();
        menu_el = menu_el.child(self.menu_item(
            "mi-pin",
            Icon::Pin,
            if pinned { "Unpin" } else { "Pin" },
            false,
            false,
            cx,
            move |this, _window, cx| {
                this.sidebar_state.context_menu = None;
                if let Some(thread) = this.state.threads.iter_mut().find(|t| t.id == pin_id) {
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

        // Pinned rows can be nudged within the pinned block.
        if pinned && let Some(project_id) = thread.as_ref().map(|t| t.project_id.clone()) {
            let ordered = self.pinned_order(project_id.as_ref());
            if let Some(index) = ordered.iter().position(|t| t.id == menu.thread_id) {
                if index > 0 {
                    let move_id = menu.thread_id.clone();
                    menu_el = menu_el.child(self.menu_item(
                        "mi-move-up",
                        Icon::ChevronUp,
                        "Move up",
                        false,
                        false,
                        cx,
                        move |this, _window, cx| {
                            this.sidebar_state.context_menu = None;
                            this.move_pinned(move_id.clone(), -1, cx);
                        },
                    ));
                }
                if index + 1 < ordered.len() {
                    let move_id = menu.thread_id.clone();
                    menu_el = menu_el.child(self.menu_item(
                        "mi-move-down",
                        Icon::ChevronDown,
                        "Move down",
                        false,
                        false,
                        cx,
                        move |this, _window, cx| {
                            this.sidebar_state.context_menu = None;
                            this.move_pinned(move_id.clone(), 1, cx);
                        },
                    ));
                }
            }
        }

        let read_id = menu.thread_id.clone();
        menu_el = menu_el.child(self.menu_item(
            if unread {
                "mi-mark-read"
            } else {
                "mi-mark-unread"
            },
            if unread { Icon::Eye } else { Icon::Bell },
            if unread { "Mark read" } else { "Mark unread" },
            false,
            false,
            cx,
            move |this, _window, cx| {
                this.sidebar_state.context_menu = None;
                if unread {
                    this.mark_read(read_id.clone(), cx);
                } else {
                    this.mark_unread(read_id.clone(), cx);
                }
            },
        ));

        let snooze_id = menu.thread_id.clone();
        menu_el = menu_el.child(self.menu_item(
            "mi-snooze",
            Icon::Clock,
            "Snooze…",
            false,
            // Web's Snooze row has children, so it gets the submenu chevron;
            // ours opens the snooze overlay but reads the same way.
            true,
            cx,
            move |this, window, cx| {
                this.sidebar_state.context_menu = None;
                this.open_snooze_overlay(snooze_id.clone(), window, cx);
            },
        ));
        if snoozed {
            let unsnooze_id = menu.thread_id.clone();
            menu_el = menu_el.child(self.menu_item(
                "mi-unsnooze",
                Icon::Clock,
                "Unsnooze",
                false,
                false,
                cx,
                move |this, _window, cx| {
                    this.sidebar_state.context_menu = None;
                    this.unsnooze_thread(unsnooze_id.clone(), cx);
                },
            ));
        }

        let archive_id = menu.thread_id.clone();
        let archive_title = thread.as_ref().map(|t| t.title.to_string());
        menu_el = menu_el
            .child(separator())
            .child(self.menu_item(
                "mi-archive",
                Icon::Archive,
                "Archive",
                false,
                false,
                cx,
                move |this, _window, cx| {
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
            ))
            .child(separator());

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
            cx,
            move |this, _window, cx| {
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

        deferred(
            anchored()
                .position(point(menu.position.x, menu.position.y))
                .child(menu_el),
        )
        .with_priority(1)
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

        let query = self.sidebar_state.query.trim().to_lowercase();
        let filtering = !query.is_empty();

        let status_color = match &self.state.connection {
            ConnectionStatus::Connected => theme::primary(),
            ConnectionStatus::Failed(_) | ConnectionStatus::Disconnected(_) => theme::error(),
            _ => theme::fg_muted(),
        };

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
            let mut row_id = 0usize;
            for (pi, project) in projects.iter().enumerate() {
                let toggle_id = project.id.clone();
                let new_thread_id = project.id.clone();
                // Web `SidebarProjectFolderHeader`: collapsed folders surface the
                // most urgent hidden-thread state as a size-1.5 dot and show the
                // hidden-thread count (priority attention > failed > unread >
                // working — `resolveSidebarFolderDot`).
                let hidden: Vec<&Thread> = self
                    .state
                    .threads
                    .iter()
                    .filter(|t| t.project_id == project.id)
                    .filter(|t| live_thread(t))
                    .collect();
                // Web `SidebarProjectFolderHeader` right slot: the newest
                // member thread's relative activity stamp.
                let header_time = hidden
                    .iter()
                    .map(|t| t.updated_at.as_ref())
                    .max()
                    .map(rel_time_label)
                    .unwrap_or_default();
                let folder_dot = if project.expanded || hidden.is_empty() {
                    None
                } else if hidden
                    .iter()
                    .any(|t| t.has_pending_request || t.has_actionable_proposed_plan)
                {
                    Some(theme::warning())
                } else if hidden
                    .iter()
                    .any(|t| matches!(t.status.as_ref(), "failed" | "error"))
                {
                    Some(theme::error())
                } else if hidden.iter().any(|t| self.is_unread(t)) {
                    Some(theme::success())
                } else if hidden.iter().any(|t| t.running()) {
                    Some(theme::info())
                } else {
                    None
                };
                // Web `SidebarProjectFolderHeader`: a size-3 muted
                // ChevronDown rotated -90° while collapsed.
                let chevron = if project.expanded {
                    icon(Icon::ChevronDown, 12.)
                } else {
                    icon(Icon::ChevronRight, 12.)
                };
                let header = div()
                    .id(("project", pi))
                    .group("project-folder")
                    .flex()
                    .items_center()
                    .h_8()
                    .gap_2p5()
                    .px_2p5()
                    .rounded_lg()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::sidebar_hover()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            if let Some(project) =
                                this.state.projects.iter_mut().find(|p| p.id == toggle_id)
                            {
                                project.expanded = !project.expanded;
                            }
                            cx.notify();
                        }),
                    )
                    // A loaded favicon beats the icon chip; failures and
                    // in-flight fetches fall back to the icon chip, then the
                    // derived `ProjectMonogram` tile (web's default).
                    .child(self.project_chip(project))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .overflow_x_hidden()
                            .text_ellipsis()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::sidebar_fg_soft())
                            .child(project.name.clone()),
                    )
                    .when_some(folder_dot, |row, color| {
                        row.child(
                            div()
                                .flex_none()
                                .w(px(6.))
                                .h(px(6.))
                                .rounded_full()
                                .bg(color),
                        )
                    })
                    .when(!header_time.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme::sidebar_fg_muted())
                                .child(header_time),
                        )
                    })
                    .when(!project.expanded && !hidden.is_empty(), |row| {
                        // Web hides the count while the folder is hovered —
                        // the new-thread button appears in its place.
                        row.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme::sidebar_fg_muted())
                                .group_hover("project-folder", |s| s.opacity(0.))
                                .child(format!("{}", hidden.len())),
                        )
                    })
                    .child(
                        div()
                            .id(("new-thread", pi))
                            .flex_none()
                            .w_6()
                            .h_6()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .text_sm()
                            .text_color(theme::sidebar_icon())
                            .opacity(0.)
                            .group_hover("project-folder", |s| s.opacity(1.))
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::sidebar_fg()).bg(theme::sidebar_hover()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.new_thread(new_thread_id.clone(), cx);
                                }),
                            )
                            // Web's in-folder new-thread button is an
                            // `icon-xs` SquarePen → pencil at 14px. Svg
                            // doesn't inherit the parent's ink — carry it.
                            .child(icon(Icon::Pencil, 14.).text_color(theme::sidebar_icon())),
                    )
                    .child(chevron.flex_none().text_color(theme::sidebar_fg_muted()));

                // Web folders get `mt-2 first:mt-0` breathing room.
                let mut section = div()
                    .flex()
                    .flex_col()
                    .when(pi > 0, |s| s.mt_2())
                    .child(header);
                if project.expanded {
                    let mut threads: Vec<Thread> = self
                        .state
                        .threads
                        .iter()
                        .filter(|t| t.project_id == project.id)
                        .filter(|t| live_thread(t))
                        .cloned()
                        .collect();
                    // Pinned first (ordered by pinOrderKey like the web's
                    // pinned block), then awake threads, snoozed last.
                    threads.sort_by_key(|t| {
                        (
                            !t.pinned,
                            t.pin_order_key.is_none(),
                            t.pin_order_key.clone().unwrap_or_default(),
                            self.is_snoozed(t),
                            Reverse(t.updated_at.clone()),
                        )
                    });
                    // An expanded empty folder renders nothing on the web —
                    // the header alone is the affordance, so no empty-state
                    // row here.
                    for thread in threads.iter() {
                        section =
                            section.child(self.render_thread_row(thread, row_id, None, true, cx));
                        row_id += 1;
                    }
                }
                list = list.child(section);
            }
        }

        // Web `SidebarHeaderIconButton`: `size-7` ghost icon button on the
        // sidebar-icon color, `bg-sidebar-row-hover` on hover. GPUI svgs
        // don't inherit the parent's text color (and Svg has no hover
        // styling), so the icon carries its own sidebar-icon ink; the web's
        // `hover:[&>svg]:text-sidebar-foreground` brighten isn't reachable.
        let header_icon_button = |id: &'static str, leading: Icon| {
            div()
                .id(id)
                .flex_none()
                .w_7()
                .h_7()
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(theme::sidebar_hover()))
                .child(icon(leading, 16.).text_color(theme::sidebar_icon()))
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
            // Web `SidebarStageBackdrop`: a blue aurora behind the brand
            // header, fading out ~80px down the sidebar. GPUI has no radial
            // blur, so it is approximated with layered linear gradients.
            .child(div().absolute().top_0().left_0().right_0().h(px(88.)).bg(
                gpui::linear_gradient(
                    180.,
                    gpui::linear_color_stop(gpui::hsla(220. / 360., 0.84, 0.60, 0.22), 0.),
                    gpui::linear_color_stop(gpui::hsla(220. / 360., 0.84, 0.60, 0.0), 1.),
                ),
            ))
            .child(
                div()
                    .absolute()
                    .top(px(-24.))
                    .left(px(-40.))
                    .w(px(200.))
                    .h(px(56.))
                    .rounded_full()
                    .bg(gpui::linear_gradient(
                        115.,
                        gpui::linear_color_stop(gpui::hsla(217. / 360., 0.91, 0.65, 0.35), 0.),
                        gpui::linear_color_stop(gpui::hsla(240. / 360., 0.75, 0.65, 0.0), 1.),
                    )),
            )
            .child(
                // Web `SidebarChromeHeader`: the 52px workspace topbar with the
                // h-7 brand — 67 wordmark in foreground + "Code" muted.
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
                            .child(div().text_color(theme::sidebar_fg()).child("67"))
                            .child(div().text_color(theme::sidebar_fg_muted()).child("Code")),
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
                                        .child(
                                            icon(Icon::X, 12.).text_color(theme::sidebar_icon()),
                                        ),
                                )
                            }),
                    )
                    // Web `SidebarThreadHeader` trailing controls, in order:
                    // project list-filter, new project, new thread.
                    .child(
                        header_icon_button("list-filter", Icon::ListFilter)
                            .when(self.sidebar_state.project_filter.is_some(), |b| {
                                b.bg(theme::sidebar_active())
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.sidebar_state.project_filter = if this
                                        .sidebar_state
                                        .project_filter
                                        .is_some()
                                    {
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
                        header_icon_button("new-project", Icon::FolderPlus).on_mouse_down(
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
                            header_icon_button("new-thread-header", Icon::SquarePen).on_mouse_down(
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
            // Web `SidebarSettledSection`: collapsible "Settled N" shelf
            // pinned below the thread list.
            .when(!settled.is_empty(), |el| {
                let mut shelf = div()
                    .id("settled-shelf")
                    .px_2()
                    .pb_1()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("settled-header")
                            .flex()
                            .items_center()
                            .h_7()
                            .gap_2()
                            .px_2p5()
                            .rounded_md()
                            .cursor_pointer()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::sidebar_fg_muted())
                            .hover(|s| s.bg(theme::sidebar_hover()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    this.sidebar_state.settled_open =
                                        !this.sidebar_state.settled_open;
                                    cx.notify();
                                }),
                            )
                            .child(div().flex_1().min_w_0().child("Settled"))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme::sidebar_fg_muted())
                                    .child(format!("{}", settled.len())),
                            )
                            .child(
                                if self.sidebar_state.settled_open {
                                    icon(Icon::ChevronDown, 12.)
                                } else {
                                    icon(Icon::ChevronRight, 12.)
                                }
                                .flex_none()
                                .text_color(theme::sidebar_fg_muted()),
                            ),
                    );
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
                        shelf = shelf.child(
                            div()
                                .id(("settled-row", i))
                                .flex()
                                .items_center()
                                .h_8()
                                .gap_2p5()
                                .px_2p5()
                                .rounded_md()
                                .text_sm()
                                .cursor_pointer()
                                .text_color(theme::sidebar_fg_muted())
                                .hover(|s| s.bg(theme::sidebar_hover()))
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
                el.child(shelf)
            })
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
                        // (size-8) carrying a size-4 lucide glyph.
                        div()
                            .id("settings-gear")
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
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.settings_state.open = !this.settings_state.open;
                                    cx.notify();
                                }),
                            )
                            .child(icon(Icon::Settings, 16.).text_color(theme::sidebar_icon())),
                    )
                    .child(
                        div()
                            .id("pull-requests")
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
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.set_center_view(CenterView::PullRequests, cx);
                                }),
                            )
                            .child(
                                icon(Icon::GitPullRequest, 16.).text_color(theme::sidebar_icon()),
                            ),
                    )
                    .child(
                        div()
                            .id("usage-view")
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
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.set_center_view(CenterView::Usage, cx);
                                }),
                            )
                            .child(
                                icon(Icon::ChartNoAxesColumn, 16.)
                                    .text_color(theme::sidebar_icon()),
                            ),
                    )
                    .child(div().flex_1().min_w_0())
                    .child(
                        // Far-right circular update/status pill — mirrors
                        // the web's update button; ink reflects connection.
                        div()
                            .id("update-status")
                            .flex_none()
                            .w_7()
                            .h_7()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .text_xs()
                            .text_color(status_color)
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
                                icon(
                                    if matches!(self.state.connection, ConnectionStatus::Connected)
                                    {
                                        Icon::CircleCheck
                                    } else {
                                        Icon::RefreshCw
                                    },
                                    14.,
                                )
                                .text_color(status_color),
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

        for (index, (label, until)) in snooze_presets(now).into_iter().enumerate() {
            let until_iso = iso_from_unix(until);
            let when = format!("{} {} UTC", &until_iso[5..10], &until_iso[11..16]);
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
