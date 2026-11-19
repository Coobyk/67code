//! Settings surface (P1): modal panel toggled with cmd-, or via palette.
//!
//! Left nav + right content layout mirroring the web settings dialogs.
//! Connection shows the live endpoint/status, Providers mirrors the composer
//! model catalog (also selectable here — same `provider`/`model` fields the
//! composer picker writes), Shortcuts is a static reference, About carries
//! version/branding.

use gpui::{Context, IntoElement, MouseButton, SharedString, Window, div, prelude::*, px, rgb};

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

/// Left-nav groups (heading, sections) mirroring `SETTINGS_NAV_GROUPS` in
/// apps/web settingsSearch.ts: an unheaded block, Agents, Code, Connections,
/// and a trailing unheaded block.
const NAV_GROUPS: &[(Option<&str>, &[(Section, &str)])] = &[
    (
        None,
        &[
            (Section::Appearance, "Appearance"),
            (Section::ChatDefaults, "Chat"),
            (Section::Keybindings, "Keybindings"),
            (Section::Shortcuts, "Keyboard Shortcuts"),
        ],
    ),
    (
        Some("Agents"),
        &[
            (Section::Providers, "Providers"),
            (Section::AcpSessions, "ACP Sessions"),
            (Section::Threads, "Threads"),
            (Section::ScheduledTasks, "Scheduled Tasks"),
        ],
    ),
    (
        Some("Code"),
        &[
            (Section::Projects, "Projects"),
            (Section::SourceControl, "Source Control"),
            (Section::Integrations, "Integrations"),
        ],
    ),
    (
        Some("Connections"),
        &[
            (Section::Connection, "Connection"),
            (Section::Environments, "Environments"),
            (Section::Clients, "Authorized Clients"),
            (Section::Secrets, "Secrets"),
            (Section::Notifications, "Notifications"),
        ],
    ),
    (
        None,
        &[
            (Section::Diagnostics, "Diagnostics"),
            (Section::Usage, "Usage Limits"),
            (Section::ArchivedThreads, "Archived Threads"),
            (Section::About, "About"),
        ],
    ),
];

/// Label for `section` — used by the content header's breadcrumb.
fn section_label(section: Section) -> &'static str {
    NAV_GROUPS
        .iter()
        .flat_map(|(_, items)| items.iter())
        .find(|(s, _)| *s == section)
        .map(|(_, label)| *label)
        .unwrap_or("Settings")
}

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

/// Section title shown at the top of the content column — web
/// `SettingsSection` h2 (`text-base font-semibold`, min-h-7).
fn section_heading(title: &'static str) -> gpui::Div {
    div()
        .pb_4()
        .min_h(px(28.))
        .text_base()
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

    /// Left nav — the web `SettingsSidebarNav`: w-60 bg-sidebar column with
    /// `SidebarMenuButton` rows (h-8, rounded control-radius, px-2.5) grouped
    /// under muted headings.
    fn render_settings_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut nav = div()
            .id("settings-nav-scroll")
            .flex()
            .flex_col()
            .w(px(240.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(theme::border())
            .bg(theme::sidebar())
            .px_2()
            .py_3()
            .gap_1()
            .child(
                div()
                    .h(px(32.))
                    .flex()
                    .items_center()
                    .px(px(10.))
                    .text_sm()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::sidebar_fg())
                    .child("Settings"),
            );

        let mut index = 0usize;
        for (group_ix, (heading, items)) in NAV_GROUPS.iter().enumerate() {
            let mut group = div().flex().flex_col().gap_1();
            if let Some(heading) = heading {
                group = group.child(
                    div()
                        .px_2()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::sidebar_fg_muted())
                        .child(*heading),
                );
            }
            let mut menu = div().flex().flex_col().gap_1();
            for (section, label) in items.iter() {
                let section = *section;
                let active = self.settings_state.section == section;
                menu = menu.child(
                    div()
                        .id(("settings-nav", index))
                        .h(px(32.))
                        .flex()
                        .items_center()
                        .px(px(10.))
                        .rounded_lg()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .cursor_pointer()
                        .when(active, |s| {
                            s.bg(theme::sidebar_selected())
                                .text_color(theme::sidebar_fg())
                        })
                        .when(!active, |s| {
                            s.text_color(theme::sidebar_fg_muted())
                                .hover(|s| s.bg(theme::sidebar_hover()))
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
                index += 1;
            }
            nav = nav.child(
                group
                    .child(menu)
                    .when(group_ix + 1 < NAV_GROUPS.len(), |g| g.mb_3()),
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
                                .bg(theme::accent())
                                .text_color(theme::fg())
                                .hover(|s| s.bg(theme::hover()))
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
                        // web `Kbd`: h-5 min-w-5 rounded bg-foreground/[0.07]
                        // text-xs font-medium text-muted-foreground.
                        div()
                            .h(px(20.))
                            .min_w(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .px_1()
                            .rounded_sm()
                            .bg(theme::kbd_bg())
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
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

        // Web header: h-12 border-b with a "Settings / <section>" breadcrumb
        // and the modal close button on the right (`AppRouteModalClose`).
        let header = div()
            .h(px(48.))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .pl_5()
            .pr_2()
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_sm()
                    .child(div().text_color(theme::fg_muted()).child("Settings"))
                    .child(div().text_color(theme::fg_muted()).child("/"))
                    .child(div().text_color(theme::fg()).child(heading)),
            )
            .child(
                div()
                    .id("settings-close")
                    .w(px(28.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            cx.stop_propagation();
                            this.settings_state.open = false;
                            cx.notify();
                        }),
                    )
                    .child("×"),
            );

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .child(
                div()
                    .id("settings-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_5()
                    .pt_6()
                    .pb_12()
                    .child(section_heading(heading))
                    .child(content),
            )
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
            .bg(theme::overlay_backdrop())
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
                    .w(px(960.))
                    .h(px(620.))
                    .rounded_2xl()
                    .bg(theme::canvas())
                    .border_1()
                    .border_color(theme::border())
                    .shadow_2xl()
                    .overflow_hidden()
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
