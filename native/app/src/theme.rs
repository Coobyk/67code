//! Design tokens ported from apps/web/src/index.css — exact resolved values
//! for the web's default dark + light themes. Every surface should consume
//! these constants instead of hardcoding colors or metrics.
#![allow(dead_code)] // Token palette — supersets are intentional.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};

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

// ── Appearance tuning (Interface sliders) ─────────────────────────
/// The web's `appearanceContrast` (50–200, default 100) and
/// `glassOpacity` (40–100, default 80) unified settings, mirrored
/// device-local in `ClientConfig`. Loaded lazily from the persisted
/// config so every surface sees stored values from first paint;
/// the settings sliders push live updates through `set_tuning`
/// (`ClientConfig::save` stays the caller's job).
static TUNING: OnceLock<RwLock<(i32, i32)>> = OnceLock::new();

fn tuning() -> (i32, i32) {
    TUNING
        .get_or_init(|| {
            let cfg = crate::config::ClientConfig::load();
            RwLock::new((cfg.appearance_contrast, cfg.glass_opacity))
        })
        .read()
        .map(|t| *t)
        .unwrap_or((100, 80))
}

/// Push new slider values into the live tuning.
pub fn set_tuning(appearance_contrast: i32, glass_opacity: i32) {
    if let Ok(mut t) = TUNING
        .get_or_init(|| RwLock::new((appearance_contrast, glass_opacity)))
        .write()
    {
        *t = (appearance_contrast, glass_opacity);
    }
}

/// Border/hairline strength multiplier — `appearanceContrast / 100`
/// clamped to the contract's 0.5–2.0 range.
pub fn contrast_factor() -> f32 {
    (tuning().0 as f32 / 100.).clamp(0.5, 2.)
}

/// Glass alpha multiplier — `glassOpacity / 100` clamped to the
/// contract's 0.4–1.0 range.
pub fn glass_factor() -> f32 {
    (tuning().1 as f32 / 100.).clamp(0.4, 1.)
}

/// Scale a translucent token's alpha by `factor` (contrast effect on
/// dark-mode hairlines, which are stored as white-at-alpha).
fn scaled_alpha(color: Rgba, factor: f32) -> Rgba {
    Rgba {
        a: (color.a * factor).clamp(0., 1.),
        ..color
    }
}

/// Nudge a solid color toward black (`f` > 0) or white (`f` < 0) —
/// the light-mode side of the contrast effect, where borders are
/// opaque hexes rather than fg-at-alpha.
fn tint(color: Rgba, f: f32) -> Rgba {
    let (target, w) = if f >= 0. {
        (0f32, f.min(1.))
    } else {
        (1f32, -f.max(-1.))
    };
    Rgba {
        r: color.r + (target - color.r) * w,
        g: color.g + (target - color.g) * w,
        b: color.b + (target - color.b) * w,
        a: color.a,
    }
}

