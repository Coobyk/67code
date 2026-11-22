//! Appearance settings — the web `AppearanceSettingsPanel` +
//! `ThemeLibrary` (ThemeSettings.tsx): the "Color scheme" mode tiles
//! carrying live `ThemeWireframe` previews, the "Themes" card grid
//! with `ThemePreviewCircles` swatches, and the "Interface" sliders
//! (contrast / glass opacity) persisted on `ClientConfig` and applied
//! through `theme::set_tuning`. Mode switching goes through
//! `theme::set_mode` and repaints every window via
//! `cx.refresh_windows()`.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Bounds, Context, IntoElement, MouseButton, Pixels, Window, WindowAppearance, canvas, div,
    prelude::*, px, relative, rgb, rgba,
};

use crate::config::ThemeMode;
use crate::icons::{Icon, icon};
use crate::{AppView, theme};

// ── Contract ranges (packages/contracts/src/settings.ts) ──────────
const CONTRAST_MIN: i32 = 50;
const CONTRAST_MAX: i32 = 200;
const GLASS_MIN: i32 = 40;
const GLASS_MAX: i32 = 100;
const SLIDER_STEP: i32 = 5;
/// Pixel width of a slider track (the control is `w-52` ≈ 208px: the
/// 48px value chip + 12px gap + the track's flex-1 share).
const TRACK_W: f32 = 144.;
/// Discs used to fake each radial-gradient blob (GPUI paints only
/// linear gradients; concentric alpha layers approximate the CSS
/// `radial-gradient(circle at …)` swatch closely at 56px).
const BLOB_DISCS: usize = 14;
/// Preview circle diameter — the web `size-14`.
const SWATCH_D: f32 = 56.;

/// Preview palette for one appearance half — the web
/// `ThemeCardPreviewColors` roles (ThemePreviewCircles.tsx). Values are
/// the `themePalettes.ts` oklch roles resolved to sRGB hex.
#[derive(Clone, Copy)]
struct Swatch {
    sidebar: u32,
    canvas: u32,
    surface: u32,
    accent_surface: u32,
    accent: u32,
    message_surface: u32,
    message_action: u32,
}

/// One Themes-grid card (`ThemeCardDefinition`): label + a swatch per
/// supported appearance. A `None` half renders as a single-circle card
/// (the web's single-appearance environment themes, e.g. Omarchy).
struct ThemeCard {
    id: &'static str,
    label: &'static str,
    light: Option<Swatch>,
    dark: Option<Swatch>,
}

// Standard card — `STANDARD_THEME_PREVIEW_COLORS` (ThemePreviewCircles
// `.tsx` overrides over `packages/shared/themePreview.ts` shared roles).
const T3_CODE_LIGHT: Swatch = Swatch {
    sidebar: 0xfafafa,
    canvas: 0xfcfcfc,
    surface: 0xffffff,
    accent_surface: 0xf4f4f5,
    accent: 0xf4f4f5,
    message_surface: 0xe4e4e7,
    message_action: 0x4f46e5,
};
const T3_CODE_DARK: Swatch = Swatch {
    sidebar: 0x0f0f10,
    canvas: 0x0a0a0a,
    surface: 0x121212,
    accent_surface: 0x27272a,
    accent: 0x1c1c1f,
    message_surface: 0x27272a,
    message_action: 0x8b9cff,
};

