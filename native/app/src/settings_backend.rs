//! Settings sections: Scheduled Tasks, Secrets, Diagnostics, Usage Limits.

use gpui::{div, prelude::*, Context, Div};

use crate::AppView;

#[derive(Default)]
pub struct State {}

impl AppView {
    pub(crate) fn render_tasks_section(&self, _cx: &mut Context<Self>) -> Div {
        div()
    }

    pub(crate) fn render_secrets_section(&self, _cx: &mut Context<Self>) -> Div {
        div()
    }

    pub(crate) fn render_diagnostics_section(&self, _cx: &mut Context<Self>) -> Div {
        div()
    }

    pub(crate) fn render_usage_section(&self, _cx: &mut Context<Self>) -> Div {
        div()
    }
}
