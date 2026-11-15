//! ACP session management (P2): native agent sessions per provider+project
//! with list / import-as-thread / delete / logout — mirroring
//! AcpSessionManagementSection.tsx. Query RPCs:
//! `server.listAcpRegistrySessions` {instanceId, projectId, cursor?}
//! `server.importAcpRegistrySession` {instanceId, projectId, sessionId, title?, updatedAt?}
//! `server.deleteAcpRegistrySession` {instanceId, projectId, sessionId}
//! `server.logoutAcpRegistry` {instanceId}

use gpui::{Context, IntoElement, div, prelude::*};

use crate::AppView;

#[derive(Default)]
pub struct State {
    /// Fetched AcpRegistrySession payloads for the current provider+project.
    pub sessions: Vec<serde_json::Value>,
    pub next_cursor: Option<String>,
    pub can_load: bool,
    pub can_resume: bool,
    pub can_delete: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// Project id the session list is scoped to.
    pub project_id: Option<String>,
    /// Provider instance the section manages.
    pub instance_id: Option<String>,
}

impl AppView {
    pub fn render_acp_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = cx;
        div().child("ACP sessions — stub")
    }
}
