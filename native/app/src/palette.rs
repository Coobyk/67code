//! Command palette + global keyboard shortcuts (P1).
//!
//! cmd-k opens a centered overlay near the top of the window: a fuzzy-filtered
//! list of recent threads plus commands. Up/Down navigate, Enter executes,
//! Esc closes, clicking the dimmed backdrop dismisses. The query input is a
//! `TextField` entity created lazily on first open (entities need a window
//! context), and list keys are handled in an `App::intercept_keystrokes` hook
//! — the same approach composer.rs uses for its slash menu — because the
//! focused `TextField` owns the keystroke before keymap dispatch.
//!
//! INTEGRATE-BINDING: none required — the five App-context bindings registered
//! in lib.rs cover this surface; palette list keys are consumed by the
//! palette's own keystroke interceptor before they can reach the TextField.

use gpui::{
    Context, Entity, EntityInputHandler, Focusable, KeystrokeEvent, MouseButton, SharedString,
    Subscription, Window, div, point, prelude::*, px,
};
use serde_json::json;
use uuid::Uuid;

use crate::net::ClientCommand;
use crate::text_field::{PromptSubmitted, TextField};
use crate::theme;
use crate::{AppView, TogglePalette};

/// Threads shown under "Recent threads" when the query is empty (matches
/// `RECENT_THREAD_LIMIT` in the web palette).
const RECENT_THREAD_LIMIT: usize = 12;
/// Cap on fuzzy-filtered thread hits so commands stay reachable.
const FILTERED_THREAD_LIMIT: usize = 12;

pub struct State {
    /// Palette overlay visible. Read by `on_close_overlay` in lib.rs.
    pub open: bool,
    /// Query input, created lazily on first open — `cx.new` needs a context.
    pub field: Option<Entity<TextField>>,
    /// Mirror of `field`'s contents. `render_palette` takes `&self`, so an
    /// entity observer syncs the text into this plain string.
    query: String,
    /// Highlighted entry's index in the flat enabled-item list.
    selection: usize,
    /// "Delete thread" was executed once; executing it again confirms.
    delete_confirm: bool,
    /// Thread id whose title the input is currently editing (rename mode).
    renaming: Option<SharedString>,
    /// Scroll position of the results list.
    scroll: gpui::ScrollHandle,
    subs: Vec<Subscription>,
    hooks_installed: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            field: None,
            query: String::new(),
            selection: 0,
            delete_confirm: false,
            renaming: None,
            scroll: gpui::ScrollHandle::new(),
            subs: Vec::new(),
            hooks_installed: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CommandKind {
    NewThread,
    OpenSettings,
    Archive,
    Pin,
    Rename,
    Delete,
    Stop,
}

/// One selectable palette entry.
#[derive(Clone)]
enum Entry {
    Thread(SharedString),
    Command(CommandKind),
}

/// A flat result row (section headers are derived from `section` at render).
struct Row {
    entry: Entry,
    section: &'static str,
    label: String,
    detail: Option<String>,
    /// Keyboard-hint chip on the right edge.
    hint: Option<&'static str>,
    /// Disabled rows render dimmed and are skipped by keyboard navigation.
    enabled: bool,
    /// Render in the destructive color (armed delete confirmation).
    danger: bool,
}

/// Case-insensitive subsequence match — every query char appears in order.
fn fuzzy_match(query: &str, haystack: &str) -> bool {
    let mut hay = haystack
        .chars()
        .flat_map(|c| c.to_lowercase());
    query
        .chars()
        .flat_map(|c| c.to_lowercase())
        .all(|qc| hay.any(|hc| hc == qc))
}

/// Read a text field's contents when no `&mut Window` is on the update stack
/// (entity observers run with `&mut App` only). Same approach sidebar.rs uses.
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

fn app_view(window: &mut Window) -> Option<Entity<AppView>> {
    window.root::<AppView>().flatten()
}

impl AppView {
    /// Lazily build the palette's query field + subscriptions, then open.
    /// Called from `on_toggle_palette`, which has the `window` entity
    /// creation and focus require.
    fn ensure_palette_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette_state.field.is_some() {
            return;
        }
        let field = cx.new(|cx| TextField::new("Search threads and commands…", cx));

