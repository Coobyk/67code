//! Settings surface (P1): modal panel toggled with cmd-, or via palette.

use gpui::{Context, IntoElement, Window, div};
use crate::{AppView, ToggleSettings};

#[derive(Default)]
pub struct State {
    pub open: bool,
}

impl AppView {
    pub fn on_toggle_settings(
        &mut self,
        _action: &ToggleSettings,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_state.open = !self.settings_state.open;
        cx.notify();
    }

    pub fn render_settings(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
