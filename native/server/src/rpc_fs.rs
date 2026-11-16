//! filesystem.* + projects.* file-access RPCs.
//!
//! Mirrors `apps/server/src/workspace/WorkspaceEntries.ts`,
//! `WorkspaceFileSystem.ts`, `WorkspacePaths.ts` and
//! `project/ManagedProjectFolders.ts` against the wire schemas in
//! `packages/contracts/src/{filesystem,project}.ts`.
use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};

use crate::domain;
use crate::handlers::RpcResult;
use crate::store::CommittedEvent;
use crate::AppState;

const READ_FILE_MAX_BYTES: u64 = 1024 * 1024;
const IGNORED_FILES_LIMIT: usize = 200;
const IGNORE_CHECK_CHUNK_SIZE: usize = 1000;
const LIST_ENTRIES_MAX: usize = 25_000;
const WALK_DEPTH_CAP: usize = 32;
const SEARCH_MAX_MATCHES_PER_FILE: usize = 100;
const SEARCH_FILE_MAX_BYTES: u64 = 8 * 1024 * 1024;
const LINE_CONTENT_CAP: usize = 512;
const MAX_NAMED_FOLDER_ATTEMPTS: u32 = 100;
const CMD_TIMEOUT: Duration = Duration::from_secs(15);

pub async fn handle(state: Arc<AppState>, tag: &str, payload: Value) -> RpcResult {
    match tag {
        "filesystem.browse" => browse(payload).await,
        "filesystem.getMetadata" => get_metadata(payload).await,
        "projects.listEntries" => list_entries(state, payload).await,
        "projects.listIgnoredFiles" => list_ignored_files(state, payload).await,
        "projects.readFile" => read_file(state, payload).await,
        "projects.writeFile" => write_file(state, payload).await,
        "projects.searchContents" => search_contents(state, payload).await,
        "projects.searchEntries" => search_entries(state, payload).await,
        "projects.mutate" => mutate(state, payload).await,
        "projects.ensureScratch" => ensure_scratch(state).await,
        "projects.createNew" => create_new(state, payload).await,
        other => RpcResult::err(
            json!({"_tag":"RpcClientError","message":format!("not implemented: {other}")}),
        ),
    }
}

/// Claim a subscription stream tag; returns true if this module handled it.
pub async fn handle_subscription(
    state: Arc<AppState>,
    out: crate::ws_rpc::OutTx,
    request_id: u64,
    tag: &str,
    payload: Value,
    window: Arc<crate::ws_rpc::Window>,
) -> bool {
    let _ = (state, out, request_id, tag, payload, window);
    false
}

// ---------------------------------------------------------------------------
// Error builders (TaggedError wire shapes from packages/contracts)
// ---------------------------------------------------------------------------

fn err_obj(tag: &str, message: String, fields: Map<String, Value>) -> RpcResult {
    let mut o = fields;
    o.insert("_tag".into(), json!(tag));
    o.insert("message".into(), json!(message));
    RpcResult::err(Value::Object(o))
}

/// `ProjectListEntriesError` / `ProjectSearchEntriesError` /
/// `ProjectSearchContentsError` — `projectEntriesFailureContext` in ws.ts.
fn entries_error(
    tag: &str,
    cwd: &str,
    failure: &str,
    normalized_cwd: Option<&str>,
    detail: Option<String>,
    message: String,
    extras: Map<String, Value>,
) -> RpcResult {
    let mut o = Map::new();
    o.insert("cwd".into(), json!(cwd));
    o.insert("failure".into(), json!(failure));
    if let Some(n) = normalized_cwd {
        o.insert("normalizedCwd".into(), json!(n));
    }
    if let Some(d) = detail {
        o.insert("detail".into(), json!(d));
    }
    for (k, v) in extras {
        o.insert(k, v);
    }
    err_obj(tag, message, o)
}

/// `ProjectReadFileError` / `ProjectWriteFileError` —
/// `projectFileFailureContext` in ws.ts.
fn file_error(
    tag: &str,
    cwd: &str,
    relative_path: &str,
    failure: &str,
    resolved_path: Option<&str>,
    resolved_root: Option<&str>,
    operation: Option<&str>,
    operation_path: Option<&str>,
    message: String,
) -> RpcResult {
    let mut o = Map::new();
    o.insert("cwd".into(), json!(cwd));
    o.insert("relativePath".into(), json!(relative_path));
    o.insert("failure".into(), json!(failure));
    if let Some(p) = resolved_path {
        o.insert("resolvedPath".into(), json!(p));
    }
    if let Some(r) = resolved_root {
        o.insert("resolvedWorkspaceRoot".into(), json!(r));
    }
    if let Some(op) = operation {
        o.insert("operation".into(), json!(op));
    }
    if let Some(op) = operation_path {
        o.insert("operationPath".into(), json!(op));
    }
    err_obj(tag, message, o)
}

fn browse_error(
    partial: &str,
    cwd: Option<&str>,
    failure: &str,
    parent_path: Option<&str>,
    path_missing: bool,
    platform: Option<&str>,
) -> RpcResult {
    let mut o = Map::new();
    o.insert("partialPath".into(), json!(partial));
    if let Some(c) = cwd {
        o.insert("cwd".into(), json!(c));
    }
    o.insert("failure".into(), json!(failure));
    if let Some(p) = parent_path {
        o.insert("parentPath".into(), json!(p));
    }
    if let Some(pl) = platform {
        o.insert("platform".into(), json!(pl));
    }
    if path_missing {
        o.insert("pathMissing".into(), json!(true));
    }
    let cwd_part = cwd.map(|c| format!(" from '{c}'")).unwrap_or_default();
    err_obj(
        "FilesystemBrowseError",
        format!("Failed to browse filesystem path '{partial}'{cwd_part}."),
        o,
    )
}

fn mutation_error(command_id: &str, message: impl Into<String>) -> RpcResult {
    err_obj(
        "ProjectMutationError",
        message.into(),
        Map::from_iter([("commandId".into(), json!(command_id))]),
    )
}

fn dispatch_error(message: impl Into<String>) -> RpcResult {
    err_obj(
        "OrchestrationDispatchCommandError",
        message.into(),
        Map::new(),
    )
}

// ---------------------------------------------------------------------------
// Path helpers (packages/shared/src/path.ts + effect/Path semantics)
// ---------------------------------------------------------------------------

fn home_dir() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/".into())
}

fn expand_home(value: &str) -> String {
    if value.is_empty() {
        return value.to_string();
    }
    if value == "~" {
        return home_dir();
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return format!("{}{}{}", home_dir(), std::path::MAIN_SEPARATOR, rest);
    }
    value.to_string()
}

fn is_unc_path(value: &str) -> bool {
    value.starts_with("\\\\")
}

fn is_network_path(value: &str) -> bool {
    value.starts_with("\\\\") || value.starts_with("//")
}

fn is_windows_drive_path(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'/' || b[2] == b'\\')
}

fn is_windows_abs(value: &str) -> bool {
    is_unc_path(value) || is_windows_drive_path(value)
}

fn is_explicit_relative(value: &str) -> bool {
    value == "."
        || value == ".."
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with(".\\")
        || value.starts_with("..\\")
}

/// Collapse `.`/`..` lexically (no symlink resolution, no existence check).
fn normalize_lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.components().last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Node `path.resolve`: absolute stays (normalized); relative joins process cwd.
fn resolve_lexical(p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        normalize_lexical(path)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        normalize_lexical(&cwd.join(path))
    }
}

fn resolve_lexical_against(base: &str, p: &str) -> PathBuf {
    let joined = Path::new(base).join(p);
    if Path::new(base).is_absolute() {
        normalize_lexical(&joined)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        normalize_lexical(&cwd.join(joined))
    }
}

fn is_within_root(candidate: &Path, root: &Path) -> bool {
    candidate.strip_prefix(root).is_ok()
}

/// `resolveRelativePathWithinRoot`: trimmed, non-absolute, inside `root`
/// (root must already be normalized). Returns (absolute, posix-relative).
fn resolve_within_root(root: &Path, relative_path: &str) -> Option<(PathBuf, String)> {
    let rel = relative_path.trim();
    if rel.is_empty() || Path::new(rel).is_absolute() || is_windows_abs(rel) {
        return None;
    }
    let absolute = normalize_lexical(&root.join(rel));
    let rel_to_root = absolute.strip_prefix(root).ok()?;
    let s = rel_to_root.to_string_lossy().replace('\\', "/");
    if s.is_empty() || s == "." || s == ".." || s.starts_with("../") {
        return None;
    }
    Some((absolute, s))
}

/// `.git` segment detection (NTFS aliases included), per WorkspaceFileSystem.
fn is_git_metadata_name(segment: &str) -> bool {
    let base = segment.split(':').next().unwrap_or("");
    let trimmed = base.trim_end_matches(['.', ' ']);
    let up = trimmed.to_uppercase();
    up == ".GIT" || up == "GIT~1"
}

fn has_git_segment(path_str: &str) -> bool {
    path_str.split(['/', '\\']).any(is_git_metadata_name)
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Workspace root resolution
// ---------------------------------------------------------------------------

enum RootError {
    NotExists,
    StatFailed,
    NotDirectory,
}

/// `normalizeWorkspaceRoot`: expand `~`, resolve, must exist and be a dir.
fn normalize_root(cwd: &str) -> Result<(PathBuf, String), (RootError, String)> {
    let normalized = resolve_lexical(&expand_home(cwd.trim()));
    let ns = path_str(&normalized);
    match std::fs::metadata(&normalized) {
        Err(e) if e.kind() == ErrorKind::NotFound => Err((RootError::NotExists, ns)),
        Err(_) => Err((RootError::StatFailed, ns)),
        Ok(m) if !m.is_dir() => Err((RootError::NotDirectory, ns)),
        Ok(_) => Ok((normalized, ns)),
    }
}

fn root_failure(err: &(RootError, String)) -> (&'static str, &str) {
    match err.0 {
        RootError::NotExists => ("workspace_root_not_found", err.1.as_str()),
        RootError::StatFailed => ("workspace_root_stat_failed", err.1.as_str()),
        RootError::NotDirectory => ("workspace_root_not_directory", err.1.as_str()),
    }
}

/// Resolve the workspace root for a `projects.*` call. The contract field is
/// `cwd` (a host path); `projectId` is also accepted and resolved through the
/// store for callers that only know the project.
async fn workspace_root(state: &AppState, payload: &Value) -> Result<String, RpcResult> {
    if let Some(c) = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(c.to_string());
    }
    if let Some(pid) = payload.get("projectId").and_then(Value::as_str) {
        if let Some(shell) = state.store.project_shell(pid).await {
            if let Some(root) = shell.get("workspaceRoot").and_then(Value::as_str) {
                return Ok(root.to_string());
            }
        }
        return Err(RpcResult::err(
            json!({"_tag":"RpcClientError","message":format!("unknown project: {pid}")}),
        ));
    }
    Err(RpcResult::err(
        json!({"_tag":"RpcClientError","message":"missing cwd"}),
    ))
}

