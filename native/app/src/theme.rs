//! Palette mirroring apps/web/src/index.css — dark and light variants.
//! Every accessor resolves through `effective_dark()` so surfaces pick up a
//! mode change without call-site edits; `set_mode` is driven by the
//! Appearance settings section (persisted via config::ClientConfig).

use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{Rgba, rgb, rgba};

use crate::config::ThemeMode;

static MODE: RwLock<ThemeMode> = RwLock::new(ThemeMode::Dark);
static SYSTEM_DARK: AtomicBool = AtomicBool::new(true);

/// Update the active mode. `system_dark` is the OS appearance — only used
/// when mode is `System`. Call `cx.refresh_windows()` (or notify) after.
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

/// 67 Code palettes, mirroring apps/web/src/index.css dark/light vars.
pub fn canvas() -> Rgba {
    pick(rgb(0x000000), rgb(0xfafafa))
}
pub fn sidebar() -> Rgba {
    pick(rgb(0x0a0a0a), rgb(0xf4f4f5))
}
pub fn raised() -> Rgba {
    pick(rgb(0x141414), rgb(0xffffff))
}
pub fn border() -> Rgba {
    pick(rgb(0x262626), rgb(0xe4e4e7))
}
pub fn fg() -> Rgba {
    pick(rgb(0xf5f5f5), rgb(0x18181b))
}
pub fn fg_muted() -> Rgba {
    pick(rgb(0x737373), rgb(0x71717a))
}
/// Accent. Light mirrors `--primary: oklch(0.488 0.217 264)` ≈ indigo-600.
pub fn primary() -> Rgba {
    pick(rgb(0x5a7cff), rgb(0x4f46e5))
}
pub fn hover() -> Rgba {
    pick(rgba(0xffffff0a), rgba(0x00000008))
}
pub fn selected() -> Rgba {
    pick(rgba(0xffffff14), rgba(0x0000000f))
}
pub fn user_bubble() -> Rgba {
    pick(rgb(0x1c1c1e), rgb(0xe9e9ec))
}
pub fn cursor() -> Rgba {
    primary()
}
pub fn selection() -> Rgba {
    pick(rgba(0x5a7cff40), rgba(0x4f46e533))
}
