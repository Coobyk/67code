//! Integrations settings section: MCP apps (the `mcpApps.*` wire surface).
//!
//! An MCP app is a `ui://` surface a provider tool call returns — the turn
//! item is `dynamic_tool` with a `t3McpApp` reference in its (possibly
//! enveloped) output. The web client renders the snapshot HTML in a sandboxed
//! chat frame (apps/web `McpAppFrame`) and the frame talks back over
//! `mcpApps.callTool` / `toolInfo` / `readResource` / `updateModelContext`.
//!
//! This client has no webview, so the section lists every app reference in
//! the loaded thread and probes `mcpApps.toolInfo` for it — the same request
//! the hosted frame would send. The reply (`callable`/`readOnly` or an
//! `McpAppRequestError` reason like `session-stopped`) is the honest status
//! of the bridge for that app.

use std::cell::RefCell;
use std::collections::HashMap;

use gpui::{Context, Div, MouseButton, SharedString, div, prelude::*, px};
use serde_json::{Value, json};

use crate::AppView;
use crate::theme;

// ---------------------------------------------------------------------------
// t3McpApp reference scan (same shape the server's rpc_mcp validates)
// ---------------------------------------------------------------------------

struct AppRefLite {
    server: String,
    tool: String,
    resource_uri: String,
}

/// Shallow port of `readResult` + `readMcpAppReference`: unwrap
/// `structuredContent`/`content` envelopes and JSON strings, then read the
/// `t3McpApp` object. Display-only — the server re-validates on every call.
fn app_ref(value: &Value, depth: usize) -> Option<AppRefLite> {
    if depth > 4 {
        return None;
    }
    match value {
        Value::String(s) => {
            let parsed: Value = serde_json::from_str(s).ok()?;
            app_ref(&parsed, depth + 1)
        }
        Value::Array(blocks) => blocks.iter().take(32).find_map(|b| {
            let inner = match b.get("text") {
                Some(t) if t.is_object() => t.get("text").cloned().unwrap_or_else(|| t.clone()),
                Some(t) => t.clone(),
                None => b.clone(),
            };
            app_ref(&inner, depth + 1)
        }),
        Value::Object(map) => {
            if let Some(app) = map.get("t3McpApp").and_then(read_ref) {
                return Some(app);
            }
            let content = map
                .get("structuredContent")
                .or_else(|| map.get("content"))?;
            app_ref(content, depth + 1)
        }
        _ => None,
    }
}

fn read_ref(v: &Value) -> Option<AppRefLite> {
    let server = v.get("server")?.as_str()?;
    let tool = v.get("tool")?.as_str()?;
    let uri = v.get("resourceUri")?.as_str()?;
    if server.trim().is_empty() || tool.trim().is_empty() || !uri.starts_with("ui://") {
        return None;
    }
    Some(AppRefLite {
        server: server.to_string(),
        tool: tool.to_string(),
        resource_uri: uri.to_string(),
    })
}

