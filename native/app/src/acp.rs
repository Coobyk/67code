//! ACP session management (P2): native agent sessions per provider+project
//! with list / import-as-thread / delete / logout — mirroring
//! AcpSessionManagementSection.tsx. Query RPCs:
//! `server.listAcpRegistrySessions` {instanceId, projectId, cursor?}
//! `server.importAcpRegistrySession` {instanceId, projectId, sessionId, title?, updatedAt?}
//! `server.deleteAcpRegistrySession` {instanceId, projectId, sessionId}
//! `server.logoutAcpRegistry` {instanceId}

use gpui::{Context, IntoElement, MouseButton, SharedString, div, prelude::*, px, rgb};
use serde_json::{Value, json};

use crate::composer::MODEL_CATALOG;
use crate::notifications::ToastKind;
use crate::{AppView, theme};

/// One `AcpRegistrySession` row from `server.listAcpRegistrySessions`.
#[derive(Clone, Debug)]
pub struct Session {
    pub session_id: String,
    pub title: Option<String>,
    pub updated_at: Option<String>,
    /// Thread id once imported — Import disables and shows "Imported".
    pub imported_thread_id: Option<String>,
}

/// Parse one wire session; rows without a `sessionId` are skipped.
fn parse_session(v: &Value) -> Option<Session> {
    let session_id = v["sessionId"].as_str()?.to_string();
    Some(Session {
        session_id,
        title: v["title"].as_str().map(str::to_owned),
        updated_at: v["updatedAt"].as_str().map(str::to_owned),
        imported_thread_id: v["importedThreadId"].as_str().map(str::to_owned),
    })
}

/// Read a human message out of an `Exit` failure frame
/// (`{"_tag":"Failure","cause":{"_tag":"Fail","error":{"message":…}}}`).
fn failure_message(exit: &Value) -> String {
    let cause = &exit["cause"];
    for err in [
        &cause["error"],
        &cause["failure"],
        &exit["error"],
        &exit["failure"],
        cause,
    ] {
        if let Some(message) = err["message"].as_str() {
            return message.to_string();
        }
        if let Some(message) = err.as_str() {
            return message.to_string();
        }
    }
    "The ACP operation failed.".to_string()
}

/// `YYYY-MM-DD HH:MM` from an ISO-8601 `updatedAt`; falls back to the raw
/// string when it doesn't look like one.
fn format_updated_at(updated_at: &str) -> String {
    updated_at
        .get(..16)
        .filter(|_| updated_at.as_bytes().get(10) == Some(&b'T'))
        .map(|slice| slice.replace('T', " "))
        .unwrap_or_else(|| updated_at.to_string())
}

/// Small settings-surface button. `enabled=false` renders it dimmed; the
/// caller then skips attaching a click handler.
fn acp_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_3()
        .py_1p5()
        .rounded_md()
        .text_xs()
        .bg(theme::hover())
        .when(enabled, |s| {
            s.cursor_pointer()
                .text_color(theme::fg())
                .hover(|s| s.bg(theme::selected()))
        })
        .when(!enabled, |s| s.text_color(theme::fg_muted()).opacity(0.6))
        .child(label.into())
}

/// Muted single-line hint under the section header.
fn hint(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(text.into())
}

#[derive(Default)]
pub struct State {
    /// Sessions fetched for the current provider+project.
    pub sessions: Vec<Session>,
    /// Cursor for the next page, when the server reports one.
    pub next_cursor: Option<String>,
    /// `canLoad` — sessions can be imported as threads.
    pub can_load: bool,
    /// `canResume` — sessions can be resumed in place.
    pub can_resume: bool,
    /// `canDelete` — the provider supports deleting native sessions.
    pub can_delete: bool,
    /// A list request is in flight.
    pub loading: bool,
    /// A list was attempted (drives the empty state + Refresh label).
    pub listed: bool,
    /// Last list/operation failure, rendered inline.
    pub error: Option<String>,
    /// Selected project id; falls back to the first project.
    pub project_id: Option<String>,
    /// Provider instance id; defaults to `devin` when it's in the catalog.
    pub instance_id: Option<String>,
    /// Project dropdown open.
    pub project_menu_open: bool,
    /// Session id with an import in flight.
    pub importing: Option<String>,
    /// Session id with a delete in flight.
    pub deleting: Option<String>,
    /// Session id whose delete is armed — a second click confirms.
    pub confirm_delete: Option<String>,
    /// Logout request in flight.
    pub logging_out: bool,
    /// Bumped on project change so late replies can't clobber new state.
    generation: u64,
}

