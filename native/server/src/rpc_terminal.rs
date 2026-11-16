//! `terminal.*` WS RPCs: persistent per-thread PTY sessions via `portable-pty`.
//!
//! Wire shapes mirror `packages/contracts/src/terminal.ts`:
//! - `terminal.open` / `write` / `resize` / `clear` / `close` are one-shot.
//! - `terminal.attach` / `terminal.observe` are Effect-RPC streams of
//!   `TerminalAttachStreamEvent` values (`snapshot` then live `output`,
//!   `exited`, `closed`, `cleared`, `restarted`, `activity` events). They are
//!   windowed tags (see `is_windowed_tag` in handlers.rs) so they arrive here
//!   through `handle_subscription`. `handle` also answers them with a one-shot
//!   `{type:"snapshot", ...}` payload as a degraded fallback for any client
//!   that issues them as unary requests.
//! - `subscribeTerminalMetadata` streams `snapshot`/`upsert`/`remove`
//!   `TerminalMetadataStreamEvent` values, diffed on a poll interval.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, PtySize};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::domain::now_iso;
use crate::handlers::RpcResult;
use crate::ws_rpc::{exit_failure, send_json, OutTx, Window};
use crate::AppState;

const MAX_HISTORY_BYTES: usize = 512 * 1024;
const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const EVENT_CAPACITY: usize = 256;
const METADATA_POLL: Duration = Duration::from_millis(2_000);
const CANCEL_CHECK: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// Session registry
// ---------------------------------------------------------------------------

struct SessionInner {
    thread_id: String,
    terminal_id: String,
    cwd: String,
    worktree_path: Option<String>,
    /// "starting" | "running" | "exited" | "error"
    status: &'static str,
    /// `terminal.close` ran; the session is gone from the registry. Kept
    /// separate from `status` because "closed" is not a TerminalSessionStatus.
    closed: bool,
    pid: Option<u32>,
    exit_code: Option<i64>,
    exit_signal: Option<i64>,
    sequence: u64,
    cols: u16,
    rows: u16,
    history: VecDeque<u8>,
    /// Bumped only by lifecycle events (not output) — drives metadata diffs.
    updated_at: String,
    writer: Option<Box<dyn Write + Send>>,
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    killer: Option<Box<dyn ChildKiller + Send + Sync>>,
}

struct Session {
    inner: Arc<Mutex<SessionInner>>,
    /// Per-session fanout of `TerminalAttachStreamEvent`-shaped JSON values.
    events: broadcast::Sender<Value>,
}

struct Registry {
    sessions: HashMap<(String, String), Session>,
}

fn registry() -> &'static Mutex<Registry> {
    static REG: OnceLock<Mutex<Registry>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(Registry { sessions: HashMap::new() }))
}

fn lock_registry() -> MutexGuard<'static, Registry> {
    registry().lock().unwrap_or_else(|e| e.into_inner())
}

fn get_session(thread_id: &str, terminal_id: &str) -> Option<Session> {
    lock_registry()
        .sessions
        .get(&(thread_id.to_string(), terminal_id.to_string()))
        .cloned()
}

fn lock_inner(session: &Session) -> MutexGuard<'_, SessionInner> {
    session.inner.lock().unwrap_or_else(|e| e.into_inner())
}

impl Clone for Session {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            events: self.events.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

/// `getTerminalLabel` from `@t3tools/shared/terminalLabels`: `term-N` /
/// `terminal-N` (optionally `-uuid` suffixed) → `Terminal N`, else the raw id.
fn label_for(terminal_id: &str) -> String {
    let lower = terminal_id.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("terminal-")
        .or_else(|| lower.strip_prefix("term-"));
    if let Some(rest) = rest {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let tail = &rest[digits.len()..];
            // Optional `-8hex-4hex-4hex-4hex-12hex` unique suffix.
            let ok = tail.is_empty()
                || (tail.len() == 37
                    && tail.starts_with('-')
                    && tail[1..]
                        .chars()
                        .all(|c| c.is_ascii_hexdigit() || c == '-'));
            if ok {
                return format!("Terminal {digits}");
            }
        }
    }
    terminal_id.to_string()
}