/// MCP app attached to a turn item, if it is a `dynamic_tool` whose output
/// carries `t3McpApp`. `result` covers the native codex runner's shape.
fn item_app(item: &Value) -> Option<AppRefLite> {
    if item.get("type").and_then(Value::as_str) != Some("dynamic_tool") {
        return None;
    }
    for key in ["output", "result"] {
        if let Some(v) = item.get(key).filter(|v| !v.is_null())
            && let Some(app) = app_ref(v, 0)
        {
            return Some(app);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Section state: per-item `mcpApps.toolInfo` probe outcomes
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub enum Probe {
    InFlight,
    /// `mcpApps.toolInfo` success: callable/readOnly/title from the bridge.
    Info {
        callable: bool,
        read_only: bool,
        title: Option<String>,
    },
    /// Failure Exit — carries the `McpAppRequestError.reason` or message.
    Failed(String),
}

/// `cloud.getRelayClientStatus` outcome (RelayClientStatusSchema).
#[derive(Clone)]
pub enum RelayProbe {
    InFlight,
    /// `{"status":"available", executablePath, source, version}`.
    Available {
        version: String,
        source: String,
        path: String,
    },
    /// `{"status":"missing", version}` — install is offered.
    Missing {
        version: String,
    },
    /// `{"status":"unsupported", platform, arch, version}`.
    Unsupported {
        platform: String,
        arch: String,
        version: String,
    },
    Failed(String),
}

/// One `device.list` device row (DeviceSummary).
pub struct DeviceRow {
    name: String,
    platform: String,
    version: String,
    booted: bool,
    physical: bool,
}

/// `device.list` outcome (DeviceServiceState).
pub enum DevicesProbe {
    InFlight,
    Ready {
        host_status: String,
        detail: Option<String>,
        devices: Vec<DeviceRow>,
    },
    Failed(String),
}

#[derive(Default)]
pub struct State {
    /// toolInfo outcome per turn-item id.
    pub probes: RefCell<HashMap<String, Probe>>,
    /// Thread id the auto-probe last ran for; switching threads re-probes.
    pub probed_thread: RefCell<Option<String>>,
    /// `cloud.getRelayClientStatus` — probed once per run (None = not yet).
    pub relay: RefCell<Option<RelayProbe>>,
    /// `cloud.installRelayClient` stream stage while an install runs.
    pub relay_install: RefCell<Option<String>>,
    /// `device.list` — probed once per run (None = not yet).
    pub devices: RefCell<Option<DevicesProbe>>,
}

fn exit_error_message(exit: &Value) -> String {
    let error = &exit["cause"]["error"];
    if let Some(reason) = error.get("reason").and_then(Value::as_str) {
        return match reason {
            "not-an-app" => "This tool call has no MCP app.".into(),
            "provider-unsupported" => "This thread's provider cannot run MCP app requests.".into(),
            "session-stopped" => "The app's thread is not running (session-stopped).".into(),
            "tool-not-callable" => "This app cannot call that tool.".into(),
            "unsupported-content" => "Only text and structured content are supported.".into(),
            "request-failed" => "The app's MCP server request failed.".into(),
            other => other.to_string(),
        };
    }
    error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("request failed")
        .to_string()
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

fn info_row(label: &'static str, value: impl Into<SharedString>) -> Div {
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

fn group_label(text: &'static str) -> Div {
    div()
        .pb_1()
        .pt_3()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(text)
}

impl AppView {
    /// Lazy-probe `mcpApps.toolInfo` once per thread selection, the same way
    /// `maybe_load_misc_settings` gates `server.getConfig`. Explicit re-probe
    /// is the "Probe" chip on each row.
    fn maybe_probe_integrations(&self, cx: &mut Context<Self>, apps: &[(String, AppRefLite)]) {
        let thread_id = self.state.selected.clone().unwrap_or_default().to_string();
        {
            let probed = self.integrations_state.probed_thread.borrow();
            if probed.as_deref() == Some(thread_id.as_str()) {
                return;
            }
        }
        *self.integrations_state.probed_thread.borrow_mut() = Some(thread_id.clone());
        for (item_id, app) in apps {
            {
                let mut probes = self.integrations_state.probes.borrow_mut();
                if probes.contains_key(item_id) {
                    continue;
                }
                probes.insert(item_id.clone(), Probe::InFlight);
            }
            self.probe_mcp_app(cx, thread_id.clone(), item_id.clone(), app.tool.clone());
        }
    }

    /// Fire one `mcpApps.toolInfo` query; the reply lands in `state.probes`.
    fn probe_mcp_app(
        &self,
        cx: &mut Context<Self>,
        thread_id: String,
        item_id: String,
        name: String,
    ) {
        let Some(rx) = self.send_query(
            "mcpApps.toolInfo",
            json!({ "threadId": thread_id, "itemId": item_id, "name": name }),
        ) else {
            self.integrations_state
                .probes
                .borrow_mut()
                .insert(item_id, Probe::Failed("Not connected to a server.".into()));
            return;
        };
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let probe = match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        let v = &exit["value"];
                        Probe::Info {
                            callable: v["callable"].as_bool().unwrap_or(false),
                            read_only: v["readOnly"].as_bool().unwrap_or(false),
                            title: v["title"].as_str().map(str::to_owned),
                        }
                    }
                    Ok(exit) => Probe::Failed(exit_error_message(&exit)),
                    Err(_) => Probe::Failed("Lost connection before the reply arrived.".into()),
                };
                this.integrations_state
                    .probes
                    .borrow_mut()
                    .insert(item_id, probe);
                cx.notify();
            });
        })
        .detach();
    }

    // ---------------------------------------------------------------------
    // 67 Connect relay + devices (the `cloud.*` / `device.*` wire surface)
    // ---------------------------------------------------------------------

    /// Map a `RelayClientStatus` wire value onto `RelayProbe`.
    fn relay_from_wire(v: &Value) -> RelayProbe {
        match v["status"].as_str() {
            Some("available") => RelayProbe::Available {
                version: v["version"].as_str().unwrap_or("?").to_string(),
                source: v["source"].as_str().unwrap_or("?").to_string(),
                path: v["executablePath"].as_str().unwrap_or("?").to_string(),
            },
            Some("unsupported") => RelayProbe::Unsupported {
                platform: v["platform"].as_str().unwrap_or("?").to_string(),
                arch: v["arch"].as_str().unwrap_or("?").to_string(),
                version: v["version"].as_str().unwrap_or("?").to_string(),
            },
            Some("missing") | _ => RelayProbe::Missing {
                version: v["version"].as_str().unwrap_or("?").to_string(),
            },
        }
    }

    /// Fire `cloud.getRelayClientStatus` once; a retry chips clears the probe
    /// back to None which re-fires it on the next render.
    fn probe_relay_status(&self, cx: &mut Context<Self>) {
        *self.integrations_state.relay.borrow_mut() = Some(RelayProbe::InFlight);
        let Some(rx) = self.send_query("cloud.getRelayClientStatus", json!({})) else {
            *self.integrations_state.relay.borrow_mut() =
                Some(RelayProbe::Failed("Not connected to a server.".into()));
            return;
        };
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let probe = match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        Self::relay_from_wire(&exit["value"])
                    }
                    Ok(exit) => RelayProbe::Failed(exit_error_message(&exit)),
                    Err(_) => {
                        RelayProbe::Failed("Lost connection before the reply arrived.".into())
                    }
                };
                *this.integrations_state.relay.borrow_mut() = Some(probe);
                cx.notify();
            });
        })
        .detach();
    }

    /// Fire `device.list` once — the real device inventory (local / SSH /
    /// EAS / Simlock hosts) or the server's honest unavailable reason.
    fn probe_devices(&self, cx: &mut Context<Self>) {
        *self.integrations_state.devices.borrow_mut() = Some(DevicesProbe::InFlight);
        let Some(rx) = self.send_query("device.list", json!({})) else {
            *self.integrations_state.devices.borrow_mut() =
                Some(DevicesProbe::Failed("Not connected to a server.".into()));
            return;
        };
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let probe = match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        let v = &exit["value"];
                        DevicesProbe::Ready {
                            host_status: v["hostStatus"].as_str().unwrap_or("unknown").to_string(),
                            detail: v["hostStatusDetail"].as_str().map(str::to_owned),
                            devices: v["devices"]
                                .as_array()
                                .map(|a| {
                                    a.iter()
                                        .map(|d| DeviceRow {
                                            name: d["name"]
                                                .as_str()
                                                .unwrap_or("device")
                                                .to_string(),
                                            platform: d["platform"]
                                                .as_str()
                                                .unwrap_or("?")
                                                .to_string(),
                                            version: d["version"]
                                                .as_str()
                                                .unwrap_or("")
                                                .to_string(),
                                            booted: d["booted"].as_bool() == Some(true),
                                            physical: d["physical"].as_bool() == Some(true),
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                        }
                    }
                    Ok(exit) => DevicesProbe::Failed(exit_error_message(&exit)),
                    Err(_) => {
                        DevicesProbe::Failed("Lost connection before the reply arrived.".into())
                    }
                };
                *this.integrations_state.devices.borrow_mut() = Some(probe);
                cx.notify();
            });
        })
        .detach();
    }

    /// Lazy-fire both connectivity probes on first render, like
    /// `maybe_probe_integrations` gates `mcpApps.toolInfo`.
    fn maybe_probe_connectivity(&self, cx: &mut Context<Self>) {
        if self.integrations_state.relay.borrow().is_none() {
            self.probe_relay_status(cx);
        }
        if self.integrations_state.devices.borrow().is_none() {
            self.probe_devices(cx);
        }
    }

    /// "Install relay client" — `cloud.installRelayClient` is a streaming
    /// RPC: each chunk is `{type:"progress",stage}` and the last is
    /// `{type:"complete",status}` (a fresh RelayClientStatus).
    fn install_relay_client(&mut self, cx: &mut Context<Self>) {
        let Some((_stream_id, rx)) = self.open_stream("cloud.installRelayClient", json!({})) else {
            *self.integrations_state.relay.borrow_mut() =
                Some(RelayProbe::Failed("Not connected to a server.".into()));
            return;
        };
        *self.integrations_state.relay_install.borrow_mut() = Some("checking".to_string());
        cx.spawn(async move |this, cx| {
            let mut completed: Option<Value> = None;
            while let Ok(chunk) = rx.recv().await {
                let alive = this
                    .update(cx, |this, cx| {
                        match chunk["type"].as_str() {
                            Some("progress") => {
                                *this.integrations_state.relay_install.borrow_mut() =
                                    Some(chunk["stage"].as_str().unwrap_or("working").to_string());
                            }
                            Some("complete") => {
                                completed = Some(chunk["status"].clone());
                            }
                            _ => {}
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                *this.integrations_state.relay_install.borrow_mut() = None;
                // The stream Exit isn't surfaced past StreamEnd — a close
                // with no `complete` event is the honest failure shape.
                let probe = match completed {
                    Some(status) => Self::relay_from_wire(&status),
                    None => {
                        RelayProbe::Failed("The install stream ended without a result.".to_string())
                    }
                };
                *this.integrations_state.relay.borrow_mut() = Some(probe);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn render_integrations_section(&self, cx: &mut Context<Self>) -> Div {
        let mut apps: Vec<(String, AppRefLite)> = Vec::new();
        for item in &self.thread_items {
            if let Some(app) = item_app(&item.raw) {
                apps.push((item.id.clone(), app));
            }
        }
        self.maybe_probe_integrations(cx, &apps);
        self.maybe_probe_connectivity(cx);
        let connected = self.commands.is_some();
        let probes = self.integrations_state.probes.borrow();

        let mut section = div().flex().flex_col().child(
            div().pb_2().text_xs().text_color(theme::fg_muted()).child(
                "MCP apps are interactive ui:// surfaces a provider tool call returns. \
                     The web client embeds them in a sandboxed frame in chat; this client \
                     lists them here and probes the same mcpApps.* bridge the frame uses.",
            ),
        );

        section = section.child(group_label("Apps in the loaded thread"));
        if apps.is_empty() {
            section = section.child(
                div()
                    .py_1p5()
                    .text_sm()
                    .text_color(theme::fg_muted())
                    .child(
                        "None. An app appears when a provider tool call returns a ui:// resource."
                            .to_string(),
                    ),
            );
        }
        for (index, (item_id, app)) in apps.iter().enumerate() {
            let probe = probes.get(item_id);
            let status = match probe {
                None | Some(Probe::InFlight) => "probing…".to_string(),
                Some(Probe::Info {
                    callable,
                    read_only,
                    title,
                }) => {
                    let mut s = String::from(if *callable {
                        "callable"
                    } else {
                        "not callable"
                    });
                    if *read_only {
                        s.push_str(" · read-only");
                    }
                    if let Some(t) = title {
                        s.push_str(&format!(" · {t}"));
                    }
                    s
                }
                Some(Probe::Failed(msg)) => msg.clone(),
            };
            let status_is_error = matches!(probe, Some(Probe::Failed(_)));

            let tid = self.state.selected.clone().unwrap_or_default().to_string();
            let iid = item_id.clone();
            let tool = app.tool.clone();
            section = section.child(
                div()
                    .py_1p5()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(info_row("Tool", format!("{}.{}", app.server, app.tool)))
                    .child(info_row("Resource", app.resource_uri.clone()))
                    .child(info_row("Turn item", iid.clone()))
                    .child(
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
                                    .child("Bridge"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(if status_is_error {
                                        theme::fg_muted()
                                    } else {
                                        theme::fg()
                                    })
                                    .child(status),
                            )
                            .child(
                                div()
                                    .id(("mcp-reprobe", index))
                                    .px_2()
                                    .py_0p5()
                                    .rounded_md()
                                    .text_xs()
                                    .cursor_pointer()
                                    .bg(theme::accent())
                                    .text_color(theme::fg())
                                    .hover(|s| s.bg(theme::hover()))
                                    .child("Probe")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _ev, _window, cx| {
                                            this.integrations_state
                                                .probes
                                                .borrow_mut()
                                                .insert(iid.clone(), Probe::InFlight);
                                            this.probe_mcp_app(
                                                cx,
                                                tid.clone(),
                                                iid.clone(),
                                                tool.clone(),
                                            );
                                            cx.notify();
                                        }),
                                    ),
                            ),
                    ),
            );
        }

        if !connected {
            section = section.child(
                div()
                    .pt_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Offline — connect to a server to probe the app bridge."),
            );
        }

        // ── 67 Connect relay ─────────────────────────────────────────
        section = section.child(group_label("67 Connect relay"));
        section = section.child(div().pb_2().text_xs().text_color(theme::fg_muted()).child(
            "67 Connect tunnels reach this server through the managed relay client \
                 binary. The wire exposes only its status and installer — sign-in/pairing \
                 happens inside the relay itself (the pair URL it prints), and pointing \
                 this app at another server is the Environments settings section.",
        ));
        if let Some(stage) = self.integrations_state.relay_install.borrow().clone() {
            section = section.child(info_row("Install", format!("in progress — {stage}")));
        }
        match self.integrations_state.relay.borrow().clone() {
            None | Some(RelayProbe::InFlight) => {
                section = section.child(info_row("Status", "checking…"));
            }
            Some(RelayProbe::Available {
                version,
                source,
                path,
            }) => {
                section = section
                    .child(info_row("Status", "installed"))
                    .child(info_row("Version", version))
                    .child(info_row("Source", source))
                    .child(info_row("Binary", path));
            }
            Some(RelayProbe::Missing { version }) => {
                let installing = self.integrations_state.relay_install.borrow().is_some();
                let mut install_chip = div()
                    .id("relay-install")
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .bg(theme::accent())
                    .text_color(theme::fg())
                    .hover(|s| s.bg(theme::hover()))
                    .child("Install relay client")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            this.install_relay_client(cx);
                        }),
                    );
                if installing {
                    install_chip = install_chip.opacity(0.45);
                }
                section = section
                    .child(info_row("Status", "not installed"))
                    .child(info_row("Available", format!("relay client {version}")))
                    .child(div().py_1p5().flex().child(install_chip));
            }
            Some(RelayProbe::Unsupported {
                platform,
                arch,
                version,
            }) => {
                section = section
                    .child(info_row(
                        "Status",
                        format!("no managed build for {platform}/{arch}"),
                    ))
                    .child(info_row("Version", version));
            }
            Some(RelayProbe::Failed(message)) => {
                section = section
                    .child(info_row("Status", "unavailable"))
                    .child(info_row("Error", message))
                    .child(
                        div().py_1p5().flex().child(
                            div()
                                .id("relay-retry")
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .text_xs()
                                .cursor_pointer()
                                .bg(theme::accent())
                                .text_color(theme::fg())
                                .hover(|s| s.bg(theme::hover()))
                                .child("Retry")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _ev, _window, cx| {
                                        this.probe_relay_status(cx);
                                        cx.notify();
                                    }),
                                ),
                        ),
                    );
            }
        }

        // ── Devices ──────────────────────────────────────────────────
        section = section.child(group_label("Devices"));
        match &*self.integrations_state.devices.borrow() {
            None | Some(DevicesProbe::InFlight) => {
                section = section.child(info_row("Status", "loading…"));
            }
            Some(DevicesProbe::Failed(message)) => {
                section = section
                    .child(info_row("Status", "unavailable"))
                    .child(info_row("Reason", message.clone()))
                    .child(
                        div().py_1p5().flex().child(
                            div()
                                .id("devices-retry")
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .text_xs()
                                .cursor_pointer()
                                .bg(theme::accent())
                                .text_color(theme::fg())
                                .hover(|s| s.bg(theme::hover()))
                                .child("Retry")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _ev, _window, cx| {
                                        this.probe_devices(cx);
                                        cx.notify();
                                    }),
                                ),
                        ),
                    );
            }
            Some(DevicesProbe::Ready {
                host_status,
                detail,
                devices,
            }) => {
                let host = match detail {
                    Some(detail) if !detail.is_empty() => format!("{host_status} — {detail}"),
                    _ => host_status.clone(),
                };
                section = section.child(info_row("Host", host));
                if devices.is_empty() {
                    section = section.child(
                        div()
                            .py_1p5()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(
                                "No devices. Simulator/emulator inventory needs a mobile dev \
                             host (or an EAS / Simlock host) configured on the server.",
                            ),
                    );
                }
                for (index, device) in devices.iter().enumerate() {
                    let mut tags = Vec::new();
                    if device.booted {
                        tags.push("booted");
                    }
                    if device.physical {
                        tags.push("physical");
                    }
                    let detail = format!(
                        "{} {}{}",
                        device.platform,
                        device.version,
                        if tags.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", tags.join(" · "))
                        },
                    );
                    section = section.child(
                        div()
                            .id(("device-row", index))
                            .flex()
                            .items_center()
                            .gap_3()
                            .py_1p5()
                            .text_sm()
                            .child(
                                div()
                                    .w(px(140.))
                                    .flex_none()
                                    .truncate()
                                    .text_color(theme::fg())
                                    .child(device.name.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme::fg_muted())
                                    .child(detail),
                            ),
                    );
                }
            }
        }

        section
            .child(group_label("Wire surface"))
            .child(info_row("RPC", "mcpApps.toolInfo"))
            .child(div().pt_1().text_xs().text_color(theme::fg_muted()).child(
                "callTool / readResource / updateModelContext ride the same bridge from \
                         inside the hosted frame; the probe above answers with the server's real \
                         status for each app.",
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_app_in_structured_content() {
        let item = serde_json::json!({
            "type": "dynamic_tool",
            "toolName": "weather.show",
            "output": {
                "content": [{"type": "text", "text": "ok"}],
                "structuredContent": {
                    "t3McpApp": {
                        "attachmentId": "a",
                        "server": "weather",
                        "tool": "show",
                        "resourceUri": "ui://weather/card.html",
                    }
                }
            }
        });
        let app = item_app(&item).expect("app ref");
        assert_eq!(app.server, "weather");
        assert_eq!(app.resource_uri, "ui://weather/card.html");
    }

    #[test]
    fn ignores_plain_tools_and_bad_uris() {
        let plain = serde_json::json!({"type": "dynamic_tool", "output": {"x": 1}});
        assert!(item_app(&plain).is_none());
        let bad_uri = serde_json::json!({
            "type": "dynamic_tool",
            "output": {"t3McpApp": {"attachmentId": "a", "server": "s", "tool": "t",
                                   "resourceUri": "https://x/y"}}
        });
        assert!(item_app(&bad_uri).is_none());
        let msg = serde_json::json!({"type": "assistant_message"});
        assert!(item_app(&msg).is_none());
    }

    #[test]
    fn finds_app_in_json_string_output() {
        let inner = serde_json::to_string(&serde_json::json!({
            "t3McpApp": {"attachmentId": "a", "server": "s", "tool": "t",
                          "resourceUri": "ui://s/x.html"}
        }))
        .unwrap();
        let item = serde_json::json!({
            "type": "dynamic_tool",
            "output": [{"text": inner}]
        });
        assert!(item_app(&item).is_some());
    }
}
