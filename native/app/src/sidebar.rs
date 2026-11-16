//! Sidebar: project grouping, thread rows, status indicators, context menus.
//! Owns `impl AppView` sidebar render methods and `sidebar::State`.

use gpui::{
    Entity, EntityInputHandler, FocusHandle, Focusable, MouseButton, SharedString, Subscription,
    Window, anchored, deferred, div, point, prelude::*, px, rgb,
};
use serde_json::json;
use uuid::Uuid;

use crate::model::{ConnectionStatus, Project, Thread};
use crate::text_field::{PromptSubmitted, TextField};
use crate::theme;
use crate::AppView;

/// Sidebar-local UI state. Expand freely — this field lives on `AppView`.
#[derive(Default)]
pub struct State {
    /// Text the user typed into the sidebar search field.
    pub query: String,
    /// Thread id whose context menu is open, plus open position.
    pub context_menu: Option<ContextMenu>,
    /// Thread currently being renamed (inline editor open).
    pub renaming: Option<SharedString>,
    /// Thread id awaiting a second "Delete" click to confirm.
    pub delete_confirm: Option<SharedString>,
    /// Inline new-project form is open.
    pub new_project_open: bool,
    /// Scroll position for the thread list.
    pub scroll: gpui::ScrollHandle,
    rename_field: Option<Entity<TextField>>,
    project_name_field: Option<Entity<TextField>>,
    project_path_field: Option<Entity<TextField>>,
    rename_blur: Option<Subscription>,
    /// Focus handle tracked by the sidebar root. The context menu grabs it on
    /// open so root-level key handling (Escape to dismiss) is reachable while
    /// focus would otherwise stay in the main content area.
    menu_focus: Option<FocusHandle>,
    subs: Vec<Subscription>,
    wired: bool,
}

#[derive(Clone)]
pub struct ContextMenu {
    pub thread_id: SharedString,
    pub position: gpui::Point<gpui::Pixels>,
}

/// Dot color for a thread's status (matches the web status pill palette).
fn status_dot_color(thread: &Thread) -> gpui::Rgba {
    if thread.running() {
        theme::primary()
    } else {
        match thread.status.as_ref() {
            "completed" => rgb(0x34d399),
            "failed" | "error" => rgb(0xf87171),
            "interrupted" | "stopped" | "cancelled" => rgb(0xfbbf24),
            _ => theme::fg_muted(),
        }
    }
}

/// Read a text field's contents during effects (observers/subscribers), when
/// the `&mut Window` isn't on the update stack but isn't passed in either.
fn field_text(field: &Entity<TextField>, cx: &mut Context<AppView>) -> String {
    if let Some(text) = cx
        .with_window(field.entity_id(), |window, app| {
            field.update(app, |field, cx| {
                field.text_for_range(0..usize::MAX, &mut None, window, cx)
            })
        })
        .flatten()
    {
        return text;
    }
    for handle in cx.windows() {
        if let Ok(text) = handle.update(cx, |_root, window, app| {
            field.update(app, |field, cx| {
                field.text_for_range(0..usize::MAX, &mut None, window, cx)
            })
        }) {
            return text.unwrap_or_default();
        }
    }
    String::new()
}

/// Read a text field's contents inside an event listener that has a `window`.
fn field_text_in(field: &Entity<TextField>, window: &mut Window, cx: &mut Context<AppView>) -> String {
    field
        .update(cx, |field, cx| {
            field.text_for_range(0..usize::MAX, &mut None, window, cx)
        })
        .unwrap_or_default()
}

fn focus_field(field: &Entity<TextField>, cx: &mut Context<AppView>) {
    let focus = field.read(cx).focus_handle(cx);
    let _ = cx.with_window(field.entity_id(), |window, app| {
        focus.focus(window, app);
    });
}

fn reset_field(field: &Entity<TextField>, cx: &mut Context<AppView>) {
    let _ = cx.with_window(field.entity_id(), |window, app| {
        field.update(app, |field, cx| field.reset(window, cx));
    });
}

fn set_field_text(field: &Entity<TextField>, text: &str, cx: &mut Context<AppView>) {
    let _ = cx.with_window(field.entity_id(), |window, app| {
        field.update(app, |field, cx| {
            field.replace_text_in_range(Some(0..usize::MAX), text, window, cx)
        });
    });
}

