//! `pullRequests.*`, `review.*`, `sourceControl.*` (and the pull-request-shaped
//! `git.*`) RPCs for the Rust server.
//!
//! Every read/write goes through the local `gh` (GitHub CLI) and `git`
//! binaries — the same tools the TS server's source-control host shells out to.
//! `gh` may be absent or unauthenticated: failures are mapped onto the
//! contract's tagged error shapes (`PullRequestUnavailableError`,
//! `PullRequestOperationError`, `GitCommandError`, …) instead of panicking or
//! returning a generic `RpcClientError`, so the web client can render its
//! "source control unavailable" states.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::domain;
use crate::handlers::RpcResult;
use crate::AppState;

const GH: &str = "gh";
const GIT: &str = "git";
const CMD_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_HOST: &str = "github.com";
/// Per-pull-request reads cap out below GitHub's own page sizes.
const PAGE_SIZE: u32 = 100;

// ---------------------------------------------------------------------------
// Error shapes (must match packages/contracts TaggedError encodings)
// ---------------------------------------------------------------------------

fn rpc_client_error(message: impl Into<String>) -> Value {
    json!({ "_tag": "RpcClientError", "message": message.into() })
}

fn not_implemented(tag: &str) -> RpcResult {
    RpcResult::err(rpc_client_error(format!("not implemented: {tag}")))
}

fn pr_unavailable(reason: &str) -> Value {
    let requirement = match reason {
        "cli-missing" => {
            "No GitHub credential on the server. Set GH_TOKEN, or install the GitHub CLI (https://cli.github.com/) and run `gh auth login`."
        }
        "cli-unauthenticated" => {
            "GitHub has no working credential for this host. Run `gh auth login`, or check the account and hosts in Settings → Source Control."
        }
        _ => "Change requests cannot be browsed for this project's host yet.",
    };
    json!({
        "_tag": "PullRequestUnavailableError",
        "reason": reason,
        "provider": "github",
        "requirement": requirement,
    })
}

fn pr_op_error(operation: &str, detail: impl Into<String>) -> Value {
    json!({
        "_tag": "PullRequestOperationError",
        "operation": operation,
        "detail": detail.into(),
    })
}

fn git_command_error(
    operation: &str,
    command: &str,
    cwd: &str,
    exit_code: Option<i32>,
    detail: impl Into<String>,
) -> Value {
    let mut e = json!({
        "_tag": "GitCommandError",
        "operation": operation,
        "command": command,
        "cwd": cwd,
        "detail": detail.into(),
    });
    if let Some(code) = exit_code {
        e["exitCode"] = json!(code);
    }
    e
}

fn git_manager_error(operation: &str, cwd: &str, detail: impl Into<String>) -> Value {
    json!({
        "_tag": "GitManagerError",
        "operation": operation,
        "cwd": cwd,
        "detail": detail.into(),
    })
}

fn sc_repo_error(operation: &str, detail: impl Into<String>) -> Value {
    json!({
        "_tag": "SourceControlRepositoryError",
        "provider": "github",
        "operation": operation,
        "detail": detail.into(),
    })
}

fn sign_in_error(host: &str, reason: &str, detail: impl Into<String>) -> Value {
    json!({
        "_tag": "SourceControlSignInError",
        "provider": "github",
        "host": host,
        "reason": reason,
        "detail": detail.into(),
    })
}

fn vcs_unsupported(operation: &str, detail: impl Into<String>) -> Value {
    json!({
        "_tag": "VcsUnsupportedOperationError",
        "operation": operation,
        "kind": "unknown",
        "detail": detail.into(),
    })
}

// ---------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------

/// True when gh's stderr names a missing/absent credential rather than a
/// request that failed on its own merits.
fn stderr_is_auth(stderr: &str) -> bool {
    let s = stderr.to_lowercase();
    [
        "gh auth login",
        "not logged into",
        "not logged in",
        "no oauth token",
        "authentication failed",
        "requires authentication",
        "bad credentials",
        "http 401",
        "credential",
    ]
    .iter()
    .any(|needle| s.contains(needle))
}

fn gh_failure(operation: &str, stderr: &str) -> Value {
    let trimmed = stderr.trim();
    if stderr_is_auth(trimmed) {
        return pr_unavailable("cli-unauthenticated");
    }
    let detail: String = trimmed.chars().take(600).collect();
    pr_op_error(
        operation,
        if detail.is_empty() {
            format!("gh exited with a failure status in {operation}")
        } else {
            detail
        },
    )
}

struct CmdOutput {
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: String,
}