// Maintainer themes — `themePalettes.ts` roles per appearance.
const T3_CHAT_LIGHT: Swatch = Swatch {
    sidebar: 0xf2e1f4,
    canvas: 0xfdf7fd,
    surface: 0xfaf3fb,
    accent_surface: 0xf3e6f5,
    accent: 0xdb2777,
    message_surface: 0xf7def2,
    message_action: 0xdb2777,
};
const T3_CHAT_DARK: Swatch = Swatch {
    sidebar: 0x171018,
    canvas: 0x1f1a24,
    surface: 0x29232d,
    accent_surface: 0x463753,
    accent: 0xa3004c,
    message_surface: 0x2b2431,
    message_action: 0xa3004c,
};
const GROVE_LIGHT: Swatch = Swatch {
    sidebar: 0xe2ede7,
    canvas: 0xf3f7f4,
    surface: 0xf3f7f4,
    accent_surface: 0xd5e6dd,
    accent: 0x1b7d50,
    message_surface: 0xcce1d7,
    message_action: 0x8f6410,
};
const GROVE_DARK: Swatch = Swatch {
    sidebar: 0x21362b,
    canvas: 0x1b2821,
    surface: 0x1b2821,
    accent_surface: 0x325c46,
    accent: 0x69d69a,
    message_surface: 0x37664d,
    message_action: 0xe3b34e,
};
const OCEAN_LIGHT: Swatch = Swatch {
    sidebar: 0xe4ecf2,
    canvas: 0xf5f7f8,
    surface: 0xf5f7f8,
    accent_surface: 0xd8e4ee,
    accent: 0x2672af,
    message_surface: 0xd0dfeb,
    message_action: 0x0a6f75,
};
const OCEAN_DARK: Swatch = Swatch {
    sidebar: 0x1e2d3b,
    canvas: 0x17212b,
    surface: 0x17212b,
    accent_surface: 0x324e66,
    accent: 0x70b9ee,
    message_surface: 0x375871,
    message_action: 0x5bd0d6,
};
const EMBER_LIGHT: Swatch = Swatch {
    sidebar: 0xf3eae5,
    canvas: 0xf9f7f5,
    surface: 0xf9f7f5,
    accent_surface: 0xeee0d9,
    accent: 0xae552a,
    message_surface: 0xebdad1,
    message_action: 0xb23535,
};
const EMBER_DARK: Swatch = Swatch {
    sidebar: 0x39281f,
    canvas: 0x291e1a,
    surface: 0x291e1a,
    accent_surface: 0x644330,
    accent: 0xf09a64,
    message_surface: 0x704b34,
    message_action: 0xf78a7a,
};
const IRIS_LIGHT: Swatch = Swatch {
    sidebar: 0xedeaf4,
    canvas: 0xf8f7f9,
    surface: 0xf8f7f9,
    accent_surface: 0xe5e0f0,
    accent: 0x7253b9,
    message_surface: 0xe0d9ee,
    message_action: 0xa82c87,
};
const IRIS_DARK: Swatch = Swatch {
    sidebar: 0x272139,
    canvas: 0x1d1929,
    surface: 0x1d1929,
    accent_surface: 0x433765,
    accent: 0x9d7df2,
    message_surface: 0x4b3d72,
    message_action: 0xf099d8,
};
// Environment theme carried by the reference environment — a
// dark-only card (Tokyo-Night-adjacent blues).
const OMARCHY_DARK: Swatch = Swatch {
    sidebar: 0x1a1c26,
    canvas: 0x20222e,
    surface: 0x262935,
    accent_surface: 0x2f3345,
    accent: 0x6f7fbf,
    message_surface: 0x2f3345,
    message_action: 0xaab2e0,
};

/// Grid order matches the web `ThemeLibrary` (standard card, then
/// `MAINTAINER_THEMES`, then environment themes).
const THEME_CARDS: &[ThemeCard] = &[
    ThemeCard {
        id: "",
        label: "T3 Code",
        light: Some(T3_CODE_LIGHT),
        dark: Some(T3_CODE_DARK),
    },
    ThemeCard {
        id: "t3-chat",
        label: "T3 Chat",
        light: Some(T3_CHAT_LIGHT),
        dark: Some(T3_CHAT_DARK),
    },
    ThemeCard {
        id: "grove",
        label: "Grove",
        light: Some(GROVE_LIGHT),
        dark: Some(GROVE_DARK),
    },
    ThemeCard {
        id: "ocean",
        label: "Ocean",
        light: Some(OCEAN_LIGHT),
        dark: Some(OCEAN_DARK),
    },
    ThemeCard {
        id: "ember",
        label: "Ember",
        light: Some(EMBER_LIGHT),
        dark: Some(EMBER_DARK),
    },
    ThemeCard {
        id: "iris",
        label: "Iris",
        light: Some(IRIS_LIGHT),
        dark: Some(IRIS_DARK),
    },
    ThemeCard {
        id: "omarchy",
        label: "Omarchy",
        light: None,
        dark: Some(OMARCHY_DARK),
    },
];

