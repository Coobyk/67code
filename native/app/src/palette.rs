//! Command palette + global keyboard shortcuts (P1).
//!
//! Overlay opened with cmd-k (ctrl-k on non-mac keymaps is handled the same
//! way by gpui). Lists navigation targets and commands; fuzzy filter as the
//! user types; Enter executes; Escape closes (handled via CloseOverlay in
//! lib.rs).

use gpui::{Context, IntoElement, Window, div};
use crate::{AppView, TogglePalette};

#[derive(Default)]
pub struct State {
    pub open: bool,
}

impl AppView {
    pub fn on_toggle_palette(
        &mut self,
        _action: &TogglePalette,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette_state.open = !self.palette_state.open;
        cx.notify();
    }

    pub fn render_palette(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