/// Spawn `gh`/`git` with a timeout. A missing binary maps onto the caller's
/// missing-tool error (`cli-missing` for gh, `GitCommandError` for git).
async fn run_cmd(
    program: &str,
    args: &[String],
    cwd: &str,
    host: Option<&str>,
    stdin: Option<&[u8]>,
    operation: &str,
) -> Result<CmdOutput, Value> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(if cwd.is_empty() { "/" } else { cwd })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    } else {
        cmd.stdin(std::process::Stdio::null());
    }
    if program == GH {
        if let Some(h) = host {
            if !h.eq_ignore_ascii_case(DEFAULT_HOST) {
                cmd.env("GH_HOST", h);
            }
        }
    }
    // Kill-on-drop covers the timeout branch below: the future owning `child`
    // is dropped on timeout, which then kills the still-running process.
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            if program == GH {
                pr_unavailable("cli-missing")
            } else {
                git_command_error(operation, "git", cwd, None, "git is not installed")
            }
        } else if program == GH {
            pr_op_error(operation, format!("gh failed to start: {e}"))
        } else {
            git_command_error(operation, "git", cwd, None, format!("git failed to start: {e}"))
        }
    })?;
    if let Some(bytes) = stdin {
        use tokio::io::AsyncWriteExt;
        if let Some(mut pipe) = child.stdin.take() {
            let _ = pipe.write_all(bytes).await;
            let _ = pipe.shutdown().await;
        }
    }
    let output = match tokio::time::timeout(CMD_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            if program == GH {
                return Err(pr_op_error(operation, format!("gh failed: {e}")));
            }
            return Err(git_command_error(
                operation,
                "git",
                cwd,
                None,
                format!("git failed: {e}"),
            ));
        }
        Err(_) => {
            if program == GH {
                return Err(pr_op_error(operation, "gh timed out"));
            }
            return Err(git_command_error(
                operation,
                "git",
                cwd,
                None,
                "git timed out",
            ));
        }
    };
    Ok(CmdOutput {
        status: output.status.code(),
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// `gh` returning stdout on success; failures classified as PR errors.
async fn gh<S: AsRef<str>>(
    cwd: &str,
    args: &[S],
    host: Option<&str>,
    operation: &str,
) -> Result<String, Value> {
    gh_stdin(cwd, args, host, None, operation).await
}

async fn gh_stdin<S: AsRef<str>>(
    cwd: &str,
    args: &[S],
    host: Option<&str>,
    stdin: Option<&[u8]>,
    operation: &str,
) -> Result<String, Value> {
    let owned: Vec<String> = args.iter().map(|a| a.as_ref().to_string()).collect();
    let out = run_cmd(GH, &owned, cwd, host, stdin, operation).await?;
    if out.status != Some(0) {
        return Err(gh_failure(operation, &out.stderr));
    }
    String::from_utf8(out.stdout).map_err(|_| pr_op_error(operation, "gh output was not UTF-8"))
}

async fn gh_json<S: AsRef<str>>(
    cwd: &str,
    args: &[S],
    host: Option<&str>,
    operation: &str,
) -> Result<Value, Value> {
    let text = gh(cwd, args, host, operation).await?;
    serde_json::from_str(&text)
        .map_err(|e| pr_op_error(operation, format!("gh returned unexpected output: {e}")))
}

/// `git` returning stdout on success, `GitCommandError` on failure.
async fn git<S: AsRef<str>>(cwd: &str, args: &[S], operation: &str) -> Result<String, Value> {
    let owned: Vec<String> = args.iter().map(|a| a.as_ref().to_string()).collect();
    let cmdline = format!("git {}", owned.join(" "));
    let out = run_cmd(GIT, &owned, cwd, None, None, operation).await?;
    if out.status == Some(0) {
        String::from_utf8(out.stdout)
            .map_err(|_| git_command_error(operation, &cmdline, cwd, None, "git output was not UTF-8"))
    } else {
        let detail = out.stderr.trim().to_string();
        Err(git_command_error(
            operation,
            &cmdline,
            cwd,
            out.status,
            if detail.is_empty() {
                format!("git exited with status {:?}", out.status)
            } else {
                detail.chars().take(600).collect()
            },
        ))
    }
}

/// `git` without the success check (exit-code-driven flows).
async fn git_out<S: AsRef<str>>(cwd: &str, args: &[S], operation: &str) -> Result<CmdOutput, Value> {
    let owned: Vec<String> = args.iter().map(|a| a.as_ref().to_string()).collect();
    run_cmd(GIT, &owned, cwd, None, None, operation).await
}

// ---------------------------------------------------------------------------
// Project / pull-request reference resolution
// ---------------------------------------------------------------------------

struct ProjectInfo {
    id: String,
    title: String,
    workspace_root: String,
}

async fn project_info(state: &AppState, project_id: &str) -> Result<ProjectInfo, Value> {
    match state.store.project_shell(project_id).await {
        Some(p) => Ok(ProjectInfo {
            id: p
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(project_id)
                .to_string(),
            title: p
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(project_id)
                .to_string(),
            workspace_root: p
                .get("workspaceRoot")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }),
        None => Err(pr_op_error(
            "resolveProject",
            format!("Project {project_id} was not found."),
        )),
    }
}

async fn all_projects(state: &AppState) -> Vec<ProjectInfo> {
    state
        .store
        .list_projects()
        .await
        .iter()
        .map(|p| ProjectInfo {
            id: p.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            title: p.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
            workspace_root: p
                .get("workspaceRoot")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        })
        .filter(|p| !p.id.is_empty() && !p.workspace_root.is_empty())
        .collect()
}

/// The `PullRequestRef` payload fields every `pullRequests.<n>` method shares.
struct PrRef {
    project_id: String,
    host: Option<String>,
    /// `owner/repo` — required by the contract.
    repository: String,
    number: i64,
}

fn pr_ref(payload: &Value) -> Result<PrRef, Value> {
    let project_id = payload
        .get("projectId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let repository = payload
        .get("repository")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let number = payload.get("number").and_then(Value::as_i64).unwrap_or(0);
    if project_id.is_empty() || repository.is_empty() || number <= 0 {
        return Err(pr_op_error(
            "parseRef",
            "Pull request reference requires projectId, repository and number.",
        ));
    }
    Ok(PrRef {
        project_id,
        host: payload
            .get("host")
            .and_then(Value::as_str)
            .map(str::to_owned),
        repository,
        number,
    })
}

struct ResolvedRef {
    pr: PrRef,
    project: ProjectInfo,
}

async fn resolved_ref(state: &AppState, payload: &Value) -> Result<ResolvedRef, Value> {
    let pr = pr_ref(payload)?;
    let project = project_info(state, &pr.project_id).await?;
    if project.workspace_root.is_empty() {
        return Err(pr_op_error(
            "resolveProject",
            format!("Project {} has no workspace root.", project.id),
        ));
    }
    Ok(ResolvedRef { pr, project })
}

fn host_of(r: &ResolvedRef) -> Option<&str> {
    r.pr.host.as_deref()
}

fn effective_host(pr: &PrRef) -> String {
    pr.host.clone().unwrap_or_else(|| DEFAULT_HOST.into())
}

// ---------------------------------------------------------------------------
// gh JSON → contract value mapping
// ---------------------------------------------------------------------------

fn actor(v: &Value) -> Value {
    let login = v
        .get("login")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if login.is_empty() {
        return Value::Null;
    }
    let name = v.get("name").and_then(Value::as_str).unwrap_or("");
    let is_bot = v.get("is_bot").and_then(Value::as_bool) == Some(true)
        || login.ends_with("[bot]")
        || v.get("__typename").and_then(Value::as_str) == Some("Bot");
    json!({
        "login": login,
        "name": if name.is_empty() { Value::Null } else { json!(name) },
        "avatarUrl": Value::Null,
        "isBot": is_bot,
    })
}

fn actor_list(v: Option<&Value>) -> Vec<Value> {
    v.and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(actor)
        .filter(|a| !a.is_null())
        .collect()
}

fn label(v: &Value) -> Value {
    json!({
        "name": v.get("name").and_then(Value::as_str).unwrap_or(""),
        "color": v.get("color").cloned().unwrap_or(Value::Null),
        "description": v.get("description").cloned().unwrap_or(Value::Null),
    })
}

fn label_list(v: Option<&Value>) -> Vec<Value> {
    v.and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(label)
        .collect()
}

/// gh's `state` is `OPEN|CLOSED|MERGED`; the contract wants lowercase.
fn pr_state_of(v: &Value) -> String {
    v.get("state")
        .and_then(Value::as_str)
        .unwrap_or("OPEN")
        .to_ascii_lowercase()
}

/// `reviewDecision` for `NullOr` fields; `None` = omit the key entirely
/// (the list-entry field is `Schema.optional`, which rejects an explicit null).
fn review_decision(v: &Value) -> Option<&'static str> {
    match v.get("reviewDecision").and_then(Value::as_str) {
        Some("APPROVED") => Some("approved"),
        Some("CHANGES_REQUESTED") => Some("changes-requested"),
        Some("REVIEW_REQUIRED") => Some("review-required"),
        _ => None,
    }
}

fn mergeability(v: &Value) -> String {
    match v.get("mergeable").and_then(Value::as_str) {
        Some("MERGEABLE") => "mergeable".into(),
        Some("CONFLICTING") => "conflicting".into(),
        _ => "unknown".into(),
    }
}

/// `statusCheckRollup` node → `PullRequestCheck`.
fn check_node(v: &Value) -> Option<Value> {
    let ty = v.get("__typename").and_then(Value::as_str).unwrap_or("");
    let (name, url) = match ty {
        "CheckRun" => (
            v.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            v.get("detailsUrl").cloned().unwrap_or(Value::Null),
        ),
        "StatusContext" => (
            v.get("context").and_then(Value::as_str).unwrap_or("").to_string(),
            v.get("targetUrl").cloned().unwrap_or(Value::Null),
        ),
        _ => return None,
    };
    if name.is_empty() {
        return None;
    }
    let status = if ty == "CheckRun" {
        match v.get("status").and_then(Value::as_str) {
            Some("COMPLETED") => match v.get("conclusion").and_then(Value::as_str) {
                Some("SUCCESS") => "success",
                Some("NEUTRAL") => "neutral",
                Some("CANCELLED") => "cancelled",
                Some("SKIPPED") | Some("STALE") => "skipped",
                Some("ACTION_REQUIRED") => "action-required",
                Some("FAILURE") | Some("TIMED_OUT") | Some("STARTUP_FAILURE") | Some("ERROR") => {
                    "failure"
                }
                _ => "pending",
            },
            _ => "pending",
        }
    } else {
        match v.get("state").and_then(Value::as_str) {
            Some("SUCCESS") => "success",
            Some("FAILURE") | Some("ERROR") => "failure",
            _ => "pending",
        }
    };
    Some(json!({
        "name": name,
        "status": status,
        "description": v.get("description").cloned().unwrap_or(Value::Null),
        "url": url,
    }))
}

fn check_list(v: Option<&Value>) -> Vec<Value> {
    v.and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(check_node)
        .collect()
}

/// `gh pr view --json` fields shared by the per-PR reads.
const PR_VIEW_FIELDS: &[&str] = &[
    "number",
    "title",
    "body",
    "url",
    "state",
    "isDraft",
    "author",
    "headRefName",
    "baseRefName",
    "headRefOid",
    "baseRefOid",
    "headRepository",
    "headRepositoryOwner",
    "createdAt",
    "updatedAt",
    "closedAt",
    "mergedAt",
    "additions",
    "deletions",
    "changedFiles",
    "labels",
    "reviewRequests",
    "reviewDecision",
    "mergeable",
    "maintainerCanModify",
    "autoMergeRequest",
    "statusCheckRollup",
];

async fn gh_pr_view(r: &ResolvedRef, fields: &[&str], operation: &str) -> Result<Value, Value> {
    let number = r.pr.number.to_string();
    let joined = fields.join(",");
    let args = vec!["pr".into(), "view".into(), number, "--json".into(), joined];
    gh_json(&r.project.workspace_root, &args, host_of(r), operation).await
}

fn num(v: &Value) -> i64 {
    v.as_i64().unwrap_or(0)
}

/// `gh pr view` row → `PullRequestSummary`. Optional contract fields that are
/// absent in the host response are omitted, never sent as null.
fn summary_value(r: &ResolvedRef, v: &Value) -> Value {
    let mut out = json!({
        "provider": "github",
        "projectId": r.project.id.as_str(),
        "repository": r.pr.repository.as_str(),
        "number": r.pr.number,
        "title": v.get("title").and_then(Value::as_str).unwrap_or(""),
        "url": v.get("url").and_then(Value::as_str).unwrap_or(""),
        "state": pr_state_of(v),
        "isDraft": v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        "headBranch": v.get("headRefName").and_then(Value::as_str).unwrap_or(""),
        "baseBranch": v.get("baseRefName").and_then(Value::as_str).unwrap_or(""),
        "closedAt": v.get("closedAt").cloned().unwrap_or(Value::Null),
        "mergedAt": v.get("mergedAt").cloned().unwrap_or(Value::Null),
        "updatedAt": v.get("updatedAt").and_then(Value::as_str).unwrap_or(""),
        "author": actor(v.get("author").unwrap_or(&Value::Null)),
        "additions": num(v.get("additions").unwrap_or(&Value::Null)),
        "deletions": num(v.get("deletions").unwrap_or(&Value::Null)),
        "changedFiles": num(v.get("changedFiles").unwrap_or(&Value::Null)),
        "reviewDecision": review_decision(v).map(Value::from).unwrap_or(Value::Null),
        "mergeability": mergeability(v),
    });
    let head_sha = v.get("headRefOid").and_then(Value::as_str).unwrap_or("");
    if !head_sha.is_empty() {
        out["headSha"] = json!(head_sha);
    }
    let auto_merge = v.get("autoMergeRequest");
    if auto_merge.is_some_and(|a| !a.is_null()) {
        out["autoMergeEnabled"] = json!(true);
        if let Some(m) = auto_merge
            .and_then(|a| a.get("mergeMethod"))
            .and_then(Value::as_str)
        {
            out["autoMergeMethod"] = json!(m.to_ascii_lowercase());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// pullRequests.* reads
// ---------------------------------------------------------------------------

async fn list_pull_requests(state: Arc<AppState>, payload: Value) -> RpcResult {
    let state_filter = payload
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("open")
        .to_string();
    let limit = payload
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(50)
        .clamp(1, 500);

    // Resolve the project scope: explicit ids win over `projectId`, absent means all.
    let mut ids: Vec<String> = payload
        .get("projectIds")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if let Some(one) = payload.get("projectId").and_then(Value::as_str) {
        if !ids.iter().any(|i| i == one) {
            ids.push(one.to_string());
        }
    }
    let projects: Vec<ProjectInfo> = if ids.is_empty() {
        all_projects(&state).await
    } else {
        let mut out = Vec::new();
        for id in &ids {
            if let Ok(p) = project_info(&state, id).await {
                out.push(p);
            }
        }
        out
    };

    let mut entries: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut first_unavailable: Option<Value> = None;
    let mut host_counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut any_ok = false;

    for project in &projects {
        // `gh pr list` state + limit + search qualifiers.
        let mut search_terms: Vec<String> = Vec::new();
        if let Some(q) = payload.get("query").and_then(Value::as_str) {
            search_terms.push(q.to_string());
        }
        if let Some(filters) = payload.get("filters") {
            match filters.get("draft").and_then(Value::as_str) {
                Some("only") => search_terms.push("is:draft".into()),
                Some("hide") => search_terms.push("-is:draft".into()),
                _ => {}
            }
            match filters.get("review").and_then(Value::as_str) {
                Some("approved") => search_terms.push("review:approved".into()),
                Some("changes-requested") => {
                    search_terms.push("review:changes_requested".into())
                }
                Some("review-required") => search_terms.push("review:required".into()),
                Some("none") => search_terms.push("review:none".into()),
                _ => {}
            }
            match filters.get("checks").and_then(Value::as_str) {
                Some("passing") => search_terms.push("status:success".into()),
                Some("failing") => search_terms.push("status:failure".into()),
                _ => {}
            }
            if let Some(groups) = filters.get("labels").and_then(Value::as_array) {
                for group in groups {
                    let names: Vec<String> = group
                        .as_array()
                        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                        .unwrap_or_default();
                    if !names.is_empty() {
                        search_terms.push(format!("label:{}", names.join(",")));
                    }
                }
            }
            if let Some(excluded) = filters.get("excludedLabels").and_then(Value::as_array) {
                for name in excluded.iter().filter_map(Value::as_str) {
                    search_terms.push(format!("-label:{name}"));
                }
            }
            if let Some(author) = filters.get("author").and_then(Value::as_str) {
                search_terms.push(format!("author:{author}"));
            }
        }
        match payload.get("involvement").and_then(Value::as_str) {
            Some("authored") => search_terms.push("author:@me".into()),
            Some("reviewing") => search_terms.push("review-requested:@me".into()),
            _ => {}
        }

        let mut args: Vec<String> = vec![
            "pr".into(),
            "list".into(),
            "--json".into(),
            "number,title,state,author,headRefName,baseRefName,url,updatedAt,createdAt,isDraft,labels,reviewDecision,reviewRequests".into(),
            "--limit".into(),
            limit.to_string(),
            "--state".into(),
            state_filter.clone(),
        ];
        if !search_terms.is_empty() {
            args.push("--search".into());
            args.push(search_terms.join(" "));
        }
        let host_filter = payload.get("host").and_then(Value::as_str);

        let rows = match gh_json(&project.workspace_root, &args, host_filter, "list").await {
            Ok(v) => {
                any_ok = true;
                v
            }
            Err(e) => {
                let unavailable = e
                    .get("_tag")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t == "PullRequestUnavailableError");
                if unavailable {
                    first_unavailable.get_or_insert(e);
                    continue;
                }
                errors.push(json!({
                    "projectId": project.id.as_str(),
                    "projectTitle": project.title.as_str(),
                    "message": e.get("detail").and_then(Value::as_str)
                        .or_else(|| e.get("message").and_then(Value::as_str))
                        .unwrap_or("Pull request listing failed."),
                }));
                continue;
            }
        };

        // Repository identity: prefer gh's nameWithOwner, else the git remote.
        let repository = gh_json(
            &project.workspace_root,
            &["repo", "view", "--json", "nameWithOwner"],
            host_filter,
            "list",
        )
        .await
        .ok()
        .and_then(|v| {
            v.get("nameWithOwner")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| remote_repo_slug(&project.workspace_root))
        .unwrap_or_else(|| "unknown/unknown".into());
        let repo_host = host_filter.unwrap_or(DEFAULT_HOST).to_string();
        *host_counts.entry(repo_host.clone()).or_insert(0) += 1;

        for row in rows.as_array().cloned().unwrap_or_default() {
            let mut entry = json!({
                "provider": "github",
                "host": repo_host.as_str(),
                "projectId": project.id.as_str(),
                "projectTitle": project.title.as_str(),
                "repository": repository.as_str(),
                "number": num(row.get("number").unwrap_or(&Value::Null)),
                "title": row.get("title").and_then(Value::as_str).unwrap_or(""),
                "url": row.get("url").and_then(Value::as_str).unwrap_or(""),
                "author": actor(row.get("author").unwrap_or(&Value::Null)),
                "headBranch": row.get("headRefName").and_then(Value::as_str).unwrap_or(""),
                "baseBranch": row.get("baseRefName").and_then(Value::as_str).unwrap_or(""),
                "state": pr_state_of(&row),
                "isDraft": row.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
                "mergeability": "unknown",
                "additions": 0,
                "deletions": 0,
                "createdAt": row.get("createdAt").and_then(Value::as_str).unwrap_or(""),
                "updatedAt": row.get("updatedAt").and_then(Value::as_str).unwrap_or(""),
                "viewerReviewRequested": row
                    .get("reviewRequests")
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty()),
                "labels": label_list(row.get("labels")),
            });
            if let Some(d) = review_decision(&row) {
                entry["reviewDecision"] = json!(d);
            }
            entries.push(entry);
        }
    }

    if !any_ok && !projects.is_empty() {
        if let Some(e) = first_unavailable {
            return RpcResult::err(e);
        }
        if entries.is_empty() && !errors.is_empty() {
            return RpcResult::err(pr_op_error(
                "list",
                "Pull request listing failed for every project.",
            ));
        }
    }

    entries.sort_by(|a, b| {
        let ua = a.get("updatedAt").and_then(Value::as_str).unwrap_or("");
        let ub = b.get("updatedAt").and_then(Value::as_str).unwrap_or("");
        ub.cmp(ua)
    });

    let providers: Vec<Value> = host_counts
        .iter()
        .map(|(host, count)| {
            json!({
                "host": host.as_str(),
                "kind": "github",
                "searchesOnHost": true,
                "projectCount": *count,
                "configured": true,
                "detail": Value::Null,
            })
        })
        .collect();

    RpcResult::ok(json!({
        "viewers": {},
        "providers": providers,
        "entries": entries,
        "errors": errors,
        "truncated": false,
        "nextCursors": {},
    }))
}

/// `owner/repo` from the project's git remote; `None` when undetectable.
fn remote_repo_slug(workspace_root: &str) -> Option<String> {
    let out = std::process::Command::new(GIT)
        .args(["remote", "get-url", "origin"])
        .current_dir(workspace_root)
        .output()
        .ok()?;
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    parse_github_remote(&url).map(|(_, slug)| slug)
}

/// `git@github.com:owner/repo(.git)` or `https://host/owner/repo(.git)` → (host, owner/repo).
fn parse_github_remote(url: &str) -> Option<(String, String)> {
    if let Some(rest) = url.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        let slug = path.trim_end_matches(".git").trim_end_matches('/');
        return Some((host.to_string(), slug.to_string()));
    }
    let stripped = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("ssh://git@"))?;
    let (host, path) = stripped.split_once('/')?;
    let slug = path.trim_end_matches(".git").trim_end_matches('/');
    Some((
        host.split(':').next().unwrap_or(host).to_string(),
        slug.to_string(),
    ))
}

/// Hostname portion of an http(s) URL.
fn url_hostname(url: &str) -> Option<String> {
    url.split("://")
        .nth(1)
        .and_then(|s| s.split('/').next())
        .map(|h| h.split(':').next().unwrap_or(h).to_string())
}

async fn list_stats(state: Arc<AppState>, payload: Value) -> RpcResult {
    let refs = payload
        .get("refs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut stats = Vec::new();
    let mut first_err: Option<Value> = None;
    for r in refs {
        let rr = match resolved_ref(&state, &r).await {
            Ok(v) => v,
            Err(e) => {
                first_err.get_or_insert(e);
                continue;
            }
        };
        match gh_pr_view(&rr, &["additions", "deletions"], "listStats").await {
            Ok(v) => stats.push(json!({
                "projectId": rr.project.id.as_str(),
                "repository": rr.pr.repository.as_str(),
                "number": rr.pr.number,
                "additions": num(v.get("additions").unwrap_or(&Value::Null)),
                "deletions": num(v.get("deletions").unwrap_or(&Value::Null)),
            })),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    if stats.is_empty() {
        if let Some(e) = first_err {
            return RpcResult::err(e);
        }
    }
    RpcResult::ok(json!({ "stats": stats }))
}

async fn filter_options(state: Arc<AppState>, payload: Value) -> RpcResult {
    let kind = payload.get("kind").and_then(Value::as_str).unwrap_or("labels");
    let query = payload
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();

    let mut ids: Vec<String> = payload
        .get("projectIds")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    if let Some(one) = payload.get("projectId").and_then(Value::as_str) {
        if !ids.iter().any(|i| i == one) {
            ids.push(one.to_string());
        }
    }
    let projects: Vec<ProjectInfo> = if ids.is_empty() {
        all_projects(&state).await
    } else {
        let mut out = Vec::new();
        for id in &ids {
            if let Ok(p) = project_info(&state, id).await {
                out.push(p);
            }
        }
        out
    };

    let mut labels: Vec<Value> = Vec::new();
    let mut authors: Vec<Value> = Vec::new();
    let mut seen_labels = std::collections::HashSet::new();
    let mut seen_authors = std::collections::HashSet::new();

    for project in &projects {
        match kind {
            "labels" => {
                let v = match gh_json(
                    &project.workspace_root,
                    &[
                        "label",
                        "list",
                        "--json",
                        "name,color,description",
                        "--limit",
                        "100",
                    ],
                    payload.get("host").and_then(Value::as_str),
                    "filterOptions",
                )
                .await
                {
                    Ok(v) => v,
                    Err(e) => return RpcResult::err(e),
                };
                for l in v.as_array().cloned().unwrap_or_default() {
                    let name = l.get("name").and_then(Value::as_str).unwrap_or("");
                    if name.is_empty()
                        || (!query.is_empty() && !name.to_lowercase().contains(&query))
                        || !seen_labels.insert(name.to_string())
                    {
                        continue;
                    }
                    labels.push(label(&l));
                }
            }
            "authors" => {
                let v = match gh_json(
                    &project.workspace_root,
                    &[
                        "pr", "list", "--state", "all", "--json", "author", "--limit", "100",
                    ],
                    payload.get("host").and_then(Value::as_str),
                    "filterOptions",
                )
                .await
                {
                    Ok(v) => v,
                    Err(e) => return RpcResult::err(e),
                };
                for row in v.as_array().cloned().unwrap_or_default() {
                    let a = actor(row.get("author").unwrap_or(&Value::Null));
                    let login = a.get("login").and_then(Value::as_str).unwrap_or("");
                    if login.is_empty()
                        || (!query.is_empty() && !login.to_lowercase().contains(&query))
                        || !seen_authors.insert(login.to_string())
                    {
                        continue;
                    }
                    authors.push(a);
                }
            }
            _ => {}
        }
    }
    RpcResult::ok(json!({
        "labels": labels,
        "authors": authors,
        "references": [],
        "truncated": false,
    }))
}

async fn pr_summary(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    match gh_pr_view(&r, PR_VIEW_FIELDS, "summary").await {
        Ok(v) => RpcResult::ok(summary_value(&r, &v)),
        Err(e) => RpcResult::err(e),
    }
}

async fn pr_preview(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    match gh_pr_view(
        &r,
        &[
            "number",
            "title",
            "url",
            "author",
            "state",
            "isDraft",
            "createdAt",
            "updatedAt",
        ],
        "preview",
    )
    .await
    {
        Ok(v) => {
            let mut out = json!({
                "projectId": r.project.id.as_str(),
                "repository": r.pr.repository.as_str(),
                "number": r.pr.number,
                "title": v.get("title").and_then(Value::as_str).unwrap_or(""),
                "url": v.get("url").and_then(Value::as_str).unwrap_or(""),
                "author": actor(v.get("author").unwrap_or(&Value::Null)),
                "state": pr_state_of(&v),
                "isDraft": v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
                "createdAt": v.get("createdAt").and_then(Value::as_str).unwrap_or(""),
            });
            let updated = v.get("updatedAt").and_then(Value::as_str).unwrap_or("");
            if !updated.is_empty() {
                out["updatedAt"] = json!(updated);
            }
            RpcResult::ok(out)
        }
        Err(e) => RpcResult::err(e),
    }
}

fn github_capabilities() -> Value {
    json!({
        "diff": true,
        "comment": true,
        "actions": [
            "merge", "ready", "draft", "close", "reopen", "delete-branch",
            "update-branch", "enable-auto-merge", "disable-auto-merge",
            "revert", "approve-workflows",
        ],
        "mergeMethods": ["merge", "squash", "rebase"],
        "deleteBranch": true,
        "updateMethods": ["merge", "rebase"],
        "search": true,
        "reactions": true,
        "mentions": true,
        "referenceSigils": ["#"],
        "viewedFiles": "environment",
        "review": {
            "inlineComment": true,
            "rangeComment": true,
            "reply": true,
            "resolve": true,
            "verdicts": ["comment", "approve", "request-changes"],
        },
        "reviewers": { "request": true, "listCandidates": true, "searchCandidates": true },
        "edit": { "changeRequest": true, "comment": true, "maintainerEdits": true },
        "labels": true,
        "checksAlwaysReport": true,
    })
}

fn github_viewer_permissions() -> Value {
    json!({
        "stackRebase": true,
        "actions": [
            "merge", "ready", "draft", "close", "reopen", "delete-branch",
            "update-branch", "enable-auto-merge", "disable-auto-merge",
            "revert", "approve-workflows",
        ],
        "comment": true,
        "resolve": true,
        "verdicts": ["comment", "approve", "request-changes"],
        "requestReviewers": true,
        "updateMethods": ["merge", "rebase"],
        "labels": true,
        "editChangeRequest": true,
        "maintainerEdits": true,
    })
}

async fn pr_detail(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let v = match gh_pr_view(&r, PR_VIEW_FIELDS, "detail").await {
        Ok(v) => v,
        Err(e) => return RpcResult::err(e),
    };
    let head_repo = v
        .get("headRepository")
        .and_then(|h| h.get("name"))
        .and_then(Value::as_str);
    let head_owner = v
        .get("headRepositoryOwner")
        .and_then(|h| h.get("login"))
        .and_then(Value::as_str);
    let head_name_with_owner = match (head_owner, head_repo) {
        (Some(o), Some(n)) => Value::String(format!("{o}/{n}")),
        _ => Value::Null,
    };
    let mut out = json!({
        "provider": "github",
        "capabilities": github_capabilities(),
        "viewerPermissions": github_viewer_permissions(),
        "projectId": r.project.id.as_str(),
        "projectTitle": r.project.title.as_str(),
        "workspaceRoot": r.project.workspace_root.as_str(),
        "repository": r.pr.repository.as_str(),
        "number": r.pr.number,
        "title": v.get("title").and_then(Value::as_str).unwrap_or(""),
        "body": v.get("body").and_then(Value::as_str).unwrap_or(""),
        "url": v.get("url").and_then(Value::as_str).unwrap_or(""),
        "author": actor(v.get("author").unwrap_or(&Value::Null)),
        "state": pr_state_of(&v),
        "isDraft": v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        "mergeability": mergeability(&v),
        "additions": num(v.get("additions").unwrap_or(&Value::Null)),
        "deletions": num(v.get("deletions").unwrap_or(&Value::Null)),
        "changedFiles": num(v.get("changedFiles").unwrap_or(&Value::Null)),
        "headBranch": v.get("headRefName").and_then(Value::as_str).unwrap_or(""),
        "headRepositoryNameWithOwner": head_name_with_owner,
        "baseBranch": v.get("baseRefName").and_then(Value::as_str).unwrap_or(""),
        "createdAt": v.get("createdAt").and_then(Value::as_str).unwrap_or(""),
        "updatedAt": v.get("updatedAt").and_then(Value::as_str).unwrap_or(""),
        "mergedAt": v.get("mergedAt").cloned().unwrap_or(Value::Null),
        "closedAt": v.get("closedAt").cloned().unwrap_or(Value::Null),
        "reviewers": actor_list(v.get("reviewRequests")),
        "labels": label_list(v.get("labels")),
        "checks": check_list(v.get("statusCheckRollup")),
        "mergeCapabilities": { "merge": true, "squash": true, "rebase": true },
    });
    let head_sha = v.get("headRefOid").and_then(Value::as_str).unwrap_or("");
    if !head_sha.is_empty() {
        out["headSha"] = json!(head_sha);
    }
    if let Some(m) = v.get("maintainerCanModify").and_then(Value::as_bool) {
        out["maintainerCanModify"] = json!(m);
    }
    let auto_merge = v.get("autoMergeRequest");
    if auto_merge.is_some_and(|a| !a.is_null()) {
        out["autoMergeEnabled"] = json!(true);
        if let Some(m) = auto_merge
            .and_then(|a| a.get("mergeMethod"))
            .and_then(Value::as_str)
        {
            out["autoMergeMethod"] = json!(m.to_ascii_lowercase());
        }
    }
    // Viewer login is best-effort; an unreadable host leaves the field absent.
    if let Ok(viewer) = gh(
        &r.project.workspace_root,
        &["api", "user", "--jq", ".login"],
        host_of(&r),
        "detail",
    )
    .await
    {
        let login = viewer.trim();
        if !login.is_empty() {
            out["viewer"] = json!(login);
        }
    }
    RpcResult::ok(out)
}

async fn pr_checks(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    match gh_pr_view(&r, &["state", "statusCheckRollup"], "checks").await {
        Ok(v) => RpcResult::ok(json!({
            "state": pr_state_of(&v),
            "checks": check_list(v.get("statusCheckRollup")),
        })),
        Err(e) => RpcResult::err(e),
    }
}

fn gh_comment(v: &Value, kind: &str) -> Value {
    let id = match v.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(other) if !other.is_null() => other.to_string(),
        _ => String::new(),
    };
    json!({
        "id": id,
        "kind": kind,
        "author": actor(v.get("author").unwrap_or(&Value::Null)),
        "body": v.get("body").and_then(Value::as_str).unwrap_or(""),
        "createdAt": v.get("createdAt").and_then(Value::as_str).unwrap_or(""),
        "editedAt": Value::Null,
        "url": v.get("url").cloned().unwrap_or(Value::Null),
        "path": Value::Null,
        "reviewState": Value::Null,
    })
}

async fn pr_activity(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let v = match gh_pr_view(
        &r,
        &["author", "reviewRequests", "comments", "reviews", "commits"],
        "activity",
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return RpcResult::err(e),
    };

    let mut comments: Vec<Value> = Vec::new();
    for c in v
        .get("comments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        comments.push(gh_comment(&c, "issue-comment"));
    }
    for rev in v
        .get("reviews")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let mut c = gh_comment(&rev, "review");
        c["reviewState"] = rev
            .get("state")
            .and_then(Value::as_str)
            .map(|s| json!(s.to_ascii_lowercase()))
            .unwrap_or(Value::Null);
        comments.push(c);
    }
    comments.sort_by(|a, b| {
        let x = a.get("createdAt").and_then(Value::as_str).unwrap_or("");
        let y = b.get("createdAt").and_then(Value::as_str).unwrap_or("");
        x.cmp(y)
    });

    let commits: Vec<Value> = v
        .get("commits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| {
            json!({
                "oid": c.get("oid").and_then(Value::as_str).unwrap_or(""),
                "messageHeadline": c.get("messageHeadline").and_then(Value::as_str).unwrap_or(""),
                "committedDate": c.get("committedDate").and_then(Value::as_str).unwrap_or(""),
                "authors": actor_list(c.get("authors")),
            })
        })
        .collect();

    // Line-level review threads: REST `pulls/{n}/comments` groups replies under
    // root comments via `in_reply_to_id`. Thread ids are the root comment's
    // REST id, which `replyToThread`/`threadComments` reuse.
    let threads_path = format!(
        "repos/{}/pulls/{}/comments?per_page={PAGE_SIZE}",
        r.pr.repository, r.pr.number
    );
    let review_comments = gh_json(
        &r.project.workspace_root,
        &["api", threads_path.as_str()],
        host_of(&r),
        "activity",
    )
    .await
    .unwrap_or_else(|_| json!([]));
    let review_comments = review_comments.as_array().cloned().unwrap_or_default();

    let mut threads: Vec<Value> = Vec::new();
    for c in &review_comments {
        // Roots have no in_reply_to_id.
        if c.get("in_reply_to_id").and_then(Value::as_i64).is_some() {
            continue;
        }
        let root_id = num(c.get("id").unwrap_or(&Value::Null));
        let mut thread_comments: Vec<Value> = vec![rest_thread_comment(c)];
        for reply in &review_comments {
            if num(reply.get("in_reply_to_id").unwrap_or(&Value::Null)) == root_id {
                thread_comments.push(rest_thread_comment(reply));
            }
        }
        let side = match c.get("side").and_then(Value::as_str).unwrap_or("RIGHT") {
            "LEFT" => "left",
            _ => "right",
        };
        let line = c
            .get("line")
            .and_then(Value::as_i64)
            .or_else(|| c.get("original_line").and_then(Value::as_i64))
            .map(Value::from)
            .unwrap_or(Value::Null);
        threads.push(json!({
            "id": root_id.to_string(),
            "path": c.get("path").and_then(Value::as_str).unwrap_or(""),
            "line": line,
            "side": side,
            "isResolved": false,
            "isOutdated": c.get("line").is_none() && c.get("original_line").is_some(),
            "comments": thread_comments,
        }));
    }

    let comment_count = comments.len();
    RpcResult::ok(json!({
        "author": actor(v.get("author").unwrap_or(&Value::Null)),
        "reviewers": actor_list(v.get("reviewRequests")),
        "comments": comments,
        "commentCount": comment_count,
        "commentsTruncated": false,
        "reviewThreadsTruncated": false,
        "reviewThreads": threads,
        "commits": commits,
    }))
}

fn rest_thread_comment(c: &Value) -> Value {
    let user = c.get("user").cloned().unwrap_or(Value::Null);
    let created = c.get("created_at").and_then(Value::as_str).unwrap_or("");
    let updated = c.get("updated_at").and_then(Value::as_str).unwrap_or("");
    json!({
        "id": num(c.get("id").unwrap_or(&Value::Null)).to_string(),
        "author": actor(&user),
        "body": c.get("body").and_then(Value::as_str).unwrap_or(""),
        "createdAt": created,
        "editedAt": if updated > created { json!(updated) } else { Value::Null },
        "url": c.get("html_url").cloned().unwrap_or(Value::Null),
    })
}

async fn pr_thread_comments(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let thread_id = payload
        .get("threadId")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let page: u32 = payload
        .get("cursor")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let path = format!(
        "repos/{}/pulls/{}/comments?per_page={PAGE_SIZE}&page={page}",
        r.pr.repository, r.pr.number
    );
    let rows = match gh_json(
        &r.project.workspace_root,
        &["api", path.as_str()],
        host_of(&r),
        "threadComments",
    )
    .await
    {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(e) => return RpcResult::err(e),
    };
    let fetched = rows.len() as u32;
    let mut comments: Vec<Value> = rows
        .iter()
        .filter(|c| {
            num(c.get("id").unwrap_or(&Value::Null)) == thread_id
                || num(c.get("in_reply_to_id").unwrap_or(&Value::Null)) == thread_id
        })
        .map(rest_thread_comment)
        .collect();
    comments.sort_by(|a, b| {
        let x = a.get("createdAt").and_then(Value::as_str).unwrap_or("");
        let y = b.get("createdAt").and_then(Value::as_str).unwrap_or("");
        x.cmp(y)
    });
    RpcResult::ok(json!({
        "comments": comments,
        "nextCursor": if fetched >= PAGE_SIZE { json!((page + 1).to_string()) } else { Value::Null },
    }))
}

async fn pr_linked_threads(state: Arc<AppState>, payload: Value) -> RpcResult {
    let pr = match pr_ref(&payload) {
        Ok(p) => p,
        Err(e) => return RpcResult::err(e),
    };
    let key_host = effective_host(&pr).to_lowercase();
    let key_repo = pr.repository.to_lowercase();

    let db_path = state.home_dir.join("userdata/statev2.sqlite");
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .or_else(|_| rusqlite::Connection::open(&db_path));
    let conn = match conn {
        Ok(c) => c,
        Err(e) => {
            return RpcResult::err(pr_op_error(
                "linkedThreads",
                format!("Could not open the state database: {e}"),
            ))
        }
    };
    let mut stmt = match conn.prepare(
        "SELECT t.thread_id, t.project_id, t.title, t.archived_at, \
         (json_extract(t.payload_json, '$.settledAt') IS NOT NULL \
           OR json_extract(t.payload_json, '$.settledOverride') IS 'settled') AS settled, \
         json_extract(l.value, '$.host') AS host, \
         json_extract(l.value, '$.repository') AS repository, \
         json_extract(l.value, '$.url') AS url \
         FROM orchestration_v2_projection_threads t \
         JOIN json_each(t.payload_json, '$.pullRequests') l \
         WHERE json_extract(l.value, '$.number') = ?1 \
           AND json_extract(l.value, '$.source') != 'stack-dismissed' \
           AND t.deleted_at IS NULL \
           AND json_extract(t.payload_json, '$.lineage.sideChat') IS NOT 1 \
         ORDER BY t.updated_at DESC, t.thread_id ASC",
    ) {
        Ok(s) => s,
        Err(e) => {
            return RpcResult::err(pr_op_error(
                "linkedThreads",
                format!("Could not load linked threads: {e}"),
            ))
        }
    };
    let rows = stmt
        .query_map(rusqlite::params![pr.number], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })
        .map(|m| m.filter_map(Result::ok).collect::<Vec<_>>())
        .unwrap_or_default();

    let threads: Vec<Value> = rows
        .into_iter()
        .filter(|(_, _, _, _, _, link_host, link_repo, link_url)| {
            let host = link_host
                .clone()
                .or_else(|| link_url.as_deref().and_then(url_hostname))
                .unwrap_or_else(|| key_host.clone())
                .to_lowercase();
            let repo = link_repo.clone().unwrap_or_default().to_lowercase();
            host == key_host && repo == key_repo
        })
        .map(|(id, project_id, title, archived_at, settled, _, _, _)| {
            json!({
                "id": id,
                "projectId": project_id,
                "title": title,
                "archivedAt": archived_at.map(Value::from).unwrap_or(Value::Null),
                "settled": settled == 1,
            })
        })
        .collect();
    RpcResult::ok(json!({ "threads": threads }))
}

/// Port of `packages/shared/src/changedFileKind.ts` classification (regexes
/// re-expressed as string checks; `regex` is not a dependency).
fn classify_changed_file(path: &str) -> &'static str {
    let normalized = path.replace('\\', "/");
    let mut segments: Vec<&str> = normalized.split('/').collect();
    let name = segments.pop().unwrap_or("");
    let lower_name = name.to_lowercase();
    let dirs: Vec<String> = segments.iter().map(|s| s.to_lowercase()).collect();
    let in_dir = |set: &[&str]| dirs.iter().any(|d| set.contains(&d.as_str()));
    let (stem, ext) = name
        .rsplit_once('.')
        .map(|(s, e)| (s.to_lowercase(), e.to_lowercase()))
        .unwrap_or_else(|| (lower_name.clone(), String::new()));

    const GENERATED_FILES: &[&str] = &[
        "bun.lock",
        "bun.lockb",
        "cargo.lock",
        "composer.lock",
        "deno.lock",
        "flake.lock",
        "gemfile.lock",
        "go.sum",
        "gradle.lockfile",
        "mix.lock",
        "npm-shrinkwrap.json",
        "package-lock.json",
        "packages.lock.json",
        "pipfile.lock",
        "pnpm-lock.yaml",
        "podfile.lock",
        "poetry.lock",
        "pubspec.lock",
        "uv.lock",
        "yarn.lock",
    ];
    let generated_name = lower_name.contains(".generated.")
        || lower_name.contains(".gen.")
        || lower_name.ends_with(".g.dart")
        || lower_name.ends_with(".pb.go")
        || lower_name.ends_with("_pb2.py")
        || lower_name.ends_with("_pb2_grpc.py")
        || lower_name.ends_with(".pb.cc")
        || lower_name.ends_with(".pb.h")
        || lower_name.ends_with(".min.js")
        || lower_name.ends_with(".min.css")
        || lower_name.ends_with(".js.map")
        || lower_name.ends_with(".css.map");
    if GENERATED_FILES.contains(&lower_name.as_str())
        || generated_name
        || in_dir(&["__generated__", "_generated", "generated"])
    {
        return "generated";
    }

    // `\.(mocks?|fixtures?|stubs?|fakes?)\.[^.]+$` — a mock token right before
    // the final extension — plus `^(mocks?|fixtures?|stubs?|fakes?)([-_.A-Z]|$)`.
    const MOCK_TOKENS: &[&str] = &[
        "mock", "mocks", "fixture", "fixtures", "stub", "stubs", "fake", "fakes",
    ];
    let mock_name = lower_name.ends_with(".snap")
        || (!ext.is_empty()
            && MOCK_TOKENS
                .iter()
                .any(|t| stem.ends_with(&format!(".{t}"))))
        || MOCK_TOKENS.iter().any(|t| {
            lower_name == *t
                || lower_name.strip_prefix(t).is_some_and(|rest| {
                    rest.starts_with('-')
                        || rest.starts_with('_')
                        || rest.starts_with('.')
                        || rest.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                })
        });
    if mock_name
        || in_dir(&[
            "__fixtures__",
            "__mocks__",
            "__snapshots__",
            "fakes",
            "fixture",
            "fixtures",
            "mock",
            "mocks",
            "stubs",
            "testdata",
        ])
    {
        return "mocks";
    }

    let test_name = (!ext.is_empty()
        && (stem.ends_with(".test")
            || stem.ends_with(".spec")
            || stem.ends_with(".e2e")
            || stem.ends_with(".eval")
            || stem.ends_with("_test")
            || stem.ends_with("_spec")))
        || (lower_name.starts_with("test_") && lower_name.ends_with(".py"))
        || lower_name == "conftest.py"
        || lower_name.starts_with("vitest.setup.")
        || lower_name.starts_with("jest.setup.")
        // Case-sensitive class-style names: FooTest.java, FooSpec.kt, …
        || name
            .rsplit_once('.')
            .is_some_and(|(s, e)| {
                (s.ends_with("Test") || s.ends_with("Tests") || s.ends_with("Spec"))
                    && ["java", "kt", "kts", "scala", "groovy", "cs", "php", "swift", "m"]
                        .contains(&e)
            });
    if test_name
        || in_dir(&[
            "__tests__", "__test__", "cypress", "e2e", "evals", "spec", "specs", "test", "tests",
        ])
    {
        return "tests";
    }

    let docs_name = ["md", "mdx", "markdown", "rst", "adoc", "txt"].contains(&ext.as_str());
    let not_docs = (lower_name.starts_with("requirements") && lower_name.ends_with(".txt"))
        || (lower_name.starts_with("constraints") && lower_name.ends_with(".txt"))
        || lower_name == "cmakelists.txt"
        || lower_name == "robots.txt"
        || lower_name == "llms.txt";
    if docs_name && !not_docs {
        return "docs";
    }
    "code"
}

