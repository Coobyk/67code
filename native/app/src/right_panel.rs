//! Right-side inspector panel hosting tabbed panes (files, terminals, PRs,
//! local-diff review, agent browser previews).
//! Owns `impl AppView` panel render + `right_panel::State`.
//!
//! The Browser tab is the honest version of an agent-browser surface: it
//! lists `preview.list` sessions for the selected thread (URL + nav status)
//! and offers "Open in browser" via `xdg-open`. There is no embedded
//! webview — the GPUI client doesn't pretend to render pages.

use gpui::{Context, Div, MouseButton, div, prelude::*, px};
use serde_json::{Value, json};

use crate::AppView;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Files,
    Terminals,
    PullRequests,
    /// `review.getDiffPreview` viewer (vcs.rs / pr_review.rs).
    Diff,
    /// `preview.list` sessions for the selected thread.
    Browser,
}

/// One `PreviewSessionSnapshot` row (packages/contracts preview.ts) reduced
/// to what the pane displays.
pub struct PreviewSession {
    pub tab_id: String,
    /// `navStatus._tag`: Idle / Loading / Success / LoadFailed.
    pub status: String,
    /// Current URL — present on every navStatus except Idle.
    pub url: Option<String>,
    /// Page title when the nav status carried one.
    pub title: Option<String>,
    /// Server runs this tab in headless Chromium.
    pub headless: bool,
    /// `runtime` — "desktop" or "server".
    pub runtime: Option<String>,
}

/// `preview.list` state for the Browser tab.
#[derive(Default)]
pub struct BrowserState {
    /// Thread the last query was issued for; a selection change refetches.
    pub thread_id: Option<String>,
    /// Sessions from the latest successful list (None until first reply).
    pub sessions: Option<Vec<PreviewSession>>,
    /// A `preview.list` request is in flight.
    pub loading: bool,
    /// Last list error (unsupported/unauthorized server lands here too).
    pub error: Option<String>,
    /// Bumps per issued query; replies from superseded queries are dropped.
    generation: u64,
}

#[derive(Default)]
pub struct State {
    /// Which tab the panel shows, or None when the panel is closed.
    pub tab: Option<Tab>,
    /// Browser-preview pane (preview.list).
    pub browser: BrowserState,
}

/// Reduce a `PreviewSessionSnapshot` to a display row.
fn parse_preview_session(v: &Value) -> PreviewSession {
    let nav = &v["navStatus"];
    PreviewSession {
        tab_id: v["tabId"].as_str().unwrap_or_default().to_string(),
        status: nav["_tag"].as_str().unwrap_or("Idle").to_string(),
        url: nav["url"].as_str().map(str::to_owned),
        title: nav["title"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(str::to_owned),
        headless: v["headless"].as_bool() == Some(true),
        runtime: v["runtime"].as_str().map(str::to_owned),
    }
}

fn status_label(status: &str) -> &'static str {
    match status {
        "Loading" => "loading…",
        "Success" => "open",
        "LoadFailed" => "load failed",
        _ => "idle",
    }
}

impl AppView {
    pub(crate) fn render_right_panel(&self, cx: &mut Context<Self>) -> Div {
        let Some(tab) = self.panel_state.tab else {
            return div();
        };
        let body = match tab {
            Tab::Files => self.render_explorer_pane(cx).into_any_element(),
            Tab::Terminals => self.render_terminal_pane(cx).into_any_element(),
            Tab::PullRequests => self.render_prs_pane(cx).into_any_element(),
            Tab::Diff => self.render_diff_pane(cx).into_any_element(),
            Tab::Browser => self.render_browser_pane(cx).into_any_element(),
        };
        div()
            .w(px(380.))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(theme::border())
            .bg(theme::canvas())
            .flex()
            .flex_col()
            .child(self.render_panel_tab_bar(cx))
            .child(body)
    }

