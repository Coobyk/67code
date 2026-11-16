//! Project content-search overlay dialog (ctrl-shift-f).
//!
//! Mirrors apps/web `ProjectContentSearchDialog`: a modal with a query input,
//! debounced `projects.searchContents` RPC scoped to the selected thread's
//! project, results grouped per file with matched ranges rendered bold, and
//! Up/Down/Enter/Esc keyboard navigation. A "Files" mode switches the same
//! dialog to `projects.searchEntries` (path search).
//!
//! `on_toggle_search` / `on_close_overlay` in lib.rs only flip `open`, so
//! field creation, focus, and teardown happen here on the open edge detected
//! in render — deferred out of render via `cx.spawn` so the window is off the
//! update stack (same trick as `field_text` below, borrowed from palette.rs).
//!
//! INTEGRATE: activating a match should reveal the file in the explorer pane
//! at the hit's line number (explorer_state is owned by explorer.rs — expose
//! a `reveal(path, line)` method). Until then we open the Files tab and toast
//! the path.

use std::time::Duration;

use gpui::{
    Context, Entity, EntityInputHandler, Focusable, KeystrokeEvent, MouseButton, SharedString,
    Subscription, Window, div, point, prelude::*, px,
};
use serde_json::{Value, json};

use crate::AppView;
use crate::text_field::{PromptSubmitted, TextField};
use crate::theme;

/// Query → RPC delay. The web client uses 120ms; a slightly longer window
/// keeps a single in-flight `rg` scan per pause on large roots.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// `PROJECT_CONTENT_SEARCH_LIMIT` in the web client (server clamps at 500).
const CONTENT_LIMIT: u64 = 500;
/// `projects.searchEntries` limit (server clamps at 200).
const ENTRIES_LIMIT: u64 = 200;

/// The three match-option toggles (the web dialog's `SearchOptionButton`s).
#[derive(Clone, Copy)]
enum SearchOption {
    Case,
    Word,
    Regex,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `projects.searchContents` — line matches grouped per file.
    Contents,
    /// `projects.searchEntries` — file-path search.
    Files,
}

/// One flattened, selectable result. `line_number == None` marks a Files-mode
/// path hit; `ranges` are the contract's UTF-16 offsets into `line`.
#[derive(Clone)]
struct Hit {
    path: String,
    line_number: Option<u64>,
    line: String,
    ranges: Vec<(u64, u64)>,
}

pub struct State {
    /// Whether the project content-search dialog is open.
    pub open: bool,
    /// Query input, created lazily on first open — `cx.new` needs a context.
    field: Option<Entity<TextField>>,
    /// Mirror of `field`'s contents (render takes `&self` semantics for reads
    /// that don't have a window on the update stack).
    query: String,
    mode: Mode,
    case_sensitive: bool,
    whole_word: bool,
    use_regex: bool,
    results: Vec<Hit>,
    truncated: bool,
    /// An RPC is in flight (or the debounce timer is armed) for `query`.
    searching: bool,
    /// A reply (or error) landed for the current query — distinguishes the
    /// idle hint from "No results".
    searched: bool,
    error: Option<String>,
    /// Server's `regexFallbackError` — invalid regex, matched literally.
    regex_fallback: Option<String>,
    /// Highlighted index in the flat `results` list.
    selection: usize,
    /// Bumped per keystroke; stale debounce timers and replies check it.
    generation: u64,
    /// Limit used for the active result set (for the truncated footer).
    result_limit: u64,
    scroll: gpui::ScrollHandle,
    subs: Vec<Subscription>,
    hooks_installed: bool,
    /// Previous render's `open` — detects the open/close edge.
    was_open: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            field: None,
            query: String::new(),
            mode: Mode::Contents,
            case_sensitive: false,
            whole_word: false,
            use_regex: false,
            results: Vec::new(),
            truncated: false,
            searching: false,
            searched: false,
            error: None,
            regex_fallback: None,
            selection: 0,
            generation: 0,
            result_limit: CONTENT_LIMIT,
            scroll: gpui::ScrollHandle::new(),
            subs: Vec::new(),
            hooks_installed: false,
            was_open: false,
        }
    }
}

