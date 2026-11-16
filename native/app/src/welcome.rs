//! First-run onboarding overlay. Owns `impl AppView` welcome render methods.

use gpui::{div, prelude::*, Context, Div};

use crate::AppView;

#[derive(Default)]
pub struct State {
    pub open: bool,
}

impl AppView {
    pub(crate) fn render_welcome(&self, _cx: &mut Context<Self>) -> Div {
        div()
    }
}
