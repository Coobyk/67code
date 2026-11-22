//! Right-side inspector panel hosting tabbed panes (files, terminals, PRs,
//! local-diff review, agent browser previews).
//! Owns `impl AppView` panel render + `right_panel::State`.
//!
//! The Browser tab is the honest version of an agent-browser surface: it
//! lists `preview.list` sessions for the selected thread (URL + nav status)
//! and offers "Open in browser" via `xdg-open`. There is no embedded
//! webview — the GPUI client doesn't pretend to render pages.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, Div, FontWeight, MouseButton, ScrollHandle, SharedString, div, point,
    prelude::*, px,
};
use serde_json::{Value, json};

use crate::AppView;
use crate::CenterView;
use crate::icons::{Icon, icon};
use crate::model::ConnectionStatus;
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

/// `server.getConfig` slice the thread-details card needs, plus the
/// expansion flags for its inline pickers (workspace, open-in). The card
/// itself is the web's `ThreadDetailsCard` — an absolute `dropdown-glass`
/// panel at the chat canvas' top-right (`right: 12`, `top: 12`, 280px
/// wide, `THREAD_DETAILS_CARD_GAP`).
#[derive(Default)]
pub struct DetailsState {
    /// `environment.label` ("ferrum" in the web screenshot).
    pub environment_label: Option<String>,
    /// `environment.environmentId` — `shell.openInEditor` needs it?
    /// (kept for parity with the web's environmentId prop).
    pub environment_id: Option<String>,
    /// `availableEditors` — EditorId list; empty means the server has no
    /// editor detection and the open-in row renders disabled.
    pub editors: Vec<String>,
    /// `settings` — `defaultProjectScripts` + per-project overrides feed
    /// the scripts rows (web `resolveProjectScripts`).
    pub settings: Option<Value>,
    /// Last successful config fetch (~5 min cadence, like the composer).
    pub config_fetched_at: Option<Instant>,
    pub config_loading: bool,
    /// Workspace row expanded → saved-environments list.
    pub env_expanded: bool,
    /// Open-in row expanded → editor list.
    pub editor_expanded: bool,
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
    /// Horizontal scroll position of the tab strip (overflow at the
    /// web-matching 280px width is real — the strip scrolls, like the web).
    pub tab_scroll: ScrollHandle,
    /// Last measured window width — a deferred probe refreshes it after
    /// each render (the Window itself is on the update stack mid-render,
    /// so render can't read `viewport_size` directly). 0 = unmeasured.
    pub viewport_w: f32,
    /// Thread-details card (floating, canvas top-right).
    pub details: DetailsState,
}

