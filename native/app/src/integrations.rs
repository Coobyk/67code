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

#[derive(Default)]
pub struct State {
    /// toolInfo outcome per turn-item id.
    pub probes: RefCell<HashMap<String, Probe>>,
    /// Thread id the auto-probe last ran for; switching threads re-probes.
    pub probed_thread: RefCell<Option<String>>,
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

    pub(crate) fn render_integrations_section(&self, cx: &mut Context<Self>) -> Div {
        let mut apps: Vec<(String, AppRefLite)> = Vec::new();
        for item in &self.thread_items {
            if let Some(app) = item_app(&item.raw) {
                apps.push((item.id.clone(), app));
            }
        }
        self.maybe_probe_integrations(cx, &apps);
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
                                    .bg(theme::hover())
                                    .text_color(theme::fg())
                                    .hover(|s| s.bg(theme::selected()))
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
