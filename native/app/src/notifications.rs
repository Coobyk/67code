//! Toast notifications (P1): transient cards for connection changes, turn
//! completion/failure, and pending approvals. Driven by observe_shell_event,
//! called from AppView::apply_shell_event for every ShellEvent.

use gpui::{Context, IntoElement, div};
use crate::net::ShellEvent;
use crate::AppView;

#[derive(Default)]
pub struct State {}

impl AppView {
    /// Called for every ShellEvent before the main match in
    /// `apply_shell_event`. Inspect the event and push toasts as needed.
    pub fn observe_shell_event(&mut self, _event: &ShellEvent, _cx: &mut Context<Self>) {}

    /// Transient toast stack rendered above the timeline, bottom-right.
    pub fn render_notifications(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
