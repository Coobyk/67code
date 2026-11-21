//! Settings surface (P1): modal panel toggled with cmd-, or via palette.
//!
//! Left nav + right content layout mirroring the web settings dialogs.
//! Connection shows the live endpoint/status, Providers mirrors the composer
//! model catalog (also selectable here — same `provider`/`model` fields the
//! composer picker writes), Shortcuts is a static reference, About carries
//! version/branding.

use gpui::{Context, IntoElement, MouseButton, SharedString, Window, div, prelude::*, px, rgb};

use crate::composer::MODEL_CATALOG;
use crate::icons::{Icon, icon};
use crate::model::ConnectionStatus;
use crate::net::ClientCommand;
use crate::theme;
use crate::{AppView, ToggleSettings};

/// Sections listed in the settings left nav.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Section {
    /// The web's `/settings/general` page — notification preferences and
    /// device-local defaults (the native surface hosts the Notifications
    /// content there).
    #[default]
    General,
    Appearance,
    Connection,
    Environments,
    Providers,
    AcpSessions,
    Clients,
    ArchivedThreads,
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
/// apps/web settingsSearch.ts: an unheaded block (General … SnapShots),
/// Agents, Code, Connections, and a trailing unheaded block. Sections the
/// web has but native doesn't implement (Diffs & pull requests, Voice,
/// SnapShots, Browser, Devices, Worktrees, Maintenance, Labs) are omitted
/// rather than stubbed; native-only sections (Keyboard Shortcuts,
/// ACP Sessions, Connection, Authorized Clients, Diagnostics, Usage)
/// stay in the nearest matching group.
const NAV_GROUPS: &[(Option<&str>, &[(Section, &str)])] = &[
    (
        None,
        &[
            (Section::General, "General"),
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
            (Section::Threads, "Threads"),
            (Section::AcpSessions, "ACP Sessions"),
            (Section::ScheduledTasks, "Scheduled tasks"),
        ],
    ),
    (
        Some("Code"),
        &[
            (Section::Projects, "Projects"),
            (Section::SourceControl, "Source control"),
            (Section::Integrations, "Integrations"),
        ],
    ),
    (
        Some("Connections"),
        &[
            (Section::Connection, "Connection"),
            (Section::Environments, "Environments"),
            (Section::Clients, "Authorized clients"),
            (Section::Secrets, "Secrets"),
        ],
    ),
    (
        None,
        &[
            (Section::Diagnostics, "Diagnostics"),
            (Section::Usage, "Usage Limits"),
            (Section::About, "About"),
            (Section::ArchivedThreads, "Archive"),
        ],
    ),
];