impl AppView {
    /// Provider instance this section manages: `devin` when the model catalog
    /// knows it, otherwise the composer-selected instance.
    fn acp_instance_id(&self) -> String {
        self.acp_state.instance_id.clone().unwrap_or_else(|| {
            if MODEL_CATALOG
                .iter()
                .any(|(instance, _, _)| *instance == "devin")
            {
                "devin".to_string()
            } else {
                self.provider.clone()
            }
        })
    }

    /// Selected project id, falling back to the first project (mirrors the
    /// web section's `projects.find(...)?.id ?? projects[0]?.id`).
    fn acp_project_id(&self) -> Option<String> {
        let selected = self.acp_state.project_id.as_deref();
        self.state
            .projects
            .iter()
            .find(|p| Some(p.id.as_ref()) == selected)
            .or_else(|| self.state.projects.first())
            .map(|p| p.id.to_string())
    }

    /// Push a toast onto the shared notifications surface.
    fn acp_toast(
        &mut self,
        kind: ToastKind,
        title: impl Into<SharedString>,
        detail: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.push_toast(kind, title, detail, None, cx);
    }

    /// Swap the scoped project and retire every pending/listed operation —
    /// the generation bump invalidates replies still in flight.
    fn acp_select_project(&mut self, project_id: String, cx: &mut Context<Self>) {
        {
            let state = &mut self.acp_state;
            state.project_menu_open = false;
            if state.project_id.as_deref() == Some(project_id.as_str()) {
                cx.notify();
                return;
            }
            state.generation = state.generation.wrapping_add(1);
            state.project_id = Some(project_id);
            state.sessions.clear();
            state.next_cursor = None;
            state.loading = false;
            state.listed = false;
            state.error = None;
            state.importing = None;
            state.deleting = None;
            state.confirm_delete = None;
        }
        cx.notify();
    }