/// `THEME_PREVIEW_RENDER_SPECS` — the radial blobs + base mix each
/// swatch circle renders (packages/shared/src/themePreview.ts).
struct BlobSpec {
    center: [f32; 2],
    /// Radial offset where the gradient reaches transparent (fraction
    /// of the circle's diameter).
    end: f32,
    /// (offset, alpha) stops — CSS interpolates linearly between them.
    stops: &'static [(f32, f32)],
}
struct ModeSpec {
    base_target: u32,
    base_weight: f32,
    accent: BlobSpec,
    action: BlobSpec,
}
const LIGHT_SPEC: ModeSpec = ModeSpec {
    base_target: 0xffffff,
    base_weight: 0.8,
    accent: BlobSpec {
        center: [0.72, 0.22],
        end: 0.58,
        stops: &[(0., 1.), (0.28, 0.72), (0.58, 0.)],
    },
    action: BlobSpec {
        center: [0.18, 0.82],
        end: 0.55,
        stops: &[(0., 0.45), (0.55, 0.)],
    },
};
const DARK_SPEC: ModeSpec = ModeSpec {
    base_target: 0x09090b,
    base_weight: 0.8,
    accent: BlobSpec {
        center: [0.28, 0.78],
        end: 0.58,
        stops: &[(0., 1.), (0.28, 0.62), (0.58, 0.)],
    },
    action: BlobSpec {
        center: [0.82, 0.18],
        end: 0.55,
        stops: &[(0., 0.45), (0.55, 0.)],
    },
};

/// Which interface slider a drag is scrubbing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SliderKind {
    Contrast,
    Glass,
}

impl SliderKind {
    fn range(self) -> (i32, i32) {
        match self {
            SliderKind::Contrast => (CONTRAST_MIN, CONTRAST_MAX),
            SliderKind::Glass => (GLASS_MIN, GLASS_MAX),
        }
    }
}

#[derive(Default)]
pub struct State {
    /// Set once the OS appearance-change observer has been armed for the
    /// window. Render has no `&mut Window`, so the subscription is armed on
    /// the first interaction inside this section instead.
    appearance_observed: bool,
    /// Window-space bounds of each slider track, stashed by a `canvas`
    /// prepaint hook so click/drag positions map to values.
    contrast_bounds: Rc<RefCell<Option<Bounds<Pixels>>>>,
    glass_bounds: Rc<RefCell<Option<Bounds<Pixels>>>>,
    /// Slider being scrubbed — set on track mousedown, cleared on
    /// mouseup (or when the button is no longer pressed mid-move).
    dragging: Option<SliderKind>,
}

/// Dark when the OS reports a dark window appearance.
fn system_dark(window: &Window) -> bool {
    matches!(
        window.appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

// ── oklab mixing (themePreview.ts `mixThemePreviewBase`) ──────────
fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1. / 2.4) - 0.055
    }
}
fn hex_to_oklab(hex: u32) -> [f32; 3] {
    let r = srgb_to_linear(((hex >> 16) & 0xff) as f32 / 255.);
    let g = srgb_to_linear(((hex >> 8) & 0xff) as f32 / 255.);
    let b = srgb_to_linear((hex & 0xff) as f32 / 255.);
    let lr = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let mr = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let sr = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * lr + 0.793617785 * mr - 0.0040720468 * sr,
        1.9779984951 * lr - 2.428592205 * mr + 0.4505937099 * sr,
        0.0259040371 * lr + 0.7827717662 * mr - 0.808675766 * sr,
    ]
}
fn oklab_to_hex([l, a, b]: [f32; 3]) -> gpui::Rgba {
    let lp = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let mp = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let sp = (l - 0.0894841775 * a - 1.291485548 * b).powi(3);
    gpui::Rgba {
        r: linear_to_srgb(4.0767416621 * lp - 3.3077115913 * mp + 0.2309699292 * sp).clamp(0., 1.),
        g: linear_to_srgb(-1.2684380046 * lp + 2.6097574011 * mp - 0.3413193965 * sp).clamp(0., 1.),
        b: linear_to_srgb(-0.0041960863 * lp - 0.7034186147 * mp + 1.707614701 * sp).clamp(0., 1.),
        a: 1.,
    }
}
/// `color-mix(in oklab, canvas baseWeight, baseTarget)` — the swatch's
/// flat base color before the blobs land.
fn mix_oklab(a: u32, aw: f32, b: u32) -> gpui::Rgba {
    let [al, aa, ab] = hex_to_oklab(a);
    let [bl, ba, bb] = hex_to_oklab(b);
    let bw = 1. - aw;
    oklab_to_hex([al * aw + bl * bw, aa * aw + ba * bw, ab * aw + bb * bw])
}