fn snapshot_json(inner: &SessionInner) -> Value {
    json!({
        "threadId": inner.thread_id,
        "terminalId": inner.terminal_id,
        "cwd": inner.cwd,
        "worktreePath": inner.worktree_path,
        "status": inner.status,
        "pid": inner.pid,
        "history": String::from_utf8_lossy(&inner.history.iter().copied().collect::<Vec<u8>>()),
        "exitCode": inner.exit_code,
        "exitSignal": inner.exit_signal,
        "label": label_for(&inner.terminal_id),
        "updatedAt": inner.updated_at,
        "sequence": inner.sequence,
        "cols": inner.cols,
        "rows": inner.rows,
    })
}

fn summary_json(inner: &SessionInner) -> Value {
    json!({
        "threadId": inner.thread_id,
        "terminalId": inner.terminal_id,
        "cwd": inner.cwd,
        "worktreePath": inner.worktree_path,
        "status": inner.status,
        "pid": inner.pid,
        "exitCode": inner.exit_code,
        "exitSignal": inner.exit_signal,
        "hasRunningSubprocess": inner.status == "running",
        "label": label_for(&inner.terminal_id),
        "updatedAt": inner.updated_at,
    })
}

fn lookup_error(thread_id: &str, terminal_id: &str) -> Value {
    json!({
        "_tag": "TerminalSessionLookupError",
        "threadId": thread_id,
        "terminalId": terminal_id,
    })
}

fn not_running_error(thread_id: &str, terminal_id: &str) -> Value {
    json!({
        "_tag": "TerminalNotRunningError",
        "threadId": thread_id,
        "terminalId": terminal_id,
    })
}

fn cwd_error(cwd: &str) -> Value {
    match std::fs::metadata(cwd) {
        Ok(m) if m.is_dir() => json!({
            "_tag": "TerminalCwdStatError",
            "cwd": cwd,
            "cause": "not a directory",
        }),
        Ok(_) => json!({ "_tag": "TerminalCwdNotDirectoryError", "cwd": cwd }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            json!({ "_tag": "TerminalCwdNotFoundError", "cwd": cwd })
        }
        Err(e) => json!({
            "_tag": "TerminalCwdStatError",
            "cwd": cwd,
            "cause": e.to_string(),
        }),
    }
}

fn rpc_client_error(message: impl Into<String>) -> Value {
    json!({ "_tag": "RpcClientError", "message": message.into() })
}

// ---------------------------------------------------------------------------
// Event fanout
// ---------------------------------------------------------------------------

/// Stamp sequence/updated_at and broadcast one attach-stream event value.
fn emit(session: &Session, mut event: Value) {
    let mut inner = lock_inner(session);
    inner.sequence += 1;
    let seq = inner.sequence;
    // Lifecycle events bump updatedAt; output does not (keeps metadata diffs quiet).
    let ty = event.get("type").and_then(Value::as_str).unwrap_or("");
    if !matches!(ty, "output" | "cleared") {
        inner.updated_at = now_iso();
    }
    if let Some(obj) = event.as_object_mut() {
        obj.insert("sequence".into(), json!(seq));
    }
    drop(inner);
    let _ = session.events.send(event);
}

fn push_history(inner: &mut SessionInner, bytes: &[u8]) {
    if bytes.len() >= MAX_HISTORY_BYTES {
        inner.history.clear();
        inner.history.extend(
            bytes[bytes.len() - MAX_HISTORY_BYTES..].iter().copied(),
        );
        return;
    }
    while inner.history.len() + bytes.len() > MAX_HISTORY_BYTES {
        inner.history.pop_front();
    }
    inner.history.extend(bytes.iter().copied());
}

// ---------------------------------------------------------------------------
// PTY spawn
// ---------------------------------------------------------------------------

fn spawn_pty(
    cwd: &str,
    cols: u16,
    rows: u16,
    extra_env: &[(String, String)],
) -> Result<
    (
        Box<dyn Read + Send>,
        Box<dyn Write + Send>,
        Box<dyn portable_pty::MasterPty + Send>,
        Box<dyn Child + Send + Sync>,
    ),
    String,