        // Keep the plain-string query mirror in sync so `&self` render can
        // filter. A changed query resets highlight + delete arming.
        self.palette_state.subs.push(cx.observe(
            &field,
            |this, field, cx| {
                let query = field_text(&field, cx);
                let state = &mut this.palette_state;
                if state.query != query {
                    state.query = query;
                    state.selection = 0;
                    state.delete_confirm = false;
                    cx.notify();
                }
            },
        ));

        // Enter normally routes here through the TextField's Submit action,
        // but the keystroke interceptor consumes it first; this subscription
        // is a fallback for paths that emit `PromptSubmitted` directly.
        self.palette_state.subs.push(cx.subscribe_in(
            &field,
            window,
            |this, _field, _event: &PromptSubmitted, window, cx| {
                this.run_palette_selection(window, cx);
            },
        ));

        self.palette_state.field = Some(field);
    }

    /// Install the app-wide keystroke hook once. It runs before keymap/action
    /// dispatch, which is what lets Up/Down/Enter drive the list while the
    /// `TextField` owns focus.
    fn ensure_palette_hooks(&mut self, cx: &mut Context<Self>) {
        if self.palette_state.hooks_installed {
            return;
        }
        self.palette_state.hooks_installed = true;
        let subscription = cx.intercept_keystrokes(|event, window, cx| {
            let Some(view) = app_view(window) else {
                return;
            };
            view.update(cx, |this, cx| this.on_palette_keystroke(event, window, cx));
        });
        self.palette_state.subs.push(subscription);
    }