impl AppView {
    /// Lazily create the sidebar's text fields and subscriptions. Called once
    /// from `render_sidebar` because entities need a `cx` to be built.
    fn wire_sidebar(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_state.wired {
            return;
        }
        self.sidebar_state.wired = true;

        self.sidebar_state.subs.push(cx.observe(
            &self.search_field,
            |this, field, cx| {
                let query = field_text(&field, cx);
                if this.sidebar_state.query != query {
                    this.sidebar_state.query = query;
                    cx.notify();
                }
            },
        ));

        let rename_field = cx.new(|cx| TextField::new("Thread title…", cx));
        self.sidebar_state.subs.push(cx.subscribe(
            &rename_field,
            |this, _field, event: &PromptSubmitted, cx| {
                this.commit_rename(&event.0, cx);
            },
        ));
        self.sidebar_state.rename_field = Some(rename_field);

        let name_field = cx.new(|cx| TextField::new("Project name", cx));
        let path_field = cx.new(|cx| TextField::new("/path/to/workspace", cx));
        self.sidebar_state.subs.push(cx.subscribe(
            &name_field,
            |this, field, event: &PromptSubmitted, cx| {
                this.submit_project_form(&field, &event.0, cx);
            },
        ));
        self.sidebar_state.subs.push(cx.subscribe(
            &path_field,
            |this, field, event: &PromptSubmitted, cx| {
                this.submit_project_form(&field, &event.0, cx);
            },
        ));
        self.sidebar_state.project_name_field = Some(name_field);
        self.sidebar_state.project_path_field = Some(path_field);
        self.sidebar_state.menu_focus = Some(cx.focus_handle());
    }

    fn commit_rename(&mut self, title: &SharedString, cx: &mut Context<Self>) {
        let Some(thread_id) = self.sidebar_state.renaming.take() else {
            return;
        };
        let title = title.trim().to_string();
        if !title.is_empty() {
            if let Some(thread) = self.state.threads.iter_mut().find(|t| t.id == thread_id) {
                thread.title = title.clone().into();
            }
            self.dispatch(json!({
                "type": "thread.metadata.update",
                "commandId": Uuid::new_v4().to_string(),
                "threadId": thread_id.to_string(),
                "title": title,
            }));
        }
        cx.notify();
    }

    /// Open the inline rename editor for `thread_id`. Called from the context
    /// menu listener, which has a `window` for prefill + focus + blur-cancel.
    fn start_rename(
        &mut self,
        thread_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(field) = self.sidebar_state.rename_field.clone() else {
            return;
        };
        let title = self
            .state
            .threads
            .iter()
            .find(|t| t.id == thread_id)
            .map(|t| t.title.to_string())
            .unwrap_or_default();
        field.update(cx, |field, cx| {
            field.reset(window, cx);
            field.replace_text_in_range(None, &title, window, cx);
        });
        let focus = field.read(cx).focus_handle(cx);
        self.sidebar_state.rename_blur = Some(cx.on_blur(&focus, window, |this, _window, cx| {
            this.sidebar_state.renaming = None;
            cx.notify();
        }));
        self.sidebar_state.renaming = Some(thread_id);
        focus.focus(window, cx);
        cx.notify();
    }

    /// Enter pressed in either new-project field. `submitted` carries the
    /// submitting field's text (its content is already reset by the time this
    /// runs); the other field is read out via `field_text`.
    fn submit_project_form(
        &mut self,
        submitter: &Entity<TextField>,
        submitted: &SharedString,
        cx: &mut Context<Self>,
    ) {
        let (Some(name_field), Some(path_field)) = (
            self.sidebar_state.project_name_field.clone(),
            self.sidebar_state.project_path_field.clone(),
        ) else {
            return;
        };
        let name = if submitter == &name_field {
            submitted.to_string()
        } else {
            field_text(&name_field, cx)
        };
        let path = if submitter == &path_field {
            submitted.to_string()
        } else {
            field_text(&path_field, cx)
        };
        if path.trim().is_empty() {
            // Name submitted with no path yet: restore the name (its field was
            // cleared on submit) and move focus to the path field.
            set_field_text(&name_field, name.trim(), cx);
            focus_field(&path_field, cx);
            return;
        }
        self.create_project(name, path, cx);
    }