// ---------------------------------------------------------------------------
// Subprocess helper
// ---------------------------------------------------------------------------

struct CmdOut {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn run_cmd(
    program: &str,
    args: &[&str],
    cwd: &Path,
    stdin: Option<Vec<u8>>,
) -> Option<CmdOut> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().ok()?;
    if let Some(input) = stdin {
        if let Some(mut w) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = w.write_all(&input).await;
        }
    }
    match tokio::time::timeout(CMD_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) => Some(CmdOut {
            code: o.status.code().unwrap_or(-1),
            stdout: o.stdout,
            stderr: o.stderr,
        }),
        _ => None,
    }
}

/// `git check-ignore -z --stdin` for chunks of relative paths; `None` when git
/// cannot classify (not a repo, missing binary, timeout).
async fn git_check_ignore(cwd: &Path, paths: &[String]) -> Option<HashSet<String>> {
    let mut ignored = HashSet::new();
    for chunk in paths.chunks(IGNORE_CHECK_CHUNK_SIZE) {
        let mut input = Vec::new();
        for p in chunk {
            input.extend_from_slice(p.as_bytes());
            input.push(0);
        }
        let res = run_cmd(
            "git",
            &[
                "-c",
                "core.fsmonitor=false",
                "check-ignore",
                "-z",
                "--stdin",
            ],
            cwd,
            Some(input),
        )
        .await?;
        if res.code != 0 && res.code != 1 {
            return if ignored.is_empty() {
                None
            } else {
                Some(ignored)
            };
        }
        for p in res.stdout.split(|&b| b == 0) {
            if !p.is_empty() {
                ignored.insert(String::from_utf8_lossy(p).into_owned());
            }
        }
    }
    Some(ignored)
}

/// `git ls-files -co --exclude-standard -z`: tracked + untracked, non-ignored,
/// relative file paths. `None` when `root` is not inside a work tree.
async fn git_ls_files(root: &Path) -> Option<Vec<String>> {
    let probe = run_cmd("git", &["rev-parse", "--is-inside-work-tree"], root, None).await?;
    if probe.code != 0 || !probe.stdout.starts_with(b"true") {
        return None;
    }
    let res = run_cmd(
        "git",
        &[
            "-c",
            "core.fsmonitor=false",
            "ls-files",
            "-co",
            "--exclude-standard",
            "-z",
        ],
        root,
        None,
    )
    .await?;
    if res.code != 0 {
        return None;
    }
    Some(
        res.stdout
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// filesystem.browse — WorkspaceEntries.browse
// ---------------------------------------------------------------------------

async fn browse(payload: Value) -> RpcResult {
    let partial = payload
        .get("partialPath")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if !cfg!(windows) && is_windows_abs(&partial) {
        return browse_error(
            &partial,
            cwd.as_deref(),
            "windows_path_unsupported",
            None,
            false,
            Some(node_platform()),
        );
    }

    let resolved = if is_explicit_relative(&partial) {
        let Some(c) = &cwd else {
            return browse_error(
                &partial,
                None,
                "current_project_required",
                None,
                false,
                None,
            );
        };
        resolve_lexical_against(&expand_home(c), &partial)
    } else {
        resolve_lexical(&expand_home(&partial))
    };

    let ends_with_separator = partial.ends_with('/') || partial.ends_with('\\') || partial == "~";
    let parent = if ends_with_separator {
        resolved.clone()
    } else {
        resolved
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| resolved.clone())
    };
    let prefix = if ends_with_separator {
        String::new()
    } else {
        resolved
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };

    let parent_str = path_str(&parent);
    let dirents: Vec<std::fs::DirEntry> = match std::fs::read_dir(&parent) {
        Ok(it) => it.flatten().collect(),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => Vec::new(),
        Err(e) => {
            return browse_error(
                &partial,
                cwd.as_deref(),
                "read_directory_failed",
                Some(&parent_str),
                e.kind() == ErrorKind::NotFound,
                None,
            );
        }
    };

    let show_hidden = ends_with_separator || prefix.starts_with('.');
    let lower_prefix = prefix.to_lowercase();
    let mut names: Vec<String> = Vec::new();
    for d in dirents {
        let Ok(ft) = d.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let name = d.file_name().to_string_lossy().into_owned();
        if (!show_hidden && name.starts_with('.'))
            || !name.to_lowercase().starts_with(&lower_prefix)
        {
            continue;
        }
        names.push(name);
    }
    // `localeCompare` primary collation is case-insensitive.
    names.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)));

    let entries: Vec<Value> = names
        .iter()
        .map(|name| json!({"name": name, "fullPath": path_str(&parent.join(name))}))
        .collect();
    RpcResult::ok(json!({"parentPath": parent_str, "entries": entries}))
}

// ---------------------------------------------------------------------------
// filesystem.getMetadata — WorkspaceFileSystem.getMetadata
// ---------------------------------------------------------------------------

fn file_header_mime_type(bytes: &[u8]) -> Option<&'static str> {
    let starts = |sig: &[u8], off: usize| {
        bytes.len() >= off + sig.len() && &bytes[off..off + sig.len()] == sig
    };
    if starts(b"\x89PNG\r\n\x1a\n", 0) {
        return Some("image/png");
    }
    if starts(b"\xff\xd8\xff", 0) {
        return Some("image/jpeg");
    }
    if starts(b"PK\x03\x04", 0) {
        return Some("application/zip");
    }
    if starts(b"RIFF", 0) && starts(b"WEBP", 8) {
        return Some("image/webp");
    }
    let header = String::from_utf8_lossy(bytes);
    if header.starts_with("GIF87a") || header.starts_with("GIF89a") {
        return Some("image/gif");
    }
    if header.starts_with("%PDF-") {
        return Some("application/pdf");
    }
    if header.starts_with("SQLite format 3\u{0}") {
        return Some("application/vnd.sqlite3");
    }
    if !header.starts_with("#!") {
        return None;
    }
    let interpreter = header.split('\n').next().unwrap_or("");
    let has = |pat: &str| interpreter.contains(pat);
    if has("python") {
        return Some("text/x-python");
    }
    if has("node") {
        return Some("text/javascript");
    }
    if has("ruby") {
        return Some("text/x-ruby");
    }
    if has("sh") || has("bash") || has("zsh") || has("fish") {
        return Some("text/x-shellscript");
    }
    None
}

fn metadata_for_path(requested: &str) -> Value {
    let expanded = expand_home(requested.trim());
    if !Path::new(&expanded).is_absolute() || is_network_path(&expanded) {
        return Value::Null;
    }
    let md = match std::fs::metadata(&expanded) {
        Ok(m) => m,
        Err(_) => return Value::Null,
    };
    if md.is_dir() {
        return json!({"kind": "directory"});
    }
    if !md.is_file() {
        return json!({"kind": "other"});
    }
    let mut entry = json!({"kind": "file", "byteLength": md.len()});
    // Extension-less non-empty files get a 512-byte header sniff.
    if Path::new(&expanded).extension().is_none() && md.len() > 0 {
        if let Ok(mut f) = std::fs::File::open(&expanded) {
            use std::io::Read;
            let mut buf = [0u8; 512];
            if let Ok(n) = f.read(&mut buf) {
                if let Some(mime) = file_header_mime_type(&buf[..n]) {
                    entry["mimeType"] = json!(mime);
                }
            }
        }
    }
    entry
}

async fn get_metadata(payload: Value) -> RpcResult {
    let paths: Vec<String> = payload
        .get("paths")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let entries = tokio::task::spawn_blocking(move || {
        let mut seen: HashSet<String> = HashSet::new();
        let mut unique: Vec<String> = Vec::new();
        for p in &paths {
            if seen.insert(p.clone()) {
                unique.push(p.clone());
            }
        }
        let by_path: HashMap<String, Value> = unique
            .into_iter()
            .map(|p| (p.clone(), metadata_for_path(&p)))
            .collect();
        paths
            .iter()
            .map(|p| by_path.get(p).cloned().unwrap_or(Value::Null))
            .collect::<Vec<Value>>()
    })
    .await
    .unwrap_or_default();
    RpcResult::ok(json!({"entries": entries}))
}

// ---------------------------------------------------------------------------
// projects.listEntries — WorkspaceEntries.list
// ---------------------------------------------------------------------------

fn normalize_root_or_err(
    cwd: &str,
    tag: &str,
    message: String,
    extras: Map<String, Value>,
) -> Result<(PathBuf, String), RpcResult> {
    match normalize_root(cwd) {
        Ok(v) => Ok(v),
        Err(e) => {
            let (failure, normalized) = root_failure(&e);
            let detail = if matches!(e.0, RootError::StatFailed) {
                Some("validate-existing".to_string())
            } else {
                None
            };
            Err(entries_error(
                tag,
                cwd,
                failure,
                Some(normalized),
                detail,
                message,
                extras,
            ))
        }
    }
}

