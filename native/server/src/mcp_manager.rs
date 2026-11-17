//! Server-owned MCP client sessions.
//!
//! The TypeScript server proxies `mcpApps.callTool`/`toolInfo`/`readResource`
//! through the *live provider session* (`ProviderAdapterV2McpApps` on
//! ClaudeAdapterV2/CodexAdapterV2). Rust providers are one-shot turn
//! subprocesses, so there is no session to borrow — instead the server owns
//! one persistent stdio MCP client per configured server name.
//!
//! ## Config discovery
//!
//! T3 does not define its own MCP config (`packages/contracts/src/settings.ts`
//! has no `mcpServers` key); providers register servers in their own files.
//! `discover_server_configs` merges every place a `t3McpApp.server` name can
//! come from, in ascending precedence (later wins):
//!
//! 1. `$CODEX_HOME/config.toml` (default `~/.codex/config.toml`) —
//!    `[mcp_servers.<name>]` codex specs (`enabled = false` honored).
//! 2. `$CLAUDE_CONFIG_DIR/.claude.json` (default `~/.claude.json`) —
//!    top-level `mcpServers`, then `projects[project_dir].mcpServers`.
//! 3. `<project_dir>/.mcp.json` — the shared project-scope `mcpServers` file.
//! 4. `<t3 home>/settings.json` — `mcpServers`, a native extension with no
//!    contract counterpart; kept so a user can point the native bridge at a
//!    server no provider config knows about.
//!
//! Only stdio specs (`{command, args?, env?, cwd?}`) are spawnable; HTTP/SSE
//! `url` entries are skipped — a name found only there is indistinguishable
//! from a provider-builtin server (T3's own `t3-code` HTTP MCP), which needs
//! the provider's session-scoped endpoint and stays `session-stopped`.
//!
//! ## Framing
//!
//! MCP stdio transport is newline-delimited JSON-RPC 2.0 (no Content-Length
//! headers — that is the LSP-style framing some SDKs also accept). Each call
//! is a `request → response` round-trip; server→client requests
//! (`roots/list`, `sampling/createMessage`, `elicitation/create`, …) are
//! answered with `-32601` since we advertise no client capabilities, and
//! notifications are ignored. Stale responses (e.g. a reply that lands after
//! the 30s call timeout) are skipped by id mismatch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, Command};

/// Per-request ceiling (`McpAppRequests` leaves the provider's own timeout;
/// 30s matches common MCP client defaults).
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// `tools/list` pagination cap — one page is almost always enough.
const MAX_LIST_PAGES: u32 = 8;
/// A single protocol line larger than this is a misbehaving server, not data.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// Read budget per request so a noisy child cannot spin the loop forever.
const MAX_LINES_PER_REQUEST: u32 = 512;

const PROTOCOL_VERSION: &str = "2025-06-18";
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum McpError {
    /// No spawnable stdio config for this server name. Includes provider
    /// builtin servers (`t3-code`, `t3-code-direct`) — those are HTTP MCP
    /// endpoints scoped to a live provider session.
    NotConfigured,
    /// Spawn/write/read failure or unexpected EOF. Retryable once.
    Transport(String),
    /// The call exceeded CALL_TIMEOUT. Retryable once (fresh child).
    Timeout,
    /// The server answered with a JSON-RPC `error`. Never retryable.
    Remote(Value),
}

impl McpError {
    fn retryable(&self) -> bool {
        matches!(self, McpError::Transport(_) | McpError::Timeout)
    }
}

// ---------------------------------------------------------------------------
// Config discovery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
}

/// Parse one `{command, args?, env?, cwd?, type?, enabled?/disabled?}` spec in
/// the shared Claude/.mcp.json/settings.json shape. `env` may be an object of
/// string values (all three writers use that shape).
fn parse_stdio_spec(value: &Value) -> Option<McpServerConfig> {
    let obj = value.as_object()?;
    // HTTP/SSE servers (`type`/`url` without `command`) can't be spawned.
    let ty = obj.get("type").and_then(Value::as_str);
    if matches!(ty, Some("http") | Some("sse") | Some("websocket"))
        || obj.get("enabled").and_then(Value::as_bool) == Some(false)
        || obj.get("disabled").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let command = obj.get("command").and_then(Value::as_str)?;
    if command.trim().is_empty() {
        return None;
    }
    let args = obj
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let env = obj
        .get("env")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let cwd = obj
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from);
    Some(McpServerConfig {
        command: command.to_string(),
        args,
        env,
        cwd,
    })
}

