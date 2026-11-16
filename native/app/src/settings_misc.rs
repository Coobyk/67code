//! Settings sections: Notifications, Chat defaults, Projects.

use gpui::{div, prelude::*, Context, Div};

use crate::AppView;

#[derive(Default)]
pub struct State {}

impl AppView {
    pub(crate) fn render_notifications_section(&self, _cx: &mut Context<Self>) -> Div {
        div()
    }

    pub(crate) fn render_chat_defaults_section(&self, _cx: &mut Context<Self>) -> Div {
        div()
    }

    pub(crate) fn render_projects_section(&self, _cx: &mut Context<Self>) -> Div {
        div()
    }
}