/// Interpolated blob alpha at radial offset `r` (fraction of the
/// circle's diameter), following the spec's (offset, alpha) stops.
fn blob_alpha(stops: &[(f32, f32)], r: f32) -> f32 {
    if r <= stops[0].0 {
        return stops[0].1;
    }
    for w in stops.windows(2) {
        if r <= w[1].0 {
            let t = (r - w[0].0) / (w[1].0 - w[0].0);
            return w[0].1 + t * (w[1].1 - w[0].1);
        }
    }
    0.
}

/// Concentric discs approximating one `radial-gradient(circle at …)`
/// blob — outermost first, each layer contributing the source-over
/// alpha needed to hit the gradient's target at its edge.
fn blob_discs(spec: &BlobSpec, hex: u32, d: f32) -> Vec<gpui::Div> {
    let mut discs = Vec::with_capacity(BLOB_DISCS);
    let mut cumulative = 0f32;
    for i in (0..BLOB_DISCS).rev() {
        let r = (spec.end * (i + 1) as f32 / BLOB_DISCS as f32).min(0.5);
        let target = blob_alpha(spec.stops, r).min(1.);
        let layer = if cumulative >= 1. {
            0.
        } else {
            ((target - cumulative) / (1. - cumulative)).clamp(0., 1.)
        };
        cumulative = target;
        let size = 2. * r * d;
        // GPUI content masks are rectangular, so the discs can't be
        // clipped to the swatch circle the way CSS border-radius clips
        // the radial gradient. Slide each disc toward the center until
        // it fits inside — its edge lands where the clip would cut it.
        let off = ((spec.center[0] - 0.5).powi(2) + (spec.center[1] - 0.5).powi(2)).sqrt();
        let fit = (0.5 - r).max(0.);
        let scale = if off > fit { fit / off } else { 1. };
        let cx = 0.5 + (spec.center[0] - 0.5) * scale;
        let cy = 0.5 + (spec.center[1] - 0.5) * scale;
        let mut color = rgb(hex);
        color.a = layer;
        discs.push(
            div()
                .absolute()
                .left(px(cx * d - size / 2.))
                .top(px(cy * d - size / 2.))
                .size(px(size))
                .rounded_full()
                .bg(color),
        );
    }
    discs
}

/// The card's swatch for `dark` — `previewColorsOf` with the web's
/// `previews[0]` fallback for single-appearance cards.
fn swatch_for(card: &ThemeCard, dark: bool) -> Swatch {
    let pick = if dark { card.dark } else { card.light };
    pick.or(card.light).or(card.dark).unwrap_or(T3_CODE_DARK)
}