async fn pr_change_breakdown(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let path = format!(
        "repos/{}/pulls/{}/files?per_page={PAGE_SIZE}",
        r.pr.repository, r.pr.number
    );
    let files = match gh_json(
        &r.project.workspace_root,
        &["api", path.as_str()],
        host_of(&r),
        "changeBreakdown",
    )
    .await
    {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(e) => return RpcResult::err(e),
    };
    let count = |kind: &str| {
        let mut additions = 0i64;
        let mut deletions = 0i64;
        for f in &files {
            let path = f.get("filename").and_then(Value::as_str).unwrap_or("");
            if classify_changed_file(path) == kind {
                additions += num(f.get("additions").unwrap_or(&Value::Null));
                deletions += num(f.get("deletions").unwrap_or(&Value::Null));
            }
        }
        json!({ "additions": additions, "deletions": deletions })
    };
    RpcResult::ok(json!({
        "code": count("code"),
        "tests": count("tests"),
        "mocks": count("mocks"),
        "generated": count("generated"),
        "docs": count("docs"),
    }))
}

// ---------------------------------------------------------------------------
// filesViewed persistence (a small JSON map under the T3 home)
// ---------------------------------------------------------------------------

fn viewed_key(project_id: &str, host: &str, repository: &str, number: i64) -> String {
    format!(
        "{project_id}:{}/{}#{}",
        host.to_lowercase(),
        repository.to_lowercase(),
        number
    )
}

