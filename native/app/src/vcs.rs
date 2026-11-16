//! Source-control surfaces: branch picker, worktrees, right-panel PRs tab.

use gpui::{div, prelude::*, Context, Div};

use crate::AppView;

#[derive(Default)]
pub struct State {}

impl AppView {
    pub(crate) fn render_prs_pane(&self, _cx: &mut Context<Self>) -> Div {
        div()
    }
}
