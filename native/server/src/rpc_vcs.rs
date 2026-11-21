//! `vcs.*` + `git.*` WS RPCs, implemented by shelling out to `git`.
//!
//! Wire shapes mirror the TypeScript server (`apps/server/src/vcs` +
//! `apps/server/src/git/GitManager.ts`): see `packages/contracts/src/git.ts`
//! for the `Vcs*`/`Git*` schemas these payloads and results follow.
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;
use tokio::time::{sleep, timeout};

use crate::handlers::RpcResult;
use crate::ws_rpc::{exit_failure, send_json, OutTx, Window};
use crate::AppState;

const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// Worktree adds, pulls and submodule updates can legitimately take minutes.
const GIT_TIMEOUT_LONG: Duration = Duration::from_secs(300);
const STATUS_POLL_INTERVAL: Duration = Duration::from_secs(3);
const LIST_REFS_DEFAULT_LIMIT: i64 = 100;

// ---------------------------------------------------------------------------
// Git process plumbing
// ---------------------------------------------------------------------------

struct GitOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn sargs(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn git_error(
    operation: &str,
    cwd: &Path,
    args: &[String],
    detail: &str,
    out: Option<&GitOutput>,
) -> Value {
    let mut e = json!({
        "_tag": "GitCommandError",
        "operation": operation,
        "command": "git",
        "cwd": cwd.to_string_lossy(),
        "argumentCount": args.len(),
        "detail": detail,
    });
    if let Some(o) = out {
        if let Some(code) = o.exit_code {
            e["exitCode"] = json!(code);
        }
        e["stdoutLength"] = json!(o.stdout.chars().count());
        e["stderrLength"] = json!(o.stderr.chars().count());
        if let Some(reason) = classify_git_failure(&o.stderr) {
            e["reason"] = json!(reason);
        }
    }
    e
}

async fn git(
    operation: &str,
    cwd: &Path,
    args: &[String],
    budget: Duration,
) -> Result<GitOutput, Value> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        // English diagnostics without overriding LC_CTYPE; concurrent status
        // polls must not take index.lock to save refreshed indexes.
        .env("LANGUAGE", "C")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .kill_on_drop(true);
    match timeout(budget, cmd.output()).await {
        Ok(Ok(o)) => Ok(GitOutput {
            exit_code: o.status.code(),
            stdout: String::from_utf8_lossy(&o.stdout).to_string(),
            stderr: String::from_utf8_lossy(&o.stderr).to_string(),
        }),
        Ok(Err(e)) => Err(git_error(
            operation,
            cwd,
            args,
            &format!("Failed to spawn Git process: {e}"),
            None,
        )),
        Err(_) => Err(git_error(operation, cwd, args, "Git command timed out.", None)),
    }
}

/// Run a command expected to succeed; nonzero exits become `GitCommandError`.
async fn git_ok(
    operation: &str,
    cwd: &Path,
    args: &[String],
    budget: Duration,
    fallback_detail: &str,
) -> Result<GitOutput, Value> {
    let out = git(operation, cwd, args, budget).await?;
    if out.exit_code == Some(0) {
        return Ok(out);
    }
    let reason = classify_git_failure(&out.stderr);
    let detail = reason
        .and_then(git_failure_detail)
        .unwrap_or(fallback_detail);
    Err(git_error(operation, cwd, args, detail, Some(&out)))
}

fn git_failure_detail(reason: &str) -> Option<&'static str> {
    match reason {
        "push_rejected_by_remote" => Some(
            "The remote rejected the push. Push this branch from a terminal to see the server's message.",
        ),
        "push_rejected_non_fast_forward" => Some(
            "The remote has commits this branch doesn't have. Pull or rebase, then push again.",
        ),
        "config_locked" => Some(
            "Another Git process kept the repository's config locked. Wait for it to finish, then retry.",
        ),
        _ => None,
    }
}

fn is_non_repo_stderr(stderr: &str) -> bool {
    stderr.to_lowercase().contains("not a git repository")
}

fn is_unborn_head_stderr(stderr: &str) -> bool {
    let normalized = stderr.to_lowercase();
    normalized.contains("bad revision 'head'")
        || (normalized.contains("unknown revision")
            && normalized.contains("path not in the working tree"))
}

/// Reduced port of the TS `classifyGitFailure`: fixed, credential-free
/// diagnoses only, matched against Git's own diagnostic lines.
fn classify_git_failure(stderr: &str) -> Option<&'static str> {
    let diagnostics: Vec<&str> = stderr
        .lines()
        .filter(|line| {
            let t = line.trim_start();
            t.starts_with("fatal:")
                || t.starts_with("error:")
                || t.starts_with('!')
                || line.to_lowercase().contains("permission denied")
                || line.to_lowercase().contains("host key verification failed")
        })
        .collect();
    if diagnostics.is_empty() {
        return None;
    }
    let d = diagnostics.join("\n").to_lowercase();
    if d.contains("not a git repository") {
        return Some("not_a_repository");
    }
    if d.contains("would clobber existing tag") {
        return Some("tag_would_be_clobbered");
    }
    if d.contains("is already used by worktree at") || d.contains("is already checked out at") {
        return Some("branch_checked_out_in_worktree");
    }
    if d.contains("a branch named") && d.contains("already exists") {
        return Some("branch_already_exists");
    }
    if d.contains("could not lock config file") && d.contains("file exists") {
        return Some("config_locked");
    }
    if d.contains("authentication failed")
        || d.contains("could not read username")
        || d.contains("could not read password")
        || d.contains("permission denied")
    {
        return Some("authentication_failed");
    }
    if d.contains("host key verification failed") {
        return Some("host_key_unverified");
    }
    if d.contains("could not read from remote repository")
        || d.contains("does not appear to be a git repository")
        || (d.contains("repository") && d.contains("not found"))
    {
        return Some("remote_unreachable");
    }
    if d.contains("invalid reference:")
        || d.contains("not a valid object name")
        || d.contains("needed a single revision")
        || d.contains("bad revision")
    {
        return Some("ref_not_found");
    }
    if d.contains("[remote rejected]") {
        return Some("push_rejected_by_remote");
    }
    if d.contains("[rejected]")
        && (d.contains("non-fast-forward") || d.contains("fetch first"))
        && d.contains("failed to push some refs")
    {
        return Some("push_rejected_non_fast_forward");
    }
    if d.contains("already exists") {
        return Some("path_already_exists");
    }
    None
}

// ---------------------------------------------------------------------------
// Small git-read helpers
// ---------------------------------------------------------------------------

fn normalize_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn resolve_under(cwd: &Path, value: &str) -> PathBuf {
    let p = Path::new(value);
    if p.is_absolute() {
        normalize_path(p)
    } else {
        normalize_path(&cwd.join(p))
    }
}

