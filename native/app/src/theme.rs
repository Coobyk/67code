//! Design tokens ported from apps/web/src/index.css — exact resolved values
//! for the web's default dark + light themes. Every surface should consume
//! these constants instead of hardcoding colors or metrics.

use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{Rgba, px, rgb, rgba};

pub use crate::config::ThemeMode;

static MODE: RwLock<ThemeMode> = RwLock::new(ThemeMode::Dark);
static SYSTEM_DARK: AtomicBool = AtomicBool::new(true);

/// Update the active mode. `system_dark` is the OS appearance — only used
/// when mode is `System`.
pub fn set_mode(mode: ThemeMode, system_dark: bool) {
    SYSTEM_DARK.store(system_dark, Ordering::Relaxed);
    if let Ok(mut m) = MODE.write() {
        *m = mode;
    }
}

pub fn mode() -> ThemeMode {
    MODE.read().map(|m| *m).unwrap_or_default()
}

/// True when surfaces should use the dark palette.
pub fn effective_dark() -> bool {
    match mode() {
        ThemeMode::Dark => true,
        ThemeMode::Light => false,
        ThemeMode::System => SYSTEM_DARK.load(Ordering::Relaxed),
    }
}

fn pick(dark: Rgba, light: Rgba) -> Rgba {
    if effective_dark() { dark } else { light }
}

// ── Typography ─────────────────────────────────────────────────────
/// Web `--font-sans`: Inter Variable bundled under native/app/fonts (OFL).
pub const FONT_SANS: &str = "Inter";
/// Web `--font-mono`: ui-monospace / SF Mono / Menlo / Consolas.
pub const FONT_MONO: &str = "monospace";

/// Web text size scale (Tailwind + the 2xs/3xs customs).
pub mod text_size {
    use gpui::{Pixels, px};
    pub const XS_3: Pixels = px(10.0); // --text-3xs
    pub const XS_2: Pixels = px(11.0); // --text-2xs
    pub const XS: Pixels = px(12.0);
    pub const SM: Pixels = px(14.0);
    pub const BASE: Pixels = px(16.0);
    pub const LG: Pixels = px(18.0);
}

// ── Layout metrics ────────────────────────────────────────────────
/// `--radius` = 0.5rem → lg; sm/md/xl/2xl/3xl per the css formulas.
pub mod radius {
    use gpui::{Pixels, px};
    pub const SM: Pixels = px(4.0);
    pub const MD: Pixels = px(6.0);
    pub const LG: Pixels = px(8.0);
    pub const XL: Pixels = px(12.0);
    pub const XL2: Pixels = px(16.0);
    pub const XL3: Pixels = px(20.0);
}

/// `--app-scrollbar-width`.
pub const SCROLLBAR_W: f32 = 6.0;
/// `--workspace-topbar-height` (max(40px, 52px)).
pub const TOPBAR_H: f32 = 52.0;
/// `--chat-content-max-width` = 46rem.
pub const CHAT_MAX_W: f32 = 736.0;
/// `--thread-details-panel-width` = 17.5rem.
pub const DETAILS_PANEL_W: f32 = 280.0;
/// `--control-radius` = 0.5rem.
pub const CONTROL_RADIUS: f32 = 8.0;

