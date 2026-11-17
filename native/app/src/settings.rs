//! Settings surface (P1): modal panel toggled with cmd-, or via palette.
//!
//! Left nav + right content layout mirroring the web settings dialogs.
//! Connection shows the live endpoint/status, Providers mirrors the composer
//! model catalog (also selectable here — same `provider`/`model` fields the
//! composer picker writes), Shortcuts is a static reference, About carries
//! version/branding.

use gpui::{
    Context, IntoElement, MouseButton, SharedString, Window, div, prelude::*, px, rgb, rgba,
};

use crate::composer::MODEL_CATALOG;
use crate::model::ConnectionStatus;
use crate::net::ClientCommand;
use crate::theme;
use crate::{AppView, ToggleSettings};

/// Sections listed in the settings left nav.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Section {
    Appearance,
    #[default]
    Connection,
    Environments,
    Providers,
    AcpSessions,
    Clients,
    ArchivedThreads,
    Notifications,
    ChatDefaults,
    Projects,
    ScheduledTasks,
    Secrets,
    Diagnostics,
    Usage,
    Shortcuts,
    Keybindings,
    SourceControl,
    Integrations,
    Threads,
    About,
}

const SECTIONS: &[(Section, &str)] = &[
    (Section::Appearance, "Appearance"),
    (Section::Connection, "Connection"),
    (Section::Environments, "Environments"),
    (Section::Providers, "Providers"),
    (Section::AcpSessions, "ACP Sessions"),
    (Section::Clients, "Authorized Clients"),
    (Section::ArchivedThreads, "Archived Threads"),
    (Section::Notifications, "Notifications"),
    (Section::ChatDefaults, "Chat"),
    (Section::Projects, "Projects"),
    (Section::ScheduledTasks, "Scheduled Tasks"),
    (Section::Secrets, "Secrets"),
    (Section::Diagnostics, "Diagnostics"),
    (Section::Usage, "Usage Limits"),
    (Section::Shortcuts, "Keyboard Shortcuts"),
    (Section::Keybindings, "Keybindings"),
    (Section::SourceControl, "Source Control"),
    (Section::Integrations, "Integrations"),
    (Section::Threads, "Threads"),
    (Section::About, "About"),
];

/// (keys, description) rows for the static shortcut reference.
const SHORTCUTS: &[(&str, &str)] = &[
    ("⌘K", "Command palette"),
    ("⌘,", "Open settings"),
    ("⌘N", "New thread"),
    ("⌘F", "Search threads"),
    ("Enter", "Send message"),
    ("Esc", "Close overlay / menu"),
];

#[derive(Default)]
pub struct State {
    pub open: bool,
    /// Section selected in the left nav.
    pub section: Section,
}

/// Dot + label + optional detail for the live `ConnectionStatus`.
fn connection_status(
    status: &ConnectionStatus,
) -> (gpui::Rgba, &'static str, Option<SharedString>) {
    match status {
        ConnectionStatus::Mock => (
            theme::fg_muted(),
            "Offline",
            Some("No server configured — showing built-in mock data.".into()),
        ),
        ConnectionStatus::Connecting => (rgb(0xfbbf24), "Connecting…", None),
        ConnectionStatus::Connected => (rgb(0x34d399), "Connected", None),
        ConnectionStatus::Failed(error) => {
            (rgb(0xf87171), "Connection failed", Some(error.clone()))
        }
        ConnectionStatus::Disconnected(reason) => {
            (rgb(0xf87171), "Disconnected", Some(reason.clone()))
        }
    }
}

/// Section title shown at the top of the content column.
fn section_heading(title: &'static str) -> gpui::Div {
    div()
        .pb_4()
        .text_lg()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::fg())
        .child(title)
}

/// Small uppercase-ish caption above a group of rows.
fn group_label(text: &'static str) -> gpui::Div {
    div()
        .pb_1()
        .pt_3()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(text)
}

/// A key/value row: muted label on the left, value on the right.
fn info_row(label: &'static str, value: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .py_1p5()
        .text_sm()
        .child(
            div()
                .w(px(140.))
                .flex_none()
                .text_color(theme::fg_muted())
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme::fg())
                .child(value.into()),
        )
}

impl AppView {
    pub fn on_toggle_settings(
        &mut self,
        _action: &ToggleSettings,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_state.open = !self.settings_state.open;
        cx.notify();
    }

