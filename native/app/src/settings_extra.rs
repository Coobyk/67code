//! Extra settings sections (wave-3): Keybindings, Source Control, Threads.
//! Stub renderers — replaced by the settings parity agent.

use gpui::{Context, Div, div, prelude::*};

use crate::AppView;
use crate::theme;

fn stub_section(label: &str) -> Div {
    div()
        .flex_1()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(format!("{label} — not implemented yet"))
}

impl AppView {
    pub(crate) fn render_keybindings_section(&self, _cx: &mut Context<Self>) -> Div {
        stub_section("Keybindings")
    }

    pub(crate) fn render_source_control_section(&self, _cx: &mut Context<Self>) -> Div {
        stub_section("Source Control")
    }

    pub(crate) fn render_threads_section(&self, _cx: &mut Context<Self>) -> Div {
        stub_section("Threads")
    }
}
