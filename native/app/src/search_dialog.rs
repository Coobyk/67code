//! Project content-search overlay dialog.

use gpui::{div, prelude::*, Context, Div};

use crate::AppView;

#[derive(Default)]
pub struct State {
    /// Whether the project content-search dialog is open.
    pub open: bool,
}

impl AppView {
    pub(crate) fn render_search_dialog(&self, _cx: &mut Context<Self>) -> Div {
        div()
    }
}