fn merge_specs(out: &mut HashMap<String, McpServerConfig>, servers: Option<&Value>) {
    let Some(map) = servers.and_then(Value::as_object) else {
        return;
    };
    for (name, spec) in map {
        if let Some(config) = parse_stdio_spec(spec) {
            out.insert(name.clone(), config);
        } else {
            // A present-but-unspawnable entry (disabled/HTTP) shadows lower
            // precedence sources rather than resurrecting them.
            out.remove(name);
        }
    }
}

fn read_json_file(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// Codex `[mcp_servers.<name>]` tables — same fields as the JSON spec plus
/// `env` as an inline table; `url`-only (HTTP) entries parse to `None`.
fn merge_codex_config(out: &mut HashMap<String, McpServerConfig>) {
    let root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".codex")));
    let Some(text) = root.and_then(|r| std::fs::read_to_string(r.join("config.toml")).ok()) else {
        return;
    };
    let Ok(doc) = text.parse::<toml::Value>() else {
        return;
    };
    let Some(table) = doc.get("mcp_servers").and_then(toml::Value::as_table) else {
        return;
    };
    let json = match serde_json::to_value(table) {
        Ok(v) => v,
        Err(_) => return,
    };
    merge_specs(out, Some(&json));
}

/// All discovered stdio server specs by name; see module docs for sources
/// and precedence. `project_dir` selects Claude's per-project `mcpServers`
/// scope and the workspace `.mcp.json`.
pub fn discover_server_configs(
    t3_home: &Path,
    project_dir: &Path,
) -> HashMap<String, McpServerConfig> {
    let mut out = HashMap::new();

    merge_codex_config(&mut out);

    let claude_json_path = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".claude.json")))
        .map(|p| {
            if p.is_dir() {
                p.join(".claude.json")
            } else {
                p
            }
        });
    if let Some(config) = claude_json_path.and_then(|p| read_json_file(&p)) {
        // Global user scope first.
        merge_specs(&mut out, config.get("mcpServers"));
        // Project scope keyed by absolute workspace path; the raw and
        // canonicalized forms both appear in the wild.
        if let Some(projects) = config.get("projects").and_then(Value::as_object) {
            let raw = project_dir.to_string_lossy().to_string();
            let canonical = std::fs::canonicalize(project_dir)
                .map(|p| p.to_string_lossy().to_string())
                .ok();
            for key in [Some(raw), canonical].into_iter().flatten() {
                if let Some(entry) = projects.get(&key) {
                    merge_specs(&mut out, entry.get("mcpServers"));
                }
            }
        }
    }

    if let Some(dot_mcp) = read_json_file(&project_dir.join(".mcp.json")) {
        merge_specs(&mut out, dot_mcp.get("mcpServers"));
    }

    // Native extension: T3's own settings.json — highest precedence so a
    // hand-configured entry always wins.
    if let Some(settings) = read_json_file(&t3_home.join("settings.json")) {
        merge_specs(&mut out, settings.get("mcpServers"));
    }

    out
}

// ---------------------------------------------------------------------------
// JSON-RPC round-trip (testable without a child process)
// ---------------------------------------------------------------------------

/// Write one JSON-RPC message as a single line.
async fn write_message<W: AsyncWrite + Unpin>(writer: &mut W, msg: &Value) -> Result<(), McpError> {
    let mut line = serde_json::to_vec(msg).map_err(|e| McpError::Transport(e.to_string()))?;
    line.push(b'\n');
    writer
        .write_all(&line)
        .await
        .map_err(|e| McpError::Transport(format!("stdin write failed: {e}")))?;
    writer
        .flush()
        .await
        .map_err(|e| McpError::Transport(format!("stdin flush failed: {e}")))
}