fn real_path(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn branch_name_from_head_ref(value: &str) -> Option<String> {
    let r = value.trim();
    r.strip_prefix("refs/heads/")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

struct RepoPaths {
    git_common_dir: PathBuf,
    worktree_root: Option<String>,
    is_at_root: Option<bool>,
    current_branch: Option<String>,
}

/// `resolveRepositoryPaths` — `None` when `cwd` is not inside a Git repository.
async fn resolve_repository_paths(cwd: &Path) -> Result<Option<RepoPaths>, Value> {
    let op = "GitVcsDriver.resolveRepositoryPaths";
    if !cwd.is_dir() {
        return Ok(None);
    }
    let common = git(
        op,
        cwd,
        &sargs(&["rev-parse", "--git-common-dir"]),
        GIT_TIMEOUT,
    )
    .await?;
    if common.exit_code != Some(0) {
        if is_non_repo_stderr(&common.stderr) {
            return Ok(None);
        }
        return Err(git_error(
            op,
            cwd,
            &sargs(&["rev-parse", "--git-common-dir"]),
            "Failed to resolve the Git common directory.",
            Some(&common),
        ));
    }
    let physical = real_path(cwd);
    let common_dir = std::fs::canonicalize(resolve_under(&physical, common.stdout.trim()))
        .unwrap_or_else(|_| resolve_under(&physical, common.stdout.trim()));

    let top_args = sargs(&["rev-parse", "--show-toplevel"]);
    let sym_args = sargs(&["symbolic-ref", "--quiet", "HEAD"]);
    let (top, sym) = tokio::join!(
        git(op, cwd, &top_args, GIT_TIMEOUT),
        git(op, cwd, &sym_args, GIT_TIMEOUT),
    );
    let worktree_root = match top {
        Ok(t) if t.exit_code == Some(0) => {
            // `--show-toplevel` terminates with exactly one "\n"; leading
            // whitespace belongs to the path and must survive.
            let out = t.stdout.strip_suffix('\n').unwrap_or(&t.stdout);
            if out.is_empty() {
                None
            } else {
                Some(
                    resolve_under(cwd, out)
                        .to_string_lossy()
                        .to_string(),
                )
            }
        }
        _ => None,
    };
    let is_at_root = worktree_root.as_ref().map(|root| real_path(cwd) == PathBuf::from(root));
    let current_branch = match sym {
        Ok(s) if s.exit_code == Some(0) => branch_name_from_head_ref(&s.stdout),
        _ => None,
    };
    Ok(Some(RepoPaths {
        git_common_dir: common_dir,
        worktree_root,
        is_at_root,
        current_branch,
    }))
}

async fn list_remote_names(cwd: &Path) -> Vec<String> {
    match git(
        "GitVcsDriver.listRemoteNames",
        cwd,
        &sargs(&["remote"]),
        GIT_TIMEOUT,
    )
    .await
    {
        Ok(r) if r.exit_code == Some(0) => r
            .stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => vec![],
    }
}

async fn config_value(cwd: &Path, key: &str) -> Option<String> {
    let args = vec!["config".into(), "--get".into(), key.into()];
    match git("GitVcsDriver.readConfigValue", cwd, &args, GIT_TIMEOUT).await {
        Ok(r) => {
            let t = r.stdout.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        Err(_) => None,
    }
}

async fn write_config_value(cwd: &Path, key: &str, value: &str) -> Result<(), Value> {
    let args = vec!["config".into(), key.into(), value.into()];
    git_ok(
        "GitVcsDriver.writeConfigValue",
        cwd,
        &args,
        GIT_TIMEOUT,
        "git config write failed",
    )
    .await?;
    Ok(())
}

async fn resolve_default_branch_name(cwd: &Path, remote: &str) -> Option<String> {
    let ref_name = format!("refs/remotes/{remote}/HEAD");
    let args = sargs(&["symbolic-ref", &ref_name]);
    match git("GitVcsDriver.resolveDefaultBranchName", cwd, &args, GIT_TIMEOUT).await {
        Ok(r) if r.exit_code == Some(0) => {
            let prefix = format!("refs/remotes/{remote}/");
            r.stdout
                .trim()
                .strip_prefix(&prefix)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        }
        _ => None,
    }
}

async fn remote_exists(cwd: &Path, remote: &str) -> bool {
    let args = vec!["remote".into(), "get-url".into(), "--".into(), remote.into()];
    matches!(
        git("GitVcsDriver.remoteExists", cwd, &args, GIT_TIMEOUT).await,
        Ok(r) if r.exit_code == Some(0)
    )
}

async fn primary_remote_name(cwd: &Path) -> Option<String> {
    if remote_exists(cwd, "origin").await {
        return Some("origin".to_string());
    }
    list_remote_names(cwd).await.into_iter().next()
}

async fn branch_exists(cwd: &Path, ref_name: &str) -> bool {
    let args = sargs(&[
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/heads/{ref_name}"),
    ]);
    matches!(
        git("GitVcsDriver.branchExists", cwd, &args, GIT_TIMEOUT).await,
        Ok(r) if r.exit_code == Some(0)
    )
}

async fn remote_branch_exists(cwd: &Path, remote: &str, ref_name: &str) -> bool {
    let args = sargs(&[
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/remotes/{remote}/{ref_name}"),
    ]);
    matches!(
        git("GitVcsDriver.remoteBranchExists", cwd, &args, GIT_TIMEOUT).await,
        Ok(r) if r.exit_code == Some(0)
    )
}

/// Parsed `{upstream}` in short `remote/branch` form.
struct Upstream {
    upstream_ref: String,
    #[allow(dead_code)]
    remote_name: String,
    #[allow(dead_code)]
    branch_name: String,
}

fn parse_remote_ref(ref_str: &str, remote_names: &[String]) -> Option<(String, String)> {
    let trimmed = ref_str.trim();
    if trimmed.is_empty() {
        return None;
    }
    for remote in remote_names {
        let prefix = format!("{remote}/");
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let branch = rest.trim();
            if branch.is_empty() {
                return None;
            }
            return Some((remote.clone(), branch.to_string()));
        }
    }
    None
}

fn parse_upstream_first_separator(upstream_ref: &str) -> Option<(String, String)> {
    let i = upstream_ref.find('/')?;
    if i == 0 || i == upstream_ref.len() - 1 {
        return None;
    }
    let remote = upstream_ref[..i].trim();
    let branch = upstream_ref[i + 1..].trim();
    if remote.is_empty() || branch.is_empty() {
        return None;
    }
    Some((remote.to_string(), branch.to_string()))
}

async fn read_current_upstream(cwd: &Path) -> Option<Upstream> {
    let args = sargs(&["rev-parse", "--symbolic-full-name", "@{upstream}"]);
    let r = git("GitVcsDriver.resolveCurrentUpstream", cwd, &args, GIT_TIMEOUT)
        .await
        .ok()?;
    let mut short = r.stdout.trim();
    if let Some(rest) = short.strip_prefix("refs/remotes/") {
        short = rest;
    } else if let Some(rest) = short.strip_prefix("refs/heads/") {
        short = rest;
    }
    if short.is_empty() || short == "@{upstream}" {
        return None;
    }
    let mut remote_names = list_remote_names(cwd).await;
    remote_names.sort_by(|a, b| b.len().cmp(&a.len()));
    let parsed = parse_remote_ref(short, &remote_names)
        .or_else(|| parse_upstream_first_separator(short))?;
    Some(Upstream {
        upstream_ref: short.to_string(),
        remote_name: parsed.0,
        branch_name: parsed.1,
    })
}

/// `resolveComparisonBaseRef`: verify a base ref exists, returning the ref to
/// diff against (`refs/heads/...` or `refs/remotes/...`).
async fn comparison_ref(cwd: &Path, base_ref: &str) -> Option<String> {
    let candidate = if base_ref.starts_with("refs/heads/") || base_ref.starts_with("refs/remotes/")
    {
        base_ref.to_string()
    } else if base_ref.contains('/') && !base_ref.starts_with("refs/") {
        format!("refs/remotes/{base_ref}")
    } else {
        return None;
    };
    let args = sargs(&["show-ref", "--verify", "--quiet", &candidate]);
    match git("GitVcsDriver.resolveComparisonBaseRef", cwd, &args, GIT_TIMEOUT).await {
        Ok(r) if r.exit_code == Some(0) => Some(candidate),
        _ => None,
    }
}

/// `resolveBaseBranchForNoUpstream` reduced to the comparison ref it yields.
async fn base_comparison_ref(
    cwd: &Path,
    ref_name: &str,
    known_origin_default: Option<&str>,
) -> Option<String> {
    if let Some(configured) =
        config_value(cwd, &format!("branch.{ref_name}.t3-merge-base-ref")).await
    {
        if configured.starts_with("refs/heads/") || configured.starts_with("refs/remotes/") {
            if let Some(comp) = comparison_ref(cwd, &configured).await {
                return Some(comp);
            }
        }
    }
    let configured_base = config_value(cwd, &format!("branch.{ref_name}.t3-merge-base"))
        .await
        .or(config_value(cwd, &format!("branch.{ref_name}.gh-merge-base")).await);
    if let Some(cb) = &configured_base {
        if let Some(comp) = comparison_ref(cwd, cb).await {
            if cb.contains('/') && !cb.starts_with("refs/") && branch_exists(cwd, cb).await {
                return Some(format!("refs/heads/{cb}"));
            }
            return Some(comp);
        }
    }

    let primary = primary_remote_name(cwd).await;
    let mut candidates: Vec<Option<String>> = vec![configured_base.clone()];
    match &primary {
        Some(p) => {
            let candidate = if p == "origin" && known_origin_default.is_some() {
                known_origin_default.map(str::to_string)
            } else {
                resolve_default_branch_name(cwd, p).await
            };
            candidates.push(candidate);
        }
        None => candidates.push(None),
    }
    candidates.push(Some("main".into()));
    candidates.push(Some("master".into()));

    for candidate in candidates.into_iter().flatten() {
        let remote_prefix = primary
            .as_ref()
            .filter(|p| p.as_str() != "origin")
            .map(|p| format!("{p}/"));
        let normalized = candidate
            .strip_prefix("origin/")
            .map(str::to_string)
            .or_else(|| {
                remote_prefix
                    .as_ref()
                    .and_then(|rp| candidate.strip_prefix(rp.as_str()).map(str::to_string))
            })
            .unwrap_or(candidate.clone());
        if normalized.is_empty() || normalized == ref_name {
            continue;
        }
        if let Some(p) = &primary {
            if remote_branch_exists(cwd, p, &normalized).await {
                return Some(format!("refs/remotes/{p}/{normalized}"));
            }
        }
        if branch_exists(cwd, &normalized).await {
            return Some(format!("refs/heads/{normalized}"));
        }
    }
    None
}

async fn compute_ahead_count_against_base(
    cwd: &Path,
    ref_name: &str,
    known_origin_default: Option<&str>,
) -> i64 {
    let Some(base) = base_comparison_ref(cwd, ref_name, known_origin_default).await else {
        return 0;
    };
    let args = sargs(&[
        "rev-list",
        "--count",
        "--end-of-options",
        &format!("{base}..HEAD"),
    ]);
    match git("GitVcsDriver.computeAheadCountAgainstBase", cwd, &args, GIT_TIMEOUT).await {
        Ok(r) if r.exit_code == Some(0) => r
            .stdout
            .trim()
            .parse::<i64>()
            .unwrap_or(0)
            .max(0),
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Status reads
// ---------------------------------------------------------------------------

fn parse_numstat(stdout: &str) -> Vec<(String, i64, i64)> {
    let fields: Vec<&str> = stdout.split('\0').collect();
    let mut files = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let field = fields[i];
        i += 1;
        let mut parts = field.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if path.is_empty() {
            // Rename records carry the paths in the two following NUL fields.
            i += 1;
            let Some(new_path) = fields.get(i) else { break };
            files.push((
                (*new_path).to_string(),
                added.parse::<i64>().unwrap_or(0),
                deleted.parse::<i64>().unwrap_or(0),
            ));
            i += 1;
            continue;
        }
        files.push((
            path.to_string(),
            added.parse::<i64>().unwrap_or(0),
            deleted.parse::<i64>().unwrap_or(0),
        ));
    }
    files
}

/// Porcelain v2 field counts preceding the path per record type.
fn parse_porcelain_path(record: &str) -> Option<String> {
    let before = match record.as_bytes().first()? {
        b'1' => 8,
        b'2' => 9,
        b'u' => 10,
        b'?' | b'!' => 1,
        _ => return None,
    };
    let path = record.split(' ').skip(before).collect::<Vec<_>>().join(" ");
    (!path.is_empty()).then_some(path)
}

struct LocalDetails {
    is_repo: bool,
    ref_name: Option<String>,
    upstream_ref: Option<String>,
    has_primary_remote: bool,
    is_default: bool,
    has_working_tree_changes: bool,
    root_path: Option<String>,
    files: Vec<Value>,
    insertions: i64,
    deletions: i64,
}

fn local_status_value(d: &LocalDetails) -> Value {
    let mut working_tree = json!({
        "files": d.files,
        "insertions": d.insertions,
        "deletions": d.deletions,
    });
    if let Some(root) = &d.root_path {
        working_tree["rootPath"] = json!(root);
    }
    json!({
        "isRepo": d.is_repo,
        "hasPrimaryRemote": d.has_primary_remote,
        "isDefaultRef": d.is_default,
        "refName": d.ref_name,
        "hasWorkingTreeChanges": d.has_working_tree_changes,
        "workingTree": working_tree,
    })
}

fn non_repo_local() -> LocalDetails {
    LocalDetails {
        is_repo: false,
        ref_name: None,
        upstream_ref: None,
        has_primary_remote: false,
        is_default: false,
        has_working_tree_changes: false,
        root_path: None,
        files: vec![],
        insertions: 0,
        deletions: 0,
    }
}

/// `readStatusDetailsLocal` with `includeDivergence: false`.
async fn read_local_details(cwd: &Path) -> Result<LocalDetails, Value> {
    let op = "GitVcsDriver.statusDetails";
    // A missing cwd behaves like a non-repository for status reads.
    if !cwd.is_dir() {
        return Ok(non_repo_local());
    }
    let index_args = sargs(&["rev-parse", "--git-path", "index"]);
    let index = git(op, cwd, &index_args, GIT_TIMEOUT).await?;
    if index.exit_code != Some(0) {
        if is_non_repo_stderr(&index.stderr) {
            return Ok(non_repo_local());
        }
        return Err(git_error(
            op,
            cwd,
            &index_args,
            "Git status failed.",
            Some(&index),
        ));
    }
    let index_path = resolve_under(cwd, index.stdout.trim());
    if index_path.with_extension("lock").exists()
        || PathBuf::from(format!("{}.lock", index_path.to_string_lossy())).exists()
    {
        return Err(git_error(
            "GitVcsDriver.statusDetails.indexPath",
            cwd,
            &sargs(&["rev-parse", "--git-path", "index"]),
            "Git index is locked. Status will resume when the index lock is removed.",
            None,
        ));
    }

    let status_args = sargs(&[
        "status",
        "--porcelain=2",
        "--branch",
        "-z",
        "--no-ahead-behind",
    ]);
    let status = git(op, cwd, &status_args, GIT_TIMEOUT).await?;
    if status.exit_code != Some(0) {
        if is_non_repo_stderr(&status.stderr) {
            return Ok(non_repo_local());
        }
        return Err(git_error(op, cwd, &status_args, "Git status failed.", Some(&status)));
    }

    // diff-index does not read status output; plumbing with -M keeps rename
    // detection without rewriting indexes despite GIT_OPTIONAL_LOCKS=0.
    let numstat_args = sargs(&["diff-index", "-M", "--numstat", "-z", "HEAD", "--"]);
    let numstat = git(op, cwd, &numstat_args, GIT_TIMEOUT).await?;
    let numstat_entries = if numstat.exit_code == Some(0) {
        parse_numstat(&numstat.stdout)
    } else if is_unborn_head_stderr(&numstat.stderr) {
        let unstaged_args = sargs(&["diff-files", "--numstat", "-z"]);
        let staged_args = sargs(&["diff", "--cached", "--numstat", "-z"]);
        let (unstaged, staged) = tokio::join!(
            git(op, cwd, &unstaged_args, GIT_TIMEOUT),
            git(op, cwd, &staged_args, GIT_TIMEOUT),
        );
        let mut entries = Vec::new();
        if let Ok(s) = staged {
            entries.extend(parse_numstat(&s.stdout));
        }
        if let Ok(u) = unstaged {
            entries.extend(parse_numstat(&u.stdout));
        }
        entries
    } else {
        return Err(git_error(
            op,
            cwd,
            &numstat_args,
            "git diff-index HEAD --numstat failed.",
            Some(&numstat),
        ));
    };

    let top_args = sargs(&["rev-parse", "--show-toplevel"]);
    let (top, default_branch, remote_names) = tokio::join!(
        git(op, cwd, &top_args, GIT_TIMEOUT),
        resolve_default_branch_name(cwd, "origin"),
        list_remote_names(cwd),
    );
    let root_path = match top {
        Ok(t) if t.exit_code == Some(0) => {
            let out = t.stdout.strip_suffix('\n').unwrap_or(&t.stdout);
            (!out.is_empty()).then(|| resolve_under(cwd, out).to_string_lossy().to_string())
        }
        _ => None,
    };

    let mut ref_name: Option<String> = None;
    let mut upstream_ref: Option<String> = None;
    let mut has_changes = false;
    let mut changed_without_numstat: Vec<String> = Vec::new();
    let records: Vec<&str> = status.stdout.split('\0').collect();
    let mut i = 0;
    while i < records.len() {
        let line = records[i];
        i += 1;
        if let Some(v) = line.strip_prefix("# branch.head ") {
            let v = v.trim();
            ref_name = (!v.starts_with('(') && !v.is_empty()).then(|| v.to_string());
            continue;
        }
        if let Some(v) = line.strip_prefix("# branch.upstream ") {
            let v = v.trim();
            upstream_ref = (!v.is_empty()).then(|| v.to_string());
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        has_changes = true;
        if let Some(p) = parse_porcelain_path(line) {
            changed_without_numstat.push(p);
        }
        // A rename record is followed by its original path as its own record.
        if line.starts_with("2 ") {
            if let Some(orig) = records.get(i) {
                if !orig.is_empty() {
                    changed_without_numstat.push((*orig).to_string());
                }
            }
            i += 1;
        }
    }

    let mut stat_map: HashMap<String, (i64, i64)> = HashMap::new();
    for (path, ins, del) in numstat_entries {
        let e = stat_map.entry(path).or_default();
        e.0 += ins;
        e.1 += del;
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut files: Vec<(String, i64, i64)> = Vec::new();
    for (path, (ins, del)) in stat_map {
        seen.insert(path.clone());
        files.push((path, ins, del));
    }
    for path in changed_without_numstat {
        if seen.insert(path.clone()) {
            files.push((path, 0, 0));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut insertions = 0;
    let mut deletions = 0;
    let file_values: Vec<Value> = files
        .into_iter()
        .map(|(path, ins, del)| {
            insertions += ins;
            deletions += del;
            json!({ "path": path, "insertions": ins, "deletions": del })
        })
        .collect();

    let is_default = match (&ref_name, &default_branch) {
        (Some(b), Some(d)) => b == d,
        (Some(b), None) => b == "main" || b == "master",
        _ => false,
    };

    Ok(LocalDetails {
        is_repo: true,
        ref_name,
        upstream_ref,
        has_primary_remote: !remote_names.is_empty(),
        is_default,
        has_working_tree_changes: has_changes,
        root_path,
        files: file_values,
        insertions,
        deletions,
    })
}

struct RemoteDetails {
    has_upstream: bool,
    ahead_count: i64,
    behind_count: i64,
    ahead_of_default_count: i64,
}

fn remote_status_value(d: &RemoteDetails) -> Value {
    json!({
        "hasUpstream": d.has_upstream,
        "aheadCount": d.ahead_count,
        "behindCount": d.behind_count,
        "aheadOfDefaultCount": d.ahead_of_default_count,
        "pr": Value::Null,
    })
}

/// `readStatusDetailsRemote` without upstream fetching or PR lookup (`pr`
/// stays null — PR resolution lives with the source-control providers).
async fn read_remote_details(cwd: &Path) -> Result<Option<RemoteDetails>, Value> {
    let op = "GitVcsDriver.statusDetailsRemote";
    let head_args = sargs(&["rev-parse", "--symbolic-full-name", "HEAD"]);
    let head = match git(op, cwd, &head_args, GIT_TIMEOUT).await {
        Ok(r) => r,
        Err(e) => return Err(e),
    };
    let branch: Option<String>;
    if head.exit_code == Some(0) {
        branch = branch_name_from_head_ref(&head.stdout);
    } else {
        if is_non_repo_stderr(&head.stderr) {
            return Ok(None);
        }
        if !is_unborn_head_stderr(&head.stderr) {
            return Err(git_error(op, cwd, &head_args, "Git branch lookup failed.", Some(&head)));
        }
        let sym = git(op, cwd, &sargs(&["symbolic-ref", "--quiet", "HEAD"]), GIT_TIMEOUT)
            .await?;
        branch = branch_name_from_head_ref(&sym.stdout);
    }

    let upstream = read_current_upstream(cwd).await;
    let mut ahead = 0i64;
    let mut behind = 0i64;
    if upstream.is_some() {
        let args = sargs(&["rev-list", "--left-right", "--count", "HEAD...@{upstream}"]);
        if let Ok(d) = git(op, cwd, &args, GIT_TIMEOUT).await {
            if d.exit_code == Some(0) {
                let mut parts = d.stdout.trim().split_whitespace();
                ahead = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                behind = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
        }
    } else if let Some(b) = &branch {
        ahead = compute_ahead_count_against_base(cwd, b, None).await;
    }

    let default_branch = resolve_default_branch_name(cwd, "origin").await;
    let is_default = match (&branch, &default_branch) {
        (Some(b), Some(d)) => b == d,
        (Some(b), None) => b == "main" || b == "master",
        _ => false,
    };
    let ahead_of_default = match (&branch, is_default) {
        (Some(b), false) => {
            if upstream.is_none() {
                ahead
            } else {
                compute_ahead_count_against_base(cwd, b, default_branch.as_deref()).await
            }
        }
        _ => 0,
    };

    Ok(Some(RemoteDetails {
        has_upstream: upstream.is_some(),
        ahead_count: ahead,
        behind_count: behind,
        ahead_of_default_count: ahead_of_default,
    }))
}

fn merged_status(local: &LocalDetails, remote: Option<&RemoteDetails>) -> Value {
    let mut v = local_status_value(local);
    let obj = v.as_object_mut().unwrap();
    match remote {
        Some(r) => {
            obj.insert("hasUpstream".into(), json!(r.has_upstream));
            obj.insert("aheadCount".into(), json!(r.ahead_count));
            obj.insert("behindCount".into(), json!(r.behind_count));
            obj.insert("aheadOfDefaultCount".into(), json!(r.ahead_of_default_count));
        }
        None => {
            obj.insert("hasUpstream".into(), json!(false));
            obj.insert("aheadCount".into(), json!(0));
            obj.insert("behindCount".into(), json!(0));
            obj.insert("aheadOfDefaultCount".into(), json!(0));
        }
    }
    obj.insert("pr".into(), Value::Null);
    v
}

// ---------------------------------------------------------------------------
// Request helpers
// ---------------------------------------------------------------------------

/// Resolve the working directory: `cwd` wins, `projectId` resolves through
/// `projection_projects.workspace_root`.
async fn resolve_cwd(state: &AppState, payload: &Value) -> Result<PathBuf, Value> {
    if let Some(cwd) = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        return Ok(PathBuf::from(cwd));
    }
    if let Some(project_id) = payload
        .get("projectId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        if let Some(project) = state.store.project_shell(project_id).await {
            if let Some(root) = project.get("workspaceRoot").and_then(Value::as_str) {
                return Ok(PathBuf::from(root));
            }
        }
        return Err(json!({
            "_tag": "GitManagerError",
            "operation": "vcs.resolveCwd",
            "cwd": "",
            "detail": format!("Unknown project: {project_id}"),
        }));
    }
    Err(json!({
        "_tag": "GitManagerError",
        "operation": "vcs.resolveCwd",
        "cwd": "",
        "detail": "Missing cwd.",
    }))
}

fn refuse_option_like(operation: &str, cwd: &Path, ref_name: Option<&str>) -> Result<(), Value> {
    match ref_name {
        Some(name) if name != "-" && name.starts_with('-') => Err(git_error(
            operation,
            cwd,
            &sargs(&["git"]),
            "Ref names cannot start with '-'.",
            None,
        )),
        _ => Ok(()),
    }
}

fn str_field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// vcs.* request handlers
// ---------------------------------------------------------------------------

fn parse_worktree_branch_paths(stdout: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut prunable = false;
    // Trailing "" flushes the last record.
    for field in stdout.split('\0').chain(std::iter::once("")) {
        if field.is_empty() {
            if let (Some(p), Some(b)) = (path.take(), branch.take()) {
                if !prunable {
                    map.insert(b, p);
                }
            }
            prunable = false;
            continue;
        }
        if let Some(p) = field.strip_prefix("worktree ") {
            path = Some(p.to_string());
        } else if let Some(b) = field.strip_prefix("branch refs/heads/") {
            branch = Some(b.to_string());
        } else if field == "prunable" || field.starts_with("prunable ") {
            prunable = true;
        }
    }
    map
}

async fn list_refs(state: &AppState, payload: &Value) -> Result<Value, Value> {
    let cwd = resolve_cwd(state, payload).await?;
    let Some(paths) = resolve_repository_paths(&cwd).await? else {
        return Ok(json!({
            "refs": [],
            "isRepo": false,
            "hasPrimaryRemote": false,
            "nextCursor": Value::Null,
            "totalCount": 0,
        }));
    };
    let common = &paths.git_common_dir;
    // `--git-dir` needs a readable cwd; the worktree parent serves when the
    // common dir is itself the checkout's `.git`.
    let fetch_cwd = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().map(Path::to_path_buf).unwrap_or_else(|| common.clone())
    } else {
        common.clone()
    };
    let gd = common.to_string_lossy().to_string();
    let refs_args = sargs(&[
        "--git-dir",
        &gd,
        "for-each-ref",
        "--format=%(refname)%09%(committerdate:unix)%09%(symref)%09%(upstream)",
        "refs/heads",
        "refs/remotes",
    ]);
    let default_args = sargs(&["--git-dir", &gd, "symbolic-ref", "refs/remotes/origin/HEAD"]);
    let wt_args = sargs(&["--git-dir", &gd, "worktree", "list", "--porcelain", "-z"]);
    let remote_args = sargs(&["--git-dir", &gd, "remote"]);
    let (refs_r, default_r, wt_r, remotes_r) = tokio::join!(
        git(
            "GitVcsDriver.listRefs.snapshotRefs",
            &fetch_cwd,
            &refs_args,
            GIT_TIMEOUT,
        ),
        git(
            "GitVcsDriver.listRefs.defaultRef",
            &fetch_cwd,
            &default_args,
            GIT_TIMEOUT,
        ),
        git(
            "GitVcsDriver.listRefs.worktreeList",
            &fetch_cwd,
            &wt_args,
            GIT_TIMEOUT,
        ),
        git(
            "GitVcsDriver.listRefs.remoteNames",
            &fetch_cwd,
            &remote_args,
            GIT_TIMEOUT,
        ),
    );
    let refs_out = refs_r?;
    if refs_out.exit_code != Some(0) {
        return Err(git_error(
            "GitVcsDriver.listRefs.snapshotRefs",
            &fetch_cwd,
            &sargs(&["--git-dir", &gd, "for-each-ref"]),
            "Git ref snapshot enumeration failed.",
            Some(&refs_out),
        ));
    }
    let mut remote_names: Vec<String> = match remotes_r {
        Ok(r) if r.exit_code == Some(0) => r
            .stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => vec![],
    };
    remote_names.sort_by(|a, b| b.len().cmp(&a.len()));
    let default_branch = match default_r {
        Ok(r) if r.exit_code == Some(0) => r
            .stdout
            .trim()
            .strip_prefix("refs/remotes/origin/")
            .map(str::to_string),
        _ => None,
    };
    let normalized_common = normalize_path(&real_path(common));
    let worktree_map: HashMap<String, String> = match wt_r {
        Ok(r) if r.exit_code == Some(0) => parse_worktree_branch_paths(&r.stdout)
            .into_iter()
            .map(|(b, p)| (b, normalize_path(&resolve_under(&fetch_cwd, &p))))
            .filter(|(_, p)| {
                *p != normalized_common && p.exists()
            })
            .map(|(b, p)| (b, p.to_string_lossy().to_string()))
            .collect(),
        _ => HashMap::new(),
    };

    let mut local: Vec<(i64, Value)> = Vec::new();
    let mut remote: Vec<(i64, Value)> = Vec::new();
    for line in refs_out.stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        let (Some(full), Some(ts), symref, upstream) =
            (f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        if !symref.unwrap_or("").is_empty() {
            continue;
        }
        let last_commit: i64 = ts.parse().unwrap_or(0);
        if let Some(name) = full.strip_prefix("refs/heads/") {
            let mut r = json!({
                "name": name,
                "current": false,
                "isRemote": false,
                "isDefault": Some(name) == default_branch.as_deref(),
                "worktreePath": worktree_map
                    .get(name)
                    .map(|p| json!(p))
                    .unwrap_or(Value::Null),
            });
            if let Some(u) = upstream.filter(|u| !u.is_empty()) {
                r["upstreamRef"] = json!(u);
            }
            local.push((last_commit, r));
            continue;
        }
        if let Some(name) = full.strip_prefix("refs/remotes/") {
            let parsed = parse_remote_ref(name, &remote_names);
            let mut r = json!({
                "name": name,
                "current": false,
                "isRemote": true,
                "isDefault": default_branch.is_some()
                    && parsed.as_ref().is_some_and(|(rn, bn)| {
                        rn == "origin" && Some(bn.as_str()) == default_branch.as_deref()
                    }),
                "worktreePath": Value::Null,
            });
            if let Some((rn, _)) = parsed {
                r["remoteName"] = json!(rn);
            }
            remote.push((last_commit, r));
        }
    }
    let by_recency_then_name = |a: &(i64, Value), b: &(i64, Value)| {
        b.0.cmp(&a.0).then_with(|| {
            a.1["name"].as_str().unwrap_or("").cmp(b.1["name"].as_str().unwrap_or(""))
        })
    };
    local.sort_by(by_recency_then_name);
    remote.sort_by(by_recency_then_name);

    let worktree_root = paths.worktree_root.clone();
    let has_current_worktree_branch = worktree_root.as_ref().is_some_and(|root| {
        local
            .iter()
            .any(|(_, r)| r["worktreePath"].as_str() == Some(root.as_str()))
    });
    let locals: Vec<Value> = local
        .into_iter()
        .map(|(_, mut r)| {
            let original_wtp = r["worktreePath"].as_str().map(str::to_string);
            let name = r["name"].as_str().unwrap_or("").to_string();
            if r["worktreePath"].is_null() {
                if Some(&name) == paths.current_branch.as_ref() {
                    r["worktreePath"] = worktree_root
                        .as_ref()
                        .map(|w| json!(w))
                        .unwrap_or(Value::Null);
                }
            }
            let current = if has_current_worktree_branch {
                original_wtp.as_deref() == worktree_root.as_deref()
            } else {
                Some(&name) == paths.current_branch.as_ref()
            };
            r["current"] = json!(current);
            r
        })
        .collect();

    let include_matching = payload
        .get("includeMatchingRemoteRefs")
        .and_then(Value::as_bool)
        == Some(true);
    let mut combined = locals;
    if include_matching {
        combined.extend(remote.into_iter().map(|(_, r)| r));
    } else {
        let local_names: HashSet<String> = combined
            .iter()
            .filter_map(|r| r["name"].as_str().map(str::to_string))
            .collect();
        combined.extend(remote.into_iter().map(|(_, r)| r).filter(|r| {
            if r["remoteName"].as_str() != Some("origin") {
                return true;
            }
            let name = r["name"].as_str().unwrap_or("");
            let derived = name
                .find('/')
                .filter(|i| *i > 0 && *i < name.len() - 1)
                .map(|i| &name[i + 1..])
                .unwrap_or(name);
            derived.is_empty() || !local_names.contains(derived)
        }));
    }
    combined.sort_by_key(|r| {
        if r["current"] == Value::Bool(true) {
            0
        } else if r["isDefault"] == Value::Bool(true) {
            1
        } else {
            2
        }
    });
    let kind = payload.get("refKind").and_then(Value::as_str).unwrap_or("all");
    let query = payload
        .get("query")
        .and_then(Value::as_str)
        .map(|q| q.to_lowercase());
    let filtered: Vec<Value> = combined
        .into_iter()
        .filter(|r| match kind {
            "local" => r["isRemote"] != Value::Bool(true),
            "remote" => r["isRemote"] == Value::Bool(true),
            _ => true,
        })
        .filter(|r| match &query {
            Some(q) => r["name"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .contains(q.as_str()),
            None => true,
        })
        .collect();
    let cursor = payload.get("cursor").and_then(Value::as_i64).unwrap_or(0).max(0) as usize;
    let limit = payload
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(LIST_REFS_DEFAULT_LIMIT)
        .max(1) as usize;
    let total = filtered.len();
    let page: Vec<Value> = filtered.into_iter().skip(cursor).take(limit).collect();
    let next_cursor = if cursor + page.len() < total {
        json!(cursor + page.len())
    } else {
        Value::Null
    };
    let mut out = json!({
        "refs": page,
        "isRepo": true,
        "hasPrimaryRemote": !remote_names.is_empty(),
        "nextCursor": next_cursor,
        "totalCount": total,
    });
    if let Some(at_root) = paths.is_at_root {
        out["isAtRepositoryRoot"] = json!(at_root);
    }
    Ok(out)
}

async fn refresh_status(state: &AppState, payload: &Value) -> Result<Value, Value> {
    let cwd = resolve_cwd(state, payload).await?;
    let local_only = payload.get("localOnly").and_then(Value::as_bool) == Some(true);
    let local = read_local_details(&cwd).await?;
    let remote = if local_only || !local.is_repo {
        None
    } else {
        read_remote_details(&cwd).await?
    };
    Ok(merged_status(&local, remote.as_ref()))
}

async fn switch_ref_at(cwd: &Path, ref_name: &str) -> Result<Value, Value> {
    let op = "GitVcsDriver.switchRef";
    let (local_exists, remote_exists_r) = tokio::join!(
        async {
            let a = sargs(&[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{ref_name}"),
            ]);
            matches!(git(op, cwd, &a, GIT_TIMEOUT).await, Ok(r) if r.exit_code == Some(0))
        },
        async {
            let a = sargs(&[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/remotes/{ref_name}"),
            ]);
            matches!(git(op, cwd, &a, GIT_TIMEOUT).await, Ok(r) if r.exit_code == Some(0))
        },
    );
    let local_tracking_branch = if remote_exists_r && !local_exists {
        let a = sargs(&[
            "for-each-ref",
            "--format=%(refname:short)\t%(upstream:short)",
            "refs/heads",
        ]);
        match git(op, cwd, &a, GIT_TIMEOUT).await {
            Ok(r) if r.exit_code == Some(0) => r.stdout.lines().find_map(|line| {
                let mut parts = line.split('\t');
                let branch = parts.next().unwrap_or("").trim();
                let upstream = parts.next().unwrap_or("").trim();
                (!branch.is_empty() && upstream == ref_name).then(|| branch.to_string())
            }),
            _ => None,
        }
    } else {
        None
    };
    let local_tracked_candidate = ref_name
        .find('/')
        .filter(|i| *i > 0 && *i < ref_name.len() - 1)
        .map(|i| ref_name[i + 1..].trim().to_string())
        .filter(|s| !s.is_empty());
    let local_tracked_target_exists = remote_exists_r
        && !local_exists
        && local_tracking_branch.is_none()
        && match &local_tracked_candidate {
            Some(c) => branch_exists(cwd, c).await,
            None => false,
        };

    let mut checkout_args: Vec<String> = if local_exists {
        sargs(&["checkout", ref_name])
    } else if remote_exists_r && local_tracking_branch.is_none() && local_tracked_target_exists {
        sargs(&["checkout", ref_name])
    } else if remote_exists_r && local_tracking_branch.is_none() {
        sargs(&["checkout", "--track", ref_name])
    } else if remote_exists_r {
        sargs(&["checkout", local_tracking_branch.as_deref().unwrap_or(ref_name)])
    } else {
        sargs(&["checkout", ref_name])
    };
    refuse_option_like(op, cwd, checkout_args.last().map(String::as_str))?;
    // A stale ref must not turn into a path checkout that discards edits.
    checkout_args.push("--".into());
    git_ok(op, cwd, &checkout_args, GIT_TIMEOUT, "git checkout failed").await?;

    let branch_args = sargs(&["branch", "--show-current"]);
    let out = git(op, cwd, &branch_args, GIT_TIMEOUT).await?;
    let name = out.stdout.trim();
    Ok(json!({ "refName": (!name.is_empty()).then_some(name) }))
}

async fn switch_ref(state: &AppState, payload: &Value) -> Result<Value, Value> {
    let cwd = resolve_cwd(state, payload).await?;
    let ref_name = str_field(payload, "refName").ok_or_else(|| {
        git_error(
            "GitVcsDriver.switchRef",
            &cwd,
            &sargs(&["checkout"]),
            "Missing refName.",
            None,
        )
    })?;
    switch_ref_at(&cwd, ref_name).await
}

async fn create_ref(state: &AppState, payload: &Value) -> Result<Value, Value> {
    let cwd = resolve_cwd(state, payload).await?;
    let ref_name = str_field(payload, "refName").ok_or_else(|| {
        git_error(
            "GitVcsDriver.createRef",
            &cwd,
            &sargs(&["branch"]),
            "Missing refName.",
            None,
        )
    })?;
    let args = sargs(&["branch", "--", ref_name]);
    git_ok(
        "GitVcsDriver.createRef",
        &cwd,
        &args,
        GIT_TIMEOUT,
        "git branch create failed",
    )
    .await?;
    if payload.get("switchRef").and_then(Value::as_bool) == Some(true) {
        switch_ref_at(&cwd, ref_name).await?;
    }
    Ok(json!({ "refName": ref_name }))
}

async fn vcs_init(state: &AppState, payload: &Value) -> Result<Value, Value> {
    let cwd = resolve_cwd(state, payload).await?;
    match payload.get("kind").and_then(Value::as_str) {
        None | Some("git") => {}
        Some(kind) => {
            return Err(json!({
                "_tag": "VcsUnsupportedOperationError",
                "operation": "VcsProvisioningService.resolveRequestedKind",
                "kind": kind,
                "detail": "This server only supports the Git VCS driver.",
            }));
        }
    }
    git_ok(
        "GitVcsDriver.initRepo",
        &cwd,
        &sargs(&["init"]),
        GIT_TIMEOUT,
        "git init failed",
    )
    .await?;
    Ok(Value::Null)
}

async fn vcs_pull(state: &AppState, payload: &Value) -> Result<Value, Value> {
    let cwd = resolve_cwd(state, payload).await?;
    let op = "GitVcsDriver.pullCurrentBranch";
    let details = read_local_details(&cwd).await?;
    let ref_name = details.ref_name.clone().ok_or_else(|| {
        git_error(op, &cwd, &sargs(&["pull", "--ff-only"]), "Cannot pull from detached HEAD.", None)
    })?;
    if details.upstream_ref.is_none() {
        return Err(git_error(
            op,
            &cwd,
            &sargs(&["pull", "--ff-only"]),
            "Current branch has no upstream configured. Push with upstream first.",
            None,
        ));
    }
    let rev = sargs(&["rev-parse", "HEAD"]);
    let before = git_ok(op, &cwd, &rev, GIT_TIMEOUT, "git rev-parse HEAD failed")
        .await?
        .stdout
        .trim()
        .to_string();
    let pull_args = sargs(&["pull", "--ff-only"]);
    let result = git(op, &cwd, &pull_args, GIT_TIMEOUT_LONG).await?;
    if result.exit_code != Some(0) {
        let reason = classify_git_failure(&result.stderr);
        let detail = fetch_failure_detail(&result.stderr)
            .or_else(|| reason.and_then(git_failure_detail))
            .unwrap_or("git pull failed");
        return Err(git_error(op, &cwd, &pull_args, detail, Some(&result)));
    }
    let after = git(op, &cwd, &rev, GIT_TIMEOUT)
        .await
        .map(|r| r.stdout.trim().to_string())
        .unwrap_or_default();
    let upstream_ref = read_current_upstream(&cwd).await.map(|u| u.upstream_ref);
    Ok(json!({
        "status": (!before.is_empty() && before == after).then_some("skipped_up_to_date").unwrap_or("pulled"),
        "refName": ref_name,
        "upstreamRef": upstream_ref,
    }))
}

fn fetch_failure_detail(stderr: &str) -> Option<&'static str> {
    let lower = stderr.to_lowercase();
    if lower.contains("authentication failed")
        || lower.contains("could not read username")
        || lower.contains("could not read password")
        || lower.contains("permission denied (publickey")
    {
        return Some(
            "Git could not authenticate with the remote. Check Git credentials or SSH access on the server, then retry.",
        );
    }
    if lower.contains("could not resolve host")
        || lower.contains("failed to connect")
        || lower.contains("connection timed out")
        || lower.contains("connection refused")
        || lower.contains("network is unreachable")
    {
        return Some(
            "Git could not reach the remote. Check the server's network connection and remote host, then retry.",
        );
    }
    if lower.contains("repository not found") || lower.contains("does not appear to be a git repository") {
        return Some(
            "Git could not access the remote repository. Check the remote URL and repository permissions on the server.",
        );
    }
    None
}

/// `resolveBranchBase` — the branch name a base ref points at plus its
/// resolved full ref, for `branch.<name>.t3-merge-base` writes.
async fn resolve_branch_base(cwd: &Path, ref_name: &str) -> Option<(String, String)> {
    let args = sargs(&[
        "rev-parse",
        "--symbolic-full-name",
        "--verify",
        "--end-of-options",
        ref_name,
    ]);
    let r = git("GitVcsDriver.resolveBranchBase", cwd, &args, GIT_TIMEOUT)
        .await
        .ok()?;
    if r.exit_code != Some(0) {
        let remote = primary_remote_name(cwd).await?;
        if remote_branch_exists(cwd, &remote, ref_name).await {
            return Some((
                format!("refs/remotes/{remote}/{ref_name}"),
                ref_name.to_string(),
            ));
        }
        return None;
    }
    let full = r.stdout.trim();
    if let Some(name) = full.strip_prefix("refs/heads/") {
        return Some((full.to_string(), name.to_string()));
    }
    if let Some(rest) = full.strip_prefix("refs/remotes/") {
        let mut names = list_remote_names(cwd).await;
        names.sort_by(|a, b| b.len().cmp(&a.len()));
        let (_, branch) = parse_remote_ref(rest, &names)?;
        return Some((full.to_string(), branch));
    }
    None
}

async fn create_worktree(state: &AppState, payload: &Value) -> Result<Value, Value> {
    let cwd = resolve_cwd(state, payload).await?;
    let op = "GitVcsDriver.createWorktree";
    let ref_name = str_field(payload, "refName").ok_or_else(|| {
        git_error(op, &cwd, &sargs(&["worktree", "add"]), "Missing refName.", None)
    })?;
    let new_ref_name = str_field(payload, "newRefName");
    refuse_option_like(op, &cwd, Some(ref_name))?;
    refuse_option_like(op, &cwd, new_ref_name)?;
    let target_branch = new_ref_name.unwrap_or(ref_name).to_string();

    let worktree_path = match str_field(payload, "path") {
        Some(p) => resolve_under(&cwd, p).to_string_lossy().to_string(),
        None => {
            // Managed folder: <67 home>/worktrees/<repo>/<branch>-<hash16>.
            let repo_name = real_path(&cwd)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "repository".to_string());
            let common = match resolve_repository_paths(&cwd).await? {
                Some(p) => p.git_common_dir,
                None => {
                    return Err(git_error(
                        op,
                        &cwd,
                        &sargs(&["worktree", "add"]),
                        "Not a Git repository.",
                        None,
                    ))
                }
            };
            let identity = format!("{}\0{target_branch}", common.to_string_lossy());
            let hash = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                identity.hash(&mut h);
                format!("{:016x}", h.finish())
            };
            let mut prefix = String::new();
            for ch in target_branch.replace('/', "-").chars() {
                if prefix.len() + ch.len_utf8() > 23 {
                    break;
                }
                prefix.push(ch);
            }
            state
                .home_dir
                .join("worktrees")
                .join(repo_name)
                .join(format!("{prefix}-{hash}"))
                .to_string_lossy()
                .to_string()
        }
    };

    if let Some(new_ref) = new_ref_name {
        let start_point = if ref_name == "-" { "@{-1}" } else { ref_name };
        let args = sargs(&["branch", "--no-track", "--", new_ref, start_point]);
        git_ok(op, &cwd, &args, GIT_TIMEOUT, "git worktree add failed").await?;
        // Record tracking when the start point names a remote ref.
        let resolve_args = sargs(&["rev-parse", "--symbolic-full-name", start_point]);
        if let Ok(r) = git(op, &cwd, &resolve_args, GIT_TIMEOUT).await {
            let start_ref = r.stdout.trim();
            if r.exit_code == Some(0) && start_ref.starts_with("refs/remotes/") {
                let track_args =
                    sargs(&["branch", &format!("--set-upstream-to={start_ref}"), "--", new_ref]);
                let _ = git(op, &cwd, &track_args, GIT_TIMEOUT).await;
            }
        }
    }
    let checkout_ref = new_ref_name.unwrap_or(ref_name);

    let workers = config_value(&cwd, "checkout.workers")
        .await
        .unwrap_or_else(|| "0".to_string());
    let add_args = sargs(&[
        "-c",
        &format!("checkout.workers={workers}"),
        "worktree",
        "add",
        "--",
        &worktree_path,
        checkout_ref,
    ]);
    if let Err(e) = git_ok(op, &cwd, &add_args, GIT_TIMEOUT_LONG, "git worktree add failed").await
    {
        if let Some(new_ref) = new_ref_name {
            let del = sargs(&["branch", "-D", "--", new_ref]);
            let _ = git(op, &cwd, &del, GIT_TIMEOUT).await;
        }
        return Err(e);
    }

    if let (Some(new_ref), Some(base_ref_name)) = (new_ref_name, str_field(payload, "baseRefName"))
    {
        if let Some((base_ref, branch_name)) = resolve_branch_base(&cwd, base_ref_name).await {
            let _ = write_config_value(
                &cwd,
                &format!("branch.{new_ref}.t3-merge-base"),
                &branch_name,
            )
            .await;
            if base_ref.starts_with("refs/remotes/") {
                let _ = write_config_value(
                    &cwd,
                    &format!("branch.{new_ref}.t3-merge-base-ref"),
                    &base_ref,
                )
                .await;
            }
        }
    }

    // worktree add leaves submodules empty; initialize them when present.
    if Path::new(&worktree_path).join(".gitmodules").exists() {
        let wt = PathBuf::from(&worktree_path);
        let _ = git(op, &wt, &sargs(&["submodule", "init"]), GIT_TIMEOUT).await;
        let _ = git(
            op,
            &wt,
            &sargs(&["submodule", "update", "--init"]),
            GIT_TIMEOUT_LONG,
        )
        .await;
    }

    Ok(json!({
        "worktree": { "path": worktree_path, "refName": target_branch },
    }))
}

async fn remove_worktree(state: &AppState, payload: &Value) -> Result<Value, Value> {
    let cwd = resolve_cwd(state, payload).await?;
    let op = "GitVcsDriver.removeWorktree";
    let path = str_field(payload, "path").ok_or_else(|| {
        git_error(op, &cwd, &sargs(&["worktree", "remove"]), "Missing path.", None)
    })?;
    let target = resolve_under(&cwd, path);
    let force = payload.get("force").and_then(Value::as_bool) == Some(true);
    // status.showUntrackedFiles=normal keeps Git's untracked check honest.
    let mut remove_args = sargs(&["-c", "status.showUntrackedFiles=normal", "worktree", "remove"]);
    if force {
        remove_args.push("--force".into());
    }
    remove_args.push("--".into());
    remove_args.push(target.to_string_lossy().to_string());
    let result = git(op, &cwd, &remove_args, GIT_TIMEOUT_LONG).await?;
    if result.exit_code == Some(0) {
        return Ok(Value::Null);
    }
    let stderr = result.stderr.to_lowercase();
    let missing =
        stderr.contains("is not a working tree") || stderr.contains("cannot remove working tree");
    // Already-gone removals are no-ops; drop the stale registration.
    if missing && !target.exists() {
        let _ = git(op, &cwd, &sargs(&["worktree", "prune"]), GIT_TIMEOUT).await;
        return Ok(Value::Null);
    }
    Err(git_error(
        op,
        &cwd,
        &remove_args,
        "git worktree remove failed",
        Some(&result),
    ))
}

// ---------------------------------------------------------------------------
// Checkpoint turn diffs — `orchestration.getTurnDiff` /
// `orchestration.getFullThreadDiff`, a port of
// `apps/server/src/checkpointing/CheckpointDiffQuery.ts` +
// `GitVcsDriver.checkpoints.{diffCheckpoints,listAuthoredPaths}`.
// ---------------------------------------------------------------------------

const CHECKPOINT_REFS_PREFIX: &str = "refs/t3/orchestration-v2/checkpoints";
const CHECKPOINT_DIFF_MAX_OUTPUT_BYTES: usize = 10_000_000;
const DIFF_TRUNCATED_MARKER: &str = "\n\n[truncated]";
// Windows limits a command line to 32,767 chars (TS MAX_PATHSPEC_CHARS).
const MAX_PATHSPEC_CHARS: usize = 24_000;

/// `checkpointRefForScopeOrdinal`: the ref name a checkpoint capture writes is
/// derived from the scope id, so `fromTurnCount == 0` can name the baseline
/// ref even before reading a checkpoint row.
fn checkpoint_ref_for_scope_ordinal(scope_id: &str, ordinal_within_scope: i64) -> String {
    use base64::Engine;
    let hex = crate::rpc_assets::sha256_hex(scope_id.as_bytes());
    // TS: Hex.encode(digest).slice(0, 32) then Base64Url.encode of that text.
    let scope_key =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&hex.as_bytes()[..32]);
    format!("{CHECKPOINT_REFS_PREFIX}/{scope_key}/ordinal/{ordinal_within_scope}")
}

/// One `parseTurnDiffFilesFromNumstat` record (`additions`/`deletions` are
/// unused by the diff path; only the paths matter for import filtering).
#[derive(Debug)]
struct TurnDiffFile {
    path: String,
    previous_path: Option<String>,
}

/// `parseTurnDiffFilesFromNumstat` — NUL-separated `git diff --numstat -z`.
fn parse_turn_diff_files_from_numstat(numstat: &str) -> Vec<TurnDiffFile> {
    let records: Vec<&str> = numstat.split('\0').collect();
    let mut files = Vec::new();
    let mut index = 0usize;
    while index < records.len() {
        let record = records[index];
        index += 1;
        // ^(\d+|-)\t(\d+|-)\t — counts then the path remainder.
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let is_count = |s: &str| s == "-" || s.bytes().all(|b| b.is_ascii_digit());
        if !is_count(added) || !is_count(deleted) {
            continue;
        }
        let mut path = path.to_string();
        let mut previous_path = None;
        if path.is_empty() {
            // Rename record: the two following NUL fields hold old and new path.
            previous_path = records.get(index).map(|s| (*s).to_string());
            path = records.get(index + 1).map(|s| (*s).to_string()).unwrap_or_default();
            index += 2;
        }
        if path.is_empty() {
            continue;
        }
        files.push(TurnDiffFile {
            path,
            previous_path,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// `isGitImport` — a path is authored when it (or its rename source) appears
/// in the authored-paths set.
fn is_git_import(file: &TurnDiffFile, authored_paths: &HashSet<String>) -> bool {
    !authored_paths.contains(&file.path)
        && file
            .previous_path
            .as_ref()
            .is_none_or(|p| !authored_paths.contains(p))
}

/// `batchFilePaths` — group retained paths under the pathspec budget.
fn batch_file_paths(files: &[TurnDiffFile]) -> Vec<Vec<String>> {
    let mut batches: Vec<Vec<String>> = Vec::new();
    let mut batch: Vec<String> = Vec::new();
    let mut length = 0usize;
    for file in files {
        let paths: Vec<String> = match &file.previous_path {
            None => vec![file.path.clone()],
            Some(prev) => vec![prev.clone(), file.path.clone()],
        };
        let size: usize = paths.iter().map(|p| p.len() + 16).sum();
        if !batch.is_empty() && length + size > MAX_PATHSPEC_CHARS {
            batches.push(std::mem::take(&mut batch));
            length = 0;
        }
        batch.extend(paths);
        length += size;
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

/// `GitVcsDriver.checkpoints.diffCheckpoints` — `git diff` between two
/// checkpoint commit refs (`fallbackFromToHead: false` throughout the turn
/// diff path).
async fn checkpoint_diff(
    cwd: &Path,
    from_ref: &str,
    to_ref: &str,
    ignore_whitespace: bool,
    numstat: bool,
    file_paths: Option<&[String]>,
) -> Result<String, Value> {
    let op = "GitVcsDriver.checkpoints.diffCheckpoints";
    if file_paths.is_some_and(|p| p.is_empty()) {
        return Ok(String::new());
    }
    let mut args = sargs(&[
        "-c",
        "diff.relative=false",
        "-c",
        "diff.suppressBlankEmpty=false",
        "diff",
    ]);
    if numstat {
        args.extend(sargs(&["--numstat", "-z"]));
    } else {
        args.push("--patch".into());
    }
    args.extend(sargs(&[
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--submodule=short",
        "--src-prefix=a/",
        "--dst-prefix=b/",
    ]));
    if ignore_whitespace {
        args.push("--ignore-all-space".into());
    }
    args.push(format!("{from_ref}^{{commit}}"));
    args.push(format!("{to_ref}^{{commit}}"));
    if let Some(paths) = file_paths {
        args.push("--".into());
        for p in paths {
            args.push(format!(":(top,literal){p}"));
        }
    }
    let out = git(op, cwd, &args, GIT_TIMEOUT).await?;
    if out.exit_code != Some(0) {
        return Err(git_error(
            op,
            cwd,
            &args,
            if out.stderr.trim().is_empty() {
                "Checkpoint ref is unavailable for diff operation."
            } else {
                out.stderr.trim()
            },
            Some(&out),
        ));
    }
    Ok(out.stdout)
}

/// `GitVcsDriver.checkpoints.listAuthoredPaths` — `None` when HEAD was
/// unchanged between the two captures (or anything fails), meaning every
/// changed path belongs to the turn.
async fn list_authored_paths(
    cwd: &Path,
    from_ref: &str,
    to_ref: &str,
) -> Option<HashSet<String>> {
    let op = "GitVcsDriver.checkpoints.listAuthoredPaths";
    let heads_args = sargs(&[
        "log",
        "--no-walk=unsorted",
        "--format=%P %ct",
        &format!("{from_ref}^{{commit}}"),
        &format!("{to_ref}^{{commit}}"),
    ]);
    let heads = git(op, cwd, &heads_args, GIT_TIMEOUT).await.ok()?;
    if heads.exit_code != Some(0) {
        return None;
    }
    let mut lines = heads.stdout.trim_end().split('\n');
    let from_line = lines.next().unwrap_or("");
    let to_line = lines.next().unwrap_or("");
    let mut from_parts = from_line.split(' ');
    let start_head = from_parts.next().unwrap_or("");
    let captured_at = from_parts.next().unwrap_or("");
    let end_head = to_line.split(' ').next().unwrap_or("");
    if start_head.is_empty() || end_head.is_empty() || start_head == end_head {
        return None;
    }

    let list_paths = |args: Vec<String>| async move {
        let mut full = sargs(&[
            "-c",
            "diff.relative=false",
            "-c",
            "log.showSignature=false",
        ]);
        full.extend(args);
        full.extend(sargs(&["--name-only", "-z", "--no-renames", "--no-ext-diff"]));
        match git(op, cwd, &full, GIT_TIMEOUT).await {
            Ok(r) if r.exit_code == Some(0) => r
                .stdout
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect::<Vec<String>>(),
            _ => Vec::new(),
        }
    };
    let from_commit = format!("{from_ref}^{{commit}}");
    let to_commit = format!("{to_ref}^{{commit}}");
    // Remerge diffs retain merge conflict fixes while excluding clean
    // upstream changes.
    let (a, b, c, d) = tokio::join!(
        list_paths(sargs(&["diff", start_head, &from_commit])),
        list_paths(sargs(&["diff", end_head, &to_commit])),
        list_paths(sargs(&[
            "log",
            "--format=",
            "--diff-merges=remerge",
            &format!("{end_head}..{start_head}"),
        ])),
        list_paths(sargs(&[
            "log",
            "--format=",
            "--diff-merges=remerge",
            &format!("--since=@{captured_at}"),
            &format!("{start_head}..{end_head}"),
        ])),
    );
    Some(a.into_iter().chain(b).chain(c).chain(d).collect())
}

/// Tagged checkpoint-diff errors, mirroring `apps/server/src/checkpointing/Errors.ts`.
fn checkpoint_error(operation: &str, tag: &str, fields: Value) -> Value {
    let mut e = json!({ "_tag": tag, "operation": operation });
    if let Value::Object(extra) = fields {
        for (k, v) in extra {
            e[k] = v;
        }
    }
    e
}

fn turn_diff_error(operation: &str, cause: Value) -> Value {
    let tag = if operation.contains("getFullThreadDiff") {
        "OrchestrationGetFullThreadDiffError"
    } else {
        "OrchestrationGetTurnDiffError"
    };
    let message = if operation.contains("getFullThreadDiff") {
        "Failed to load full thread diff"
    } else {
        "Failed to load turn diff"
    };
    json!({ "_tag": tag, "message": message, "cause": cause })
}

/// `CheckpointDiffQuery.getTurnDiff` core: resolve from/to checkpoint refs
/// from the checkpoint context, diff them in the scope's working directory,
/// and strip changes a moved HEAD imported between captures.
async fn turn_diff(
    operation: &str,
    thread_id: &str,
    from_turn: i64,
    to_turn: i64,
    ignore_whitespace: bool,
    ctx: &Value,
) -> Result<String, Value> {
    let err = |tag: &str, fields: Value| turn_diff_error(operation, checkpoint_error(operation, tag, fields));
    let runs = ctx.get("runs").and_then(Value::as_array).cloned().unwrap_or_default();
    let scopes = ctx
        .get("checkpointScopes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let checkpoints = ctx
        .get("checkpoints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let completed_run_ids: HashSet<&str> = runs
        .iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("completed"))
        .filter_map(|r| r.get("id").and_then(Value::as_str))
        .collect();
    let ready: Vec<&Value> = checkpoints
        .iter()
        .filter(|c| {
            c.get("status").and_then(Value::as_str) == Some("ready")
                && c.get("appRunOrdinal").and_then(Value::as_i64).is_some()
                && c.get("runId")
                    .and_then(Value::as_str)
                    .is_some_and(|id| completed_run_ids.contains(id))
        })
        .collect();
    let max_turn = ready
        .iter()
        .filter_map(|c| c.get("appRunOrdinal").and_then(Value::as_i64))
        .max()
        .unwrap_or(0);
    if to_turn > max_turn {
        return Err(err(
            "CheckpointTurnRangeUnavailableError",
            json!({
                "threadId": thread_id,
                "requestedTurnCount": to_turn,
                "availableTurnCount": max_turn,
            }),
        ));
    }
    let to_checkpoint = ready
        .iter()
        .find(|c| c.get("appRunOrdinal").and_then(Value::as_i64) == Some(to_turn))
        .ok_or_else(|| {
            err(
                "CheckpointRefUnavailableError",
                json!({ "threadId": thread_id, "turnCount": to_turn, "checkpoint": "to" }),
            )
        })?;
    let to_scope_id = to_checkpoint
        .get("scopeId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let to_scope = scopes
        .iter()
        .find(|s| s.get("id").and_then(Value::as_str) == Some(to_scope_id))
        .ok_or_else(|| {
            err(
                "CheckpointWorkspacePathMissingError",
                json!({ "threadId": thread_id }),
            )
        })?;
    let cwd_str = to_scope.get("cwd").and_then(Value::as_str).ok_or_else(|| {
        err(
            "CheckpointWorkspacePathMissingError",
            json!({ "threadId": thread_id }),
        )
    })?;
    let cwd = Path::new(cwd_str);
    let to_ref = to_checkpoint
        .get("ref")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            err(
                "CheckpointRefUnavailableError",
                json!({ "threadId": thread_id, "turnCount": to_turn, "checkpoint": "to" }),
            )
        })?;

    // Capture compares against the preceding scope ordinal, even when that
    // checkpoint belongs to a stopped run or has no run.
    let from_ref: String = if from_turn == 0 {
        match scopes
            .iter()
            .find(|s| s.get("kind").and_then(Value::as_str) == Some("root_run"))
            .and_then(|s| s.get("id").and_then(Value::as_str))
        {
            Some(scope_id) => checkpoint_ref_for_scope_ordinal(scope_id, 0),
            None => {
                return Err(err(
                    "CheckpointRefUnavailableError",
                    json!({ "threadId": thread_id, "turnCount": from_turn, "checkpoint": "from" }),
                ))
            }
        }
    } else {
        let parent = if from_turn == to_turn - 1 {
            checkpoints.iter().find(|c| {
                c.get("status").and_then(Value::as_str) == Some("ready")
                    && c.get("scopeId").and_then(Value::as_str) == Some(to_scope_id)
                    && c.get("ordinalWithinScope").and_then(Value::as_i64)
                        == to_checkpoint
                            .get("ordinalWithinScope")
                            .and_then(Value::as_i64)
                            .map(|o| o - 1)
            })
        } else {
            None
        };
        let source = parent
            .or_else(|| {
                ready
                    .iter()
                    .find(|c| {
                        c.get("appRunOrdinal").and_then(Value::as_i64) == Some(from_turn)
                    })
                    .copied()
            })
            .and_then(|c| c.get("ref").and_then(Value::as_str));
        match source {
            Some(r) => r.to_string(),
            None => {
                return Err(err(
                    "CheckpointRefUnavailableError",
                    json!({ "threadId": thread_id, "turnCount": from_turn, "checkpoint": "from" }),
                ))
            }
        }
    };

    // A pull or rebase between captures imports changes the turn did not
    // author; exclude those paths from the patch (TS `isGitImport` filter).
    let authored_paths = list_authored_paths(cwd, &from_ref, to_ref).await;
    let files = match &authored_paths {
        None => Vec::new(),
        Some(_) => {
            let numstat = checkpoint_diff(cwd, &from_ref, to_ref, ignore_whitespace, true, None)
                .await
                .unwrap_or_default();
            parse_turn_diff_files_from_numstat(&numstat)
        }
    };
    let retained: Vec<&TurnDiffFile> = match &authored_paths {
        None => Vec::new(),
        Some(set) => files.iter().filter(|f| !is_git_import(f, set)).collect(),
    };
    let retained_files: Vec<TurnDiffFile> = retained
        .into_iter()
        .map(|f| TurnDiffFile {
            path: f.path.clone(),
            previous_path: f.previous_path.clone(),
        })
        .collect();
    // Select retained paths before generating a patch, so imported bulk
    // cannot exhaust its output limit.
    let path_batches: Vec<Option<Vec<String>>> = if retained_files.len() == files.len() {
        vec![None]
    } else {
        batch_file_paths(&retained_files).into_iter().map(Some).collect()
    };

    let mut diff = String::new();
    let mut remaining = CHECKPOINT_DIFF_MAX_OUTPUT_BYTES;
    for file_paths in path_batches {
        let patch = checkpoint_diff(
            cwd,
            &from_ref,
            to_ref,
            ignore_whitespace,
            false,
            file_paths.as_deref(),
        )
        .await
        .map_err(|cause| turn_diff_error(operation, cause))?;
        let patch_bytes = patch.len();
        diff.push_str(&patch);
        if patch_bytes > remaining {
            // One extra byte distinguishes an exact fit from a truncated
            // patch; reserve marker space only on overflow.
            let cut = CHECKPOINT_DIFF_MAX_OUTPUT_BYTES - DIFF_TRUNCATED_MARKER.len();
            let mut end = cut.min(diff.len());
            while !diff.is_char_boundary(end) {
                end -= 1;
            }
            diff.truncate(end);
            diff.push_str(DIFF_TRUNCATED_MARKER);
            break;
        }
        remaining -= patch_bytes;
    }
    Ok(diff)
}

/// `orchestration.getTurnDiff` — `{threadId, fromTurnCount, toTurnCount,
/// ignoreWhitespace?}` → `{threadId, fromTurnCount, toTurnCount, diff}`.
pub async fn get_turn_diff(state: Arc<AppState>, payload: &Value) -> RpcResult {
    const OP: &str = "CheckpointDiffQuery.getTurnDiff";
    let thread_id = payload.get("threadId").and_then(Value::as_str).unwrap_or("");
    let from_turn = payload
        .get("fromTurnCount")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let to_turn = payload
        .get("toTurnCount")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let ignore_whitespace = payload
        .get("ignoreWhitespace")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let result = |diff: String| {
        RpcResult::ok(json!({
            "threadId": thread_id,
            "fromTurnCount": from_turn,
            "toTurnCount": to_turn,
            "diff": diff,
        }))
    };

    if thread_id.is_empty() || from_turn < 0 || to_turn < 0 || from_turn > to_turn {
        return RpcResult::err(turn_diff_error(
            OP,
            checkpoint_error(
                OP,
                "CheckpointDiffResultInvalidError",
                json!({ "threadId": thread_id }),
            ),
        ));
    }
    if from_turn == to_turn {
        return result(String::new());
    }
    let Some(ctx) = state.store.checkpoint_context(thread_id).await else {
        return RpcResult::err(turn_diff_error(
            OP,
            checkpoint_error(
                OP,
                "CheckpointThreadNotFoundError",
                json!({ "threadId": thread_id }),
            ),
        ));
    };
    match turn_diff(OP, thread_id, from_turn, to_turn, ignore_whitespace, &ctx).await {
        Ok(diff) => result(diff),
        Err(e) => RpcResult::err(e),
    }
}

/// `orchestration.getFullThreadDiff` — `{threadId, toTurnCount,
/// ignoreWhitespace?}`; `getTurnDiff` with `fromTurnCount = 0`.
pub async fn get_full_thread_diff(state: Arc<AppState>, payload: &Value) -> RpcResult {
    const OP: &str = "CheckpointDiffQuery.getFullThreadDiff";
    let thread_id = payload.get("threadId").and_then(Value::as_str).unwrap_or("");
    let to_turn = payload
        .get("toTurnCount")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let ignore_whitespace = payload
        .get("ignoreWhitespace")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let result = |diff: String| {
        RpcResult::ok(json!({
            "threadId": thread_id,
            "fromTurnCount": 0,
            "toTurnCount": to_turn,
            "diff": diff,
        }))
    };

    if thread_id.is_empty() || to_turn < 0 {
        return RpcResult::err(turn_diff_error(
            OP,
            checkpoint_error(
                OP,
                "CheckpointDiffResultInvalidError",
                json!({ "threadId": thread_id }),
            ),
        ));
    }
    if to_turn == 0 {
        return result(String::new());
    }
    let Some(ctx) = state.store.checkpoint_context(thread_id).await else {
        return RpcResult::err(turn_diff_error(
            OP,
            checkpoint_error(
                OP,
                "CheckpointThreadNotFoundError",
                json!({ "threadId": thread_id }),
            ),
        ));
    };
    match turn_diff(OP, thread_id, 0, to_turn, ignore_whitespace, &ctx).await {
        Ok(diff) => result(diff),
        Err(e) => RpcResult::err(e),
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

pub async fn handle(state: Arc<AppState>, tag: &str, payload: Value) -> RpcResult {
    let result = match tag {
        "vcs.listRefs" => list_refs(&state, &payload).await,
        "vcs.refreshStatus" => refresh_status(&state, &payload).await,
        "vcs.switchRef" => switch_ref(&state, &payload).await,
        "vcs.createRef" => create_ref(&state, &payload).await,
        "vcs.init" => vcs_init(&state, &payload).await,
        "vcs.pull" => vcs_pull(&state, &payload).await,
        "vcs.createWorktree" => create_worktree(&state, &payload).await,
        "vcs.removeWorktree" => remove_worktree(&state, &payload).await,
        // Void success; subscriptions pick the change up on their next poll.
        "vcs.refreshBranch" => Ok(Value::Null),
        other => Err(json!({
            "_tag": "RpcClientError",
            "message": format!("not implemented: {other}"),
        })),
    };
    match result {
        Ok(v) => RpcResult::ok(v),
        Err(e) => RpcResult::err(e),
    }
}

/// Claim a subscription stream tag; returns true if this module handled it.
pub async fn handle_subscription(
    state: Arc<AppState>,
    out: OutTx,
    request_id: u64,
    tag: &str,
    payload: Value,
    window: Arc<Window>,
) -> bool {
    match tag {
        "subscribeVcsStatus" => {
            vcs_status_stream(state, out, request_id, payload, window).await;
            true
        }
        "subscribeVcsBranch" => {
            vcs_branch_stream(state, out, request_id, payload, window).await;
            true
        }
        _ => false,
    }
}

/// `streamStatus`: one `snapshot` chunk, then `localUpdated`/`remoteUpdated`
/// chunks as re-polled status parts change.
async fn vcs_status_stream(
    state: Arc<AppState>,
    out: OutTx,
    request_id: u64,
    payload: Value,
    window: Arc<Window>,
) {
    let cwd = match resolve_cwd(&state, &payload).await {
        Ok(c) => c,
        Err(e) => {
            send_json(&out, &exit_failure(request_id, e));
            return;
        }
    };
    let include_remote = payload.get("includeRemote").and_then(Value::as_bool) != Some(false);
    let local = match read_local_details(&cwd).await {
        Ok(l) => l,
        Err(e) => {
            send_json(&out, &exit_failure(request_id, e));
            return;
        }
    };
    let mut remote: Option<RemoteDetails> = if include_remote && local.is_repo {
        match read_remote_details(&cwd).await {
            Ok(r) => r,
            Err(e) => {
                send_json(&out, &exit_failure(request_id, e));
                return;
            }
        }
    } else {
        None
    };
    let mut last_ref_name = local.ref_name.clone();
    let mut last_local = local_status_value(&local);
    let mut last_remote = remote.as_ref().map(remote_status_value);
    if !window
        .send_chunk(
            &out,
            request_id,
            vec![json!({
                "_tag": "snapshot",
                "local": last_local,
                "remote": last_remote.clone().unwrap_or(Value::Null),
            })],
        )
        .await
    {
        return;
    }

    loop {
        sleep(STATUS_POLL_INTERVAL).await;
        if window.is_cancelled() {
            return;
        }
        let Ok(next) = read_local_details(&cwd).await else {
            continue;
        };
        let branch_changed = next.ref_name != last_ref_name;
        last_ref_name = next.ref_name.clone();
        let next_value = local_status_value(&next);
        if next_value != last_local {
            last_local = next_value.clone();
            if !window
                .send_chunk(&out, request_id, vec![json!({"_tag": "localUpdated", "local": next_value})])
                .await
            {
                return;
            }
        }
        // Remote data keyed to the old branch is stale; clear it first.
        if branch_changed && remote.is_some() {
            remote = None;
            last_remote = None;
            if !window
                .send_chunk(
                    &out,
                    request_id,
                    vec![json!({"_tag": "remoteUpdated", "remote": Value::Null})],
                )
                .await
            {
                return;
            }
        }
        if include_remote && next.is_repo {
            if let Ok(next_remote) = read_remote_details(&cwd).await {
                let value = next_remote.as_ref().map(remote_status_value);
                if value != last_remote {
                    last_remote = value.clone();
                    if !window
                        .send_chunk(
                            &out,
                            request_id,
                            vec![json!({
                                "_tag": "remoteUpdated",
                                "remote": value.unwrap_or(Value::Null),
                            })],
                        )
                        .await
                    {
                        return;
                    }
                }
                remote = next_remote;
            }
        }
    }
}

/// Current checked-out branch, `None` detached or not a repository.
async fn read_head_branch(cwd: &Path) -> Option<String> {
    match git(
        "GitVcsDriver.currentBranch",
        cwd,
        &sargs(&["symbolic-ref", "--quiet", "HEAD"]),
        GIT_TIMEOUT,
    )
    .await
    {
        Ok(r) if r.exit_code == Some(0) => branch_name_from_head_ref(&r.stdout),
        _ => None,
    }
}

/// `streamBranchStatus`: `{refName}` chunks on every branch change.
async fn vcs_branch_stream(
    state: Arc<AppState>,
    out: OutTx,
    request_id: u64,
    payload: Value,
    window: Arc<Window>,
) {
    let cwd = match resolve_cwd(&state, &payload).await {
        Ok(c) => c,
        Err(e) => {
            send_json(&out, &exit_failure(request_id, e));
            return;
        }
    };
    let mut last = read_head_branch(&cwd).await;
    if !window
        .send_chunk(&out, request_id, vec![json!({ "refName": last })])
        .await
    {
        return;
    }
    loop {
        sleep(STATUS_POLL_INTERVAL).await;
        if window.is_cancelled() {
            return;
        }
        let next = read_head_branch(&cwd).await;
        if next != last {
            last = next;
            if !window
                .send_chunk(&out, request_id, vec![json!({ "refName": last })])
                .await
            {
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Run git in `cwd`, returning trimmed stdout; panics on nonzero exit.
    fn tgit(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let mut cmd = Command::new("git");
        cmd.args(args).current_dir(cwd);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("c67-turn-diff-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `commit-tree` a tree built from `files` ((path, content) pairs) with an
    /// optional parent commit and fixed committer date.
    fn commit_tree(
        cwd: &Path,
        files: &[(&str, &str)],
        parent: Option<&str>,
        when: &str,
    ) -> String {
        // Build a flat tree; paths stay single-level in these fixtures.
        let mut entries = String::new();
        for (path, content) in files {
            let blob = {
                let mut child = Command::new("git")
                    .args(["hash-object", "-w", "--stdin"])
                    .current_dir(cwd)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                use std::io::Write;
                child
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(content.as_bytes())
                    .unwrap();
                let out = child.wait_with_output().unwrap();
                assert!(out.status.success());
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            };
            entries.push_str(&format!("100644 blob {blob}\t{path}\n"));
        }
        let tree = {
            let mut child = Command::new("git")
                .args(["mktree"])
                .current_dir(cwd)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(entries.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success());
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let mut args: Vec<String> = vec!["commit-tree".into(), tree];
        if let Some(p) = parent {
            args.extend(["-p".into(), p.to_string()]);
        }
        args.extend(["-m".into(), "checkpoint".into()]);
        let envs = [
            ("GIT_AUTHOR_DATE", when),
            ("GIT_COMMITTER_DATE", when),
            ("GIT_AUTHOR_NAME", "test"),
            ("GIT_AUTHOR_EMAIL", "t@t"),
            ("GIT_COMMITTER_NAME", "test"),
            ("GIT_COMMITTER_EMAIL", "t@t"),
        ];
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        tgit(cwd, &arg_refs, &envs)
    }

    fn checkpoint_ctx(cwd: &Path, scope_id: &str, checkpoints: Vec<Value>) -> Value {
        json!({
            "runs": [{ "id": "r1", "ordinal": 1, "status": "completed" }],
            "checkpointScopes": [{
                "id": scope_id,
                "runId": "r1",
                "kind": "root_run",
                "cwd": cwd.to_string_lossy(),
            }],
            "checkpoints": checkpoints,
        })
    }

    fn checkpoint_row(
        scope_id: &str,
        run_id: Option<&str>,
        ordinal: i64,
        app_run_ordinal: Option<i64>,
        ref_name: &str,
    ) -> Value {
        json!({
            "scopeId": scope_id,
            "runId": run_id,
            "ordinalWithinScope": ordinal,
            "appRunOrdinal": app_run_ordinal,
            "status": "ready",
            "ref": ref_name,
        })
    }

    #[test]
    fn checkpoint_ref_matches_ts_derivation() {
        // Seeded data: scope id → stored checkpoint ref (see
        // orchestration_v2_projection_checkpoint_scopes/.t3 test data).
        let scope_id = "checkpoint-scope:thread:9ead6dd9-11e4-45df-be46-b1c07674ff5a:name:root";
        assert_eq!(
            checkpoint_ref_for_scope_ordinal(scope_id, 0),
            "refs/t3/orchestration-v2/checkpoints/\
             N2ZlMjBmODM0YzhkZTFkNWVkNTViMjU0ZTg4NTM1Zjk/ordinal/0"
        );
    }

    #[test]
    fn numstat_parse_handles_plain_and_rename_records() {
        let files = parse_turn_diff_files_from_numstat("3\t1\ta.txt\0-\t-\tbin.png\0");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "a.txt");
        assert!(files[0].previous_path.is_none());
        assert_eq!(files[1].path, "bin.png");

        // Rename: counts record carries an empty path, followed by old/new.
        let renamed = parse_turn_diff_files_from_numstat("2\t2\t\0old.txt\0new.txt\0");
        assert_eq!(renamed.len(), 1);
        assert_eq!(renamed[0].previous_path.as_deref(), Some("old.txt"));
        assert_eq!(renamed[0].path, "new.txt");
    }

    /// HEAD unchanged between captures: every path in the checkpoint range is
    /// retained (authored paths is `None`).
    #[tokio::test]
    async fn turn_diff_returns_patch_between_checkpoint_refs() {
        let dir = test_dir("basic");
        tgit(&dir, &["init", "-q"], &[]);
        let scope_id = "checkpoint-scope:thread:test:name:root";
        let base = commit_tree(
            &dir,
            &[("a.txt", "one\n"), ("gone.txt", "stale\n")],
            None,
            "2026-01-01T00:00:00Z",
        );
        let from_ref = checkpoint_ref_for_scope_ordinal(scope_id, 0);
        let to_ref = checkpoint_ref_for_scope_ordinal(scope_id, 1);
        // Checkpoint commits parent on the same HEAD, like captureCheckpoint.
        let c0 = commit_tree(
            &dir,
            &[("a.txt", "one\n"), ("gone.txt", "stale\n")],
            Some(&base),
            "2026-01-02T00:00:00Z",
        );
        let c1 = commit_tree(
            &dir,
            &[("a.txt", "one\ntwo\n"), ("new.txt", "added\n")],
            Some(&base),
            "2026-01-03T00:00:00Z",
        );
        tgit(&dir, &["update-ref", &from_ref, &c0], &[]);
        tgit(&dir, &["update-ref", &to_ref, &c1], &[]);

        let ctx = checkpoint_ctx(
            &dir,
            scope_id,
            vec![
                checkpoint_row(scope_id, None, 0, None, &from_ref),
                checkpoint_row(scope_id, Some("r1"), 1, Some(1), &to_ref),
            ],
        );
        let diff = turn_diff(
            "CheckpointDiffQuery.getTurnDiff",
            "thread:test",
            0,
            1,
            true,
            &ctx,
        )
        .await
        .expect("turn diff");
        assert!(diff.contains("diff --git a/a.txt b/a.txt"), "{diff}");
        assert!(diff.contains("+two"), "{diff}");
        assert!(diff.contains("new file mode"), "{diff}");
        assert!(diff.contains("deleted file mode"), "{diff}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// HEAD moved between captures (a pull): paths the move imported are
    /// excluded from the patch.
    #[tokio::test]
    async fn turn_diff_filters_head_move_imports() {
        let dir = test_dir("import");
        tgit(&dir, &["init", "-q"], &[]);
        let scope_id = "checkpoint-scope:thread:test:name:root";
        let from_ref = checkpoint_ref_for_scope_ordinal(scope_id, 0);
        let to_ref = checkpoint_ref_for_scope_ordinal(scope_id, 1);

        // HEAD at capture 0: C0 on top of an identical base.
        let base = commit_tree(
            &dir,
            &[("a.txt", "one\n"), ("up.txt", "v0\n")],
            None,
            "2025-01-01T00:00:00Z",
        );
        let c0_head = commit_tree(
            &dir,
            &[("a.txt", "one\n"), ("up.txt", "v0\n")],
            Some(&base),
            "2025-06-01T00:00:00Z",
        );
        // The pulled-in HEAD: old committer date so `--since` drops it.
        let c1_head = commit_tree(
            &dir,
            &[("a.txt", "one\n"), ("up.txt", "v1\n")],
            Some(&base),
            "2025-07-01T00:00:00Z",
        );
        // Checkpoint commits parent on their capture-time HEAD.
        let k0 = commit_tree(
            &dir,
            &[("a.txt", "one\n"), ("up.txt", "v0\n")],
            Some(&c0_head),
            "2026-01-02T00:00:00Z",
        );
        let k1 = commit_tree(
            &dir,
            &[("a.txt", "two\n"), ("up.txt", "v1\n")],
            Some(&c1_head),
            "2026-01-03T00:00:00Z",
        );
        tgit(&dir, &["update-ref", &from_ref, &k0], &[]);
        tgit(&dir, &["update-ref", &to_ref, &k1], &[]);

        let ctx = checkpoint_ctx(
            &dir,
            scope_id,
            vec![
                checkpoint_row(scope_id, None, 0, None, &from_ref),
                checkpoint_row(scope_id, Some("r1"), 1, Some(1), &to_ref),
            ],
        );
        let diff = turn_diff(
            "CheckpointDiffQuery.getTurnDiff",
            "thread:test",
            0,
            1,
            true,
            &ctx,
        )
        .await
        .expect("turn diff");
        assert!(diff.contains("a.txt"), "{diff}");
        assert!(
            !diff.contains("up.txt"),
            "imported upstream change must be filtered: {diff}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `toTurnCount` beyond the last ready checkpoint reports the TS
    /// `CheckpointTurnRangeUnavailableError` inside the RPC error tag.
    #[tokio::test]
    async fn turn_diff_rejects_unavailable_range() {
        let dir = test_dir("range");
        let scope_id = "checkpoint-scope:thread:test:name:root";
        let ctx = checkpoint_ctx(&dir, scope_id, vec![]);
        let err = turn_diff(
            "CheckpointDiffQuery.getTurnDiff",
            "thread:test",
            0,
            3,
            true,
            &ctx,
        )
        .await
        .expect_err("range error");
        assert_eq!(err["_tag"], "OrchestrationGetTurnDiffError");
        assert_eq!(
            err["cause"]["_tag"], "CheckpointTurnRangeUnavailableError",
            "{err}"
        );
        assert_eq!(err["cause"]["availableTurnCount"], 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A scope whose directory is not a git repo surfaces the ref-unavailable
    /// failure instead of a panic (seeded data points at absent repos).
    #[tokio::test]
    async fn turn_diff_missing_repo_errors() {
        let dir = test_dir("norepo");
        let scope_id = "checkpoint-scope:thread:test:name:root";
        let from_ref = checkpoint_ref_for_scope_ordinal(scope_id, 0);
        let ctx = checkpoint_ctx(
            &dir,
            scope_id,
            vec![
                checkpoint_row(scope_id, None, 0, None, &from_ref),
                checkpoint_row(scope_id, Some("r1"), 1, Some(1), "refs/t3/x/ordinal/1"),
            ],
        );
        let err = turn_diff(
            "CheckpointDiffQuery.getTurnDiff",
            "thread:test",
            0,
            1,
            true,
            &ctx,
        )
        .await
        .expect_err("git failure");
        assert_eq!(err["_tag"], "OrchestrationGetTurnDiffError");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