/// Children of one workspace directory (the `directoryPath` form). Includes
/// per-entry `ignored` labels via `git check-ignore`.
async fn list_directory(root: &Path, normalized_cwd: &str, dir_path: &str, cwd: &str) -> RpcResult {
    let derr = |detail: String| -> RpcResult {
        entries_error(
            "ProjectListEntriesError",
            cwd,
            "directory_list_failed",
            Some(normalized_cwd),
            Some(detail),
            format!("Failed to list workspace entries in '{cwd}'."),
            Map::new(),
        )
    };

    let (target_abs, target_rel) = if dir_path.is_empty() {
        (root.to_path_buf(), String::new())
    } else {
        match resolve_within_root(root, dir_path) {
            Some(t) => t,
            None => {
                return derr(format!(
                    "Directory must be inside the workspace and outside .git: {dir_path}"
                ))
            }
        }
    };

    let real_root = match std::fs::canonicalize(root) {
        Ok(r) => r,
        Err(e) => return derr(e.to_string()),
    };
    let real_dir = match std::fs::canonicalize(&target_abs) {
        Ok(r) => r,
        Err(e) => return derr(e.to_string()),
    };
    let rel_real = match real_dir.strip_prefix(&real_root) {
        Ok(r) => r.to_path_buf(),
        Err(_) => return derr("Directory must be inside the workspace and outside .git.".into()),
    };
    let dot_git = std::ffi::OsStr::new(".git");
    if rel_real
        .components()
        .any(|c| matches!(c, Component::Normal(s) if s == dot_git))
        || has_git_segment(&target_rel)
    {
        return derr("Directory must be inside the workspace and outside .git.".into());
    }

    let children: Vec<std::fs::DirEntry> = match std::fs::read_dir(&real_dir) {
        Ok(it) => it.flatten().collect(),
        Err(e) => return derr(e.to_string()),
    };

    // Canonical ancestors of the listed dir; symlinked subdirs that would
    // cycle back through one are rejected (matches the TS visitedFolders set).
    let mut visited: Vec<PathBuf> = vec![real_root.clone()];
    {
        let mut cur = root.to_path_buf();
        for seg in target_rel.split('/').filter(|s| !s.is_empty()) {
            cur.push(seg);
            if let Ok(r) = std::fs::canonicalize(&cur) {
                visited.push(r);
            }
        }
    }

    let mut entries: Vec<(String, &'static str)> = Vec::new();
    for child in children {
        let name = child.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let Ok(ft) = child.file_type() else { continue };
        let kind = if ft.is_dir() {
            Some("directory")
        } else if ft.is_file() {
            Some("file")
        } else if ft.is_symlink() {
            match std::fs::canonicalize(child.path()) {
                Err(_) => None,
                Ok(resolved) => {
                    let Ok(rel_r) = resolved.strip_prefix(&real_root) else {
                        continue;
                    };
                    if rel_r
                        .components()
                        .any(|c| matches!(c, Component::Normal(s) if s == dot_git))
                    {
                        continue;
                    }
                    match std::fs::metadata(&resolved) {
                        Ok(m) if m.is_file() => Some("file"),
                        Ok(m) if m.is_dir() => {
                            if visited.iter().any(|f| is_within_root(f, &resolved)) {
                                continue;
                            }
                            Some("directory")
                        }
                        _ => None,
                    }
                }
            }
        } else {
            None
        };
        if let Some(kind) = kind {
            let path = if target_rel.is_empty() {
                name
            } else {
                format!("{target_rel}/{name}")
            };
            entries.push((path, kind));
        }
    }

    let through_symlink = rel_real.to_string_lossy().replace('\\', "/") != target_rel;

    // Nested repositories redirect ignore classification to the deepest root.
    let segments: Vec<&str> = target_rel.split('/').filter(|s| !s.is_empty()).collect();
    let mut nested_roots: Vec<String> = Vec::new();
    for i in 1..=segments.len() {
        let cand = segments[..i].join("/");
        if std::fs::symlink_metadata(root.join(&cand).join(".git")).is_ok() {
            nested_roots.push(cand);
        }
    }
    let mut git_root = String::new();
    let mut git_prefix = String::new();
    for nested in &nested_roots {
        let probe = format!("{nested}/.git");
        let probe_in = probe
            .strip_prefix(&git_prefix)
            .unwrap_or(&probe)
            .to_string();
        let res = run_cmd(
            "git",
            &[
                "-c",
                "core.fsmonitor=false",
                "check-ignore",
                "-z",
                "--stdin",
            ],
            &root.join(&git_root),
            Some(format!("{probe_in}\0").into_bytes()),
        )
        .await;
        if res.map(|r| r.code) == Some(128) {
            git_root = nested.clone();
            git_prefix = format!("{nested}/");
        }
    }

    let git_cwd = root.join(&git_root);
    let run_check = |paths: Vec<String>| {
        let git_cwd = git_cwd.clone();
        let git_prefix = git_prefix.clone();
        async move {
            let stripped: Vec<String> = paths
                .iter()
                .map(|p| {
                    p.strip_prefix(&git_prefix)
                        .unwrap_or(p.as_str())
                        .to_string()
                })
                .collect();
            let mut input = Vec::new();
            for p in &stripped {
                input.extend_from_slice(p.as_bytes());
                input.push(0);
            }
            run_cmd(
                "git",
                &[
                    "-c",
                    "core.fsmonitor=false",
                    "check-ignore",
                    "-z",
                    "--stdin",
                ],
                &git_cwd,
                Some(input),
            )
            .await
        }
    };

    // Whole-dir shortcut: >1000 children inside an ignored folder are all ignored.
    let directory_git_path = target_rel
        .strip_prefix(&git_prefix)
        .unwrap_or(&target_rel)
        .to_string();
    let magic = directory_git_path.starts_with(':')
        || directory_git_path
            .chars()
            .any(|c| matches!(c, '*' | '?' | '[' | '\\'));
    if entries.len() > IGNORE_CHECK_CHUNK_SIZE
        && !directory_git_path.is_empty()
        && !through_symlink
        && !magic
        && run_check(vec![target_rel.clone()]).await.map(|r| r.code) == Some(0)
    {
        let out: Vec<Value> = entries
            .iter()
            .map(|(p, k)| json!({"path": p, "kind": k, "ignored": true}))
            .collect();
        return RpcResult::ok(json!({"entries": out, "truncated": false}));
    }

    let mut ignored: HashSet<String> = HashSet::new();
    let entry_paths: Vec<String> = entries.iter().map(|(p, _)| p.clone()).collect();
    for chunk in entry_paths.chunks(IGNORE_CHECK_CHUNK_SIZE) {
        let Some(res) = run_check(chunk.to_vec()).await else {
            break;
        };
        if res.code != 0 && res.code != 1 {
            break;
        }
        for p in res.stdout.split(|&b| b == 0) {
            if !p.is_empty() {
                ignored.insert(format!("{git_prefix}{}", String::from_utf8_lossy(p)));
            }
        }
    }

    let out: Vec<Value> = entries
        .iter()
        .map(|(p, k)| {
            if ignored.contains(p) {
                json!({"path": p, "kind": k, "ignored": true})
            } else {
                json!({"path": p, "kind": k})
            }
        })
        .collect();
    RpcResult::ok(json!({"entries": out, "truncated": false}))
}

fn parent_path_of(input: &str) -> Option<&str> {
    input.rfind('/').map(|i| &input[..i])
}

fn with_directory_ancestors(entries: Vec<(String, &'static str)>) -> Vec<(String, &'static str)> {
    let mut by_path: HashMap<String, &'static str> = entries.into_iter().collect();
    let keys: Vec<String> = by_path.keys().cloned().collect();
    for path in keys {
        let mut parent = parent_path_of(&path);
        while let Some(p) = parent {
            if !p.is_empty() {
                by_path.entry(p.to_string()).or_insert("directory");
            }
            parent = parent_path_of(p);
        }
    }
    by_path.into_iter().collect()
}

/// Recursive walk used when the workspace is not a git repo: hidden entries
/// (`.git` included) are skipped; symlinked dirs are followed only inside the
/// real root and only once.
fn walk_entries(root: &Path, real_root: &Path) -> (Vec<(String, &'static str)>, bool) {
    let mut entries: Vec<(String, &'static str)> = Vec::new();
    let mut truncated = false;
    let mut visited: HashSet<PathBuf> = HashSet::from([real_root.to_path_buf()]);
    let mut queue: Vec<(PathBuf, String, usize)> = vec![(root.to_path_buf(), String::new(), 0)];
    while let Some((dir, rel, depth)) = queue.pop() {
        if depth >= WALK_DEPTH_CAP {
            truncated = true;
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let path = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                entries.push((child_rel.clone(), "directory"));
                queue.push((path, child_rel, depth + 1));
            } else if ft.is_file() {
                entries.push((child_rel, "file"));
            } else if ft.is_symlink() {
                let Ok(canon) = std::fs::canonicalize(&path) else {
                    continue;
                };
                if !is_within_root(&canon, real_root) {
                    continue;
                }
                match std::fs::metadata(&canon) {
                    Ok(m) if m.is_dir() => {
                        if visited.insert(canon) {
                            entries.push((child_rel.clone(), "directory"));
                            queue.push((path, child_rel, depth + 1));
                        }
                    }
                    Ok(m) if m.is_file() => entries.push((child_rel, "file")),
                    _ => {}
                }
            }
            if entries.len() >= LIST_ENTRIES_MAX {
                truncated = true;
                break;
            }
        }
        if truncated {
            break;
        }
    }
    (entries, truncated)
}

/// Directories for a repo workspace: non-hidden, non-ignored dirs. Ignored
/// directories are dropped without descending; files come from `git ls-files`.
async fn walk_dirs_repo(root: &Path, real_root: &Path) -> (Vec<String>, bool) {
    let mut dirs: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut visited: HashSet<PathBuf> = HashSet::from([real_root.to_path_buf()]);
    let mut queue: Vec<(PathBuf, String, usize)> = vec![(root.to_path_buf(), String::new(), 0)];
    while let Some((dir, rel, depth)) = queue.pop() {
        if depth >= WALK_DEPTH_CAP {
            truncated = true;
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<(PathBuf, String)> = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = e.path();
            let Ok(ft) = e.file_type() else { continue };
            let is_dir = if ft.is_dir() {
                true
            } else if ft.is_symlink() {
                match std::fs::canonicalize(&path) {
                    Ok(canon) => {
                        is_within_root(&canon, real_root)
                            && std::fs::metadata(&canon)
                                .map(|m| m.is_dir())
                                .unwrap_or(false)
                            && !visited.contains(&canon)
                    }
                    Err(_) => false,
                }
            } else {
                false
            };
            if !is_dir {
                continue;
            }
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            children.push((path, child_rel));
        }
        let ignored = git_check_ignore(
            root,
            &children.iter().map(|(_, r)| r.clone()).collect::<Vec<_>>(),
        )
        .await;
        for (path, child_rel) in children {
            if let Some(set) = &ignored {
                if set.contains(&child_rel) {
                    continue;
                }
            }
            if let Ok(canon) = std::fs::canonicalize(&path) {
                visited.insert(canon);
            }
            dirs.push(child_rel.clone());
            queue.push((path, child_rel, depth + 1));
            if dirs.len() >= LIST_ENTRIES_MAX {
                truncated = true;
                break;
            }
        }
        if truncated {
            break;
        }
    }
    (dirs, truncated)
}

/// Full recursive listing (`directoryPath` absent): repo workspaces use
/// `git ls-files` plus an ignore-aware dir walk; everything else walks the
/// tree with hidden-file filtering.
async fn list_recursive(root: &Path, real_root: &Path) -> (Vec<Value>, bool) {
    let mut truncated = false;
    let mut entries: Vec<(String, &'static str)> = Vec::new();
    match git_ls_files(root).await {
        Some(files) => {
            let file_count = files.len();
            entries.extend(files.into_iter().map(|f| (f, "file")));
            if file_count > LIST_ENTRIES_MAX {
                truncated = true;
            }
            let (dirs, dirs_trunc) = walk_dirs_repo(root, real_root).await;
            truncated |= dirs_trunc;
            entries.extend(dirs.into_iter().map(|d| (d, "directory")));
        }
        None => {
            let (walked, t) = walk_entries(root, real_root);
            entries = walked;
            truncated = t;
        }
    }
    let mut entries = with_directory_ancestors(entries);
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    if entries.len() > LIST_ENTRIES_MAX {
        entries.truncate(LIST_ENTRIES_MAX);
        truncated = true;
    }
    (
        entries
            .into_iter()
            .map(|(p, k)| json!({"path": p, "kind": k}))
            .collect(),
        truncated,
    )
}

async fn list_entries(state: Arc<AppState>, payload: Value) -> RpcResult {
    let cwd = match workspace_root(&state, &payload).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let (root, normalized) = match normalize_root_or_err(
        &cwd,
        "ProjectListEntriesError",
        format!("Failed to list workspace entries in '{cwd}'."),
        Map::new(),
    ) {
        Ok(v) => v,
        Err(r) => return r,
    };

    if let Some(dir_path) = payload.get("directoryPath").and_then(Value::as_str) {
        return list_directory(&root, &normalized, dir_path, &cwd).await;
    }

    let real_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    let (entries, truncated) = list_recursive(&root, &real_root).await;
    RpcResult::ok(json!({"entries": entries, "truncated": truncated}))
}

// ---------------------------------------------------------------------------
// projects.listIgnoredFiles — WorkspaceEntries.listIgnoredFiles
// ---------------------------------------------------------------------------

async fn list_ignored_files(state: Arc<AppState>, payload: Value) -> RpcResult {
    let cwd = match workspace_root(&state, &payload).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let (root, _normalized) = match normalize_root_or_err(
        &cwd,
        "ProjectListEntriesError",
        format!("Failed to list workspace entries in '{cwd}'."),
        Map::new(),
    ) {
        Ok(v) => v,
        Err(r) => return r,
    };

    let result = run_cmd(
        "git",
        &[
            "-c",
            "core.fsmonitor=false",
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "--no-empty-directory",
            "-z",
        ],
        &root,
        None,
    )
    .await;
    let Some(res) = result else {
        return RpcResult::ok(json!({"files": [], "truncated": false}));
    };
    if res.code != 0 {
        return RpcResult::ok(json!({"files": [], "truncated": false}));
    }
    let mut paths: Vec<String> = res
        .stdout
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty() && !p.ends_with(b"/"))
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    paths.sort();
    let truncated = paths.len() > IGNORED_FILES_LIMIT;
    let files: Vec<Value> = paths
        .iter()
        .take(IGNORED_FILES_LIMIT)
        .filter_map(|rel| {
            let md = std::fs::symlink_metadata(root.join(rel)).ok()?;
            md.is_file()
                .then(|| json!({"path": rel, "byteLength": md.len()}))
        })
        .collect();
    RpcResult::ok(json!({"files": files, "truncated": truncated}))
}

// ---------------------------------------------------------------------------
// projects.readFile / writeFile — WorkspaceFileSystem
// ---------------------------------------------------------------------------

fn read_error(
    tag: &str,
    cwd: &str,
    rel: &str,
    failure: &str,
    resolved: Option<&str>,
    root: Option<&str>,
    op: Option<&str>,
    op_path: Option<&str>,
) -> RpcResult {
    let verb = if tag == "ProjectReadFileError" {
        "read"
    } else {
        "write"
    };
    file_error(
        tag,
        cwd,
        rel,
        failure,
        resolved,
        root,
        op,
        op_path,
        format!("Failed to {verb} workspace file '{rel}' in '{cwd}'."),
    )
}

/// Resolve the deepest existing ancestor then re-attach the missing tail —
/// the write path's `realpath` walk and `allowMissing` read path share it.
/// Returns (real path, exists-as-named).
fn realpath_through_ancestors(
    target: &Path,
    allow_missing: bool,
) -> Result<(PathBuf, bool), (std::io::Error, PathBuf)> {
    let mut candidate = target.to_path_buf();
    loop {
        match std::fs::canonicalize(&candidate) {
            Ok(real) => {
                let tail = target
                    .strip_prefix(&candidate)
                    .unwrap_or_else(|_| Path::new(""));
                return Ok((normalize_lexical(&real.join(tail)), candidate == target));
            }
            Err(e) => {
                if !allow_missing || e.kind() != ErrorKind::NotFound {
                    return Err((e, candidate));
                }
                // An existing entry that cannot resolve is a dangling link.
                if std::fs::symlink_metadata(&candidate).is_ok() {
                    return Err((e, candidate));
                }
                match candidate.parent() {
                    Some(p) if p != candidate => candidate = p.to_path_buf(),
                    _ => return Err((e, candidate)),
                }
            }
        }
    }
}

struct ReadTarget {
    relative_path: String,
    real_target: Option<PathBuf>,
}

fn resolve_read_target(
    cwd: &str,
    relative_in: &str,
    allow_missing: bool,
) -> Result<ReadTarget, RpcResult> {
    let requested = relative_in.trim();
    let host = if requested.starts_with("~/") {
        expand_home(requested)
    } else {
        requested.to_string()
    };
    let op_failed = |resolved: &str, op_path: &str| -> RpcResult {
        read_error(
            "ProjectReadFileError",
            cwd,
            relative_in,
            "operation_failed",
            Some(resolved),
            None,
            Some("realpath-target"),
            Some(op_path),
        )
    };
    // Absolute host paths (and `~/`) read in place; network paths must stay
    // inside the workspace.
    if is_network_path(&host) {
        let resolved_host = resolve_lexical(&host);
        let resolved_cwd = resolve_lexical(&expand_home(cwd.trim()));
        if !is_within_root(&resolved_host, &resolved_cwd) {
            return Err(read_error(
                "ProjectReadFileError",
                cwd,
                relative_in,
                "workspace_path_outside_root",
                None,
                None,
                None,
                None,
            ));
        }
    }
    if Path::new(&host).is_absolute() {
        return match std::fs::canonicalize(&host) {
            Ok(real) => Ok(ReadTarget {
                relative_path: requested.to_string(),
                real_target: Some(real),
            }),
            Err(e) if e.kind() == ErrorKind::NotFound && allow_missing => Ok(ReadTarget {
                relative_path: requested.to_string(),
                real_target: None,
            }),
            Err(_) => Err(op_failed(&host, &host)),
        };
    }

    let lex_root = resolve_lexical(&expand_home(cwd.trim()));
    let Some((abs_target, rel)) = resolve_within_root(&lex_root, requested) else {
        return Err(read_error(
            "ProjectReadFileError",
            cwd,
            relative_in,
            "workspace_path_outside_root",
            None,
            None,
            None,
            None,
        ));
    };
    let real_root = match std::fs::canonicalize(&lex_root) {
        Ok(r) => r,
        Err(_) => {
            return Err(read_error(
                "ProjectReadFileError",
                cwd,
                relative_in,
                "operation_failed",
                Some(&path_str(&abs_target)),
                None,
                Some("realpath-workspace-root"),
                Some(cwd),
            ))
        }
    };
    match realpath_through_ancestors(&abs_target, allow_missing) {
        Err((_, cand)) => Err(op_failed(&path_str(&abs_target), &path_str(&cand))),
        Ok((real, exists)) => {
            if !is_within_root(&real, &real_root) {
                return Err(read_error(
                    "ProjectReadFileError",
                    cwd,
                    relative_in,
                    "resolved_path_outside_root",
                    Some(&path_str(&real)),
                    Some(&path_str(&real_root)),
                    None,
                    None,
                ));
            }
            Ok(ReadTarget {
                relative_path: rel,
                real_target: if exists { Some(real) } else { None },
            })
        }
    }
}

async fn read_file(state: Arc<AppState>, payload: Value) -> RpcResult {
    let cwd = match workspace_root(&state, &payload).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let relative_in = payload
        .get("relativePath")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let allow_missing = payload.get("allowMissing").and_then(Value::as_bool) == Some(true);

    let cwd2 = cwd.clone();
    let rel2 = relative_in.clone();
    tokio::task::spawn_blocking(move || {
        let target = match resolve_read_target(&cwd2, &rel2, allow_missing) {
            Ok(t) => t,
            Err(r) => return r,
        };
        let absent = |relative_path: String| -> RpcResult {
            RpcResult::ok(json!({
                "relativePath": relative_path,
                "contents": "",
                "byteLength": 0,
                "truncated": false,
                "exists": false,
            }))
        };
        let Some(real) = target.real_target else {
            return absent(target.relative_path);
        };
        let real_str = path_str(&real);
        let md = match std::fs::metadata(&real) {
            Ok(m) => m,
            Err(_) => {
                return read_error(
                    "ProjectReadFileError",
                    &cwd2,
                    &rel2,
                    "operation_failed",
                    Some(&real_str),
                    None,
                    Some("stat"),
                    Some(&real_str),
                )
            }
        };
        if !md.is_file() {
            return read_error(
                "ProjectReadFileError",
                &cwd2,
                &rel2,
                "path_not_file",
                Some(&real_str),
                None,
                None,
                None,
            );
        }
        let bytes_to_read = md.len().min(READ_FILE_MAX_BYTES) as usize;
        let mut buf = vec![0u8; bytes_to_read];
        let mut offset = 0usize;
        {
            use std::io::Read;
            let mut f = match std::fs::File::open(&real) {
                Ok(f) => f,
                Err(_) => {
                    return read_error(
                        "ProjectReadFileError",
                        &cwd2,
                        &rel2,
                        "operation_failed",
                        Some(&real_str),
                        None,
                        Some("open"),
                        Some(&real_str),
                    )
                }
            };
            while offset < bytes_to_read {
                match f.read(&mut buf[offset..]) {
                    Ok(0) => break,
                    Ok(n) => offset += n,
                    Err(_) => {
                        return read_error(
                            "ProjectReadFileError",
                            &cwd2,
                            &rel2,
                            "operation_failed",
                            Some(&real_str),
                            None,
                            Some("read"),
                            Some(&real_str),
                        )
                    }
                }
            }
        }
        let file_bytes = &buf[..offset];
        if file_bytes.contains(&0) {
            return read_error(
                "ProjectReadFileError",
                &cwd2,
                &rel2,
                "binary_file",
                Some(&real_str),
                None,
                None,
                None,
            );
        }
        let truncated = md.len() > READ_FILE_MAX_BYTES;
        RpcResult::ok(json!({
            "relativePath": target.relative_path,
            "contents": String::from_utf8_lossy(file_bytes),
            "byteLength": md.len(),
            "truncated": truncated,
        }))
    })
    .await
    .unwrap_or_else(|_| {
        read_error(
            "ProjectReadFileError",
            &cwd,
            &relative_in,
            "operation_failed",
            None,
            None,
            Some("read"),
            None,
        )
    })
}

/// `.git` folder rejection for writes, including renamed bare repos.
fn is_in_git_folder(absolute: &Path) -> bool {
    if has_git_segment(&absolute.to_string_lossy().replace('\\', "/")) {
        return true;
    }
    let mut folder = absolute.parent();
    while let Some(f) = folder {
        let has = |name: &str| std::fs::symlink_metadata(f.join(name)).is_ok();
        let head = has("HEAD");
        let commondir = has("commondir");
        let objects = has("objects");
        let refs = has("refs");
        let dot_git_file = std::fs::symlink_metadata(f.join(".git"))
            .map(|m| m.is_file())
            .unwrap_or(false);
        if head && (commondir || (objects && refs)) {
            return true;
        }
        if dot_git_file {
            return false;
        }
        match f.parent() {
            Some(p) if p != f => folder = Some(p),
            _ => break,
        }
    }
    false
}

/// `resolveWriteTarget`: lexical containment, ancestor realpath, `.git`
/// rejection. Returns (relative_path, real target).
fn resolve_write_target(cwd: &str, relative_in: &str) -> Result<(String, PathBuf), RpcResult> {
    let lex_root = resolve_lexical(&expand_home(cwd.trim()));
    let Some((abs_target, rel)) = resolve_within_root(&lex_root, relative_in) else {
        return Err(read_error(
            "ProjectWriteFileError",
            cwd,
            relative_in,
            "workspace_path_outside_root",
            None,
            None,
            None,
            None,
        ));
    };
    let abs_str = path_str(&abs_target);
    let real_root = match std::fs::canonicalize(&lex_root) {
        Ok(r) => r,
        Err(_) => {
            return Err(read_error(
                "ProjectWriteFileError",
                cwd,
                relative_in,
                "operation_failed",
                Some(&abs_str),
                None,
                Some("realpath-workspace-root"),
                Some(cwd),
            ))
        }
    };
    // Deepest existing ancestor resolves; missing tail stays lexical.
    let segments: Vec<String> = rel.split('/').map(str::to_owned).collect();
    let real_target = {
        let mut resolved: Option<PathBuf> = None;
        for depth in (1..=segments.len()).rev() {
            let candidate = real_root.join(segments[..depth].join("/"));
            match std::fs::canonicalize(&candidate) {
                Ok(real) => {
                    resolved = Some(normalize_lexical(&real.join(segments[depth..].join("/"))));
                    break;
                }
                Err(e) => {
                    let exists = std::fs::symlink_metadata(&candidate).is_ok();
                    if e.kind() != ErrorKind::NotFound || exists {
                        return Err(read_error(
                            "ProjectWriteFileError",
                            cwd,
                            relative_in,
                            "operation_failed",
                            Some(&abs_str),
                            None,
                            Some("realpath-target"),
                            Some(&abs_str),
                        ));
                    }
                }
            }
        }
        resolved.unwrap_or_else(|| real_root.join(segments.join("/")))
    };
    let named_target = real_root.join(segments.join("/"));
    let real_str = path_str(&real_target);
    if !is_within_root(&real_target, &real_root)
        || is_in_git_folder(&real_target)
        || is_in_git_folder(&named_target)
    {
        return Err(read_error(
            "ProjectWriteFileError",
            cwd,
            relative_in,
            "resolved_path_outside_root",
            Some(&real_str),
            Some(&path_str(&real_root)),
            None,
            None,
        ));
    }
    Ok((rel, real_target))
}

async fn write_file(state: Arc<AppState>, payload: Value) -> RpcResult {
    let cwd = match workspace_root(&state, &payload).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let relative_in = payload
        .get("relativePath")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let contents = payload
        .get("contents")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    // `knownWorkspaces.assertContains`: writes only inside a known workspace
    // (a project root, or the managed scratch/projects folders).
    let real_cwd = std::fs::canonicalize(resolve_lexical(&expand_home(cwd.trim())));
    let known = match real_cwd {
        Err(_) => false,
        Ok(real_cwd) => {
            let mut roots: Vec<PathBuf> = vec![
                state.home_dir.join("scratch"),
                state.home_dir.join("projects"),
            ];
            for p in state.store.list_projects().await {
                if let Some(w) = p.get("workspaceRoot").and_then(Value::as_str) {
                    roots.push(PathBuf::from(expand_home(w)));
                }
            }
            roots.iter().any(|r| {
                std::fs::canonicalize(r)
                    .map(|rr| is_within_root(&real_cwd, &rr))
                    .unwrap_or(false)
            })
        }
    };
    if !known {
        return read_error(
            "ProjectWriteFileError",
            &cwd,
            &relative_in,
            "workspace_path_outside_root",
            None,
            None,
            None,
            None,
        );
    }

    let cwd2 = cwd.clone();
    let rel2 = relative_in.clone();
    tokio::task::spawn_blocking(move || {
        let (rel, real_target) = match resolve_write_target(&cwd2, &rel2) {
            Ok(t) => t,
            Err(r) => return r,
        };
        let real_str = path_str(&real_target);
        if let Some(parent) = real_target.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                let _ = e;
                return read_error(
                    "ProjectWriteFileError",
                    &cwd2,
                    &rel2,
                    "operation_failed",
                    Some(&real_str),
                    None,
                    Some("make-directory"),
                    Some(&path_str(parent)),
                );
            }
        }
        // A symlink swapped in after the checks is not followed.
        #[cfg(unix)]
        let write_res = {
            use std::os::unix::fs::OpenOptionsExt;
            const O_NOFOLLOW: i32 = 0o400000;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .custom_flags(O_NOFOLLOW)
                .open(&real_target)
                .and_then(|mut f| std::io::Write::write_all(&mut f, contents.as_bytes()))
        };
        #[cfg(not(unix))]
        let write_res = std::fs::write(&real_target, contents.as_bytes());
        if write_res.is_err() {
            return read_error(
                "ProjectWriteFileError",
                &cwd2,
                &rel2,
                "operation_failed",
                Some(&real_str),
                None,
                Some("write-file"),
                Some(&real_str),
            );
        }
        RpcResult::ok(json!({"relativePath": rel}))
    })
    .await
    .unwrap_or_else(|_| {
        read_error(
            "ProjectWriteFileError",
            &cwd,
            &relative_in,
            "operation_failed",
            None,
            None,
            Some("write-file"),
            None,
        )
    })
}

// ---------------------------------------------------------------------------
// projects.searchContents — WorkspaceSearchIndex.searchContents
// ---------------------------------------------------------------------------

fn b64_decode(s: &str) -> Vec<u8> {
    fn val(b: u8) -> u8 {
        match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => 255,
        }
    }
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut n = 0u32;
    for &b in s.as_bytes() {
        if b == b'=' {
            break;
        }
        let v = val(b);
        if v == 255 {
            continue;
        }
        acc = (acc << 6) | v as u32;
        n += 1;
        if n == 4 {
            out.push((acc >> 16) as u8);
            out.push((acc >> 8) as u8);
            out.push(acc as u8);
            acc = 0;
            n = 0;
        }
    }
    match n {
        3 => {
            out.push((acc >> 10) as u8);
            out.push((acc >> 2) as u8);
        }
        2 => out.push((acc >> 4) as u8),
        _ => {}
    }
    out
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(|c| c.len_utf16()).sum()
}

