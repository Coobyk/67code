//! Authorized clients settings section (P2): paired device sessions with
//! revoke / revoke-others + pairing-link creation — mirroring
//! AuthorizedClientsSettings.tsx. Live data rides the `subscribeAuthAccess`
//! stream (ShellEvent::AuthAccessChange → observe_auth_access); revokes are
//! HTTP POSTs to `/api/auth/clients/revoke{,-others}`.

use gpui::{Context, IntoElement, div, prelude::*};

use crate::AppView;

#[derive(Default)]
pub struct State {
    /// Raw client-session payloads from the auth-access stream.
    pub sessions: Vec<serde_json::Value>,
    /// Raw pairing-link payloads.
    pub pairing_links: Vec<serde_json::Value>,
    /// Session id currently being revoked (disables its button).
    pub revoking: Option<String>,
}

impl AppView {
    /// Fold a raw auth-access stream value into state
    /// (snapshot / clientUpserted / clientRemoved / pairingLink*).
    pub fn observe_auth_access(&mut self, value: &serde_json::Value, cx: &mut Context<Self>) {
        let _ = (value, cx);
    }

    pub fn render_clients_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = cx;
        div().child("Authorized clients — stub")
    }
}