/// Web `RIGHT_PANEL_INLINE_LAYOUT_MEDIA_QUERY` = `(max-width: 980px)`
/// (apps/web/src/rightPanelLayout.ts): at or below 980px the web swaps the
/// inline 280px column for a floating sheet. The native client has no sheet
/// surface, so it auto-collapses the panel instead — the tab state persists
/// and the column returns when the window widens again.
const INLINE_PANEL_BREAKPOINT: f32 = 980.0;

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
        // Viewport-width probe: `window.viewport_size()` is unreachable
        // mid-render (the window sits on the update stack), so stash the
        // width on panel state at the end of the effect cycle. Re-renders
        // on resize pick the fresh value up; `cx.notify` only fires when
        // it actually changed, so this doesn't loop.
        cx.defer(|app| {
            for handle in app.windows() {
                let _ = handle.update(app, |root, window, app| {
                    let width: f32 = window.viewport_size().width.into();
                    if let Ok(view) = root.downcast::<AppView>() {
                        let _ = view.update(app, |this, cx| {
                            if this.panel_state.viewport_w != width {
                                this.panel_state.viewport_w = width;
                                cx.notify();
                            }
                        });
                    }
                });
            }
        });
        let Some(tab) = self.panel_state.tab else {
            // Panel closed → zero-size positioned anchor at the canvas'
            // top-right corner; it still carries the floating details
            // card (absolute children anchor to the nearest positioned
            // parent and aren't clipped to its bounds).
            return div().relative().child(self.render_details_card(cx));
        };
        // Below the web's 980px inline-layout breakpoint the panel hides
        // (the web renders a floating sheet there — see narrow.png at
        // 900px). 0 = width not measured yet → keep the column.
        if self.panel_state.viewport_w > 0.
            && self.panel_state.viewport_w <= INLINE_PANEL_BREAKPOINT
        {
            return div().relative().child(self.render_details_card(cx));
        }
        let body = match tab {
            Tab::Files => self.render_explorer_pane(cx).into_any_element(),
            Tab::Terminals => self.render_terminal_pane(cx).into_any_element(),
            Tab::PullRequests => self.render_prs_pane(cx).into_any_element(),
            Tab::Diff => self.render_diff_pane(cx).into_any_element(),
            Tab::Browser => self.render_browser_pane(cx).into_any_element(),
        };
        // Web parity: measured on the 1280×800 dark reference the right
        // panel runs exactly 280px (`--thread-details-panel-width: 17.5rem`,
        // apps/web/src/index.css:132; THREAD_DETAILS_CARD_WIDTH = 280). At
        // that width five tab pills overflow, so the strip scrolls with ‹ ›
        // buttons — the same behaviour the web's RightPanelTabs implements
        // (`hasOverflow` → ChevronLeft/ChevronRight icon-xs buttons).
        div()
            .relative()
            // w + max_w: content min-width (long tree names etc.) would
            // otherwise stretch the item past 280 despite flex_none.
            // (`overflow_x_hidden` sits on the body wrap instead — the
            // floating details card escapes the left edge when the panel
            // is open and must not be clipped.)
            .w(px(theme::DETAILS_PANEL_W))
            .max_w(px(theme::DETAILS_PANEL_W))
            .min_w_0()
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(theme::border())
            .bg(theme::canvas())
            .flex()
            .flex_col()
            .child(self.render_panel_tab_bar(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_x_hidden()
                    .child(body),
            )
            .child(self.render_details_card(cx))
    }

    /// Web `RightPanelTabs`: the strip sits at `--workspace-topbar-height`
    /// (52px) with compact `h-6 max-w-36 shrink-0 gap-0.5 rounded-md
    /// pr-2 pl-1.5 text-xs` pills — a leading 12px lucide glyph, active
    /// `bg-accent text-foreground`, inactive muted + `hover:bg-accent/60`.
    /// When pills overflow the panel the strip scrolls horizontally and
    /// ghost ‹ › buttons (`icon-xs`, scroll by ~75% of the viewport)
    /// appear at the right edge.
    fn render_panel_tab_bar(&self, cx: &mut Context<Self>) -> Div {
        let tab = |tab_icon: Icon,
                   label: &'static str,
                   target: Tab,
                   ix: usize,
                   cx: &mut Context<Self>| {
            let active = self.panel_state.tab == Some(target);
            div()
                .h(px(24.))
                .flex_none()
                .flex()
                .items_center()
                .gap_0p5()
                .pl_1p5()
                .pr_2()
                .rounded(theme::radius::MD)
                .cursor_pointer()
                .text_xs()
                .whitespace_nowrap()
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
                .child(icon(tab_icon, 12.).text_color(if active {
                    theme::fg()
                } else {
                    theme::fg_muted()
                }))
                .child(label)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _ev, _w, cx| {
                        this.panel_state.tab = Some(target);
                        // Web scrolls a newly-active tab into view.
                        this.panel_state.tab_scroll.scroll_to_item(ix);
                        cx.notify();
                    }),
                )
        };
        // Web `scrollTabs(direction)`: `max(120, viewport.clientWidth * 0.75)`
        // — ~180px at the 280px panel width.
        let scroll_step = px(180.);
        let scroll_btn = |dir_icon: Icon, id: &'static str, cx: &mut Context<Self>| {
            div()
                .id(id)
                .w(px(20.))
                .h(px(20.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .cursor_pointer()
                .text_color(theme::fg_muted())
                .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                .child(icon(dir_icon, 12.).text_color(theme::fg_muted()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _w, cx| {
                        let handle = &this.panel_state.tab_scroll;
                        let max = handle.max_offset().x;
                        let cur = handle.offset().x;
                        // GPUI scroll offsets are ≤0 and grow more negative
                        // toward the max ("distance from container top-left
                        // to first child").
                        let next = if dir_icon == Icon::ChevronLeft {
                            (cur + scroll_step).min(px(0.))
                        } else {
                            (cur - scroll_step).max(-max)
                        };
                        handle.set_offset(point(next, px(0.)));
                        cx.notify();
                    }),
                )
        };
        let scroll = self.panel_state.tab_scroll.clone();
        let overflowing = scroll.max_offset().x > px(0.5);
        let mut strip = div()
            .h(px(theme::TOPBAR_H))
            .pl_2()
            .pr_1()
            .flex()
            .flex_row()
            .items_center()
            .child(
                div()
                    .id("panel-tab-scroll")
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_0p5()
                    .overflow_x_scroll()
                    .track_scroll(&scroll)
                    .child(tab(Icon::Folder, "Files", Tab::Files, 0, cx))
                    .child(tab(Icon::Terminal, "Terminals", Tab::Terminals, 1, cx))
                    .child(tab(
                        Icon::GitPullRequest,
                        "Pull requests",
                        Tab::PullRequests,
                        2,
                        cx,
                    ))
                    .child(tab(Icon::GitCompare, "Diff", Tab::Diff, 3, cx))
                    .child(tab(Icon::Globe, "Browser", Tab::Browser, 4, cx)),
            );
        if overflowing {
            strip = strip.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .pl_1()
                    .child(scroll_btn(Icon::ChevronLeft, "panel-tab-scroll-left", cx))
                    .child(scroll_btn(Icon::ChevronRight, "panel-tab-scroll-right", cx)),
            );
        }
        strip
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
                        .w(px(22.))
                        .h(px(22.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .cursor_pointer()
                        .text_color(theme::fg_muted())
                        .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                        .child(icon(Icon::RefreshCw, 12.).text_color(theme::fg_muted()))
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
                .child(
                    div()
                        .flex_none()
                        .child(icon(Icon::Globe, 13.).text_color(theme::icon_muted())),
                )
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

    // ------------------------------------------------------------------
    // Thread details card — web `ThreadDetailsCard`/`ThreadDetailsPanel`
    // ------------------------------------------------------------------
    //
    // The web card is `absolute z-20` at the chat canvas' top-right:
    // `right: THREAD_DETAILS_CARD_GAP (12)`, `top: 12`, width 280,
    // `rounded-2xl dropdown-glass` (popover @92% + contrast-fg/10 hairline).
    // `resolveThreadDetailsCardLayout` hides it when the lane would shrink
    // below `minChatWidth` 640 (plus `DETAILS_CARD_CLEARANCE` 32 and lane
    // padding ~24): canvas ≥ 988.
    //
    // Rows (threadDetailsPanelStyles): h-8 rounded-lg controls, gap-2.5
    // px-2.5 content, 13px medium text-foreground/80 labels, size-4
    // muted icons and chevrons, `hover:bg-white/[0.08]` (dark) =
    // `theme::hover()`.

    /// The card needs at least this much canvas width (see layout notes).
    const DETAILS_CARD_MIN_CANVAS_W: f32 = 988.0;
    /// Chat sidebar column width (`sidebar.rs` root `.w(px(256.))`).
    const SIDEBAR_W: f32 = 256.0;
    /// Refetch `server.getConfig` after 5 min — matches the composer's
    /// provider-config cadence.
    const DETAILS_CONFIG_TTL: Duration = Duration::from_secs(300);

    /// Deferred reconcile (render is `&self`): fetch `server.getConfig`
    /// when the cached slice is missing or stale.
    fn details_maybe_sync(&self, cx: &mut Context<Self>) {
        let connected = matches!(self.state.connection, ConnectionStatus::Connected);
        if !connected {
            return;
        }
        let d = &self.panel_state.details;
        let stale = d
            .config_fetched_at
            .map(|t| t.elapsed() > Self::DETAILS_CONFIG_TTL)
            .unwrap_or(true);
        if !stale || d.config_loading {
            return;
        }
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |this, cx| this.details_fetch(cx));
        })
        .detach();
    }

    fn details_fetch(&mut self, cx: &mut Context<Self>) {
        if self.panel_state.details.config_loading {
            return;
        }
        let Some(rx) = self.send_query("server.getConfig", json!({})) else {
            return;
        };
        self.panel_state.details.config_loading = true;
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let d = &mut this.panel_state.details;
                d.config_loading = false;
                if exit["_tag"].as_str() == Some("Success") {
                    let env = &exit["value"]["environment"];
                    d.environment_label = env["label"].as_str().map(str::to_string);
                    d.environment_id = env["environmentId"].as_str().map(str::to_string);
                    d.editors = exit["value"]["availableEditors"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|e| e.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    d.settings = Some(exit["value"]["settings"].clone());
                    d.config_fetched_at = Some(Instant::now());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Port of `resolveProjectScripts` (packages/shared/projectScripts.ts)
    /// minus `project.scripts` — the native shell stream doesn't carry a
    /// per-project scripts field, so defaults and settings overrides are
    /// the honest sources.
    fn details_scripts(&self) -> Vec<Value> {
        let Some(settings) = &self.panel_state.details.settings else {
            return Vec::new();
        };
        let project_id = self.pr_scope().map(|(id, _)| id);
        let defaults = settings["defaultProjectScripts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(pid) = project_id.as_ref() {
            if let Some(over) =
                settings["projectSettingsOverrides"][pid]["defaultProjectScripts"].as_array()
            {
                return over.clone();
            }
        }
        if settings["projectSettingsFolded"].as_bool() == Some(true) {
            return defaults;
        }
        if let Some(pid) = project_id.as_ref() {
            let legacy = &settings["projectScriptOverrides"][pid];
            if legacy.is_null() {
                return defaults;
            }
            if let Some(list) = legacy.as_array() {
                return list.clone();
            }
        }
        defaults
    }

    /// `shell.openInEditor` — real editors only; the row is disabled when
    /// `availableEditors` is empty so nothing claims support the server
    /// doesn't have.
    fn details_open_editor(&mut self, editor: String, cx: &mut Context<Self>) {
        let Some((_pid, cwd)) = self.pr_scope() else {
            return;
        };
        let Some(rx) = self.send_query(
            "shell.openInEditor",
            json!({ "cwd": cwd, "editor": editor }),
        ) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                if exit["_tag"].as_str() != Some("Success") {
                    this.push_toast(
                        crate::notifications::ToastKind::Error,
                        "Couldn't open editor".to_string(),
                        Some(crate::vcs::failure_message(&exit)),
                        None,
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// The floating card itself. Lives on the panel anchor (a zero-size
    /// flex item at the canvas' top-right when the tabbed panel is closed)
    /// so no lib.rs changes are needed; `right` adds the panel width when
    /// the column is open.
    pub(crate) fn render_details_card(&self, cx: &mut Context<Self>) -> AnyElement {
        self.details_maybe_sync(cx);
        self.vcs_maybe_sync(cx);
        if self.center_view != CenterView::Chat {
            return div().into_any_element();
        }
        let panel_open = self.panel_state.tab.is_some()
            && !(self.panel_state.viewport_w > 0.
                && self.panel_state.viewport_w <= INLINE_PANEL_BREAKPOINT);
        // `resolveThreadDetailsCardLayout` → null when the remaining lane
        // drops under `minChatWidth`; mirror the arithmetic.
        if self.panel_state.viewport_w > 0. {
            let canvas_w = self.panel_state.viewport_w
                - Self::SIDEBAR_W
                - if panel_open { theme::DETAILS_PANEL_W } else { 0. };
            if canvas_w < Self::DETAILS_CARD_MIN_CANVAS_W {
                return div().into_any_element();
            }
        }
        let connected = matches!(self.state.connection, ConnectionStatus::Connected);
        if !connected || self.state.selected.is_none() {
            return div().into_any_element();
        }

        // Row factory: web THREAD_DETAILS_PANEL_ROW_CLASS. Generic
        // `on_click` so rows can capture context (script commands capture
        // the command string).
        fn row(
            id: &'static str,
            row_icon: Icon,
            label: SharedString,
            chevron: Option<Icon>,
            disabled: bool,
            cx: &mut Context<AppView>,
            on_click: impl Fn(&mut AppView, &mut Context<AppView>) + 'static,
            trailing: Option<AnyElement>,
        ) -> impl IntoElement {
            div()
                .id(id)
                .h(px(32.))
                .w_full()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(10.))
                .rounded(theme::radius::LG)
                .when(!disabled, |s| s.cursor_pointer())
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if disabled {
                    theme::fg_muted()
                } else {
                    theme::control_fg()
                })
                .when(!disabled, |s| {
                    s.hover(|h| h.bg(theme::hover())).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _w, cx| on_click(this, cx)),
                    )
                })
                .child(
                    icon(row_icon, 16.)
                        .flex_none()
                        .text_color(theme::fg_muted()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_nowrap()
                        .overflow_x_hidden()
                        .text_ellipsis()
                        .child(label),
                )
                .when_some(trailing, |el, t| el.child(t))
                .when_some(chevron, |el, c| {
                    el.child(icon(c, 16.).flex_none().text_color(theme::fg_muted()))
                })
        }

        let mut card = div()
            .id("thread-details-card")
            .w(px(theme::DETAILS_PANEL_W))
            .flex()
            .flex_col()
            .rounded(theme::radius::XL2)
            .border_1()
            .border_color(theme::popover_border())
            .bg(theme::popover_92())
            .shadow_lg()
            .p_1()
            .overflow_hidden();

        // ── Workspace section: environment row, open-in row, scripts ──
        let env_label = self
            .panel_state
            .details
            .environment_label
            .clone()
            .unwrap_or_else(|| self.endpoint_label.clone());
        card = card.child(row(
            "details-env",
            Icon::Monitor,
            env_label.into(),
            Some(Icon::ChevronDown),
            false,
            cx,
            |this, cx| {
                this.panel_state.details.env_expanded =
                    !this.panel_state.details.env_expanded;
                cx.notify();
            },
            Some(icon(Icon::Folder, 16.).text_color(theme::fg_muted()).into_any_element()),
        ));
        if self.panel_state.details.env_expanded {
            let mut envs = div().flex().flex_col();
            for (ix, env) in self.client_config.environments.iter().enumerate() {
                // `envs::is_env_active` is private to that module; the
                // check is one line — compare against the live endpoint.
                let active = self
                    .active_endpoint
                    .as_ref()
                    .is_some_and(|e| e.http_url == env.http_url);
                let label = env.label.clone();
                envs = envs.child(
                    div()
                        .id(("details-env-row", ix))
                        .h(px(28.))
                        .w_full()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(10.))
                        .rounded(theme::radius::LG)
                        .cursor_pointer()
                        .text_size(px(13.))
                        .text_color(if active {
                            theme::fg()
                        } else {
                            theme::control_fg()
                        })
                        .hover(|s| s.bg(theme::hover()))
                        .child(icon(Icon::Monitor, 14.).text_color(theme::fg_muted()))
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .when(active, |s| {
                            s.child(icon(Icon::Check, 14.).text_color(theme::fg()))
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _w, cx| {
                                // `envs::connect_environment` is private to
                                // that module — inline the same switch:
                                // persist the pick, swap endpoint labels,
                                // ask the net loop to reconnect.
                                let Some(env) =
                                    this.client_config.environments.get(ix).cloned()
                                else {
                                    return;
                                };
                                this.client_config.active_environment = Some(ix);
                                this.client_config.save();
                                this.endpoint_label = env.http_url.clone();
                                this.active_endpoint = Some(crate::net::ServerEndpoint {
                                    http_url: env.http_url.clone(),
                                    token: env.token.clone(),
                                });
                                if let Some(commands) = &this.commands {
                                    if let Err(err) = commands.send(
                                        crate::net::ClientCommand::SwitchEndpoint {
                                            http_url: env.http_url,
                                            token: env.token,
                                        },
                                    ) {
                                        this.push_toast(
                                            crate::notifications::ToastKind::Error,
                                            "Couldn't switch environment".to_string(),
                                            Some(err.to_string()),
                                            None,
                                            cx,
                                        );
                                    } else {
                                        this.state.connection =
                                            ConnectionStatus::Connecting;
                                    }
                                }
                                this.panel_state.details.env_expanded = false;
                                cx.notify();
                            }),
                        ),
                );
            }
            card = card.child(envs);
        }

        // ── Open-in row (web OpenInPicker): always rendered, disabled
        // when the server reports no editors. ──
        let editors = self.panel_state.details.editors.clone();
        let primary_editor = editors.first().cloned();
        let open_label = primary_editor
            .as_deref()
            .map(|id| format!("Open in {}", editor_label(id)))
            .unwrap_or_else(|| "Open in editor".to_string());
        card = card.child(row(
            "details-open-in",
            primary_editor
                .as_deref()
                .map(editor_icon)
                .unwrap_or(Icon::SquareArrowOutUpRight),
            open_label.into(),
            primary_editor.as_ref().map(|_| Icon::ChevronDown),
            primary_editor.is_none(),
            cx,
            |this, cx| {
                if this.panel_state.details.editors.is_empty() {
                    this.push_toast(
                        crate::notifications::ToastKind::Info,
                        "No editors available".to_string(),
                        Some("This server reported no detected editors.".to_string()),
                        None,
                        cx,
                    );
                } else {
                    this.panel_state.details.editor_expanded =
                        !this.panel_state.details.editor_expanded;
                    cx.notify();
                }
            },
            None,
        ));
        if self.panel_state.details.editor_expanded {
            let mut rows = div().flex().flex_col();
            for editor in &editors {
                let editor = editor.clone();
                let label = editor_label(&editor);
                rows = rows.child(
                    div()
                        .id(format!("details-editor-{editor}"))
                        .h(px(28.))
                        .w_full()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(10.))
                        .rounded(theme::radius::LG)
                        .cursor_pointer()
                        .text_size(px(13.))
                        .text_color(theme::control_fg())
                        .hover(|s| s.bg(theme::hover()))
                        .child(icon(editor_icon(&editor), 14.).text_color(theme::fg_muted()))
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _w, cx| {
                                this.details_open_editor(editor.clone(), cx);
                            }),
                        ),
                );
            }
            card = card.child(rows);
        }

        // ── Project scripts (web ProjectScriptsControl): each script is a
        // Play-icon row; running opens the drawer and types the command. ──
        for script in self.details_scripts() {
            let name = script["name"].as_str().unwrap_or("Script").to_string();
            let command = script["command"].as_str().unwrap_or_default().to_string();
            if command.is_empty() {
                continue;
            }
            let cmd = command.clone();
            card = card.child(row(
                "details-script",
                Icon::Play,
                name.into(),
                None,
                false,
                cx,
                move |this, cx| this.terminal_run_script(cmd.clone(), cx),
                Some(
                    icon(Icon::Terminal, 16.)
                        .text_color(theme::fg_muted())
                        .into_any_element(),
                ),
            ));
        }

        // ── Version control section (border-border/65 divider) ──
        let (local, _remote, _seen) = self.vcs_state.card_status();
        let is_repo = local
            .and_then(|l| l["isRepo"].as_bool())
            .unwrap_or(false);
        if local.is_some() && is_repo {
            card = card.child(
                div()
                    .h(px(1.))
                    .mx(px(10.))
                    .my(px(3.))
                    .bg(theme::border_70()),
            );
            let branch = local
                .and_then(|l| l["refName"].as_str())
                .unwrap_or("detached")
                .to_string();
            card = card.child(row(
                "details-branch",
                Icon::GitBranch,
                branch.into(),
                Some(Icon::ChevronDown),
                false,
                cx,
                |this, cx| this.vcs_toggle_picker(cx),
                None,
            ));
            if self.vcs_state.picker_open() {
                card = card.child(self.render_branch_picker(cx).into_any_element());
            }
            let quick = self.git_quick_action();
            let disabled = matches!(quick.kind, crate::vcs::QuickActionKind::Hint);
            card = card.child(row(
                "details-git-action",
                quick.icon,
                quick.label.clone().into(),
                Some(Icon::ChevronDown),
                disabled,
                cx,
                move |this, cx| {
                    let quick = this.git_quick_action();
                    this.vcs_run_quick_action(&quick, cx);
                },
                None,
            ));
            // Changes row → diff pane (web onOpenChanges). The web shows
            // only the +/− counters on the right; the base descriptor would
            // squeeze the label to an ellipsis at 280px.
            let (ins, del, _base) = self.git_changes_totals();
            card = card.child(row(
                "details-changes",
                Icon::FileDiff,
                "Changes".into(),
                None,
                false,
                cx,
                |this, cx| {
                    this.panel_state.tab = Some(Tab::Diff);
                    cx.notify();
                },
                Some(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .font_family(theme::FONT_MONO)
                                .child(
                                    div()
                                        .text_color(if ins > 0 {
                                            theme::diff_add_fg()
                                        } else {
                                            theme::fg_muted()
                                        })
                                        .child(format!("+{ins}")),
                                )
                                .child(
                                    div()
                                        .text_color(if del > 0 {
                                            theme::diff_del_fg()
                                        } else {
                                            theme::fg_muted()
                                        })
                                        .child(format!("-{del}")),
                                ),
                        )
                        .into_any_element(),
                ),
            ));
        }

        card.absolute()
            // The anchor row's top is the titlebar's bottom edge — the
            // canvas starts below the topbar, so y = TOPBAR_H + 12 (the
            // web's `topInset`-aware `gap + topInset` resolves the same).
            .top(px(theme::TOPBAR_H + 12.))
            .right(px(12. + if panel_open { theme::DETAILS_PANEL_W } else { 0. }))
            .into_any_element()
    }
}

/// EditorId → display label (web `editorLabelForPlatform` simplifies to
/// the catalog name — we only know ids over the wire, so title-case the
/// common ones).
fn editor_label(id: &str) -> String {
    match id {
        "zed" => "Zed".to_string(),
        "vscode" => "VS Code".to_string(),
        "cursor" => "Cursor".to_string(),
        "file-manager" => "File manager".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
                None => other.to_string(),
            }
        }
    }
}

/// EditorId → row glyph: the Zed brand mark, a file-manager folder, else
/// the generic external-open square.
fn editor_icon(id: &str) -> Icon {
    match id {
        "zed" => Icon::Zed,
        "file-manager" => Icon::Folder,
        _ => Icon::SquareArrowOutUpRight,
    }
}