    /// "Create" button in the new-project form — same as `submit_project_form`
    /// but reads both fields through the listener's `window`.
    fn submit_project_form_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(name_field), Some(path_field)) = (
            self.sidebar_state.project_name_field.clone(),
            self.sidebar_state.project_path_field.clone(),
        ) else {
            return;
        };
        let name = field_text_in(&name_field, window, cx);
        let path = field_text_in(&path_field, window, cx);
        self.create_project(name, path, cx);
    }

    fn create_project(&mut self, name: String, path: String, cx: &mut Context<Self>) {
        let path = path.trim().to_string();
        if path.is_empty() {
            cx.notify();
            return;
        }
        let name = {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                path.rsplit('/')
                    .find(|segment| !segment.is_empty())
                    .unwrap_or("project")
                    .to_string()
            } else {
                trimmed.to_string()
            }
        };
        self.dispatch(json!({
            "type": "project.create",
            "commandId": Uuid::new_v4().to_string(),
            "title": name,
            "workspaceRoot": path,
        }));
        // The server assigns the project id, so an optimistic entry would
        // duplicate the upcoming upsert — only do it in offline mock mode.
        if self.state.connection == ConnectionStatus::Mock {
            self.state.projects.push(Project {
                id: Uuid::new_v4().to_string().into(),
                name: name.clone().into(),
                path: path.clone().into(),
                expanded: true,
            });
        }
        if let (Some(name_field), Some(path_field)) = (
            self.sidebar_state.project_name_field.clone(),
            self.sidebar_state.project_path_field.clone(),
        ) {
            reset_field(&name_field, cx);
            reset_field(&path_field, cx);
        }
        self.sidebar_state.new_project_open = false;
        cx.notify();
    }

    fn new_thread(&mut self, project_id: SharedString, cx: &mut Context<Self>) {
        let thread_id = Uuid::new_v4().to_string();
        self.dispatch(json!({
            "type": "thread.create",
            "commandId": Uuid::new_v4().to_string(),
            "threadId": thread_id,
            "projectId": project_id.to_string(),
            "title": "New thread",
            "modelSelection": {
                "instanceId": self.provider.clone(),
                "model": self.model.clone(),
            },
            "runtimeMode": "full-access",
            "interactionMode": "default",
            "branch": null,
            "worktreePath": null,
            "createdBy": "user",
            "creationSource": "web",
        }));
        // Optimistic entry; the server's ThreadUpserted carries our threadId
        // and replaces it in place.
        self.state.threads.push(Thread {
            id: thread_id.clone().into(),
            project_id,
            title: "New thread".into(),
            status: "idle".into(),
            activity_run_status: None,
            has_pending_request: false,
            has_actionable_proposed_plan: false,
            provider_instances: Vec::new(),
            pinned: false,
            updated_at: "".into(),
        });
        self.select_thread(thread_id.into(), cx);
    }

    fn render_thread_row(
        &self,
        thread: &Thread,
        row_id: usize,
        project_name: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.state.selected.as_ref() == Some(&thread.id);
        let renaming = self.sidebar_state.renaming.as_ref() == Some(&thread.id);
        let select_id = thread.id.clone();
        let menu_id = thread.id.clone();

        let mut row = div()
            .id(("thread", row_id))
            .mx_1()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .when(active, |s| s.bg(theme::selected()).text_color(theme::fg()))
            .when(!active, |s| {
                s.text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::hover()))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _window, cx| {
                    if this.sidebar_state.renaming.as_ref() != Some(&select_id) {
                        this.sidebar_state.renaming = None;
                    }
                    this.select_thread(select_id.clone(), cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &gpui::MouseDownEvent, window, cx| {
                    this.sidebar_state.context_menu = Some(ContextMenu {
                        thread_id: menu_id.clone(),
                        position: ev.position,
                    });
                    this.sidebar_state.delete_confirm = None;
                    if let Some(focus) = this.sidebar_state.menu_focus.clone() {
                        focus.focus(window, cx);
                    }
                    this.select_thread(menu_id.clone(), cx);
                }),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(6.))
                    .h(px(6.))
                    .rounded_full()
                    .bg(status_dot_color(thread)),
            );

        if renaming {
            if let Some(field) = self.sidebar_state.rename_field.clone() {
                row = row.child(div().flex_1().min_w_0().text_color(theme::fg()).child(field));
            }
        } else {
            row = row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_x_hidden()
                    .text_ellipsis()
                    .child(thread.title.clone()),
            );
        }

        if let Some(name) = project_name {
            row = row.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(name),
            );
        }
        if thread.has_pending_request {
            row = row.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(rgb(0xfbbf24))
                    .child("⚠"),
            );
        }
        if thread.has_actionable_proposed_plan {
            row = row.child(
                div()
                    .flex_none()
                    .px_1()
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(0xa78bfa))
                    .text_xs()
                    .text_color(rgb(0xa78bfa))
                    .child("Plan"),
            );
        }
        if thread.pinned {
            row = row.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme::primary())
                    .child("◆"),
            );
        }
        row
    }

    fn render_context_menu(&self, menu: &ContextMenu, cx: &mut Context<Self>) -> impl IntoElement {
        let thread = self
            .state
            .threads
            .iter()
            .find(|t| t.id == menu.thread_id)
            .cloned();
        let pinned = thread.as_ref().is_some_and(|t| t.pinned);
        let confirming = self.sidebar_state.delete_confirm.as_ref() == Some(&menu.thread_id);

        let rename_id = menu.thread_id.clone();
        let pin_id = menu.thread_id.clone();
        let archive_id = menu.thread_id.clone();
        let delete_id = menu.thread_id.clone();

        let mut menu_el = div()
            .flex()
            .flex_col()
            .w(px(190.))
            .py_1()
            .rounded_md()
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border())
            .text_sm()
            .text_color(theme::fg())
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _ev, _window, cx| cx.stop_propagation()),
            )
            .child(
                div()
                    .id(("mi", 0usize))
                    .px_3()
                    .py_1p5()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, window, cx| {
                            cx.stop_propagation();
                            this.sidebar_state.context_menu = None;
                            this.start_rename(rename_id.clone(), window, cx);
                        }),
                    )
                    .child("Rename"),
            )
            .child(
                div()
                    .id(("mi", 1usize))
                    .px_3()
                    .py_1p5()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            cx.stop_propagation();
                            this.sidebar_state.context_menu = None;
                            if let Some(t) =
                                this.state.threads.iter_mut().find(|t| t.id == pin_id)
                            {
                                t.pinned = !pinned;
                            }
                            this.dispatch(json!({
                                "type": if pinned { "thread.unpin" } else { "thread.pin" },
                                "commandId": Uuid::new_v4().to_string(),
                                "threadId": pin_id.to_string(),
                            }));
                            cx.notify();
                        }),
                    )
                    .child(if pinned { "Unpin" } else { "Pin" }),
            )
            .child(
                div()
                    .id(("mi", 2usize))
                    .px_3()
                    .py_1p5()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            cx.stop_propagation();
                            this.sidebar_state.context_menu = None;
                            this.state.threads.retain(|t| t.id != archive_id);
                            this.dispatch(json!({
                                "type": "thread.archive",
                                "commandId": Uuid::new_v4().to_string(),
                                "threadId": archive_id.to_string(),
                            }));
                            cx.notify();
                        }),
                    )
                    .child("Archive"),
            )
            .child(
                div()
                    .h(px(1.))
                    .mx_2()
                    .my_1()
                    .bg(theme::border()),
            );

        let delete_label = if confirming {
            "Confirm delete"
        } else {
            "Delete"
        };
        menu_el = menu_el.child(
            div()
                .id(("mi", 4usize))
                .px_3()
                .py_1p5()
                .cursor_pointer()
                .text_color(rgb(0xf87171))
                .hover(|s| s.bg(theme::hover()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        cx.stop_propagation();
                        if confirming {
                            this.sidebar_state.context_menu = None;
                            this.sidebar_state.delete_confirm = None;
                            this.state.threads.retain(|t| t.id != delete_id);
                            if this.state.selected.as_ref() == Some(&delete_id) {
                                this.state.selected = None;
                            }
                            this.dispatch(json!({
                                "type": "thread.delete",
                                "commandId": Uuid::new_v4().to_string(),
                                "threadId": delete_id.to_string(),
                            }));
                        } else {
                            this.sidebar_state.delete_confirm = Some(delete_id.clone());
                        }
                        cx.notify();
                    }),
                )
                .child(delete_label),
        );

        deferred(
            anchored()
                .position(point(menu.position.x, menu.position.y))
                .child(menu_el),
        )
        .with_priority(1)
    }

    fn render_project_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let name_field = self.sidebar_state.project_name_field.clone();
        let path_field = self.sidebar_state.project_path_field.clone();
        div()
            .mx_1()
            .mb_1()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .rounded_md()
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border())
            .when_some(name_field, |el, field| {
                el.child(
                    div()
                        .rounded_sm()
                        .bg(theme::canvas())
                        .border_1()
                        .border_color(theme::border())
                        .px_2()
                        .py_1()
                        .text_sm()
                        .text_color(theme::fg())
                        .child(field),
                )
            })
            .when_some(path_field, |el, field| {
                el.child(
                    div()
                        .rounded_sm()
                        .bg(theme::canvas())
                        .border_1()
                        .border_color(theme::border())
                        .px_2()
                        .py_1()
                        .text_sm()
                        .text_color(theme::fg())
                        .child(field),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pt_1()
                    .child(
                        div()
                            .id("create-project")
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .bg(theme::primary())
                            .text_xs()
                            .text_color(gpui::white())
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.85))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, window, cx| {
                                    cx.stop_propagation();
                                    this.submit_project_form_click(window, cx);
                                }),
                            )
                            .child("Create"),
                    )
                    .child(
                        div()
                            .id("cancel-project")
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.sidebar_state.new_project_open = false;
                                    cx.notify();
                                }),
                            )
                            .child("Cancel"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("Enter to create"),
                    ),
            )
    }

    pub fn render_sidebar(&mut self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        self.wire_sidebar(cx);

        let query = self.sidebar_state.query.trim().to_lowercase();
        let filtering = !query.is_empty();

        let status_text = match &self.state.connection {
            ConnectionStatus::Mock => "Local mock data".to_string(),
            ConnectionStatus::Connecting => "Connecting…".to_string(),
            ConnectionStatus::Connected => "Connected".to_string(),
            ConnectionStatus::Failed(e) => format!("Error: {e}"),
            ConnectionStatus::Disconnected(e) => format!("Disconnected: {e}"),
        };
        let status_color = match &self.state.connection {
            ConnectionStatus::Connected => theme::primary(),
            ConnectionStatus::Failed(_) | ConnectionStatus::Disconnected(_) => rgb(0xff5a5a),
            _ => theme::fg_muted(),
        };

        let mut list = div()
            .id("sidebar-list")
            .flex()
            .flex_col()
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&self.sidebar_state.scroll)
            .px_2()
            .pb_2();

        if filtering {
            let mut hits: Vec<Thread> = self
                .state
                .threads
                .iter()
                .filter(|t| t.title.to_lowercase().contains(&query))
                .cloned()
                .collect();
            hits.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            if hits.is_empty() {
                list = list.child(
                    div()
                        .px_2()
                        .py_2()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child("No matching threads"),
                );
            }
            for (row_id, thread) in hits.iter().enumerate() {
                let project_name = self
                    .state
                    .projects
                    .iter()
                    .find(|p| p.id == thread.project_id)
                    .map(|p| p.name.clone());
                list = list.child(self.render_thread_row(
                    thread,
                    row_id,
                    project_name,
                    cx,
                ));
            }
        } else {
            let projects = self.state.projects.clone();
            let mut row_id = 0usize;
            for (pi, project) in projects.iter().enumerate() {
                let toggle_id = project.id.clone();
                let new_thread_id = project.id.clone();
                let chevron = if project.expanded { "▾" } else { "▸" };
                let header = div()
                    .id(("project", pi))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            if let Some(project) = this
                                .state
                                .projects
                                .iter_mut()
                                .find(|p| p.id == toggle_id)
                            {
                                project.expanded = !project.expanded;
                            }
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .w(px(12.))
                            .flex_none()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(chevron),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .overflow_x_hidden()
                            .text_ellipsis()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::fg())
                            .child(project.name.clone()),
                    )
                    .child(
                        div()
                            .id(("new-thread", pi))
                            .px_1()
                            .rounded_sm()
                            .text_sm()
                            .text_color(theme::fg_muted())
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::fg()).bg(theme::hover()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.new_thread(new_thread_id.clone(), cx);
                                }),
                            )
                            .child("+"),
                    );

                let mut section = div().flex().flex_col().child(header);
                if project.expanded {
                    section = section.child(
                        div()
                            .pl_6()
                            .pr_2()
                            .pb_1()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .whitespace_nowrap()
                            .overflow_x_hidden()
                            .text_ellipsis()
                            .child(project.path.clone()),
                    );
                    let mut threads: Vec<Thread> = self
                        .state
                        .threads
                        .iter()
                        .filter(|t| t.project_id == project.id)
                        .cloned()
                        .collect();
                    threads.sort_by(|a, b| {
                        b.pinned
                            .cmp(&a.pinned)
                            .then_with(|| b.updated_at.cmp(&a.updated_at))
                    });
                    if threads.is_empty() {
                        section = section.child(
                            div()
                                .pl_6()
                                .pr_2()
                                .py_1()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child("No threads"),
                        );
                    }
                    for thread in threads.iter() {
                        section = section.child(self.render_thread_row(
                            thread, row_id, None, cx,
                        ));
                        row_id += 1;
                    }
                }
                list = list.child(section);
            }
        }

        let mut root = div()
            .w(px(260.))
            .h_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme::sidebar())
            .border_r_1()
            .border_color(theme::border())
            .when_some(
                self.sidebar_state.menu_focus.clone(),
                |el, focus| el.track_focus(&focus),
            )
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _window, cx| {
                if ev.keystroke.key == "escape" {
                    this.sidebar_state.context_menu = None;
                    this.sidebar_state.renaming = None;
                    this.sidebar_state.new_project_open = false;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .h(px(44.))
                    .px_4()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(theme::fg())
                            .child("67"),
                    ),
            )
            .child(
                div().px_2().pt_2().pb_1().child(
                    div()
                        .flex()
                        .items_center()
                        .rounded_md()
                        .bg(theme::raised())
                        .border_1()
                        .border_color(theme::border())
                        .px_2()
                        .py_1()
                        .text_sm()
                        .child(div().flex_1().min_w_0().text_color(theme::fg()).child(self.search_field.clone())),
                ),
            )
            .child(
                div()
                    .px_4()
                    .pt_1()
                    .pb_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("Projects"),
                    )
                    .child(
                        div()
                            .id("new-project")
                            .px_1()
                            .rounded_sm()
                            .text_sm()
                            .text_color(theme::fg_muted())
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::fg()).bg(theme::hover()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, window, cx| {
                                    cx.stop_propagation();
                                    this.sidebar_state.new_project_open =
                                        !this.sidebar_state.new_project_open;
                                    if this.sidebar_state.new_project_open
                                        && let Some(field) =
                                            this.sidebar_state.project_name_field.clone()
                                    {
                                        let focus = field.read(cx).focus_handle(cx);
                                        focus.focus(window, cx);
                                    }
                                    cx.notify();
                                }),
                            )
                            .child("+"),
                    ),
            )
            .when(self.sidebar_state.new_project_open, |el| {
                el.child(self.render_project_form(cx))
            })
            .child(list)
            .child(
                div()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(theme::border())
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .overflow_x_hidden()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(status_color)
                            .child(status_text),
                    )
                    .child(
                        div()
                            .id("settings-gear")
                            .px_1()
                            .rounded_sm()
                            .text_sm()
                            .text_color(theme::fg_muted())
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::fg()).bg(theme::hover()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.settings_state.open = !this.settings_state.open;
                                    cx.notify();
                                }),
                            )
                            .child("⚙"),
                    ),
            );

        if self.sidebar_state.context_menu.is_some() {
            root = root.child(
                deferred(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(px(10000.))
                        .h(px(10000.))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _window, cx| {
                                this.sidebar_state.context_menu = None;
                                this.sidebar_state.delete_confirm = None;
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, _ev, _window, cx| {
                                this.sidebar_state.context_menu = None;
                                this.sidebar_state.delete_confirm = None;
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        ),
                )
                .with_priority(0),
            );
        }
        if let Some(menu) = self.sidebar_state.context_menu.clone() {
            root = root.child(self.render_context_menu(&menu, cx));
        }

        root
    }
}

impl AppView {
    /// Snooze/thread-action overlay rendered above the sidebar.
    pub(crate) fn render_snooze_overlay(&self, _cx: &mut Context<Self>) -> gpui::Div {
        div()
    }
}
