//! Authorized clients settings section (P2): paired device sessions with
//! revoke / revoke-others + pairing-link creation — mirroring
//! AuthorizedClientsSettings.tsx. Live data rides the `subscribeAuthAccess`
//! stream (ShellEvent::AuthAccessChange → observe_auth_access); revokes and
//! link creation are authenticated HTTP POSTs (ureq is blocking, so calls
//! run inside `smol::unblock` on a spawned task).

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    ClipboardItem, Context, Entity, EntityInputHandler, Focusable, IntoElement, MouseButton,
    Subscription, Window, div, prelude::*, px, rgb,
};
use serde_json::{Value, json};

use crate::AppView;
use crate::net::ServerEndpoint;
use crate::text_field::{PromptSubmitted, TextField};
use crate::theme;

const DANGER: u32 = 0xf87171;
const DANGER_BORDER: u32 = 0x7a2e2e;
const SUCCESS: u32 = 0x34d399;

/// (scope, title, description) — mirrors `AUTH_SCOPE_OPTIONS` in
/// packages/shared/src/authScopeOptions.ts.
const SCOPE_OPTIONS: &[(&str, &str, &str)] = &[
    (
        "orchestration:read",
        "View environment",
        "Read threads, status, checkpoints, and configuration.",
    ),
    (
        "orchestration:operate",
        "Operate tasks",
        "Start, update, and stop tasks.",
    ),
    (
        "settings:write",
        "Change environment settings",
        "Edit environment preferences and keybindings.",
    ),
    (
        "providers:manage",
        "Manage providers",
        "Configure, install, sign in to, and update providers and usage sources.",
    ),
    (
        "environment:maintain",
        "Maintain environment",
        "Update the server and control environment processes.",
    ),
    (
        "preview:operate",
        "Control previews",
        "Open browser previews and host browser automation.",
    ),
    (
        "diagnostics:read",
        "View diagnostics and usage",
        "Read process diagnostics, resource history, and usage totals.",
    ),
    (
        "terminal:read",
        "View terminals",
        "Read existing terminal output and status.",
    ),
    (
        "terminal:operate",
        "Use terminals",
        "Create terminals and send input to running shells.",
    ),
    (
        "source-control:write",
        "Change source control",
        "Commit, push, manage branches and repositories, and change pull requests.",
    ),
    (
        "filesystem:read",
        "Read files",
        "Browse host files, search workspaces, and inspect local changes.",
    ),
    (
        "filesystem:write",
        "Write files",
        "Edit workspace files and save plans to disk.",
    ),
    (
        "access:read",
        "View access",
        "Inspect pairing links and authorized clients.",
    ),
    (
        "access:write",
        "Manage access",
        "Issue and revoke credentials for other clients.",
    ),
    (
        "relay:read",
        "View relay",
        "Inspect managed relay connectivity.",
    ),
    (
        "relay:write",
        "Manage relay",
        "Change managed tunnel connectivity.",
    ),
];

/// `AuthStandardClientScopes` — every option, checked by default.
const STANDARD_SCOPES: &[&str] = &[
    "orchestration:read",
    "orchestration:operate",
    "settings:write",
    "providers:manage",
    "environment:maintain",
    "preview:operate",
    "diagnostics:read",
    "terminal:read",
    "terminal:operate",
    "source-control:write",
    "filesystem:read",
    "filesystem:write",
    "relay:read",
    "access:read",
    "access:write",
    "relay:write",
];

/// The web dialog's "Read only" preset.
const READ_ONLY_SCOPES: &[&str] = &[
    "orchestration:read",
    "filesystem:read",
    "diagnostics:read",
    "terminal:read",
];

