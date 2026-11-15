//! Environment switching (P2): saved server endpoints with add / remove /
//! connect — mirroring the Connections settings + SavedEnvironmentRows in the
//! web UI. Persisted in `config::ClientConfig.environments`; connecting sends
//! `ClientCommand::SwitchEndpoint` (net::run swaps endpoint and reconnects).

use gpui::{Context, IntoElement, div, prelude::*};

use crate::AppView;

#[derive(Default)]
pub struct State {
    /// Add-form draft fields (label / url / token).
    pub draft_label: String,
    pub draft_url: String,
    pub draft_token: String,
    /// Index pending removal confirmation.
    pub confirm_remove: Option<usize>,
}

impl AppView {
    pub fn render_envs_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = cx;
        div().child("Environments — stub")
    }
}
