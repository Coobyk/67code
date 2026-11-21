//! Tooltip primitive — web `Tooltip`/`TooltipPopup` parity for icon buttons.
//!
//! GPUI's built-in `StatefulInteractiveElement::tooltip` shows a view at the
//! cursor after a 500ms hover delay (`DEFAULT_TOOLTIP_SHOW_DELAY` — the same
//! delay base-ui's tooltip provider uses on the web), clamped into the window.
//! Attach it to any stateful element: `.tooltip(tooltip("New thread"))`.

use gpui::{AnyView, App, Context, IntoElement, Render, SharedString, Window, div, prelude::*, px};

use crate::theme;

/// Plain-text tooltip card — the web `TooltipPopup` default variant:
/// `border bg-popover text-popover-foreground text-xs rounded-md
/// shadow-md/5`, `px-2 py-1`, `max-w-80`, `leading-snug`.
struct TextTooltip {
    text: SharedString,
}

impl Render for TextTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(320.))
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .shadow_md()
            .text_xs()
            .text_color(theme::fg())
            .child(self.text.clone())
    }
}

/// `StatefulInteractiveElement::tooltip` builder for a plain-text tooltip:
/// `.tooltip(tooltip("Settings"))` on a `div().id(...)` element.
pub fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let text = text.into();
    move |_, cx| cx.new(|_| TextTooltip { text: text.clone() }).into()
}