/// Read a text field's contents when no `&mut Window` is on the update stack
/// (entity observers run with `&mut App` only). Same approach palette.rs uses.
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

/// A readable message out of a Failure `Exit` frame's cause.
fn failure_message(exit: &Value) -> String {
    let cause = &exit["cause"];
    for candidate in [
        &cause["message"],
        &cause["error"]["message"],
        &cause["defect"],
        &cause["reason"],
        cause,
    ] {
        if let Some(s) = candidate.as_str() {
            return s.to_string();
        }
    }
    cause.to_string().chars().take(160).collect()
}

fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(ix) => (&path[ix + 1..], &path[..ix]),
        None => (path, ""),
    }
}

/// Split `line` into (text, is_match) runs for `matchRanges`, which the
/// contract encodes as UTF-16 code-unit offsets (JS string indices). Mirrors
/// the web `normalizeRanges`: clamp, drop empties, sort, merge overlaps —
/// then map back to char boundaries (never byte offsets).
fn line_segments(line: &str, ranges: &[(u64, u64)]) -> Vec<(String, bool)> {
    let chars: Vec<char> = line.chars().collect();
    let mut utf16 = Vec::with_capacity(chars.len() + 1);
    utf16.push(0usize);
    for c in &chars {
        utf16.push(utf16[utf16.len() - 1] + c.len_utf16());
    }
    let total = *utf16.last().unwrap_or(&0);

    let mut sorted: Vec<(usize, usize)> = ranges
        .iter()
        .map(|&(s, e)| {
            let (s, e) = (s.min(e) as usize, s.max(e) as usize);
            (s.min(total), e.min(total))
        })
        .filter(|&(s, e)| e > s)
        .collect();
    sorted.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in sorted {
        if let Some(prev) = merged.last_mut()
            && s <= prev.1
        {
            prev.1 = prev.1.max(e);
            continue;
        }
        merged.push((s, e));
    }

    // UTF-16 offset → char index boundary. `utf16` is one prefix per char
    // boundary; an offset inside a surrogate pair rounds toward including
    // the char (start floors, end ceilings).
    let to_char = |off: usize, ceil: bool| -> usize {
        match utf16.binary_search(&off) {
            Ok(i) => i,
            Err(i) => {
                if ceil { i } else { i.saturating_sub(1) }
            }
        }
    };

    let mut out: Vec<(String, bool)> = Vec::new();
    let mut cursor = 0usize;
    for (s, e) in merged {
        let cs = to_char(s, false);
        let ce = to_char(e, true);
        if cs > cursor {
            out.push((chars[cursor..cs].iter().collect(), false));
        }
        if ce > cs {
            out.push((chars[cs..ce].iter().collect(), true));
            cursor = ce;
        }
    }
    if cursor < chars.len() {
        out.push((chars[cursor..].iter().collect(), false));
    }
    out
}

impl AppView {
    /// Selected thread's project → (workspace root, project name). `None`
    /// means there is nothing to search (no thread, or project missing).
    fn search_target(&self) -> Option<(String, String)> {
        let thread = self.state.selected_thread()?;
        let project = self
            .state
            .projects
            .iter()
            .find(|p| p.id == thread.project_id)?;
        Some((project.path.to_string(), project.name.to_string()))
    }