/// One request → response round-trip over an already-initialized transport.
///
/// - Skips notifications, stale responses (id mismatch) and non-JSON noise.
/// - Answers server→client requests with `-32601 Method not found`.
/// - `Remote` on a JSON-RPC error, `Transport` on EOF/IO, `Timeout` upstream.
async fn roundtrip<W, R>(
    writer: &mut W,
    lines: &mut Lines<R>,
    next_id: &mut u64,
    method: &str,
    params: Value,
) -> Result<Value, McpError>
where
    W: AsyncWrite + Unpin,
    R: AsyncBufRead + Unpin,
{
    let id = *next_id;
    *next_id += 1;
    let request = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    write_message(writer, &request).await?;

    let mut read = 0u32;
    loop {
        if read >= MAX_LINES_PER_REQUEST {
            return Err(McpError::Transport(format!(
                "exceeded {MAX_LINES_PER_REQUEST} lines without a reply to {method}"
            )));
        }
        read += 1;
        let line = lines
            .next_line()
            .await
            .map_err(|e| McpError::Transport(format!("stdout read failed: {e}")))?
            .ok_or_else(|| McpError::Transport("MCP server closed stdout".to_string()))?;
        if line.len() > MAX_LINE_BYTES {
            return Err(McpError::Transport(format!(
                "oversized MCP message ({} bytes)",
                line.len()
            )));
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue; // non-protocol noise on stdout
        };
        let has_method = msg.get("method").and_then(Value::as_str).is_some();
        match msg.get("id") {
            // Server→client request: we advertise no capabilities, so the
            // honest reply is "method not found" rather than silence.
            Some(req_id) if has_method => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "error": { "code": JSONRPC_METHOD_NOT_FOUND, "message": "Method not found" },
                });
                // A failed error reply must not kill the pending call.
                let _ = write_message(writer, &err).await;
            }
            Some(resp_id) if resp_id == &json!(id) => {
                if let Some(error) = msg.get("error") {
                    return Err(McpError::Remote(error.clone()));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            _ => continue, // notification or stale response
        }
    }
}

// ---------------------------------------------------------------------------
// Session lifecycle
// ---------------------------------------------------------------------------

struct LiveChild {
    child: Child,
    stdin: tokio::process::ChildStdin,
    stdout: Lines<BufReader<tokio::process::ChildStdout>>,
    next_id: u64,
}

impl LiveChild {
    /// Spawn + `initialize` + `notifications/initialized`.
    async fn spawn(config: &McpServerConfig, default_cwd: &Path) -> Result<Self, McpError> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (k, v) in &config.env {
            cmd.env(k, v);
        }
        let cwd = config
            .cwd
            .clone()
            .unwrap_or_else(|| default_cwd.to_path_buf());
        if cwd.is_dir() {
            cmd.current_dir(&cwd);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| McpError::Transport(format!("spawn {} failed: {e}", config.command)))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("child stdin missing".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("child stdout missing".into()))?;
        let mut live = LiveChild {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            next_id: 1,
        };
        live.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "t3-code", "version": env!("CARGO_PKG_VERSION") },
            }),
        )
        .await
        .map_err(|e| match e {
            McpError::Remote(err) => McpError::Transport(format!("initialize rejected: {err}")),
            other => other,
        })?;
        write_message(
            &mut live.stdin,
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        )
        .await?;
        Ok(live)
    }

    /// One request with the call timeout applied.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        // If the child already died, fail fast so the caller respawns.
        match self.child.try_wait() {
            Ok(Some(status)) => {
                return Err(McpError::Transport(format!("MCP server exited ({status})")))
            }
            Ok(None) => {}
            Err(e) => return Err(McpError::Transport(format!("child poll failed: {e}"))),
        }
        match tokio::time::timeout(
            CALL_TIMEOUT,
            roundtrip(
                &mut self.stdin,
                &mut self.stdout,
                &mut self.next_id,
                method,
                params,
            ),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(McpError::Timeout),
        }
    }
}

struct Session {
    config: McpServerConfig,
    default_cwd: PathBuf,
    live: tokio::sync::Mutex<Option<LiveChild>>,
}

