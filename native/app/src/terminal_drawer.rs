//! Thread terminal drawer + right-panel Terminals tab.

use gpui::{div, prelude::*, Context, Div};

use crate::AppView;

#[derive(Default)]
pub struct State {
    /// Whether the bottom terminal drawer is open for the selected thread.
    pub drawer_open: bool,
}

impl AppView {
    pub(crate) fn render_terminal_drawer(&self, _cx: &mut Context<Self>) -> Div {
        div()
    }

    pub(crate) fn render_terminal_pane(&self, _cx: &mut Context<Self>) -> Div {
        div()
    }
}