/// Per-section lucide glyph — web `SETTINGS_SECTION_ICONS` in
/// `SettingsSidebarNav.tsx` (size-4, `--sidebar-icon-color`).
fn section_icon(section: Section) -> Icon {
    match section {
        Section::General => Icon::Settings2,
        Section::Appearance => Icon::Palette,
        Section::ChatDefaults => Icon::MessageSquareText,
        Section::Keybindings => Icon::Keyboard,
        Section::Shortcuts => Icon::ScrollText,
        Section::Providers => Icon::Bot,
        Section::AcpSessions => Icon::Activity,
        Section::Threads => Icon::MessagesSquare,
        Section::ScheduledTasks => Icon::CalendarClock,
        Section::Projects => Icon::PanelsTopLeft,
        Section::SourceControl => Icon::GitBranch,
        Section::Integrations => Icon::Link,
        Section::Connection => Icon::Globe,
        Section::Environments => Icon::Server,
        Section::Clients => Icon::ShieldCheck,
        Section::Secrets => Icon::KeyRound,
        Section::Diagnostics => Icon::Wrench,
        Section::Usage => Icon::ChartNoAxesColumn,
        Section::ArchivedThreads => Icon::Archive,
        Section::About => Icon::Info,
    }
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
    /// Nav search input (web `SettingsSidebarNav` search row) — created
    /// lazily on first open since `cx.new` needs a window context.
    pub search_field: Option<gpui::Entity<crate::text_field::TextField>>,
    /// Mirror of `search_field`'s contents; an observer syncs it so the
    /// `&self` nav render can filter.
    pub search_query: String,
    search_subs: Vec<gpui::Subscription>,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_state.open = !self.settings_state.open;
        if self.settings_state.open {
            self.ensure_settings_search_field(window, cx);
            self.settings_state.search_query.clear();
            if let Some(field) = self.settings_state.search_field.clone() {
                field.update(cx, |field, cx| field.reset(window, cx));
            }
        }
        cx.notify();
    }

    /// Lazily build the nav search `TextField` + the observer that mirrors
    /// its text into `search_query` (same pattern as palette.rs's field).
    fn ensure_settings_search_field(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_state.search_field.is_some() {
            return;
        }
        let field = cx.new(|cx| crate::text_field::TextField::new("Search", cx));
        self.settings_state
            .search_subs
            .push(cx.observe(&field, |this, field, cx| {
                let query = crate::palette::field_text(&field, cx);
                if this.settings_state.search_query != query {
                    this.settings_state.search_query = query;
                    cx.notify();
                }
            }));
        self.settings_state.search_field = Some(field);
    }

    /// Left nav — the web `SettingsSidebarNav`: w-60 bg-sidebar column with
    /// an `h-8 rounded-md` search row (SearchIcon + input + `/` Kbd), then
    /// `SidebarMenuButton` rows (h-8, rounded, px-2.5) grouped under muted
    /// headings. A non-empty query swaps the grouped nav for the flat
    /// filtered result list (web `isSearching`).
    fn render_settings_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.settings_state.search_query.trim().to_lowercase();
        let searching = !query.is_empty();
        let mut nav =
            div()
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
                    // Web search row: h-8 rounded-md px-2 py-1.5 gap-2, SearchIcon
                    // size-4 `text-sidebar-muted-foreground/80`, input "Search",
                    // `Kbd "/"` (decorative — "/" isn't bound natively).
                    div()
                        .h(px(32.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::sidebar_fg_muted())
                        .child(
                            div().flex_none().flex().items_center().child(
                                icon(Icon::Search, 16.).text_color(theme::sidebar_fg_muted()),
                            ),
                        )
                        .when_some(self.settings_state.search_field.clone(), |row, field| {
                            row.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(theme::sidebar_fg())
                                    .child(field),
                            )
                        })
                        .child(
                            div()
                                .flex_none()
                                .h(px(20.))
                                .min_w(px(20.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .px_1()
                                .rounded_sm()
                                .bg(theme::kbd_bg())
                                .text_xs()
                                .text_color(theme::sidebar_fg_muted())
                                .child("/"),
                        ),
                );

        if searching {
            // Web result rows: section icon + title + group-label sub-line.
            let mut any = false;
            let mut result_ix = 0usize;
            for (heading, items) in NAV_GROUPS {
                for (section, label) in items.iter() {
                    result_ix += 1;
                    let section = *section;
                    let haystack = format!(
                        "{} {}",
                        label.to_lowercase(),
                        heading.unwrap_or("").to_lowercase()
                    );
                    if !haystack.contains(&query) {
                        continue;
                    }
                    any = true;
                    let heading_label = heading.map(|h| h.to_string());
                    nav = nav.child(
                        div()
                            .id(("settings-search", result_ix))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px(px(10.))
                            .py_1()
                            .min_h(px(40.))
                            .rounded_lg()
                            .cursor_pointer()
                            .text_sm()
                            .hover(|s| s.bg(theme::sidebar_hover()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, window, cx| {
                                    this.settings_state.section = section;
                                    this.settings_state.search_query.clear();
                                    if let Some(field) = this.settings_state.search_field.clone() {
                                        field.update(cx, |field, cx| field.reset(window, cx));
                                    }
                                    cx.notify();
                                }),
                            )
                            .child(div().flex_none().flex().items_center().child(
                                icon(section_icon(section), 16.).text_color(theme::sidebar_icon()),
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .truncate()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(theme::sidebar_fg())
                                            .child(*label),
                                    )
                                    .when_some(heading_label, |col, h| {
                                        col.child(
                                            div()
                                                .truncate()
                                                .text_xs()
                                                .text_color(theme::sidebar_fg_muted())
                                                .child(h),
                                        )
                                    }),
                            ),
                    );
                }
            }
            if !any {
                nav = nav.child(
                    div()
                        .px_2()
                        .py_6()
                        .flex()
                        .justify_center()
                        .text_xs()
                        .text_color(theme::sidebar_fg_muted())
                        .child("No settings found"),
                );
            }
            return nav;
        }

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
                        .gap_2()
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
                        .child(div().flex_none().flex().items_center().child(
                            icon(section_icon(section), 16.).text_color(theme::sidebar_icon()),
                        ))
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
                            .flex()
                            .items_center()
                            .when(active, |el| {
                                el.child(icon(Icon::Check, 12.).text_color(theme::primary()))
                            }),
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
            Section::General => (
                "General",
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

        // Web header: h-12 border-b ps-5 pe-2 with a "Settings / <section>"
        // breadcrumb, "Restore device defaults" (ghost xs, only on
        // /settings/general), then `AppRouteModalClose` (ghost icon-sm X).
        let device_defaults_changed = self.client_config.notification_mode != ""
            || !self.client_config.in_app_notifications
            || self.client_config.diff_layout != "stacked"
            || !self.client_config.diff_ignore_whitespace
            || !self.client_config.diff_files_collapsed
            || self.client_config.proactive_panels
            || self.client_config.settings_scope_project.is_some()
            || !self.client_config.sidebar_project_grouping
            || self.client_config.sidebar_project_sort_order != "updated_at"
            || !self.client_config.sidebar_working_shelf
            || self.client_config.confirm_thread_unpin
            || self.client_config.confirm_thread_archive
            || !self.client_config.confirm_thread_delete
            || !self.client_config.timestamp_format.is_empty();
        let close_btn = div()
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
            .child(icon(Icon::X, 14.).text_color(theme::fg_muted()));
        let mut header_right = div().flex().items_center().gap_1();
        if self.settings_state.section == Section::General {
            // `RestoreDeviceDefaultsButton` — resets the device-local
            // (ClientSettings-mirroring) fields on ClientConfig and re-seeds
            // the misc settings mirror; ghost xs, disabled at defaults.
            header_right = header_right.child(
                div()
                    .id("settings-restore-defaults")
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .rounded_md()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .when(device_defaults_changed, |b| {
                        b.cursor_pointer()
                            .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                    })
                    .when(!device_defaults_changed, |b| b.opacity(0.5))
                    .child(icon(Icon::RefreshCw, 13.).text_color(theme::fg_muted()))
                    .child("Restore device defaults")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            if !device_defaults_changed {
                                return;
                            }
                            let cfg = &mut this.client_config;
                            cfg.notification_mode.clear();
                            cfg.in_app_notifications = true;
                            cfg.diff_layout = "stacked".to_string();
                            cfg.diff_ignore_whitespace = true;
                            cfg.diff_files_collapsed = true;
                            cfg.proactive_panels = false;
                            cfg.settings_scope_project = None;
                            cfg.sidebar_project_grouping = true;
                            cfg.sidebar_project_sort_order = "updated_at".to_string();
                            cfg.sidebar_working_shelf = true;
                            cfg.confirm_thread_unpin = false;
                            cfg.confirm_thread_archive = false;
                            cfg.confirm_thread_delete = true;
                            cfg.timestamp_format.clear();
                            cfg.save();
                            this.misc_settings_state.seed_from_config(cfg);
                            cx.notify();
                        }),
                    ),
            );
        }
        header_right = header_right.child(close_btn);
        let header = div()
            .h(px(48.))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .pl_5()
            .pr_2()
            .gap_3()
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
            .child(header_right);

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
                    .child(
                        // Web `SettingsPageContainer` → `WorkspacePageContainer`:
                        // `mx-auto w-full max-w-4xl gap-6 px-5 sm:px-6 pt-6
                        // pb-12`. The settings dialog only opens meaningfully
                        // above the sm breakpoint, so px-6 is the common case.
                        div()
                            .w_full()
                            .max_w(px(896.))
                            .mx_auto()
                            .flex()
                            .flex_col()
                            .px_6()
                            .pt_6()
                            .pb_12()
                            // Web pages carry the section name in the nav +
                            // "Settings / <section>" breadcrumb — there is no
                            // page-level h2 inside the content column.
                            .child(content),
                    ),
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
            .p(px(40.))
            .child(
                // Web `DialogPrimitive.Popup`: `size-full sm:max-w-[80rem]
                // sm:max-h-[60rem]` inside `lg:p-10` — 1200×720 at the
                // 1280×800 reference size.
                div()
                    .flex()
                    .size_full()
                    .max_w(px(1280.))
                    .max_h(px(960.))
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