fn viewed_path(state: &AppState) -> std::path::PathBuf {
    state
        .home_dir
        .join("userdata")
        .join("pull_request_files_viewed.json")
}

fn read_viewed_map(state: &AppState) -> Map<String, Value> {
    std::fs::read_to_string(viewed_path(state))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_viewed_map(state: &AppState, map: &Map<String, Value>) {
    let path = viewed_path(state);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(map) {
        let _ = std::fs::write(path, text);
    }
}

async fn pr_files_viewed(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let key = viewed_key(
        &r.project.id,
        &effective_host(&r.pr),
        &r.pr.repository,
        r.pr.number,
    );
    let map = read_viewed_map(&state);
    let files: Vec<Value> = map
        .get(&key)
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(path, state)| json!({ "path": path, "state": state }))
                .collect()
        })
        .unwrap_or_default();
    RpcResult::ok(json!({ "files": files, "truncated": false }))
}

async fn pr_set_files_viewed(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let files = payload
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let key = viewed_key(
        &r.project.id,
        &effective_host(&r.pr),
        &r.pr.repository,
        r.pr.number,
    );
    let mut map = read_viewed_map(&state);
    let mut entry = map
        .get(&key)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for f in files {
        let path = f.get("path").and_then(Value::as_str).unwrap_or("");
        if path.is_empty() {
            continue;
        }
        if f.get("viewed").and_then(Value::as_bool) == Some(true) {
            entry.insert(path.to_string(), json!("viewed"));
        } else {
            entry.remove(path);
        }
    }
    if entry.is_empty() {
        map.remove(&key);
    } else {
        map.insert(key, Value::Object(entry));
    }
    write_viewed_map(&state, &map);
    RpcResult::ok(Value::Null)
}

// ---------------------------------------------------------------------------
// pullRequests.* writes
// ---------------------------------------------------------------------------

async fn gh_api_json_body(
    cwd: &str,
    method: &str,
    path: &str,
    body: Value,
    host: Option<&str>,
    operation: &str,
) -> Result<Value, Value> {
    let bytes = serde_json::to_vec(&body)
        .map_err(|e| pr_op_error(operation, format!("could not encode request: {e}")))?;
    let text = gh_stdin(
        cwd,
        &["api", "-X", method, path, "--input", "-"],
        host,
        Some(&bytes),
        operation,
    )
    .await?;
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text)
        .map_err(|e| pr_op_error(operation, format!("gh api returned unexpected output: {e}")))
}

async fn pr_comment(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let body = payload.get("body").and_then(Value::as_str).unwrap_or("");
    let path = format!("repos/{}/issues/{}/comments", r.pr.repository, r.pr.number);
    match gh_api_json_body(
        &r.project.workspace_root,
        "POST",
        &path,
        json!({ "body": body }),
        host_of(&r),
        "comment",
    )
    .await
    {
        Ok(_) => RpcResult::ok(Value::Null),
        Err(e) => RpcResult::err(e),
    }
}

async fn pr_update(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let mut body = Map::new();
    if let Some(t) = payload.get("title").and_then(Value::as_str) {
        body.insert("title".into(), json!(t));
    }
    if let Some(b) = payload.get("body").and_then(Value::as_str) {
        body.insert("body".into(), json!(b));
    }
    if let Some(m) = payload.get("maintainerCanModify").and_then(Value::as_bool) {
        body.insert("maintainer_can_modify".into(), json!(m));
    }
    let path = format!("repos/{}/pulls/{}", r.pr.repository, r.pr.number);
    match gh_api_json_body(
        &r.project.workspace_root,
        "PATCH",
        &path,
        Value::Object(body),
        host_of(&r),
        "update",
    )
    .await
    {
        Ok(_) => RpcResult::ok(Value::Null),
        Err(e) => RpcResult::err(e),
    }
}

async fn pr_update_comment(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let comment_id = payload.get("commentId").and_then(Value::as_str).unwrap_or("");
    let kind = payload
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("issue-comment");
    let body = payload.get("body").and_then(Value::as_str).unwrap_or("");
    let segment = if kind == "review-comment" {
        "pulls/comments"
    } else {
        "issues/comments"
    };
    let path = format!("repos/{}/{segment}/{comment_id}", r.pr.repository);
    match gh_api_json_body(
        &r.project.workspace_root,
        "PATCH",
        &path,
        json!({ "body": body }),
        host_of(&r),
        "updateComment",
    )
    .await
    {
        Ok(_) => RpcResult::ok(Value::Null),
        Err(e) => RpcResult::err(e),
    }
}