> {
    let ptys = native_pty_system();
    let pair = ptys
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("openpty failed: {e}"))?;

    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/bash".to_string());
    let mut cmd = CommandBuilder::new(&shell);
    // Login shells match the TS server: zsh everywhere, bash on macOS only
    // (Linux `bash -l` reads .bash_profile instead of .bashrc).
    let shell_name = Path::new(&shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if shell_name == "zsh" || (shell_name == "bash" && cfg!(target_os = "macos")) {
        cmd.arg("-l");
    }
    cmd.cwd(cwd);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    if !extra_env.iter().any(|(k, _)| k.eq_ignore_ascii_case("COLORTERM")) {
        cmd.env("COLORTERM", "truecolor");
    }
    if !extra_env.iter().any(|(k, _)| k == "TERM") {
        cmd.env("TERM", "xterm-256color");
    }

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("spawn {shell} failed: {e}"))?;
    // Dropping the slave lets the master observe EOF once the child exits.
    drop(pair.slave);

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("clone reader failed: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("take writer failed: {e}"))?;
    Ok((reader, writer, pair.master, child))
}

/// Wire reader + exit-waiter threads for a session whose handles are already
/// stored in `inner`.
fn attach_io(session: &Session, mut reader: Box<dyn Read + Send>) {
    let out_session = session.clone();
    std::thread::Builder::new()
        .name(format!("pty-read-{}", lock_inner(session).terminal_id))
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                        {
                            let mut inner = lock_inner(&out_session);
                            push_history(&mut inner, &buf[..n]);
                        }
                        let (thread_id, terminal_id) = {
                            let inner = lock_inner(&out_session);
                            (inner.thread_id.clone(), inner.terminal_id.clone())
                        };
                        emit(
                            &out_session,
                            json!({
                                "type": "output",
                                "threadId": thread_id,
                                "terminalId": terminal_id,
                                "data": chunk,
                            }),
                        );
                    }
                    Err(_) => break,
                }
            }
        })
        .ok();
}

/// Spawn the `child.wait()` blocker; publishes the `exited` event.
fn watch_child(session: &Session, mut child: Box<dyn Child + Send + Sync>) {
    let s = session.clone();
    tokio::task::spawn_blocking(move || {
        let status = child.wait().ok();
        let mut inner = lock_inner(&s);
        if inner.closed {
            return;
        }
        inner.status = "exited";
        inner.exit_code = status.as_ref().map(|st| st.exit_code() as i64);
        // portable-pty reports the signal name, not its number; NullOr<Int>
        // on the wire, so we publish the code only when no signal occurred.
        inner.exit_signal = None;
        inner.pid = None;
        let ev = json!({
            "type": "exited",
            "threadId": inner.thread_id,
            "terminalId": inner.terminal_id,
            "exitCode": inner.exit_code,
            "exitSignal": inner.exit_signal,
        });
        drop(inner);
        emit(&s, ev);
    });
}

// ---------------------------------------------------------------------------
// cwd resolution & session lifecycle
// ---------------------------------------------------------------------------