    pub fn on_toggle_palette(
        &mut self,
        _action: &TogglePalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette_state.open {
            self.close_palette(window, cx);
            return;
        }
        self.palette_state.open = true;
        self.palette_state.query.clear();
        self.palette_state.selection = 0;
        self.palette_state.delete_confirm = false;
        self.palette_state.renaming = None;
        self.palette_state.scroll.set_offset(point(px(0.), px(0.)));

        self.ensure_palette_field(window, cx);
        self.ensure_palette_hooks(cx);

        if let Some(field) = self.palette_state.field.clone() {
            field.update(cx, |field, cx| field.reset(window, cx));
            field.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// Every keystroke, before action dispatch. Only list-navigation keys are
    /// consumed; everything else falls through to the TextField/keymap.
    fn on_palette_keystroke(
        &mut self,
        event: &KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.palette_state.open {
            return;
        }
        let key = event.keystroke.key.as_str();
        let modified = event.keystroke.modifiers.modified();

        // Rename mode: the input is the new title. Enter commits, Esc backs
        // out to the list instead of closing the whole palette.
        if self.palette_state.renaming.is_some() {
            match key {
                "escape" => {
                    self.palette_state.renaming = None;
                    self.reset_palette_input(window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }
                "enter" if !modified => {
                    self.commit_palette_rename(window, cx);
                    cx.stop_propagation();
                }
                _ => {}
            }
            return;
        }

        match key {
            "up" | "down" => {
                let count = self.palette_enabled_entries().len();
                if count > 0 {
                    let selection = self.palette_state.selection;
                    self.palette_state.selection = if key == "down" {
                        (selection + 1) % count
                    } else {
                        selection.checked_sub(1).unwrap_or(count - 1)
                    };
                    self.palette_state.delete_confirm = false;
                    cx.notify();
                }
                cx.stop_propagation();
            }
            "enter" if !modified => {
                self.run_palette_selection(window, cx);
                cx.stop_propagation();
            }
            // Close via close_palette (not lib.rs's CloseOverlay) so the input
            // is reset and focus returns to the composer.
            "escape" => {
                self.close_palette(window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn reset_palette_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_state.query.clear();
        if let Some(field) = self.palette_state.field.clone() {
            field.update(cx, |field, cx| field.reset(window, cx));
        }
    }

    /// Close the overlay: reset input state and give the composer focus back
    /// so keystrokes keep landing somewhere useful.
    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        {
            let state = &mut self.palette_state;
            state.open = false;
            state.query.clear();
            state.selection = 0;
            state.delete_confirm = false;
            state.renaming = None;
        }
        if let Some(field) = self.palette_state.field.clone() {
            field.update(cx, |field, cx| field.reset(window, cx));
        }
        self.composer.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// The flat list of result rows for the current query, threads first.
    fn palette_rows(&self) -> Vec<Row> {
        let query = self.palette_state.query.trim();
        let filtering = !query.is_empty();
        let mut rows = Vec::new();

        let mut threads: Vec<&crate::model::Thread> = self.state.threads.iter().collect();
        threads.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        let mut thread_count = 0usize;
        for thread in threads {
            let limit = if filtering {
                FILTERED_THREAD_LIMIT
            } else {
                RECENT_THREAD_LIMIT
            };
            if thread_count >= limit {
                break;
            }
            let project = self
                .state
                .projects
                .iter()
                .find(|p| p.id == thread.project_id)
                .map(|p| p.name.to_string())
                .unwrap_or_default();
            let haystack = format!("{} {}", thread.title, project);
            if filtering && !fuzzy_match(query, &haystack) {
                continue;
            }
            thread_count += 1;
            rows.push(Row {
                entry: Entry::Thread(thread.id.clone()),
                section: if filtering {
                    "Threads"
                } else {
                    "Recent threads"
                },
                label: thread.title.to_string(),
                detail: (!project.is_empty()).then_some(project),
                hint: None,
                enabled: true,
                danger: false,
            });
        }

        let selected = self.state.selected_thread();
        let has_thread = selected.is_some();
        let pinned = selected.is_some_and(|t| t.pinned);
        let running = selected.is_some_and(|t| t.running());
        let thread_title = selected.map(|t| t.title.to_string());

        // Commands that act on a thread always target `state.selected` (the
        // "current thread"), like the web palette's thread commands.
        let commands: [(CommandKind, &str, Option<&str>, Option<&str>, bool); 7] = [
            (
                CommandKind::NewThread,
                "New thread",
                None,
                Some("⌘N"),
                true,
            ),
            (
                CommandKind::OpenSettings,
                "Open settings",
                None,
                Some("⌘,"),
                true,
            ),
            // model::Thread drops archived threads on ingest, so "Unarchive"
            // is unreachable here — only the archive direction is listed.
            (
                CommandKind::Archive,
                "Archive thread",
                thread_title.as_deref(),
                None,
                has_thread,
            ),
            (
                CommandKind::Pin,
                if pinned { "Unpin thread" } else { "Pin thread" },
                thread_title.as_deref(),
                None,
                has_thread,
            ),
            (
                CommandKind::Rename,
                "Rename thread…",
                thread_title.as_deref(),
                None,
                has_thread,
            ),
            (
                CommandKind::Delete,
                "Delete thread",
                thread_title.as_deref(),
                None,
                has_thread,
            ),
            (
                CommandKind::Stop,
                "Stop turn",
                thread_title.as_deref(),
                None,
                running,
            ),
        ];

        for (kind, label, detail, hint, enabled) in commands {
            let keywords = match kind {
                CommandKind::NewThread => "create start newthread",
                CommandKind::OpenSettings => "preferences options config",
                CommandKind::Archive => "unarchive close hide",
                CommandKind::Pin => "unpin favorite star",
                CommandKind::Rename => "title edit retitle",
                CommandKind::Delete => "remove destroy trash",
                CommandKind::Stop => "interrupt cancel halt",
            };
            let haystack = format!("{label} {keywords}");
            if filtering && !fuzzy_match(query, &haystack) {
                continue;
            }
            let armed = kind == CommandKind::Delete && self.palette_state.delete_confirm;
            rows.push(Row {
                entry: Entry::Command(kind),
                section: "Commands",
                label: if armed {
                    "Confirm delete thread".to_string()
                } else {
                    label.to_string()
                },
                detail: if armed {
                    Some("Press Enter again to delete".to_string())
                } else {
                    detail.map(str::to_string)
                },
                hint,
                enabled,
                danger: armed,
            });
        }

        rows
    }

    /// Enabled entries in render order — the set Up/Down navigates over.
    fn palette_enabled_entries(&self) -> Vec<Entry> {
        self.palette_rows()
            .into_iter()
            .filter(|row| row.enabled)
            .map(|row| row.entry)
            .collect()
    }

    fn run_palette_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.palette_enabled_entries();
        let Some(entry) = entries.get(self.palette_state.selection).cloned() else {
            return;
        };
        self.execute_palette_entry(entry, window, cx);
    }

    /// Mouse click on row `ix` of the enabled list: highlight then execute.
    fn run_palette_index(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_state.selection = ix;
        self.run_palette_selection(window, cx);
    }

    fn execute_palette_entry(
        &mut self,
        entry: Entry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match entry {
            Entry::Thread(id) => {
                self.state.selected = Some(id.clone());
                self.thread_items.clear();
                if let Some(commands) = &self.commands {
                    let _ = commands.send(ClientCommand::SubscribeThread {
                        thread_id: id.to_string(),
                    });
                }
                self.close_palette(window, cx);
            }
            Entry::Command(CommandKind::NewThread) => {
                self.state.selected = None;
                self.thread_items.clear();
                self.close_palette(window, cx);
            }
            Entry::Command(CommandKind::OpenSettings) => {
                self.settings_state.open = true;
                self.close_palette(window, cx);
            }
            Entry::Command(CommandKind::Archive) => {
                if let Some(id) = self.state.selected.clone() {
                    self.state.threads.retain(|t| t.id != id);
                    self.dispatch(json!({
                        "type": "thread.archive",
                        "commandId": Uuid::new_v4().to_string(),
                        "threadId": id.to_string(),
                    }));
                }
                self.close_palette(window, cx);
            }
            Entry::Command(CommandKind::Pin) => {
                if let Some(id) = self.state.selected.clone() {
                    let pinned = self
                        .state
                        .threads
                        .iter()
                        .find(|t| t.id == id)
                        .is_some_and(|t| t.pinned);
                    if let Some(thread) = self.state.threads.iter_mut().find(|t| t.id == id) {
                        thread.pinned = !pinned;
                    }
                    self.dispatch(json!({
                        "type": if pinned { "thread.unpin" } else { "thread.pin" },
                        "commandId": Uuid::new_v4().to_string(),
                        "threadId": id.to_string(),
                    }));
                }
                self.close_palette(window, cx);
            }
            Entry::Command(CommandKind::Rename) => {
                if let Some(thread) = self.state.selected_thread() {
                    let (id, title) = (thread.id.clone(), thread.title.clone());
                    self.palette_state.renaming = Some(id);
                    self.palette_state.delete_confirm = false;
                    if let Some(field) = self.palette_state.field.clone() {
                        field.update(cx, |field, cx| {
                            field.reset(window, cx);
                            field.replace_text_in_range(None, &title, window, cx);
                        });
                    }
                    cx.notify();
                }
            }
            Entry::Command(CommandKind::Delete) => {
                if !self.palette_state.delete_confirm {
                    // First execution arms the row instead of deleting.
                    self.palette_state.delete_confirm = true;
                    cx.notify();
                    return;
                }
                if let Some(id) = self.state.selected.clone() {
                    self.state.threads.retain(|t| t.id != id);
                    if self.state.selected.as_ref() == Some(&id) {
                        self.state.selected = None;
                    }
                    self.dispatch(json!({
                        "type": "thread.delete",
                        "commandId": Uuid::new_v4().to_string(),
                        "threadId": id.to_string(),
                    }));
                }
                self.close_palette(window, cx);
            }
            Entry::Command(CommandKind::Stop) => {
                if let Some(thread) = self.state.selected_thread() {
                    self.dispatch(json!({
                        "type": "thread.stop",
                        "commandId": Uuid::new_v4().to_string(),
                        "threadId": thread.id.to_string(),
                    }));
                }
                self.close_palette(window, cx);
            }
        }
    }

    /// Enter pressed while `renaming` is set: dispatch `thread.metadata.update`
    /// with the input text as the new title.
    fn commit_palette_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.palette_state.renaming.take() else {
            return;
        };
        let title = self
            .palette_state
            .field
            .clone()
            .map(|field| {
                field.update(cx, |field, cx| {
                    field
                        .text_for_range(0..usize::MAX, &mut None, window, cx)
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default()
            .trim()
            .to_string();
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
        self.close_palette(window, cx);
    }

    fn render_palette_row(
        &self,
        row: &Row,
        dom_id: usize,
        enabled_index: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = enabled_index == Some(self.palette_state.selection);
        let mut el = div()
            .id(("palette-row", dom_id))
            .mx_1()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded_md()
            .text_sm();
        if active {
            el = el.bg(theme::selected());
        }
        if row.enabled {
            el = el
                .cursor_pointer()
                .when(!active, |el| el.hover(|s| s.bg(theme::hover())));
        } else {
            el = el.opacity(0.45);
        }
        if let Some(ix) = enabled_index {
            el = el
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, window, cx| {
                        this.run_palette_index(ix, window, cx);
                    }),
                )
                .on_mouse_move(cx.listener(move |this, _ev, _window, cx| {
                    if this.palette_state.selection != ix {
                        this.palette_state.selection = ix;
                        this.palette_state.delete_confirm = false;
                        cx.notify();
                    }
                }));
        }
        el.child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_nowrap()
                .overflow_x_hidden()
                .text_ellipsis()
                .text_color(if row.danger {
                    gpui::rgb(0xf87171)
                } else if row.enabled {
                    theme::fg()
                } else {
                    theme::fg_muted()
                })
                .child(SharedString::from(row.label.clone())),
        )
        .when_some(row.detail.clone(), |el, detail| {
            el.child(
                div()
                    .flex_none()
                    .max_w(px(200.))
                    .truncate()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(detail),
            )
        })
        .when_some(row.hint, |el, hint| {
            el.child(
                div()
                    .flex_none()
                    .px_1p5()
                    .py_0p5()
                    .rounded_sm()
                    .border_1()
                    .border_color(theme::border())
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(hint),
            )
        })
    }

    pub fn render_palette(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.palette_state.open {
            return div().id("palette-closed");
        }

        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0();

        if self.palette_state.renaming.is_some() {
            body = body.child(
                div()
                    .px_3()
                    .py_3()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Type a new title — Enter to save · Esc to cancel"),
            );
        } else {
            let rows = self.palette_rows();
            let mut list = div()
                .id("palette-results")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.palette_state.scroll)
                .py_1();

            let mut child_ix = 0usize;
            let mut enabled_ix = 0usize;
            let mut highlighted_child = None;
            let mut last_section = "";
            for row in &rows {
                if row.section != last_section {
                    last_section = row.section;
                    list = list.child(
                        div()
                            .px_3()
                            .pt_2()
                            .pb_1()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::fg_muted())
                            .child(row.section),
                    );
                    child_ix += 1;
                }
                let enabled_index = if row.enabled {
                    let ix = enabled_ix;
                    enabled_ix += 1;
                    Some(ix)
                } else {
                    None
                };
                if enabled_index == Some(self.palette_state.selection) {
                    highlighted_child = Some(child_ix);
                }
                list = list.child(self.render_palette_row(row, child_ix, enabled_index, cx));
                child_ix += 1;
            }
            if rows.is_empty() {
                list = list.child(
                    div()
                        .px_3()
                        .py_3()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child("No results"),
                );
            }
            if let Some(ix) = highlighted_child {
                self.palette_state.scroll.scroll_to_item(ix);
            }
            body = body.child(list);
        }

        let mut panel = div()
            .id("palette-panel")
            .w(px(560.))
            .mt(px(80.))
            .flex()
            .flex_col()
            .rounded_lg()
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border())
            .max_h(px(440.))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                cx.stop_propagation();
            });

        if let Some(field) = self.palette_state.field.clone() {
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme::border())
                    .text_sm()
                    .child(div().flex_1().min_w_0().text_color(theme::fg()).child(field)),
            );
        }

        panel = panel.child(body).child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(theme::border())
                .text_xs()
                .text_color(theme::fg_muted())
                .child("↑↓ navigate")
                .child("↵ select")
                .child("esc close"),
        );

        // Full-window dimmed backdrop; a click outside the panel dismisses.
        div()
            .id("palette-overlay")
            .absolute()
            .inset_0()
            .bg(gpui::black().opacity(0.5))
            .flex()
            .justify_center()
            .items_start()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| {
                    this.close_palette(window, cx);
                }),
            )
            .child(panel)
    }
}