    /// Lazily build the query field + subscriptions. Called from
    /// `search_opened`, which runs deferred so `cx.new` and window access are
    /// safe.
    fn ensure_search_field(&mut self, cx: &mut Context<Self>) {
        if self.search_state.field.is_some() {
            return;
        }
        let placeholder = match self.search_target() {
            Some((_, name)) => format!("Search in {name}"),
            None => "Search this project's files".to_string(),
        };
        let field = cx.new(|cx| TextField::new(placeholder, cx));

        // Keep the plain-string query mirror in sync so render can draw the
        // status row; a changed query re-arms the debounce.
        self.search_state.subs.push(cx.observe(
            &field,
            |this, field, cx| {
                let query = field_text(&field, cx);
                let changed = {
                    let s = &mut this.search_state;
                    if s.query == query {
                        false
                    } else {
                        s.query = query;
                        s.selection = 0;
                        s.searched = false;
                        s.regex_fallback = None;
                        true
                    }
                };
                if changed {
                    this.schedule_search(cx);
                    cx.notify();
                }
            },
        ));

        // Enter is consumed by the keystroke interceptor before the field's
        // Submit action can run; this covers any path that emits
        // `PromptSubmitted` directly.
        self.search_state.subs.push(cx.subscribe(
            &field,
            |this, _field, _event: &PromptSubmitted, cx| {
                this.search_activate(cx);
            },
        ));

        self.search_state.field = Some(field);
    }

    /// Install the app-wide keystroke hook once; it runs before keymap/action
    /// dispatch, which is what lets Up/Down/Enter drive the list while the
    /// TextField owns focus. Escape is left to the shared CloseOverlay
    /// binding in lib.rs.
    fn ensure_search_hooks(&mut self, cx: &mut Context<Self>) {
        if self.search_state.hooks_installed {
            return;
        }
        self.search_state.hooks_installed = true;
        let subscription = cx.intercept_keystrokes(|event, window, cx| {
            let Some(view) = app_view(window) else {
                return;
            };
            view.update(cx, |this, cx| this.on_search_keystroke(event, cx));
        });
        self.search_state.subs.push(subscription);
    }

    /// Deferred open-edge handler: reset state, build field/hooks, focus.
    fn search_opened(&mut self, cx: &mut Context<Self>) {
        {
            let s = &mut self.search_state;
            s.query.clear();
            s.results.clear();
            s.truncated = false;
            s.searching = false;
            s.searched = false;
            s.error = None;
            s.regex_fallback = None;
            s.selection = 0;
            s.scroll.set_offset(point(px(0.), px(0.)));
        }
        self.ensure_search_field(cx);
        self.ensure_search_hooks(cx);
        if let Some(field) = self.search_state.field.clone() {
            let view_id = cx.entity_id();
            let focused = cx
                .with_window(view_id, |window, app| {
                    field.update(app, |field, cx| field.reset(window, cx));
                    field.read(app).focus_handle(app).focus(window, app);
                })
                .is_some();
            if !focused {
                for handle in cx.windows() {
                    if handle
                        .update(cx, |_root, window, app| {
                            field.update(app, |field, cx| field.reset(window, cx));
                            field.read(app).focus_handle(app).focus(window, app);
                        })
                        .is_ok()
                    {
                        break;
                    }
                }
            }
        }
        cx.notify();
    }

    /// Deferred close-edge handler: hand focus back to the composer so
    /// keystrokes keep landing somewhere useful.
    fn search_closed(&mut self, cx: &mut Context<Self>) {
        let composer = self.composer.clone();
        let view_id = cx.entity_id();
        let focused = cx
            .with_window(view_id, |window, app| {
                composer.read(app).focus_handle(app).focus(window, app);
            })
            .is_some();
        if !focused {
            for handle in cx.windows() {
                let _ = handle.update(cx, |_root, window, app| {
                    composer.read(app).focus_handle(app).focus(window, app);
                });
            }
        }
        cx.notify();
    }