impl Session {
    /// Serialized call; a dead/hung child is restarted once per call.
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let mut guard = self.live.lock().await;

        let first = match guard.as_mut() {
            Some(live) => live.request(method, params.clone()).await,
            None => Err(McpError::Transport("session not started".into())),
        };
        match first {
            Ok(result) => return Ok(result),
            Err(e) if !e.retryable() => return Err(e),
            Err(_) => {}
        }

        // One restart per call: drop the corpse, respawn, retry once.
        *guard = None;
        let mut live = LiveChild::spawn(&self.config, &self.default_cwd).await?;
        match live.request(method, params).await {
            Ok(result) => {
                *guard = Some(live);
                Ok(result)
            }
            Err(e) => {
                // Child dies with the guard; next call starts fresh.
                Err(e)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// McpManager
// ---------------------------------------------------------------------------

pub struct McpManager {
    /// Live sessions by config name; entries persist while the child is
    /// healthy. Config re-reads happen per call so edits pick up on the next
    /// spawn — a cached session is not re-validated against config drift.
    sessions: Mutex<HashMap<String, std::sync::Arc<Session>>>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Process-wide instance used by the RPC layer.
    pub fn global() -> &'static McpManager {
        static MANAGER: OnceLock<McpManager> = OnceLock::new();
        MANAGER.get_or_init(McpManager::new)
    }

    fn session(
        &self,
        server: &str,
        config: &McpServerConfig,
        default_cwd: &Path,
    ) -> std::sync::Arc<Session> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions
            .entry(server.to_string())
            .or_insert_with(|| {
                std::sync::Arc::new(Session {
                    config: config.clone(),
                    default_cwd: default_cwd.to_path_buf(),
                    live: tokio::sync::Mutex::new(None),
                })
            })
            .clone()
    }

    /// Any JSON-RPC request against a configured server.
    async fn request(
        &self,
        config: &McpServerConfig,
        server: &str,
        default_cwd: &Path,
        method: &str,
        params: Value,
    ) -> Result<Value, McpError> {
        self.session(server, config, default_cwd)
            .request(method, params)
            .await
    }

    /// `tools/list` with cursor pagination; returns the raw tool objects.
    pub async fn list_tools(
        &self,
        config: &McpServerConfig,
        server: &str,
        default_cwd: &Path,
    ) -> Result<Vec<Value>, McpError> {
        let mut tools = Vec::new();
        let mut cursor = Value::Null;
        for _ in 0..MAX_LIST_PAGES {
            let mut params = json!({});
            if !cursor.is_null() {
                params["cursor"] = cursor.clone();
            }
            let result = self
                .request(config, server, default_cwd, "tools/list", params)
                .await?;
            if let Some(page) = result.get("tools").and_then(Value::as_array) {
                tools.extend(page.iter().cloned());
            }
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) => cursor = json!(next),
                None => break,
            }
        }
        Ok(tools)
    }

    /// Find one tool by name through `tools/list` (early-exits on match).
    pub async fn find_tool(
        &self,
        config: &McpServerConfig,
        server: &str,
        default_cwd: &Path,
        name: &str,
    ) -> Result<Option<Value>, McpError> {
        let mut cursor = Value::Null;
        for _ in 0..MAX_LIST_PAGES {
            let mut params = json!({});
            if !cursor.is_null() {
                params["cursor"] = cursor.clone();
            }
            let result = self
                .request(config, server, default_cwd, "tools/list", params)
                .await?;
            if let Some(page) = result.get("tools").and_then(Value::as_array) {
                if let Some(tool) = page
                    .iter()
                    .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
                {
                    return Ok(Some(tool.clone()));
                }
            }
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) => cursor = json!(next),
                None => return Ok(None),
            }
        }
        Ok(None)
    }

    pub async fn call_tool(
        &self,
        config: &McpServerConfig,
        server: &str,
        default_cwd: &Path,
        name: &str,
        arguments: Value,
    ) -> Result<Value, McpError> {
        self.request(
            config,
            server,
            default_cwd,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    pub async fn read_resource(
        &self,
        config: &McpServerConfig,
        server: &str,
        default_cwd: &Path,
        uri: &str,
    ) -> Result<Value, McpError> {
        self.request(
            config,
            server,
            default_cwd,
            "resources/read",
            json!({ "uri": uri }),
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// Drive the "server" end of a duplex link: answer each request with
    /// `responder(request_json) -> response_json`.
    async fn serve(io: tokio::io::DuplexStream, responder: fn(&Value) -> Value) {
        let (mut rh, mut wh) = tokio::io::split(io);
        let mut lines = BufReader::new(&mut rh).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(req) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let resp = responder(&req);
            let mut bytes = serde_json::to_vec(&resp).unwrap();
            bytes.push(b'\n');
            if wh.write_all(&bytes).await.is_err() {
                return;
            }
        }
    }

    fn responder(req: &Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": req.get("id").cloned().unwrap_or(Value::Null),
            "result": { "echo": req.get("method").cloned().unwrap_or(Value::Null) },
        })
    }

    #[tokio::test]
    async fn roundtrip_returns_result() {
        let (client, server) = duplex(64 * 1024);
        let (rh, mut wh) = tokio::io::split(client);
        tokio::spawn(serve(server, responder));
        let mut lines = BufReader::new(rh).lines();
        let mut next_id = 1;
        let result = roundtrip(&mut wh, &mut lines, &mut next_id, "tools/list", json!({}))
            .await
            .unwrap();
        assert_eq!(result["echo"], "tools/list");
        assert_eq!(next_id, 2);
    }

    #[tokio::test]
    async fn roundtrip_maps_jsonrpc_error_to_remote() {
        let (client, server) = duplex(64 * 1024);
        let (rh, mut wh) = tokio::io::split(client);
        tokio::spawn(serve(server, |req| {
            json!({
                "jsonrpc": "2.0",
                "id": req["id"],
                "error": { "code": -32602, "message": "bad params" },
            })
        }));
        let mut lines = BufReader::new(rh).lines();
        let mut next_id = 1;
        match roundtrip(&mut wh, &mut lines, &mut next_id, "tools/call", json!({})).await {
            Err(McpError::Remote(err)) => assert_eq!(err["code"], -32602),
            other => panic!("expected Remote, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn roundtrip_skips_noise_notifications_and_stale_ids() {
        let (client, server) = duplex(64 * 1024);
        let (rh, mut wh) = tokio::io::split(client);
        tokio::spawn(async move {
            let (mut srh, mut swh) = tokio::io::split(server);
            let mut lines = BufReader::new(&mut srh).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(req) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let noise = concat!(
                    "not json at all\n",
                    "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n",
                    "{\"jsonrpc\":\"2.0\",\"id\":999,\"result\":{}}\n",
                );
                swh.write_all(noise.as_bytes()).await.unwrap();
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": req["id"],
                    "result": { "ok": true },
                });
                let mut b = serde_json::to_vec(&resp).unwrap();
                b.push(b'\n');
                swh.write_all(&b).await.unwrap();
            }
        });
        let mut lines = BufReader::new(rh).lines();
        let mut next_id = 1;
        let result = roundtrip(&mut wh, &mut lines, &mut next_id, "tools/list", json!({}))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
    }

    #[tokio::test]
    async fn roundtrip_replies_method_not_found_to_server_requests() {
        let (client, server) = duplex(64 * 1024);
        let (rh, mut wh) = tokio::io::split(client);
        tokio::spawn(async move {
            let (mut srh, mut swh) = tokio::io::split(server);
            let mut lines = BufReader::new(&mut srh).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(req) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if req.get("method").is_none() {
                    continue; // our own error reply bouncing back — ignore
                }
                // Server-initiated request first, then the real response.
                let inbound = json!({
                    "jsonrpc": "2.0", "id": "srv-1", "method": "roots/list", "params": {}
                });
                let mut b = serde_json::to_vec(&inbound).unwrap();
                b.push(b'\n');
                swh.write_all(&b).await.unwrap();
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": req["id"],
                    "result": { "ok": true },
                });
                let mut b = serde_json::to_vec(&resp).unwrap();
                b.push(b'\n');
                swh.write_all(&b).await.unwrap();
            }
        });
        let mut lines = BufReader::new(rh).lines();
        let mut next_id = 1;
        let result = roundtrip(&mut wh, &mut lines, &mut next_id, "tools/list", json!({}))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
    }

    #[tokio::test]
    async fn roundtrip_eof_is_transport_error() {
        let (client, server) = duplex(64 * 1024);
        drop(server); // immediate EOF
        let (rh, mut wh) = tokio::io::split(client);
        let mut lines = BufReader::new(rh).lines();
        let mut next_id = 1;
        match roundtrip(&mut wh, &mut lines, &mut next_id, "tools/list", json!({})).await {
            Err(McpError::Transport(_)) => {}
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    // -- config discovery ---------------------------------------------------

    #[test]
    fn parse_stdio_spec_accepts_full_shape() {
        let spec = parse_stdio_spec(&json!({
            "type": "stdio",
            "command": "/usr/bin/echo",
            "args": ["a", "b"],
            "env": { "KEY": "val", "N": 42 },
            "cwd": "/tmp",
        }))
        .unwrap();
        assert_eq!(spec.command, "/usr/bin/echo");
        assert_eq!(spec.args, vec!["a", "b"]);
        assert_eq!(spec.env, vec![("KEY".to_string(), "val".to_string())]);
        assert_eq!(spec.cwd, Some(PathBuf::from("/tmp")));
    }

    #[test]
    fn parse_stdio_spec_rejects_http_disabled_and_missing_command() {
        assert!(parse_stdio_spec(&json!({"type": "http", "url": "http://x"})).is_none());
        assert!(parse_stdio_spec(&json!({"url": "http://x", "command": "c"})).is_some());
        assert!(parse_stdio_spec(&json!({"command": "c", "enabled": false})).is_none());
        assert!(parse_stdio_spec(&json!({"command": "c", "disabled": true})).is_none());
        assert!(parse_stdio_spec(&json!({"args": []})).is_none());
    }

    #[test]
    fn discover_merges_sources_in_precedence_order() {
        let base = std::env::temp_dir().join(format!("mcp-cfg-{}", uuid::Uuid::new_v4()));
        let t3_home = base.join("t3");
        let project = base.join("proj");
        std::fs::create_dir_all(&t3_home).unwrap();
        std::fs::create_dir_all(&project).unwrap();

        // Project .mcp.json defines `alpha`; settings.json overrides `alpha`
        // and adds `beta`.
        std::fs::write(
            project.join(".mcp.json"),
            json!({
                "mcpServers": {
                    "alpha": { "command": "from-mcp-json", "args": ["1"] },
                    "gone": { "command": "x" }
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            t3_home.join("settings.json"),
            json!({
                "mcpServers": {
                    "alpha": { "command": "from-settings", "args": ["2"] },
                    "beta": { "command": "b", "args": [] },
                    "gone": { "command": "x", "enabled": false }
                }
            })
            .to_string(),
        )
        .unwrap();

        let configs = discover_server_configs(&t3_home, &project);
        assert_eq!(configs["alpha"].command, "from-settings");
        assert_eq!(configs["beta"].command, "b");
        assert!(!configs.contains_key("gone"));

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn codex_toml_mcp_servers_are_discovered() {
        // Feed the toml parser directly: env-dependent CODEX_HOME paths are
        // covered by merge_codex_config in the field.
        let doc: toml::Value = r#"
            [mcp_servers.weather]
            command = "weather-mcp"
            args = ["--fast"]
            enabled = true

            [mcp_servers.weather.env]
            KEY = "v"

            [mcp_servers.off]
            command = "nope"
            enabled = false

            [mcp_servers.remote]
            url = "https://example.com/mcp"
        "#
        .parse()
        .unwrap();
        let table = doc.get("mcp_servers").unwrap();
        let json = serde_json::to_value(table).unwrap();
        let mut out = HashMap::new();
        merge_specs(&mut out, Some(&json));
        assert_eq!(out["weather"].command, "weather-mcp");
        assert_eq!(out["weather"].args, vec!["--fast"]);
        assert_eq!(
            out["weather"].env,
            vec![("KEY".to_string(), "v".to_string())]
        );
        assert!(!out.contains_key("off"));
        assert!(!out.contains_key("remote"));
    }

    // -- live child -----------------------------------------------------------

    /// A stdio MCP stub speaking initialize/tools/resources over NDJSON.
    #[cfg(unix)]
    const STUB: &str = concat!(
        "import sys, json\n",
        "for line in sys.stdin:\n",
        "    try: req = json.loads(line)\n",
        "    except Exception: continue\n",
        "    mid, method = req.get('id'), req.get('method')\n",
        "    if method == 'initialize':\n",
        "        resp = {'protocolVersion': req['params']['protocolVersion'],",
        " 'capabilities': {'tools': {}, 'resources': {}},",
        " 'serverInfo': {'name': 'stub', 'version': '0'}}\n",
        "    elif method == 'notifications/initialized':\n",
        "        continue\n",
        "    elif method == 'tools/list':\n",
        "        resp = {'tools': [{'name': 'echo', 'title': 'Echo',",
        " 'inputSchema': {'type': 'object'},",
        " 'annotations': {'readOnlyHint': True, 'title': 'Echo'},",
        " '_meta': {'ui': {'visibility': ['app', 'model']}}}]}\n",
        "    elif method == 'tools/call':\n",
        "        resp = {'content': [{'type': 'text', 'text': 'echo:' + json.dumps(req['params'].get('arguments', {}))}]}\n",
        "    elif method == 'resources/read':\n",
        "        resp = {'contents': [{'uri': req['params']['uri'], 'text': 'html'}]}\n",
        "    else:\n",
        "        resp = None\n",
        "    if mid is None:\n",
        "        continue\n",
        "    out = {'jsonrpc': '2.0', 'id': mid}\n",
        "    if resp is None: out['error'] = {'code': -32601, 'message': 'nf'}\n",
        "    else: out['result'] = resp\n",
        "    sys.stdout.write(json.dumps(out) + '\\n')\n",
        "    sys.stdout.flush()\n",
    );

    #[cfg(unix)]
    fn stub_config() -> Option<McpServerConfig> {
        // Skip cleanly on hosts without python3.
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            return None;
        }
        Some(McpServerConfig {
            command: "python3".to_string(),
            args: vec!["-c".to_string(), STUB.to_string()],
            env: vec![],
            cwd: None,
        })
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn live_stub_initialize_list_call_read() {
        let Some(config) = stub_config() else {
            return;
        };
        let manager = McpManager::new();
        let cwd = std::env::temp_dir();

        let tools = manager
            .list_tools(&config, "stub-live", &cwd)
            .await
            .expect("tools/list");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "echo");
        assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);
        assert_eq!(
            tools[0]["_meta"]["ui"]["visibility"],
            json!(["app", "model"])
        );

        let found = manager
            .find_tool(&config, "stub-live", &cwd, "echo")
            .await
            .unwrap();
        assert!(found.is_some());
        assert!(manager
            .find_tool(&config, "stub-live", &cwd, "nope")
            .await
            .unwrap()
            .is_none());

        let called = manager
            .call_tool(&config, "stub-live", &cwd, "echo", json!({"x": 1}))
            .await
            .unwrap();
        assert_eq!(called["content"][0]["text"], "echo:{\"x\": 1}");

        let read = manager
            .read_resource(&config, "stub-live", &cwd, "ui://stub/app.html")
            .await
            .unwrap();
        assert_eq!(read["contents"][0]["uri"], "ui://stub/app.html");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dead_child_restarts_once_per_call() {
        // `true` exits instantly — every call hits Transport, respawns once,
        // and reports Transport rather than hanging.
        let config = McpServerConfig {
            command: "true".to_string(),
            args: vec![],
            env: vec![],
            cwd: None,
        };
        let manager = McpManager::new();
        match manager
            .list_tools(&config, "dead", std::env::temp_dir().as_path())
            .await
        {
            Err(McpError::Transport(_)) | Err(McpError::Timeout) => {}
            other => panic!("expected Transport/Timeout, got {other:?}"),
        }
    }
}