fn clamp_boundary(s: &str, mut off: usize) -> usize {
    off = off.min(s.len());
    while !s.is_char_boundary(off) {
        off -= 1;
    }
    off
}

/// Byte range in `line` → UTF-16 code-unit range (JS string indices).
fn to_string_range(line: &str, start_b: usize, end_b: usize) -> Value {
    json!({
        "start": utf16_len(&line[..clamp_boundary(line, start_b)]),
        "end": utf16_len(&line[..clamp_boundary(line, end_b)]),
    })
}

fn is_word_char(c: Option<char>) -> bool {
    c.map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false)
}

/// `isWholeWordRange` from WorkspaceSearchIndex.ts.
fn is_whole_word_range(line: &str, start_b: usize, end_b: usize) -> bool {
    if end_b <= start_b {
        return false;
    }
    let left_ok = start_b == 0
        || !is_word_char(line[..start_b].chars().next_back())
        || !is_word_char(line[start_b..].chars().next());
    let right_ok = end_b >= line.len()
        || !is_word_char(line[end_b..].chars().next())
        || !is_word_char(line[..end_b].chars().next_back());
    left_ok && right_ok
}

fn cap_line(line: &str) -> String {
    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
    trimmed.chars().take(LINE_CONTENT_CAP).collect()
}

