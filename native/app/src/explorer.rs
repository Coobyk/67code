//! File explorer + preview pane (right-panel Files tab).

use gpui::{div, prelude::*, Context, Div};

use crate::AppView;

#[derive(Default)]
pub struct State {}

impl AppView {
    pub(crate) fn render_explorer_pane(&self, _cx: &mut Context<Self>) -> Div {
        div()
    }
}
