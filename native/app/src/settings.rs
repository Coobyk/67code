//! Settings surface: modal panel toggled with cmd-, or via palette.
//!
//! Left nav + right content layout mirroring the web settings dialogs —
//! the canonical `SETTINGS_NAV_GROUPS` from apps/web (General … SnapShots,
//! Agents, Code, Connections, Maintenance/Labs/About/Archive) with the
//! bottom-left avatar chip the web's `SidebarFooter` renders. Page
//! renderers live in settings_misc/settings_extra/settings_backend plus
//! the per-area modules (appearance, envs, clients, acp, integrations,
//! archived).

use std::cell::Cell;

use gpui::{
    Context, IntoElement, MouseButton, SharedString, Window, deferred, div, prelude::*, px, rgb,
};
use serde_json::json;

use crate::icons::{Icon, icon};
use crate::model::ConnectionStatus;
use crate::settings_misc::{setting_row, settings_card, settings_section_title, switch_control};
use crate::theme;
use crate::{AppView, ToggleSettings};

/// Sections listed in the settings left nav — the canonical web nav
/// (`SETTINGS_NAV_GROUPS` in apps/web settingsSearch.ts), one variant per
/// `/settings/<path>` page. Native-only sections were folded into the
/// matching web page: Connection → Environments, Authorized Clients →
/// Sharing & access, Diagnostics + Usage → Maintenance, Keyboard
/// Shortcuts → Keybindings, ACP Sessions → Providers, Integrations →
/// Browser.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Section {
    /// The web's `/settings/general` page — notification preferences and
    /// device-local defaults (the native surface hosts the Notifications
    /// content there).
    #[default]
    General,
    Appearance,
    /// Web `/settings/chat` — composer, follow-ups, and activity prefs.
    Chat,
    /// Web `/settings/diffs` — "Diffs & pull requests".
    Diffs,
    Keybindings,
    Voice,
    SnapShots,
    Providers,
    Threads,
    Browser,
    Devices,
    ScheduledTasks,
    Projects,
    SourceControl,
    Worktrees,
    /// Web `/settings/connections` — the environment list plus this
    /// device's live connection status.
    Environments,
    /// Web `/settings/access` — authorized clients / sharing.
    SharingAccess,
    Secrets,
    /// Web `/settings/maintenance` — updates, background activity, plus
    /// the native diagnostics and usage panels folded in.
    Maintenance,
    Labs,
    About,
    /// Web `/settings/archived`.
    Archive,
}

/// Left-nav groups (heading, sections) — identical to the web
/// `SETTINGS_NAV_GROUPS`: an unheaded block (General … SnapShots),
/// Agents, Code, Connections, and a trailing unheaded block.
const NAV_GROUPS: &[(Option<&str>, &[(Section, &str)])] = &[
    (
        None,
        &[
            (Section::General, "General"),
            (Section::Appearance, "Appearance"),
            (Section::Chat, "Chat"),
            (Section::Diffs, "Diffs & pull requests"),
            (Section::Keybindings, "Keybindings"),
            (Section::Voice, "Voice"),
            (Section::SnapShots, "SnapShots"),
        ],
    ),
    (
        Some("Agents"),
        &[
            (Section::Providers, "Providers"),
            (Section::Threads, "Threads"),
            (Section::Browser, "Browser"),
            (Section::Devices, "Devices"),
            (Section::ScheduledTasks, "Scheduled tasks"),
        ],
    ),
    (
        Some("Code"),
        &[
            (Section::Projects, "Projects"),
            (Section::SourceControl, "Source control"),
            (Section::Worktrees, "Worktrees"),
        ],
    ),
    (
        Some("Connections"),
        &[
            (Section::Environments, "Environments"),
            (Section::SharingAccess, "Sharing & access"),
            (Section::Secrets, "Secrets"),
        ],
    ),
    (
        None,
        &[
            (Section::Maintenance, "Maintenance"),
            (Section::Labs, "Labs"),
            (Section::About, "About"),
            (Section::Archive, "Archive"),
        ],
    ),
];

