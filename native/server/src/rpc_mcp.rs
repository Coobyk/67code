//! `mcpApps.*` — the MCP app bridge from
//! `apps/server/src/mcpApps/McpAppRequests.ts` and
//! `packages/contracts/src/mcpApps.ts`.
//!
//! An "MCP app" is a `dynamic_tool` turn item whose parsed tool output carries
//! a `t3McpApp` reference (`{attachmentId, server, tool, resourceUri: ui://…}`)
//! — the web client renders the snapshot HTML in a sandboxed frame and the
//! frame talks back over these RPCs.
//!
//! Fidelity notes vs the TypeScript service:
//! - `resolveApp` is implemented in full: turn item → dynamic_tool →
//!   `t3McpApp` reference with the same `toolName` impersonation guard
//!   (`server.tool` or Claude's `mcp__<server>__<tool>` naming), plus the
//!   envelope unwrapping `readResult` performs.
//! - `toolInfo`/`callTool`/`readResource` run against the server's own MCP
//!   client (`crate::mcp_manager`): the resolved `t3McpApp.server` name is
//!   looked up in the discovered stdio server configs (claude.json,
//!   `.mcp.json`, codex `config.toml`, t3 `settings.json`) and served by a
//!   persistent child process. Names with no spawnable config — including
//!   provider-builtin HTTP servers like T3's `t3-code` — keep the
//!   `session-stopped` answer, and a present `providerSessionId` keeps
//!   `provider-unsupported` (no Rust adapter MCP bridge exists).
//! - `updateModelContext` is implemented end-to-end: app resolution, the
//!   fork-lineage `descendsFrom` guard, content validation, the 16 KiB cap,
//!   and the same `mcp_app_model_context` upsert the TS migration creates.
//!   Consumption (prefixing the next turn prompt) is not yet wired into the
//!   Rust provider runners — the row is written so the wiring only has to
//!   read it.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::handlers::RpcResult;
use crate::mcp_manager::McpServerConfig;
use crate::AppState;

/// `MCP_APP_MODEL_CONTEXT_MAX_BYTES` (McpAppModelContext.ts).
const MODEL_CONTEXT_MAX_BYTES: usize = 16 * 1024;
const MAX_NAME_LENGTH: usize = 256;
const MAX_RESOURCE_URI_LENGTH: usize = 4096;
const RESOURCE_SCHEME: &str = "ui://";

/// `readResult` envelope caps (packages/shared/src/toolOutput.ts). Kept so a
/// hostile or pathological tool output can't spin the unwrapper.
const MAX_PARSED_BYTES: usize = 16_384;
const MAX_ENVELOPE_DEPTH: usize = 4;
const MAX_ENVELOPE_NODES: i64 = 128;
const MAX_CONTENT_BLOCKS: usize = 32;

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Wire errors
// ---------------------------------------------------------------------------

fn fail(thread_id: &str, reason: &str) -> RpcResult {
    RpcResult::err(json!({
        "_tag": "McpAppRequestError",
        "threadId": thread_id,
        "reason": reason,
    }))
}

fn unsupported(tag: &str) -> RpcResult {
    RpcResult::err(json!({
        "_tag": "UnsupportedOperationError",
        "message": format!("{tag} is not a known mcpApps method"),
    }))
}

// ---------------------------------------------------------------------------
// McpAppReference extraction (packages/shared/src/mcpApp.ts + toolOutput.ts)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct AppRef {
    server: String,
    tool: String,
    /// Validated `ui://` URI — routing needs only server/tool today; the
    /// resource URI is what a future bridge passes to `readResource`.
    #[allow(dead_code)]
    resource_uri: String,
}

fn bounded_name(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    if s.trim().is_empty() || s.chars().count() > MAX_NAME_LENGTH {
        return None;
    }
    Some(s.to_string())
}

/// `readMcpAppReference`: we only need the identity fields; csp/permissions
/// matter to the snapshot pipeline, not to request routing.
fn read_app_reference(value: &Value) -> Option<AppRef> {
    let obj = value.as_object()?;
    bounded_name(obj.get("attachmentId")?)?;
    let server = bounded_name(obj.get("server")?)?;
    let tool = bounded_name(obj.get("tool")?)?;
    let resource_uri = obj.get("resourceUri")?.as_str()?;
    if resource_uri.chars().count() > MAX_RESOURCE_URI_LENGTH
        || !resource_uri.starts_with(RESOURCE_SCHEME)
    {
        return None;
    }
    Some(AppRef {
        server,
        tool,
        resource_uri: resource_uri.to_string(),
    })
}