/// thread → project workspace_root, read-only against the server's own DB.
fn workspace_root_for_thread(state: &AppState, thread_id: &str) -> Option<String> {
    let db = state.home_dir.join("userdata/statev2.sqlite");
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    conn.query_row(
        "SELECT p.workspace_root \
         FROM orchestration_v2_projection_threads t \
         JOIN projection_projects p ON p.project_id = t.project_id \
         WHERE t.thread_id = ?1 AND t.deleted_at IS NULL AND p.deleted_at IS NULL",
        rusqlite::params![thread_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// Payload `cwd` wins; then the thread's project workspace_root; then the
/// server's own project dir. Validated cwd errors use the TerminalError union.
fn resolve_cwd(state: &AppState, thread_id: &str, payload: &Value) -> Result<String, Value> {
    if let Some(cwd) = payload.get("cwd").and_then(Value::as_str) {
        let cwd = cwd.trim();
        if !cwd.is_empty() {
            let meta = std::fs::metadata(cwd);
            return match meta {
                Ok(m) if m.is_dir() => Ok(cwd.to_string()),
                _ => Err(cwd_error(cwd)),
            };
        }
    }
    if let Some(root) = workspace_root_for_thread(state, thread_id) {
        if Path::new(&root).is_dir() {
            return Ok(root);
        }
    }
    Ok(state.project_dir.to_string_lossy().to_string())
}

struct OpenSpec {
    thread_id: String,
    terminal_id: String,
    cwd: String,
    worktree_path: Option<String>,
    /// Requested grid; absent leaves an existing session's size alone.
    cols: Option<u16>,
    rows: Option<u16>,
    env: Vec<(String, String)>,
}

fn open_spec_from_payload(
    state: &AppState,
    payload: &Value,
) -> Result<OpenSpec, Value> {
    let thread_id = payload
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let terminal_id = payload
        .get("terminalId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if thread_id.is_empty() || terminal_id.is_empty() {
        return Err(rpc_client_error("terminal request requires threadId and terminalId"));
    }
    let cols = payload
        .get("cols")
        .and_then(Value::as_u64)
        .map(|c| c.clamp(1, 1000) as u16);
    let rows = payload
        .get("rows")
        .and_then(Value::as_u64)
        .map(|r| r.clamp(1, 500) as u16);
    let env = payload
        .get("env")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let worktree_path = payload
        .get("worktreePath")
        .and_then(Value::as_str)
        .map(str::to_string);
    let cwd = resolve_cwd(state, &thread_id, payload)?;
    Ok(OpenSpec {
        thread_id,
        terminal_id,
        cwd,
        worktree_path,
        cols,
        rows,
        env,
    })
}

/// Resize a running session's PTY when the requested grid differs.
fn maybe_resize(session: &Session, cols: u16, rows: u16) {
    let mut inner = lock_inner(session);
    if inner.status != "running" || (inner.cols == cols && inner.rows == rows) {
        return;
    }
    if let Some(master) = &inner.master {
        let _ = master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
    inner.cols = cols;
    inner.rows = rows;
}

/// Get the live session for `spec` or spawn a new PTY for it.
fn open_session(spec: OpenSpec) -> Result<Session, Value> {
    let key = (spec.thread_id.clone(), spec.terminal_id.clone());
    if let Some(existing) = lock_registry().sessions.get(&key).cloned() {
        // A re-open on a running session resizes to the requested grid.
        if let (Some(c), Some(r)) = (spec.cols, spec.rows) {
            maybe_resize(&existing, c, r);
        }
        return Ok(existing);
    }

    let cols = spec.cols.unwrap_or(DEFAULT_COLS);
    let rows = spec.rows.unwrap_or(DEFAULT_ROWS);
    let (reader, writer, master, child) = spawn_pty(&spec.cwd, cols, rows, &spec.env)
        .map_err(rpc_client_error)?;
    let pid = child.process_id();
    let killer = child.clone_killer();

    let session = Session {
        inner: Arc::new(Mutex::new(SessionInner {
            thread_id: spec.thread_id,
            terminal_id: spec.terminal_id,
            cwd: spec.cwd,
            worktree_path: spec.worktree_path,
            status: "running",
            closed: false,
            pid,
            exit_code: None,
            exit_signal: None,
            sequence: 0,
            cols,
            rows,
            history: VecDeque::new(),
            updated_at: now_iso(),
            writer: Some(writer),
            master: Some(master),
            killer: Some(killer),
        })),
        events: broadcast::channel(EVENT_CAPACITY).0,
    };

    attach_io(&session, reader);
    watch_child(&session, child);

    lock_registry().sessions.insert(key, session.clone());

    let snap = {
        let inner = lock_inner(&session);
        snapshot_json(&inner)
    };
    emit(
        &session,
        json!({ "type": "started", "snapshot": snap }),
    );
    Ok(session)
}

// ---------------------------------------------------------------------------
// One-shot request handlers
// ---------------------------------------------------------------------------

pub async fn handle(state: Arc<AppState>, tag: &str, payload: Value) -> RpcResult {
    match tag {
        "terminal.open" => match open_spec_from_payload(&state, &payload)
            .and_then(open_session)
        {
            Ok(session) => {
                let inner = lock_inner(&session);
                RpcResult::ok(snapshot_json(&inner))
            }
            Err(e) => RpcResult::err(e),
        },
        "terminal.attach" | "terminal.observe" => {
            // Degraded unary fallback; the streamed path is handle_subscription.
            attach_fallback(&state, tag, &payload)
        }
        "terminal.write" => {
            let thread_id = payload.get("threadId").and_then(Value::as_str).unwrap_or("");
            let terminal_id = payload.get("terminalId").and_then(Value::as_str).unwrap_or("");
            let Some(session) = get_session(thread_id, terminal_id) else {
                return RpcResult::err(lookup_error(thread_id, terminal_id));
            };
            let data = match write_bytes(&payload) {
                Ok(d) => d,
                Err(e) => return RpcResult::err(e),
            };
            let mut inner = lock_inner(&session);
            if inner.status != "running" {
                return RpcResult::err(not_running_error(thread_id, terminal_id));
            }
            let pid = inner.pid.unwrap_or(0);
            match inner
                .writer
                .as_mut()
                .map(|w| w.write_all(&data).and_then(|_| w.flush()))
            {
                Some(Ok(())) => RpcResult::ok(Value::Null),
                Some(Err(e)) => RpcResult::err(json!({
                    "_tag": "TerminalWriteError",
                    "threadId": thread_id,
                    "terminalId": terminal_id,
                    "terminalPid": pid,
                    "cause": e.to_string(),
                })),
                None => RpcResult::err(not_running_error(thread_id, terminal_id)),
            }
        }
        "terminal.resize" => {
            let thread_id = payload.get("threadId").and_then(Value::as_str).unwrap_or("");
            let terminal_id = payload.get("terminalId").and_then(Value::as_str).unwrap_or("");
            let cols = payload.get("cols").and_then(Value::as_u64).unwrap_or(0);
            let rows = payload.get("rows").and_then(Value::as_u64).unwrap_or(0);
            if cols == 0 || rows == 0 {
                return RpcResult::err(rpc_client_error("terminal.resize requires cols and rows"));
            }
            let cols = cols.clamp(1, 1000) as u16;
            let rows = rows.clamp(1, 500) as u16;
            let Some(session) = get_session(thread_id, terminal_id) else {
                return RpcResult::err(lookup_error(thread_id, terminal_id));
            };
            {
                let mut inner = lock_inner(&session);
                let resize = inner.master.as_ref().map(|m| {
                    m.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                });
                match resize {
                    Some(Ok(())) => {
                        inner.cols = cols;
                        inner.rows = rows;
                    }
                    Some(Err(e)) => {
                        return RpcResult::err(json!({
                            "_tag": "TerminalResizeError",
                            "threadId": thread_id,
                            "terminalId": terminal_id,
                            "terminalPid": inner.pid.unwrap_or(0),
                            "cols": cols,
                            "rows": rows,
                            "cause": e.to_string(),
                        }))
                    }
                    None => {
                        return RpcResult::err(not_running_error(thread_id, terminal_id));
                    }
                }
            }
            // Grid changes ride on `activity` per the contract comment.
            let ev = {
                let inner = lock_inner(&session);
                json!({
                    "type": "activity",
                    "threadId": inner.thread_id,
                    "terminalId": inner.terminal_id,
                    "hasRunningSubprocess": inner.status == "running",
                    "label": label_for(&inner.terminal_id),
                    "cols": inner.cols,
                    "rows": inner.rows,
                })
            };
            emit(&session, ev);
            RpcResult::ok(Value::Null)
        }
        "terminal.clear" => {
            let thread_id = payload.get("threadId").and_then(Value::as_str).unwrap_or("");
            let terminal_id = payload.get("terminalId").and_then(Value::as_str).unwrap_or("");
            let Some(session) = get_session(thread_id, terminal_id) else {
                return RpcResult::err(lookup_error(thread_id, terminal_id));
            };
            {
                let mut inner = lock_inner(&session);
                inner.history.clear();
            }
            emit(
                &session,
                json!({
                    "type": "cleared",
                    "threadId": thread_id,
                    "terminalId": terminal_id,
                }),
            );
            RpcResult::ok(Value::Null)
        }
        "terminal.close" => {
            let thread_id = payload.get("threadId").and_then(Value::as_str).unwrap_or("");
            let terminal_id = payload.get("terminalId").and_then(Value::as_str);
            let targets: Vec<(String, String)> = {
                let reg = lock_registry();
                match terminal_id {
                    Some(tid) if !tid.is_empty() => {
                        let key = (thread_id.to_string(), tid.to_string());
                        if reg.sessions.contains_key(&key) {
                            vec![key]
                        } else {
                            return RpcResult::err(lookup_error(thread_id, tid));
                        }
                    }
                    _ => reg
                        .sessions
                        .keys()
                        .filter(|(t, _)| t == thread_id)
                        .cloned()
                        .collect(),
                }
            };
            for key in targets {
                close_session(&key.0, &key.1);
            }
            RpcResult::ok(Value::Null)
        }
        other if other.starts_with("worktreeSetup.") => RpcResult::err(rpc_client_error(
            format!("worktree setup is not supported by this server: {other}"),
        )),
        other => RpcResult::err(rpc_client_error(format!("not implemented: {other}"))),
    }
}

fn write_bytes(payload: &Value) -> Result<Vec<u8>, Value> {
    let data = payload
        .get("data")
        .and_then(Value::as_str)
        .unwrap_or("");
    if data.is_empty() {
        return Err(rpc_client_error("terminal.write requires non-empty data"));
    }
    let encoding = payload
        .get("encoding")
        .and_then(Value::as_str)
        .unwrap_or("");
    if encoding.eq_ignore_ascii_case("base64") {
        base64_decode(data).map_err(|_| rpc_client_error("terminal.write: invalid base64 data"))
    } else {
        Ok(data.as_bytes().to_vec())
    }
}

fn base64_decode(s: &str) -> Result<Vec<u8>, ()> {
    let mut table = [255u8; 256];
    for (i, c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
        .iter()
        .enumerate()
    {
        table[*c as usize] = i as u8;
    }
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if bytes.len() % 4 != 0 {
        return Err(());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().filter(|&&b| b == b'=').count();
        if pad > 2 {
            return Err(());
        }
        let mut vals = [0u8; 4];
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                vals[i] = 0;
            } else {
                vals[i] = table[b as usize];
                if vals[i] == 255 {
                    return Err(());
                }
            }
        }
        let n = ((vals[0] as u32) << 18)
            | ((vals[1] as u32) << 12)
            | ((vals[2] as u32) << 6)
            | vals[3] as u32;
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

/// Kill the child, drop the handles, broadcast `closed`, remove from registry.
fn close_session(thread_id: &str, terminal_id: &str) {
    let key = (thread_id.to_string(), terminal_id.to_string());
    let session = lock_registry().sessions.remove(&key);
    let Some(session) = session else { return };
    {
        let mut inner = lock_inner(&session);
        // Flag before killing so `watch_child`'s `exited` emit loses the race.
        inner.closed = true;
        inner.status = "exited";
        inner.pid = None;
        if let Some(killer) = inner.killer.as_mut() {
            let _ = killer.kill();
        }
        inner.killer = None;
        inner.writer = None;
        inner.master = None;
    }
    emit(
        &session,
        json!({
            "type": "closed",
            "threadId": thread_id,
            "terminalId": terminal_id,
        }),
    );
}

/// One-shot answer for `terminal.attach`/`terminal.observe` when the request
/// did not arrive through the streamed path.
fn attach_fallback(state: &Arc<AppState>, tag: &str, payload: &Value) -> RpcResult {
    let thread_id = payload.get("threadId").and_then(Value::as_str).unwrap_or("");
    let terminal_id = payload.get("terminalId").and_then(Value::as_str).unwrap_or("");
    let session = if tag == "terminal.observe" {
        match get_session(thread_id, terminal_id) {
            Some(s) => s,
            None => return RpcResult::err(lookup_error(thread_id, terminal_id)),
        }
    } else {
        match open_spec_from_payload(state, payload).and_then(open_session) {
            Ok(s) => s,
            Err(e) => return RpcResult::err(e),
        }
    };
    let inner = lock_inner(&session);
    RpcResult::ok(json!({ "type": "snapshot", "snapshot": snapshot_json(&inner) }))
}

// ---------------------------------------------------------------------------
// Subscription streams
// ---------------------------------------------------------------------------

pub async fn handle_subscription(
    state: Arc<AppState>,
    out: OutTx,
    request_id: u64,
    tag: &str,
    payload: Value,
    window: Arc<Window>,
) -> bool {
    match tag {
        "terminal.attach" => {
            attach_stream(state, out, request_id, payload, window, false).await;
            true
        }
        "terminal.observe" => {
            attach_stream(state, out, request_id, payload, window, true).await;
            true
        }
        "subscribeTerminalMetadata" => {
            metadata_stream(out, request_id, window).await;
            true
        }
        "subscribeWorktreeSetup" => {
            send_json(
                &out,
                &exit_failure(
                    request_id,
                    rpc_client_error("worktree setup is not supported by this server"),
                ),
            );
            true
        }
        _ => false,
    }
}

/// Stream end for a `stream: true` RPC: `Schema.Exit(Schema.Void, ...)`
/// encodes success without a `value` key.
fn stream_exit_success(request_id: u64) -> Value {
    json!({ "_tag": "Exit", "requestId": request_id, "exit": { "_tag": "Success" } })
}

/// Kill the dead session under `spec`'s key and spawn a fresh PTY; reports the
/// new snapshot on the `restarted` event for other attachers.
fn restart_session(spec: OpenSpec) -> Result<Session, Value> {
    close_session(&spec.thread_id, &spec.terminal_id);
    let session = open_session(spec)?;
    let snap = {
        let inner = lock_inner(&session);
        snapshot_json(&inner)
    };
    emit(
        &session,
        json!({ "type": "restarted", "threadId": snap["threadId"], "terminalId": snap["terminalId"], "snapshot": snap }),
    );
    Ok(session)
}

/// Shared loop for `terminal.attach` (open-or-attach) and `terminal.observe`
/// (existing only): snapshot chunk, then live events until close/cancel.
async fn attach_stream(
    state: Arc<AppState>,
    out: OutTx,
    request_id: u64,
    payload: Value,
    window: Arc<Window>,
    observe_only: bool,
) {
    let thread_id = payload.get("threadId").and_then(Value::as_str).unwrap_or("");
    let terminal_id = payload.get("terminalId").and_then(Value::as_str).unwrap_or("");

    let session = if observe_only {
        match get_session(thread_id, terminal_id) {
            Some(s) => s,
            None => {
                send_json(&out, &exit_failure(request_id, lookup_error(thread_id, terminal_id)));
                return;
            }
        }
    } else {
        let restart = payload
            .get("restartIfNotRunning")
            .and_then(Value::as_bool)
            == Some(true);
        let existing = get_session(thread_id, terminal_id);
        // cwd is only validated when a spawn is actually needed, matching the
        // TS openOrAttachForStream path (existing sessions skip launch input).
        let spec = |state: &Arc<AppState>| open_spec_from_payload(state, &payload);
        match existing {
            Some(s) => {
                let running = lock_inner(&s).status == "running";
                if !running && restart {
                    match spec(&state).and_then(restart_session) {
                        Ok(s) => s,
                        Err(e) => {
                            send_json(&out, &exit_failure(request_id, e));
                            return;
                        }
                    }
                } else {
                    if let (Some(c), Some(r)) = (
                        payload
                            .get("cols")
                            .and_then(Value::as_u64)
                            .map(|c| c.clamp(1, 1000) as u16),
                        payload
                            .get("rows")
                            .and_then(Value::as_u64)
                            .map(|r| r.clamp(1, 500) as u16),
                    ) {
                        maybe_resize(&s, c, r);
                    }
                    s
                }
            }
            None => match spec(&state).and_then(open_session) {
                Ok(s) => s,
                Err(e) => {
                    send_json(&out, &exit_failure(request_id, e));
                    return;
                }
            },
        }
    };

    // Subscribe before snapshotting so live events cannot be missed.
    let mut rx = session.events.subscribe();
    let snapshot = {
        let inner = lock_inner(&session);
        snapshot_json(&inner)
    };
    if !window
        .send_chunk(&out, request_id, vec![json!({ "type": "snapshot", "snapshot": snapshot })])
        .await
    {
        return;
    }

    loop {
        tokio::select! {
            ev = rx.recv() => {
                match ev {
                    Ok(event) => {
                        let ty = event
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        // `started` maps to a fresh snapshot on the attach wire.
                        let wire = if ty == "started" {
                            let inner = lock_inner(&session);
                            json!({ "type": "snapshot", "snapshot": snapshot_json(&inner) })
                        } else {
                            event
                        };
                        let last = ty == "closed";
                        if !window.send_chunk(&out, request_id, vec![wire]).await {
                            return;
                        }
                        if last {
                            send_json(&out, &stream_exit_success(request_id));
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Overflow recovery: resync with a fresh snapshot.
                        let snap = {
                            let inner = lock_inner(&session);
                            json!({ "type": "snapshot", "snapshot": snapshot_json(&inner) })
                        };
                        if !window.send_chunk(&out, request_id, vec![snap]).await {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        send_json(&out, &stream_exit_success(request_id));
                        return;
                    }
                }
            }
            _ = tokio::time::sleep(CANCEL_CHECK) => {
                if window.is_cancelled() {
                    return;
                }
            }
        }
    }
}

/// `subscribeTerminalMetadata`: initial snapshot, then upsert/remove diffs on
/// a poll interval (registry has no metadata bus by design).
async fn metadata_stream(out: OutTx, request_id: u64, window: Arc<Window>) {
    let summaries = || -> Vec<Value> {
        let reg = lock_registry();
        let mut v: Vec<Value> = reg
            .sessions
            .values()
            .map(|s| {
                let inner = lock_inner(s);
                summary_json(&inner)
            })
            .collect();
        v.sort_by(|a, b| {
            let key = |v: &Value| {
                (
                    v.get("updatedAt").and_then(Value::as_str).unwrap_or("").to_string(),
                    v.get("threadId").and_then(Value::as_str).unwrap_or("").to_string(),
                    v.get("terminalId").and_then(Value::as_str).unwrap_or("").to_string(),
                )
            };
            let (ka, kb) = (key(a), key(b));
            kb.0.cmp(&ka.0).then(ka.1.cmp(&kb.1)).then(ka.2.cmp(&kb.2))
        });
        v
    };

    let initial = summaries();
    if !window
        .send_chunk(&out, request_id, vec![json!({ "type": "snapshot", "terminals": initial })])
        .await
    {
        return;
    }

    let mut last: HashMap<(String, String), Value> = HashMap::new();
    for s in summaries() {
        let key = (
            s.get("threadId").and_then(Value::as_str).unwrap_or("").to_string(),
            s.get("terminalId").and_then(Value::as_str).unwrap_or("").to_string(),
        );
        last.insert(key, s);
    }

    let mut tick = tokio::time::interval(METADATA_POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if window.is_cancelled() {
            return;
        }
        let current = summaries();
        let mut events: Vec<Value> = Vec::new();
        let mut seen: HashMap<(String, String), Value> = HashMap::new();
        for s in current {
            let key = (
                s.get("threadId").and_then(Value::as_str).unwrap_or("").to_string(),
                s.get("terminalId").and_then(Value::as_str).unwrap_or("").to_string(),
            );
            if last.get(&key) != Some(&s) {
                events.push(json!({ "type": "upsert", "terminal": s }));
            }
            seen.insert(key, s);
        }
        for key in last.keys() {
            if !seen.contains_key(key) {
                events.push(json!({
                    "type": "remove",
                    "threadId": key.0,
                    "terminalId": key.1,
                }));
            }
        }
        last = seen;
        if !events.is_empty() && !window.send_chunk(&out, request_id, events).await {
            return;
        }
    }
}
