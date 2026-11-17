//! Minimal client for a 67 server: HTTP websocket-ticket mint + Effect-RPC
//! over WebSocket (`packages/contracts/src/rpc.ts`, JSON serialization).
//!
//! Wire format (`.repos/effect/.../rpc/RpcMessage.ts`):
//!   client -> {"_tag":"Request","id":N,"tag":"method","payload":{...},"headers":[]}
//!   server -> {"_tag":"Chunk","requestId":N,"values":[...]}
//!             {"_tag":"Exit","requestId":N,"exit":{"_tag":"Success","value":...}}
//!             {"_tag":"Ping"}  (answer with {"_tag":"Pong"})

use std::io::ErrorKind;
use std::time::Duration;

use serde_json::{Value, json};
use smol::channel::Sender;

#[derive(Debug, Clone)]
pub struct ServerEndpoint {
    /// e.g. `http://127.0.0.1:13773`
    pub http_url: String,
    /// Bearer credential (`CODE67_DEV_AUTH_TOKEN` in dev).
    pub token: String,
}

impl ServerEndpoint {
    pub fn from_env() -> Option<Self> {
        let http_url = std::env::var("C67_SERVER_URL").ok()?;
        let token = std::env::var("C67_TOKEN").ok()?;
        Some(Self { http_url, token })
    }

    fn ws_url(&self) -> String {
        let mut url = self.http_url.trim_end_matches('/').to_string();
        if let Some(rest) = url.strip_prefix("http://") {
            url = format!("ws://{rest}");
        } else if let Some(rest) = url.strip_prefix("https://") {
            url = format!("wss://{rest}");
        }
        url
    }
}

#[derive(Debug, Clone)]
pub struct ShellProject {
    pub id: String,
    pub title: String,
    pub workspace_root: String,
}