    /// `server.listAcpRegistrySessions` — `cursor=None` refreshes (replaces),
    /// `Some` appends the next page ("Load more").
    fn acp_load_sessions(&mut self, cursor: Option<String>, cx: &mut Context<Self>) {
        if self.acp_state.loading {
            return;
        }
        let Some(project_id) = self.acp_project_id() else {
            return;
        };
        let instance_id = self.acp_instance_id();
        let append = cursor.is_some();
        let mut payload = json!({
            "instanceId": instance_id,
            "projectId": project_id,
        });
        if let Some(cursor) = cursor {
            payload["cursor"] = json!(cursor);
        }
        let generation = self.acp_state.generation;
        let Some(rx) = self.send_query("server.listAcpRegistrySessions", payload) else {
            self.acp_state.error = Some("Not connected to a server.".to_string());
            cx.notify();
            return;
        };
        self.acp_state.loading = true;
        self.acp_state.error = None;
        self.acp_state.confirm_delete = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let _ = this.update(cx, |this, cx| {
                this.acp_apply_list(exit, append, generation, cx);
            });
        })
        .detach();
    }

    /// Fold a list reply into state; a stale generation (project changed
    /// mid-flight) drops the reply untouched.
    fn acp_apply_list(
        &mut self,
        exit: Value,
        append: bool,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        if self.acp_state.generation != generation {
            return;
        }
        self.acp_state.loading = false;
        self.acp_state.listed = true;
        if exit["_tag"].as_str() == Some("Success") {
            let value = &exit["value"];
            let sessions: Vec<Session> = value["sessions"]
                .as_array()
                .map(|a| a.iter().filter_map(parse_session).collect())
                .unwrap_or_default();
            if append {
                self.acp_state.sessions.extend(sessions);
            } else {
                self.acp_state.sessions = sessions;
            }
            self.acp_state.next_cursor = value["nextCursor"].as_str().map(str::to_owned);
            self.acp_state.can_load = value["canLoad"].as_bool() == Some(true);
            self.acp_state.can_resume = value["canResume"].as_bool() == Some(true);
            self.acp_state.can_delete = value["canDelete"].as_bool() == Some(true);
            self.acp_state.confirm_delete = None;
        } else {
            self.acp_state.error = Some(failure_message(&exit));
            if !append {
                self.acp_state.sessions.clear();
                self.acp_state.next_cursor = None;
            }
        }
        cx.notify();
    }

    /// `server.importAcpRegistrySession` — resume a native session as a T3
    /// thread. One import at a time; already-imported rows are inert.
    fn acp_import_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        if self.acp_state.importing.is_some() {
            return;
        }
        let Some(session) = self
            .acp_state
            .sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .cloned()
        else {
            return;
        };
        if session.imported_thread_id.is_some() {
            return;
        }
        let Some(project_id) = self.acp_project_id() else {
            return;
        };
        let instance_id = self.acp_instance_id();
        let generation = self.acp_state.generation;
        let Some(rx) = self.send_query(
            "server.importAcpRegistrySession",
            json!({
                "instanceId": instance_id,
                "projectId": project_id,
                "sessionId": session.session_id,
                "title": session.title,
                "updatedAt": session.updated_at,
            }),
        ) else {
            self.acp_toast(
                ToastKind::Error,
                "Could not import ACP session",
                Some("Not connected to a server.".to_string()),
                cx,
            );
            return;
        };
        self.acp_state.importing = Some(session.session_id.clone());
        self.acp_state.confirm_delete = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let _ = this.update(cx, |this, cx| {
                if this.acp_state.generation != generation {
                    return;
                }
                this.acp_state.importing = None;
                if exit["_tag"].as_str() == Some("Success") {
                    let value = &exit["value"];
                    let imported = value["imported"].as_bool() == Some(true);
                    if let Some(thread_id) = value["threadId"].as_str() {
                        let thread_id = thread_id.to_string();
                        if let Some(s) = this
                            .acp_state
                            .sessions
                            .iter_mut()
                            .find(|s| s.session_id == session.session_id)
                        {
                            s.imported_thread_id = Some(thread_id);
                        }
                    }
                    this.acp_toast(
                        ToastKind::Success,
                        if imported {
                            "ACP session imported"
                        } else {
                            "ACP session already imported"
                        },
                        None,
                        cx,
                    );
                } else {
                    let message = failure_message(&exit);
                    this.acp_toast(
                        ToastKind::Error,
                        "Could not import ACP session",
                        Some(message),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `server.deleteAcpRegistrySession` — destructive, two-stage: the first
    /// click arms the row's button ("Confirm"), the second executes.
    fn acp_delete_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        if self.acp_state.deleting.is_some() {
            return;
        }
        if self.acp_state.confirm_delete.as_deref() != Some(session_id.as_str()) {
            self.acp_state.confirm_delete = Some(session_id);
            cx.notify();
            return;
        }
        self.acp_state.confirm_delete = None;
        let Some(session) = self
            .acp_state
            .sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .cloned()
        else {
            return;
        };
        if session.imported_thread_id.is_some() {
            return;
        }
        let Some(project_id) = self.acp_project_id() else {
            return;
        };
        let instance_id = self.acp_instance_id();
        let generation = self.acp_state.generation;
        let Some(rx) = self.send_query(
            "server.deleteAcpRegistrySession",
            json!({
                "instanceId": instance_id,
                "projectId": project_id,
                "sessionId": session.session_id,
            }),
        ) else {
            self.acp_toast(
                ToastKind::Error,
                "Could not delete ACP session",
                Some("Not connected to a server.".to_string()),
                cx,
            );
            return;
        };
        self.acp_state.deleting = Some(session.session_id.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let _ = this.update(cx, |this, cx| {
                if this.acp_state.generation != generation {
                    return;
                }
                this.acp_state.deleting = None;
                if exit["_tag"].as_str() == Some("Success") {
                    this.acp_state
                        .sessions
                        .retain(|s| s.session_id != session.session_id);
                    this.acp_toast(ToastKind::Success, "ACP session deleted", None, cx);
                } else {
                    let message = failure_message(&exit);
                    this.acp_toast(
                        ToastKind::Error,
                        "Could not delete ACP session",
                        Some(message),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `server.logoutAcpRegistry` — clears the listed sessions on success.
    fn acp_logout(&mut self, cx: &mut Context<Self>) {
        if self.acp_state.logging_out {
            return;
        }
        let instance_id = self.acp_instance_id();
        let generation = self.acp_state.generation;
        let Some(rx) = self.send_query(
            "server.logoutAcpRegistry",
            json!({ "instanceId": instance_id }),
        ) else {
            self.acp_toast(
                ToastKind::Error,
                "Could not log out of ACP agent",
                Some("Not connected to a server.".to_string()),
                cx,
            );
            return;
        };
        self.acp_state.logging_out = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let _ = this.update(cx, |this, cx| {
                if this.acp_state.generation != generation {
                    return;
                }
                this.acp_state.logging_out = false;
                if exit["_tag"].as_str() == Some("Success") {
                    this.acp_state.sessions.clear();
                    this.acp_state.next_cursor = None;
                    this.acp_state.confirm_delete = None;
                    this.acp_toast(ToastKind::Success, "Logged out of ACP agent", None, cx);
                } else {
                    let message = failure_message(&exit);
                    this.acp_toast(
                        ToastKind::Error,
                        "Could not log out of ACP agent",
                        Some(message),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn render_acp_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = &self.acp_state;
        // A command channel exists even while the shell reports Failed
        // (a single failed request flips the status); queries still ride the
        // socket and Failure exits render gracefully.
        let connected = self.commands.is_some();
        let has_projects = !self.state.projects.is_empty();
        let project_id = self.acp_project_id();
        let instance_id = self.acp_instance_id();
        let project_title = self
            .state
            .projects
            .iter()
            .find(|p| Some(p.id.to_string()) == project_id)
            .map(|p| p.name.to_string())
            .unwrap_or_else(|| "Select project".to_string());
        let can_import = state.can_load || state.can_resume;

        let mut section = div().flex().flex_col().gap_3();

        // Header: "Native sessions" + description, controls on the right.
        let mut controls = div().flex().items_center().gap_2().flex_none();

        let logout_enabled = connected && !state.logging_out;
        controls = controls.child(
            acp_button(
                "acp-logout",
                if state.logging_out {
                    "Logging out…"
                } else {
                    "Log out"
                },
                logout_enabled,
            )
            .when(logout_enabled, |s| {
                s.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _window, cx| this.acp_logout(cx)),
                )
            }),
        );

        if has_projects {
            // Project picker chip; the menu renders in-flow below the header.
            let picker_open = state.project_menu_open;
            controls = controls.child(
                div()
                    .id("acp-project-chip")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .text_xs()
                    .cursor_pointer()
                    .text_color(theme::fg())
                    .when(picker_open, |s| s.bg(theme::selected()))
                    .when(!picker_open, |s| s.hover(|s| s.bg(theme::hover())))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            this.acp_state.project_menu_open = !this.acp_state.project_menu_open;
                            cx.notify();
                        }),
                    )
                    .child(div().max_w(px(160.)).truncate().child(project_title))
                    .child(div().text_color(theme::fg_muted()).child("▾")),
            );

            let list_label = if state.loading {
                "Loading…"
            } else if state.listed || !state.sessions.is_empty() {
                "Refresh"
            } else {
                "List sessions"
            };
            let list_enabled = connected && !state.loading && project_id.is_some();
            controls = controls.child(acp_button("acp-list", list_label, list_enabled).when(
                list_enabled,
                |s| {
                    s.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            this.acp_load_sessions(None, cx);
                        }),
                    )
                },
            ));
        }

        section = section.child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::fg())
                                .child("Native sessions"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child("Resume agent-owned conversations as T3 threads."),
                        )
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(theme::fg_muted())
                                .child(format!("Provider instance: {instance_id}")),
                        ),
                )
                .child(controls),
        );

        if !has_projects {
            section = section.child(hint("Add a project before importing sessions."));
        }

        // In-flow project menu (the settings panel scrolls; an absolute
        // overlay would clip against its viewport).
        if state.project_menu_open && has_projects {
            let mut menu = div()
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(theme::border())
                .bg(theme::raised())
                .py_1();
            for (index, project) in self.state.projects.iter().enumerate() {
                let pid = project.id.to_string();
                let active = project_id.as_deref() == Some(pid.as_str());
                menu = menu.child(
                    div()
                        .id(("acp-project", index))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1p5()
                        .text_xs()
                        .cursor_pointer()
                        .when(active, |s| s.bg(theme::selected()))
                        .when(!active, |s| s.hover(|s| s.bg(theme::hover())))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.acp_select_project(pid.clone(), cx);
                            }),
                        )
                        .child(
                            div()
                                .w(px(14.))
                                .flex_none()
                                .text_color(theme::primary())
                                .child(if active { "✓" } else { "" }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme::fg())
                                .child(project.name.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .max_w(px(220.))
                                .truncate()
                                .text_color(theme::fg_muted())
                                .child(project.path.clone()),
                        ),
                );
            }
            section = section.child(menu);
        }

        if let Some(error) = &state.error {
            section = section.child(
                div()
                    .text_xs()
                    .text_color(rgb(0xf87171))
                    .child(format!("Could not list ACP sessions: {error}")),
            );
        }

        // Session rows: title (or sessionId), muted mono sessionId, optional
        // updatedAt, then Import/Delete.
        if !state.sessions.is_empty() {
            let mut list = div()
                .flex()
                .flex_col()
                .border_t_1()
                .border_b_1()
                .border_color(theme::border());
            for (index, session) in state.sessions.iter().enumerate() {
                let session_id = session.session_id.clone();
                let imported = session.imported_thread_id.is_some();
                let importing_this =
                    state.importing.as_deref() == Some(session.session_id.as_str());
                let deleting_this = state.deleting.as_deref() == Some(session.session_id.as_str());
                let armed = state.confirm_delete.as_deref() == Some(session.session_id.as_str());

                let import_label = if imported {
                    "Imported"
                } else if importing_this {
                    "Importing…"
                } else {
                    "Import"
                };
                let import_enabled = can_import && !imported && state.importing.is_none();

                let mut left = div()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div().truncate().text_xs().text_color(theme::fg()).child(
                            session
                                .title
                                .clone()
                                .unwrap_or_else(|| session.session_id.clone()),
                        ),
                    )
                    .child(
                        div()
                            .truncate()
                            .font_family("monospace")
                            .text_size(px(10.))
                            .text_color(theme::fg_muted())
                            .child(session.session_id.clone()),
                    );
                if let Some(updated_at) = &session.updated_at {
                    left = left.child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme::fg_muted())
                            .child(format_updated_at(updated_at)),
                    );
                }

                let mut buttons = div().flex().flex_none().items_center().gap_1();

                let mut import_btn = div()
                    .id(("acp-import", index))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .child(import_label);
                if import_enabled {
                    let sid = session_id.clone();
                    import_btn = import_btn
                        .cursor_pointer()
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::hover()))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.acp_import_session(sid.clone(), cx);
                            }),
                        );
                } else {
                    import_btn = import_btn.text_color(theme::fg_muted()).opacity(0.7);
                }
                buttons = buttons.child(import_btn);

                if state.can_delete {
                    let delete_enabled = !imported && state.deleting.is_none();
                    let delete_label = if deleting_this {
                        "Deleting…"
                    } else if armed {
                        "Confirm"
                    } else {
                        "Delete"
                    };
                    let mut delete_btn = div()
                        .id(("acp-delete", index))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .child(delete_label);
                    if delete_enabled {
                        let sid = session_id.clone();
                        delete_btn = delete_btn
                            .cursor_pointer()
                            .when(armed, |s| {
                                s.bg(rgb(0x7a2e2e))
                                    .text_color(gpui::white())
                                    .hover(|s| s.bg(rgb(0x9a3a3a)))
                            })
                            .when(!armed, |s| {
                                s.text_color(theme::fg_muted())
                                    .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    this.acp_delete_session(sid.clone(), cx);
                                }),
                            );
                    } else {
                        delete_btn = delete_btn.text_color(theme::fg_muted()).opacity(0.7);
                    }
                    buttons = buttons.child(delete_btn);
                }

                list = list.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .py_2()
                        .when(index > 0, |s| s.border_t_1().border_color(theme::border()))
                        .child(left)
                        .child(buttons),
                );
            }
            section = section.child(list);
        } else if state.listed && !state.loading && state.error.is_none() && has_projects {
            section = section.child(hint("No sessions found for this project."));
        }

        if let Some(cursor) = &state.next_cursor {
            let cursor = cursor.clone();
            let more_enabled = connected && !state.loading;
            section = section.child(
                div().flex().child(
                    acp_button(
                        "acp-load-more",
                        if state.loading {
                            "Loading…"
                        } else {
                            "Load more"
                        },
                        more_enabled,
                    )
                    .when(more_enabled, |s| {
                        s.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.acp_load_sessions(Some(cursor.clone()), cx);
                            }),
                        )
                    }),
                ),
            );
        }

        section
    }
}