/// `ThemeWireframePane` — the mini-UI thumbnail inside each mode tile.
/// All positions are the web's percentage literals via `relative()`.
fn wireframe_pane(colors: &Swatch) -> gpui::Div {
    let line = rgba(0x7f7f7f40); // rgb(127 127 127 / 0.25)
    let mut pane = div().absolute().inset_0().bg(rgb(colors.canvas)).child(
        // Sidebar strip + inset right hairline (the web's
        // `box-shadow: inset -1px 0 0`).
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .w(relative(0.22))
            .bg(rgb(colors.sidebar))
            .border_r_1()
            .border_color(line),
    );
    // Sidebar rows: search pill + three list rows.
    for (top, h, fill, fade) in [
        (0.08, 0.08, colors.surface, 1f32),
        (0.22, 0.07, colors.accent_surface, 1.),
        (0.32, 0.07, colors.message_surface, 0.7),
        (0.42, 0.07, colors.message_surface, 0.5),
    ] {
        let mut row = div()
            .absolute()
            .left(relative(0.03))
            .top(relative(top))
            .h(relative(h))
            .w(relative(0.16))
            .rounded_md()
            .bg(rgb(fill));
        if fade < 1. {
            row = row.opacity(fade);
        }
        if top < 0.1 {
            row = row.border_1().border_color(line);
        }
        pane = pane.child(row);
    }
    pane
        // Assistant message bubble (top-right of the canvas).
        .child(
            div()
                .absolute()
                .right(relative(0.28))
                .top(relative(0.11))
                .h(relative(0.09))
                .w(relative(0.24))
                .rounded_lg()
                .bg(rgb(colors.message_surface)),
        )
        .child(
            div()
                .absolute()
                .left(relative(0.27))
                .top(relative(0.28))
                .h(relative(0.05))
                .w(relative(0.34))
                .rounded_sm()
                .bg(line),
        )
        .child(
            div()
                .absolute()
                .left(relative(0.27))
                .top(relative(0.38))
                .h(relative(0.05))
                .w(relative(0.26))
                .rounded_sm()
                .bg(line),
        )
        // Composer bar: surface pill + send dot.
        .child(
            div()
                .absolute()
                .bottom(relative(0.08))
                .left(relative(0.26))
                .right(relative(0.06))
                .h(relative(0.15))
                .rounded_md()
                .bg(rgb(colors.surface))
                .border_1()
                .border_color(line)
                .flex()
                .items_center()
                .justify_between()
                .px_1()
                .child(
                    div()
                        .h(relative(0.26))
                        .w(relative(0.34))
                        .rounded_full()
                        .bg(line)
                        .opacity(0.7),
                )
                .child(
                    div()
                        .h(relative(0.58))
                        .aspect_square()
                        .rounded_full()
                        .bg(rgb(colors.message_action)),
                ),
        )
        // Right inspector card with three status rows.
        .child(
            div()
                .absolute()
                .right(relative(0.05))
                .top(relative(0.08))
                .h(relative(0.46))
                .w(relative(0.2))
                .rounded_lg()
                .bg(rgb(colors.surface))
                .border_1()
                .border_color(line)
                .shadow_sm()
                .children((0..3).map(|row| {
                    let dot = match row {
                        0 => rgb(0x34d399),
                        1 => rgb(colors.message_action),
                        _ => rgb(0xfbbf24),
                    };
                    div()
                        .absolute()
                        .left(relative(0.11))
                        .right(relative(0.11))
                        .top(relative(0.1 + row as f32 * 0.3))
                        .h(relative(0.2))
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .h(relative(0.26))
                                .aspect_square()
                                .rounded_full()
                                .bg(dot)
                                .opacity(0.55),
                        )
                        .child(
                            div()
                                .h(relative(0.3))
                                .w(relative(0.52))
                                .rounded_sm()
                                .bg(line),
                        )
                })),
        )
}

/// `ThemeWireframe` — `h-[8.75rem] rounded-lg border` + one pane (two
/// clipped halves for the System tile's light|dark split).
fn wireframe(mode: ThemeMode, card: &ThemeCard) -> gpui::Div {
    let frame = div()
        .relative()
        .w_full()
        .h(px(140.))
        .rounded_lg()
        .overflow_hidden()
        .border_1()
        .border_color(theme::card_border());
    match mode {
        ThemeMode::System => frame
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(relative(0.5))
                    .overflow_hidden()
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .w(relative(2.))
                            .child(wireframe_pane(&swatch_for(card, false))),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .w(relative(0.5))
                    .overflow_hidden()
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .w(relative(2.))
                            .child(wireframe_pane(&swatch_for(card, true))),
                    ),
            ),
        ThemeMode::Light => frame.child(wireframe_pane(&swatch_for(card, false))),
        ThemeMode::Dark => frame.child(wireframe_pane(&swatch_for(card, true))),
    }
}

/// `ThemePreviewCircle` in its `size-[68px]` slot: the 56px gradient
/// swatch + (when picked) the primary inset ring and the Sun/Moon
/// badge at bottom-right.
fn swatch_slot(colors: &Swatch, dark_mode: bool, picked: bool) -> gpui::Div {
    let spec = if dark_mode { &DARK_SPEC } else { &LIGHT_SPEC };
    div()
        .size(px(68.))
        .flex_none()
        .relative()
        .child(
            div()
                .absolute()
                .inset(px(6.))
                .rounded_full()
                .overflow_hidden()
                .bg(mix_oklab(colors.canvas, spec.base_weight, spec.base_target))
                .children(blob_discs(&spec.accent, colors.accent, SWATCH_D))
                .children(blob_discs(&spec.action, colors.message_action, SWATCH_D)),
        )
        // Edge: `border-2 border-background` + the light inset hairline
        // (the web's themePreviewEdgeShadow, drop part folded into the ring).
        .child(
            div()
                .absolute()
                .inset(px(6.))
                .rounded_full()
                .border_2()
                .border_color(theme::canvas()),
        )
        .child(
            div()
                .absolute()
                .inset(px(7.))
                .rounded_full()
                .border_1()
                .border_color(if dark_mode {
                    rgba(0xffffff24)
                } else {
                    rgba(0x0000001a)
                }),
        )
        .when(picked, |slot| {
            slot.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded_full()
                    .border_2()
                    .border_color(theme::primary()),
            )
            .child(
                div()
                    .absolute()
                    .bottom(px(2.))
                    .right(px(2.))
                    .size(px(20.))
                    .rounded_full()
                    .bg(theme::canvas())
                    .border_1()
                    .border_color(theme::border_70())
                    .shadow_sm()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        icon(if dark_mode { Icon::Moon } else { Icon::Sun }, 12.)
                            .text_color(theme::fg()),
                    ),
            )
        })
}