#[derive(Debug, Clone)]
pub struct ShellThread {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub status: String,
    pub archived: bool,
    pub deleted: bool,
    /// `activityRunStatus` — live status of the in-flight run, if any.
    pub activity_run_status: Option<String>,
    /// `latestRunStatus`-derived top status (completed/failed/interrupted/…).
    pub latest_run_id: Option<String>,
    /// A runtime request (approval/user-input) is pending on this thread.
    pub has_pending_request: bool,
    /// A proposed plan awaits a decision.
    pub has_actionable_proposed_plan: bool,
    /// `providerInstanceHistory` — providers that have run on this thread.
    pub provider_instances: Vec<String>,
    /// `pinned` ordering key from the shell, if any.
    pub pinned: bool,
    /// `updatedAt` ISO timestamp.
    pub updated_at: String,
    /// `snoozedUntil` ISO timestamp while the thread is snoozed.
    pub snoozed_until: Option<String>,
    /// `markedUnreadAt` ISO timestamp — explicit unread marker.
    pub marked_unread_at: Option<String>,
    /// `visitedAt` ISO timestamp — last time the user opened the thread.
    pub visited_at: Option<String>,
    /// `latestRunStartedAt` ISO timestamp for unread comparison.
    pub latest_run_started_at: Option<String>,
    /// `linkedPullRequest` object (e.g. `{url,...}`) when a PR is linked.
    pub linked_pull_request: Option<Value>,
    /// `pinOrderKey`/`activeOrderKey` ordering keys.
    pub pin_order_key: Option<String>,
    pub active_order_key: Option<String>,
    /// `interactionMode`/`runtimeMode`/`modelSelection` from the shell.
    pub interaction_mode: Option<String>,
    pub runtime_mode: Option<String>,
    pub model_selection: Option<Value>,
    /// `branch`/`worktreePath` when the thread targets a non-default workspace.
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ShellSnapshot {
    pub projects: Vec<ShellProject>,
    pub threads: Vec<ShellThread>,
}

/// A render-ready view of one `OrchestrationV2TurnItem`.
#[derive(Debug, Clone)]
pub struct TurnItemView {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub streaming: bool,
    pub status: String,
    /// The complete wire payload — detail fields (diffStr, output, exitCode,
    /// approval options, steps, …) for rich rendering.
    pub raw: serde_json::Value,
}

#[derive(Debug)]
pub enum ShellEvent {
    Connected,
    Snapshot(ShellSnapshot),
    ProjectUpserted(ShellProject),
    ProjectRemoved {
        project_id: String,
    },
    ThreadUpserted {
        thread: ShellThread,
    },
    ThreadRemoved {
        thread_id: String,
    },
    ThreadSnapshot {
        items: Vec<TurnItemView>,
    },
    ThreadItemUpdated {
        item: TurnItemView,
    },
    OlderSettledThreads {
        threads: Vec<ShellThread>,
    },
    /// Enrichment refresh: `resolvedRepositoryIdentityRoots` is present, so the
    /// snapshot carries project updates only — threads must be preserved.
    ProjectsRefreshed {
        projects: Vec<ShellProject>,
    },
    /// Raw auth-access stream value (`{type:"clientUpserted"|"clientRemoved"|
    /// "snapshot"|…, payload}`); the clients surface folds these into state.
    AuthAccessChange {
        value: Value,
    },
    /// Chunk values from a `ClientCommand::Subscribe` stream, raw and
    /// unparsed — the owning surface decodes its own event shapes.
    StreamChunk {
        stream_id: String,
        values: Vec<Value>,
    },
    /// The stream exited (success, failure, or socket drop).
    StreamEnd {
        stream_id: String,
    },
    Error(String),
    Disconnected(String),
}

#[derive(Debug)]
pub enum ClientCommand {
    SubscribeShell,
    GetOlderSettled,
    /// `subscribeAuthAccess` stream — authorized clients + pairing links.
    /// Chunk values arrive as ShellEvent::AuthAccessChange.
    SubscribeAuthAccess,
    SubscribeThread {
        thread_id: String,
    },
    /// A raw `OrchestrationV2Command` JSON payload.
    Dispatch {
        command: Value,
    },
    /// Request/response RPC (`{"_tag":"Request","tag":<tag>}`); the reply
    /// sender resolves once with the `Exit` frame's value or an error object.
    Query {
        tag: String,
        payload: Value,
        reply: smol::channel::Sender<Value>,
    },
    /// Open a windowed RPC stream (`terminal.attach`, `subscribeVcsStatus`, …).
    /// Chunks arrive as `ShellEvent::StreamChunk` keyed by `stream_id`; the
    /// Exit arrives as `ShellEvent::StreamEnd`.
    Subscribe {
        tag: String,
        payload: Value,
        stream_id: String,
    },
    /// Interrupt an open `Subscribe` stream by its `stream_id`.
    InterruptStream {
        stream_id: String,
    },
    /// Drop the current socket (or pending backoff) and reconnect now.
    Reconnect,
    /// Point the connection loop at a different server (environment switch).
    SwitchEndpoint {
        http_url: String,
        token: String,
    },
    #[allow(dead_code)]
    Interrupt {
        request_id: u64,
    },
}

fn parse_project(v: &Value) -> ShellProject {
    ShellProject {
        id: v["id"].as_str().unwrap_or_default().to_string(),
        title: v["title"]
            .as_str()
            .unwrap_or("Untitled project")
            .to_string(),
        workspace_root: v["workspaceRoot"].as_str().unwrap_or_default().to_string(),
    }
}

fn parse_thread(v: &Value) -> ShellThread {
    ShellThread {
        id: v["id"].as_str().unwrap_or_default().to_string(),
        project_id: v["projectId"].as_str().unwrap_or_default().to_string(),
        title: v["title"].as_str().unwrap_or("Untitled").to_string(),
        status: v["status"].as_str().unwrap_or("idle").to_string(),
        archived: !v["archivedAt"].is_null(),
        deleted: !v["deletedAt"].is_null(),
        activity_run_status: v["activityRunStatus"].as_str().map(str::to_owned),
        latest_run_id: v["latestRunId"].as_str().map(str::to_owned),
        has_pending_request: !v["pendingRuntimeRequest"].is_null(),
        has_actionable_proposed_plan: v["hasActionableProposedPlan"].as_bool() == Some(true),
        provider_instances: v["providerInstanceHistory"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        pinned: v["pinned"].as_bool() == Some(true)
            || !v["pinnedOrderKey"].is_null()
            || !v["orderKey"].is_null(),
        updated_at: v["updatedAt"].as_str().unwrap_or_default().to_string(),
        snoozed_until: v["snoozedUntil"].as_str().map(str::to_owned),
        marked_unread_at: v["markedUnreadAt"].as_str().map(str::to_owned),
        visited_at: v["visitedAt"].as_str().map(str::to_owned),
        latest_run_started_at: v["latestRunStartedAt"].as_str().map(str::to_owned),
        linked_pull_request: v.get("linkedPullRequest").filter(|v| !v.is_null()).cloned(),
        pin_order_key: v["pinOrderKey"]
            .as_str()
            .or_else(|| v["pinnedOrderKey"].as_str())
            .map(str::to_owned),
        active_order_key: v["activeOrderKey"].as_str().map(str::to_owned),
        interaction_mode: v["interactionMode"].as_str().map(str::to_owned),
        runtime_mode: v["runtimeMode"].as_str().map(str::to_owned),
        model_selection: v.get("modelSelection").filter(|v| !v.is_null()).cloned(),
        branch: v["branch"].as_str().map(str::to_owned),
        worktree_path: v["worktreePath"].as_str().map(str::to_owned),
    }
}

fn first_lines(v: &Value, max: usize) -> Option<String> {
    let s = v.as_str()?;
    let lines: Vec<&str> = s.lines().take(max).collect();
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn turn_item_body(item: &Value) -> Option<String> {
    let kind = item["type"].as_str()?;
    let text = match kind {
        "user_message" | "assistant_message" | "reasoning" | "system_notice" | "error" => {
            item["text"]
                .as_str()
                .or_else(|| item["message"].as_str())
                .map(|s| s.to_string())
        }
        "proposed_plan" => item["markdown"].as_str().map(|s| s.to_string()),
        "command_execution" => {
            let cmd = item["input"].as_str().unwrap_or("command");
            let mut out = format!("$ {cmd}");
            if let Some(exit) = item["exitCode"].as_i64() {
                out.push_str(&format!("  (exit {exit})"));
            }
            if let Some(o) = first_lines(&item["output"], 12) {
                out.push_str(&format!("\n{o}"));
            }
            Some(out)
        }
        "dynamic_tool" => {
            let name = item["toolName"].as_str().unwrap_or("tool");
            let mut out = name.to_string();
            if let Some(title) = item["title"].as_str() {
                out.push_str(&format!("  {title}"));
            }
            if let Some(o) = first_lines(&item["output"], 10) {
                out.push_str(&format!("\n{o}"));
            }
            Some(out)
        }
        "file_change" => {
            let f = item["fileName"].as_str()?;
            let mut out = format!("edited {f}");
            let adds = item["additions"].as_u64().unwrap_or(0);
            let dels = item["deletions"].as_u64().unwrap_or(0);
            if adds + dels > 0 {
                out.push_str(&format!("  +{adds} -{dels}"));
            }
            Some(out)
        }
        "todo_list" => {
            let steps = item["steps"].as_array()?;
            let mut lines = Vec::new();
            for s in steps.iter().take(12) {
                let title = s["content"]
                    .as_str()
                    .or_else(|| s["title"].as_str())
                    .unwrap_or("step");
                let status = s["status"].as_str().unwrap_or("pending");
                let mark = match status {
                    "completed" => "☑",
                    "in_progress" | "running" => "▸",
                    _ => "☐",
                };
                lines.push(format!("{mark} {title}"));
            }
            Some(lines.join("\n"))
        }
        "checkpoint" => item["files"]
            .as_array()
            .map(|f| format!("checkpoint: {} files", f.len())),
        "user_input_request" => {
            let qs = item["questions"].as_array()?;
            let line = qs
                .iter()
                .filter_map(|q| q["question"].as_str().or_else(|| q["header"].as_str()))
                .collect::<Vec<_>>()
                .join(" ");
            Some(line)
        }
        "approval_request" => Some(
            item["prompt"]
                .as_str()
                .unwrap_or("approval requested")
                .to_string(),
        ),
        "notification" => item["message"]
            .as_str()
            .or_else(|| item["text"].as_str())
            .map(|s| s.to_string()),
        "compaction" => Some("context compacted".to_string()),
        "subagent" => {
            let p = item["prompt"].as_str().unwrap_or("subagent");
            let mut out = format!("subagent: {}", p.chars().take(80).collect::<String>());
            if let Some(r) = item["result"].as_str() {
                out.push_str(&format!(
                    "\n{}",
                    r.lines().take(6).collect::<Vec<_>>().join("\n")
                ));
            }
            Some(out)
        }
        _ => None,
    };
    text.filter(|t| !t.is_empty())
}

fn parse_turn_item(item: &Value) -> Option<TurnItemView> {
    Some(TurnItemView {
        id: item["id"].as_str()?.to_string(),
        kind: item["type"].as_str()?.to_string(),
        title: item["title"].as_str().map(|s| s.to_string()),
        body: turn_item_body(item),
        streaming: item["streaming"].as_bool().unwrap_or(false),
        status: item["status"].as_str().unwrap_or("done").to_string(),
        raw: item.clone(),
    })
}

/// One item of the `orchestration.subscribeThread` stream.
fn parse_thread_stream_item(v: &Value) -> Option<ShellEvent> {
    match v["kind"].as_str()? {
        "snapshot" => {
            let items = v["projection"]["visibleTurnItems"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|entry| parse_turn_item(&entry["item"]))
                        .collect()
                })
                .unwrap_or_default();
            Some(ShellEvent::ThreadSnapshot { items })
        }
        "event" => {
            let event = &v["event"];
            if event["type"].as_str() == Some("turn-item.updated") {
                parse_turn_item(&event["payload"])
                    .map(|item| ShellEvent::ThreadItemUpdated { item })
            } else {
                None
            }
        }
        _ => None,
    }
}

fn parse_stream_item(v: &Value) -> Option<ShellEvent> {
    match v["kind"].as_str()? {
        "snapshot" => {
            let s = &v["snapshot"];
            if v["resolvedRepositoryIdentityRoots"].is_array() {
                return Some(ShellEvent::ProjectsRefreshed {
                    projects: s["projects"]
                        .as_array()
                        .map(|a| a.iter().map(parse_project).collect())
                        .unwrap_or_default(),
                });
            }
            Some(ShellEvent::Snapshot(ShellSnapshot {
                projects: s["projects"]
                    .as_array()
                    .map(|a| a.iter().map(parse_project).collect())
                    .unwrap_or_default(),
                threads: s["threads"]
                    .as_array()
                    .map(|a| a.iter().map(parse_thread).collect())
                    .unwrap_or_default(),
            }))
        }
        "project.updated" => Some(ShellEvent::ProjectUpserted(parse_project(&v["project"]))),
        "project.removed" => Some(ShellEvent::ProjectRemoved {
            project_id: v["projectId"].as_str().unwrap_or_default().to_string(),
        }),
        "thread.updated" => Some(ShellEvent::ThreadUpserted {
            thread: parse_thread(&v["thread"]),
        }),
        "thread.removed" => Some(ShellEvent::ThreadRemoved {
            thread_id: v["threadId"].as_str().unwrap_or_default().to_string(),
        }),
        "synchronized" | "snapshot-required" => None,
        _ => None,
    }
}

fn mint_ticket(endpoint: &ServerEndpoint) -> Result<String, String> {
    let url = format!(
        "{}/api/auth/websocket-ticket/rpc",
        endpoint.http_url.trim_end_matches('/')
    );
    let mut response = ureq::post(&url)
        .header("authorization", format!("Bearer {}", endpoint.token))
        .header("content-type", "application/json")
        .send_json(json!({}))
        .map_err(|e| format!("ticket request failed: {e}"))?;
    let body: Value = response
        .body_mut()
        .read_json()
        .map_err(|e| format!("ticket response decode failed: {e}"))?;
    body["ticket"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("no ticket in response: {body}"))
}

/// Connects, subscribes to the orchestration shell stream, and forwards parsed
/// events on `events`. Runs on a dedicated thread; `commands` carries outbound
/// RPC requests. Reconnects with exponential backoff until the app exits.
pub fn run(
    mut endpoint: ServerEndpoint,
    events: Sender<ShellEvent>,
    commands: std::sync::mpsc::Receiver<ClientCommand>,
) {
    let mut backoff = Duration::from_millis(500);
    // Commands issued while disconnected queue here for the next session.
    let mut deferred: std::collections::VecDeque<ClientCommand> = Default::default();
    loop {
        match run_inner(&mut endpoint, events.clone(), &commands, &mut deferred) {
            Err((connected, e)) => {
                let _ = events.send_blocking(ShellEvent::Disconnected(e));
                // After a healthy session retry soon; a refused reconnect backs off.
                backoff = if connected {
                    Duration::from_millis(500)
                } else {
                    (backoff * 2).min(Duration::from_secs(15))
                };
                // Wait out the backoff, but a manual Reconnect retries now and
                // SwitchEndpoint swaps target immediately; other commands
                // issued during downtime ride the next session.
                let deadline = std::time::Instant::now() + backoff;
                while std::time::Instant::now() < deadline {
                    let mut wake = false;
                    while let Ok(cmd) = commands.try_recv() {
                        match cmd {
                            ClientCommand::Reconnect => wake = true,
                            ClientCommand::SwitchEndpoint { http_url, token } => {
                                endpoint = ServerEndpoint { http_url, token };
                                wake = true;
                            }
                            other => deferred.push_back(other),
                        }
                    }
                    if wake {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            Ok(()) => return,
        }
    }
}

/// Returns `Err((was_connected, error))` on disconnect. `deferred` carries
/// commands issued during downtime; they are sent right after the session
/// subscriptions. `endpoint` may be swapped mid-loop by SwitchEndpoint.
fn run_inner(
    endpoint: &mut ServerEndpoint,
    events: Sender<ShellEvent>,
    commands: &std::sync::mpsc::Receiver<ClientCommand>,
    deferred: &mut std::collections::VecDeque<ClientCommand>,
) -> Result<(), (bool, String)> {
    let ticket = mint_ticket(endpoint).map_err(|e| (false, e))?;
    let ws_url = format!(
        "{}/ws?wsTicket={ticket}&frameParts=1&orchestrationProtocol=2",
        endpoint.ws_url()
    );

    let (mut socket, _response) = tungstenite::connect(&ws_url)
        .map_err(|e| (false, format!("websocket connect failed: {e}")))?;
    if let tungstenite::stream::MaybeTlsStream::Plain(tcp) = socket.get_mut() {
        tcp.set_read_timeout(Some(Duration::from_millis(50)))
            .map_err(|e| (false, format!("socket config failed: {e}")))?;
    }

    let _ = events.send_blocking(ShellEvent::Connected);
    let fail = |e: String| (true, e);

    let mut next_id: u64 = 1;
    let mut subscriptions: std::collections::HashMap<u64, SubKind> =
        std::collections::HashMap::new();
    let mut queries: std::collections::HashMap<u64, smol::channel::Sender<Value>> =
        std::collections::HashMap::new();
    let mut pending_commands: std::collections::VecDeque<ClientCommand> = [
        ClientCommand::SubscribeShell,
        ClientCommand::GetOlderSettled,
        ClientCommand::SubscribeAuthAccess,
    ]
    .into();
    pending_commands.append(deferred);
    let mut pending = String::new();

    loop {
        let mut send_one =
            |socket: &mut _,
             subscriptions: &mut _,
             queries: &mut std::collections::HashMap<u64, smol::channel::Sender<Value>>,
             id: u64,
             command: &ClientCommand|
             -> Result<Option<String>, tungstenite::Error> {
                match command {
                    ClientCommand::Reconnect => return Ok(Some("reconnect requested".into())),
                    ClientCommand::SwitchEndpoint { http_url, token } => {
                        *endpoint = ServerEndpoint {
                            http_url: http_url.clone(),
                            token: token.clone(),
                        };
                        return Ok(Some("switching environment".into()));
                    }
                    ClientCommand::Query { reply, .. } => {
                        queries.insert(id, reply.clone());
                    }
                    _ => {}
                }
                send_command(socket, subscriptions, id, command)?;
                Ok(None)
            };

        if let Some(command) = pending_commands.pop_front() {
            if let Some(reason) = send_one(
                &mut socket,
                &mut subscriptions,
                &mut queries,
                next_id,
                &command,
            )
            .map_err(|e| fail(format!("send failed: {e}")))?
            {
                return Err(fail(reason));
            }
            next_id += 1;
        }
        while let Ok(command) = commands.try_recv() {
            if let Some(reason) = send_one(
                &mut socket,
                &mut subscriptions,
                &mut queries,
                next_id,
                &command,
            )
            .map_err(|e| fail(format!("send failed: {e}")))?
            {
                return Err(fail(reason));
            }
            next_id += 1;
        }

        match socket.read() {
            Ok(tungstenite::Message::Text(text)) => {
                // Large frames arrive split: every part but the last is
                // prefixed with \u{1e}. Accumulate until the tail lands.
                if let Some(part) = text.strip_prefix("\u{1e}") {
                    pending.push_str(part);
                } else {
                    let whole = if pending.is_empty() {
                        text.to_string()
                    } else {
                        pending.push_str(&text);
                        std::mem::take(&mut pending)
                    };
                    if let Err(e) = handle_frame(
                        &whole,
                        &mut socket,
                        &events,
                        &mut subscriptions,
                        &mut queries,
                    ) {
                        eprintln!("[frame] dropped: {e}");
                    }
                }
            }
            Ok(tungstenite::Message::Ping(payload)) => {
                let _ = socket.send(tungstenite::Message::Pong(payload));
            }
            Ok(tungstenite::Message::Close(_)) => {
                return Err(fail("socket closed by server".into()));
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
            Err(tungstenite::Error::Io(e)) => {
                return Err(fail(format!("socket io error: {e}")));
            }
            Err(e) => return Err(fail(format!("socket error: {e}"))),
        }
    }
}

#[derive(Debug)]
enum SubKind {
    Shell,
    Thread,
    OlderSettled,
    AuthAccess,
    /// Client-driven RPC stream; the string is the caller's `stream_id`.
    Dynamic(String),
}

fn send_command(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    subscriptions: &mut std::collections::HashMap<u64, SubKind>,
    id: u64,
    command: &ClientCommand,
) -> Result<(), tungstenite::Error> {
    match command {
        ClientCommand::SubscribeShell => {
            subscriptions.insert(id, SubKind::Shell);
        }
        ClientCommand::SubscribeAuthAccess => {
            subscriptions.insert(id, SubKind::AuthAccess);
        }
        ClientCommand::GetOlderSettled => {
            subscriptions.insert(id, SubKind::OlderSettled);
        }
        ClientCommand::SubscribeThread { .. } => {
            // One live thread stream at a time: interrupt the previous one.
            let stale: Vec<u64> = subscriptions
                .iter()
                .filter(|(_, kind)| matches!(kind, SubKind::Thread))
                .map(|(rid, _)| *rid)
                .collect();
            for rid in stale {
                subscriptions.remove(&rid);
                let _ = socket.send(tungstenite::Message::Text(
                    json!({"_tag": "Interrupt", "requestId": rid, "interruptors": []})
                        .to_string()
                        .into(),
                ));
            }
            subscriptions.insert(id, SubKind::Thread);
        }
        ClientCommand::Subscribe { stream_id, .. } => {
            subscriptions.insert(id, SubKind::Dynamic(stream_id.clone()));
        }
        ClientCommand::InterruptStream { stream_id } => {
            let rid = subscriptions.iter().find_map(|(rid, kind)| {
                matches!(kind, SubKind::Dynamic(s) if s == stream_id).then_some(*rid)
            });
            if let Some(rid) = rid {
                subscriptions.remove(&rid);
                return socket.send(tungstenite::Message::Text(
                    json!({"_tag": "Interrupt", "requestId": rid, "interruptors": []})
                        .to_string()
                        .into(),
                ));
            }
            return Ok(());
        }
        _ => {}
    }
    let frame = match command {
        ClientCommand::SubscribeShell => json!({
            "_tag": "Request",
            "id": id,
            "tag": "orchestration.subscribeShell",
            "payload": { "requestCompletionMarker": true },
            "headers": [],
        }),
        ClientCommand::GetOlderSettled => json!({
            "_tag": "Request",
            "id": id,
            "tag": "orchestration.getOlderSettledThreads",
            "payload": {
                "before": { "settledAt": "2999-01-01T00:00:00.000Z" },
                "limit": 50,
            },
            "headers": [],
        }),
        ClientCommand::SubscribeAuthAccess => json!({
            "_tag": "Request",
            "id": id,
            "tag": "subscribeAuthAccess",
            "payload": {},
            "headers": [],
        }),
        ClientCommand::SubscribeThread { thread_id } => json!({
            "_tag": "Request",
            "id": id,
            "tag": "orchestration.subscribeThread",
            "payload": { "threadId": thread_id, "requestCompletionMarker": true },
            "headers": [],
        }),
        ClientCommand::Dispatch { command } => json!({
            "_tag": "Request",
            "id": id,
            "tag": "orchestration.dispatchCommand",
            "payload": command,
            "headers": [],
        }),
        ClientCommand::Query { tag, payload, .. } => json!({
            "_tag": "Request",
            "id": id,
            "tag": tag,
            "payload": payload,
            "headers": [],
        }),
        ClientCommand::Subscribe { tag, payload, .. } => json!({
            "_tag": "Request",
            "id": id,
            "tag": tag,
            "payload": payload,
            "headers": [],
        }),
        // Handled in the run loop — they never produce a frame.
        ClientCommand::Reconnect | ClientCommand::SwitchEndpoint { .. } => {
            unreachable!("control commands are handled in the run loop")
        }
        // InterruptStream emits its frame in the subscription match above.
        ClientCommand::InterruptStream { .. } => unreachable!(),
        ClientCommand::Interrupt { request_id } => json!({
            "_tag": "Interrupt",
            "requestId": request_id,
        }),
    };
    if std::env::var("C67_DEBUG").is_ok() {
        eprintln!("[send] id={} tag={:?}", id, frame["tag"].as_str());
    }
    socket.send(tungstenite::Message::Text(frame.to_string().into()))
}

fn handle_frame(
    text: &str,
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    events: &Sender<ShellEvent>,
    subscriptions: &mut std::collections::HashMap<u64, SubKind>,
    queries: &mut std::collections::HashMap<u64, smol::channel::Sender<Value>>,
) -> Result<(), String> {
    let frames: Vec<Value> = serde_json::from_str::<Value>(text)
        .map(|v| if v.is_array() { vec![v] } else { vec![v] })
        .and_then(|v| {
            // Frames may arrive as a single object or an array of them.
            if let Value::Array(items) = &v[0] {
                Ok(items.clone())
            } else {
                Ok(v)
            }
        })
        .map_err(|e| format!("frame decode failed: {e}"))?;

    for frame in frames {
        if std::env::var("C67_DEBUG").is_ok() {
            eprintln!(
                "[frame] tag={:?} req={:?}",
                frame["_tag"].as_str(),
                frame["requestId"]
            );
        }
        match frame["_tag"].as_str() {
            Some("Chunk") => {
                let request_id = frame["requestId"].as_u64().unwrap_or(0);
                // Stream chunks are windowed (8 per request): ack each one so the
                // server keeps writing.
                let _ = socket.send(tungstenite::Message::Text(
                    json!({"_tag": "Ack", "requestId": request_id})
                        .to_string()
                        .into(),
                ));
                let is_thread = matches!(subscriptions.get(&request_id), Some(SubKind::Thread));
                let is_auth_access =
                    matches!(subscriptions.get(&request_id), Some(SubKind::AuthAccess));
                let dynamic_stream = match subscriptions.get(&request_id) {
                    Some(SubKind::Dynamic(s)) => Some(s.clone()),
                    _ => None,
                };
                if std::env::var("C67_DEBUG").is_ok() {
                    eprintln!(
                        "[chunk] req={request_id} is_thread={is_thread} kinds={:?}",
                        frame["values"].as_array().map(|a| a
                            .iter()
                            .map(|v| v["kind"].as_str().unwrap_or("?").to_string())
                            .collect::<Vec<_>>())
                    );
                }
                if let Some(values) = frame["values"].as_array() {
                    if let Some(sid) = dynamic_stream {
                        let _ = events.send_blocking(ShellEvent::StreamChunk {
                            stream_id: sid,
                            values: values.clone(),
                        });
                        continue;
                    }
                    for value in values {
                        if is_auth_access {
                            let _ = events.send_blocking(ShellEvent::AuthAccessChange {
                                value: value.clone(),
                            });
                            continue;
                        }
                        let parsed = if is_thread {
                            if std::env::var("C67_DEBUG").is_ok()
                                && value["kind"].as_str() == Some("event")
                            {
                                eprintln!(
                                    "[ev] type={:?} parsed={}",
                                    value["event"]["type"].as_str(),
                                    parse_thread_stream_item(value).is_some()
                                );
                            }
                            parse_thread_stream_item(value)
                        } else {
                            parse_stream_item(value)
                        };
                        if let Some(event) = parsed {
                            let _ = events.send_blocking(event);
                        }
                    }
                }
            }
            Some("Exit") => {
                let request_id = frame["requestId"].as_u64();
                if let Some(reply) = request_id.and_then(|rid| queries.remove(&rid)) {
                    let _ = reply.send_blocking(frame["exit"].clone());
                    continue;
                }
                let kind = request_id.and_then(|rid| subscriptions.remove(&rid));
                if let Some(SubKind::Dynamic(sid)) = &kind {
                    let _ = events.send_blocking(ShellEvent::StreamEnd {
                        stream_id: sid.clone(),
                    });
                    continue;
                }
                if frame["exit"]["_tag"].as_str() == Some("Success")
                    && matches!(kind, Some(SubKind::OlderSettled))
                {
                    let threads = frame["exit"]["value"]["threads"]
                        .as_array()
                        .map(|a| a.iter().map(parse_thread).collect())
                        .unwrap_or_default();
                    let _ = events.send_blocking(ShellEvent::OlderSettledThreads { threads });
                }
                // Interrupts are expected when we swap thread subscriptions.
                let is_interrupt = frame["exit"]["cause"]["_tag"].as_str() == Some("Interrupt");
                if frame["exit"]["_tag"].as_str() != Some("Success") && !is_interrupt {
                    let _ = events.send_blocking(ShellEvent::Error(format!(
                        "request failed: {}",
                        frame["exit"]
                    )));
                }
            }
            Some("Ping") => {
                let _ = socket.send(tungstenite::Message::Text(
                    json!({"_tag": "Pong"}).to_string().into(),
                ));
            }
            Some("Defect") => {
                let _ = events.send_blocking(ShellEvent::Error(format!(
                    "server defect: {}",
                    frame["defect"]
                )));
            }
            _ => {}
        }
    }
    Ok(())
}