/// Per-section lucide glyph — web `SETTINGS_SECTION_ICONS` in
/// `SettingsSidebarNav.tsx` (size-4, `--sidebar-icon-color`).
fn section_icon(section: Section) -> Icon {
    match section {
        Section::General => Icon::Settings2,
        Section::Appearance => Icon::Palette,
        Section::Chat => Icon::MessageSquareText,
        Section::Diffs => Icon::FileDiff,
        Section::Keybindings => Icon::Keyboard,
        Section::Voice => Icon::Mic,
        Section::SnapShots => Icon::SnapShot,
        Section::Providers => Icon::Bot,
        Section::Threads => Icon::MessagesSquare,
        Section::Browser => Icon::Globe,
        Section::Devices => Icon::Smartphone,
        Section::ScheduledTasks => Icon::CalendarClock,
        Section::Projects => Icon::PanelsTopLeft,
        Section::SourceControl => Icon::GitBranch,
        Section::Worktrees => Icon::FolderTree,
        Section::Environments => Icon::Server,
        Section::SharingAccess => Icon::ShieldCheck,
        Section::Secrets => Icon::KeyRound,
        Section::Maintenance => Icon::Wrench,
        Section::Labs => Icon::FlaskConical,
        Section::About => Icon::Info,
        Section::Archive => Icon::Archive,
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
    /// About-page `server.getNpmRelease` check — fired once per session on
    /// first About render (Cell flags so the `&self` render can gate it,
    /// like the `maybe_load_*` probes elsewhere).
    pub about_update_checked: Cell<bool>,
    pub about_update_checking: Cell<bool>,
    /// Latest release version the registry reported (None = check ran but
    /// returned no version, or hasn't completed).
    pub about_latest: Option<String>,
    /// About "Update track" select open (self-contained — the shared
    /// `open_list` enum lives in settings_misc).
    pub about_track_open: bool,
    /// Environments page: index of the saved env whose ⋯ menu is open.
    pub env_menu_open: Option<usize>,
    /// About "c67 command" — last install/remove outcome, shown inline.
    pub cli_message: Option<SharedString>,
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

/// The canonical repo URL — the fork's home for "View policy" /
/// "View licenses" / "Download" (upstream web links point at t3.codes;
/// this build ships from GitHub).
const REPO_URL: &str = "https://github.com/Coobyk/67code";

/// About-page row: like `settings_misc::setting_row` but the title and
/// description take elements/strings built at render time (version
/// numbers, CLI paths, inline status lines).
fn about_row(
    title: impl IntoElement,
    description: impl IntoElement,
    control: impl IntoElement,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_8()
        .px_4()
        .py_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::fg())
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(description),
                ),
        )
        .child(control)
}

/// Web `Button variant="outline" size="sm"`: h-7 px-3 rounded-md border.
fn outline_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    glyph: Option<Icon>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(28.))
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .px_3()
        .rounded_md()
        .border_1()
        .border_color(theme::input())
        .text_xs()
        .cursor_pointer()
        .text_color(theme::fg())
        .hover(|s| s.bg(theme::accent()))
        .when_some(glyph, |b, g| b.child(icon(g, 13.).text_color(theme::fg())))
        .child(label)
}

/// True when `url` points at this machine — the launcher-started
/// in-process server. Loopback endpoints back the "This app / Local
/// environment" row rather than a saved-env row.
fn is_loopback_url(url: &str) -> bool {
    url.contains("localhost") || url.contains("127.0.0.1") || url.contains("[::1]")
}