/// `h3` section heading inside the ThemeLibrary block (`min-h-7
/// items-center px-4 text-base font-semibold`).
fn section_heading(text: &'static str) -> gpui::Div {
    div()
        .min_h(px(28.))
        .flex()
        .items_center()
        .px_4()
        .text_base()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::fg())
        .child(text)
}

/// Web `Button size="xs" variant="outline"` — h-6, icon + label,
/// `border-input` outline on `bg-input/32` (dark).
fn ghost_button(
    id: impl Into<gpui::ElementId>,
    glyph: Icon,
    label: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(24.))
        .flex()
        .items_center()
        .gap_1()
        .px(px(7.))
        .rounded_lg()
        .border_1()
        .border_color(theme::input())
        .bg(theme::field_surface())
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::fg())
        .cursor_pointer()
        .hover(|s| s.bg(theme::accent_soft()))
        .child(icon(glyph, 14.).text_color(theme::fg_muted()))
        .child(label)
}

impl AppView {
    /// Subscribe to OS appearance changes so System mode follows the OS live.
    /// Idempotent — the flag guards the one subscription per window.
    fn ensure_appearance_observer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.appearance_state.appearance_observed {
            return;
        }
        self.appearance_state.appearance_observed = true;
        cx.observe_window_appearance(window, |this, window, cx| {
            theme::set_mode(this.client_config.theme_mode, system_dark(window));
            cx.refresh_windows();
        })
        .detach();
    }

    fn set_theme_mode(&mut self, mode: ThemeMode, window: &mut Window, cx: &mut Context<Self>) {
        theme::set_mode(mode, system_dark(window));
        if self.client_config.theme_mode != mode {
            self.client_config.theme_mode = mode;
            self.client_config.save();
        }
        cx.refresh_windows();
        cx.notify();
    }

    /// Persist the picked theme card — the web's `persistTheme` reduced
    /// to store-only (the native palette itself stays the T3 Code
    /// default; the previews retint immediately).
    fn select_theme_card(&mut self, card_id: &'static str, cx: &mut Context<Self>) {
        if self.client_config.theme_id != card_id {
            self.client_config.theme_id = card_id.to_string();
            self.client_config.save();
            cx.notify();
        }
    }

    /// Map a pointer x (window space) onto the slider's contract range,
    /// snapped to `SLIDER_STEP`, and store it. The stashed track bounds
    /// come from the prepaint `canvas` inside the track.
    fn apply_slider_position(&mut self, kind: SliderKind, x: Pixels, cx: &mut Context<Self>) {
        let cell = match kind {
            SliderKind::Contrast => &self.appearance_state.contrast_bounds,
            SliderKind::Glass => &self.appearance_state.glass_bounds,
        };
        let Some(bounds) = *cell.borrow() else { return };
        let (min, max) = kind.range();
        let frac = ((x - bounds.origin.x) / bounds.size.width).clamp(0., 1.);
        let value =
            min + ((frac * (max - min) as f32) / SLIDER_STEP as f32).round() as i32 * SLIDER_STEP;
        let value = value.clamp(min, max);
        let field = match kind {
            SliderKind::Contrast => &mut self.client_config.appearance_contrast,
            SliderKind::Glass => &mut self.client_config.glass_opacity,
        };
        if *field != value {
            *field = value;
            theme::set_tuning(
                self.client_config.appearance_contrast,
                self.client_config.glass_opacity,
            );
            self.client_config.save();
            cx.refresh_windows();
            cx.notify();
        }
    }

    /// The `settings-slider` control: value chip + 6px track (primary
    /// fill up to the thumb center) + 16px hollow thumb. Click or drag
    /// sets the value.
    fn slider_control(
        &self,
        kind: SliderKind,
        value: i32,
        bounds_cell: Rc<RefCell<Option<Bounds<Pixels>>>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (min, max) = kind.range();
        let frac = (value - min) as f32 / (max - min) as f32;
        let thumb_x = frac * (TRACK_W - 16.);
        let track_bg = if theme::effective_dark() {
            rgba(0xa3a3a338)
        } else {
            rgba(0x71717a38)
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .w(px(208.))
            // Value chip: `min-w-12 rounded-md bg-muted px-2 py-1
            // font-mono text-xs text-center`.
            .child(
                div()
                    .min_w(px(48.))
                    .rounded_md()
                    .bg(theme::muted())
                    .px_2()
                    .py_1()
                    .text_center()
                    .text_xs()
                    .font_family(theme::FONT_MONO)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::fg())
                    .child(format!("{value}%")),
            )
            .child(
                div()
                    .id((
                        "iface-slider",
                        match kind {
                            SliderKind::Contrast => 0usize,
                            SliderKind::Glass => 1,
                        },
                    ))
                    .flex_none()
                    .relative()
                    .w(px(TRACK_W))
                    .h(px(24.))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _window, cx| {
                            this.appearance_state.dragging = Some(kind);
                            this.apply_slider_position(kind, event.position.x, cx);
                        }),
                    )
                    .child(
                        canvas(
                            move |bounds, _window, _cx| {
                                *bounds_cell.borrow_mut() = Some(bounds);
                            },
                            |_bounds, _t, _window, _cx| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    // Track base: 6px pill, `muted-foreground 22%` fill +
                    // the web's 1px inset border ring.
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .top(px(9.))
                            .h(px(6.))
                            .rounded_full()
                            .bg(track_bg)
                            .border_1()
                            .border_color(theme::hairline()),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top(px(9.))
                            .h(px(6.))
                            .w(px(thumb_x + 8.))
                            .rounded_full()
                            .bg(theme::primary()),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(thumb_x))
                            .top(px(4.))
                            .size(px(16.))
                            .rounded_full()
                            .bg(theme::canvas())
                            .border_2()
                            .border_color(theme::primary())
                            .shadow_sm(),
                    ),
            )
    }

    /// One "Interface" row — `SettingsRow` (title + muted description
    /// left, control right).
    fn interface_row(
        &self,
        kind: SliderKind,
        title: &'static str,
        description: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let (value, cell) = match kind {
            SliderKind::Contrast => (
                self.client_config.appearance_contrast,
                self.appearance_state.contrast_bounds.clone(),
            ),
            SliderKind::Glass => (
                self.client_config.glass_opacity,
                self.appearance_state.glass_bounds.clone(),
            ),
        };
        crate::settings_misc::setting_row(
            title,
            description,
            self.slider_control(kind, value, cell, cx)
                .into_any_element(),
        )
    }

    /// One mode tile (`renderModeTiles`): `rounded-xl border p-2` +
    /// wireframe + centered label; the picked tile swaps its border for
    /// the primary ring and `bg-accent/30`.
    fn mode_tile(
        &self,
        mode: ThemeMode,
        label: &'static str,
        index: usize,
        card: &ThemeCard,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.client_config.theme_mode == mode;
        div()
            .id(("scheme-mode", index))
            .flex()
            .flex_col()
            .gap_1p5()
            .rounded_xl()
            .p_2()
            .cursor_pointer()
            .when(active, |s| {
                s.border_2()
                    .border_color(theme::primary())
                    .bg(theme::accent_soft())
            })
            .when(!active, |s| {
                s.border_1()
                    .border_color(theme::border_70())
                    .bg(theme::card_surface_60())
                    .hover(|s| s.bg(theme::accent_soft()))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event, window, cx| {
                    this.ensure_appearance_observer(window, cx);
                    this.set_theme_mode(mode, window, cx);
                }),
            )
            .child(wireframe(mode, card))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(if active {
                        theme::fg()
                    } else {
                        theme::fg_muted()
                    })
                    .child(label),
            )
    }

    /// One `ThemeLibraryCard` — circles row + label/duplicate row.
    fn theme_card(
        &self,
        card: &'static ThemeCard,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.client_config.theme_id == card.id;
        let card_id = card.id;
        let mut circles = div()
            .min_h(px(64.))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .px_3()
            .pt_3();
        if let Some(light) = card.light {
            circles = circles.child(
                div()
                    .id(("theme-swatch", index * 2))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            cx.stop_propagation();
                            this.select_theme_card(card_id, cx);
                        }),
                    )
                    .child(swatch_slot(&light, false, selected)),
            );
        }
        if let Some(dark) = card.dark {
            circles = circles.child(
                div()
                    .id(("theme-swatch", index * 2 + 1))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            cx.stop_propagation();
                            this.select_theme_card(card_id, cx);
                        }),
                    )
                    .child(swatch_slot(&dark, true, selected)),
            );
        }
        div()
            .id(("theme-card", index))
            .rounded_xl()
            .border_1()
            .border_color(theme::border_70())
            .bg(theme::card_surface_60())
            .cursor_pointer()
            .hover(|s| s.bg(theme::accent_soft()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, _w, cx| {
                    this.select_theme_card(card_id, cx);
                }),
            )
            .child(circles)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .pt_2()
                    .pb_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::fg())
                            .child(card.label),
                    )
                    // Web duplicate button (opens the theme editor).
                    // Native has no theme editor — the affordance is
                    // rendered but inert, swallowing its own click so
                    // it can't read as a theme selection.
                    .child(
                        div()
                            .id(("theme-card-dup", index))
                            .w(px(24.))
                            .h(px(24.))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_this, _e, _w, cx| cx.stop_propagation()),
                            )
                            .child(icon(Icon::Copy, 14.).text_color(theme::fg_muted())),
                    ),
            )
    }

    pub fn render_appearance_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = THEME_CARDS
            .iter()
            .find(|c| c.id == self.client_config.theme_id)
            .unwrap_or(&THEME_CARDS[0]);

        let mut tiles = div().grid().grid_cols(3).gap_3();
        for (index, (mode, label)) in [
            (ThemeMode::System, "System"),
            (ThemeMode::Light, "Light"),
            (ThemeMode::Dark, "Dark"),
        ]
        .iter()
        .enumerate()
        {
            tiles = tiles.child(self.mode_tile(*mode, label, index, selected, cx));
        }

        let mut theme_grid = div().grid().grid_cols(3).gap_2();
        for (index, card) in THEME_CARDS.iter().enumerate() {
            theme_grid = theme_grid.child(self.theme_card(card, index, cx));
        }

        div()
            .id("appearance-page")
            .flex()
            .flex_col()
            .gap_6()
            // Scrub state for the sliders — the listener lives on the
            // page root so moves bubble here from wherever the pointer is.
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, _window, cx| {
                    if let Some(kind) = this.appearance_state.dragging {
                        if event.dragging() {
                            this.apply_slider_position(kind, event.position.x, cx);
                        } else {
                            this.appearance_state.dragging = None;
                        }
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    if this.appearance_state.dragging.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .child(
                // `ThemeLibrary` — `space-y-3`.
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(section_heading("Color scheme"))
                    .child(tiles)
                    .child(
                        // `min-h-8 items-center justify-between px-4 pt-5`.
                        div()
                            .min_h(px(32.))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .px_4()
                            .pt_5()
                            .child(section_heading("Themes").px_0().min_h_0())
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .gap_2()
                                    .child(ghost_button(
                                        "create-theme",
                                        Icon::Paintbrush,
                                        "Create theme",
                                    ))
                                    .child(ghost_button("add-theme", Icon::Plus, "Add theme")),
                            ),
                    )
                    .child(theme_grid),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(crate::settings_misc::settings_section_title("Interface"))
                    .child(crate::settings_misc::settings_card(vec![
                        self.interface_row(
                            SliderKind::Contrast,
                            "Contrast",
                            "Adjust the contrast of colors and borders across the interface.",
                            cx,
                        ),
                        self.interface_row(
                            SliderKind::Glass,
                            "Glass opacity",
                            "Higher values make menus, dialogs, and the composer more solid.",
                            cx,
                        ),
                    ])),
            )
    }
}