struct Budget {
    remaining_bytes: usize,
    remaining_nodes: i64,
    exceeded: bool,
}

impl Budget {
    fn new() -> Self {
        Self {
            remaining_bytes: MAX_PARSED_BYTES,
            remaining_nodes: MAX_ENVELOPE_NODES,
            exceeded: false,
        }
    }
}

/// `readResult` from toolOutput.ts: unwraps `structuredContent`/`content`
/// envelopes, JSON strings and content-block arrays until it reaches the
/// result data object. Returns the first `data` found (TS `data ??=`).
fn read_result_data(value: &Value, budget: &mut Budget, depth: usize) -> Option<Value> {
    if depth > MAX_ENVELOPE_DEPTH || budget.remaining_nodes <= 0 {
        budget.exceeded = true;
        return None;
    }
    budget.remaining_nodes -= 1;
    match value {
        Value::String(s) => {
            let bytes = s.len();
            if s.chars().count() > budget.remaining_bytes || bytes > budget.remaining_bytes {
                budget.exceeded = true;
                return None;
            }
            budget.remaining_bytes -= bytes;
            match serde_json::from_str::<Value>(s) {
                Ok(v) => read_result_data(&v, budget, depth + 1),
                Err(_) => None,
            }
        }
        Value::Array(blocks) => {
            if blocks.len() > MAX_CONTENT_BLOCKS {
                budget.exceeded = true;
                return None;
            }
            let mut data = None;
            for block in blocks {
                // TS: Predicate.isObject(text) ? (text.text ?? text) : text
                let text = block.get("text").cloned().unwrap_or(Value::Null);
                let inner = if text.is_object() {
                    text.get("text").cloned().unwrap_or(text)
                } else {
                    text
                };
                if data.is_none() {
                    data = read_result_data(&inner, budget, depth + 1);
                } else {
                    // Still consume budget faithfully on later blocks.
                    let _ = read_result_data(&inner, budget, depth + 1);
                }
                if budget.exceeded {
                    break;
                }
            }
            data
        }
        Value::Object(map) => {
            let content = map.get("structuredContent").or_else(|| map.get("content"));
            match content {
                Some(c) => read_result_data(c, budget, depth + 1),
                None => Some(Value::Object(map.clone())),
            }
        }
        _ => None,
    }
}