/// `PullRequestReviewPosition` → REST (line, side) pair.
fn position_to_api(p: &Value) -> Option<(i64, &'static str)> {
    match p.get("kind").and_then(Value::as_str) {
        Some("added") => p
            .get("newLine")
            .and_then(Value::as_i64)
            .map(|l| (l, "RIGHT")),
        Some("deleted") => p
            .get("oldLine")
            .and_then(Value::as_i64)
            .map(|l| (l, "LEFT")),
        Some("context") => {
            if p.get("side").and_then(Value::as_str) == Some("left") {
                p.get("oldLine")
                    .and_then(Value::as_i64)
                    .map(|l| (l, "LEFT"))
            } else {
                p.get("newLine")
                    .and_then(Value::as_i64)
                    .map(|l| (l, "RIGHT"))
            }
        }
        _ => None,
    }
}

async fn pr_submit_review(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let event = match payload
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("comment")
    {
        "approve" => "APPROVE",
        "request-changes" => "REQUEST_CHANGES",
        _ => "COMMENT",
    };
    let mut body = Map::new();
    body.insert("event".into(), json!(event));
    if let Some(b) = payload.get("body").and_then(Value::as_str) {
        if !b.is_empty() {
            body.insert("body".into(), json!(b));
        }
    }
    // Inline comments ride on the review's head commit.
    if let Ok(v) = gh_pr_view(&r, &["headRefOid"], "submitReview").await {
        if let Some(sha) = v.get("headRefOid").and_then(Value::as_str) {
            body.insert("commit_id".into(), json!(sha));
        }
    }
    let comments: Vec<Value> = payload
        .get("comments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            let (line, side) = position_to_api(c.get("position")?)?;
            let mut m = Map::new();
            m.insert(
                "path".into(),
                json!(c.get("path").and_then(Value::as_str).unwrap_or("")),
            );
            m.insert("line".into(), json!(line));
            m.insert("side".into(), json!(side));
            m.insert(
                "body".into(),
                json!(c.get("body").and_then(Value::as_str).unwrap_or("")),
            );
            if let Some((sl, ss)) = c.get("startPosition").and_then(position_to_api) {
                m.insert("start_line".into(), json!(sl));
                m.insert("start_side".into(), json!(ss));
            }
            Some(Value::Object(m))
        })
        .collect();
    if !comments.is_empty() {
        body.insert("comments".into(), Value::Array(comments));
    }
    let path = format!("repos/{}/pulls/{}/reviews", r.pr.repository, r.pr.number);
    match gh_api_json_body(
        &r.project.workspace_root,
        "POST",
        &path,
        Value::Object(body),
        host_of(&r),
        "submitReview",
    )
    .await
    {
        Ok(_) => RpcResult::ok(Value::Null),
        Err(e) => RpcResult::err(e),
    }
}

async fn pr_reply_to_thread(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let thread_id = payload.get("threadId").and_then(Value::as_str).unwrap_or("");
    let body = payload.get("body").and_then(Value::as_str).unwrap_or("");
    let path = format!(
        "repos/{}/pulls/{}/comments/{thread_id}/replies",
        r.pr.repository, r.pr.number
    );
    match gh_api_json_body(
        &r.project.workspace_root,
        "POST",
        &path,
        json!({ "body": body }),
        host_of(&r),
        "replyToThread",
    )
    .await
    {
        Ok(_) => RpcResult::ok(Value::Null),
        Err(e) => RpcResult::err(e),
    }
}

async fn pr_set_thread_resolution(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let thread_id = payload.get("threadId").and_then(Value::as_str).unwrap_or("");
    let resolved = payload.get("resolved").and_then(Value::as_bool) == Some(true);
    let mutation = if resolved {
        "resolveReviewThread"
    } else {
        "unresolveReviewThread"
    };
    let query_arg = format!(
        "query=mutation($threadId: ID!) {{ {mutation}(input: {{threadId: $threadId}}) {{ thread {{ isResolved }} }} }}"
    );
    let thread_arg = format!("threadId={thread_id}");
    match gh(
        &r.project.workspace_root,
        &["api", "graphql", "-f", query_arg.as_str(), "-f", thread_arg.as_str()],
        host_of(&r),
        "setThreadResolution",
    )
    .await
    {
        Ok(_) => RpcResult::ok(Value::Null),
        Err(e) => RpcResult::err(e),
    }
}

async fn pr_set_reaction(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let content = match payload.get("content").and_then(Value::as_str).unwrap_or("") {
        "thumbs-up" => "+1",
        "thumbs-down" => "-1",
        other => other,
    };
    let reacted = payload.get("reacted").and_then(Value::as_bool) == Some(true);
    // The subject is the pull request itself unless a comment id is given; a
    // comment id could be either kind, so try review comments then issue ones.
    let subject_paths: Vec<String> = match payload.get("subjectId").and_then(Value::as_str) {
        Some(id) if !id.is_empty() => vec![
            format!("repos/{}/pulls/comments/{id}/reactions", r.pr.repository),
            format!("repos/{}/issues/comments/{id}/reactions", r.pr.repository),
        ],
        _ => vec![format!(
            "repos/{}/issues/{}/reactions",
            r.pr.repository, r.pr.number
        )],
    };
    if reacted {
        let mut last_err = pr_op_error("setReaction", "No reaction target resolved.");
        for path in &subject_paths {
            match gh_api_json_body(
                &r.project.workspace_root,
                "POST",
                path,
                json!({ "content": content }),
                host_of(&r),
                "setReaction",
            )
            .await
            {
                Ok(_) => return RpcResult::ok(Value::Null),
                Err(e) => last_err = e,
            }
        }
        return RpcResult::err(last_err);
    }
    // Unreact: find the viewer's reaction of this content and delete it.
    let viewer = match gh(
        &r.project.workspace_root,
        &["api", "user", "--jq", ".login"],
        host_of(&r),
        "setReaction",
    )
    .await
    {
        Ok(v) => v.trim().to_string(),
        Err(e) => return RpcResult::err(e),
    };
    let mut last_err = pr_op_error("setReaction", "No reaction target resolved.");
    for path in &subject_paths {
        match gh_json(
            &r.project.workspace_root,
            &["api", path.as_str()],
            host_of(&r),
            "setReaction",
        )
        .await
        {
            Ok(v) => {
                for reaction in v.as_array().cloned().unwrap_or_default() {
                    let same = reaction.get("content").and_then(Value::as_str) == Some(content)
                        && reaction
                            .get("user")
                            .and_then(|u| u.get("login"))
                            .and_then(Value::as_str)
                            == Some(viewer.as_str());
                    if same {
                        let id = num(reaction.get("id").unwrap_or(&Value::Null));
                        let del = format!("{path}/{id}");
                        let _ = gh(
                            &r.project.workspace_root,
                            &["api", "-X", "DELETE", del.as_str()],
                            host_of(&r),
                            "setReaction",
                        )
                        .await;
                        return RpcResult::ok(Value::Null);
                    }
                }
                // The subject existed but the reaction did not — nothing to delete.
                return RpcResult::ok(Value::Null);
            }
            Err(e) => last_err = e,
        }
    }
    RpcResult::err(last_err)
}

