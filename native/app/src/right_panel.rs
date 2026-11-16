//! Right-side inspector panel hosting tabbed panes (files, terminals, PRs).
//! Owns `impl AppView` panel render + `right_panel::State`.

use gpui::{div, prelude::*, px, Context, Div};

use crate::AppView;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Files,
    Terminals,
    PullRequests,
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
        };
        div()
            .w(px(380.))
            .h_full()
            .border_l_1()
            .border_color(crate::theme::border())
            .bg(crate::theme::canvas())
            .flex()
            .flex_col()
            .child(self.render_panel_tab_bar(cx))
            .child(body)
    }

    fn render_panel_tab_bar(&self, _cx: &mut Context<Self>) -> Div {
        div().h(px(36.)).border_b_1().border_color(crate::theme::border())
    }
}