    fn render_settings_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut nav = div()
            .id("settings-nav-scroll")
            .flex()
            .flex_col()
            .w(px(180.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(theme::border())
            .px_2()
            .py_3()
            .gap_1()
            .child(
                div()
                    .px_2()
                    .pb_3()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::fg())
                    .child("Settings"),
            );

        for (index, (section, label)) in SECTIONS.iter().enumerate() {
            let section = *section;
            let active = self.settings_state.section == section;
            nav = nav.child(
                div()
                    .id(("settings-nav", index))
                    .px_2()
                    .py_1p5()
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
                            this.settings_state.section = section;
                            cx.notify();
                        }),
                    )
                    .child(*label),
            );
        }
        nav
    }

    fn render_connection_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (dot_color, status_label, status_detail) = connection_status(&self.state.connection);
        let in_process = self.endpoint_label != "offline";

        let mut section = div().flex().flex_col().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .py_1p5()
                .text_sm()
                .child(
                    div()
                        .w(px(8.))
                        .h(px(8.))
                        .flex_none()
                        .rounded_full()
                        .bg(dot_color),
                )
                .child(div().text_color(theme::fg()).child(status_label))
                .when_some(status_detail, |s, detail| {
                    s.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(detail),
                    )
                }),
        );

        section = section
            .child(group_label("Server"))
            .child(info_row("Endpoint", self.endpoint_label.clone()))
            .child(info_row("Status", status_label));

        if in_process {
            let reconnecting = matches!(self.state.connection, ConnectionStatus::Connecting);
            section = section
                .child(
                    div()
                        .pt_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("reconnect-btn")
                                .px_3()
                                .py_1p5()
                                .rounded_md()
                                .text_xs()
                                .cursor_pointer()
                                .bg(theme::hover())
                                .text_color(theme::fg())
                                .hover(|s| s.bg(theme::selected()))
                                .child(if reconnecting { "Connecting…" } else { "Reconnect now" })
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _ev, _window, cx| {
                                        if let Some(commands) = &this.commands {
                                            let _ = commands.send(ClientCommand::Reconnect);
                                        }
                                        cx.notify();
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child("Retries automatically; this skips the backoff wait."),
                        ),
                )
                .child(
                    div()
                        .pt_2()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child("Server is managed by the native launcher. Set C67_SERVER_URL and C67_TOKEN before start to connect elsewhere."),
                );
        }

        section
    }

    fn render_providers_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div()
            .flex()
            .flex_col()
            .child(group_label("Model used for new threads and messages"));
        for (index, (instance, model, label)) in MODEL_CATALOG.iter().enumerate() {
            let active = self.provider == *instance && self.model == *model;
            list = list.child(
                div()
                    .id(("provider", index))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .when(active, |s| s.bg(theme::selected()).text_color(theme::fg()))
                    .when(!active, |s| {
                        s.text_color(theme::fg()).hover(|s| s.bg(theme::hover()))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.provider = (*instance).to_string();
                            this.model = (*model).to_string();
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .w(px(14.))
                            .flex_none()
                            .text_color(theme::primary())
                            .child(if active { "✓" } else { "" }),
                    )
                    .child(div().flex_none().child(*label))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(format!("{instance}/{model}")),
                    ),
            );
        }
        list
    }

    fn render_shortcuts_section(&self) -> impl IntoElement {
        let mut list = div().flex().flex_col();
        for (keys, description) in SHORTCUTS {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_1p5()
                    .text_sm()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(theme::fg())
                            .child(*description),
                    )
                    .child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::canvas())
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(*keys),
                    ),
            );
        }
        list
    }

    fn render_about_section(&self) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::fg())
                    .child("67 Code"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme::fg_muted())
                    .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
            )
            .child(
                div()
                    .pt_3()
                    .text_sm()
                    .text_color(theme::fg_muted())
                    .child("Native GPUI client · Rust orchestration server"),
            )
    }

    fn render_settings_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (heading, content) = match self.settings_state.section {
            Section::Appearance => (
                "Appearance",
                self.render_appearance_section(cx).into_any_element(),
            ),
            Section::Connection => (
                "Connection",
                self.render_connection_section(cx).into_any_element(),
            ),
            Section::Environments => (
                "Environments",
                self.render_envs_section(cx).into_any_element(),
            ),
            Section::Providers => (
                "Providers",
                self.render_providers_section(cx).into_any_element(),
            ),
            Section::AcpSessions => (
                "ACP Sessions",
                self.render_acp_section(cx).into_any_element(),
            ),
            Section::Clients => (
                "Authorized Clients",
                self.render_clients_section(cx).into_any_element(),
            ),
            Section::ArchivedThreads => (
                "Archived Threads",
                self.render_archived_section(cx).into_any_element(),
            ),
            Section::Notifications => (
                "Notifications",
                self.render_notifications_section(cx).into_any_element(),
            ),
            Section::ChatDefaults => (
                "Chat",
                self.render_chat_defaults_section(cx).into_any_element(),
            ),
            Section::Projects => (
                "Projects",
                self.render_projects_section(cx).into_any_element(),
            ),
            Section::ScheduledTasks => (
                "Scheduled Tasks",
                self.render_tasks_section(cx).into_any_element(),
            ),
            Section::Secrets => (
                "Secrets",
                self.render_secrets_section(cx).into_any_element(),
            ),
            Section::Diagnostics => (
                "Diagnostics",
                self.render_diagnostics_section(cx).into_any_element(),
            ),
            Section::Usage => (
                "Usage Limits",
                self.render_usage_section(cx).into_any_element(),
            ),
            Section::Shortcuts => (
                "Keyboard Shortcuts",
                self.render_shortcuts_section().into_any_element(),
            ),
            Section::Keybindings => (
                "Keybindings",
                self.render_keybindings_section(cx).into_any_element(),
            ),
            Section::SourceControl => (
                "Source Control",
                self.render_source_control_section(cx).into_any_element(),
            ),
            Section::Integrations => (
                "Integrations",
                self.render_integrations_section(cx).into_any_element(),
            ),
            Section::Threads => (
                "Threads",
                self.render_threads_section(cx).into_any_element(),
            ),
            Section::About => ("About", self.render_about_section().into_any_element()),
        };

        div()
            .id("settings-content")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .px_5()
            .py_4()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .justify_between()
                    .child(section_heading(heading))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("esc to close"),
                    ),
            )
            .child(content)
    }

    pub fn render_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.settings_state.open {
            return div();
        }

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000b3))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, _window, cx| {
                    this.settings_state.open = false;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex()
                    .w(px(720.))
                    .h(px(500.))
                    .rounded_lg()
                    .bg(theme::raised())
                    .border_1()
                    .border_color(theme::border())
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_this, _ev, _window, cx| cx.stop_propagation()),
                    )
                    .child(self.render_settings_nav(cx))
                    .child(self.render_settings_content(cx)),
            )
    }
}