pub struct State {
    /// Raw client-session payloads from the auth-access stream.
    pub sessions: Vec<serde_json::Value>,
    /// Raw pairing-link payloads.
    pub pairing_links: Vec<serde_json::Value>,
    /// Session or pairing-link id currently being revoked (disables its row).
    pub revoking: Option<String>,
    /// Revoke-others request in flight.
    pub revoking_others: bool,
    /// At least one snapshot/upsert has arrived.
    pub loaded: bool,
    /// "Create link" inline panel is open.
    pub create_open: bool,
    /// Label input, created lazily on first open — `cx.new` needs a context.
    pub label_field: Option<Entity<TextField>>,
    /// Scopes selected for the pairing link being drafted.
    pub draft_scopes: Vec<String>,
    /// Pairing-link POST in flight.
    pub creating: bool,
    /// Freshly minted pairing URL shown for copying (dismissed via Done).
    pub created_url: Option<String>,
    /// The created URL was copied once — button shows "Copied".
    pub copied: bool,
    /// Latest auth HTTP error, rendered under the header.
    pub error: Option<String>,
    subs: Vec<Subscription>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            pairing_links: Vec::new(),
            revoking: None,
            revoking_others: false,
            loaded: false,
            create_open: false,
            label_field: None,
            draft_scopes: STANDARD_SCOPES.iter().map(|s| s.to_string()).collect(),
            creating: false,
            created_url: None,
            copied: false,
            error: None,
            subs: Vec::new(),
        }
    }
}

/// `sessionId` (client sessions) or `id` (tolerant of either spelling).
fn session_id(session: &Value) -> &str {
    session["sessionId"]
        .as_str()
        .or_else(|| session["id"].as_str())
        .unwrap_or_default()
}