    /// Every keystroke, before action dispatch. Only list keys are consumed;
    /// everything else (including Escape → CloseOverlay) falls through.
    fn on_search_keystroke(&mut self, event: &KeystrokeEvent, cx: &mut Context<Self>) {
        if !self.search_state.open {
            return;
        }
        let key = event.keystroke.key.as_str();
        let modified = event.keystroke.modifiers.modified();
        match key {
            "up" | "down" => {
                let count = self.search_state.results.len();
                if count > 0 {
                    let selection = self.search_state.selection;
                    self.search_state.selection = if key == "down" {
                        (selection + 1) % count
                    } else {
                        selection.checked_sub(1).unwrap_or(count - 1)
                    };
                    cx.notify();
                }
                cx.stop_propagation();
            }
            "enter" if !modified => {
                self.search_activate(cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    /// Arm the debounce; when it fires and the generation still matches,
    /// `run_search` issues the RPC.
    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.search_state.generation = self.search_state.generation.wrapping_add(1);
        self.search_state.searching = !self.search_state.query.trim().is_empty();
        let generation = self.search_state.generation;
        cx.spawn(async move |this, cx| {
            smol::Timer::after(DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                if this.search_state.open && this.search_state.generation == generation {
                    this.run_search(generation, cx);
                }
            });
        })
        .detach();
    }

    /// Issue `projects.searchContents` / `projects.searchEntries` for the
    /// current query + options, scoped to the selected thread's project root.
    fn run_search(&mut self, generation: u64, cx: &mut Context<Self>) {
        let query = self.search_state.query.clone();
        if query.trim().is_empty() {
            let s = &mut self.search_state;
            s.results.clear();
            s.truncated = false;
            s.searching = false;
            s.searched = false;
            s.error = None;
            s.regex_fallback = None;
            cx.notify();
            return;
        }
        let Some((cwd, _name)) = self.search_target() else {
            return;
        };
        let (tag, payload) = match self.search_state.mode {
            Mode::Contents => (
                "projects.searchContents",
                json!({
                    "cwd": cwd,
                    "query": query,
                    "limit": CONTENT_LIMIT,
                    "caseSensitive": self.search_state.case_sensitive,
                    "wholeWord": self.search_state.whole_word,
                    "useRegex": self.search_state.use_regex,
                }),
            ),
            Mode::Files => (
                "projects.searchEntries",
                json!({
                    "cwd": cwd,
                    "query": query,
                    "limit": ENTRIES_LIMIT,
                    "kind": "file",
                }),
            ),
        };
        let Some(rx) = self.send_query(tag, payload) else {
            let s = &mut self.search_state;
            s.searching = false;
            s.searched = true;
            s.error = Some("Not connected to a server.".to_string());
            cx.notify();
            return;
        };
        self.search_state.searching = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let _ = this.update(cx, |this, cx| {
                this.apply_search_reply(generation, exit, cx);
            });
        })
        .detach();
    }

