use gpui::SharedString;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Agent,
}

pub struct Message {
    pub role: Role,
    pub text: SharedString,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub id: SharedString,
    pub name: SharedString,
    pub path: SharedString,
    pub expanded: bool,
}

#[derive(Debug, Clone)]
pub struct Thread {
    pub id: SharedString,
    pub project_id: SharedString,
    pub title: SharedString,
    pub status: SharedString,
    /// Live run status while a turn is in flight, if any.
    pub activity_run_status: Option<String>,
    /// A runtime request (approval/user-input) is pending.
    pub has_pending_request: bool,
    /// A proposed plan awaits a decision.
    pub has_actionable_proposed_plan: bool,
    /// Provider instances that have run on this thread.
    pub provider_instances: Vec<SharedString>,
    pub pinned: bool,
    pub updated_at: SharedString,
    /// Snoozed until ISO timestamp.
    pub snoozed_until: Option<SharedString>,
    /// Explicit unread marker timestamp.
    pub marked_unread_at: Option<SharedString>,
    /// Last-visited timestamp.
    pub visited_at: Option<SharedString>,
    /// Latest run start timestamp (unread comparison).
    pub latest_run_started_at: Option<SharedString>,
    /// Linked PR object (url etc.) when present.
    pub linked_pull_request: Option<serde_json::Value>,
    pub pin_order_key: Option<SharedString>,
    pub active_order_key: Option<SharedString>,
    pub runtime_mode: Option<SharedString>,
    pub interaction_mode: Option<SharedString>,
}

impl Thread {
    /// Build a model thread from a wire shell row.
    pub fn from_shell(t: &crate::net::ShellThread) -> Self {
        Self {
            id: t.id.clone().into(),
            project_id: t.project_id.clone().into(),
            title: t.title.clone().into(),
            status: t.status.clone().into(),
            activity_run_status: t.activity_run_status.clone(),
            has_pending_request: t.has_pending_request,
            has_actionable_proposed_plan: t.has_actionable_proposed_plan,
            provider_instances: t
                .provider_instances
                .iter()
                .cloned()
                .map(Into::into)
                .collect(),
            pinned: t.pinned,
            updated_at: t.updated_at.clone().into(),
            snoozed_until: t.snoozed_until.clone().map(Into::into),
            marked_unread_at: t.marked_unread_at.clone().map(Into::into),
            visited_at: t.visited_at.clone().map(Into::into),
            latest_run_started_at: t.latest_run_started_at.clone().map(Into::into),
            linked_pull_request: t.linked_pull_request.clone(),
            pin_order_key: t.pin_order_key.clone().map(Into::into),
            active_order_key: t.active_order_key.clone().map(Into::into),
            runtime_mode: t.runtime_mode.clone().map(Into::into),
            interaction_mode: t.interaction_mode.clone().map(Into::into),
        }
    }

    /// Simple mock thread for the offline state.
    fn mock(id: &str, project_id: &str, title: &str) -> Self {
        Self {
            id: id.into(),
            project_id: project_id.into(),
            title: title.into(),
            status: "idle".into(),
            activity_run_status: None,
            has_pending_request: false,
            has_actionable_proposed_plan: false,
            provider_instances: Vec::new(),
            pinned: false,
            updated_at: "".into(),
            snoozed_until: None,
            marked_unread_at: None,
            visited_at: None,
            latest_run_started_at: None,
            linked_pull_request: None,
            pin_order_key: None,
            active_order_key: None,
            runtime_mode: None,
            interaction_mode: None,
        }
    }

    /// True while a run is actively working on this thread.
    pub fn running(&self) -> bool {
        self.activity_run_status
            .as_deref()
            .is_some_and(|s| matches!(s, "running" | "starting" | "queued" | "waiting"))
            || matches!(
                self.status.as_ref(),
                "running" | "starting" | "queued" | "active"
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    Mock,
    Connecting,
    Connected,
    Failed(SharedString),
    Disconnected(SharedString),
}

pub struct AppState {
    pub projects: Vec<Project>,
    pub threads: Vec<Thread>,
    /// Selected thread id.
    pub selected: Option<SharedString>,
    /// Mock messages shown per thread while the client is offline or before
    /// `orchestration.subscribeThread` lands.
    pub messages: Vec<Message>,
    pub connection: ConnectionStatus,
}

impl AppState {
    pub fn mock() -> Self {
        Self {
            projects: vec![
                Project {
                    id: "p-67code".into(),
                    name: "67code".into(),
                    path: "~/Projects/67code".into(),
                    expanded: true,
                },
                Project {
                    id: "p-web".into(),
                    name: "web-playground".into(),
                    path: "~/Projects/web-playground".into(),
                    expanded: true,
                },
            ],
            threads: vec![
                Thread::mock("t-rust", "p-67code", "Rewrite the app in Rust + GPUI"),
                Thread::mock("t-scheme", "p-67code", "Fix Electron scheme crash"),
                Thread::mock("t-rebrand", "p-67code", "Rebrand sweep"),
                Thread::mock("t-sandbox", "p-web", "Sandbox test"),
            ],
            selected: Some("t-rust".into()),
            messages: vec![
                Message {
                    role: Role::User,
                    text: "Rewrite the desktop app in Rust and GPUI.".into(),
                },
                Message {
                    role: Role::Agent,
                    text: "On it. Scaffolding a GPUI shell on the dev box, committing as I go."
                        .into(),
                },
            ],
            connection: ConnectionStatus::Mock,
        }
    }

    pub fn connecting() -> Self {
        Self {
            projects: Vec::new(),
            threads: Vec::new(),
            selected: None,
            messages: Vec::new(),
            connection: ConnectionStatus::Connecting,
        }
    }

    pub fn threads_for<'a>(&'a self, project_id: &'a str) -> impl Iterator<Item = &'a Thread> {
        self.threads
            .iter()
            .filter(move |t| t.project_id.as_ref() == project_id)
    }

    pub fn selected_thread(&self) -> Option<&Thread> {
        let id = self.selected.as_ref()?;
        self.threads.iter().find(|t| &t.id == id)
    }
}