fn claude_tool_name_part(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `claudeMcpToolName`: `mcp__<server>__<tool>`; "claude.ai " connector names
/// get their underscores collapsed and trimmed.
fn claude_mcp_tool_name(server: &str, tool: &str) -> String {
    let part = claude_tool_name_part(server);
    let part = if server.starts_with("claude.ai ") {
        let mut collapsed = String::with_capacity(part.len());
        let mut last_underscore = false;
        for c in part.chars() {
            if c == '_' {
                if last_underscore {
                    continue;
                }
                last_underscore = true;
            } else {
                last_underscore = false;
            }
            collapsed.push(c);
        }
        collapsed.trim_matches('_').to_string()
    } else {
        part
    };
    format!("mcp__{part}__{}", claude_tool_name_part(tool))
}

/// `mcpAppFromToolItem`: the app reference counts only when the item's tool
/// name matches the declared source, so tools can't impersonate another app.
/// The contract shape reads `toolName`; the native codex runner stores the
/// equivalent `server`/`tool` pair, which is accepted too.
fn app_from_tool_item(item: &Value) -> Option<AppRef> {
    if item.get("type").and_then(Value::as_str) != Some("dynamic_tool") {
        return None;
    }
    let output = item
        .get("output")
        .filter(|v| !v.is_null())
        .or_else(|| item.get("result"))?;
    let data = read_result_data(output, &mut Budget::new(), 0)?;
    let app = read_app_reference(data.get("t3McpApp")?)?;
    let declared = format!("{}.{}", app.server, app.tool);
    let tool_name_matches = match item.get("toolName").and_then(Value::as_str) {
        Some(name) => name == declared || name == claude_mcp_tool_name(&app.server, &app.tool),
        None => {
            // Native codex shape: {server, tool} fields instead of toolName.
            item.get("server").and_then(Value::as_str) == Some(app.server.as_str())
                && item.get("tool").and_then(Value::as_str) == Some(app.tool.as_str())
        }
    };
    tool_name_matches.then_some(app)
}

// ---------------------------------------------------------------------------
// Resolution (McpAppRequests.resolveApp / resolve)
// ---------------------------------------------------------------------------

/// Turn item → `t3McpApp` + provider thread. `not-an-app` covers every way an
/// item fails to be an app source, matching the TS check order.
async fn resolve_app(
    state: &Arc<AppState>,
    thread_id: &str,
    item_id: &str,
) -> Result<(AppRef, String), &'static str> {
    let item = state.store.get_turn_item(thread_id, item_id).await;
    let app = item.as_ref().and_then(app_from_tool_item);
    let provider_thread_id = item
        .as_ref()
        .and_then(|i| i.get("providerThreadId"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    match (item.is_some(), app, provider_thread_id) {
        (true, Some(app), Some(ptid)) => Ok((app, ptid)),
        _ => Err("not-an-app"),
    }
}

/// `providerThreads` from the projection carry `providerSessionId`; Rust
/// providers never set one. When a session id ever appears the request must
/// go through that provider's own MCP session — the Rust runtime has no
/// adapter MCP bridge, hence `provider-unsupported` at the call site.
async fn provider_session_id(
    state: &Arc<AppState>,
    thread_id: &str,
    provider_thread_id: &str,
) -> Option<String> {
    state
        .store
        .thread_projection(thread_id)
        .await
        .and_then(|p| {
            p.get("providerThreads")?
                .as_array()?
                .iter()
                .find(|t| t.get("id").and_then(Value::as_str) == Some(provider_thread_id))
                .cloned()
        })
        .and_then(|t| {
            t.get("providerSessionId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
}

/// `descendsFrom`: context may only land on the app's thread or a fork below
/// it. Walks `lineage.parentThreadId` while `relationshipToParent == "fork"`.
async fn descends_from(state: &Arc<AppState>, thread_id: &str, ancestor_id: &str) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut current = Some(thread_id.to_string());
    while let Some(id) = current {
        if !seen.insert(id.clone()) {
            break;
        }
        if id == ancestor_id {
            return true;
        }
        let Some(thread) = state.store.get_thread(&id).await else {
            break;
        };
        let lineage = thread.get("lineage").cloned().unwrap_or(Value::Null);
        current = if lineage.get("relationshipToParent").and_then(Value::as_str) == Some("fork") {
            lineage
                .get("parentThreadId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        } else {
            None
        };
    }
    false
}

// ---------------------------------------------------------------------------
// Model-context persistence (mcp_app_model_context, migration 059)
// ---------------------------------------------------------------------------

/// The store actor owns the DB, but WAL lets a second short-lived connection
/// upsert this side table without widening the StoreCmd API.
fn model_context_conn(state: &AppState) -> rusqlite::Result<Connection> {
    let conn = Connection::open(state.home_dir.join("userdata").join("statev2.sqlite"))?;
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mcp_app_model_context (
           thread_id TEXT NOT NULL,
           item_id TEXT NOT NULL,
           server TEXT NOT NULL,
           tool TEXT NOT NULL,
           text TEXT NOT NULL,
           updated_at TEXT NOT NULL,
           PRIMARY KEY (thread_id, item_id)
         )",
    )?;
    Ok(conn)
}

fn set_model_context(
    state: &AppState,
    thread_id: &str,
    item_id: &str,
    server: &str,
    tool: &str,
    text: &str,
) -> rusqlite::Result<()> {
    let conn = model_context_conn(state)?;
    if text.trim().is_empty() {
        conn.execute(
            "DELETE FROM mcp_app_model_context WHERE thread_id = ?1 AND item_id = ?2",
            rusqlite::params![thread_id, item_id],
        )?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO mcp_app_model_context (thread_id, item_id, server, tool, text, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(thread_id, item_id) DO UPDATE SET text = excluded.text, updated_at = excluded.updated_at",
        rusqlite::params![thread_id, item_id, server, tool, text, crate::domain::now_iso()],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

fn required_str<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// `mcpApps.updateModelContext` — the one method this server can fully
/// satisfy: it needs no live session, only the item's app identity, lineage,
/// and the context row.
async fn update_model_context(state: Arc<AppState>, payload: Value) -> RpcResult {
    let thread_id = required_str(&payload, "threadId").unwrap_or("");
    let Some(item_id) = required_str(&payload, "itemId") else {
        return fail(thread_id, "request-failed");
    };
    let Some(conversation_thread_id) = required_str(&payload, "conversationThreadId") else {
        return fail(thread_id, "request-failed");
    };
    let app = match resolve_app(&state, thread_id, item_id).await {
        Ok((app, _)) => app,
        Err(reason) => return fail(thread_id, reason),
    };
    if !descends_from(&state, conversation_thread_id, thread_id).await {
        return fail(thread_id, "not-an-app");
    }
    let mut texts: Vec<String> = Vec::new();
    if let Some(content) = payload.get("content") {
        let Some(blocks) = content.as_array() else {
            return fail(thread_id, "request-failed");
        };
        for block in blocks {
            let ok = block.is_object()
                && block.get("type").and_then(Value::as_str) == Some("text")
                && block.get("text").is_some_and(Value::is_string);
            if !ok {
                return fail(thread_id, "unsupported-content");
            }
            texts.push(block["text"].as_str().unwrap_or_default().to_string());
        }
    }
    if let Some(structured) = payload.get("structuredContent") {
        if !structured.is_object() {
            return fail(thread_id, "request-failed");
        }
        texts.push(structured.to_string());
    }
    let text = texts.join("\n");
    if text.len() > MODEL_CONTEXT_MAX_BYTES {
        return fail(thread_id, "request-failed");
    }
    let st = state.clone();
    let iid = item_id.to_string();
    let ctid = conversation_thread_id.to_string();
    let srv = app.server.clone();
    let tool = app.tool.clone();
    let write = tokio::task::spawn_blocking(move || {
        set_model_context(&st, &ctid, &iid, &srv, &tool, &text)
    })
    .await;
    match write {
        Ok(Ok(())) => RpcResult::ok(Value::Null),
        _ => fail(thread_id, "request-failed"),
    }
}

// ---------------------------------------------------------------------------
// Live MCP requests (server-owned sessions via mcp_manager)
// ---------------------------------------------------------------------------

/// `mcpAppToolCallableByApp`: `_meta.ui.visibility` array must list "app";
/// absent/non-array visibility leaves the tool callable.
fn tool_callable_by_app(tool: &Value) -> bool {
    match tool
        .get("_meta")
        .and_then(|m| m.get("ui"))
        .and_then(|u| u.get("visibility"))
    {
        Some(Value::Array(visibility)) => visibility.iter().any(|v| v.as_str() == Some("app")),
        _ => true,
    }
}

fn tool_read_only(tool: &Value) -> bool {
    tool.get("annotations")
        .and_then(|a| a.get("readOnlyHint"))
        .and_then(Value::as_bool)
        == Some(true)
}

fn tool_title(tool: &Value) -> Option<&str> {
    tool.get("annotations")
        .and_then(|a| a.get("title"))
        .and_then(Value::as_str)
}

fn fail_cause(thread_id: &str, reason: &str, cause: String) -> RpcResult {
    RpcResult::err(json!({
        "_tag": "McpAppRequestError",
        "threadId": thread_id,
        "reason": reason,
        "cause": cause,
    }))
}

fn mcp_fail(thread_id: &str, err: crate::mcp_manager::McpError) -> RpcResult {
    use crate::mcp_manager::McpError::*;
    match err {
        // No spawnable stdio config: either the name belongs to a provider
        // builtin (T3's `t3-code` HTTP server needs the live session) or the
        // server simply isn't configured anywhere we can see — in TS both
        // cases surface as session-stopped because only the provider session
        // could reach the server.
        NotConfigured => fail(thread_id, "session-stopped"),
        Timeout => fail_cause(
            thread_id,
            "request-failed",
            "MCP server request timed out".to_string(),
        ),
        Transport(detail) => fail_cause(thread_id, "request-failed", detail),
        Remote(error) => fail_cause(thread_id, "request-failed", error.to_string()),
    }
}

/// `mcpApps.toolInfo` — `tools/list` on the server-owned session, shaped as
/// `McpAppToolInfo` (`{callable, readOnly, title?, tool?}`).
async fn tool_info(
    state: &Arc<AppState>,
    thread_id: &str,
    config: &McpServerConfig,
    app: &AppRef,
    name: &str,
) -> RpcResult {
    match crate::mcp_manager::McpManager::global()
        .find_tool(config, &app.server, &state.project_dir, name)
        .await
    {
        Err(e) => mcp_fail(thread_id, e),
        Ok(found) => {
            let mut info = json!({
                "callable": found.as_ref().is_some_and(tool_callable_by_app),
                "readOnly": found.as_ref().is_some_and(tool_read_only),
            });
            if let Some(tool) = &found {
                if let Some(title) = tool_title(tool) {
                    info["title"] = json!(title);
                }
                info["tool"] = tool.clone();
            }
            RpcResult::ok(info)
        }
    }
}

/// `mcpApps.callTool` — enforce `_meta.ui.visibility` server-side like
/// `McpAppRequests.callTool` does, then `tools/call` on the session.
async fn call_tool(
    state: &Arc<AppState>,
    thread_id: &str,
    config: &McpServerConfig,
    app: &AppRef,
    name: &str,
    arguments: Value,
) -> RpcResult {
    let manager = crate::mcp_manager::McpManager::global();
    match manager
        .find_tool(config, &app.server, &state.project_dir, name)
        .await
    {
        Err(e) => mcp_fail(thread_id, e),
        Ok(None) => fail(thread_id, "tool-not-callable"),
        Ok(Some(tool)) if !tool_callable_by_app(&tool) => fail(thread_id, "tool-not-callable"),
        Ok(Some(_)) => match manager
            .call_tool(config, &app.server, &state.project_dir, name, arguments)
            .await
        {
            Err(e) => mcp_fail(thread_id, e),
            Ok(result) => RpcResult::ok(normalize_tool_result(result)),
        },
    }
}

/// `McpAppCallToolResult` requires a `content` array; MCP guarantees it but
/// tolerate a server omitting it on an edge-case result.
fn normalize_tool_result(result: Value) -> Value {
    if result.get("content").is_none() {
        let mut obj = result.as_object().cloned().unwrap_or_default();
        obj.insert("content".into(), json!([]));
        Value::Object(obj)
    } else {
        result
    }
}

/// `mcpApps.readResource` — `resources/read` → `{contents}`.
async fn read_resource(
    state: &Arc<AppState>,
    thread_id: &str,
    config: &McpServerConfig,
    app: &AppRef,
    uri: &str,
) -> RpcResult {
    match crate::mcp_manager::McpManager::global()
        .read_resource(config, &app.server, &state.project_dir, uri)
        .await
    {
        Err(e) => mcp_fail(thread_id, e),
        Ok(result) => {
            if result.get("contents").is_none() {
                let mut obj = result.as_object().cloned().unwrap_or_default();
                obj.insert("contents".into(), json!([]));
                RpcResult::ok(Value::Object(obj))
            } else {
                RpcResult::ok(result)
            }
        }
    }
}

/// `toolInfo` / `callTool` / `readResource`. Resolution runs in full; when a
/// live provider session id exists the request still reports
/// `provider-unsupported` (no Rust adapter MCP bridge). Otherwise the call
/// runs against the server's own persistent MCP session for the resolved
/// `t3McpApp.server` name — spawned from discovered stdio configs — and only
/// names with no spawnable config report `session-stopped`, matching the TS
/// outcome for apps whose provider session is gone.
async fn live_request(state: Arc<AppState>, tag: &str, payload: Value) -> RpcResult {
    let thread_id = required_str(&payload, "threadId").unwrap_or("");
    let Some(item_id) = required_str(&payload, "itemId") else {
        return fail(thread_id, "request-failed");
    };
    let (app, provider_thread_id) = match resolve_app(&state, thread_id, item_id).await {
        Ok(resolved) => resolved,
        Err(reason) => return fail(thread_id, reason),
    };
    if provider_session_id(&state, thread_id, &provider_thread_id)
        .await
        .is_some()
    {
        return fail(thread_id, "provider-unsupported");
    }
    let configs = crate::mcp_manager::discover_server_configs(&state.home_dir, &state.project_dir);
    let Some(config) = configs.get(&app.server) else {
        return mcp_fail(thread_id, crate::mcp_manager::McpError::NotConfigured);
    };
    match tag {
        "mcpApps.toolInfo" => {
            let Some(name) = required_str(&payload, "name") else {
                return fail(thread_id, "request-failed");
            };
            tool_info(&state, thread_id, config, &app, name).await
        }
        "mcpApps.callTool" => {
            let Some(name) = required_str(&payload, "name") else {
                return fail(thread_id, "request-failed");
            };
            let arguments = payload
                .get("arguments")
                .filter(|v| v.is_object())
                .cloned()
                .unwrap_or_else(|| json!({}));
            call_tool(&state, thread_id, config, &app, name, arguments).await
        }
        "mcpApps.readResource" => {
            let Some(uri) = required_str(&payload, "uri") else {
                return fail(thread_id, "request-failed");
            };
            read_resource(&state, thread_id, config, &app, uri).await
        }
        other => unsupported(other),
    }
}

pub async fn handle(state: Arc<AppState>, tag: &str, payload: Value) -> RpcResult {
    match tag {
        "mcpApps.callTool" | "mcpApps.toolInfo" | "mcpApps.readResource" => {
            live_request(state, tag, payload).await
        }
        "mcpApps.updateModelContext" => update_model_context(state, payload).await,
        other => unsupported(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_tool_naming() {
        assert_eq!(
            claude_mcp_tool_name("weather", "get_forecast"),
            "mcp__weather__get_forecast"
        );
        assert_eq!(
            claude_mcp_tool_name("claude.ai Weather  Plus", "a b"),
            "mcp__claude_ai_Weather_Plus__a_b"
        );
    }

    #[test]
    fn unwraps_structured_content_envelope() {
        let item = json!({
            "type": "dynamic_tool",
            "toolName": "weather.show",
            "output": {
                "content": [{ "type": "text", "text": "ok" }],
                "structuredContent": {
                    "t3McpApp": {
                        "attachmentId": "att-1",
                        "server": "weather",
                        "tool": "show",
                        "resourceUri": "ui://weather/card.html",
                    }
                }
            }
        });
        let app = app_from_tool_item(&item).expect("app ref");
        assert_eq!(app.server, "weather");
        assert_eq!(app.resource_uri, "ui://weather/card.html");
    }

    #[test]
    fn unwraps_json_string_envelope() {
        let inner = serde_json::to_string(&json!({
            "t3McpApp": {
                "attachmentId": "a",
                "server": "s",
                "tool": "t",
                "resourceUri": "ui://s/x.html",
            }
        }))
        .unwrap();
        let item = json!({
            "type": "dynamic_tool",
            "toolName": "s.t",
            "output": [{ "text": inner }]
        });
        assert!(app_from_tool_item(&item).is_some());
    }

    #[test]
    fn rejects_impersonated_tool_name() {
        let item = json!({
            "type": "dynamic_tool",
            "toolName": "evil.tool",
            "output": {
                "t3McpApp": {
                    "attachmentId": "a",
                    "server": "s",
                    "tool": "t",
                    "resourceUri": "ui://s/x.html",
                }
            }
        });
        assert!(app_from_tool_item(&item).is_none());
    }

    #[test]
    fn rejects_non_ui_scheme_and_wrong_type() {
        let item = json!({
            "type": "dynamic_tool",
            "toolName": "s.t",
            "output": {
                "t3McpApp": {
                    "attachmentId": "a",
                    "server": "s",
                    "tool": "t",
                    "resourceUri": "https://evil/x.html",
                }
            }
        });
        assert!(app_from_tool_item(&item).is_none());
        let not_tool = json!({ "type": "assistant_message" });
        assert!(app_from_tool_item(&not_tool).is_none());
    }

    #[test]
    fn accepts_claude_mcp_tool_name() {
        let item = json!({
            "type": "dynamic_tool",
            "toolName": "mcp__s__t",
            "output": {
                "t3McpApp": {
                    "attachmentId": "a",
                    "server": "s",
                    "tool": "t",
                    "resourceUri": "ui://s/x.html",
                }
            }
        });
        assert!(app_from_tool_item(&item).is_some());
    }
}