    /// Fold a search `Exit` frame into state; a stale generation (newer query
    /// typed mid-flight) drops the reply untouched.
    fn apply_search_reply(&mut self, generation: u64, exit: Value, cx: &mut Context<Self>) {
        if self.search_state.generation != generation {
            return;
        }
        let s = &mut self.search_state;
        s.searching = false;
        s.searched = true;
        s.selection = 0;
        if exit["_tag"].as_str() == Some("Success") {
            let value = &exit["value"];
            s.error = None;
            s.truncated = value["truncated"].as_bool() == Some(true);
            s.regex_fallback = value["regexFallbackError"]
                .as_str()
                .map(str::to_string);
            s.result_limit = match s.mode {
                Mode::Contents => CONTENT_LIMIT,
                Mode::Files => ENTRIES_LIMIT,
            };
            s.results = match s.mode {
                Mode::Contents => value["matches"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|m| Hit {
                                path: m["path"].as_str().unwrap_or_default().to_string(),
                                line_number: m["lineNumber"].as_u64(),
                                line: m["lineContent"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                                ranges: m["matchRanges"]
                                    .as_array()
                                    .map(|rs| {
                                        rs.iter()
                                            .filter_map(|r| {
                                                Some((
                                                    r["start"].as_u64()?,
                                                    r["end"].as_u64()?,
                                                ))
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                Mode::Files => value["entries"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|e| Hit {
                                path: e["path"].as_str().unwrap_or_default().to_string(),
                                line_number: None,
                                line: String::new(),
                                ranges: Vec::new(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            };
        } else {
            s.results.clear();
            s.truncated = false;
            s.regex_fallback = None;
            s.error = Some(failure_message(&exit));
        }
        cx.notify();
    }

    /// Enter / click: open the highlighted hit. Until the explorer exposes a
    /// reveal method, this opens the Files tab and toasts the target.
    fn search_activate(&mut self, cx: &mut Context<Self>) {
        let Some(hit) = self
            .search_state
            .results
            .get(self.search_state.selection)
            .cloned()
        else {
            return;
        };
        self.search_state.open = false;
        self.panel_state.tab = Some(crate::right_panel::Tab::Files);
        // Explorer reveal lands the file in the tree + preview; the hit's
        // line number is not yet scrolled to (preview has no line anchors).
        self.explorer_reveal(hit.path.clone(), cx);
        cx.notify();
    }

    fn search_activate_index(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.search_state.selection = ix;
        self.search_activate(cx);
    }

    fn set_search_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.search_state.mode == mode {
            return;
        }
        let s = &mut self.search_state;
        s.mode = mode;
        s.results.clear();
        s.truncated = false;
        s.error = None;
        s.regex_fallback = None;
        s.searched = false;
        s.selection = 0;
        self.schedule_search(cx);
        cx.notify();
    }

    /// Groups per file in first-seen order (web `groupMatches`); the flat
    /// `results` order is what Up/Down navigates.
    fn search_groups(&self) -> Vec<(String, Vec<usize>)> {
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for (ix, hit) in self.search_state.results.iter().enumerate() {
            match groups.iter_mut().find(|(p, _)| *p == hit.path) {
                Some((_, hits)) => hits.push(ix),
                None => groups.push((hit.path.clone(), vec![ix])),
            }
        }
        groups
    }

    /// One search-option toggle chip (Aa / ab / .*) — the web dialog's
    /// `SearchOptionButton`.
    fn render_search_option(
        &self,
        option: SearchOption,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (id, label, active) = match option {
            SearchOption::Case => (
                "search-opt-case",
                "Aa",
                self.search_state.case_sensitive,
            ),
            SearchOption::Word => ("search-opt-word", "ab", self.search_state.whole_word),
            SearchOption::Regex => ("search-opt-regex", ".*", self.search_state.use_regex),
        };
        let mut el = div()
            .id(id)
            .px_1p5()
            .py_0p5()
            .rounded_sm()
            .cursor_pointer()
            .text_xs()
            .font_family("monospace")
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _w, cx| {
                    let s = &mut this.search_state;
                    match option {
                        SearchOption::Case => s.case_sensitive = !s.case_sensitive,
                        SearchOption::Word => s.whole_word = !s.whole_word,
                        SearchOption::Regex => s.use_regex = !s.use_regex,
                    }
                    this.schedule_search(cx);
                    cx.notify();
                }),
            );
        if active {
            el = el.bg(theme::selected()).text_color(theme::fg());
        } else {
            el = el
                .text_color(theme::fg_muted())
                .hover(|s| s.bg(theme::hover()));
        }
        el
    }

    fn render_search_mode_chip(
        &self,
        mode: Mode,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.search_state.mode == mode;
        let mut el = div()
            .id(format!("search-mode-{label}"))
            .px_1p5()
            .py_0p5()
            .rounded_sm()
            .cursor_pointer()
            .text_xs()
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _w, cx| {
                    this.set_search_mode(mode, cx);
                }),
            );
        if active {
            el = el.bg(theme::selected()).text_color(theme::fg());
        } else {
            el = el
                .text_color(theme::fg_muted())
                .hover(|s| s.bg(theme::hover()));
        }
        el
    }

    /// One content-match row: line number + line content with matched ranges
    /// rendered bold over a faint accent highlight (the web `<mark>`).
    fn render_search_hit(
        &self,
        hit: &Hit,
        ix: usize,
        dom_ix: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.search_state.selection == ix;
        let mut line_row = div()
            .flex()
            .items_center()
            .gap_3()
            .min_w_0()
            .whitespace_nowrap()
            .font_family("monospace");
        for (text, matched) in line_segments(&hit.line, &hit.ranges) {
            if matched {
                line_row = line_row.child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .bg(theme::primary().opacity(0.25))
                        .rounded_sm()
                        .child(SharedString::from(text)),
                );
            } else {
                line_row = line_row.child(div().child(SharedString::from(text)));
            }
        }

        let mut el = div()
            .id(("search-row", dom_ix))
            .mx_1()
            .flex()
            .items_center()
            .gap_3()
            .px_2()
            .h(px(28.))
            .rounded_md()
            .text_xs()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _w, cx| {
                    this.search_activate_index(ix, cx);
                }),
            )
            .on_mouse_move(cx.listener(move |this, _ev, _w, cx| {
                if this.search_state.selection != ix {
                    this.search_state.selection = ix;
                    cx.notify();
                }
            }));
        if active {
            el = el.bg(theme::selected());
        } else {
            el = el.hover(|s| s.bg(theme::hover()));
        }
        el.child(
            div()
                .flex_none()
                .w(px(40.))
                .text_right()
                .text_color(theme::fg_muted())
                .child(SharedString::from(
                    hit.line_number.map(|n| n.to_string()).unwrap_or_default(),
                )),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_x_hidden()
                .text_color(theme::fg())
                .child(line_row),
        )
    }

    /// One Files-mode row: file name + directory, path-search style.
    fn render_search_file_hit(
        &self,
        hit: &Hit,
        ix: usize,
        dom_ix: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.search_state.selection == ix;
        let (name, dir) = split_path(&hit.path);
        let mut el = div()
            .id(("search-row", dom_ix))
            .mx_1()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .h(px(28.))
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _w, cx| {
                    this.search_activate_index(ix, cx);
                }),
            )
            .on_mouse_move(cx.listener(move |this, _ev, _w, cx| {
                if this.search_state.selection != ix {
                    this.search_state.selection = ix;
                    cx.notify();
                }
            }));
        if active {
            el = el.bg(theme::selected());
        } else {
            el = el.hover(|s| s.bg(theme::hover()));
        }
        el.child(
            div()
                .flex_none()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::fg())
                .child(SharedString::from(name.to_string())),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(theme::fg_muted())
                .child(SharedString::from(dir.to_string())),
        )
    }

    pub(crate) fn render_search_dialog(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        // lib.rs's toggle/close handlers only flip `open`; do the real
        // open/close work deferred so entity creation and window focus are
        // off the render path.
        let open = self.search_state.open;
        if self.search_state.was_open != open {
            self.search_state.was_open = open;
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.search_state.open {
                        this.search_opened(cx);
                    } else {
                        this.search_closed(cx);
                    }
                });
            })
            .detach();
        }
        if !open {
            return div().id("search-closed");
        }

        let has_target = self.search_target().is_some();
        let has_query = !self.search_state.query.trim().is_empty();
        let groups = self.search_groups();
        let file_count = groups.len();
        let result_count = self.search_state.results.len();
        let contents_mode = self.search_state.mode == Mode::Contents;

        // Status row between input and results (web `showSearchStatus`).
        let status: Option<(String, bool /* is_error */)> = if !has_target {
            None
        } else if let Some(err) = self.search_state.error.clone() {
            Some((err, true))
        } else if self.search_state.searching {
            Some(("Searching…".to_string(), false))
        } else if has_query && result_count > 0 {
            let label = match self.search_state.mode {
                Mode::Contents => format!(
                    "{result_count}{} results in {file_count} files",
                    if self.search_state.truncated { "+" } else { "" }
                ),
                Mode::Files => format!(
                    "{result_count}{} files",
                    if self.search_state.truncated { "+" } else { "" }
                ),
            };
            Some((label, false))
        } else {
            None
        };

        let mut body = div().flex().flex_col().flex_1().min_h_0();

        if !has_target {
            body = body.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_6()
                    .py_8()
                    .text_sm()
                    .text_color(theme::fg_muted())
                    .child("Open a project to search its files."),
            );
        } else if !has_query {
            body = body.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_6()
                    .py_8()
                    .text_sm()
                    .text_color(theme::fg_muted())
                    .child("Search this project's files"),
            );
        } else if self.search_state.searching && result_count == 0 {
            body = body.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_6()
                    .py_8()
                    .text_sm()
                    .text_color(theme::fg_muted())
                    .child("Searching…"),
            );
        } else if result_count == 0 {
            let (text, is_error) = match &self.search_state.error {
                Some(err) => (err.clone(), true),
                None => ("No results".to_string(), false),
            };
            let mut el = div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px_6()
                .py_8()
                .text_sm();
            el = if is_error {
                el.text_color(gpui::rgb(0xf87171))
            } else {
                el.text_color(theme::fg_muted())
            };
            body = body.child(el.child(text));
        } else {
            let mut list = div()
                .id("search-results")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.search_state.scroll)
                .py_1();

            let mut child_ix = 0usize;
            let mut highlighted_child = None;
            for (path, hit_ixs) in &groups {
                if contents_mode {
                    let (name, dir) = split_path(path);
                    list = list.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .pt_2()
                            .pb_1()
                            .text_xs()
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme::fg())
                                    .child(SharedString::from(name.to_string())),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme::fg_muted())
                                    .child(SharedString::from(dir.to_string())),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .px_1p5()
                                    .py_0p5()
                                    .rounded_full()
                                    .bg(theme::hover())
                                    .text_color(theme::fg_muted())
                                    .child(SharedString::from(hit_ixs.len().to_string())),
                            ),
                    );
                    child_ix += 1;
                }
                for &ix in hit_ixs {
                    let hit = &self.search_state.results[ix];
                    if ix == self.search_state.selection {
                        highlighted_child = Some(child_ix);
                    }
                    if contents_mode {
                        list = list.child(self.render_search_hit(hit, ix, child_ix, cx));
                    } else {
                        list = list.child(self.render_search_file_hit(hit, ix, child_ix, cx));
                    }
                    child_ix += 1;
                }
            }
            if self.search_state.truncated {
                list = list.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(format!(
                            "+ more results — showing the first {}",
                            self.search_state.result_limit
                        )),
                );
            }
            if let Some(ix) = highlighted_child {
                self.search_state.scroll.scroll_to_item(ix);
            }
            body = body.child(list);
        }

        let mut panel = div()
            .id("search-panel")
            .w(px(620.))
            .mt(px(80.))
            .flex()
            .flex_col()
            .rounded_lg()
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border())
            .max_h(px(520.))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                cx.stop_propagation();
            });

        // Query input row: field + (mode chips | search-option toggles).
        if let Some(field) = self.search_state.field.clone() {
            let mut accessories = div().flex_none().flex().items_center().gap_1();
            if contents_mode {
                accessories = accessories
                    .child(self.render_search_option(SearchOption::Case, cx))
                    .child(self.render_search_option(SearchOption::Word, cx))
                    .child(self.render_search_option(SearchOption::Regex, cx));
            }
            accessories = accessories
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_0p5()
                        .ml_2()
                        .child(self.render_search_mode_chip(Mode::Contents, "Contents", cx))
                        .child(self.render_search_mode_chip(Mode::Files, "Files", cx)),
                );
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme::border())
                    .text_sm()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(theme::fg())
                            .child(field),
                    )
                    .child(accessories),
            );
        }

        // `regexFallbackError` — invalid regex matched literally; muted.
        if let Some(msg) = self.search_state.regex_fallback.clone() {
            panel = panel.child(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(format!("Invalid regex — matched literally: {msg}")),
            );
        }

        if let Some((text, is_error)) = status {
            let mut el = div()
                .h(px(36.))
                .flex_none()
                .flex()
                .items_center()
                .px_3()
                .border_b_1()
                .border_color(theme::border())
                .text_xs();
            el = if is_error {
                el.text_color(gpui::rgb(0xf87171))
            } else {
                el.text_color(theme::fg_muted())
            };
            panel = panel.child(el.child(text));
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
                .child("↵ open file")
                .child("esc close"),
        );

        div()
            .id("search-overlay")
            .absolute()
            .inset_0()
            .bg(gpui::black().opacity(0.5))
            .flex()
            .justify_center()
            .items_start()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, _w, cx| {
                    this.search_state.open = false;
                    cx.notify();
                }),
            )
            .child(panel)
    }
}