struct ContentMatch {
    path: String,
    line_number: u64,
    line: String,
    ranges: Vec<(usize, usize)>, // byte offsets in `line`
}

fn content_match_json(m: &ContentMatch) -> Value {
    json!({
        "path": m.path,
        "lineNumber": m.line_number,
        "lineContent": m.line,
        "matchRanges": m.ranges.iter().map(|(s, e)| to_string_range(&m.line, *s, *e)).collect::<Vec<_>>(),
    })
}

/// Parse `rg --json` output into content matches.
fn parse_rg_json(
    stdout: &[u8],
    root: &Path,
    whole_word: bool,
    limit: usize,
) -> (Vec<ContentMatch>, bool) {
    let mut matches: Vec<ContentMatch> = Vec::new();
    let mut truncated = false;
    'outer: for line in stdout.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("match") {
            continue;
        }
        let data = &v["data"];
        let text_of = |key: &str| -> String {
            let node = &data[key];
            if let Some(t) = node.get("text").and_then(Value::as_str) {
                t.to_string()
            } else if let Some(b) = node.get("bytes").and_then(Value::as_str) {
                String::from_utf8_lossy(&b64_decode(b)).into_owned()
            } else {
                String::new()
            }
        };
        let raw_path = text_of("path");
        let line_text = cap_line(&text_of("lines"));
        let line_number = data.get("line_number").and_then(Value::as_u64).unwrap_or(0);
        // Path relative to the searched root; rg prefixes "./" for "." scans.
        let rel = raw_path.strip_prefix("./").unwrap_or(&raw_path).to_string();
        let rel = if Path::new(&rel).is_absolute() {
            Path::new(&rel)
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or(rel)
        } else {
            rel
        };
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        if let Some(subs) = data.get("submatches").and_then(Value::as_array) {
            for sm in subs {
                let s = sm.get("start").and_then(Value::as_u64).unwrap_or(0) as usize;
                let e = sm.get("end").and_then(Value::as_u64).unwrap_or(0) as usize;
                let s = clamp_boundary(&line_text, s);
                let e = clamp_boundary(&line_text, e);
                if s >= line_text.len() || e <= s {
                    continue;
                }
                if whole_word && !is_whole_word_range(&line_text, s, e) {
                    continue;
                }
                ranges.push((s, e));
            }
        }
        if whole_word && ranges.is_empty() {
            continue;
        }
        if matches.len() >= limit {
            truncated = true;
            break 'outer;
        }
        matches.push(ContentMatch {
            path: rel,
            line_number,
            line: line_text,
            ranges,
        });
    }
    (matches, truncated)
}

