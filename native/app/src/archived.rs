//! Archived threads settings section (P2): list archived threads grouped by
//! project, with unarchive / delete actions — mirroring
//! ArchivedThreadsPanel.tsx. Data comes from the
//! `orchestration.getArchivedShellSnapshot` query RPC.

use gpui::{Context, IntoElement, div, prelude::*};

use crate::AppView;

#[derive(Default)]
pub struct State {
    /// Raw `OrchestrationV2ThreadShell` payloads for archived threads.
    pub threads: Vec<serde_json::Value>,
    /// Raw `OrchestrationProjectShell` payloads (for group titles).
    pub projects: Vec<serde_json::Value>,
    pub loading: bool,
    pub error: Option<String>,
    /// Fetched at least once (auto-load when the section first renders).
    pub loaded: bool,
}

impl AppView {
    pub fn render_archived_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = cx;
        div().child("Archived threads — stub")
    }
}
