//! Appearance settings (P2): theme mode rows (Dark / Light / System) plus
//! accent selection, mirroring AppearanceSettingsPanel in the web UI.
//! Mode switching goes through `theme::set_mode` and persists via
//! `config::ClientConfig` (`self.client_config`).

use gpui::{Context, IntoElement, Window, div, prelude::*};

use crate::AppView;

#[derive(Default)]
pub struct State {}

impl AppView {
    pub fn render_appearance_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = cx;
        div().child("Appearance — stub")
    }

    #[allow(dead_code)]
    fn _appearance_placeholder(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}
}