    /// Web `RightPanelTabs`: the strip sits at `--workspace-topbar-height`
    /// (52px) with compact `h-6 rounded-md` pills — active `bg-accent
    /// text-foreground`, inactive muted + `hover:bg-accent/60`.
    fn render_panel_tab_bar(&self, cx: &mut Context<Self>) -> Div {
        let tab = |label: &'static str, target: Tab, cx: &mut Context<Self>| {
            let active = self.panel_state.tab == Some(target);
            div()
                .h(px(24.))
                .flex()
                .items_center()
                .pl_1p5()
                .pr_2()
                .rounded(theme::radius::MD)
                .cursor_pointer()
                .text_xs()
                .text_color(if active {
                    theme::fg()
                } else {
                    theme::fg_muted()
                })
                .when(active, |s| s.bg(theme::accent()))
                .hover(move |s| {
                    if active {
                        s
                    } else {
                        s.bg(theme::accent_soft()).text_color(theme::fg())
                    }
                })
                .child(label)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _ev, _w, cx| {
                        this.panel_state.tab = Some(target);
                        cx.notify();
                    }),
                )
        };
        div()
            .h(px(theme::TOPBAR_H))
            .pl_2()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(tab("Files", Tab::Files, cx))
            .child(tab("Terminals", Tab::Terminals, cx))
            .child(tab("Pull requests", Tab::PullRequests, cx))
            .child(tab("Diff", Tab::Diff, cx))
            .child(tab("Browser", Tab::Browser, cx))
    }

    // ------------------------------------------------------------------
    // Browser tab — `preview.list` sessions for the selected thread
    // ------------------------------------------------------------------

    /// Deferred reconcile (render is `&self`): fetch sessions when the
    /// selected thread differs from the one the last query targeted.
    fn browser_sync(&mut self, cx: &mut Context<Self>) {
        let thread_id = self.state.selected.as_ref().map(|t| t.to_string());
        let b = &mut self.panel_state.browser;
        b.thread_id = thread_id.clone();
        b.sessions = None;
        b.error = None;
        b.loading = false;
        let Some(thread_id) = thread_id else {
            cx.notify();
            return;
        };
        let Some(rx) = self.send_query("preview.list", json!({ "threadId": thread_id })) else {
            self.panel_state.browser.error = Some("Not connected to a server.".to_string());
            cx.notify();
            return;
        };
        self.panel_state.browser.loading = true;
        let generation = self.panel_state.browser.generation.wrapping_add(1);
        self.panel_state.browser.generation = generation;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let _ = this.update(cx, |this, cx| {
                this.browser_apply(thread_id, exit, generation, cx);
            });
        })
        .detach();
    }

    fn browser_apply(
        &mut self,
        thread_id: String,
        exit: Value,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        if self.panel_state.browser.generation != generation {
            // A newer query is in flight — this reply is superseded.
            return;
        }
        if self.state.selected.as_deref() != Some(thread_id.as_str()) {
            // Stale reply for a thread that is no longer selected and no
            // query for the new selection is in flight yet — clear loading
            // so the render-side reconcile can issue one.
            self.panel_state.browser.loading = false;
            cx.notify();
            return;
        }
        let b = &mut self.panel_state.browser;
        b.loading = false;
        if exit["_tag"].as_str() == Some("Success") {
            b.sessions = Some(
                exit["value"]["sessions"]
                    .as_array()
                    .map(|a| a.iter().map(parse_preview_session).collect())
                    .unwrap_or_default(),
            );
            b.error = None;
        } else {
            let (_, message) = crate::explorer::rpc_error(&exit);
            b.sessions = None;
            b.error = Some(message);
        }
        cx.notify();
    }

    /// ↻ — re-issue `preview.list` even though the thread didn't change.
    fn browser_refresh(&mut self, cx: &mut Context<Self>) {
        self.panel_state.browser.thread_id = None;
        self.browser_sync(cx);
    }

    fn render_browser_pane(&self, cx: &mut Context<Self>) -> Div {
        let thread_id = self.state.selected.as_ref().map(|t| t.to_string());
        let b = &self.panel_state.browser;
        // Selection changed since the last query — schedule a fetch after
        // this render (`&self` can't mutate here).
        if b.thread_id != thread_id && !b.loading {
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| this.browser_sync(cx));
            })
            .detach();
        }

        let mut pane = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .text_xs();

        // Header: label + refresh — same h-9 inline subheader as the
        // Files/Terminals panes.
        pane = pane.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .h(px(36.))
                .px_2()
                .mb_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_nowrap()
                        .overflow_x_hidden()
                        .text_ellipsis()
                        .text_color(theme::fg_muted())
                        .child("Browser previews"),
                )
                .child(
                    div()
                        .id("browser-refresh")
                        .px_1p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .text_color(theme::fg_muted())
                        .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                        .child("↻")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _w, cx| this.browser_refresh(cx)),
                        ),
                ),
        );

        let empty = |message: &str| {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .p_4()
                .text_color(theme::fg_muted())
                .child(message.to_string())
        };

        if thread_id.is_none() {
            return pane.child(empty("Select a thread to see its browser previews"));
        }
        if let Some(err) = &b.error {
            return pane.child(
                empty(&format!("Couldn't load previews — {err}")).child(
                    div()
                        .id("browser-retry")
                        .mt_2()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .cursor_pointer()
                        .bg(theme::raised())
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::hover()))
                        .child("Retry")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _w, cx| this.browser_refresh(cx)),
                        ),
                ),
            );
        }
        let Some(sessions) = &b.sessions else {
            return pane.child(empty("Loading…"));
        };
        if sessions.is_empty() {
            return pane.child(empty("No browser preview sessions for this thread"));
        }

        let mut list = div()
            .id("browser-session-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_2()
            .py_1();
        for (ix, session) in sessions.iter().enumerate() {
            let title = session
                .title
                .clone()
                .or_else(|| session.url.clone())
                .unwrap_or_else(|| "Untitled tab".to_string());
            let mut meta = if session.tab_id.is_empty() {
                status_label(&session.status).to_string()
            } else {
                format!("{} · {}", session.tab_id, status_label(&session.status))
            };
            if let Some(runtime) = &session.runtime {
                meta.push_str(&format!(" · {runtime}"));
            }
            if session.headless {
                meta.push_str(" · headless");
            }
            let mut row = div()
                .id(("browser-session", ix))
                .flex()
                .items_center()
                .gap_2()
                .py_1p5()
                .border_b_1()
                .border_color(theme::border())
                .child(div().flex_none().child("🌐"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .whitespace_nowrap()
                                .overflow_x_hidden()
                                .text_ellipsis()
                                .text_color(theme::fg())
                                .child(title),
                        )
                        .child(
                            div()
                                .whitespace_nowrap()
                                .overflow_x_hidden()
                                .text_ellipsis()
                                .text_color(theme::fg_muted())
                                .child(session.url.clone().unwrap_or_else(|| meta.clone())),
                        )
                        .child(div().text_color(theme::fg_muted()).child(meta)),
                );
            if let Some(url) = session.url.clone() {
                row = row.child(
                    div()
                        .id(("browser-open", ix))
                        .flex_none()
                        .px_1p5()
                        .py_0p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .bg(theme::raised())
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::hover()))
                        .child("Open in browser")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |_this, _ev, _w, _cx| {
                                crate::explorer::open_external(&url);
                            }),
                        ),
                );
            }
            list = list.child(row);
        }
        pane.child(list)
    }
}