/// `appearanceContrast` applied to a border-family token: alpha-scale
/// dark-mode hairlines, lightness-nudge light-mode solid borders.
fn contrast_border(dark: Rgba, light: Rgba) -> Rgba {
    let c = contrast_factor();
    if effective_dark() {
        scaled_alpha(dark, c)
    } else if c >= 1. {
        tint(light, (c - 1.) * 0.25)
    } else {
        tint(light, -(1. - c) * 0.6)
    }
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
    contrast_border(rgba(0xffffff14), rgb(0xe4e4e7))
}
/// `border-border/70` — the model picker's pane separators and footer top
/// edge (`border-l border-border/70`, `border-t border-border/70`).
pub fn border_70() -> Rgba {
    contrast_border(rgba(0xffffff0e), rgba(0xe4e4e7b3))
}
/// `bg-foreground/10` — icon-button hover/selected fill on the provider rail.
pub fn fg_10() -> Rgba {
    pick(rgba(0xf5f5f51a), rgba(0x18181b1a))
}
/// `border-border/60` — the web's 60%-border (SettingsGroup card outline).
/// (`border_soft` already covers the terminal drawer's /80 hairline.)
pub fn card_border() -> Rgba {
    contrast_border(rgba(0xffffff0d), rgba(0xe4e4e799))
}
/// `bg-card/40` — the web SettingsGroup card fill (--card · 40%).
pub fn card_surface_40() -> Rgba {
    pick(rgba(0x11111166), rgba(0xffffff66))
}
/// `bg-card/60` — the web ThemeLibraryCard fill (Appearance theme cards).
pub fn card_surface_60() -> Rgba {
    pick(rgba(0x11111199), rgba(0xffffff99))
}
/// `border-border/50` — the web SettingsGroup row divider (a shade lighter
/// than the card's /60 outline).
pub fn card_divider() -> Rgba {
    contrast_border(rgba(0xffffff0a), rgba(0xe4e4e780))
}
/// `--input`. dark white·11% · light zinc-300 #d4d4d8
pub fn input() -> Rgba {
    contrast_border(rgba(0xffffff1c), rgb(0xd4d4d8))
}
/// `bg-input/40` — segmented-control track (picker footer speed/permissions).
pub fn input_40() -> Rgba {
    pick(rgba(0xffffff0b), rgba(0xd4d4d866))
}
/// `bg-input/72` — the checked segment inside an `bg-input/40` track.
pub fn input_72() -> Rgba {
    pick(rgba(0xffffff14), rgba(0xd4d4d8b8))
}
/// `--muted` (surface). dark white·3% · light zinc-50 #fafafa
pub fn muted() -> Rgba {
    pick(rgba(0xffffff08), rgb(0xfafafa))
}
/// `bg-muted/30` — the model picker's left provider rail.
pub fn muted_30() -> Rgba {
    pick(rgba(0xffffff02), rgba(0xfafafa4d))
}
/// `bg-muted/40` — the model picker's model/effort panes and footer strip.
pub fn muted_40() -> Rgba {
    pick(rgba(0xffffff03), rgba(0xfafafa66))
}
/// `--muted-foreground`. dark neutral-400 #a3a3a3 · light zinc-500 #71717a
pub fn fg_muted() -> Rgba {
    pick(rgb(0xa3a3a3), rgb(0x71717a))
}
/// `text-muted-foreground/70` — the web's 70%-muted body line (palette row
/// descriptions, thread-row timestamps).
pub fn fg_muted_soft() -> Rgba {
    pick(rgba(0xa3a3a3b3), rgba(0x71717ab3))
}
/// `text-muted-foreground/55` — the model picker's search-glass icon.
pub fn fg_muted_55() -> Rgba {
    pick(rgba(0xa3a3a38c), rgba(0x71717a8c))
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
/// `.chat-markdown-codeblock` card: `bg-secondary`, dark `bg-input/32`
/// (white·11% at 32% ≈ white·3.5%). dark white·3.5% · light zinc-50 #fafafa
pub fn code_block_bg() -> Rgba {
    pick(rgba(0xffffff09), rgb(0xfafafa))
}
/// `ChangedFilesCard`: `bg-secondary`, dark `bg-input/20` (white·11% at 20%
/// ≈ white·2.2%). dark white·2.2% · light zinc-50 #fafafa
pub fn changed_files_bg() -> Rgba {
    pick(rgba(0xffffff06), rgb(0xfafafa))
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

// ── Sidebar extras (index.css additions used by the sidebar rewrite) ──
/// `--sidebar-icon-color`: muted-foreground at 85% over the sidebar surface.
/// Used for header/search/footer chrome icons.
pub fn sidebar_icon() -> Rgba {
    pick(rgba(0xa3a3a3d9), rgba(0x71717ad9))
}
/// `text-sidebar-foreground/90` — project folder names.
pub fn sidebar_fg_soft() -> Rgba {
    pick(rgba(0xf1f3f7e6), rgba(0x18181be6))
}
/// `text-muted-foreground/65` — quiet row affordances like the pin icon.
pub fn sidebar_faint() -> Rgba {
    pick(rgba(0xa3a3a3a6), rgba(0x71717aa6))
}
/// `text-muted-foreground/60` — the empty-list hint color.
pub fn sidebar_dim() -> Rgba {
    pick(rgba(0xa3a3a399), rgba(0x71717a99))
}
/// `--info` — the "working" folder/row dot. blue-500 in both themes.
pub fn info() -> Rgba {
    rgb(0x3b82f6)
}
/// Draft-row surface `bg-warning/4` and its `hover:bg-warning/8`.
pub fn draft_surface() -> Rgba {
    rgba(0xf59e0b0a)
}
pub fn draft_surface_hover() -> Rgba {
    rgba(0xf59e0b14)
}
/// `--sidebar-border` at 60% — the draft-block divider (`bg-sidebar-border/60`).
pub fn sidebar_border_soft() -> Rgba {
    pick(rgba(0xffffff0c), rgba(0xe4e4e799))
}
/// `dropdown-glass` border — foreground at 10% (menu/popover outlines).
pub fn popover_border() -> Rgba {
    pick(rgba(0xffffff1a), rgba(0x18181b1a))
}

// ── Composer surfaces (apps/web ComposerSurface + dropdown-glass) ──
/// `--chat-composer-glass-surface` (`--surface-raised`) at `--glass-opacity`.
/// The web mixes `--background` at `glassOpacity` over transparent; the
/// baked baseline is the 80% default scaled by the live factor.
pub fn composer_surface() -> Rgba {
    let a = 0.8 * glass_factor();
    pick(
        scaled_alpha(rgba(0x111111ff), a),
        scaled_alpha(rgba(0xffffffff), a),
    )
}
/// `--chat-composer-outline` — the composer card's hairline.
/// dark white·5% · light black·8%
pub fn composer_outline() -> Rgba {
    pick(rgba(0xffffff0d), rgba(0x00000014))
}
/// `--chat-composer-highlight` — the card's top inset highlight (inset-shadow-2xs).
/// dark white·3% · light black·4%
pub fn composer_highlight() -> Rgba {
    pick(rgba(0xffffff08), rgba(0x0000000a))
}
/// `dropdown-glass` border: `color-mix(contrast-foreground 10%, transparent)`.
/// dark white·10% · light zinc-900·10%
pub fn popup_border() -> Rgba {
    pick(rgba(0xffffff1a), rgba(0x18181b1a))
}
/// ComposerControl label ink: `text-foreground/80`.
/// dark neutral-100·80% · light zinc-900·80%
pub fn control_fg() -> Rgba {
    pick(rgba(0xf5f5f5cc), rgba(0x18181bcc))
}
/// `--message-action-hover`: `color-mix(primary 90%, background)`.
/// dark #346bf1·90% over #000 ≈ #2f61d9 · light #1b4ed8·90% over #fcfcfc ≈ #325fdb
pub fn primary_hover() -> Rgba {
    pick(rgb(0x2f61d9), rgb(0x325fdb))
}
/// Disabled send when the blue circle treatment is kept — the dev
/// stage-backdrop button art at `disabled:opacity-64` (~primary·45% over the
/// card, matching the reference's #1c3c76 reading).
pub fn primary_dim() -> Rgba {
    pick(rgba(0x346bf180), rgba(0x1b4ed880))
}
/// The disabled send's white glyph at ~60% (`disabled:opacity-64` over the
/// primary-foreground white).
pub fn primary_fg_dim() -> Rgba {
    rgba(0xffffff99)
}
/// Disabled send/stop fill: `bg-foreground/12`.
/// dark neutral-100·12% · light zinc-900·12%
#[allow(dead_code)] // Kept for the non-backdrop disabled control look.
pub fn disabled_fill() -> Rgba {
    pick(rgba(0xf5f5f51f), rgba(0x18181b1f))
}
/// `--inset-shadow-control-highlight` on send/stop: `0 1px white·16%`.
pub fn control_highlight() -> Rgba {
    rgba(0xffffff29)
}
/// `bg-destructive/90` — the stop button's fill.
/// dark #f15757·90% · light #ef4444·90%
pub fn destructive_90() -> Rgba {
    pick(rgba(0xf15757e6), rgba(0xef4444e6))
}
/// `media-close` chip on staged images: `bg-black/65` + white glyph.
pub fn media_close() -> Rgba {
    rgba(0x000000a6)
}
/// `--context-chip-terminal` accent: oklch(0.62 0.134 163) ≈ emerald.
pub fn ctx_terminal() -> Rgba {
    pick(rgb(0x2fbf8f), rgb(0x0f9d70))
}

// ── Overlay / control tokens ────────────────────────────────────────
/// `.dialog-backdrop` — `--background` at 64% (dark) / 60% (light).
pub fn overlay_backdrop() -> Rgba {
    pick(rgba(0x000000a3), rgba(0xfcfcfc99))
}
/// `bg-foreground/[0.09]` — cmdk highlighted item.
pub fn command_highlight() -> Rgba {
    pick(rgba(0xf5f5f517), rgba(0x18181b17))
}
/// `bg-accent/60` — hover under an accent-highlighted row.
pub fn accent_soft() -> Rgba {
    pick(rgba(0xffffff06), rgba(0x00000004))
}
/// `bg-foreground/[0.08]` — Kbd chip fill.
pub fn kbd_bg() -> Rgba {
    pick(rgba(0xf5f5f514), rgba(0x18181b14))
}
/// `bg-foreground/[0.025]` — cmdk footer strip.
pub fn command_footer_bg() -> Rgba {
    pick(rgba(0xf5f5f504), rgba(0x18181b03))
}
/// `border-foreground/[0.07]` — cmdk footer separator.
pub fn command_footer_border() -> Rgba {
    pick(rgba(0xf5f5f512), rgba(0x18181b12))
}
/// `--secondary-label` outside the sidebar (palette shortcut hints).
pub fn secondary_label() -> Rgba {
    sidebar_secondary_label()
}
/// Text-input surface — dark `bg-input/32`, light `bg-background`.
pub fn field_surface() -> Rgba {
    pick(rgba(0xffffff09), rgb(0xffffff))
}
/// Unchecked-switch thumb — dark `bg-muted-foreground`, light `bg-background`.
pub fn switch_thumb_off() -> Rgba {
    pick(rgb(0xa3a3a3), rgb(0xffffff))
}
/// Checked-switch thumb — `bg-background`.
pub fn switch_thumb_on() -> Rgba {
    canvas()
}
/// Command/search magnifier icon — `text-icon-muted`.
pub fn icon_muted() -> Rgba {
    pick(rgb(0x8f8f8f), rgb(0x71717a))
}

// ── Diff surface (pierre diffs, index.css DIFF_SURFACE_THEME) ──────
// Line/gutter fills below are the resolved `color-mix()` results of
// `--diff-addition` (`--success`) / `--diff-deletion` (`--destructive`)
// blended over `--code-background`, exactly as the web's diff surface CSS
// computes them for the default dark/light themes.
/// `--diff-addition-foreground` — added-line text/emphasis.
/// dark emerald-400 · light emerald-700 (same as `success_fg()`).
pub fn diff_add_fg() -> Rgba {
    success_fg()
}
/// `--diff-deletion-foreground` — removed-line text/emphasis.
/// dark red-400 · light red-700 (same as `error_fg()`).
pub fn diff_del_fg() -> Rgba {
    error_fg()
}
/// `text-diff-addition` (`--diff-addition` = `--success`) — "+N" counters.
pub fn diff_add_count() -> Rgba {
    success()
}
/// `text-diff-deletion` (`--diff-deletion` = `--destructive`) — "−N" counters.
pub fn diff_del_count() -> Rgba {
    error()
}
/// `--diffs-bg-addition` — added-line row fill.
/// dark code-bg·70% + emerald-500·30% ≈ #0f4231 · light 50/50 ≈ #85dabe
pub fn diff_add_bg() -> Rgba {
    pick(rgb(0x0f4231), rgb(0x85dabe))
}
/// `--diffs-bg-addition-number` — gutter fill on added lines.
/// dark code-bg·60% + emerald-500·40% ≈ #0f533c · light code-bg·35% + emerald-500·65% ≈ #62d0ab
pub fn diff_add_num_bg() -> Rgba {
    pick(rgb(0x0f533c), rgb(0x62d0ab))
}
/// `--diffs-bg-deletion` — removed-line row fill.
/// dark code-bg·70% + #f15757·30% ≈ #522424 · light 50/50 w/ #ef4444 ≈ #f59f9f
pub fn diff_del_bg() -> Rgba {
    pick(rgb(0x522424), rgb(0xf59f9f))
}
/// `--diffs-bg-deletion-number` — gutter fill on removed lines.
/// dark code-bg·60% + #f15757·40% ≈ #692b2b · light code-bg·35% + #ef4444·65% ≈ #f38484
pub fn diff_del_num_bg() -> Rgba {
    pick(rgb(0x692b2b), rgb(0xf38484))
}
/// `--diffs-bg-context` — unchanged-line fill inside a diff surface.
/// dark code-bg·97% + fg·3% ≈ #151515 · light code-bg·97% + fg·3% ≈ #f4f4f4
pub fn diff_context_bg() -> Rgba {
    pick(rgb(0x151515), rgb(0xf4f4f4))
}
/// `--diffs-bg-separator` — hunk-header (`@@`) row fill.
/// dark code-bg·95% + fg·5% ≈ #1a1a1a · light ≈ #efefef
pub fn diff_hunk_bg() -> Rgba {
    pick(rgb(0x1a1a1a), rgb(0xefefef))
}
/// `--diffs-fg-number` — line-number gutter text (fg·65% over bg). Matches
/// the muted-fg tone, so this delegates to `fg_muted()`.
pub fn diff_gutter_fg() -> Rgba {
    fg_muted()
}

// ── File tree (pierre trees, pierre-tree-theme.ts overrides) ────────
/// `--trees-hover-bg-override` = currentColor·7% → fg·7%.
pub fn tree_hover() -> Rgba {
    pick(rgba(0xf5f5f512), rgba(0x18181b12))
}
/// `--trees-selected-bg-override` = currentColor·12% → fg·12%.
pub fn tree_selected() -> Rgba {
    pick(rgba(0xf5f5f51f), rgba(0x18181b1f))
}

// ── PR state tones (pullRequestPresentation.tsx) ────────────────────
/// Open PR: `text-emerald-600 dark:text-emerald-300/90`.
pub fn pr_open() -> Rgba {
    pick(rgba(0x6ee7b7e6), rgb(0x059669))
}
/// Merged PR: `text-violet-600 dark:text-violet-300/90`.
pub fn pr_merged() -> Rgba {
    pick(rgba(0xc4b5fde6), rgb(0x7c3aed))
}
/// Closed PR: `text-red-600 dark:text-red-300/90`.
pub fn pr_closed() -> Rgba {
    pick(rgba(0xfca5a5e6), rgb(0xdc2626))
}
/// Draft PR: `text-zinc-500 dark:text-zinc-400/80`.
pub fn pr_draft() -> Rgba {
    pick(rgba(0xa1a1aacd), rgb(0x71717a))
}
/// `border-border/80` — the web terminal drawer's top hairline.
/// dark white·6.4% ≈ rgba(0xffffff10) · light zinc-200·80%
pub fn border_soft() -> Rgba {
    contrast_border(rgba(0xffffff10), rgba(0xe4e4e7cc))
}

// ── Timeline / chat-markdown extras ───────────────────────────────
/// `--font-size-code` default (contracts `DEFAULT_CODE_FONT_SIZE` = 13).
pub fn code_text_size() -> gpui::Pixels {
    px(13.)
}
/// `ChatMarkdown` body ink: `text-foreground/[90%]` (contrast boost 0).
/// dark neutral-100·90% · light zinc-900·90%
pub fn chat_fg() -> Rgba {
    pick(rgba(0xf5f5f5e6), rgba(0x18181be6))
}
/// Inline-code chip fill: `color-mix(contrast-foreground 10%, transparent)`.
/// dark neutral-100·10% · light zinc-900·10%
pub fn inline_code_bg() -> Rgba {
    pick(rgba(0xf5f5f51a), rgba(0x18181b1a))
}
/// Blockquote bar: `color-mix(currentcolor 35%, transparent)`.
/// dark neutral-100·35% · light zinc-900·35%
pub fn quote_border() -> Rgba {
    pick(rgba(0xf5f5f559), rgba(0x18181b59))
}
/// `.chat-markdown-codeblock-header` label: `--code-foreground` at 72%.
/// dark neutral-100·72% · light zinc-900·72%
pub fn code_header_fg() -> Rgba {
    pick(rgba(0xf5f5f5b8), rgba(0x18181bb8))
}
/// `.chat-markdown pre` body ink — the syntax theme's plain foreground.
/// pierre-dark #fafafa · pierre-light #0a0a0a (measured in the web refs).
pub fn code_fg() -> Rgba {
    pick(rgb(0xfafafa), rgb(0x0a0a0a))
}
/// `text-tool-error-icon/40` — failed tool-call icon / ✕ mark.
/// dark red-300 #fca5a5·40% · light error #ef4444·40%
pub fn tool_error_icon() -> Rgba {
    pick(rgba(0xfca5a566), rgba(0xef444466))
}
/// `border-border/70` — hairlines on working rows and meta pills.
/// dark white·~5.5% · light zinc-200·70%
pub fn hairline() -> Rgba {
    contrast_border(rgba(0xffffff0e), rgba(0xe4e4e7b3))
}
/// `.chat-markdown hr` — dark `--contrast-muted-foreground`, light `--border`.
/// dark neutral-400 #a3a3a3 · light zinc-200 #e4e4e7
pub fn rule_color() -> Rgba {
    contrast_border(rgb(0xa3a3a3), rgb(0xe4e4e7))
}

// ── Sidebar/menu extras (visual-polish icon pass) ─────────────────
/// `hover:bg-destructive/10` — hover tint under a destructive menu row
/// (contextMenuFallback / MenuItem destructive variant).
/// dark error·10% · light error·10%
pub fn destructive_tint() -> Rgba {
    pick(rgba(0xf157571a), rgba(0xef44441a))
}

// ── Icon / option marks & chip accents ────────────────────────────
/// `--context-chip-citation`: oklch(0.62 0.16 259) solved to min contrast 5
/// over the default themes' surfaces (themePalette `contextChipVariables`).
/// dark #4f8def · light #215fbc
pub fn citation_accent() -> Rgba {
    pick(rgb(0x4f8def), rgb(0x215fbc))
}
/// `border-foreground/55` — pending-option radio/checkbox rings and
/// task-list checkbox outlines.
/// dark neutral-100·55% · light zinc-900·55%
pub fn option_mark_border() -> Rgba {
    pick(rgba(0xf5f5f58c), rgba(0x18181b8c))
}
/// `text-foreground/70` — unselected pending-option labels.
/// dark neutral-100·70% · light zinc-900·70%
pub fn fg_dim() -> Rgba {
    pick(rgba(0xf5f5f5b3), rgba(0x18181bb3))
}
/// `color-mix(--contrast-border 60%, transparent)` — `.chat-markdown`
/// table row separators (border-bottom on th/td).
/// dark border(white·8%)·60% ≈ white·4.8% · light zinc-200·60%
pub fn table_rule() -> Rgba {
    pick(rgba(0xffffff0c), rgba(0xe4e4e799))
}
/// `bg-destructive/30` — the failed divider rules on error/lifecycle rows.
/// dark #f15757·30% · light #ef4444·30%
pub fn error_rule() -> Rgba {
    pick(rgba(0xf157574d), rgba(0xef44444d))
}
/// `border-warning/32`-ish ring for pending-attention chips.
/// dark #f59e0b·33% · light #f59e0b·33%
pub fn warning_border() -> Rgba {
    pick(rgba(0xf59e0b55), rgba(0xf59e0b55))
}

// ── Explorer / toast polish (P4 icon + state pass) ───────────────
/// pierre FileTree `--trees-icon-gray`: the default file-glyph stroke.
/// dark #adadb1 · light #84848a
pub fn tree_icon_fg() -> Rgba {
    pick(rgb(0xadadb1), rgb(0x84848a))
}
/// pierre FileTree `--trees-fg-muted`: carets / "…" meta affordances.
/// #84848a in both themes.
pub fn tree_chevron_fg() -> Rgba {
    rgb(0x84848a)
}
/// Ignored-file name ink — `--trees-fg` at ~40%.
/// dark #4a4a4e · light #adadb1
pub fn tree_ignored() -> Rgba {
    pick(rgb(0x4a4a4e), rgb(0xadadb1))
}
/// Toast dismiss orb `bg-popover/92` (apps/web toast.tsx).
/// dark #111·92% · light #fff·92% — scaled by `glassOpacity` like the
/// web's `dropdown-glass`/`popover` surfaces.
pub fn popover_92() -> Rgba {
    let a = 0.92 * glass_factor();
    pick(
        scaled_alpha(rgba(0x111111ff), a),
        scaled_alpha(rgba(0xffffffff), a),
    )
}
/// Toast orb hairline `border-border/60`.
/// dark white·6% · light #e4e4e7·60%
pub fn toast_orb_border() -> Rgba {
    pick(rgba(0xffffff0f), rgba(0xe4e4e799))
}