/// `http://host:port/path` → `host:port` for the env-row transport label.
fn url_authority(url: &str) -> String {
    let rest = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    rest.trim_end_matches('/')
        .split('/')
        .next()
        .unwrap_or(rest)
        .to_string()
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
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
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
            return self.settings_nav_shell(nav);
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
        self.settings_nav_shell(nav)
    }

    /// Sidebar shell: the scrollable nav on top and the `SidebarFooter`
    /// avatar chip pinned bottom-left (web `T3ConnectSidebarAvatar` — a
    /// size-6 rounded-full user mark; native has no account, so it shows the
    /// connected environment's initial).
    fn settings_nav_shell(&self, nav: gpui::Stateful<gpui::Div>) -> gpui::Div {
        let initial: SharedString = self
            .endpoint_label
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_ascii_uppercase().to_string())
            .unwrap_or_else(|| "T".to_string())
            .into();
        div()
            .flex()
            .flex_col()
            .w(px(240.))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(theme::border())
            .bg(theme::sidebar())
            .child(nav)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .child(
                        div()
                            .w(px(24.))
                            .h(px(24.))
                            .flex_none()
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme::primary())
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::canvas())
                            .child(initial),
                    ),
            )
    }

    /// One saved-environment row — the web `EnvironmentRow`: machine glyph,
    /// name + `transport · status · Routes › · Permissions ›` subtitle,
    /// enable switch, ⋯ menu (deferred popup with Connect / Remove).
    fn render_env_row(&self, ix: usize, cx: &mut Context<Self>) -> gpui::Div {
        let env = self.client_config.environments[ix].clone();
        let active = self.is_env_active(ix);
        let armed = self.env_state.confirm_remove == Some(ix);
        let menu_open = self.settings_state.env_menu_open == Some(ix);

        // Transport label — `environmentTransportLabel`: our saved envs
        // carry a direct URL, so the label is the endpoint authority.
        let transport = url_authority(&env.http_url);
        let status = if active {
            connection_status(&self.state.connection).1
        } else {
            "Off"
        };
        let subtitle: SharedString =
            format!("{transport} · {status} · Routes › · Permissions ›").into();

        let switch = div()
            .id(("env-switch", ix))
            .cursor_pointer()
            .flex_none()
            .child(switch_control(active))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _window, cx| {
                    // The switch mirrors "is this the live endpoint": a
                    // click on an off row connects it; the live row stays
                    // on (there's no disconnect — pick another env).
                    if !this.is_env_active(ix) {
                        this.connect_environment(ix, cx);
                    }
                }),
            );

        let mut menu_list = div()
            .min_w(px(140.))
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .shadow_2xl()
            .flex()
            .flex_col();
        if !active {
            menu_list = menu_list.child(
                div()
                    .id(("env-menu-connect", ix))
                    .flex()
                    .items_center()
                    .px_2()
                    .py_1()
                    .min_h(px(28.))
                    .rounded_sm()
                    .text_sm()
                    .cursor_pointer()
                    .text_color(theme::fg())
                    .hover(|s| s.bg(theme::accent()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.settings_state.env_menu_open = None;
                            this.connect_environment(ix, cx);
                        }),
                    )
                    .child("Connect"),
            );
        }
        // Remove: refused on the live endpoint, two-stage arm/confirm
        // elsewhere (same state the old inline buttons used).
        let remove_item = div()
            .id(("env-menu-remove", ix))
            .flex()
            .items_center()
            .px_2()
            .py_1()
            .min_h(px(28.))
            .rounded_sm()
            .text_sm()
            .child(if armed { "Confirm remove" } else { "Remove" });
        let remove_item = if active {
            remove_item.text_color(theme::fg_muted()).opacity(0.5)
        } else {
            remove_item
                .cursor_pointer()
                .text_color(crate::settings_misc::danger())
                .hover(|s| s.bg(theme::accent()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        this.remove_environment(ix, cx);
                        // Armed → stay open for the confirm click; removed
                        // (or refused) → close.
                        if this.env_state.confirm_remove != Some(ix) {
                            this.settings_state.env_menu_open = None;
                        }
                        cx.notify();
                    }),
                )
        };
        menu_list = menu_list.child(remove_item);

        let menu_button = div().relative().flex_none().child(
            div()
                .id(("env-menu", ix))
                .w(px(28.))
                .h(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .cursor_pointer()
                .text_color(theme::fg_muted())
                .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        this.settings_state.env_menu_open =
                            if this.settings_state.env_menu_open == Some(ix) {
                                None
                            } else {
                                Some(ix)
                            };
                        this.env_state.confirm_remove = None;
                        cx.notify();
                    }),
                )
                .child(icon(Icon::Ellipsis, 14.).text_color(theme::fg_muted())),
        );
        let menu_button = menu_button.when(menu_open, |cell| {
            cell.child(
                deferred(
                    div()
                        .absolute()
                        .top_full()
                        .right_0()
                        .pt_1()
                        .child(menu_list),
                )
                .with_priority(2),
            )
        });

        div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_2p5()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(icon(Icon::Server, 16.).text_color(theme::fg_muted())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::fg())
                            .child(env.label.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(subtitle),
                    ),
            )
            .child(switch)
            .child(menu_button)
    }

    /// Environments page — the web `/settings/connections`: a "This app"
    /// card with the Local environment switch, then the Environments list
    /// (`EnvironmentRow` cards) with a "+ Add environment" header action.
    fn render_environments_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let local_on = self.client_config.local_environment_enabled;

        // Web `LocalEnvironmentSetting`: the switch mirrors the startup
        // state; the persisted flag applies on next launch (the web
        // restarts the app to apply it — the launcher reads the flag then).
        let local_switch = div()
            .id("env-local-switch")
            .cursor_pointer()
            .flex_none()
            .child(switch_control(local_on))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, _window, cx| {
                    this.client_config.local_environment_enabled =
                        !this.client_config.local_environment_enabled;
                    this.client_config.save();
                    cx.notify();
                }),
            );
        let this_app = div()
            .flex()
            .flex_col()
            .child(settings_section_title("This app"))
            .child(settings_card(vec![setting_row(
                "Local environment",
                if local_on {
                    "Run agents on this computer. Turn off to use 67 Code only with remote environments."
                } else {
                    "Turned off. Agents only run in remote environments."
                },
                local_switch,
            )]));

        // "Environments" heading + right-aligned add action (web
        // `AddEnvironmentDialog` trigger).
        let env_header = div()
            .flex()
            .items_center()
            .justify_between()
            .pr_4()
            .child(settings_section_title("Environments"))
            .child(
                div()
                    .id("env-add-open")
                    .h(px(28.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .px_2()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                    .child("+ Add environment")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, window, cx| {
                            this.open_env_form(window, cx);
                        }),
                    ),
            );

        let mut list = div().flex().flex_col().gap_2();
        for ix in 0..self.client_config.environments.len() {
            list = list.child(settings_card(vec![self.render_env_row(ix, cx)]));
        }

        // A live endpoint that no saved env covers — e.g. supplied via
        // C67_SERVER_URL/C67_TOKEN — shows read-only. Loopback endpoints
        // are this app's own server (the "Local environment" row above).
        let live_url = self.active_endpoint.as_ref().map(|e| e.http_url.clone());
        let live_is_saved = live_url.as_ref().is_some_and(|url| {
            self.client_config
                .environments
                .iter()
                .any(|e| &e.http_url == url)
        });
        let unsaved_live_url = live_url
            .as_ref()
            .filter(|url| !live_is_saved && !is_loopback_url(url));
        if let Some(url) = unsaved_live_url {
            let (_, status_label, _) = connection_status(&self.state.connection);
            let subtitle: SharedString = format!(
                "{} · {status_label} · Routes › · Permissions ›",
                url_authority(&url)
            )
            .into();
            list = list.child(settings_card(vec![
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_2p5()
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .child(icon(Icon::Server, 16.).text_color(theme::fg_muted())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme::fg())
                                    .child(url_authority(&url)),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme::fg_muted())
                                    .child(subtitle),
                            ),
                    )
                    .child(switch_control(true)),
            ]));
        }

        if self.client_config.environments.is_empty() && live_url.is_none() {
            list = list.child(
                div()
                    .px_4()
                    .py_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(
                        "No saved environments yet — add one to switch servers without restarting.",
                    ),
            );
        }

        let mut page = div()
            .flex()
            .flex_col()
            .gap_6()
            .child(this_app)
            .child(div().flex().flex_col().child(env_header).child(list));

        // The add form replaces the row list's trailing card while open
        // (inline — the web dialog's contents).
        if self.env_state.add_open {
            page = page.child(settings_card(vec![
                div()
                    .px_4()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("New environment"),
                    )
                    .child(self.render_env_add_form(cx)),
            ]));
        }

        page
    }

    /// Keybindings page — the editable keybinding list plus the native-only
    /// "Keyboard Shortcuts" static reference folded in at the bottom.
    fn render_keybindings_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(self.render_keybindings_section(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(crate::settings_misc::settings_section_title(
                        "Built-in shortcuts",
                    ))
                    .child(crate::settings_misc::settings_card(vec![
                        div().px_4().py_3().child(self.render_shortcuts_section()),
                    ])),
            )
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

    /// Fire the About page's `server.getNpmRelease` check. Gated by the
    /// Cell flags so first render starts it and later renders no-op;
    /// "Check for updates" clears `about_update_checked` first to re-run.
    fn start_about_update_check(&self, cx: &mut Context<Self>) {
        let state = &self.settings_state;
        if state.about_update_checked.get() || state.about_update_checking.get() {
            return;
        }
        let Some(rx) = self.send_query("server.getNpmRelease", json!({ "spec": "latest" })) else {
            state.about_update_checked.set(true);
            return;
        };
        state.about_update_checking.set(true);
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.settings_state;
                state.about_update_checking.set(false);
                state.about_update_checked.set(true);
                state.about_latest = match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        exit["value"]["version"].as_str().map(str::to_string)
                    }
                    _ => None,
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// About page — the web `AboutSettingsPanel`: an "App" card (Version +
    /// update check, Update track select, `c67` command install row, Mobile
    /// app store links, Welcome page) and a "Legal" card (privacy policy,
    /// open-source licenses).
    fn render_about_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        self.start_about_update_check(cx);

        // ── Version + update check ───────────────────────────────────
        // The release the check compares against is the server's (the
        // launcher ships app + server together); fall back to this
        // binary's version when no server reports one.
        let version: SharedString = self
            .misc_settings_state
            .server_version
            .clone()
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
            .into();
        let checking = self.settings_state.about_update_checking.get();
        let checked = self.settings_state.about_update_checked.get();
        let latest = self.settings_state.about_latest.clone();
        let update_available = latest
            .as_ref()
            .is_some_and(|l| l.trim_start_matches('v') != version.trim_start_matches('v'));
        let connected = self.commands.is_some();

        let update_desc: SharedString = if checking {
            "Checking for updates…".into()
        } else if update_available {
            "Update available.".into()
        } else if checked && latest.is_none() && !connected {
            "Update checks need a connected server.".into()
        } else if checked && latest.is_none() {
            "No release info from the registry.".into()
        } else {
            "Current version of the application.".into()
        };

        // Web `resolveDesktopUpdateButtonAction` + labels: Download when a
        // newer release exists, "Up to Date" when current, otherwise a
        // manual "Check for updates".
        let version_control: gpui::AnyElement = if checking {
            outline_button("about-update-btn", "Checking…", None)
                .opacity(0.5)
                .into_any_element()
        } else if update_available {
            outline_button("about-update-btn", "Download", None)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_this, _ev, _window, _cx| {
                        crate::explorer::open_external(&format!("{REPO_URL}/releases"));
                    }),
                )
                .into_any_element()
        } else if checked && latest.is_some() {
            outline_button("about-update-btn", "Up to Date", None)
                .opacity(0.5)
                .into_any_element()
        } else {
            let btn = outline_button("about-update-btn", "Check for updates", None);
            let btn = if connected {
                btn.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _window, cx| {
                        this.settings_state.about_update_checked.set(false);
                        this.settings_state.about_latest = None;
                        this.start_about_update_check(cx);
                    }),
                )
            } else {
                btn.opacity(0.5)
            };
            btn.into_any_element()
        };

        let version_title = div()
            .flex()
            .items_baseline()
            .gap_2()
            .child("Version")
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::fg_muted())
                    .child(version),
            );

        // ── Update track ─────────────────────────────────────────────
        let track = if self.client_config.update_track == "stable" {
            "Stable"
        } else {
            "Nightly"
        };
        let mut track_list = div()
            .min_w(px(180.))
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .shadow_2xl()
            .flex()
            .flex_col();
        for (index, (value, label)) in [("stable", "Stable"), ("nightly", "Nightly")]
            .into_iter()
            .enumerate()
        {
            let selected = value == self.client_config.update_track;
            track_list = track_list.child(
                div()
                    .id(("about-track", index))
                    .flex()
                    .items_center()
                    .px_2()
                    .py_1()
                    .min_h(px(28.))
                    .rounded_sm()
                    .text_sm()
                    .cursor_pointer()
                    .text_color(theme::fg())
                    .when(selected, |s| s.bg(theme::selected()))
                    .when(!selected, |s| s.hover(|s| s.bg(theme::accent())))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.client_config.update_track = value.to_string();
                            this.client_config.save();
                            this.settings_state.about_track_open = false;
                            cx.notify();
                        }),
                    )
                    .child(label),
            );
        }
        let track_cell =
            div()
                .relative()
                .flex_none()
                .child(
                    div()
                        .id("about-track-select")
                        .min_w(px(160.))
                        .h(px(32.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .px_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme::input())
                        .bg(theme::field_surface())
                        .text_sm()
                        .cursor_pointer()
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::accent()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme::fg())
                                .child(track),
                        )
                        .child(
                            div().flex_none().flex().items_center().child(
                                icon(Icon::ChevronDown, 12.).text_color(theme::icon_muted()),
                            ),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _window, cx| {
                                this.settings_state.about_track_open =
                                    !this.settings_state.about_track_open;
                                cx.notify();
                            }),
                        ),
                )
                .when(self.settings_state.about_track_open, |cell| {
                    cell.child(
                        deferred(
                            div()
                                .absolute()
                                .top_full()
                                .right_0()
                                .pt_1()
                                .child(track_list),
                        )
                        .with_priority(2),
                    )
                });

        // ── c67 command (web `CliCommandSettingsRow`) ─────────────────
        // Install target is `~/.local/bin/c67` → the running binary; a
        // different `c67` earlier on PATH is "shadowed by".
        let home = std::env::var("HOME").unwrap_or_default();
        let install_path = format!("{home}/.local/bin/c67");
        let installed = std::fs::symlink_metadata(&install_path).is_ok();
        let path_dirs: Vec<std::path::PathBuf> = std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .filter(|d| !d.is_empty())
            .map(std::path::PathBuf::from)
            .collect();
        let install_dir = format!("{home}/.local/bin");
        let on_path = path_dirs
            .iter()
            .any(|d| d.as_os_str() == install_dir.as_str());
        let which_c67 = path_dirs.iter().find_map(|d| {
            let candidate = d.join("c67");
            let meta = std::fs::metadata(&candidate).ok()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                    return Some(candidate.display().to_string());
                }
            }
            #[cfg(not(unix))]
            {
                if meta.is_file() {
                    return Some(candidate.display().to_string());
                }
            }
            None
        });
        let shadowed_by = which_c67.filter(|p| p != &install_path);

        let cli_desc: SharedString = if let Some(shadowed) = &shadowed_by {
            format!(
                "Another c67 at {shadowed} runs first in a new terminal. Remove it to use 67 Code's."
            )
            .into()
        } else if !installed {
            "Run 67 Code's CLI as `c67` from any terminal.".into()
        } else if on_path {
            format!("Installed at {install_path}. Open a new terminal to use it.").into()
        } else {
            format!(
                "Installed at {install_path}, which is not on your PATH yet. Add its folder to your PATH to run `c67`."
            )
            .into()
        };
        let cli_desc_el = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(cli_desc)
            .when_some(self.settings_state.cli_message.clone(), |col, msg| {
                col.child(div().text_xs().text_color(theme::fg_muted()).child(msg))
            });

        let cli_control: gpui::AnyElement = if installed {
            outline_button("about-cli-btn", "Remove", None)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        let path = install_path.clone();
                        this.settings_state.cli_message = Some(match std::fs::remove_file(&path) {
                            Ok(()) => format!("Removed {path}.").into(),
                            Err(e) => format!("Could not remove {path}: {e}.").into(),
                        });
                        cx.notify();
                    }),
                )
                .into_any_element()
        } else {
            let btn = outline_button("about-cli-btn", "Install", None);
            let btn = if shadowed_by.is_some() {
                // Web disables install while another binary shadows the
                // command — installing wouldn't put ours first anyway.
                btn.opacity(0.5)
            } else {
                btn.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        let message: SharedString = 'install: {
                            let Some(dir) = std::path::Path::new(&install_path).parent() else {
                                break 'install "Install path has no parent.".into();
                            };
                            if let Err(e) = std::fs::create_dir_all(dir) {
                                break 'install format!("Could not create {}: {e}.", dir.display())
                                    .into();
                            }
                            let Ok(exe) = std::env::current_exe() else {
                                break 'install "Could not locate the app binary.".into();
                            };
                            let _ = std::fs::remove_file(&install_path);
                            match std::os::unix::fs::symlink(&exe, &install_path) {
                                Ok(()) => format!(
                                    "Installed at {install_path}. Open a new terminal to use it."
                                )
                                .into(),
                                Err(e) => format!("Could not install: {e}.").into(),
                            }
                        };
                        this.settings_state.cli_message = Some(message);
                        cx.notify();
                    }),
                )
            };
            btn.into_any_element()
        };

        // ── Assemble cards ───────────────────────────────────────────
        let app_card = settings_card(vec![
            about_row(version_title, update_desc, version_control),
            about_row(
                "Update track",
                "Switches this app and every connected machine whose database the target release supports.",
                track_cell,
            ),
            about_row("c67 command", cli_desc_el, cli_control),
            about_row(
                "Mobile app",
                "Connect to your server from your phone or tablet.",
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        outline_button("about-mobile-ios", "iPhone and iPad", Some(Icon::Apple))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_this, _ev, _window, _cx| {
                                    crate::explorer::open_external(REPO_URL);
                                }),
                            ),
                    )
                    .child(
                        outline_button("about-mobile-android", "Android", Some(Icon::Android))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_this, _ev, _window, _cx| {
                                    crate::explorer::open_external(REPO_URL);
                                }),
                            ),
                    ),
            ),
            about_row(
                "Welcome page",
                "See your agents, import projects, and set up your phone.",
                outline_button("about-welcome-open", "Open", None).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _window, cx| {
                        this.welcome_state.open = true;
                        this.settings_state.open = false;
                        cx.notify();
                    }),
                ),
            ),
        ]);

        let legal_card = settings_card(vec![
            about_row(
                "Privacy policy",
                "How we handle your data, including the anonymous usage data 67 Code collects.",
                outline_button("about-privacy", "View policy", None).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_this, _ev, _window, _cx| {
                        crate::explorer::open_external(REPO_URL);
                    }),
                ),
            ),
            about_row(
                "Open source licenses",
                "Notices for dependencies, assets, and optional tools used by 67 Code.",
                outline_button("about-licenses", "View licenses", None).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_this, _ev, _window, _cx| {
                        crate::explorer::open_external(&format!(
                            "{REPO_URL}/tree/main/third-party-licenses"
                        ));
                    }),
                ),
            ),
        ]);

        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(settings_section_title("App"))
                    .child(app_card),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(settings_section_title("Legal"))
                    .child(legal_card),
            )
    }

    fn render_settings_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (heading, content) = match self.settings_state.section {
            Section::Appearance => (
                "Appearance",
                self.render_appearance_section(cx).into_any_element(),
            ),
            Section::Environments => (
                "Environments",
                self.render_environments_page(cx).into_any_element(),
            ),
            Section::Providers => (
                "Providers",
                self.render_providers_section(cx).into_any_element(),
            ),
            Section::SharingAccess => (
                "Sharing & access",
                self.render_clients_section(cx).into_any_element(),
            ),
            Section::Archive => (
                "Archive",
                self.render_archived_section(cx).into_any_element(),
            ),
            Section::General => (
                "General",
                self.render_notifications_section(cx).into_any_element(),
            ),
            Section::Chat => ("Chat", self.render_chat_section(cx).into_any_element()),
            Section::Diffs => (
                "Diffs & pull requests",
                self.render_diffs_section(cx).into_any_element(),
            ),
            Section::Voice => ("Voice", self.render_voice_section(cx).into_any_element()),
            Section::SnapShots => (
                "SnapShots",
                self.render_snapshots_section(cx).into_any_element(),
            ),
            Section::Browser => (
                "Browser",
                self.render_browser_section(cx).into_any_element(),
            ),
            Section::Devices => (
                "Devices",
                self.render_devices_section(cx).into_any_element(),
            ),
            Section::Worktrees => (
                "Worktrees",
                self.render_worktrees_section(cx).into_any_element(),
            ),
            Section::Maintenance => (
                "Maintenance",
                self.render_maintenance_section(cx).into_any_element(),
            ),
            Section::Labs => ("Labs", self.render_labs_section(cx).into_any_element()),
            Section::Projects => (
                "Projects",
                self.render_projects_section(cx).into_any_element(),
            ),
            Section::ScheduledTasks => (
                "Scheduled tasks",
                self.render_tasks_section(cx).into_any_element(),
            ),
            Section::Secrets => (
                "Secrets",
                self.render_secrets_section(cx).into_any_element(),
            ),
            Section::Keybindings => (
                "Keybindings",
                self.render_keybindings_page(cx).into_any_element(),
            ),
            Section::SourceControl => (
                "Source control",
                self.render_source_control_section(cx).into_any_element(),
            ),
            Section::Threads => (
                "Threads",
                self.render_threads_section(cx).into_any_element(),
            ),
            Section::About => ("About", self.render_about_section(cx).into_any_element()),
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
