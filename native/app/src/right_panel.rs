//! Right-side inspector panel hosting tabbed panes (files, terminals, PRs,
//! local-diff review).
//! Owns `impl AppView` panel render + `right_panel::State`.

use gpui::{Context, Div, div, prelude::*, px};

use crate::AppView;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Files,
    Terminals,
    PullRequests,
    /// `review.getDiffPreview` viewer (vcs.rs / pr_review.rs).
    Diff,
}

#[derive(Default)]
pub struct State {
    /// Which tab the panel shows, or None when the panel is closed.
    pub tab: Option<Tab>,
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
        };
        div()
            .w(px(380.))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(crate::theme::border())
            .bg(crate::theme::canvas())
            .flex()
            .flex_col()
            .child(self.render_panel_tab_bar(cx))
            .child(body)
    }

    fn render_panel_tab_bar(&self, cx: &mut Context<Self>) -> Div {
        let tab = |label: &'static str, target: Tab, cx: &mut Context<Self>| {
            let active = self.panel_state.tab == Some(target);
            div()
                .px_2()
                .h_full()
                .flex()
                .items_center()
                .cursor_pointer()
                .text_xs()
                .text_color(if active {
                    crate::theme::fg()
                } else {
                    crate::theme::fg_muted()
                })
                .border_b_2()
                .border_color(if active {
                    crate::theme::primary()
                } else {
                    gpui::rgba(0x00000000)
                })
                .hover(|s| s.bg(crate::theme::hover()))
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
            .h(px(36.))
            .px_1()
            .border_b_1()
            .border_color(crate::theme::border())
            .flex()
            .flex_row()
            .child(tab("Files", Tab::Files, cx))
            .child(tab("Terminals", Tab::Terminals, cx))
            .child(tab("Pull requests", Tab::PullRequests, cx))
            .child(tab("Diff", Tab::Diff, cx))
    }
}