// ── Color roles (resolved hexes from index.css) ───────────────────
/// `--background`. dark #000 · light zinc-25 #fcfcfc
pub fn canvas() -> Rgba {
    pick(rgb(0x000000), rgb(0xfcfcfc))
}
/// `--foreground`. dark neutral-100 #f5f5f5 · light zinc-900 #18181b
pub fn fg() -> Rgba {
    pick(rgb(0xf5f5f5), rgb(0x18181b))
}
/// `--card`/`--popover`. dark neutral-950·97%+white·3% #111111 · light #ffffff
pub fn raised() -> Rgba {
    pick(rgb(0x111111), rgb(0xffffff))
}
/// `--border`. dark white·8% · light zinc-200 #e4e4e7
pub fn border() -> Rgba {
    pick(rgba(0xffffff14), rgb(0xe4e4e7))
}
/// `--input`. dark white·11% · light zinc-300 #d4d4d8
pub fn input() -> Rgba {
    pick(rgba(0xffffff1c), rgb(0xd4d4d8))
}
/// `--muted` (surface). dark white·3% · light zinc-50 #fafafa
pub fn muted() -> Rgba {
    pick(rgba(0xffffff08), rgb(0xfafafa))
}
/// `--muted-foreground`. dark neutral-400 #a3a3a3 · light zinc-500 #71717a
pub fn fg_muted() -> Rgba {
    pick(rgb(0xa3a3a3), rgb(0x71717a))
}
/// `--placeholder` = muted-foreground.
pub fn placeholder() -> Rgba {
    fg_muted()
}
/// `--secondary` (surface). dark white·3% · light zinc-50
pub fn secondary() -> Rgba {
    pick(rgba(0xffffff08), rgb(0xfafafa))
}
/// `--secondary-foreground`. dark neutral-100 · light zinc-800 #27272a
pub fn secondary_fg() -> Rgba {
    pick(rgb(0xf5f5f5), rgb(0x27272a))
}
/// `--accent` (surface). dark white·4% · light zinc-100 #f4f4f5
pub fn accent() -> Rgba {
    pick(rgba(0xffffff0a), rgb(0xf4f4f5))
}
/// `--accent-foreground`. dark neutral-100 · light zinc-900
pub fn accent_fg() -> Rgba {
    pick(rgb(0xf5f5f5), rgb(0x18181b))
}
/// `--primary`. dark oklch(0.571 .21 264)=#346bf1 · light oklch(0.488 .217 264)=#1b4ed8
pub fn primary() -> Rgba {
    pick(rgb(0x346bf1), rgb(0x1b4ed8))
}
/// `--primary-foreground` = white.
pub fn primary_fg() -> Rgba {
    rgb(0xffffff)
}
/// Generic hover = `--sidebar-row-hover` (fg·8%). Generic selected = 7%.
pub fn hover() -> Rgba {
    pick(rgba(0xffffff14), rgba(0x0000000a))
}
pub fn selected() -> Rgba {
    pick(rgba(0xffffff12), rgba(0x00000010))
}
/// `--message-surface` (user bubble). dark neutral-950·92%+white·8% #1e1e1e · light #ebebed
pub fn user_bubble() -> Rgba {
    pick(rgb(0x1e1e1e), rgb(0xebebed))
}
/// `--code-background`. dark ~#0f0f0f · light ~#fbfbfb
pub fn code_bg() -> Rgba {
    pick(rgb(0x0f0f0f), rgb(0xfbfbfb))
}
/// `--error` / `--destructive`. dark #f15757 · light #ef4444
pub fn error() -> Rgba {
    pick(rgb(0xf15757), rgb(0xef4444))
}
/// `--error-foreground`. dark red-400 #f87171 · light red-700 #b91c1c
pub fn error_fg() -> Rgba {
    pick(rgb(0xf87171), rgb(0xb91c1c))
}
/// `--error-surface`: error at 16%/8% alpha.
pub fn error_surface() -> Rgba {
    pick(rgba(0xf1575729), rgba(0xef444414))
}
/// `--info-foreground`. dark blue-400 · light blue-700
pub fn info_fg() -> Rgba {
    pick(rgb(0x60a5fa), rgb(0x1d4ed8))
}
/// `--success`. dark/light emerald-500 #10b981
pub fn success() -> Rgba {
    pick(rgb(0x10b981), rgb(0x10b981))
}
/// `--success-foreground`. dark emerald-400 · light emerald-700
pub fn success_fg() -> Rgba {
    pick(rgb(0x34d399), rgb(0x047857))
}
/// `--warning`. amber-500 #f59e0b
pub fn warning() -> Rgba {
    pick(rgb(0xf59e0b), rgb(0xf59e0b))
}
/// `--warning-foreground`. dark amber-400 · light amber-700
pub fn warning_fg() -> Rgba {
    pick(rgb(0xfbbf24), rgb(0xb45309))
}
/// `--warning-surface`: warning at 16%/8% alpha.
pub fn warning_surface() -> Rgba {
    pick(rgba(0xf59e0b29), rgba(0xf59e0b14))
}
/// `--update-foreground`. dark blue-400 · light primary
pub fn update_fg() -> Rgba {
    pick(rgb(0x60a5fa), rgb(0x1b4ed8))
}
/// `--guest` fuchsia-500 · `--guest-foreground` fuchsia-300/600.
pub fn guest() -> Rgba {
    pick(rgb(0xd946ef), rgb(0xd946ef))
}
pub fn guest_fg() -> Rgba {
    pick(rgb(0xf0abfc), rgb(0xc026d3))
}
/// `--guest-surface`: guest 14% over message surface.
pub fn guest_surface() -> Rgba {
    pick(rgba(0xd946ef24), rgba(0xd946ef24))
}

// ── Sidebar roles ([data-app-sidebar]) ────────────────────────────
/// `--sidebar`. dark #000 · light zinc-50 #fafafa
pub fn sidebar() -> Rgba {
    pick(rgb(0x000000), rgb(0xfafafa))
}
/// `--sidebar-foreground`. dark #f1f3f7 · light zinc-900
pub fn sidebar_fg() -> Rgba {
    pick(rgb(0xf1f3f7), rgb(0x18181b))
}
/// `--sidebar-muted-foreground`. dark #a3a3a3 · light zinc-500
pub fn sidebar_fg_muted() -> Rgba {
    pick(rgb(0xa3a3a3), rgb(0x71717a))
}
/// `--secondary-label` — row metadata a step below titles. dark #8f8f8f
pub fn sidebar_secondary_label() -> Rgba {
    pick(rgb(0x8f8f8f), rgb(0x71717a))
}
/// `--sidebar-control-surface`. dark muted·(#0a0a0a) · light zinc-100
pub fn sidebar_control() -> Rgba {
    pick(rgb(0x0a0a0a), rgb(0xf4f4f5))
}
/// `--sidebar-row-hover` (fg·8% / zinc-25).
pub fn sidebar_hover() -> Rgba {
    pick(rgba(0xf1f3f714), rgb(0xfcfcfc))
}
/// `--sidebar-row-active` (fg·11% / white).
pub fn sidebar_active() -> Rgba {
    pick(rgba(0xf1f3f71c), rgb(0xffffff))
}
/// `--sidebar-row-selected` (fg·7% / white).
pub fn sidebar_selected() -> Rgba {
    pick(rgba(0xf1f3f712), rgb(0xffffff))
}

// ── Terminal ──────────────────────────────────────────────────────
/// `--terminal-cursor`. dark #b4cbff · light #26384e
pub fn terminal_cursor() -> Rgba {
    pick(rgb(0xb4cbff), rgb(0x26384e))
}
/// `--terminal-selection-background`. dark rgba(180,203,255,.25) · light rgba(37,63,99,.2)
pub fn selection() -> Rgba {
    pick(rgba(0xb4cbff40), rgba(0x253f6333))
}
/// Text cursor (TextField caret).
pub fn cursor() -> Rgba {
    pick(rgb(0xf5f5f5), rgb(0x18181b))
}
