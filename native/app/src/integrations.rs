//! Integrations settings section (wave-3): MCP apps management.
//! Stub renderer — replaced by the integrations parity agent.

use gpui::{Context, Div, div, prelude::*};

use crate::AppView;
use crate::theme;

impl AppView {
    pub(crate) fn render_integrations_section(&self, _cx: &mut Context<Self>) -> Div {
        div()
            .flex_1()
            .text_xs()
            .text_color(theme::fg_muted())
            .child("Integrations — not implemented yet".to_string())
    }
}