fn literal_ranges(line: &str, needle: &[u8], case_sensitive: bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    let lb = line.as_bytes();
    let mut i = 0usize;
    while i + needle.len() <= lb.len() {
        let hit = if case_sensitive {
            &lb[i..i + needle.len()] == needle
        } else {
            lb[i..i + needle.len()].eq_ignore_ascii_case(needle)
        };
        if hit {
            out.push((i, i + needle.len()));
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// Files to scan for content search: `git ls-files` when available, else a
/// hidden-filtered walk.
async fn collect_search_files(root: &Path, real_root: &Path) -> Vec<String> {
    if let Some(files) = git_ls_files(root).await {
        return files;
    }
    let (entries, _) = walk_entries(root, real_root);
    entries
        .into_iter()
        .filter(|(_, k)| *k == "file")
        .map(|(p, _)| p)
        .collect()
}

fn walk_search_contents(
    root: &Path,
    files: Vec<String>,
    query: &str,
    case_sensitive: bool,
    whole_word: bool,
    limit: usize,
) -> (Vec<ContentMatch>, bool) {
    let mut matches = Vec::new();
    let mut truncated = false;
    let needle = query.as_bytes();
    'files: for rel in files {
        let path = root.join(&rel);
        let Ok(md) = std::fs::metadata(&path) else {
            continue;
        };
        if !md.is_file() || md.len() > SEARCH_FILE_MAX_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes[..bytes.len().min(8192)].contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut per_file = 0usize;
        for (idx, raw_line) in text.split('\n').enumerate() {
            if per_file >= SEARCH_MAX_MATCHES_PER_FILE {
                continue 'files;
            }
            let line = cap_line(raw_line);
            let ranges: Vec<(usize, usize)> = literal_ranges(&line, needle, case_sensitive)
                .into_iter()
                .filter(|(s, e)| !whole_word || is_whole_word_range(&line, *s, *e))
                .collect();
            if ranges.is_empty() {
                continue;
            }
            per_file += 1;
            matches.push(ContentMatch {
                path: rel.clone(),
                line_number: idx as u64 + 1,
                line,
                ranges,
            });
            if matches.len() > limit {
                break 'files;
            }
        }
    }
    if matches.len() > limit {
        matches.truncate(limit);
        truncated = true;
    }
    (matches, truncated)
}

async fn search_contents(state: Arc<AppState>, payload: Value) -> RpcResult {
    let cwd = match workspace_root(&state, &payload).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let query = payload
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .min(500) as usize;
    let case_sensitive = payload.get("caseSensitive").and_then(Value::as_bool) == Some(true);
    let whole_word = payload.get("wholeWord").and_then(Value::as_bool) == Some(true);
    let use_regex = payload.get("useRegex").and_then(Value::as_bool) == Some(true);

    let mut extras = Map::new();
    extras.insert("queryLength".into(), json!(query.chars().count()));
    extras.insert("limit".into(), json!(limit));
    let (root, _normalized) = match normalize_root_or_err(
        &cwd,
        "ProjectSearchContentsError",
        format!("Failed to search workspace contents in '{cwd}'."),
        extras.clone(),
    ) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if query.is_empty() || limit == 0 {
        return RpcResult::ok(json!({"matches": [], "truncated": false}));
    }

    // ripgrep path: --json gives byte-precise submatch ranges.
    let rg_pattern = if use_regex && !case_sensitive {
        format!("(?i){query}")
    } else {
        query.clone()
    };
    let mut args: Vec<String> = vec![
        "--json".into(),
        "--max-count".into(),
        SEARCH_MAX_MATCHES_PER_FILE.to_string(),
        "--max-columns".into(),
        "0".into(),
    ];
    if !use_regex {
        args.push("-F".into());
    }
    if !case_sensitive && !use_regex {
        args.push("-i".into());
    }
    args.push("-e".into());
    args.push(rg_pattern.clone());
    args.push(".".into());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let rg_result = run_cmd("rg", &arg_refs, &root, None).await;

    let mut regex_fallback_error: Option<String> = None;
    let (matches, truncated) = match rg_result {
        Some(res) if res.code == 0 || res.code == 1 => {
            parse_rg_json(&res.stdout, &root, whole_word, limit)
        }
        Some(res) if use_regex => {
            // Bad pattern: retry literally, mirroring the fff fallback.
            regex_fallback_error = Some(
                String::from_utf8_lossy(&res.stderr)
                    .lines()
                    .next()
                    .unwrap_or("invalid regex")
                    .to_string(),
            );
            let mut args2: Vec<String> = vec![
                "--json".into(),
                "--max-count".into(),
                SEARCH_MAX_MATCHES_PER_FILE.to_string(),
                "--max-columns".into(),
                "0".into(),
                "-F".into(),
            ];
            if !case_sensitive {
                args2.push("-i".into());
            }
            args2.push("-e".into());
            args2.push(query.clone());
            args2.push(".".into());
            let refs2: Vec<&str> = args2.iter().map(String::as_str).collect();
            match run_cmd("rg", &refs2, &root, None).await {
                Some(r2) if r2.code == 0 || r2.code == 1 => {
                    parse_rg_json(&r2.stdout, &root, whole_word, limit)
                }
                _ => (Vec::new(), false),
            }
        }
        _ => {
            // No rg (or spawn/timeout): walk + literal scan.
            if use_regex {
                regex_fallback_error =
                    Some("ripgrep is required for regex search; matched literally".into());
            }
            let real_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
            let files = collect_search_files(&root, &real_root).await;
            let root2 = root.clone();
            tokio::task::spawn_blocking(move || {
                walk_search_contents(&root2, files, &query, case_sensitive, whole_word, limit)
            })
            .await
            .unwrap_or_default()
        }
    };

    let mut out = Map::new();
    out.insert(
        "matches".into(),
        Value::Array(matches.iter().map(content_match_json).collect()),
    );
    out.insert("truncated".into(), json!(truncated));
    if let Some(e) = regex_fallback_error {
        out.insert("regexFallbackError".into(), json!(e));
    }
    RpcResult::ok(Value::Object(out))
}

// ---------------------------------------------------------------------------
// projects.searchEntries — WorkspaceSearchIndex.search (subsequence match)
// ---------------------------------------------------------------------------

/// Trim, strip leading `[@./]+`, lowercase — `normalizeSearchQuery`.
fn normalize_search_query(input: &str) -> String {
    let trimmed = input.trim();
    trimmed.trim_start_matches(['@', '.', '/']).to_lowercase()
}

/// All query chars appear in order (case-folded); returns first-hit index.
fn subsequence_match(path: &str, query: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }
    let mut qi = query.chars().peekable();
    let mut first: Option<usize> = None;
    for (i, c) in path.chars().enumerate() {
        let Some(&q) = qi.peek() else { break };
        if c.to_lowercase().next() == Some(q) || c == q {
            if first.is_none() {
                first = Some(i);
            }
            qi.next();
        }
    }
    if qi.peek().is_none() {
        Some(first.unwrap_or(0))
    } else {
        None
    }
}

fn is_favicon_path(path: &str) -> bool {
    const EXTS: &[&str] = &[
        ".avif", ".bmp", ".gif", ".ico", ".heic", ".heif", ".jpeg", ".jpg", ".png", ".svg", ".tif",
        ".tiff", ".webp", ".icns",
    ];
    let lower = path.to_lowercase();
    EXTS.iter().any(|e| lower.ends_with(e))
}

async fn search_entries(state: Arc<AppState>, payload: Value) -> RpcResult {
    let cwd = match workspace_root(&state, &payload).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let query_raw = payload
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .min(200) as usize;
    let kind = payload.get("kind").and_then(Value::as_str);
    let image_only = payload.get("imageOnly").and_then(Value::as_bool) == Some(true);

    let mut extras = Map::new();
    extras.insert("queryLength".into(), json!(query_raw.chars().count()));
    extras.insert("limit".into(), json!(limit));
    let (root, _normalized) = match normalize_root_or_err(
        &cwd,
        "ProjectSearchEntriesError",
        format!("Failed to search workspace entries in '{cwd}'."),
        extras,
    ) {
        Ok(v) => v,
        Err(r) => return r,
    };

    let real_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    let (entries, mut truncated) = list_recursive(&root, &real_root).await;
    let query = normalize_search_query(&query_raw);
    let mut scored: Vec<(usize, usize, Value)> = entries
        .into_iter()
        .filter(|e| {
            let k = e.get("kind").and_then(Value::as_str).unwrap_or("");
            let p = e.get("path").and_then(Value::as_str).unwrap_or("");
            (kind.is_none() || kind == Some(k))
                && (!image_only || (k == "file" && is_favicon_path(p)))
        })
        .filter_map(|e| {
            let p = e
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            subsequence_match(&p, &query).map(|first| (first, p.len(), e))
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    if scored.len() > limit {
        scored.truncate(limit);
        truncated = true;
    }
    RpcResult::ok(json!({
        "entries": scored.into_iter().map(|(_, _, e)| e).collect::<Vec<_>>(),
        "truncated": truncated,
    }))
}

// ---------------------------------------------------------------------------
// Direct SQLite access (projection_projects lives outside the store actor's
// public API; mirror append_event / apply_project_create conventions).
// ---------------------------------------------------------------------------

fn open_db(home: &Path) -> Result<Connection, String> {
    let conn = Connection::open(home.join("userdata/statev2.sqlite")).map_err(|e| e.to_string())?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

/// `append_event` equivalent for `aggregate_kind = 'project'` rows.
fn append_project_event(
    conn: &Connection,
    stream_kind: &str,
    stream_id: &str,
    event: &Value,
    command_id: Option<&str>,
) -> Result<CommittedEvent, String> {
    let ty = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let occurred_at = event
        .get("occurredAt")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(domain::now_iso);
    let event_id = event
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(domain::new_id);
    let payload_json = serde_json::to_string(event.get("payload").unwrap_or(&Value::Null))
        .map_err(|e| e.to_string())?;
    let sequence: i64 = conn
        .query_row(
            "INSERT INTO orchestration_events (
               event_id, aggregate_kind, stream_id, stream_version, event_type,
               occurred_at, command_id, causation_event_id, correlation_id,
               actor_kind, payload_json, metadata_json, application_event_version
             ) VALUES (
               ?1, ?2, ?3,
               COALESCE((SELECT MAX(stream_version) + 1 FROM orchestration_events
                         WHERE aggregate_kind = ?2 AND stream_id = ?3), 0),
               ?4, ?5, ?6, NULL, ?6, 'server', ?7, '{}', 2
             ) RETURNING sequence",
            params![
                event_id,
                stream_kind,
                stream_id,
                ty,
                occurred_at,
                command_id,
                payload_json
            ],
            |r| r.get(0),
        )
        .map_err(|e| format!("append event {ty}: {e}"))?;
    Ok(CommittedEvent {
        sequence,
        aggregate_kind: stream_kind.into(),
        stream_id: stream_id.into(),
        event: json!({
            "id": event_id,
            "threadId": stream_id,
            "type": ty,
            "occurredAt": occurred_at,
            "payload": serde_json::from_str::<Value>(&payload_json).unwrap_or(Value::Null),
        }),
    })
}

const PROJECT_COLS: &str = "project_id, title, workspace_root, scripts_json, created_at, \
     updated_at, deleted_at, default_model_selection_json, default_thread_env_mode, \
     favicon_path, auto_pull, project_icon_json";

fn project_row_to_json(
    project_id: &str,
    title: &str,
    workspace_root: &str,
    scripts_json: &str,
    created_at: &str,
    updated_at: &str,
    deleted_at: Option<&str>,
    default_model: Option<&str>,
    env_mode: Option<&str>,
    favicon: Option<&str>,
    auto_pull: i64,
    icon_json: Option<&str>,
) -> Value {
    json!({
        "id": project_id,
        "title": title,
        "workspaceRoot": workspace_root,
        "repositoryIdentity": Value::Null,
        "faviconPath": favicon.map(Value::from).unwrap_or(Value::Null),
        "projectIcon": icon_json.and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(Value::Null),
        "defaultModelSelection": default_model.and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(Value::Null),
        "defaultThreadEnvMode": env_mode.map(Value::from).unwrap_or(Value::Null),
        "autoPull": auto_pull != 0,
        "scripts": serde_json::from_str::<Value>(scripts_json).unwrap_or(json!([])),
        "createdAt": created_at,
        "updatedAt": updated_at,
        "deletedAt": deleted_at.map(Value::from).unwrap_or(Value::Null),
    })
}

fn read_project_row(conn: &Connection, project_id: &str) -> Option<Value> {
    conn.query_row(
        &format!("SELECT {PROJECT_COLS} FROM projection_projects WHERE project_id = ?1"),
        params![project_id],
        |r| {
            let s = |i: usize| r.get::<_, Option<String>>(i).ok().flatten();
            Ok(project_row_to_json(
                &s(0).unwrap_or_default(),
                &s(1).unwrap_or_default(),
                &s(2).unwrap_or_default(),
                &s(3).unwrap_or_else(|| "[]".into()),
                &s(4).unwrap_or_default(),
                &s(5).unwrap_or_default(),
                s(6).as_deref(),
                s(7).as_deref(),
                s(8).as_deref(),
                s(9).as_deref(),
                r.get::<_, i64>(10).unwrap_or(0),
                s(11).as_deref(),
            ))
        },
    )
    .ok()
}

// ---------------------------------------------------------------------------
// projects.mutate — ProjectMutation {create,update,delete}
// ---------------------------------------------------------------------------

async fn mutate(state: Arc<AppState>, payload: Value) -> RpcResult {
    let ty = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let command_id = payload
        .get("commandId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let project_id = payload
        .get("projectId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    if ty == "project.create" {
        return match state.store.apply_command(payload.clone()).await {
            Err(e) => mutation_error(&command_id, e),
            Ok(applied) => {
                for ev in &applied.events {
                    let _ = state.events_tx.send(ev.clone());
                }
                let home = state.home_dir.clone();
                let pid = project_id.clone();
                let project = tokio::task::spawn_blocking(move || {
                    open_db(&home).and_then(|conn| {
                        read_project_row(&conn, &pid).ok_or_else(|| "row missing".to_string())
                    })
                })
                .await
                .unwrap_or_else(|_| Err("task failed".into()));
                match project {
                    Ok(p) => RpcResult::ok(p),
                    Err(e) => mutation_error(&command_id, e),
                }
            }
        };
    }

    if project_id.is_empty() {
        return mutation_error(&command_id, "Failed to mutate project.");
    }

    let state2 = state.clone();
    let payload2 = payload.clone();
    let cid = command_id.clone();
    let result =
        tokio::task::spawn_blocking(move || -> Result<(Value, Vec<CommittedEvent>), String> {
            let conn = open_db(&state2.home_dir)?;
            let now = domain::now_iso();
            match ty.as_str() {
                "project.update" => {
                    let existing = read_project_row(&conn, &project_id)
                        .ok_or_else(|| "Failed to mutate project.".to_string())?;
                    if !existing.get("deletedAt").unwrap_or(&Value::Null).is_null() {
                        return Err("Failed to mutate project.".to_string());
                    }
                    let get = |k: &str| payload2.get(k);
                    // Only provided fields change; explicit null clears.
                    let mut sets: Vec<String> = Vec::new();
                    let mut vals: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
                    let mut event_payload = Map::new();
                    event_payload.insert("projectId".into(), json!(project_id));
                    if let Some(v) = get("title") {
                        if let Some(t) = v.as_str() {
                            sets.push("title = ?".into());
                            vals.push(Box::new(t.to_string()));
                            event_payload.insert("title".into(), v.clone());
                        }
                    }
                    if let Some(v) = get("workspaceRoot") {
                        if let Some(t) = v.as_str() {
                            sets.push("workspace_root = ?".into());
                            vals.push(Box::new(t.to_string()));
                            event_payload.insert("workspaceRoot".into(), v.clone());
                        }
                    }
                    if payload2.get("defaultModelSelection").is_some() {
                        sets.push("default_model_selection_json = ?".into());
                        let v = payload2.get("defaultModelSelection").cloned().unwrap();
                        vals.push(Box::new(if v.is_null() {
                            None
                        } else {
                            Some(v.to_string())
                        }));
                        event_payload.insert("defaultModelSelection".into(), v);
                    }
                    if payload2.get("autoPull").is_some() {
                        sets.push("auto_pull = ?".into());
                        let v = payload2
                            .get("autoPull")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        vals.push(Box::new(v as i64));
                        event_payload.insert("autoPull".into(), json!(v));
                    }
                    if payload2.get("projectIcon").is_some() {
                        sets.push("project_icon_json = ?".into());
                        let v = payload2.get("projectIcon").cloned().unwrap();
                        vals.push(Box::new(if v.is_null() {
                            None
                        } else {
                            Some(v.to_string())
                        }));
                        event_payload.insert("projectIcon".into(), v);
                    }
                    if payload2.get("faviconPath").is_some() {
                        sets.push("favicon_path = ?".into());
                        let v = payload2.get("faviconPath").cloned().unwrap();
                        vals.push(Box::new(if v.is_null() {
                            None
                        } else {
                            v.as_str().map(str::to_owned)
                        }));
                        event_payload.insert("faviconPath".into(), v);
                    }
                    if payload2.get("defaultThreadEnvMode").is_some() {
                        sets.push("default_thread_env_mode = ?".into());
                        let v = payload2.get("defaultThreadEnvMode").cloned().unwrap();
                        vals.push(Box::new(if v.is_null() {
                            None
                        } else {
                            v.as_str().map(str::to_owned)
                        }));
                        event_payload.insert("defaultThreadEnvMode".into(), v);
                    }
                    if payload2.get("scripts").is_some() {
                        sets.push("scripts_json = ?".into());
                        let v = payload2.get("scripts").cloned().unwrap_or(json!([]));
                        vals.push(Box::new(v.to_string()));
                        event_payload.insert("scripts".into(), v);
                    }
                    sets.push("updated_at = ?".into());
                    vals.push(Box::new(now.clone()));
                    event_payload.insert("updatedAt".into(), json!(now));

                    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
                    let sql = format!(
                        "UPDATE projection_projects SET {} WHERE project_id = ?",
                        sets.join(", ")
                    );
                    let mut params_vec: Vec<&dyn rusqlite::ToSql> = vals
                        .iter()
                        .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
                        .collect();
                    params_vec.push(&project_id);
                    tx.execute(&sql, rusqlite::params_from_iter(params_vec))
                        .map_err(|e| e.to_string())?;
                    let event = json!({
                        "id": domain::new_id(),
                        "type": "project.meta-updated",
                        "occurredAt": now,
                        "payload": Value::Object(event_payload),
                    });
                    let stored = append_project_event(
                        &tx,
                        "project",
                        &project_id,
                        &event,
                        if cid.is_empty() { None } else { Some(&cid) },
                    )?;
                    tx.commit().map_err(|e| e.to_string())?;
                    let project = read_project_row(&conn, &project_id)
                        .ok_or_else(|| "Failed to mutate project.".to_string())?;
                    Ok((project, vec![stored]))
                }
                "project.delete" => {
                    let existing = read_project_row(&conn, &project_id)
                        .ok_or_else(|| "Failed to mutate project.".to_string())?;
                    if !existing.get("deletedAt").unwrap_or(&Value::Null).is_null() {
                        return Ok((existing, vec![]));
                    }
                    let force = payload2.get("force").and_then(Value::as_bool) == Some(true);
                    let mut stmt = conn
                    .prepare(
                        "SELECT thread_id, payload_json FROM orchestration_v2_projection_threads \
                         WHERE project_id = ?1 AND deleted_at IS NULL",
                    )
                    .map_err(|e| e.to_string())?;
                    let threads: Vec<(String, String)> = stmt
                        .query_map(params![project_id], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                        })
                        .map_err(|e| e.to_string())?
                        .filter_map(|r| r.ok())
                        .collect();
                    drop(stmt);
                    if !threads.is_empty() && !force {
                        return Err(format!("Project {project_id} is not empty."));
                    }
                    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
                    let mut committed: Vec<CommittedEvent> = Vec::new();
                    for (tid, pj) in &threads {
                        let mut app: Value = serde_json::from_str(pj).unwrap_or_else(|_| json!({}));
                        app["updatedAt"] = json!(now);
                        app["deletedAt"] = json!(now);
                        tx.execute(
                            "UPDATE orchestration_v2_projection_threads \
                         SET payload_json = ?1, updated_at = ?2, deleted_at = ?2 \
                         WHERE thread_id = ?3",
                            params![app.to_string(), now, tid],
                        )
                        .map_err(|e| e.to_string())?;
                        let ev = domain::thread_event(
                            tid,
                            "thread.deleted",
                            app,
                            None,
                            None,
                            None,
                            None,
                            None,
                        );
                        committed.push(append_project_event(
                            &tx,
                            "thread",
                            tid,
                            &ev,
                            if cid.is_empty() { None } else { Some(&cid) },
                        )?);
                    }
                    tx.execute(
                        "UPDATE projection_projects SET deleted_at = ?1, updated_at = ?1 \
                     WHERE project_id = ?2",
                        params![now, project_id],
                    )
                    .map_err(|e| e.to_string())?;
                    let event = json!({
                        "id": domain::new_id(),
                        "type": "project.deleted",
                        "occurredAt": now,
                        "payload": {"projectId": project_id, "deletedAt": now},
                    });
                    committed.push(append_project_event(
                        &tx,
                        "project",
                        &project_id,
                        &event,
                        if cid.is_empty() { None } else { Some(&cid) },
                    )?);
                    tx.commit().map_err(|e| e.to_string())?;
                    let project = read_project_row(&conn, &project_id)
                        .ok_or_else(|| "Failed to mutate project.".to_string())?;
                    Ok((project, committed))
                }
                _ => Err("Failed to mutate project.".to_string()),
            }
        })
        .await
        .unwrap_or_else(|_| Err("task failed".into()));

    match result {
        Err(e) => mutation_error(&command_id, e),
        Ok((project, events)) => {
            for ev in events {
                let _ = state.events_tx.send(ev);
            }
            RpcResult::ok(project)
        }
    }
}

// ---------------------------------------------------------------------------
// projects.ensureScratch / createNew — ManagedProjectFolders
// ---------------------------------------------------------------------------

/// Apply a `project.create` command through the store and broadcast events.
async fn create_project_via_store(
    state: &AppState,
    command: Value,
) -> Result<crate::store::ApplyResult, String> {
    let applied = state.store.apply_command(command).await?;
    for ev in &applied.events {
        let _ = state.events_tx.send(ev.clone());
    }
    Ok(applied)
}

async fn ensure_scratch(state: Arc<AppState>) -> RpcResult {
    // Scratch inside a checkout inherits git state; refuse like the TS server.
    if let Some(res) = run_cmd(
        "git",
        &["rev-parse", "--show-toplevel"],
        &state.home_dir,
        None,
    )
    .await
    {
        if res.code == 0 {
            let repo = String::from_utf8_lossy(&res.stdout).trim().to_string();
            let base = path_str(&state.home_dir);
            return dispatch_error(format!(
                "Threads without a project are off because the T3 home {base} is inside the \
                 Git repository {repo}. If that repository was created by mistake, move \
                 {repo}/.git out of the way; otherwise point T3CODE_HOME (or --base-dir) at a \
                 folder outside it. Then restart T3 Code."
            ));
        }
    }
    let scratch_root = state.home_dir.join("scratch");
    if let Err(e) = std::fs::create_dir_all(&scratch_root) {
        return dispatch_error(format!(
            "Failed to create the folder for threads without a project: {e}"
        ));
    }
    let scratch_str = path_str(&scratch_root);
    for p in state.store.list_projects().await {
        if p.get("workspaceRoot").and_then(Value::as_str) == Some(scratch_str.as_str()) {
            if let Some(id) = p.get("id").and_then(Value::as_str) {
                return RpcResult::ok(json!({"projectId": id}));
            }
        }
    }
    let id = domain::new_id();
    let command = json!({
        "type": "project.create",
        "commandId": format!("scratch-project:{id}"),
        "projectId": id,
        "title": "No project",
        "workspaceRoot": scratch_str,
    });
    if let Err(e) = create_project_via_store(&state, command).await {
        return dispatch_error(format!(
            "Failed to create the project for threads without a project: {e}"
        ));
    }
    // Set once at create; a user's own icon choice is never overwritten.
    let icon = json!({"kind": "lucide", "name": "message-square-dashed", "color": "gray"});
    let _ = mutate(
        state.clone(),
        json!({
            "type": "project.update",
            "commandId": format!("scratch-project-icon:{id}"),
            "projectId": id,
            "projectIcon": icon,
        }),
    )
    .await;
    RpcResult::ok(json!({"projectId": id}))
}

/// `newProjectFolderName` (NFKD diacritic stripping is approximated by
/// dropping non-ASCII characters).
fn new_project_folder_name(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.chars().flat_map(|c| c.to_lowercase()) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug: String = slug.chars().take(64).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        return "project".to_string();
    }
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    if RESERVED.contains(&slug) {
        format!("{slug}-project")
    } else {
        slug.to_string()
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const ICON_BACKGROUNDS: &[&str] = &[
    "#dc2626", "#ea580c", "#d97706", "#16a34a", "#059669", "#0d9488", "#0891b2", "#0284c7",
    "#2563eb", "#4f46e5", "#7c3aed", "#9333ea", "#c026d3", "#db2777", "#e11d48",
];

fn named_project_icon_svg(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let initials: String = words
        .iter()
        .take(2)
        .filter_map(|w| w.chars().next())
        .collect::<String>()
        .to_uppercase();
    let initials = if initials.is_empty() {
        name.trim()
            .chars()
            .next()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".into())
    } else {
        initials
    };
    let mut hash: u32 = 0;
    for c in name.chars() {
        hash = hash.wrapping_mul(31).wrapping_add(c as u32);
    }
    let bg = ICON_BACKGROUNDS[(hash as usize) % ICON_BACKGROUNDS.len()];
    let size = if initials.chars().count() > 1 { 26 } else { 32 };
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\">\n  \
         <rect width=\"64\" height=\"64\" rx=\"14\" fill=\"{bg}\"/>\n  \
         <text x=\"32\" y=\"32\" dy=\"0.35em\" text-anchor=\"middle\" \
         font-family=\"ui-sans-serif, system-ui, -apple-system, sans-serif\" \
         font-size=\"{size}\" font-weight=\"600\" fill=\"#ffffff\">{}</text>\n</svg>\n",
        escape_xml(&initials)
    )
}

fn named_project_readme(name: &str) -> String {
    format!(
        "<img src=\"assets/icon.svg\" width=\"64\" height=\"64\" alt=\"\">\n\n# {name}\n\nCreated in [T3 Code](https://t3.codes).\n"
    )
}

fn describe_commit_failure(stderr: &str) -> String {
    let lower = stderr.to_lowercase();
    if lower.contains("identity unknown")
        || lower.contains("tell me who you are")
        || lower.contains("no name was given")
        || lower.contains("no email was given")
    {
        return "Git has no name or email on this machine. Set user.name and user.email, then commit.".into();
    }
    stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .last()
        .unwrap_or("Git could not make the first commit.")
        .to_string()
}

async fn create_new(state: Arc<AppState>, payload: Value) -> RpcResult {
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() {
        return dispatch_error("Failed to create the project: missing name.");
    }
    let projects_root = state.home_dir.join("projects");
    if let Err(e) = std::fs::create_dir_all(&projects_root) {
        return dispatch_error(format!("Failed to create the project folder: {e}"));
    }
    let slug = new_project_folder_name(&name);
    let mut claimed: Option<PathBuf> = None;
    for attempt in 1..=MAX_NAMED_FOLDER_ATTEMPTS {
        let folder = if attempt == 1 {
            projects_root.join(&slug)
        } else {
            projects_root.join(format!("{slug}-{attempt}"))
        };
        match std::fs::create_dir(&folder) {
            Ok(()) => {
                claimed = Some(folder);
                break;
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return dispatch_error(format!(
                    "Failed to create the project folder {}: {e}",
                    path_str(&folder)
                ))
            }
        }
    }
    let Some(workspace_root) = claimed else {
        return dispatch_error(format!(
            "Failed to create the project folder: every folder name for \"{slug}\" is taken."
        ));
    };
    let root_str = path_str(&workspace_root);

    // scaffoldRepository: git init + README + icon + initial commit.
    let branch = run_cmd(
        "git",
        &["config", "--get", "init.defaultBranch"],
        &workspace_root,
        None,
    )
    .await
    .filter(|r| r.code == 0)
    .map(|r| String::from_utf8_lossy(&r.stdout).trim().to_string())
    .filter(|b| !b.is_empty())
    .unwrap_or_else(|| "main".to_string());
    if let Some(r) = run_cmd(
        "git",
        &["init", &format!("--initial-branch={branch}")],
        &workspace_root,
        None,
    )
    .await
    {
        if r.code != 0 {
            let _ = std::fs::remove_dir_all(&workspace_root);
            return dispatch_error(format!(
                "Failed to create the project folder: {}",
                String::from_utf8_lossy(&r.stderr).trim()
            ));
        }
    }
    let _ = std::fs::write(
        workspace_root.join("README.md"),
        named_project_readme(&name),
    );
    let _ = std::fs::create_dir_all(workspace_root.join("assets"));
    let _ = std::fs::write(
        workspace_root.join("assets/icon.svg"),
        named_project_icon_svg(&name),
    );
    let _ = run_cmd(
        "git",
        &[
            "-c",
            "core.safecrlf=false",
            "add",
            "--force",
            "--",
            "README.md",
            "assets/icon.svg",
        ],
        &workspace_root,
        None,
    )
    .await;
    let commit_error = match run_cmd(
        "git",
        &["commit", "--message", "Initial commit"],
        &workspace_root,
        None,
    )
    .await
    {
        Some(r) if r.code == 0 => None,
        Some(r) => Some(describe_commit_failure(&String::from_utf8_lossy(&r.stderr))),
        None => Some("Git could not make the first commit.".to_string()),
    };

    let id = domain::new_id();
    let command = json!({
        "type": "project.create",
        "commandId": format!("named-project:{id}"),
        "projectId": id,
        "title": name,
        "workspaceRoot": root_str,
    });
    if let Err(e) = create_project_via_store(&state, command).await {
        let _ = std::fs::remove_dir_all(&workspace_root);
        return dispatch_error(format!("Failed to create the project: {e}"));
    }
    let mut out = json!({"projectId": id, "workspaceRoot": root_str});
    if let Some(e) = commit_error {
        out["commitError"] = json!(e);
    }
    RpcResult::ok(out)
}