async fn pr_run_action(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let action = payload.get("action").and_then(Value::as_str).unwrap_or("");
    let number = r.pr.number.to_string();
    let merge_method = payload
        .get("mergeMethod")
        .and_then(Value::as_str)
        .unwrap_or("merge");
    let merge_flag = match merge_method {
        "squash" => "--squash",
        "rebase" => "--rebase",
        _ => "--merge",
    };
    let root = r.project.workspace_root.as_str();
    let host = host_of(&r);

    let result: Result<(), Value> = match action {
        "merge" => gh(
            root,
            &["pr", "merge", number.as_str(), merge_flag],
            host,
            "runAction",
        )
        .await
        .map(|_| ()),
        "ready" => gh(root, &["pr", "ready", number.as_str()], host, "runAction")
            .await
            .map(|_| ()),
        "draft" => gh(
            root,
            &["pr", "ready", number.as_str(), "--undo"],
            host,
            "runAction",
        )
        .await
        .map(|_| ()),
        "close" => gh(root, &["pr", "close", number.as_str()], host, "runAction")
            .await
            .map(|_| ()),
        "reopen" => gh(root, &["pr", "reopen", number.as_str()], host, "runAction")
            .await
            .map(|_| ()),
        "enable-auto-merge" => gh(
            root,
            &["pr", "merge", number.as_str(), "--auto", merge_flag],
            host,
            "runAction",
        )
        .await
        .map(|_| ()),
        "disable-auto-merge" => gh(
            root,
            &["pr", "merge", number.as_str(), "--disable-auto"],
            host,
            "runAction",
        )
        .await
        .map(|_| ()),
        "delete-branch" => {
            let head = match gh_pr_view(&r, &["headRefName"], "runAction").await {
                Ok(v) => v
                    .get("headRefName")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_default(),
                Err(e) => return RpcResult::err(e),
            };
            if head.is_empty() {
                Err(pr_op_error(
                    "runAction",
                    "Pull request head branch is unknown.",
                ))
            } else {
                let path = format!("repos/{}/git/refs/heads/{head}", r.pr.repository);
                gh(root, &["api", "-X", "DELETE", path.as_str()], host, "runAction")
                    .await
                    .map(|_| ())
            }
        }
        "update-branch" => {
            let head_sha = gh_pr_view(&r, &["headRefOid"], "runAction")
                .await
                .ok()
                .and_then(|v| {
                    v.get("headRefOid")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
            let path = format!(
                "repos/{}/pulls/{}/update-branch",
                r.pr.repository, r.pr.number
            );
            let body = match head_sha {
                Some(sha) => json!({ "expected_head_sha": sha }),
                None => json!({}),
            };
            gh_api_json_body(root, "PUT", &path, body, host, "runAction")
                .await
                .map(|_| ())
        }
        other => Err(pr_op_error(
            "runAction",
            format!("Pull request action '{other}' is not supported."),
        )),
    };
    if let Err(e) = result {
        return RpcResult::err(e);
    }
    if payload.get("replyWithSummary").and_then(Value::as_bool) == Some(true) {
        match gh_pr_view(&r, PR_VIEW_FIELDS, "runAction").await {
            Ok(v) => return RpcResult::ok(summary_value(&r, &v)),
            Err(e) => return RpcResult::err(e),
        }
    }
    RpcResult::ok(Value::Null)
}

async fn pr_request_reviewers(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let requested = payload.get("requested").and_then(Value::as_bool) == Some(true);
    let reviewers = payload
        .get("reviewers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let users: Vec<Value> = reviewers
        .iter()
        .filter(|v| v.get("kind").and_then(Value::as_str) != Some("team"))
        .filter_map(|v| v.get("id").and_then(Value::as_str).map(|s| json!(s)))
        .collect();
    let teams: Vec<Value> = reviewers
        .iter()
        .filter(|v| v.get("kind").and_then(Value::as_str) == Some("team"))
        .filter_map(|v| v.get("id").and_then(Value::as_str).map(|s| json!(s)))
        .collect();
    let path = format!(
        "repos/{}/pulls/{}/requested_reviewers",
        r.pr.repository, r.pr.number
    );
    let body = json!({ "reviewers": users, "team_reviewers": teams });
    match gh_api_json_body(
        &r.project.workspace_root,
        if requested { "POST" } else { "DELETE" },
        &path,
        body,
        host_of(&r),
        "requestReviewers",
    )
    .await
    {
        Ok(_) => RpcResult::ok(Value::Null),
        Err(e) => RpcResult::err(e),
    }
}

async fn pr_reviewer_candidates(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let query = payload
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();

    // Already-requested reviewers mark `isRequested`.
    let requested_path = format!(
        "repos/{}/pulls/{}/requested_reviewers",
        r.pr.repository, r.pr.number
    );
    let requested = gh_json(
        &r.project.workspace_root,
        &["api", requested_path.as_str()],
        host_of(&r),
        "reviewerCandidates",
    )
    .await
    .unwrap_or_else(|_| json!({}));
    let requested_users: std::collections::HashSet<String> = requested
        .get("users")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|u| u.get("login").and_then(Value::as_str).map(str::to_owned))
        .collect();

    let collab_path = format!(
        "repos/{}/collaborators?per_page={PAGE_SIZE}",
        r.pr.repository
    );
    let collaborators = match gh_json(
        &r.project.workspace_root,
        &["api", collab_path.as_str()],
        host_of(&r),
        "reviewerCandidates",
    )
    .await
    {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(e) => return RpcResult::err(e),
    };

    let mut candidates: Vec<Value> = Vec::new();
    for u in collaborators {
        let login = u.get("login").and_then(Value::as_str).unwrap_or("");
        if login.is_empty() || (!query.is_empty() && !login.to_lowercase().contains(&query)) {
            continue;
        }
        candidates.push(json!({
            "id": login,
            "kind": "user",
            "login": login,
            "name": Value::Null,
            "avatarUrl": u.get("avatar_url").cloned().unwrap_or(Value::Null),
            "isRequested": requested_users.contains(login),
        }));
    }
    for team in requested
        .get("teams")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let slug = team.get("slug").and_then(Value::as_str).unwrap_or("");
        if slug.is_empty() || (!query.is_empty() && !slug.to_lowercase().contains(&query)) {
            continue;
        }
        candidates.push(json!({
            "id": slug,
            "kind": "team",
            "login": slug,
            "name": team.get("name").cloned().unwrap_or(Value::Null),
            "avatarUrl": Value::Null,
            "isRequested": true,
        }));
    }
    RpcResult::ok(json!({ "candidates": candidates, "truncated": false }))
}

async fn pr_label_candidates(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let path = format!("repos/{}/labels?per_page={PAGE_SIZE}", r.pr.repository);
    let labels = match gh_json(
        &r.project.workspace_root,
        &["api", path.as_str()],
        host_of(&r),
        "labelCandidates",
    )
    .await
    {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(e) => return RpcResult::err(e),
    };
    let applied: std::collections::HashSet<String> = gh_pr_view(&r, &["labels"], "labelCandidates")
        .await
        .ok()
        .and_then(|v| v.get("labels").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|l| l.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let candidates: Vec<Value> = labels
        .iter()
        .map(|l| {
            let name = l.get("name").and_then(Value::as_str).unwrap_or("");
            json!({
                "name": name,
                "color": l.get("color").cloned().unwrap_or(Value::Null),
                "description": l.get("description").cloned().unwrap_or(Value::Null),
                "isApplied": applied.contains(name),
            })
        })
        .collect();
    RpcResult::ok(json!({ "candidates": candidates, "truncated": false }))
}

async fn pr_set_labels(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let labels: Vec<String> = payload
        .get("labels")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|l| l.as_str().map(str::to_owned))
        .collect();
    let applied = payload.get("applied").and_then(Value::as_bool) == Some(true);
    if applied {
        let path = format!("repos/{}/issues/{}/labels", r.pr.repository, r.pr.number);
        return match gh_api_json_body(
            &r.project.workspace_root,
            "POST",
            &path,
            json!({ "labels": labels }),
            host_of(&r),
            "setLabels",
        )
        .await
        {
            Ok(_) => RpcResult::ok(Value::Null),
            Err(e) => RpcResult::err(e),
        };
    }
    for name in labels {
        let path = format!(
            "repos/{}/issues/{}/labels/{}",
            r.pr.repository,
            r.pr.number,
            domain::uri_component(&name)
        );
        if let Err(e) = gh(
            &r.project.workspace_root,
            &["api", "-X", "DELETE", path.as_str()],
            host_of(&r),
            "setLabels",
        )
        .await
        {
            return RpcResult::err(e);
        }
    }
    RpcResult::ok(Value::Null)
}

async fn pr_diff_file_contents(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let change_type = payload
        .get("changeType")
        .and_then(Value::as_str)
        .unwrap_or("change");
    let old_path = payload.get("oldPath").and_then(Value::as_str).unwrap_or("");
    let new_path = payload.get("newPath").and_then(Value::as_str).unwrap_or("");
    let refs = match gh_pr_view(&r, &["baseRefOid", "headRefOid"], "diffFileContents").await {
        Ok(v) => v,
        Err(e) => return RpcResult::err(e),
    };
    let base_sha = refs
        .get("baseRefOid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let head_sha = refs
        .get("headRefOid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let encode_path = |p: &str| {
        p.split('/')
            .map(domain::uri_component)
            .collect::<Vec<_>>()
            .join("/")
    };
    let contents_at = |path: &str, git_ref: &str| {
        format!(
            "repos/{}/contents/{}?ref={git_ref}",
            r.pr.repository,
            encode_path(path)
        )
    };
    let old_contents = if change_type == "new" {
        String::new()
    } else {
        let p = contents_at(old_path, &base_sha);
        match gh(
            &r.project.workspace_root,
            &["api", p.as_str(), "-H", "Accept: application/vnd.github.raw"],
            host_of(&r),
            "diffFileContents",
        )
        .await
        {
            Ok(s) => s,
            Err(e) => return RpcResult::err(e),
        }
    };
    let new_contents = if change_type == "deleted" {
        String::new()
    } else {
        let p = contents_at(new_path, &head_sha);
        match gh(
            &r.project.workspace_root,
            &["api", p.as_str(), "-H", "Accept: application/vnd.github.raw"],
            host_of(&r),
            "diffFileContents",
        )
        .await
        {
            Ok(s) => s,
            Err(e) => return RpcResult::err(e),
        }
    };
    RpcResult::ok(json!({ "oldContents": old_contents, "newContents": new_contents }))
}

async fn pr_routing(state: Arc<AppState>, payload: Value) -> RpcResult {
    let r = match resolved_ref(&state, &payload).await {
        Ok(r) => r,
        Err(e) => return RpcResult::err(e),
    };
    let viewer = match gh(
        &r.project.workspace_root,
        &["api", "user", "--jq", ".login"],
        host_of(&r),
        "routing",
    )
    .await
    {
        Ok(v) => v.trim().to_string(),
        Err(e) => return RpcResult::err(e),
    };
    let host = effective_host(&r.pr);
    RpcResult::ok(json!({
        "accountId": format!("{host}:{viewer}"),
        "host": host,
        "provider": "github",
        "viewer": viewer,
        "projectTitle": r.project.title.as_str(),
        "workspaceRoot": r.project.workspace_root.as_str(),
    }))
}

async fn pr_routing_identity(_state: Arc<AppState>, payload: Value) -> RpcResult {
    let host = payload
        .get("host")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_HOST);
    let viewer = match gh(
        "/",
        &["api", "user", "--jq", ".login"],
        Some(host),
        "routingIdentity",
    )
    .await
    {
        Ok(v) => v.trim().to_string(),
        Err(e) => return RpcResult::err(e),
    };
    RpcResult::ok(json!({
        "accountId": format!("{host}:{viewer}"),
        "host": host,
        "provider": "github",
        "viewer": viewer,
    }))
}

async fn pr_review_guide(state: Arc<AppState>, payload: Value) -> RpcResult {
    // Guides are generated artifacts; none exist in this implementation.
    // Report the PR head so the client can still diff against `guide.headSha`.
    let mut head: Value = Value::Null;
    if let Ok(r) = resolved_ref(&state, &payload).await {
        if let Ok(v) = gh_pr_view(&r, &["headRefOid"], "reviewGuide").await {
            head = v.get("headRefOid").cloned().unwrap_or(Value::Null);
        }
    }
    RpcResult::ok(
        json!({ "guide": Value::Null, "run": Value::Null, "currentHeadSha": head }),
    )
}

// ---------------------------------------------------------------------------
// review.* (local git reads — no gh needed)
// ---------------------------------------------------------------------------

/// Resolve the diff base like `resolveReviewMergeBase`/`resolveBaseBranchForNoUpstream`:
/// an explicit ref goes through `git merge-base <ref> HEAD`; without one the
/// chain is branch merge-base config → `@{upstream}` → remote `HEAD` →
/// `origin/main`, `origin/master`, `main`, `master` → finally `HEAD`.
/// Returns `(displayBaseRef, mergeBaseShaOrRef)`.
async fn review_merge_base(
    cwd: &str,
    base_ref: Option<&str>,
    operation: &str,
) -> Result<(Value, String), Value> {
    /// First arg is what the client sees, second what `merge-base` compares.
    /// `display` of `None` means "no base" — the caller returns `HEAD`.
    async fn resolved_base(cwd: &str, operation: &str) -> Option<(String, String)> {
        let git_ok = |args: &[&str]| {
            let cwd = cwd.to_string();
            let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            async move { git(&cwd, &owned, operation).await.ok().map(|s| s.trim().to_string()) }
        };
        // 1) `branch.<name>.t3-merge-base-ref` config override (full refs only).
        if let Some(branch) = git_ok(&["rev-parse", "--abbrev-ref", "HEAD"]).await.filter(|b| b != "HEAD") {
            let key = format!("branch.{branch}.t3-merge-base-ref");
            if let Some(cfg) = git_ok(&["config", "--get", key.as_str()]).await {
                if cfg.starts_with("refs/heads/") || cfg.starts_with("refs/remotes/") {
                    return Some((cfg.clone(), cfg));
                }
            }
        }
        // 2) Upstream tracking ref — a full `refs/remotes/...` so a similarly
        //    named local branch cannot shadow it.
        if let Some(up) = git_ok(&["rev-parse", "--symbolic-full-name", "@{upstream}"]).await {
            if let Some(short) = up.strip_prefix("refs/remotes/") {
                return Some((short.to_string(), up));
            }
        }
        // 3) The remote's default branch (`refs/remotes/origin/HEAD`).
        if let Some(head) = git_ok(&["symbolic-ref", "refs/remotes/origin/HEAD"]).await {
            if let Some(short) = head.strip_prefix("refs/remotes/") {
                return Some((short.to_string(), head));
            }
        }
        // 4) Well-known base candidates, remote-tracking then local.
        for (display, full) in [
            ("origin/main", "refs/remotes/origin/main"),
            ("origin/master", "refs/remotes/origin/master"),
            ("main", "refs/heads/main"),
            ("master", "refs/heads/master"),
        ] {
            let Ok(out) = git_out(cwd, &["show-ref", "--verify", "--quiet", full], operation).await
            else {
                continue;
            };
            if out.status == Some(0) {
                return Some((display.to_string(), full.to_string()));
            }
        }
        None
    }

    let (display, comparison) = match base_ref {
        Some(b) => (json!(b), b.to_string()),
        None => match resolved_base(cwd, operation).await {
            Some((d, c)) => (json!(d), c),
            None => return Ok((Value::Null, "HEAD".into())),
        },
    };
    let out = git_out(
        cwd,
        &["merge-base", "--end-of-options", comparison.as_str(), "HEAD"],
        operation,
    )
    .await?;
    let mb = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status != Some(0) || mb.is_empty() {
        if base_ref.is_none() {
            // No explicit base and nothing to compare against (e.g. unborn HEAD).
            return Ok((Value::Null, "HEAD".into()));
        }
        return Err(git_command_error(
            operation,
            &format!("git merge-base {comparison} HEAD"),
            cwd,
            out.status,
            format!("Could not find a common commit between '{comparison}' and HEAD."),
        ));
    }
    Ok((display, mb))
}

async fn review_branch_commits(_state: Arc<AppState>, payload: Value) -> RpcResult {
    let op = "ReviewService.getBranchCommits";
    let cwd = payload.get("cwd").and_then(Value::as_str).unwrap_or("");
    // Not a repo → the contract's empty result, not an error.
    let root = match git(cwd, &["rev-parse", "--show-toplevel"], op).await {
        Ok(r) => r.trim().to_string(),
        Err(_) => return RpcResult::ok(json!({ "commits": [], "truncated": false })),
    };
    if root.is_empty() {
        return RpcResult::ok(json!({ "commits": [], "truncated": false }));
    }
    // Unborn HEAD → empty list.
    let head = match git_out(&root, &["rev-parse", "--verify", "--quiet", "HEAD"], op).await {
        Ok(h) => h,
        Err(e) => return RpcResult::err(e),
    };
    if head.status != Some(0) {
        if head.stderr.trim().is_empty() {
            return RpcResult::ok(json!({ "commits": [], "truncated": false }));
        }
        return RpcResult::err(git_command_error(
            op,
            "git rev-parse --verify HEAD",
            &root,
            head.status,
            "Could not resolve the current branch commit.",
        ));
    }
    let (_, merge_base_sha) = match review_merge_base(
        &root,
        payload.get("baseRef").and_then(Value::as_str),
        op,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return RpcResult::err(e),
    };
    const LIMIT: usize = 100;
    let range = format!("{merge_base_sha}..HEAD");
    let max_count = format!("--max-count={}", LIMIT + 1);
    let out = match git(
        &root,
        &[
            "log",
            "-z",
            "--no-notes",
            "--no-show-signature",
            "--format=%H%x00%s%x00%cI",
            max_count.as_str(),
            range.as_str(),
        ],
        op,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => return RpcResult::err(e),
    };
    let fields: Vec<&str> = out.split('\0').collect();
    let mut commits = Vec::new();
    let mut i = 0;
    while i + 2 < fields.len() && commits.len() <= LIMIT {
        let (sha, subject, committed_at) = (fields[i], fields[i + 1], fields[i + 2]);
        if !sha.is_empty() && sha.chars().all(|c| c.is_ascii_hexdigit()) {
            commits.push(json!({
                "sha": sha,
                "subject": subject.chars().take(300).collect::<String>(),
                "committedAt": committed_at,
            }));
        }
        i += 3;
    }
    let truncated = commits.len() > LIMIT;
    commits.truncate(LIMIT);
    RpcResult::ok(json!({ "commits": commits, "truncated": truncated }))
}

/// FNV-1a 64-bit over `diff` + `files`, hex-encoded. `diffHash` only needs to
/// be a stable change key — the client never inspects it (no SHA crate here).
fn diff_hash(diff: &str, files: &Value) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in diff
        .as_bytes()
        .iter()
        .chain(serde_json::to_string(files).unwrap_or_default().as_bytes())
    {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// `git diff --numstat -z` rows → `ReviewDiffFileStat`. Renames arrive as
/// `add\tdel\t\0old\0new` — an empty path slot followed by two paths.
fn parse_numstat(stdout: &str) -> Vec<Value> {
    let fields: Vec<&str> = stdout.split('\0').collect();
    let mut files = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let field = fields[i];
        i += 1;
        if field.is_empty() {
            continue;
        }
        let mut parts = field.splitn(3, '\t');
        let (Some(add), Some(del), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if !(add.chars().all(|c| c.is_ascii_digit()) || add == "-")
            || !(del.chars().all(|c| c.is_ascii_digit()) || del == "-")
        {
            continue;
        }
        let (previous, path) = if path.is_empty() && i < fields.len() {
            let old = fields[i];
            let new = fields.get(i + 1).copied().unwrap_or("");
            i += 2;
            (Value::String(old.to_string()), new.to_string())
        } else {
            (Value::Null, path.to_string())
        };
        files.push(json!({
            "path": path,
            "previousPath": previous,
            "additions": if add == "-" { 0 } else { add.parse::<i64>().unwrap_or(0) },
            "deletions": if del == "-" { 0 } else { del.parse::<i64>().unwrap_or(0) },
        }));
    }
    files
}

const DIFF_CONFIG_ARGS: &[&str] = &[
    "-c",
    "diff.suppressBlankEmpty=false",
    "diff",
    "--find-renames",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--submodule=short",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

async fn review_diff_preview(_state: Arc<AppState>, payload: Value) -> RpcResult {
    let op = "ReviewService.getDiffPreview";
    let cwd = payload.get("cwd").and_then(Value::as_str).unwrap_or("");
    let empty = json!({
        "cwd": cwd,
        "generatedAt": domain::now_iso(),
        "sources": [],
    });
    let root = match git(cwd, &["rev-parse", "--show-toplevel"], op).await {
        Ok(r) => r.trim().to_string(),
        Err(_) => return RpcResult::ok(empty),
    };
    if root.is_empty() {
        return RpcResult::ok(empty);
    }

    let ignore_ws = payload.get("ignoreWhitespace").and_then(Value::as_bool) == Some(true);
    let metadata_only = payload.get("metadataOnly").and_then(Value::as_bool) == Some(true);
    let file = payload.get("file");
    let commit_sha = payload.get("commitSha").and_then(Value::as_str);
    let source_kind = payload
        .get("sourceKind")
        .and_then(Value::as_str)
        .or_else(|| file.and_then(|f| f.get("sourceKind")).and_then(Value::as_str));

    // Pathspecs limited to the requested file, top-literal like the TS driver.
    let mut pathspecs: Vec<String> = Vec::new();
    if let Some(f) = file {
        if let Some(p) = f.get("path").and_then(Value::as_str) {
            pathspecs.push(format!(":(top,literal){p}"));
        }
        if let Some(p) = f.get("previousPath").and_then(Value::as_str) {
            pathspecs.push(format!(":(top,literal){p}"));
        }
    }

    let mut base_args: Vec<String> = DIFF_CONFIG_ARGS.iter().map(|s| s.to_string()).collect();
    if ignore_ws {
        base_args.push("--ignore-all-space".into());
    }
    base_args.push("--minimal".into());
    let numstat_args = |refs: &[String]| -> Vec<String> {
        let mut a = base_args.clone();
        a.push("--numstat".into());
        a.push("-z".into());
        a.extend(refs.iter().cloned());
        if !pathspecs.is_empty() {
            a.push("--".into());
            a.extend(pathspecs.iter().cloned());
        }
        a
    };
    let patch_args = |refs: &[String]| -> Vec<String> {
        let mut a = base_args.clone();
        a.push("--patch".into());
        a.extend(refs.iter().cloned());
        if !pathspecs.is_empty() {
            a.push("--".into());
            a.extend(pathspecs.iter().cloned());
        }
        a
    };
    let no_patch = metadata_only && file.is_none();

    // ---- Single-commit source ----
    if commit_sha.is_some() || source_kind == Some("commit") {
        let Some(sha) = commit_sha.filter(|s| !s.is_empty()) else {
            return RpcResult::err(git_command_error(
                op,
                "git",
                &root,
                None,
                "Selected commit request is invalid.",
            ));
        };
        let parents = match git(&root, &["rev-list", "--parents", "-n", "1", sha], op).await {
            Ok(p) => p,
            Err(e) => return RpcResult::err(e),
        };
        let parent = parents
            .split_whitespace()
            .nth(1)
            .unwrap_or(EMPTY_TREE)
            .to_string();
        let subject = git(&root, &["log", "-1", "--format=%s", sha], op)
            .await
            .unwrap_or_default();
        let refs = vec![parent.clone(), sha.to_string()];
        let files = Value::Array(parse_numstat(
            &git(&root, &numstat_args(&refs), op)
                .await
                .unwrap_or_default(),
        ));
        let diff = if no_patch {
            String::new()
        } else {
            match git(&root, &patch_args(&refs), op).await {
                Ok(d) => d,
                Err(e) => return RpcResult::err(e),
            }
        };
        let hash = diff_hash(&diff, &files);
        let mut src = json!({
            "id": sha,
            "kind": "commit",
            "title": format!("Commit {}", &sha[..sha.len().min(7)]),
            "baseRef": parent,
            "headRef": sha,
            "diff": diff,
            "diffHash": hash,
            "truncated": false,
            "files": files,
        });
        if !subject.trim().is_empty() {
            src["commitSubject"] = json!(subject.trim());
        }
        if no_patch {
            src["patchesDeferred"] = json!(true);
        }
        return RpcResult::ok(json!({
            "cwd": cwd,
            "generatedAt": domain::now_iso(),
            "sources": [src],
        }));
    }

    // ---- Working-tree + branch-range sources ----
    let current_branch = git(&root, &["rev-parse", "--abbrev-ref", "HEAD"], op)
        .await
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "HEAD".into());
    let (display_base, merge_base_sha) = match review_merge_base(
        &root,
        payload.get("baseRef").and_then(Value::as_str),
        op,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return RpcResult::err(e),
    };

    let want = |kind: &str| source_kind.is_none() || source_kind == Some(kind);
    let mut sources: Vec<Value> = Vec::new();

    if want("working-tree") {
        let refs = vec!["HEAD".to_string()];
        let files = Value::Array(parse_numstat(
            &git(&root, &numstat_args(&refs), op)
                .await
                .unwrap_or_default(),
        ));
        let diff = if no_patch {
            String::new()
        } else {
            match git(&root, &patch_args(&refs), op).await {
                Ok(d) => d,
                Err(e) => return RpcResult::err(e),
            }
        };
        let hash = diff_hash(&diff, &files);
        let mut src = json!({
            "id": "working-tree",
            "kind": "working-tree",
            "title": "Uncommitted",
            "baseRef": "HEAD",
            "headRef": Value::Null,
            "diff": diff,
            "diffHash": hash,
            "truncated": false,
            "files": files,
        });
        if no_patch {
            src["patchesDeferred"] = json!(true);
        }
        sources.push(src);
    }

    if want("branch-range") {
        let refs = vec![merge_base_sha.clone()];
        let files = Value::Array(parse_numstat(
            &git(&root, &numstat_args(&refs), op)
                .await
                .unwrap_or_default(),
        ));
        let diff = if no_patch {
            String::new()
        } else {
            match git(&root, &patch_args(&refs), op).await {
                Ok(d) => d,
                Err(e) => return RpcResult::err(e),
            }
        };
        let hash = diff_hash(&diff, &files);
        let title = display_base
            .as_str()
            .map(|b| format!("Changes vs {b}"))
            .unwrap_or_else(|| "Changes".to_string());
        let mut src = json!({
            "id": "branch-range",
            "kind": "branch-range",
            "title": title,
            "baseRef": display_base,
            "headRef": if current_branch.is_empty() { "HEAD".to_string() } else { current_branch },
            "diff": diff,
            "diffHash": hash,
            "truncated": false,
            "files": files,
        });
        if no_patch {
            src["patchesDeferred"] = json!(true);
        }
        sources.push(src);
    }

    let mut out = json!({
        "cwd": cwd,
        "generatedAt": domain::now_iso(),
        "sources": sources,
    });
    if payload.get("trackedOnly").and_then(Value::as_bool) == Some(true) {
        out["incomplete"] = json!(true);
    }
    RpcResult::ok(out)
}

/// `git show <rev>:<path>` contents for review file expansion; NUL bytes mark
/// the file binary like the TS driver does.
async fn show_at_revision(
    operation: &str,
    root: &str,
    rev: &str,
    path: &str,
) -> Result<String, Value> {
    let spec = format!("{rev}:{path}");
    let out = git_out(root, &["show", "--end-of-options", spec.as_str()], operation).await?;
    if out.status != Some(0) {
        let detail = out.stderr.trim().to_string();
        return Err(git_command_error(
            operation,
            &format!("git show {spec}"),
            root,
            out.status,
            if detail.is_empty() {
                format!("git show {spec} failed")
            } else {
                detail.chars().take(600).collect()
            },
        ));
    }
    if out.stdout.contains(&0) {
        return Err(git_command_error(
            operation,
            &format!("git show {spec}"),
            root,
            None,
            format!("Cannot expand binary file '{path}'."),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

async fn review_diff_file_contents(_state: Arc<AppState>, payload: Value) -> RpcResult {
    let op = "ReviewService.getDiffFileContents";
    let cwd = payload.get("cwd").and_then(Value::as_str).unwrap_or("");
    let root = match git(cwd, &["rev-parse", "--show-toplevel"], op).await {
        Ok(r) => r.trim().to_string(),
        Err(_) => {
            return RpcResult::err(vcs_unsupported(
                op,
                "Unchanged diff expansion currently requires a Git repository.",
            ))
        }
    };
    if root.is_empty() {
        return RpcResult::err(git_command_error(
            op,
            "git rev-parse --show-toplevel",
            cwd,
            None,
            "Could not resolve the Git repository root.",
        ));
    }
    let source_kind = payload
        .get("sourceKind")
        .and_then(Value::as_str)
        .unwrap_or("working-tree");
    let change_type = payload
        .get("changeType")
        .and_then(Value::as_str)
        .unwrap_or("change");
    let old_path = payload.get("oldPath").and_then(Value::as_str).unwrap_or("");
    let new_path = payload.get("newPath").and_then(Value::as_str).unwrap_or("");

    if source_kind == "commit" {
        let head = payload.get("headRef").and_then(Value::as_str).unwrap_or("");
        if head.is_empty() {
            return RpcResult::err(git_command_error(
                op,
                "git",
                cwd,
                None,
                "Selected commit is no longer available.",
            ));
        }
        let parents = match git(&root, &["rev-list", "--parents", "-n", "1", head], op).await {
            Ok(p) => p,
            Err(e) => return RpcResult::err(e),
        };
        let parent = parents
            .split_whitespace()
            .nth(1)
            .unwrap_or(EMPTY_TREE)
            .to_string();
        let expected = payload.get("baseRef").and_then(Value::as_str);
        if expected.is_some() && expected != Some(parent.as_str()) {
            return RpcResult::err(git_command_error(
                op,
                "git",
                cwd,
                None,
                "Selected commit changed. Refresh the comparison.",
            ));
        }
        let old_contents = if change_type == "new" {
            String::new()
        } else {
            match show_at_revision(op, &root, &parent, old_path).await {
                Ok(s) => s,
                Err(e) => return RpcResult::err(e),
            }
        };
        let new_contents = if change_type == "deleted" {
            String::new()
        } else {
            match show_at_revision(op, &root, head, new_path).await {
                Ok(s) => s,
                Err(e) => return RpcResult::err(e),
            }
        };
        return RpcResult::ok(
            json!({ "oldContents": old_contents, "newContents": new_contents }),
        );
    }

    let old_revision = if source_kind == "working-tree" {
        payload
            .get("baseRef")
            .and_then(Value::as_str)
            .unwrap_or("HEAD")
            .to_string()
    } else {
        match review_merge_base(&root, payload.get("baseRef").and_then(Value::as_str), op).await {
            Ok((_, mb)) => mb,
            Err(e) => return RpcResult::err(e),
        }
    };
    let old_contents = if change_type == "new" {
        String::new()
    } else {
        match show_at_revision(op, &root, &old_revision, old_path).await {
            Ok(s) => s,
            Err(e) => return RpcResult::err(e),
        }
    };
    let new_contents = if change_type == "deleted" {
        String::new()
    } else {
        // Working-tree content: the file must live under the repo root.
        let target = std::path::Path::new(&root).join(new_path);
        let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone().into());
        let canonical = match std::fs::canonicalize(&target) {
            Ok(t) => t,
            Err(e) => {
                return RpcResult::err(git_command_error(
                    op,
                    "fs.realPath",
                    cwd,
                    None,
                    format!("Could not resolve diff file '{new_path}': {e}"),
                ))
            }
        };
        if !canonical.starts_with(&canonical_root) {
            return RpcResult::err(git_command_error(
                op,
                "fs.realPath",
                cwd,
                None,
                format!("Diff file '{new_path}' resolves outside the review workspace."),
            ));
        }
        let bytes = match std::fs::read(&canonical) {
            Ok(b) => b,
            Err(e) => {
                return RpcResult::err(git_command_error(
                    op,
                    "fs.readFile",
                    cwd,
                    None,
                    format!("Could not read diff file '{new_path}': {e}"),
                ))
            }
        };
        if bytes.len() > 1024 * 1024 {
            return RpcResult::err(git_command_error(
                op,
                "fs.readFile",
                cwd,
                None,
                format!("Diff file '{new_path}' exceeds the 1 MB expansion limit."),
            ));
        }
        if bytes.contains(&0) {
            return RpcResult::err(git_command_error(
                op,
                "fs.readFile",
                cwd,
                None,
                format!("Cannot expand binary file '{new_path}'."),
            ));
        }
        String::from_utf8_lossy(&bytes).to_string()
    };
    RpcResult::ok(json!({ "oldContents": old_contents, "newContents": new_contents }))
}

// ---------------------------------------------------------------------------
// sourceControl.*
// ---------------------------------------------------------------------------

fn repo_info(provider: &str, v: &Value) -> Value {
    json!({
        "provider": provider,
        "nameWithOwner": v.get("nameWithOwner").and_then(Value::as_str).unwrap_or(""),
        "url": v.get("url").and_then(Value::as_str).unwrap_or(""),
        "sshUrl": v.get("sshUrl").and_then(Value::as_str).unwrap_or(""),
    })
}

fn sc_provider_error(operation: &str, cwd: &str, detail: impl Into<String>) -> Value {
    json!({
        "_tag": "SourceControlProviderError",
        "provider": "github",
        "operation": operation,
        "cwd": cwd,
        "detail": detail.into(),
    })
}

/// `sourceControl.*` cannot surface `PullRequestUnavailableError` — it is not
/// in `SourceControlRpcError` — so a missing/unauthenticated `gh` maps onto
/// `SourceControlProviderError` with the same human-readable requirement.
fn to_sc_repo_error(operation: &str, cwd: &str, e: Value) -> Value {
    if e.get("_tag").and_then(Value::as_str) == Some("PullRequestUnavailableError") {
        return sc_provider_error(
            operation,
            cwd,
            e.get("requirement")
                .and_then(Value::as_str)
                .or_else(|| e.get("detail").and_then(Value::as_str))
                .unwrap_or("GitHub CLI credential is unavailable."),
        );
    }
    sc_repo_error(
        operation,
        e.get("detail")
            .and_then(Value::as_str)
            .or_else(|| e.get("message").and_then(Value::as_str))
            .unwrap_or("Source control operation failed."),
    )
}

async fn sc_lookup_repository(_state: Arc<AppState>, payload: Value) -> RpcResult {
    let op = "lookupRepository";
    let provider = payload
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("github");
    let repository = payload
        .get("repository")
        .and_then(Value::as_str)
        .unwrap_or("");
    let cwd = payload.get("cwd").and_then(Value::as_str).unwrap_or("/");
    if repository.is_empty() {
        return RpcResult::err(sc_repo_error(op, "A repository is required."));
    }
    match gh_json(
        cwd,
        &["repo", "view", repository, "--json", "nameWithOwner,url,sshUrl"],
        None,
        op,
    )
    .await
    {
        Ok(v) => RpcResult::ok(repo_info(provider, &v)),
        Err(e) => RpcResult::err(to_sc_repo_error(op, cwd, e)),
    }
}

async fn sc_clone_repository(_state: Arc<AppState>, payload: Value) -> RpcResult {
    let op = "cloneRepository";
    let destination = payload
        .get("destinationPath")
        .and_then(Value::as_str)
        .unwrap_or("");
    if destination.is_empty() {
        return RpcResult::err(sc_repo_error(op, "A destination path is required."));
    }
    let repository = payload.get("repository").and_then(Value::as_str);
    let remote_url = payload.get("remoteUrl").and_then(Value::as_str);
    if repository.is_none() && remote_url.is_none() {
        return RpcResult::err(sc_repo_error(
            op,
            "A repository or remote URL is required.",
        ));
    }
    let clone_result: Result<(), Value> = if let Some(repo) = repository {
        gh("/", &["repo", "clone", repo, destination], None, op)
            .await
            .map(|_| ())
            .map_err(|e| to_sc_repo_error(op, "/", e))
    } else {
        let url = remote_url.unwrap_or("");
        git_out("/", &["clone", url, destination], op)
            .await
            .and_then(|out| {
                if out.status == Some(0) {
                    Ok(())
                } else {
                    let detail = out.stderr.trim().to_string();
                    Err(sc_repo_error(
                        op,
                        if detail.is_empty() {
                            "git clone failed".to_string()
                        } else {
                            detail.chars().take(600).collect()
                        },
                    ))
                }
            })
    };
    if let Err(e) = clone_result {
        return RpcResult::err(e);
    }
    let resolved_remote = git(destination, &["remote", "get-url", "origin"], op)
        .await
        .unwrap_or_else(|_| remote_url.unwrap_or("").to_string());
    let provider = payload
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("github");
    let info = match repository {
        Some(repo) => gh_json(
            destination,
            &["repo", "view", repo, "--json", "nameWithOwner,url,sshUrl"],
            None,
            op,
        )
        .await
        .map(|v| repo_info(provider, &v))
        .unwrap_or(Value::Null),
        None => Value::Null,
    };
    RpcResult::ok(json!({
        "cwd": destination,
        "remoteUrl": resolved_remote.trim(),
        "repository": info,
    }))
}

async fn sc_publish_repository(_state: Arc<AppState>, payload: Value) -> RpcResult {
    let op = "publishRepository";
    let cwd = payload.get("cwd").and_then(Value::as_str).unwrap_or("");
    let repository = payload
        .get("repository")
        .and_then(Value::as_str)
        .unwrap_or("");
    let visibility = match payload.get("visibility").and_then(Value::as_str) {
        Some("private") => "--private",
        _ => "--public",
    };
    if cwd.is_empty() || repository.is_empty() {
        return RpcResult::err(sc_repo_error(
            op,
            "A working directory and repository name are required.",
        ));
    }
    let remote_name = payload
        .get("remoteName")
        .and_then(Value::as_str)
        .unwrap_or("origin");
    match gh(
        cwd,
        &[
            "repo",
            "create",
            repository,
            visibility,
            "--source",
            cwd,
            "--remote",
            remote_name,
            "--push",
        ],
        None,
        op,
    )
    .await
    {
        Ok(_) => {}
        Err(e) => return RpcResult::err(to_sc_repo_error(op, cwd, e)),
    }
    let provider = payload
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("github");
    let info = gh_json(
        cwd,
        &["repo", "view", repository, "--json", "nameWithOwner,url,sshUrl"],
        None,
        op,
    )
    .await
    .map(|v| repo_info(provider, &v))
    .unwrap_or_else(|_| {
        json!({
            "provider": provider,
            "nameWithOwner": repository,
            "url": "",
            "sshUrl": "",
        })
    });
    let remote_url = git(cwd, &["remote", "get-url", remote_name], op)
        .await
        .unwrap_or_default();
    let branch = git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"], op)
        .await
        .unwrap_or_else(|_| "main".into());
    RpcResult::ok(json!({
        "repository": info,
        "remoteName": remote_name,
        "remoteUrl": remote_url.trim(),
        "branch": branch.trim(),
        "status": "pushed",
    }))
}

async fn sc_app_access(_state: Arc<AppState>, payload: Value) -> RpcResult {
    let _ = payload;
    // The T3 Code GitHub App sign-in is not used by this server.
    RpcResult::ok(json!({ "status": "signed-out" }))
}

async fn sc_starred(_state: Arc<AppState>, payload: Value, star: bool) -> RpcResult {
    let repository = payload
        .get("repository")
        .and_then(Value::as_str)
        .unwrap_or("");
    let host = payload.get("host").and_then(Value::as_str);
    // "False when the forge cannot say" — errors collapse to false, not a failure.
    let path = format!("user/starred/{repository}");
    if star {
        let ok = gh("/", &["api", "-X", "PUT", path.as_str()], host, "star")
            .await
            .is_ok();
        return RpcResult::ok(json!(ok));
    }
    let out = gh("/", &["api", path.as_str()], host, "starred").await;
    RpcResult::ok(json!(out.is_ok()))
}

// ---------------------------------------------------------------------------
// git.* pull-request helpers (routed here by the rpc owner for PR-shaped tags)
// ---------------------------------------------------------------------------

/// Parse the `reference` field of `git.resolvePullRequest` /
/// `git.preparePullRequestThread`: a PR URL, `#N`, `N`, or `owner/repo#N`.
/// Returns `(host, owner/repo, number)`.
fn parse_pr_reference(reference: &str) -> Option<(Option<String>, Option<String>, i64)> {
    let s = reference.trim();
    if s.starts_with("http://") || s.starts_with("https://") {
        let rest = s.split("://").nth(1)?;
        let mut parts = rest.split('/');
        let host = parts.next()?.to_string();
        let owner = parts.next()?;
        let repo = parts.next()?;
        if parts.next() != Some("pull") {
            return None;
        }
        let n: i64 = parts
            .next()?
            .split(['?', '#', '/'])
            .next()?
            .parse()
            .ok()?;
        return Some((Some(host), Some(format!("{owner}/{repo}")), n));
    }
    if let Some((slug, num)) = s.split_once('#') {
        let n: i64 = num.parse().ok()?;
        if slug.contains('/') {
            return Some((None, Some(slug.to_string()), n));
        }
        return Some((None, None, n));
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some((None, None, n));
    }
    None
}

async fn git_resolve_pull_request(_state: Arc<AppState>, payload: Value) -> RpcResult {
    let op = "git.resolvePullRequest";
    let cwd = payload.get("cwd").and_then(Value::as_str).unwrap_or("");
    let reference = payload
        .get("reference")
        .and_then(Value::as_str)
        .unwrap_or("");
    let Some((host, repo, number)) = parse_pr_reference(reference) else {
        return RpcResult::err(git_manager_error(
            op,
            cwd,
            format!("Could not parse pull request reference '{reference}'."),
        ));
    };
    let number_s = number.to_string();
    let repo_arg = repo.clone().unwrap_or_default();
    let mut args: Vec<String> = vec![
        "pr".into(),
        "view".into(),
        number_s,
        "--json".into(),
        "number,title,url,baseRefName,headRefName,state".into(),
    ];
    if repo.is_some() {
        args.push("--repo".into());
        args.push(repo_arg);
    }
    match gh_json(cwd, &args, host.as_deref(), op).await {
        Ok(v) => RpcResult::ok(json!({
            "pullRequest": {
                "number": num(v.get("number").unwrap_or(&Value::Null)),
                "title": v.get("title").and_then(Value::as_str).unwrap_or(""),
                "url": v.get("url").and_then(Value::as_str).unwrap_or(""),
                "baseBranch": v.get("baseRefName").and_then(Value::as_str).unwrap_or(""),
                "headBranch": v.get("headRefName").and_then(Value::as_str).unwrap_or(""),
                "state": pr_state_of(&v),
            },
        })),
        Err(e) => {
            if e.get("_tag").and_then(Value::as_str) == Some("PullRequestUnavailableError") {
                RpcResult::err(e)
            } else {
                RpcResult::err(git_manager_error(
                    op,
                    cwd,
                    e.get("detail")
                        .and_then(Value::as_str)
                        .or_else(|| e.get("message").and_then(Value::as_str))
                        .unwrap_or("Pull request resolution failed."),
                ))
            }
        }
    }
}

/// `git.*` tags handled by this module. The `git.*` prefix routes to
/// `rpc_vcs` in `handlers.rs`, which may delegate these PR-shaped tags here.
pub async fn handle_git(state: Arc<AppState>, tag: &str, payload: Value) -> RpcResult {
    match tag {
        "git.resolvePullRequest" => git_resolve_pull_request(state, payload).await,
        "git.preparePullRequestThread" => {
            let cwd = payload.get("cwd").and_then(Value::as_str).unwrap_or("");
            RpcResult::err(git_manager_error(
                "git.preparePullRequestThread",
                cwd,
                "Pull request worktree materialization is not supported by this server.",
            ))
        }
        "git.runStackedAction" => {
            let cwd = payload.get("cwd").and_then(Value::as_str).unwrap_or("");
            RpcResult::err(git_manager_error(
                "git.runStackedAction",
                cwd,
                "Stacked actions are not supported by this server.",
            ))
        }
        _ => not_implemented(tag),
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

pub async fn handle(state: Arc<AppState>, tag: &str, payload: Value) -> RpcResult {
    match tag {
        // ---- reads ----
        "pullRequests.list" => list_pull_requests(state, payload).await,
        "pullRequests.listStats" => list_stats(state, payload).await,
        "pullRequests.filterOptions" => filter_options(state, payload).await,
        "pullRequests.summary" => pr_summary(state, payload).await,
        "pullRequests.detail" => pr_detail(state, payload).await,
        "pullRequests.preview" => pr_preview(state, payload).await,
        "pullRequests.checks" => pr_checks(state, payload).await,
        "pullRequests.activity" => pr_activity(state, payload).await,
        "pullRequests.linkedThreads" => pr_linked_threads(state, payload).await,
        "pullRequests.changeBreakdown" => pr_change_breakdown(state, payload).await,
        "pullRequests.filesViewed" => pr_files_viewed(state, payload).await,
        "pullRequests.threadComments" => pr_thread_comments(state, payload).await,
        "pullRequests.diffFileContents" => pr_diff_file_contents(state, payload).await,
        "pullRequests.routing" => pr_routing(state, payload).await,
        "pullRequests.routingIdentity" => pr_routing_identity(state, payload).await,
        "pullRequests.reviewGuide" => pr_review_guide(state, payload).await,
        "pullRequests.labelCandidates" => pr_label_candidates(state, payload).await,
        "pullRequests.reviewerCandidates" => pr_reviewer_candidates(state, payload).await,
        // Valid empty results, not stubs: the contract encodes "none" inline.
        "pullRequests.stack" => RpcResult::ok(Value::Null),
        "pullRequests.invalidate" => RpcResult::ok(Value::Null),
        "pullRequests.reportState" => RpcResult::ok(Value::Null),
        // ---- writes ----
        "pullRequests.setFilesViewed" => pr_set_files_viewed(state, payload).await,
        "pullRequests.comment" => pr_comment(state, payload).await,
        "pullRequests.update" => pr_update(state, payload).await,
        "pullRequests.updateComment" => pr_update_comment(state, payload).await,
        "pullRequests.submitReview" => pr_submit_review(state, payload).await,
        "pullRequests.replyToThread" => pr_reply_to_thread(state, payload).await,
        "pullRequests.setThreadResolution" => pr_set_thread_resolution(state, payload).await,
        "pullRequests.setReaction" => pr_set_reaction(state, payload).await,
        "pullRequests.runAction" => pr_run_action(state, payload).await,
        "pullRequests.requestReviewers" => pr_request_reviewers(state, payload).await,
        "pullRequests.setLabels" => pr_set_labels(state, payload).await,
        "pullRequests.generateReviewGuide" => not_implemented(tag),
        // ---- review.* — local git, no gh credential needed ----
        "review.getBranchCommits" => review_branch_commits(state, payload).await,
        "review.getDiffPreview" => review_diff_preview(state, payload).await,
        "review.getDiffFileContents" => review_diff_file_contents(state, payload).await,
        // ---- git.* PR helpers — also reachable if rpc_vcs delegates ----
        "git.resolvePullRequest" | "git.preparePullRequestThread" | "git.runStackedAction" => {
            handle_git(state, tag, payload).await
        }
        // ---- sourceControl.* ----
        "sourceControl.lookupRepository" => sc_lookup_repository(state, payload).await,
        "sourceControl.cloneRepository" => sc_clone_repository(state, payload).await,
        "sourceControl.publishRepository" => sc_publish_repository(state, payload).await,
        "sourceControl.appAccess" => sc_app_access(state, payload).await,
        "sourceControl.starred" => sc_starred(state, payload, false).await,
        "sourceControl.star" => sc_starred(state, payload, true).await,
        "sourceControl.signInStart" | "sourceControl.signInWait" => {
            let host = payload
                .get("host")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_HOST);
            let reason = if tag.ends_with("signInWait") {
                "not-started"
            } else {
                "unsupported"
            };
            RpcResult::err(sign_in_error(
                host,
                reason,
                "GitHub sign-in must run `gh auth login` directly; this server cannot drive a device flow.",
            ))
        }
        "sourceControl.signInCancel" => RpcResult::ok(Value::Null),
        _ => not_implemented(tag),
    }
}

/// Claim a subscription stream tag; returns true if this module handled it.
pub async fn handle_subscription(
    _state: Arc<AppState>,
    _out: crate::ws_rpc::OutTx,
    _request_id: u64,
    tag: &str,
    _payload: Value,
    window: Arc<crate::ws_rpc::Window>,
) -> bool {
    match tag {
        // Nothing to push: no credential watch or host polling exists here, so
        // the stream simply stays open and never emits a revision.
        "pullRequests.subscribeRefreshes" => {
            while !window.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            true
        }
        _ => false,
    }
}