/// Display label: `client.label` → `label` → "os · browser" → subject → id,
/// matching `primaryLabel` in ConnectedClientListRow.
fn session_label(session: &Value) -> String {
    let client = &session["client"];
    if let Some(label) = client["label"]
        .as_str()
        .or_else(|| session["label"].as_str())
    {
        return label.to_string();
    }
    let os_browser = [client["os"].as_str(), client["browser"].as_str()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    if !os_browser.is_empty() {
        return os_browser;
    }
    session["subject"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| session_id(session).to_string())
}

/// `scopes` array, falling back to `permissions` (authScopeResponse carries
/// both; older payloads may only have one).
fn scope_list(v: &Value) -> Vec<String> {
    for key in ["scopes", "permissions"] {
        let scopes: Vec<String> = v[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if !scopes.is_empty() {
            return scopes;
        }
    }
    Vec::new()
}

/// "a · b" for tiny lists, "N scopes" otherwise (the web shows N + popover).
fn scope_summary(v: &Value) -> String {
    let scopes = scope_list(v);
    match scopes.len() {
        0 => "no scopes".to_string(),
        1 | 2 => scopes.join(" · "),
        n => format!("{n} scopes"),
    }
}

/// deviceType (capitalized, skip "unknown") · os · browser · ipAddress.
fn device_bits(session: &Value) -> Vec<String> {
    let client = &session["client"];
    let mut bits = Vec::new();
    if let Some(device_type) = client["deviceType"].as_str() {
        if device_type != "unknown" {
            let mut chars = device_type.chars();
            let capitalized = chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>())
                .unwrap_or_default()
                + chars.as_str();
            bits.push(capitalized);
        }
    }
    for key in ["os", "browser", "ipAddress"] {
        if let Some(value) = client[key].as_str() {
            bits.push(value.to_string());
        }
    }
    bits
}

/// Days since epoch for a proleptic-Gregorian date (days-from-civil).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse the leading `YYYY-MM-DDTHH:MM:SS` of an ISO-8601 timestamp as UTC
/// (display-only; fractional seconds and zone offsets are ignored).
fn parse_iso_epoch(iso: &str) -> Option<i64> {
    if iso.len() < 19 {
        return None;
    }
    let year: i64 = iso.get(0..4)?.parse().ok()?;
    let month: i64 = iso.get(5..7)?.parse().ok()?;
    let day: i64 = iso.get(8..10)?.parse().ok()?;
    let hour: i64 = iso.get(11..13)?.parse().ok()?;
    let minute: i64 = iso.get(14..16)?.parse().ok()?;
    let second: i64 = iso.get(17..19)?.parse().ok()?;
    Some(days_from_civil(year, month, day) * 86400 + hour * 3600 + minute * 60 + second)
}

fn humanize(seconds: i64) -> String {
    if seconds < 90 {
        format!("{seconds}s")
    } else if seconds < 90 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 36 * 3600 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}

/// "5m ago" / "in 45m" for an ISO timestamp, or None when unparseable.
fn relative_time(iso: &str) -> Option<String> {
    let ts = parse_iso_epoch(iso)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    let diff = now - ts;
    Some(if diff < -60 {
        format!("in {}", humanize(-diff))
    } else if diff < 60 {
        "just now".to_string()
    } else {
        format!("{} ago", humanize(diff))
    })
}

/// Authenticated POST against the connected environment (blocking — always
/// call inside `smol::unblock`, never on the UI thread).
fn auth_post(endpoint: &ServerEndpoint, path: &str, body: Value) -> Result<Value, String> {
    let url = format!("{}{}", endpoint.http_url.trim_end_matches('/'), path);
    let mut response = ureq::post(&url)
        .header("authorization", format!("Bearer {}", endpoint.token))
        .header("content-type", "application/json")
        .send_json(body)
        .map_err(|e| format!("{e}"))?;
    response
        .body_mut()
        .read_json::<Value>()
        .map_err(|e| format!("response decode failed: {e}"))
}

/// Small uppercase-ish caption above a group of rows (settings.rs style).
fn group_label(text: &'static str) -> gpui::Div {
    div()
        .pb_1()
        .pt_3()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(text)
}

/// Reconnect-button styling; `danger` renders the destructive-outline look.
/// Caller attaches `.id(...)` and the click listener.
fn action_button(label: &'static str, enabled: bool, danger: bool) -> gpui::Div {
    let button = div()
        .px_3()
        .py_1p5()
        .rounded_md()
        .text_xs()
        .text_color(if danger { rgb(DANGER) } else { theme::fg() });
    let button = if danger {
        button.border_1().border_color(rgb(DANGER_BORDER))
    } else {
        button.bg(theme::hover())
    };
    if enabled {
        button.cursor_pointer().hover(|s| s.bg(theme::selected()))
    } else {
        button.opacity(0.5)
    }
    .child(label)
}

fn checkbox(checked: bool) -> gpui::Div {
    div()
        .w(px(14.))
        .h(px(14.))
        .flex_none()
        .rounded_sm()
        .border_1()
        .border_color(if checked {
            theme::primary()
        } else {
            theme::border()
        })
        .when(checked, |d| d.bg(theme::primary()))
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(gpui::white())
        .child(if checked { "✓" } else { "" })
}

impl AppView {
    /// Fold a raw auth-access stream value into state
    /// (snapshot / clientUpserted / clientRemoved / pairingLink*).
    pub fn observe_auth_access(&mut self, value: &serde_json::Value, cx: &mut Context<Self>) {
        let state = &mut self.clients_state;

        // Snapshot-ish values replace both lists. The stream's initial value
        // is {type:"snapshot", payload:{pairingLinks, clientSessions}}; be
        // tolerant of a bare object carrying the lists directly.
        let snapshot = if value["clientSessions"].is_array() || value["pairingLinks"].is_array() {
            Some(value.clone())
        } else if value["type"].as_str() == Some("snapshot") {
            Some(value["payload"].clone())
        } else {
            None
        };
        if let Some(snap) = snapshot {
            state.sessions = snap["clientSessions"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            state.pairing_links = snap["pairingLinks"].as_array().cloned().unwrap_or_default();
            state.loaded = true;
            cx.notify();
            return;
        }

        let payload = &value["payload"];
        match value["type"].as_str() {
            Some("clientUpserted") => {
                let id = session_id(payload).to_string();
                match state.sessions.iter().position(|s| session_id(s) == id) {
                    Some(i) => state.sessions[i] = payload.clone(),
                    None => state.sessions.push(payload.clone()),
                }
                state.loaded = true;
            }
            Some("clientRemoved") => {
                let id = payload["sessionId"]
                    .as_str()
                    .or_else(|| payload["id"].as_str())
                    .unwrap_or_default()
                    .to_string();
                state.sessions.retain(|s| session_id(s) != id);
                if state.revoking.as_deref() == Some(id.as_str()) {
                    state.revoking = None;
                }
            }
            Some("pairingLinkUpserted") => {
                let id = payload["id"].as_str().unwrap_or_default().to_string();
                match state
                    .pairing_links
                    .iter()
                    .position(|l| l["id"].as_str() == Some(id.as_str()))
                {
                    Some(i) => state.pairing_links[i] = payload.clone(),
                    None => state.pairing_links.push(payload.clone()),
                }
            }
            Some("pairingLinkRemoved") => {
                let id = payload["id"]
                    .as_str()
                    .or_else(|| payload["linkId"].as_str())
                    .unwrap_or_default()
                    .to_string();
                state
                    .pairing_links
                    .retain(|l| l["id"].as_str() != Some(id.as_str()));
                if state.revoking.as_deref() == Some(id.as_str()) {
                    state.revoking = None;
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn revoke_client_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        let Some(endpoint) = self.active_endpoint.clone() else {
            self.clients_state.error = Some("Connect to a server first.".into());
            cx.notify();
            return;
        };
        self.clients_state.revoking = Some(session_id.clone());
        self.clients_state.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let ep = endpoint.clone();
            let sid = session_id.clone();
            let result = smol::unblock(move || {
                auth_post(&ep, "/api/auth/clients/revoke", json!({ "sessionId": sid }))
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.clients_state;
                state.revoking = None;
                match result {
                    // The stream's clientRemoved confirms; drop it locally
                    // too so the row is gone even if events lag.
                    Ok(_) => state
                        .sessions
                        .retain(|s| self::session_id(s) != session_id.as_str()),
                    Err(e) => state.error = Some(format!("Revoke failed: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn revoke_other_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(endpoint) = self.active_endpoint.clone() else {
            self.clients_state.error = Some("Connect to a server first.".into());
            cx.notify();
            return;
        };
        self.clients_state.revoking_others = true;
        self.clients_state.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || {
                auth_post(&endpoint, "/api/auth/clients/revoke-others", json!({}))
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.clients_state;
                state.revoking_others = false;
                match result {
                    Ok(_) => state
                        .sessions
                        .retain(|s| s["current"].as_bool() == Some(true)),
                    Err(e) => state.error = Some(format!("Revoke others failed: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn revoke_pairing_link(&mut self, link_id: String, cx: &mut Context<Self>) {
        let Some(endpoint) = self.active_endpoint.clone() else {
            self.clients_state.error = Some("Connect to a server first.".into());
            cx.notify();
            return;
        };
        self.clients_state.revoking = Some(link_id.clone());
        self.clients_state.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let ep = endpoint.clone();
            let lid = link_id.clone();
            let result = smol::unblock(move || {
                auth_post(&ep, "/api/auth/pairing-links/revoke", json!({ "id": lid }))
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.clients_state;
                state.revoking = None;
                match result {
                    Ok(_) => state
                        .pairing_links
                        .retain(|l| l["id"].as_str() != Some(link_id.as_str())),
                    Err(e) => state.error = Some(format!("Revoke failed: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Open/close the "Create link" panel, lazily building + focusing the
    /// label field (same pattern as `ensure_palette_field`).
    fn toggle_create_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        {
            let state = &mut self.clients_state;
            state.create_open = !state.create_open;
            state.error = None;
            if !state.create_open {
                cx.notify();
                return;
            }
            if state.label_field.is_none() {
                let field = cx.new(|cx| TextField::new("e.g. Living room iPad", cx));
                let sub = cx.subscribe_in(
                    &field,
                    window,
                    |this, _field, event: &PromptSubmitted, window, cx| {
                        this.submit_pairing_link(Some(event.0.to_string()), window, cx);
                    },
                );
                state.subs.push(sub);
                state.label_field = Some(field);
            }
        }
        if let Some(field) = self.clients_state.label_field.clone() {
            field.update(cx, |field, cx| field.reset(window, cx));
            field.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    fn toggle_draft_scope(&mut self, scope: &'static str, cx: &mut Context<Self>) {
        let scopes = &mut self.clients_state.draft_scopes;
        if let Some(ix) = scopes.iter().position(|s| s == scope) {
            scopes.remove(ix);
        } else {
            scopes.push(scope.to_string());
        }
        cx.notify();
    }

    fn set_draft_scopes(&mut self, scopes: &[&'static str], cx: &mut Context<Self>) {
        self.clients_state.draft_scopes = scopes.iter().map(|s| s.to_string()).collect();
        cx.notify();
    }

    /// Current label-input contents (None when the field was never built).
    fn pairing_label_text(&mut self, window: &mut Window, cx: &mut Context<Self>) -> String {
        self.clients_state
            .label_field
            .clone()
            .map(|field| {
                field.update(cx, |field, cx| {
                    field
                        .text_for_range(0..usize::MAX, &mut None, window, cx)
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    fn submit_pairing_link(
        &mut self,
        label: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(endpoint) = self.active_endpoint.clone() else {
            self.clients_state.error = Some("Connect to a server first.".into());
            cx.notify();
            return;
        };
        if self.clients_state.creating || self.clients_state.draft_scopes.is_empty() {
            return;
        }
        let label = label.unwrap_or_else(|| self.pairing_label_text(window, cx));
        let mut body = json!({ "scopes": self.clients_state.draft_scopes });
        if !label.is_empty() {
            body["label"] = json!(label);
        }
        self.clients_state.creating = true;
        self.clients_state.error = None;
        cx.notify();
        let base = endpoint.http_url.trim_end_matches('/').to_string();
        cx.spawn(async move |this, cx| {
            let ep = endpoint.clone();
            let result =
                smol::unblock(move || auth_post(&ep, "/api/auth/pairing-token", body)).await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.clients_state;
                state.creating = false;
                match result {
                    Ok(v) => {
                        let credential = v["credential"].as_str().or_else(|| v["token"].as_str());
                        if let Some(credential) = credential {
                            // Same URL shape the web builds via
                            // resolveDesktopPairingUrl: <endpoint>/pair#token=…
                            state.created_url = Some(format!("{base}/pair#token={credential}"));
                            state.copied = false;
                            state.create_open = false;
                        } else {
                            state.error = Some(format!("Unexpected response: {v}"));
                        }
                    }
                    Err(e) => state.error = Some(format!("Create link failed: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_client_session_row(
        &self,
        index: usize,
        session: &Value,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let state = &self.clients_state;
        let id = session_id(session).to_string();
        let current = session["current"].as_bool() == Some(true);
        let live = current || session["connected"].as_bool() == Some(true);
        let revoking = state.revoking.as_deref() == Some(id.as_str());

        // Device bits · scope summary on line 2; created/last-seen on line 3.
        let mut detail = device_bits(session);
        detail.push(scope_summary(session));
        let mut times: Vec<String> = [session["createdAt"]
            .as_str()
            .or_else(|| session["issuedAt"].as_str())]
        .into_iter()
        .flatten()
        .filter_map(|iso| relative_time(iso).map(|r| format!("created {r}")))
        .collect();
        if let Some(iso) = session["lastSeenAt"]
            .as_str()
            .or_else(|| session["lastConnectedAt"].as_str())
        {
            if let Some(r) = relative_time(iso) {
                times.push(format!("last seen {r}"));
            }
        }

        let mut row = div()
            .flex()
            .items_center()
            .gap_2()
            .py_1p5()
            .child(
                div()
                    .w(px(8.))
                    .h(px(8.))
                    .flex_none()
                    .rounded_full()
                    .bg(if live {
                        rgb(SUCCESS)
                    } else {
                        theme::fg_muted()
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme::fg())
                                    .child(session_label(session)),
                            )
                            .when(current, |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .px_1()
                                        .py_0p5()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(theme::border())
                                        .bg(theme::hover())
                                        .text_xs()
                                        .text_color(theme::fg_muted())
                                        .child("This device"),
                                )
                            }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(detail.join(" · ")),
                    )
                    .when(!times.is_empty(), |d| {
                        d.child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child(times.join(" · ")),
                        )
                    }),
            );

        if !current {
            let mut button = action_button(
                if revoking { "Revoking…" } else { "Revoke" },
                !revoking,
                true,
            )
            .id(("revoke-session", index));
            if !revoking {
                let session_id = id.clone();
                button = button.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        this.revoke_client_session(session_id.clone(), cx);
                    }),
                );
            }
            row = row.child(button);
        }
        row
    }

    fn render_pairing_link_row(
        &self,
        index: usize,
        link: &Value,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let state = &self.clients_state;
        let id = link["id"].as_str().unwrap_or_default().to_string();
        // The web skips expired links entirely.
        if let Some(expires) = link["expiresAt"].as_str().and_then(parse_iso_epoch) {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
            if expires <= now {
                return None;
            }
        }
        let revoking = state.revoking.as_deref() == Some(id.as_str());

        let label = link["label"].as_str().unwrap_or("Pairing link").to_string();
        let mut detail = Vec::new();
        if let Some(r) = link["expiresAt"].as_str().and_then(relative_time) {
            detail.push(format!("expires {r}"));
        }
        detail.push(scope_summary(link));

        let mut button = action_button(
            if revoking { "Revoking…" } else { "Revoke" },
            !revoking,
            true,
        )
        .id(("revoke-link", index));
        if !revoking {
            let link_id = id.clone();
            button = button.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _window, cx| {
                    this.revoke_pairing_link(link_id.clone(), cx);
                }),
            );
        }

        Some(
            div()
                .flex()
                .items_center()
                .gap_2()
                .py_1p5()
                .child(
                    div()
                        .w(px(8.))
                        .h(px(8.))
                        .flex_none()
                        .rounded_full()
                        .bg(rgb(0xfbbf24)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::fg())
                                .child(label),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child(detail.join(" · ")),
                        ),
                )
                .child(button),
        )
    }

    /// The inline "Create link" panel: label field + scope toggles + actions.
    fn render_create_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        let state = &self.clients_state;
        let mut panel = div()
            .mb_2()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::canvas())
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .pb_1()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::fg())
                    .child("Create pairing link"),
            )
            .child(
                div()
                    .pb_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Generate a one-time link that another device can use to pair with this server as an authorized client."),
            )
            .child(
                div()
                    .pb_1()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Client label (optional)"),
            );

        if let Some(field) = state.label_field.clone() {
            panel = panel.child(
                div()
                    .mb_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .text_sm()
                    .text_color(theme::fg())
                    .child(field),
            );
        }

        let mut presets = div().flex().items_center().gap_2().pb_1().child(
            div()
                .flex_1()
                .text_xs()
                .text_color(theme::fg_muted())
                .child("Permissions — limit what the paired client can do."),
        );
        for (index, (name, scopes)) in [
            ("Read only", READ_ONLY_SCOPES),
            ("Standard", STANDARD_SCOPES),
        ]
        .into_iter()
        .enumerate()
        {
            let mut button =
                action_button(name, !state.creating, false).id(("scope-preset", index));
            if !state.creating {
                button = button.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        this.set_draft_scopes(scopes, cx);
                    }),
                );
            }
            presets = presets.child(button);
        }
        panel = panel.child(presets);

        for (index, (scope, title, description)) in SCOPE_OPTIONS.iter().enumerate() {
            let checked = state.draft_scopes.iter().any(|s| s == scope);
            panel = panel.child(
                div()
                    .id(("pairing-scope", index))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.toggle_draft_scope(scope, cx);
                        }),
                    )
                    .child(checkbox(checked))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_xs().text_color(theme::fg()).child(*title))
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme::fg_muted())
                                    .child(format!("{scope} — {description}")),
                            ),
                    ),
            );
        }

        if state.draft_scopes.is_empty() {
            panel = panel.child(
                div()
                    .pt_1()
                    .text_xs()
                    .text_color(rgb(DANGER))
                    .child("Select at least one permission."),
            );
        }

        let create_enabled = !state.creating && !state.draft_scopes.is_empty();
        let mut create = action_button(
            if state.creating {
                "Creating…"
            } else {
                "Create link"
            },
            create_enabled,
            false,
        )
        .id("create-link-submit");
        if create_enabled {
            create = create.on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| {
                    this.submit_pairing_link(None, window, cx);
                }),
            );
        }
        let mut cancel = action_button("Cancel", true, false).id("create-link-cancel");
        cancel = cancel.on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _ev, _window, cx| {
                this.clients_state.create_open = false;
                cx.notify();
            }),
        );

        panel.child(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap_2()
                .pt_2()
                .child(cancel)
                .child(create),
        )
    }

    /// The freshly minted pairing URL box: full URL (truncated visually) +
    /// Copy + Done.
    fn render_created_link(&self, url: &str, cx: &mut Context<Self>) -> gpui::Div {
        let state = &self.clients_state;
        let url = url.to_string();

        let copy_label = if state.copied { "Copied" } else { "Copy link" };
        let mut copy = action_button(copy_label, true, false).id("pairing-url-copy");
        let copy_url = url.clone();
        copy = copy.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _ev, _window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()));
                this.clients_state.copied = true;
                cx.notify();
            }),
        );
        let mut done = action_button("Done", true, false).id("pairing-url-done");
        done = done.on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _ev, _window, cx| {
                this.clients_state.created_url = None;
                this.clients_state.copied = false;
                cx.notify();
            }),
        );

        div()
            .mb_2()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::canvas())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::fg())
                    .child("Pairing link created"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Open this link on the device you want to pair. It works once."),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .px_2()
                            .py_1p5()
                            .rounded_md()
                            .bg(theme::hover())
                            .text_xs()
                            .text_color(theme::fg())
                            .child(url),
                    )
                    .child(copy)
                    .child(done),
            )
    }

    pub fn render_clients_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = &self.clients_state;
        let connected = self.active_endpoint.is_some();
        let non_current = state
            .sessions
            .iter()
            .filter(|s| s["current"].as_bool() != Some(true))
            .count();

        let mut section = div().flex().flex_col();

        // Header: description on the left, Revoke others + Create link right.
        let mut actions = div().flex().flex_none().items_center().gap_2();
        let others_enabled = connected && non_current > 0 && !state.revoking_others;
        let mut revoke_others = action_button(
            if state.revoking_others {
                "Revoking…"
            } else {
                "Revoke others"
            },
            others_enabled,
            true,
        )
        .id("revoke-others-btn");
        if others_enabled {
            revoke_others = revoke_others.on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, _window, cx| this.revoke_other_sessions(cx)),
            );
        }
        let create_enabled = connected && !state.creating;
        let mut create = action_button(
            if state.create_open {
                "Close"
            } else {
                "Create link"
            },
            create_enabled,
            false,
        )
        .id("create-link-btn");
        if create_enabled {
            create = create.on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| this.toggle_create_panel(window, cx)),
            );
        }
        actions = actions.child(revoke_others).child(create);

        section = section.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child("Devices and pairing links authorized on this server."),
                )
                .child(actions),
        );

        if let Some(error) = state.error.clone() {
            section = section.child(div().pt_2().text_xs().text_color(rgb(DANGER)).child(error));
        }

        if state.create_open {
            section = section.child(self.render_create_panel(cx));
        }
        if let Some(url) = state.created_url.clone() {
            section = section.child(self.render_created_link(&url, cx));
        }

        if !state.pairing_links.is_empty() {
            section = section.child(group_label("Pairing links"));
            for (index, link) in state.pairing_links.iter().enumerate() {
                if let Some(row) = self.render_pairing_link_row(index, link, cx) {
                    section = section.child(row);
                }
            }
        }

        section = section.child(group_label("Connected clients"));
        if state.sessions.is_empty() {
            let text = if !connected {
                "Connect to a server to manage authorized clients."
            } else if !state.loaded {
                "Loading…"
            } else {
                "No connected clients."
            };
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(text),
            );
        }
        for (index, session) in state.sessions.iter().enumerate() {
            section = section.child(self.render_client_session_row(index, session, cx));
        }

        if state.sessions.is_empty() && state.pairing_links.is_empty() && state.loaded {
            section = section.child(
                div()
                    .pt_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No pairing links or client sessions."),
            );
        }

        section
    }
}
