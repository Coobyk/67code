//! Appearance settings (P2): theme mode rows (Light / Dark / System)
//! mirroring the AppearanceSettingsPanel mode tiles in the web UI. Mode
//! switching goes through `theme::set_mode`, persists via
//! `config::ClientConfig` (`self.client_config`), and repaints every window
//! via `cx.refresh_windows()`. The web's accent picker is part of the custom
//! theme editor (canvas + accent generating a whole palette), not a preset
//! swatch row, so there is no accent section here.

use gpui::{Context, IntoElement, MouseButton, Window, WindowAppearance, div, prelude::*, px};

use crate::config::ThemeMode;
use crate::{AppView, theme};

/// (mode, label, description) rows; order matches the task spec and the web
/// panel groups the same three choices.
const THEME_MODES: &[(ThemeMode, &str, &str)] = &[
    (
        ThemeMode::Light,
        "Light",
        "Bright surfaces for daylight work.",
    ),
    (
        ThemeMode::Dark,
        "Dark",
        "Dim surfaces that are easy on the eyes.",
    ),
    (ThemeMode::System, "System", "Follow the OS appearance."),
];

#[derive(Default)]
pub struct State {
    /// Set once the OS appearance-change observer has been armed for the
    /// window. Render has no `&mut Window`, so the subscription is armed on
    /// the first interaction inside this section instead.
    appearance_observed: bool,
}

/// Dark when the OS reports a dark window appearance.
fn system_dark(window: &Window) -> bool {
    matches!(
        window.appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// Small caption above a group of rows (settings.rs keeps its own copy).
fn group_label(text: &'static str) -> gpui::Div {
    div()
        .pb_1()
        .pt_3()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(text)
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

    pub fn render_appearance_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.client_config.theme_mode;
        let note = match active {
            ThemeMode::System => {
                if theme::effective_dark() {
                    "Following the OS — currently dark."
                } else {
                    "Following the OS — currently light."
                }
            }
            ThemeMode::Dark => "Dark palette applied to all surfaces.",
            ThemeMode::Light => "Light palette applied to all surfaces.",
        };

        let mut list = div().flex().flex_col().child(group_label("Theme"));
        for (index, (mode, label, description)) in THEME_MODES.iter().enumerate() {
            let mode = *mode;
            let is_active = active == mode;
            list = list.child(
                div()
                    .id(("theme-mode", index))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .when(is_active, |s| s.bg(theme::selected()))
                    .when(!is_active, |s| s.hover(|s| s.bg(theme::hover())))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _event, window, cx| {
                            this.ensure_appearance_observer(window, cx);
                            this.set_theme_mode(mode, window, cx);
                        }),
                    )
                    .child(
                        div()
                            .w(px(14.))
                            .flex_none()
                            .text_color(theme::primary())
                            .child(if is_active { "✓" } else { "" }),
                    )
                    .child(div().flex_none().text_color(theme::fg()).child(*label))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(*description),
                    ),
            );
        }

        list.child(
            div()
                .pt_2()
                .text_xs()
                .text_color(theme::fg_muted())
                .child(note),
        )
    }
}
