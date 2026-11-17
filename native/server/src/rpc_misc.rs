//! rpc_misc — the catch-all module. Every WS request tag not claimed by the
//! domain modules lands here: `server.*`, `shell.*`, `secrets.*`, `provider.*`
//! (non-stream), `peerLinks.*`, `cloud.*`, `device.*`, `mcpApps.*`, `preview.*`,
//! `bedtimeReminder.*`, `guestShare.*`, and anything unrecognized.
//!
//! Implemented for real: config/settings, keybindings, usage summary, host &
//! process diagnostics, database storage/cleanup, probe, provider refresh,
//! triage prep, secret answers, and the misc subscription streams. Everything
//! else answers a clean `RpcClientError("not supported: <tag>")` — never
//! panic, never leave a request unanswered.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::domain;
use crate::handlers::RpcResult;
use crate::ws_rpc::{exit_failure, send_json, OutTx, Window};
use crate::AppState;

const MAX_KEYBINDINGS_COUNT: usize = 256;
const MAX_WHEN_EXPRESSION_DEPTH: usize = 64;
const USAGE_CONTRACT_VERSION: u64 = 6;

fn client_error(message: impl Into<String>) -> Value {
    json!({ "_tag": "RpcClientError", "message": message.into() })
}

fn not_supported(tag: &str) -> RpcResult {
    RpcResult::err(client_error(format!("not supported: {tag}")))
}

fn internal_error(context: &str, e: impl std::fmt::Display) -> RpcResult {
    RpcResult::err(client_error(format!("{context}: {e}")))
}

// ---------------------------------------------------------------------------
// On-disk state files
// ---------------------------------------------------------------------------

fn settings_path(state: &AppState) -> PathBuf {
    state.home_dir.join("settings.json")
}

fn keybindings_path(state: &AppState) -> PathBuf {
    state.home_dir.join("keybindings.json")
}

fn db_path(state: &AppState) -> PathBuf {
    state.home_dir.join("userdata").join("statev2.sqlite")
}

fn cleanup_report_path(state: &AppState) -> PathBuf {
    state
        .home_dir
        .join("userdata")
        .join("storage-cleanup-report.json")
}

fn read_json_file(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn read_json_object(path: &Path) -> Map<String, Value> {
    read_json_file(path)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Small files only: write to a sibling temp path then rename.
fn write_json_atomic(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let mut text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    text.push('\n');
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename to {}: {e}", path.display()))
}

fn load_settings(state: &AppState) -> Map<String, Value> {
    read_json_object(&settings_path(state))
}

// ---------------------------------------------------------------------------
// Server config snapshot — same shape as `handlers::server_config_snapshot`,
// extended with the real keybindings path/rules and persisted settings.
// ---------------------------------------------------------------------------

fn provider_catalog() -> Value {
    let checked_at = domain::now_iso();
    let provider = |instance: &str, driver: &str, name: &str, models: Value| {
        json!({
            "instanceId": instance,
            "driver": driver,
            "displayName": name,
            "enabled": true,
            "installed": true,
            "version": "1.0.0",
            "status": "ready",
            "auth": { "status": "authenticated" },
            "checkedAt": checked_at,
            "models": models,
        })
    };
    let model = |slug: &str, name: &str| json!({ "slug": slug, "name": name, "isCustom": false, "capabilities": null });
    json!([
        provider(
            "devin",
            "devin",
            "Devin",
            json!([model("swe-2-high", "SWE-2 High"),])
        ),
        provider(
            "devReplay",
            "devReplay",
            "Replay (dev)",
            json!([model("showcase", "Showcase"),])
        ),
        provider(
            "codex",
            "codex",
            "Codex",
            json!([model("o4-mini", "o4-mini"), model("gpt-5", "GPT-5"),])
        ),
    ])
}

/// Environment id persisted at `<home>/userdata/environment-id`, same as the
/// TS server. `create_new` loses the race to an existing file, matching the
/// link-then-read winner protocol in ServerEnvironment.ts.
fn environment_id(state: &AppState) -> String {
    let path = state.home_dir.join("userdata").join("environment-id");
    if let Ok(raw) = std::fs::read_to_string(&path) {
        let id = raw.trim();
        if !id.is_empty() {
            return id.to_string();
        }
    }
    let generated = uuid::Uuid::new_v4().to_string();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    let mut f = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(_) => {
            return std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| {
                    let id = raw.trim().to_string();
                    (!id.is_empty()).then_some(id)
                })
                .unwrap_or(generated);
        }
    };
    let _ = writeln!(f, "{generated}");
    generated
}

fn environment_label(state: &AppState) -> String {
    read_proc("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            state
                .project_dir
                .file_name()
                .map(|n| n.to_string_lossy().trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "T3 environment".to_string())
}

/// `ExecutionEnvironmentDescriptor`: identity, platform, server version and
/// the honest capability set of this build — only what rpc_* modules actually
/// serve is advertised.
fn environment_descriptor(state: &AppState) -> Value {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        _ => "unknown",
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        _ => "other",
    };
    json!({
        "environmentId": environment_id(state),
        "label": environment_label(state),
        "platform": { "os": os, "arch": arch, "machine": "server" },
        "serverVersion": env!("CARGO_PKG_VERSION"),
        "orchestrationProtocolVersion": 2,
        "capabilities": {
            "repositoryIdentity": false,
            "attachmentUploads": true,
            "pullRequests": true,
            "pullRequestChecks": true,
            "inlineMessageContext": true,
            "storageCleanup": true,
            "storageCleanupRun": true,
            "databaseStorage": true,
            "threadStop": true,
            "ignoredFileListing": true,
            "triage": true,
            "oneTimeSchedules": true,
            "scheduledTaskThreadModes": true,
            "freshContext": true,
            "npmReleaseLookup": true,
            "cloudVoiceTranscription": true,
        },
    })
}

/// `ServerAuthDescriptor` for a dev-auth or unauthenticated local server.
fn auth_descriptor(state: &AppState) -> Value {
    json!({
        "policy": if state.auth_token.is_some() { "remote-reachable" } else { "unsafe-no-auth" },
        "bootstrapMethods": ["one-time-token"],
        "sessionMethods": ["bearer-access-token"],
        "sessionCookieName": format!("t3_session_{}", state.listen_addr.port()),
    })
}

fn server_config(state: &AppState) -> Value {
    let (rules, issues) = load_keybinding_rules(state);
    let resolved = resolve_keybindings(&rules);
    let bindings: Vec<Value> = rules
        .iter()
        .map(|r| {
            let mut o = Map::new();
            o.insert("key".into(), json!(r.key));
            o.insert("command".into(), json!(r.command));
            if let Some(when) = &r.when {
                o.insert("when".into(), json!(when));
            }
            Value::Object(o)
        })
        .collect();
    // `keybindings` keeps the envelope `handlers::server_config_snapshot` emits
    // ({bindings, issues}); `resolvedKeybindings` additionally carries the
    // contract-shaped ResolvedKeybindingsConfig for clients that read it.
    json!({
        "environment": environment_descriptor(state),
        "auth": auth_descriptor(state),
        "cwd": state.project_dir.to_string_lossy(),
        "keybindingsConfigPath": keybindings_path(state).to_string_lossy(),
        "keybindings": { "bindings": bindings, "issues": issues },
        "resolvedKeybindings": resolved,
        "issues": issues,
        "providers": provider_catalog(),
        "availableEditors": [],
        "observability": { "otlp": { "enabled": false } },
        "settings": Value::Object(load_settings(state)),
    })
}

// ---------------------------------------------------------------------------
// Keybindings — `keybindings.json` is `{bindings: [{key, command, when?}]}`.
// ---------------------------------------------------------------------------

struct KeybindingRule {
    key: String,
    command: String,
    when: Option<String>,
}

fn parse_rule(value: &Value) -> Option<KeybindingRule> {
    let key = value.get("key").and_then(Value::as_str)?.trim().to_string();
    let command = value.get("command").and_then(Value::as_str)?.to_string();
    if key.is_empty() || key.len() > 64 || command.is_empty() {
        return None;
    }
    let when = value
        .get("when")
        .and_then(Value::as_str)
        .map(str::to_string);
    if when
        .as_deref()
        .is_some_and(|w| w.is_empty() || w.len() > 256)
    {
        return None;
    }
    Some(KeybindingRule { key, command, when })
}

/// Raw file entries preserved verbatim (TS keeps undecodable rows), with the
/// decoded rule alongside when it parses.
fn load_keybinding_entries(state: &AppState) -> (Vec<(Value, Option<KeybindingRule>)>, Vec<Value>) {
    let path = keybindings_path(state);
    let Some(file) = read_json_file(&path) else {
        return (Vec::new(), Vec::new());
    };
    let entries = match file.get("bindings").and_then(Value::as_array) {
        Some(a) => a.clone(),
        None => {
            return (
                Vec::new(),
                vec![json!({
                    "kind": "keybindings.malformed-config",
                    "message": "keybindings.json must be an object with a bindings array",
                })],
            );
        }
    };
    let mut issues = Vec::new();
    let parsed = entries
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            let rule = parse_rule(&entry);
            if rule.is_none() {
                issues.push(json!({
                    "kind": "keybindings.invalid-entry",
                    "index": index,
                    "message": "Invalid keybinding rule",
                }));
            }
            (entry, rule)
        })
        .collect();
    (parsed, issues)
}

fn load_keybinding_rules(state: &AppState) -> (Vec<KeybindingRule>, Vec<Value>) {
    let (entries, issues) = load_keybinding_entries(state);
    (entries.into_iter().filter_map(|(_, r)| r).collect(), issues)
}

struct Shortcut {
    key: String,
    meta: bool,
    ctrl: bool,
    shift: bool,
    alt: bool,
    mod_: bool,
}

/// Port of `parseKeybindingShortcut` in packages/shared/src/keybindings.ts.
fn parse_shortcut(value: &str) -> Option<Shortcut> {
    let mut tokens: Vec<String> = value
        .to_lowercase()
        .split('+')
        .map(|t| t.trim().to_string())
        .collect();
    let mut trailing_empty = 0usize;
    while tokens.last().is_some_and(|t| t.is_empty()) {
        trailing_empty += 1;
        tokens.pop();
    }
    if trailing_empty > 0 {
        tokens.push("+".to_string());
    }
    if tokens.is_empty() || tokens.iter().any(|t| t.is_empty()) {
        return None;
    }
    let mut sc = Shortcut {
        key: String::new(),
        meta: false,
        ctrl: false,
        shift: false,
        alt: false,
        mod_: false,
    };
    let mut have_key = false;
    for token in &tokens {
        match token.as_str() {
            "cmd" | "meta" => sc.meta = true,
            "ctrl" | "control" => sc.ctrl = true,
            "shift" => sc.shift = true,
            "alt" | "option" => sc.alt = true,
            "mod" => sc.mod_ = true,
            _ => {
                if have_key {
                    return None;
                }
                have_key = true;
                sc.key = match token.as_str() {
                    "space" => " ".to_string(),
                    "esc" => "escape".to_string(),
                    other => other.to_string(),
                };
            }
        }
    }
    have_key.then_some(sc)
}

/// Port of `encodeShortcut` — canonical key spelling for identity compares.
fn encode_shortcut(sc: &Shortcut) -> Option<String> {
    let mut modifiers: Vec<&str> = Vec::new();
    if sc.mod_ {
        modifiers.push("mod");
    }
    if sc.meta {
        modifiers.push("meta");
    }
    if sc.ctrl {
        modifiers.push("ctrl");
    }
    if sc.alt {
        modifiers.push("alt");
    }
    if sc.shift {
        modifiers.push("shift");
    }
    if sc.key.is_empty() || (sc.key != "+" && sc.key.contains('+')) {
        return None;
    }
    let key = if sc.key == " " { "space" } else { &sc.key };
    Some(
        modifiers
            .iter()
            .copied()
            .chain(std::iter::once(key))
            .collect::<Vec<_>>()
            .join("+"),
    )
}

// ----- when-expression parser (port of parseKeybindingWhenExpression) -------

#[derive(Debug, Clone, PartialEq)]
enum WhenToken {
    Ident(String),
    Not,
    And,
    Or,
    LParen,
    RParen,
}

fn tokenize_when(expr: &str) -> Option<Vec<WhenToken>> {
    let bytes = expr.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if expr[i..].starts_with("&&") {
            tokens.push(WhenToken::And);
            i += 2;
            continue;
        }
        if expr[i..].starts_with("||") {
            tokens.push(WhenToken::Or);
            i += 2;
            continue;
        }
        match c {
            '!' => {
                tokens.push(WhenToken::Not);
                i += 1;
            }
            '(' => {
                tokens.push(WhenToken::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(WhenToken::RParen);
                i += 1;
            }
            _ => {
                let rest = &expr[i..];
                let mut end = 0usize;
                for (j, ch) in rest.char_indices() {
                    let ok = if j == 0 {
                        ch.is_ascii_alphabetic() || ch == '_'
                    } else {
                        ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-'
                    };
                    if !ok {
                        break;
                    }
                    end = j + ch.len_utf8();
                }
                if end == 0 {
                    return None;
                }
                tokens.push(WhenToken::Ident(rest[..end].to_string()));
                i += end;
            }
        }
    }
    Some(tokens)
}

struct WhenParser<'a> {
    tokens: &'a [WhenToken],
    index: usize,
}

impl WhenParser<'_> {
    fn primary(&mut self, depth: usize) -> Option<Value> {
        if depth > MAX_WHEN_EXPRESSION_DEPTH {
            return None;
        }
        match self.tokens.get(self.index)? {
            WhenToken::Ident(name) => {
                self.index += 1;
                Some(json!({ "type": "identifier", "name": name }))
            }
            WhenToken::LParen => {
                self.index += 1;
                let node = self.parse_or(depth + 1)?;
                if self.tokens.get(self.index) != Some(&WhenToken::RParen) {
                    return None;
                }
                self.index += 1;
                Some(node)
            }
            _ => None,
        }
    }

    fn unary(&mut self, depth: usize) -> Option<Value> {
        let mut nots = 0usize;
        while self.tokens.get(self.index) == Some(&WhenToken::Not) {
            self.index += 1;
            nots += 1;
            if nots > MAX_WHEN_EXPRESSION_DEPTH {
                return None;
            }
        }
        let mut node = self.primary(depth)?;
        while nots > 0 {
            node = json!({ "type": "not", "node": node });
            nots -= 1;
        }
        Some(node)
    }

    fn parse_and(&mut self, depth: usize) -> Option<Value> {
        let mut left = self.unary(depth)?;
        while self.tokens.get(self.index) == Some(&WhenToken::And) {
            self.index += 1;
            let right = self.unary(depth)?;
            left = json!({ "type": "and", "left": left, "right": right });
        }
        Some(left)
    }

    fn parse_or(&mut self, depth: usize) -> Option<Value> {
        let mut left = self.parse_and(depth)?;
        while self.tokens.get(self.index) == Some(&WhenToken::Or) {
            self.index += 1;
            let right = self.parse_and(depth)?;
            left = json!({ "type": "or", "left": left, "right": right });
        }
        Some(left)
    }
}

fn parse_when_expression(expr: &str) -> Option<Value> {
    let tokens = tokenize_when(expr)?;
    if tokens.is_empty() {
        return None;
    }
    let mut parser = WhenParser {
        tokens: &tokens,
        index: 0,
    };
    let ast = parser.parse_or(0)?;
    (parser.index == tokens.len()).then_some(ast)
}

/// Port of `encodeWhenAst` — canonical when spelling for identity compares.
fn encode_when(node: &Value) -> Option<String> {
    match node.get("type").and_then(Value::as_str)? {
        "identifier" => node.get("name").and_then(Value::as_str).map(str::to_owned),
        "not" => Some(format!("!({})", encode_when(node.get("node")?)?)),
        "and" => Some(format!(
            "({} && {})",
            encode_when(node.get("left")?)?,
            encode_when(node.get("right")?)?
        )),
        "or" => Some(format!(
            "({} || {})",
            encode_when(node.get("left")?)?,
            encode_when(node.get("right")?)?
        )),
        _ => None,
    }
}

/// `keybindingRuleIdentity` — rules compare on parsed key/when forms so
/// "cmd+k" and "meta+k" are the same rule.
fn rule_identity(rule: &KeybindingRule) -> String {
    let key = parse_shortcut(&rule.key)
        .and_then(|sc| encode_shortcut(&sc))
        .unwrap_or_else(|| rule.key.clone());
    let when = rule.when.as_deref().and_then(|w| {
        Some(
            parse_when_expression(w)
                .and_then(|ast| encode_when(&ast))
                .unwrap_or_else(|| w.to_string()),
        )
    });
    json!([rule.command, key, when]).to_string()
}

/// `{command, shortcut, whenAst?}` — ResolvedKeybindingRule, or None when the
/// rule does not compile.
fn resolve_rule(rule: &KeybindingRule) -> Option<Value> {
    let sc = parse_shortcut(&rule.key)?;
    let shortcut = json!({
        "key": sc.key,
        "metaKey": sc.meta,
        "ctrlKey": sc.ctrl,
        "shiftKey": sc.shift,
        "altKey": sc.alt,
        "modKey": sc.mod_,
    });
    let mut resolved = json!({ "command": rule.command, "shortcut": shortcut });
    if let Some(when) = &rule.when {
        resolved["whenAst"] = parse_when_expression(when)?;
    }
    Some(resolved)
}

fn resolve_keybindings(rules: &[KeybindingRule]) -> Vec<Value> {
    rules.iter().filter_map(resolve_rule).collect()
}

fn rule_from_payload(value: &Value) -> Option<KeybindingRule> {
    parse_rule(value)
}

fn keybindings_result(rules: Vec<KeybindingRule>, issues: Vec<Value>) -> RpcResult {
    let resolved = resolve_keybindings(&rules);
    RpcResult::ok(json!({ "keybindings": resolved, "issues": issues }))
}

fn write_keybindings(
    state: &AppState,
    entries: &[(Value, Option<KeybindingRule>)],
) -> Result<(), String> {
    let bindings: Vec<Value> = entries.iter().map(|(v, _)| v.clone()).collect();
    write_json_atomic(&keybindings_path(state), &json!({ "bindings": bindings }))
}

fn upsert_keybinding(state: &AppState, payload: &Value) -> RpcResult {
    let Some(rule) = rule_from_payload(payload) else {
        return RpcResult::err(client_error("invalid keybinding rule"));
    };
    if parse_shortcut(&rule.key).is_none() {
        return RpcResult::err(client_error(format!(
            "unparseable keybinding key {:?}",
            rule.key
        )));
    }
    if let Some(when) = &rule.when {
        if parse_when_expression(when).is_none() {
            return RpcResult::err(client_error(format!(
                "unparseable keybinding when {:?}",
                when
            )));
        }
    }
    let replace_target = payload.get("replace").and_then(rule_from_payload);
    let (entries, _) = load_keybinding_entries(state);
    let new_identity = rule_identity(&rule);
    let replace_identity = replace_target.as_ref().map(rule_identity);
    let mut next: Vec<(Value, Option<KeybindingRule>)> = entries
        .into_iter()
        .filter(|(_, existing)| match existing {
            // Undecodable rows are kept verbatim.
            None => true,
            Some(e) => {
                let id = rule_identity(e);
                if id == new_identity {
                    return false;
                }
                match &replace_identity {
                    Some(target) => id != *target,
                    None => true,
                }
            }
        })
        .collect();
    next.push((rule_value(&rule), Some(rule)));
    if next.len() > MAX_KEYBINDINGS_COUNT {
        next = next.split_off(next.len() - MAX_KEYBINDINGS_COUNT);
    }
    if let Err(e) = write_keybindings(state, &next) {
        return internal_error("failed to write keybindings config", e);
    }
    let (rules, issues) = load_keybinding_rules(state);
    keybindings_result(rules, issues)
}

fn remove_keybinding(state: &AppState, payload: &Value) -> RpcResult {
    let Some(target) = rule_from_payload(payload) else {
        return RpcResult::err(client_error("invalid keybinding rule"));
    };
    let target_identity = rule_identity(&target);
    let (entries, _) = load_keybinding_entries(state);
    let next: Vec<(Value, Option<KeybindingRule>)> = entries
        .into_iter()
        .filter(|(_, existing)| match existing {
            None => true,
            Some(e) => rule_identity(e) != target_identity,
        })
        .collect();
    if let Err(e) = write_keybindings(state, &next) {
        return internal_error("failed to write keybindings config", e);
    }
    let (rules, issues) = load_keybinding_rules(state);
    keybindings_result(rules, issues)
}

fn rule_value(rule: &KeybindingRule) -> Value {
    let mut o = Map::new();
    o.insert("key".into(), json!(rule.key));
    o.insert("command".into(), json!(rule.command));
    if let Some(when) = &rule.when {
        o.insert("when".into(), json!(when));
    }
    Value::Object(o)
}

// ---------------------------------------------------------------------------
// Settings — `settings.json` is the flat ServerSettings object; updateSettings
// merges top-level patch keys.
// ---------------------------------------------------------------------------

fn update_settings(state: &AppState, payload: &Value) -> RpcResult {
    let path = settings_path(state);
    let mut settings = load_settings(state);
    if let Some(patch) = payload.get("patch").and_then(Value::as_object) {
        for (k, v) in patch {
            settings.insert(k.clone(), v.clone());
        }
    }
    if let Err(e) = write_json_atomic(&path, &Value::Object(settings.clone())) {
        return internal_error("failed to write settings", e);
    }
    RpcResult::ok(Value::Object(settings))
}

// ---------------------------------------------------------------------------
// /proc-based diagnostics
// ---------------------------------------------------------------------------

fn read_proc(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn proc_self_stat() -> Option<(i64, i64, i64, i64, i64)> {
    // (ppid, pgrp, utime, stime, starttime) in jiffies where applicable.
    let stat = read_proc("/proc/self/stat")?;
    let rparen = stat.rfind(')')?;
    let fields: Vec<&str> = stat[rparen + 1..].split_whitespace().collect();
    // fields[0] = state (field 3); index n → field n+3.
    let get = |idx: usize| fields.get(idx)?.parse::<i64>().ok();
    Some((get(1)?, get(2)?, get(11)?, get(12)?, get(19)?))
}

fn proc_self_rss() -> u64 {
    if let Some(status) = read_proc("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                if let Some(kb) = rest
                    .trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse::<u64>()
                    .ok()
                {
                    return kb * 1024;
                }
            }
        }
    }
    0
}

fn proc_self_cmdline() -> String {
    std::fs::read("/proc/self/cmdline")
        .ok()
        .map(|b| {
            b.split(|c| *c == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "c67-server".to_string())
}

fn proc_uptime_secs() -> f64 {
    read_proc("/proc/uptime")
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn proc_btime() -> i64 {
    read_proc("/proc/stat")
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("btime ")?.trim().parse::<i64>().ok())
        })
        .unwrap_or(0)
}

const CLOCK_TICKS: i64 = 100;

fn format_elapsed(ms: u64) -> String {
    let total = ms / 1_000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn process_diagnostics() -> RpcResult {
    let pid = std::process::id() as i64;
    let (ppid, pgid, utime, stime, starttime) = proc_self_stat().unwrap_or((0, 0, 0, 0, 0));
    let rss = proc_self_rss();
    let uptime_secs = proc_uptime_secs();
    let elapsed_ms =
        ((uptime_secs - (starttime as f64 / CLOCK_TICKS as f64)) * 1000.0).max(0.0) as u64;
    let start_ms = (proc_btime() + starttime / CLOCK_TICKS) * 1000;
    let cpu_time_secs = (utime + stime) as f64 / CLOCK_TICKS as f64;
    let cpu_percent = if elapsed_ms > 0 {
        cpu_time_secs / (elapsed_ms as f64 / 1000.0) * 100.0
    } else {
        0.0
    };
    let entry = json!({
        "pid": pid,
        "startTimeMs": start_ms,
        "ppid": ppid,
        "pgid": { "_id": "Option", "_tag": "Some", "value": pgid },
        "status": "running",
        "cpuPercent": cpu_percent,
        "rssBytes": rss,
        "elapsed": format_elapsed(elapsed_ms),
        "command": proc_self_cmdline(),
        "depth": 0,
        "childPids": [],
    });
    RpcResult::ok(json!({
        "serverPid": pid,
        "readAt": domain::now_iso(),
        "processCount": 1,
        "totalRssBytes": rss,
        "totalCpuPercent": cpu_percent,
        "processes": [entry],
        "error": { "_id": "Option", "_tag": "None" },
    }))
}

fn cpu_totals() -> Option<(u64, u64, usize)> {
    // (busy+idle jiffies, idle jiffies, cpu count)
    let stat = read_proc("/proc/stat")?;
    let mut total = 0u64;
    let mut idle = 0u64;
    let mut count = 0usize;
    for line in stat.lines() {
        if line.starts_with("cpu ") {
            let nums: Vec<u64> = line[4..]
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            // user nice system idle iowait irq softirq steal ...
            if nums.len() >= 5 {
                idle = nums[3] + nums[4];
                total = nums.iter().sum();
            }
        } else if line.starts_with("cpu") {
            count += 1;
        } else {
            break;
        }
    }
    if count == 0 {
        count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
    }
    Some((total, idle, count))
}

fn meminfo() -> (u64, u64) {
    // (total bytes, available bytes)
    let mut total = 0u64;
    let mut available = 0u64;
    if let Some(m) = read_proc("/proc/meminfo") {
        for line in m.lines() {
            let mut parts = line.split_whitespace();
            let (Some(key), Some(val)) = (parts.next(), parts.next()) else {
                continue;
            };
            let Ok(kb) = val.parse::<u64>() else { continue };
            match key {
                "MemTotal:" => total = kb * 1024,
                "MemAvailable:" => available = kb * 1024,
                _ => {}
            }
        }
    }
    (total, available)
}

async fn host_resources() -> RpcResult {
    let before = cpu_totals();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after = cpu_totals();
    let (cpu_count, utilization) = match (before, after) {
        (Some((t1, i1, c1)), Some((t2, i2, c2))) if c1 == c2 && t2 > t1 && i2 >= i1 => {
            let util = 1.0 - (i2 - i1) as f64 / (t2 - t1) as f64;
            (c1 as u64, Some(util.clamp(0.0, 1.0)))
        }
        (Some((_, _, c)), _) | (_, Some((_, _, c))) => (c as u64, None),
        _ => (0, None),
    };
    let (total_mem, avail_mem) = meminfo();
    let sampled_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    RpcResult::ok(json!({
        "sampledAt": sampled_at,
        "cpuUtilization": utilization,
        "cpuCount": cpu_count,
        "availableMemoryBytes": avail_mem.min(total_mem),
        "totalMemoryBytes": total_mem,
    }))
}

// ---------------------------------------------------------------------------
// Database storage & cleanup
// ---------------------------------------------------------------------------

fn open_db(path: &Path) -> Result<rusqlite::Connection, String> {
    rusqlite::Connection::open(path).map_err(|e| format!("open {}: {e}", path.display()))
}

fn pragma_i64(conn: &rusqlite::Connection, name: &str) -> i64 {
    conn.pragma_query_value(None, name, |row| row.get::<_, i64>(0))
        .unwrap_or(0)
}

fn database_storage(state: &AppState) -> RpcResult {
    let path = db_path(state);
    if !path.exists() {
        return RpcResult::ok(Value::Null);
    }
    let conn = match open_db(&path) {
        Ok(c) => c,
        Err(e) => return internal_error("failed to open database", e),
    };
    let page_size = pragma_i64(&conn, "page_size").max(0) as u64;
    let page_count = pragma_i64(&conn, "page_count").max(0) as u64;
    let freelist = pragma_i64(&conn, "freelist_count").max(0) as u64;
    let auto_vacuum = pragma_i64(&conn, "auto_vacuum");
    RpcResult::ok(json!({
        "databaseBytes": page_count * page_size,
        "reclaimableBytes": freelist * page_size,
        "incrementalVacuumEnabled": auto_vacuum == 2,
        "compactCommand": format!("t3 doctor --base-dir {}", state.home_dir.display()),
        "canReclaim": false,
    }))
}

/// VACUUM via a second connection; returns bytes freed (never negative).
fn vacuum_db(path: &Path) -> Result<u64, String> {
    let before = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let conn = open_db(path)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
        .map_err(|e| format!("vacuum: {e}"))?;
    drop(conn);
    let after = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(before.saturating_sub(after))
}

fn run_storage_cleanup(state: &AppState) -> RpcResult {
    let started_at = domain::now_iso();
    let path = db_path(state);
    let mut entries = Vec::new();
    let mut freed = 0u64;
    if path.exists() {
        match vacuum_db(&path) {
            Ok(bytes) => {
                freed = bytes;
                entries.push(json!({
                    "kind": "logs",
                    "outcome": "removed",
                    "reason": "database compaction",
                    "path": path.to_string_lossy(),
                    "projectPath": null,
                    "threadId": null,
                    "threadTitle": null,
                    "bytes": bytes,
                    "files": null,
                }));
            }
            Err(e) => {
                tracing::warn!(error = %e, "database vacuum failed during storage cleanup");
                entries.push(json!({
                    "kind": "logs",
                    "outcome": "failed",
                    "reason": format!("database compaction: {e}"),
                    "path": path.to_string_lossy(),
                    "projectPath": null,
                    "threadId": null,
                    "threadTitle": null,
                    "bytes": null,
                    "files": null,
                }));
            }
        }
    }
    let (removed, failed) = entries.iter().fold((0u64, 0u64), |(r, f), e| {
        match e.get("outcome").and_then(Value::as_str) {
            Some("removed") => (r + 1, f),
            Some("failed") => (r, f + 1),
            _ => (r + 1, f), // kept
        }
    });
    let kept = entries.len() as u64 - removed - failed;
    let report = json!({
        "trigger": "manual",
        "startedAt": started_at,
        "finishedAt": domain::now_iso(),
        "entries": entries,
        "counts": { "removed": removed, "kept": kept, "failed": failed },
        "omittedCount": 0,
        "bytesFreed": freed,
    });
    let _ = write_json_atomic(&cleanup_report_path(state), &report);
    RpcResult::ok(report)
}

fn reclaim_database_space(state: &AppState) -> RpcResult {
    let path = db_path(state);
    if !path.exists() {
        return RpcResult::err(json!({
            "_tag": "DatabaseReclaimError",
            "reason": "No database found at this path.",
        }));
    }
    match vacuum_db(&path) {
        Ok(_) => RpcResult::ok(Value::Null),
        Err(e) => RpcResult::err(json!({
            "_tag": "DatabaseReclaimError",
            "reason": format!("Could not reclaim database space: {e}"),
        })),
    }
}

// ---------------------------------------------------------------------------
// Usage summary — aggregates whatever usage/token fields provider turn
// payloads carry into the TS UsageSummary shape. Sources this server runs
// write no usage records today, so an empty-but-valid summary is normal.
// ---------------------------------------------------------------------------

fn usage_provider_kind(raw: &str) -> Option<&'static str> {
    match raw.to_ascii_lowercase().as_str() {
        "claude" | "claudecode" | "claude-code" => Some("claude"),
        "codex" => Some("codex"),
        "grok" => Some("grok"),
        "cursor" => Some("cursor"),
        "opencode" => Some("opencode"),
        "antigravity" => Some("antigravity"),
        "pi" => Some("pi"),
        "muse" => Some("muse"),
        _ => None,
    }
}

fn num_field(v: &Value, keys: &[&str]) -> u64 {
    for k in keys {
        if let Some(n) = v.get(*k) {
            if let Some(i) = n.as_u64() {
                return i;
            }
            if let Some(f) = n.as_f64() {
                if f >= 0.0 {
                    return f as u64;
                }
            }
        }
    }
    0
}

/// Token totals out of a usage-ish object; None when it carries no tokens.
fn token_totals(usage: &Value) -> Option<[u64; 5]> {
    let t = [
        num_field(
            usage,
            &[
                "uncachedInputTokens",
                "inputTokens",
                "input",
                "promptTokens",
            ],
        ),
        num_field(
            usage,
            &[
                "cachedInputTokens",
                "cacheReadInputTokens",
                "cacheReadTokens",
            ],
        ),
        num_field(
            usage,
            &[
                "cacheCreationTokens",
                "cacheCreationInputTokens",
                "cacheWriteTokens",
            ],
        ),
        num_field(usage, &["outputTokens", "output", "completionTokens"]),
        num_field(usage, &["reasoningTokens", "reasoning"]),
    ];
    (!t.iter().all(|&x| x == 0)).then_some(t)
}

/// Find a usage object on a payload: `usage`/`tokenUsage`/`tokens`/`totals`
/// sub-object first, then the payload itself.
fn usage_object(payload: &Value) -> Option<[u64; 5]> {
    for key in ["usage", "tokenUsage", "tokens", "totals"] {
        if let Some(u) = payload.get(key) {
            if let Some(t) = token_totals(u) {
                return Some(t);
            }
        }
    }
    token_totals(payload)
}

struct UsageAgg {
    totals: [u64; 5],
    records: u64,
    sessions: HashSet<String>,
}

fn get_usage_summary(state: &AppState, payload: &Value) -> RpcResult {
    let started = Instant::now();
    let since_day = payload
        .get("sinceDay")
        .and_then(Value::as_str)
        .unwrap_or("");
    let until_day = payload
        .get("untilDay")
        .and_then(Value::as_str)
        .unwrap_or("9999-12-31");
    let time_zone = payload
        .get("timeZone")
        .and_then(Value::as_str)
        .unwrap_or("UTC");
    let hourly = payload.get("resolution").and_then(Value::as_str) == Some("hour");

    // UsageModelFilter: `include` keeps matching records (absent = all),
    // `exclude` drops matches. A selector is {provider?, model}; an omitted
    // provider matches any.
    let model_filter = payload.get("modelFilter");
    let sel_matches = |sel: &Value, provider: &str, model: &str| {
        sel.get("model").and_then(Value::as_str) == Some(model)
            && sel
                .get("provider")
                .and_then(Value::as_str)
                .is_none_or(|p| p == provider)
    };
    let model_allowed = |provider: &str, model: &str| -> bool {
        match model_filter {
            None => true,
            Some(f) => {
                let inc_ok = f
                    .get("include")
                    .and_then(Value::as_array)
                    .is_none_or(|arr| arr.iter().any(|s| sel_matches(s, provider, model)));
                let exc_hit = f
                    .get("exclude")
                    .and_then(Value::as_array)
                    .is_some_and(|arr| arr.iter().any(|s| sel_matches(s, provider, model)));
                inc_ok && !exc_hit
            }
        }
    };

    // key: (day, hourStart, provider, model)
    let mut buckets: BTreeMap<(String, Option<String>, String, String), UsageAgg> = BTreeMap::new();
    let mut provider_sessions: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    let mut malformed: u64 = 0;
    let mut record_row =
        |timestamp: &str, provider: &str, model: &str, thread_id: &str, t: [u64; 5]| {
            if !model_allowed(provider, model) {
                return;
            }
            provider_sessions
                .entry(provider.to_string())
                .or_default()
                .insert(thread_id.to_string());
            let day = timestamp.get(..10).unwrap_or("").to_string();
            if day.is_empty()
                || (!since_day.is_empty() && day.as_str() < since_day)
                || day.as_str() > until_day
            {
                return;
            }
            let hour_start = if hourly {
                timestamp.get(..13).map(|h| format!("{h}:00"))
            } else {
                None
            };
            let agg = buckets
                .entry((day, hour_start, provider.to_string(), model.to_string()))
                .or_insert_with(|| UsageAgg {
                    totals: [0; 5],
                    records: 0,
                    sessions: HashSet::new(),
                });
            for (a, b) in agg.totals.iter_mut().zip(t.iter()) {
                *a += b;
            }
            agg.records += 1;
            agg.sessions.insert(thread_id.to_string());
        };

    let path = db_path(state);
    if path.exists() {
        match open_db(&path) {
            Ok(conn) => {
                // Provider turns: day comes from the turn's start/completion.
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT thread_id, started_at, completed_at, payload_json \
                     FROM orchestration_v2_projection_provider_turns",
                ) {
                    if let Ok(rows) = stmt.query_map([], |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    }) {
                        for row in rows.flatten() {
                            let (thread_id, started_at, completed_at, payload_json) = row;
                            let Ok(p) = serde_json::from_str::<Value>(&payload_json) else {
                                malformed += 1;
                                continue;
                            };
                            let Some(t) = usage_object(&p) else { continue };
                            let provider = p
                                .get("providerInstanceId")
                                .or_else(|| p.get("provider"))
                                .or_else(|| p.get("driver"))
                                .and_then(Value::as_str)
                                .and_then(usage_provider_kind);
                            let Some(provider) = provider else { continue };
                            let model = p
                                .get("model")
                                .or_else(|| p.get("modelSlug"))
                                .and_then(Value::as_str)
                                .unwrap_or("unknown");
                            let ts = completed_at
                                .as_deref()
                                .or(started_at.as_deref())
                                .unwrap_or("");
                            record_row(ts, provider, model, thread_id.as_deref().unwrap_or(""), t);
                        }
                    }
                }
                // Turn items may also carry token/usage rows.
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT thread_id, updated_at, payload_json \
                     FROM orchestration_v2_projection_turn_items \
                     WHERE type LIKE '%usage%'",
                ) {
                    if let Ok(rows) = stmt.query_map([], |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    }) {
                        for row in rows.flatten() {
                            let (thread_id, updated_at, payload_json) = row;
                            let Ok(p) = serde_json::from_str::<Value>(&payload_json) else {
                                malformed += 1;
                                continue;
                            };
                            let Some(t) = usage_object(&p) else { continue };
                            let provider = p
                                .get("providerInstanceId")
                                .or_else(|| p.get("provider"))
                                .or_else(|| p.get("driver"))
                                .and_then(Value::as_str)
                                .and_then(usage_provider_kind)
                                .unwrap_or("codex");
                            let model = p
                                .get("model")
                                .or_else(|| p.get("modelSlug"))
                                .and_then(Value::as_str)
                                .unwrap_or("unknown");
                            record_row(
                                &updated_at,
                                provider,
                                model,
                                thread_id.as_deref().unwrap_or(""),
                                t,
                            );
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "usage summary: failed to open database");
            }
        }
    }

    let bucket_values: Vec<Value> = buckets
        .into_iter()
        .map(|((day, hour_start, provider, model), agg)| {
            let mut b = json!({
                "day": day,
                "provider": provider,
                "model": model,
                "totals": {
                    "uncachedInputTokens": agg.totals[0],
                    "cachedInputTokens": agg.totals[1],
                    "cacheCreationTokens": agg.totals[2],
                    "outputTokens": agg.totals[3],
                    "reasoningTokens": agg.totals[4],
                },
                "costUsd": 0,
                "cacheSavingsUsd": 0,
                "costSource": "unpriced",
                "records": agg.records,
                "unpricedRecords": agg.records,
                "sessions": agg.sessions.len(),
            });
            if let Some(h) = hour_start {
                b["hourStart"] = json!(h);
            }
            b
        })
        .collect();

    // Honest sources: one entry per provider whose rows we aggregated from
    // the local orchestration projection store.
    let volume_id = std::fs::metadata(&path)
        .map(|m| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                format!("{}:{}", m.dev(), m.ino())
            }
            #[cfg(not(unix))]
            {
                let _ = m;
                String::new()
            }
        })
        .unwrap_or_default();
    let sources: Vec<Value> = provider_sessions
        .iter()
        .map(|(provider, sessions)| {
            json!({
                "fingerprint": {
                    "hostId": environment_id(state),
                    "provider": provider,
                    "resolvedHomePath": path.to_string_lossy(),
                    "volumeId": volume_id,
                },
                "status": "ok",
                "scannedFiles": 1,
                "skippedFiles": 0,
                "malformedRecords": malformed,
                "distinctSessions": sessions.len(),
                "message": if malformed > 0 {
                    json!(format!("{malformed} projection rows could not be decoded"))
                } else {
                    Value::Null
                },
            })
        })
        .collect();

    let mut out = json!({
        "contractVersion": USAGE_CONTRACT_VERSION,
        "readAt": domain::now_iso(),
        "timeZone": time_zone,
        "sinceDay": since_day,
        "untilDay": if until_day == "9999-12-31" { Value::from(since_day) } else { Value::from(until_day) },
        "buckets": bucket_values,
        "sources": sources,
        "pricing": {
            "status": "unavailable",
            "source": "none",
            "fetchedAt": null,
            "knownModels": 0,
        },
        "scanDurationMs": started.elapsed().as_millis() as u64,
    });
    if let Some(filter) = payload.get("modelFilter") {
        out["modelFilter"] = filter.clone();
    }
    RpcResult::ok(out)
}

// ---------------------------------------------------------------------------
// Triage — writes a session folder with context.md + triage-playbook.md and
// returns the composer prompt linking it, like TriageService.prepare.
// ---------------------------------------------------------------------------

fn describe_triage_client(client: &Value) -> String {
    let label = client
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("an unknown client");
    let mut parts = vec![label.to_string()];
    if let Some(v) = client.get("appVersion").and_then(Value::as_str) {
        parts.push(format!("version {v}"));
    }
    if let Some(os) = client.get("os").and_then(Value::as_str) {
        parts.push(format!("on {os}"));
    }
    if let Some(b) = client.get("browser").and_then(Value::as_str) {
        parts.push(format!("in {b}"));
    }
    if client.get("webDeployment").and_then(Value::as_str) == Some("hosted") {
        parts.push("(app.t3.codes)".to_string());
    }
    parts.join(" ")
}

fn markdown_link_destination(path: &str) -> String {
    path.replace('\\', "%5C")
        .replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29")
        .replace('#', "%23")
        .replace('?', "%3F")
}

async fn prepare_triage(state: &AppState, payload: &Value) -> RpcResult {
    let client_desc = describe_triage_client(payload.get("client").unwrap_or(&Value::Null));
    let stamp = domain::now_iso().replace(':', "-").replace('.', "-");
    let session_dir = state.home_dir.join("userdata").join("triage").join(&stamp);
    if let Err(e) = std::fs::create_dir_all(&session_dir) {
        return RpcResult::err(json!({
            "_tag": "ServerPrepareTriageError",
            "cause": format!("create {}: {e}", session_dir.display()),
        }));
    }

    // Gather recent threads/projects for the context file.
    let snapshot = state.store.shell_snapshot().await;
    let mut recent = String::new();
    if let Some(projects) = snapshot.get("projects").and_then(Value::as_array) {
        recent.push_str("\n## Projects\n\n");
        for p in projects.iter().take(20) {
            let title = p.get("title").and_then(Value::as_str).unwrap_or("?");
            let root = p.get("workspaceRoot").and_then(Value::as_str).unwrap_or("");
            let id = p.get("id").and_then(Value::as_str).unwrap_or("");
            recent.push_str(&format!("- {title} — {root} ({id})\n"));
        }
    }
    if let Some(threads) = snapshot.get("threads").and_then(Value::as_array) {
        recent.push_str("\n## Recent threads\n\n");
        for t in threads.iter().take(20) {
            let title = t.get("title").and_then(Value::as_str).unwrap_or("?");
            let id = t.get("id").and_then(Value::as_str).unwrap_or("");
            let updated = t.get("updatedAt").and_then(Value::as_str).unwrap_or("");
            recent.push_str(&format!("- {title} ({id}, updated {updated})\n"));
        }
    }

    let os = format!(
        "{} {} ({})",
        std::env::consts::OS,
        std::env::consts::ARCH,
        read_proc("/proc/sys/kernel/osrelease")
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".into())
    );
    let state_dir = state.home_dir.join("userdata");
    let context_path = session_dir.join("context.md");
    let context = format!(
        "# T3 Code triage context\n\n\
         Generated by the T3 Code app at {at}.\n\n\
         - Started from: {client} (the client the user pressed \"Triage an issue\" in)\n\
         - OS: {os}\n\
         - Runtime: c67-server (Rust)\n\
         - Server process: running (pid {pid}); this session runs inside it\n\
         - Repo: https://github.com/pingdotgg/t3code\n\n\
         ## This session runs inside T3 Code\n\n\
         You are a thread inside the T3 Code server described above. Stopping,\n\
         restarting, or killing that server ends this session. The database is\n\
         in use by that server: read it, never lock it.\n\n\
         ## Paths\n\n\
         - State dir: {state}\n\
         - Database (SQLite; write only with the user's explicit permission): {db}\n\
         - Settings: {settings}\n\
         - Keybindings: {keybindings}\n\
         - Logs dir: {logs}\n\
         - Secrets dir (NEVER read this): {secrets}\n\
         {recent}",
        at = domain::now_iso(),
        client = client_desc,
        os = os,
        pid = std::process::id(),
        state = state_dir.display(),
        db = db_path(state).display(),
        settings = settings_path(state).display(),
        keybindings = keybindings_path(state).display(),
        logs = state_dir.join("logs").display(),
        secrets = state_dir.join("secrets").display(),
        recent = recent,
    );
    if let Err(e) = std::fs::write(&context_path, context) {
        return RpcResult::err(json!({
            "_tag": "ServerPrepareTriageError",
            "cause": format!("write {}: {e}", context_path.display()),
        }));
    }

    let prompt_path = session_dir.join("triage-playbook.md");
    let seed = format!(
        "A T3 Code user is having a problem with their install and started this \
         session from the T3 Code app.\n\n\
         Machine facts (versions, OS, paths, recent projects and threads) are in \
         the triage context file:\n\n    {}\n\n\
         Start by asking the user what went wrong if their message does not \
         already describe it. Then gather evidence from the paths listed in the \
         context file, identify the likely cause, and propose a fix.\n",
        context_path.display()
    );
    if let Err(e) = std::fs::write(&prompt_path, seed) {
        return RpcResult::err(json!({
            "_tag": "ServerPrepareTriageError",
            "cause": format!("write {}: {e}", prompt_path.display()),
        }));
    }

    RpcResult::ok(json!({
        "prompt": format!(
            "Read [triage-playbook.md]({}) and follow it. ",
            markdown_link_destination(&prompt_path.to_string_lossy())
        ),
    }))
}

// ---------------------------------------------------------------------------
// Secrets — resolve a pending secret prompt when one is registered.
// ---------------------------------------------------------------------------

fn secrets_answer(state: &AppState, payload: &Value) -> RpcResult {
    let answer_type = payload
        .get("answer")
        .and_then(|a| a.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let decision = match answer_type {
        "save" => payload
            .get("answer")
            .and_then(|a| a.get("secret"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => "decline".to_string(),
    };
    // Pending prompts are keyed by their runtime request id; the turn item id
    // and request/clientRequest ids are all plausible keys.
    let keys: Vec<String> = ["requestId", "clientRequestId", "turnItemId"]
        .iter()
        .filter_map(|k| payload.get(*k).and_then(Value::as_str).map(str::to_owned))
        .collect();
    let sender = keys.iter().find_map(|k| {
        state
            .pending_requests
            .lock()
            .map(|mut m| m.remove(k))
            .unwrap_or(None)
    });
    match sender {
        Some(tx) => {
            let _ = tx.send(decision);
            RpcResult::ok(json!({ "answered": true }))
        }
        None => RpcResult::ok(json!({ "answered": false })),
    }
}

// ---------------------------------------------------------------------------
// Subprocess helpers shared by the external-tool RPCs
// ---------------------------------------------------------------------------

use tokio::process::Command;
use tokio::time::timeout;

struct ProbeOut {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run a program to completion with a hard timeout. `Err(())` means the
/// binary is missing; `Err` details are kept in logs, not leaked to clients.
async fn probe_cmd(program: &str, args: &[&str], secs: u64) -> Result<ProbeOut, String> {
    let child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let child = match child {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("{program} not on PATH"))
        }
        Err(e) => return Err(format!("spawn {program}: {e}")),
    };
    match timeout(Duration::from_secs(secs), child.wait_with_output()).await {
        Ok(Ok(out)) => Ok(ProbeOut {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
        Ok(Err(e)) => Err(format!("wait {program}: {e}")),
        Err(_) => Err(format!("{program} timed out after {secs}s")),
    }
}

fn first_line(s: &str) -> Option<String> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

fn wire_opt(v: Option<Value>) -> Value {
    match v {
        Some(value) => json!({ "_id": "Option", "_tag": "Some", "value": value }),
        None => json!({ "_id": "Option", "_tag": "None" }),
    }
}

fn wire_opt_str(v: Option<String>) -> Value {
    wire_opt(v.map(Value::String))
}

// ---------------------------------------------------------------------------
// server.discoverSourceControl — CLI/API probes mirroring
// packages/source-control-* discovery specs and SourceControlDiscovery.ts.
// ---------------------------------------------------------------------------

const GIT_INSTALL_HINT: &str =
    "Install Git from https://git-scm.com/downloads or with your package manager.";
const JJ_INSTALL_HINT: &str =
    "Install Jujutsu with `brew install jj` or from https://github.com/jj-vcs/jj.";
const GH_INSTALL_HINT: &str = "Install the GitHub command-line tool (`gh`) via https://cli.github.com/ or your package manager (for example `brew install gh`).";
const ORIGIN_INSTALL_HINT: &str = "Install the Origin CLI with `curl -fsSL https://downloads.cursor.com/origin/install.sh | sh`, then run `origin auth login`.";
const GLAB_INSTALL_HINT: &str = "Install the GitLab command-line tool (`glab`) from https://gitlab.com/gitlab-org/cli or your package manager (for example `brew install glab`).";
const CAFE_INSTALL_HINT: &str = "Install the GitCafe CLI with `bun install -g @gitcafe/cli`, then run `cafe auth login --host https://git.cafe/api`, save a token in Settings, or set CAFE_TOKEN on the server.";
const TEA_INSTALL_HINT: &str = "Install `fj` 0.6 or later from https://codeberg.org/forgejo-contrib/forgejo-cli and run `fj --host <server-url> auth add-token`, or install `tea` 0.16 or later from https://gitea.com/gitea/tea and run `tea login add` for each Forgejo or Gitea server.";
const AZ_INSTALL_HINT: &str = "Install the Azure command-line tools (`az`), then enable Azure DevOps support with `az extension add --name azure-devops`.";
const BITBUCKET_INSTALL_HINT: &str = "Add a Bitbucket token in Settings → Source Control.";

/// Missing-CLI auth, per probeSourceControlProvider: unknown, with the same
/// detail the TS host reports.
const CLI_MISSING_AUTH_DETAIL: &str =
    "Hosting integration command was not found on the server PATH.";

/// SourceControlProviderAuth wire value.
fn provider_auth(
    status: &str,
    account: Option<String>,
    host: Option<String>,
    detail: Option<String>,
    accounts: Option<Vec<Value>>,
) -> Value {
    let mut v = json!({
        "status": status,
        "account": wire_opt_str(account),
        "host": wire_opt_str(host),
        "detail": wire_opt_str(detail),
    });
    if let Some(accounts) = accounts {
        v["accounts"] = json!(accounts);
    }
    v
}

fn first_safe_auth_line(out: &ProbeOut) -> Option<String> {
    first_line(&out.stdout).or_else(|| first_line(&out.stderr))
}

fn vcs_item(
    kind: &str,
    label: &str,
    executable: &str,
    implemented: bool,
    out: &Result<ProbeOut, String>,
    install_hint: &str,
) -> Value {
    match out {
        Ok(p) => json!({
            "kind": kind,
            "label": label,
            "executable": executable,
            "implemented": implemented,
            "status": "available",
            "version": wire_opt_str(first_line(&p.stdout).or_else(|| first_line(&p.stderr))),
            "installHint": install_hint,
            "detail": wire_opt_str(None),
        }),
        Err(detail) => json!({
            "kind": kind,
            "label": label,
            "executable": executable,
            "implemented": implemented,
            "status": "missing",
            "version": wire_opt_str(None),
            "installHint": install_hint,
            "detail": wire_opt_str(Some(detail.clone())),
        }),
    }
}

/// Shared provider-item fields when the CLI is missing.
fn provider_item(
    kind: &str,
    label: &str,
    version_probe: &Result<ProbeOut, String>,
    executable: &str,
    install_hint: &str,
    auth: Value,
    app_sign_in: bool,
) -> Value {
    let mut v = match version_probe {
        Ok(p) => json!({
            "kind": kind,
            "label": label,
            "executable": executable,
            "status": "available",
            "version": wire_opt_str(first_line(&p.stdout).or_else(|| first_line(&p.stderr))),
            "installHint": install_hint,
            "detail": wire_opt_str(None),
        }),
        Err(detail) => json!({
            "kind": kind,
            "label": label,
            "executable": executable,
            "status": "missing",
            "version": wire_opt_str(None),
            "installHint": install_hint,
            "detail": wire_opt_str(Some(detail.clone())),
        }),
    };
    v["auth"] = auth;
    if app_sign_in {
        v["appSignIn"] = json!(true);
    }
    v
}

fn missing_cli_auth() -> Value {
    provider_auth(
        "unknown",
        None,
        None,
        Some(CLI_MISSING_AUTH_DETAIL.into()),
        None,
    )
}

// --- per-provider auth parsers, mirroring the TS `parseAuth` functions ---

fn is_env_token_source(token_source: &str) -> bool {
    // /^(?:GH|GITHUB)_(?:ENTERPRISE_)?TOKEN$/
    matches!(
        token_source,
        "GH_TOKEN" | "GITHUB_TOKEN" | "GH_ENTERPRISE_TOKEN" | "GITHUB_ENTERPRISE_TOKEN"
    )
}

/// `settings.sourceControlHosts.github.hosts[host]` → {account?, enabled}.
fn github_host_choice(settings: &Value, host: &str) -> (bool, Option<String>) {
    let choice = settings
        .get("sourceControlHosts")
        .and_then(|s| s.get("github"))
        .and_then(|s| s.get("hosts"))
        .and_then(|h| h.get(host));
    match choice {
        Some(c) => (
            c.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            c.get("account")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        ),
        None => (true, None),
    }
}

struct GhAccount {
    host: String,
    account: String,
    authenticated: bool,
    active: bool,
    error: Option<String>,
    environment_variable: Option<String>,
}

fn parse_gh_auth(out: &ProbeOut, settings: &Value) -> Value {
    let parsed: Option<Value> = serde_json::from_str(&out.stdout).ok();
    let mut accounts: Vec<GhAccount> = Vec::new();
    let mut valid_json = false;
    if let Some(hosts) = parsed
        .as_ref()
        .and_then(|v| v.get("hosts"))
        .and_then(Value::as_object)
    {
        valid_json = true;
        for arr in hosts.values() {
            for a in arr.as_array().into_iter().flatten() {
                let host = a
                    .get("host")
                    .and_then(Value::as_str)
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty());
                let login = a
                    .get("login")
                    .and_then(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                let (host, account) = match (host, login) {
                    (Some(h), Some(l)) => (h, l),
                    _ => continue,
                };
                let token_source = a.get("tokenSource").and_then(Value::as_str).unwrap_or("");
                accounts.push(GhAccount {
                    host,
                    account,
                    authenticated: a.get("state").and_then(Value::as_str) == Some("success"),
                    active: a.get("active").and_then(Value::as_bool).unwrap_or(false),
                    error: a
                        .get("error")
                        .and_then(Value::as_str)
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                    environment_variable: is_env_token_source(token_source)
                        .then(|| token_source.to_string()),
                });
            }
        }
    }

    let accounts_wire = valid_json.then(|| {
        accounts
            .iter()
            .map(|a| {
                let mut v = json!({
                    "host": a.host,
                    "account": a.account,
                    "active": a.active,
                    "authenticated": a.authenticated,
                });
                if let Some(e) = &a.error {
                    v["error"] = json!(e);
                }
                if let Some(ev) = &a.environment_variable {
                    v["environmentVariable"] = json!(ev);
                }
                v
            })
            .collect::<Vec<_>>()
    });

    let fallback = accounts
        .iter()
        .find(|a| a.authenticated && a.active)
        .or_else(|| accounts.iter().find(|a| a.authenticated))
        .map(|a| a.host.clone());

    // Order hosts with the fallback host first, unique.
    let mut hosts: Vec<String> = Vec::new();
    for a in &accounts {
        if !hosts.contains(&a.host) {
            hosts.push(a.host.clone());
        }
    }
    if let Some(fh) = &fallback {
        if let Some(pos) = hosts.iter().position(|h| h == fh) {
            let h = hosts.remove(pos);
            hosts.insert(0, h);
        }
    }

    // effectiveGitHubAccount per host.
    let mut chosen: Option<&GhAccount> = None;
    'outer: for host in &hosts {
        let (enabled, choice_account) = github_host_choice(settings, host);
        if !enabled {
            continue;
        }
        let usable: Vec<&GhAccount> = accounts
            .iter()
            .filter(|a| &a.host == host && a.authenticated)
            .collect();
        let pick_host = usable
            .iter()
            .find(|a| a.environment_variable.is_some())
            .or_else(|| {
                choice_account
                    .as_ref()
                    .and_then(|want| usable.iter().find(|a| &a.account == want))
            })
            .or_else(|| usable.iter().find(|a| a.active).or_else(|| usable.first()));
        if let Some(a) = pick_host {
            chosen = Some(*a);
            break 'outer;
        }
    }

    if let Some(a) = chosen {
        return provider_auth(
            "authenticated",
            Some(a.account.clone()),
            Some(a.host.clone()),
            a.environment_variable.as_ref().map(|ev| {
                format!("Using {ev} from the server environment; it overrides the account chosen in Settings.")
            }),
            accounts_wire,
        );
    }
    if fallback.is_some() {
        return provider_auth(
            "unauthenticated",
            None,
            fallback,
            Some(
                "Every GitHub host gh is signed in to is turned off in Settings → Source Control."
                    .into(),
            ),
            accounts_wire,
        );
    }
    if valid_json {
        let failed = accounts
            .iter()
            .find(|a| a.active)
            .or_else(|| accounts.first());
        return provider_auth(
            "unauthenticated",
            None,
            failed.map(|a| a.host.clone()),
            Some(failed.and_then(|a| a.error.clone()).unwrap_or_else(|| {
                "Run `gh auth login` to authenticate GitHub CLI with an active account.".into()
            })),
            accounts_wire,
        );
    }

    let output = format!("{}\n{}", out.stdout, out.stderr);
    if out.code != Some(0) && output.contains("unknown flag: --json") {
        return provider_auth(
            "unknown",
            None,
            None,
            Some("GitHub CLI is too old to report sign-in status. Update `gh` to 2.81.0 or newer (for example `brew upgrade gh`) and rescan.".into()),
            None,
        );
    }
    if out.code != Some(0) {
        return provider_auth(
            "unauthenticated",
            None,
            None,
            Some(
                first_safe_auth_line(out)
                    .unwrap_or_else(|| "Run `gh auth login` to authenticate GitHub CLI.".into()),
            ),
            None,
        );
    }
    provider_auth(
        "unknown",
        None,
        None,
        Some(
            first_safe_auth_line(out)
                .unwrap_or_else(|| "GitHub CLI auth status could not be parsed.".into()),
        ),
        None,
    )
}

fn parse_origin_auth(out: &ProbeOut) -> Value {
    let parsed: Option<Value> = serde_json::from_str(&out.stdout).ok();
    let status = parsed.filter(|v| v.get("authenticated").is_some());
    match status {
        None => provider_auth("unknown", None, None, None, None),
        Some(v) => {
            let authenticated = v.get("authenticated").and_then(Value::as_bool) == Some(true)
                && v.get("tokenValid").and_then(Value::as_bool) != Some(false);
            if authenticated {
                let account = v
                    .get("account")
                    .and_then(|a| a.get("email"))
                    .and_then(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                provider_auth(
                    "authenticated",
                    account,
                    Some("origin.cursor.com".into()),
                    None,
                    None,
                )
            } else {
                provider_auth(
                    "unauthenticated",
                    None,
                    None,
                    Some("Run `origin auth login` to sign in to Cursor Origin.".into()),
                    None,
                )
            }
        }
    }
}

/// `glab auth status` text: host-name lines at column 0, then indented
/// `Logged in to <host> as <account>` detail lines.
fn parse_glab_auth(out: &ProbeOut) -> Value {
    let host_line = |line: &str| -> bool {
        if line.is_empty() || line.starts_with(char::is_whitespace) {
            return false;
        }
        let t = line.trim();
        // Host or [ipv6]:port, same shape as HOST_LINE_PATTERN.
        if t.starts_with('[') {
            return t.ends_with(']')
                || t.rsplit(']').next().map_or(false, |p| {
                    p.starts_with(':') && p[1..].chars().all(|c| c.is_ascii_digit())
                });
        }
        let (host, port) = match t.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (t, None),
        };
        !host.is_empty()
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
            && port.map_or(true, |p| p.chars().all(|c| c.is_ascii_digit()))
    };
    let logged_in_account = |block: &str| -> Option<String> {
        // /Logged in to .+? as\s+([^\s(]+)/
        for line in block.lines() {
            if let Some(pos) = line.find("Logged in to") {
                if let Some(as_pos) = line[pos..].rfind(" as ") {
                    let account = line[pos + as_pos + 4..]
                        .trim()
                        .split(|c: char| c.is_whitespace() || c == '(')
                        .next()
                        .unwrap_or("")
                        .trim();
                    if !account.is_empty() {
                        return Some(account.to_string());
                    }
                }
            }
        }
        None
    };

    let output = format!("{}\n{}", out.stdout, out.stderr);
    let mut hosts: Vec<(String, Option<String>)> = Vec::new();
    let mut current_host: Option<String> = None;
    let mut current_lines: Vec<String> = Vec::new();
    let flush = |hosts: &mut Vec<(String, Option<String>)>,
                 current_host: &mut Option<String>,
                 current_lines: &mut Vec<String>| {
        if let Some(h) = current_host.take() {
            let block = current_lines.join("\n");
            hosts.push((h, logged_in_account(&block)));
        }
        current_lines.clear();
    };
    for raw in output.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if host_line(raw) {
            flush(&mut hosts, &mut current_host, &mut current_lines);
            current_host = Some(line.to_lowercase());
        } else if current_host.is_some() {
            current_lines.push(line.to_string());
        }
    }
    flush(&mut hosts, &mut current_host, &mut current_lines);

    if let Some((host, account)) = hosts.iter().find(|(_, a)| a.is_some()) {
        return provider_auth(
            "authenticated",
            account.clone(),
            Some(host.clone()),
            None,
            None,
        );
    }
    // Whole-output fallbacks (as in parseGitLabAuth).
    let account = logged_in_account(&output).or_else(|| {
        for line in output.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("account:") {
                let a = rest
                    .trim()
                    .split(|c: char| c.is_whitespace() || c == '(')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if !a.is_empty() {
                    return Some(a);
                }
            }
        }
        None
    });
    let host = hosts.first().map(|(h, _)| h.clone());
    if account.is_some() {
        return provider_auth("authenticated", account, host, None, None);
    }
    if out.code != Some(0) {
        return provider_auth(
            "unauthenticated",
            None,
            host,
            Some(
                first_safe_auth_line(out)
                    .unwrap_or_else(|| "Run `glab auth login` to authenticate GitLab CLI.".into()),
            ),
            None,
        );
    }
    provider_auth(
        "unknown",
        None,
        host,
        Some(
            first_safe_auth_line(out)
                .unwrap_or_else(|| "GitLab CLI auth status could not be parsed.".into()),
        ),
        None,
    )
}

fn parse_cafe_auth(out: &ProbeOut) -> Value {
    let parsed: Option<Value> = serde_json::from_str(out.stdout.trim()).ok();
    let signed_in = parsed.and_then(|v| {
        if v.get("schemaVersion").and_then(Value::as_i64) != Some(1) {
            return None;
        }
        v.get("data")
            .and_then(|d| d.get("username"))
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    });
    if out.code == Some(0) {
        if let Some(username) = signed_in {
            return provider_auth(
                "authenticated",
                Some(username),
                Some("git.cafe".into()),
                None,
                None,
            );
        }
    }
    provider_auth(
        "unknown",
        None,
        Some("git.cafe".into()),
        Some("GitCafe authentication status could not be read. Run `cafe auth login --host https://git.cafe/api`.".into()),
        None,
    )
}

/// Host part of a URL like `https://gitea.example.com/org`.
fn url_host(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split(|c| matches!(c, '/' | '?' | '#')).next()?.trim();
    (!host.is_empty()).then(|| host.to_string())
}

fn parse_tea_auth(out: &ProbeOut) -> Value {
    let logins: Vec<Value> = serde_json::from_str::<Vec<Value>>(&out.stdout).unwrap_or_default();
    let pick = logins
        .iter()
        .find(|l| l.get("default").and_then(Value::as_str) == Some("true"))
        .or_else(|| logins.first());
    match pick {
        Some(login) => {
            let account = login
                .get("user")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let host = login.get("url").and_then(Value::as_str).and_then(url_host);
            provider_auth(
                if login.get("valid").and_then(Value::as_str) == Some("true") {
                    "authenticated"
                } else {
                    "unauthenticated"
                },
                account,
                host,
                None,
                None,
            )
        }
        None => provider_auth(
            "unauthenticated",
            None,
            None,
            Some("Run `tea login add` to authenticate a Forgejo or Gitea server.".into()),
            None,
        ),
    }
}

fn parse_az_auth(out: &ProbeOut) -> Value {
    let account = first_line(&out.stdout);
    if out.code != Some(0) {
        return provider_auth(
            "unauthenticated",
            None,
            None,
            Some(
                first_safe_auth_line(out)
                    .unwrap_or_else(|| "Run `az login` to authenticate Azure CLI.".into()),
            ),
            None,
        );
    }
    if let Some(account) = account {
        return provider_auth(
            "authenticated",
            Some(account),
            Some("dev.azure.com".into()),
            None,
            None,
        );
    }
    provider_auth(
        "unknown",
        None,
        Some("dev.azure.com".into()),
        Some("Azure CLI account status could not be parsed.".into()),
        None,
    )
}

/// Bitbucket credentials: settings `sourceControlHosts.bitbucket` literal
/// secrets first (only literal strings resolve — references stay unresolved),
/// then the T3CODE_BITBUCKET_* environment, matching resolveCredential.
fn bitbucket_credential(settings: &Value) -> Option<(String, Option<String>)> {
    // (auth_header_value, account_hint)
    let s = settings
        .get("sourceControlHosts")
        .and_then(|h| h.get("bitbucket"));
    let literal = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let access_token = literal(s.and_then(|b| b.get("accessToken"))).or_else(|| {
        std::env::var("T3CODE_BITBUCKET_ACCESS_TOKEN")
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    });
    if let Some(token) = access_token {
        return Some((format!("Bearer {token}"), None));
    }
    let email = literal(s.and_then(|b| b.get("email"))).or_else(|| {
        std::env::var("T3CODE_BITBUCKET_EMAIL")
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    });
    let api_token = literal(s.and_then(|b| b.get("apiToken"))).or_else(|| {
        std::env::var("T3CODE_BITBUCKET_API_TOKEN")
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    });
    if let (Some(email), Some(token)) = (email, api_token) {
        use base64::Engine;
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("{email}:{token}"));
        return Some((format!("Basic {basic}"), Some(email)));
    }
    None
}

async fn bitbucket_auth(settings: &Value) -> Value {
    let base = std::env::var("T3CODE_BITBUCKET_API_BASE_URL")
        .ok()
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://api.bitbucket.org/2.0".to_string());
    let credential = bitbucket_credential(settings);
    let Some((auth_header, email_hint)) = credential else {
        return provider_auth(
            "unauthenticated",
            None,
            Some("bitbucket.org".into()),
            Some("Add a Bitbucket token in Settings → Source Control, or set the T3CODE_BITBUCKET_* environment variables on the server.".into()),
            None,
        );
    };
    let is_api_token = auth_header.starts_with("Basic ");
    let url = format!("{base}/user");
    let out = probe_cmd(
        "curl",
        &[
            "-fsSL",
            "--max-time",
            "30",
            "-H",
            &format!("Authorization: {auth_header}"),
            &url,
        ],
        35,
    )
    .await;
    let account = out
        .ok()
        .filter(|o| o.code == Some(0))
        .and_then(|o| serde_json::from_str::<Value>(&o.stdout).ok())
        .and_then(|v| {
            ["username", "display_name", "account_id"]
                .iter()
                .filter_map(|k| v.get(*k).and_then(Value::as_str))
                .map(|s| s.trim().to_string())
                .find(|s| !s.is_empty())
        });
    if let Some(account) = account {
        return provider_auth(
            "authenticated",
            Some(account),
            Some("bitbucket.org".into()),
            None,
            None,
        );
    }
    // authFromCredential: configured but unverified → "unknown".
    if is_api_token {
        provider_auth(
            "unknown",
            email_hint,
            Some("bitbucket.org".into()),
            Some("An API token is configured.".into()),
            None,
        )
    } else {
        provider_auth(
            "unknown",
            None,
            Some("bitbucket.org".into()),
            Some("An access token is configured.".into()),
            None,
        )
    }
}

async fn discover_source_control(state: &Arc<AppState>) -> RpcResult {
    let settings = load_settings(state);
    let settings_v = json!(settings);

    let git_v = probe_cmd("git", &["--version"], 10);
    let jj_v = probe_cmd("jj", &["--version"], 10);
    let gh_v = probe_cmd("gh", &["--version"], 10);
    let origin_v = probe_cmd("origin", &["--version"], 10);
    let glab_v = probe_cmd("glab", &["--version"], 10);
    let cafe_v = probe_cmd("cafe", &["--version"], 10);
    let tea_v = probe_cmd("tea", &["--version"], 10);
    let az_v = probe_cmd("az", &["--version"], 20); // probeTimeoutMs 20s
    let (git_v, jj_v, gh_v, origin_v, glab_v, cafe_v, tea_v, az_v) =
        tokio::join!(git_v, jj_v, gh_v, origin_v, glab_v, cafe_v, tea_v, az_v);

    // Auth probes run only for CLIs that exist (per probeSourceControlProvider).
    let gh_auth = async {
        match &gh_v {
            Err(_) => missing_cli_auth(),
            Ok(_) => match probe_cmd("gh", &["auth", "status", "--json", "hosts"], 15).await {
                Ok(out) => parse_gh_auth(&out, &settings_v),
                Err(detail) => provider_auth("unknown", None, None, Some(detail), None),
            },
        }
    };
    let origin_auth = async {
        match &origin_v {
            Err(_) => missing_cli_auth(),
            Ok(_) => match probe_cmd("origin", &["auth", "status", "--json"], 15).await {
                Ok(out) => parse_origin_auth(&out),
                Err(detail) => provider_auth("unknown", None, None, Some(detail), None),
            },
        }
    };
    let glab_auth = async {
        match &glab_v {
            Err(_) => missing_cli_auth(),
            Ok(_) => {
                let first = probe_cmd("glab", &["auth", "status"], 15).await;
                let parsed = first.as_ref().map(parse_glab_auth).unwrap_or_else(|e| {
                    provider_auth("unknown", None, None, Some(e.clone()), None)
                });
                // authRetryArgs: retry once when the first pass signed out.
                if parsed.get("status").and_then(Value::as_str) == Some("unauthenticated") {
                    match probe_cmd("glab", &["auth", "status", "--all"], 15).await {
                        Ok(out) => {
                            let retry = parse_glab_auth(&out);
                            if retry.get("status").and_then(Value::as_str) == Some("authenticated")
                            {
                                retry
                            } else {
                                parsed
                            }
                        }
                        Err(_) => parsed,
                    }
                } else {
                    parsed
                }
            }
        }
    };
    let cafe_auth = async {
        match &cafe_v {
            Err(_) => missing_cli_auth(),
            Ok(_) => match probe_cmd(
                "cafe",
                &[
                    "--host",
                    "https://git.cafe/api",
                    "--no-input",
                    "--no-update-check",
                    "auth",
                    "status",
                    "--json",
                ],
                15,
            )
            .await
            {
                Ok(out) => parse_cafe_auth(&out),
                Err(detail) => provider_auth("unknown", None, None, Some(detail), None),
            },
        }
    };
    let tea_auth = async {
        match &tea_v {
            Err(_) => missing_cli_auth(),
            Ok(_) => match probe_cmd("tea", &["login", "status", "--output", "json"], 15).await {
                Ok(out) => parse_tea_auth(&out),
                Err(detail) => provider_auth("unknown", None, None, Some(detail), None),
            },
        }
    };
    let az_auth = async {
        match &az_v {
            Err(_) => missing_cli_auth(),
            Ok(_) => {
                match probe_cmd(
                    "az",
                    &["account", "show", "--query", "user.name", "-o", "tsv"],
                    20,
                )
                .await
                {
                    Ok(out) => parse_az_auth(&out),
                    Err(detail) => provider_auth("unknown", None, None, Some(detail), None),
                }
            }
        }
    };

    let (gh_auth, origin_auth, glab_auth, cafe_auth, tea_auth, az_auth, bb_auth) = tokio::join!(
        gh_auth,
        origin_auth,
        glab_auth,
        cafe_auth,
        tea_auth,
        az_auth,
        bitbucket_auth(&settings_v)
    );

    // appSignIn is offered only when the server carries a GitHub App client id.
    let gh_app_sign_in = std::env::var("T3CODE_GITHUB_APP_CLIENT_ID")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);

    RpcResult::ok(json!({
        "versionControlSystems": [
            vcs_item("git", "Git", "git", true, &git_v, GIT_INSTALL_HINT),
            vcs_item("jj", "Jujutsu", "jj", false, &jj_v, JJ_INSTALL_HINT),
        ],
        "sourceControlProviders": [
            provider_item("github", "GitHub", &gh_v, "gh", GH_INSTALL_HINT, gh_auth, gh_app_sign_in),
            provider_item("cursor-origin", "Cursor Origin", &origin_v, "origin", ORIGIN_INSTALL_HINT, origin_auth, false),
            provider_item("gitlab", "GitLab", &glab_v, "glab", GLAB_INSTALL_HINT, glab_auth, false),
            provider_item("gitcafe", "GitCafe", &cafe_v, "cafe", CAFE_INSTALL_HINT, cafe_auth, false),
            provider_item("forgejo", "Forgejo / Gitea", &tea_v, "tea", TEA_INSTALL_HINT, tea_auth, false),
            json!({
                "kind": "bitbucket",
                "label": "Bitbucket",
                "status": "available",
                "version": wire_opt_str(None),
                "installHint": BITBUCKET_INSTALL_HINT,
                "detail": wire_opt_str(None),
                "auth": bb_auth,
            }),
            provider_item("azure-devops", "Azure DevOps", &az_v, "az", AZ_INSTALL_HINT, az_auth, false),
        ],
    }))
}

// ---------------------------------------------------------------------------
// server.getNpmRelease — mirrors apps/server/src/cloud/npmRelease.ts
// ---------------------------------------------------------------------------

const NPM_REGISTRY_BASE: &str = "https://registry.npmjs.org";

fn npm_release_error(reason: &str) -> RpcResult {
    RpcResult::err(json!({ "_tag": "ServerNpmReleaseError", "reason": reason }))
}

/// Extract `{version, databaseMigration?}` from one manifest entry, matching
/// `parseReleaseManifest` in npmRelease.ts.
fn parse_npm_manifest(m: &Value) -> Option<Value> {
    // Bare version string.
    if let Some(version) = m.as_str() {
        let version = version.trim();
        if !version.is_empty() {
            return Some(json!({ "version": version }));
        }
        return None;
    }
    let version = m.get("version").and_then(Value::as_str)?.trim().to_string();
    if version.is_empty() {
        return None;
    }
    let mut out = json!({ "version": version });
    if let Some(dm) = m
        .get("t3code")
        .and_then(|t| t.get("databaseMigration"))
        .and_then(Value::as_i64)
    {
        out["databaseMigration"] = json!(dm);
    }
    Some(out)
}

/// `toRelease` parity: decode the manifest and, for non-dist-tag specs,
/// require the returned version to equal the requested spec.
fn npm_release_from_json(spec: &str, body: &str) -> Result<Value, RpcResult> {
    let parsed: Value = serde_json::from_str(body)
        .map_err(|_| npm_release_error("npm returned invalid release metadata."))?;
    // Accept `{...}`, `[manifest]`, or a bare version string.
    let release = if let Some(arr) = parsed.as_array() {
        arr.first().and_then(parse_npm_manifest)
    } else {
        parse_npm_manifest(&parsed)
    };
    let release =
        release.ok_or_else(|| npm_release_error("npm returned invalid release metadata."))?;
    if spec != "latest" && spec != "nightly" && release.get("version") != Some(&json!(spec)) {
        return Err(npm_release_error("npm returned invalid release metadata."));
    }
    Ok(release)
}

async fn get_npm_release(_state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let spec = match payload.get("spec").and_then(Value::as_str) {
        Some(s) => s.trim().to_string(),
        None => return npm_release_error("Invalid release version or track."),
    };
    // ServerNpmReleaseInput: latest | nightly | \d+\.\d+\.\d+(-prerelease)?
    let semver = |s: &str| -> bool {
        let (core, _pre) = match s.split_once('-') {
            Some((c, p))
                if !p.is_empty()
                    && p.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')) =>
            {
                (c, p)
            }
            Some(_) => return false,
            None => (s, ""),
        };
        let mut parts = core.split('.');
        matches!(parts.next(), Some(a) if !a.is_empty() && a.chars().all(|c| c.is_ascii_digit()))
            && matches!(parts.next(), Some(b) if !b.is_empty() && b.chars().all(|c| c.is_ascii_digit()))
            && matches!(parts.next(), Some(c) if !c.is_empty() && c.chars().all(|d| d.is_ascii_digit()))
            && parts.next().is_none()
    };
    if spec.len() > 64 || !(spec == "latest" || spec == "nightly" || semver(&spec)) {
        return npm_release_error("Invalid release version or track.");
    }

    // Primary path: `npm view` against the configured registry (npmRelease.ts
    // honors a private registry this way); the public registry is only the
    // fallback when npm is not installed.
    if probe_cmd("npm", &["--version"], 10)
        .await
        .is_ok_and(|o| o.code == Some(0))
    {
        let out = match probe_cmd(
            "npm",
            &[
                "view",
                "--global",
                "--json",
                &format!("t3@{spec}"),
                "version",
                "t3code",
            ],
            20,
        )
        .await
        {
            Ok(o) => o,
            Err(detail) if detail.contains("timed out") => {
                return npm_release_error("npm returned an incomplete response.");
            }
            Err(_) => return npm_release_error("npm could not complete the lookup."),
        };
        if out.code != Some(0) {
            let e404 = serde_json::from_str::<Value>(&out.stdout)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.get("code"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .as_deref()
                == Some("E404");
            return if e404 {
                RpcResult::ok(Value::Null)
            } else {
                npm_release_error("npm could not read release metadata.")
            };
        }
        return match npm_release_from_json(&spec, &out.stdout) {
            Ok(v) => RpcResult::ok(v),
            Err(e) => e,
        };
    }

    // Fallback: no npm → the public registry, 404 means "no such release".
    let url = format!("{NPM_REGISTRY_BASE}/t3/{spec}");
    let unreachable = || {
        npm_release_error("npm is not installed and the public npm registry could not be reached.")
    };
    let out = match probe_cmd(
        "curl",
        &[
            "-fsSL",
            "--max-time",
            "15",
            "--connect-timeout",
            "10",
            "-w",
            "\n%{http_code}",
            &url,
        ],
        20,
    )
    .await
    {
        Ok(o) => o,
        Err(_) => return unreachable(),
    };
    // `curl -f` still writes the -w http_code on stdout, so a 404 page
    // arrives as an empty body plus "404".
    let stdout = out.stdout.trim_end().to_string();
    let (body, code) = match stdout.rsplit_once('\n') {
        Some((b, c)) => (b.to_string(), c.trim().to_string()),
        None => (stdout.clone(), String::new()),
    };
    if code == "404" || (out.code != Some(0) && out.stderr.contains("404")) {
        return RpcResult::ok(Value::Null);
    }
    if out.code != Some(0) || !(200..300).contains(&code.parse::<u16>().unwrap_or(0)) {
        return unreachable();
    }
    match npm_release_from_json(&spec, &body) {
        Ok(v) => RpcResult::ok(v),
        Err(e) => e,
    }
}

fn self_update_unsupported() -> RpcResult {
    RpcResult::err(json!({
        "_tag": "ServerSelfUpdateError",
        "reason": "the native Rust server cannot self-update; download a new `c67` release and restart it manually",
    }))
}

// ---------------------------------------------------------------------------
// cloud.getRelayClientStatus / cloud.installRelayClient — cloudflared probes
// mirroring packages/shared/src/relayClient.ts (honest install: download+pin)
// ---------------------------------------------------------------------------

const CLOUDFLARED_VERSION: &str = "2026.10.4";
const CLOUDFLARED_MIN_VERSION: &str = "2025.6.1";

/// `cloudflared version` prints `cloudflared version 2026.10.4 (built ...)`.
fn cloudflared_version_out(stdout: &str) -> Option<String> {
    let line = first_line(stdout)?;
    let mut parts = line.split_whitespace();
    while let Some(tok) = parts.next() {
        if tok == "version" {
            return parts.next().map(|s| s.trim_start_matches('v').to_string());
        }
    }
    // Bare `x.y.z` output.
    line.chars()
        .next()
        .filter(|c| c.is_ascii_digit())
        .map(|_| line.trim_start_matches('v').to_string())
}

fn version_tuple(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.split('.');
    Some((
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ))
}

fn version_supported(v: &str) -> bool {
    match (version_tuple(v), version_tuple(CLOUDFLARED_MIN_VERSION)) {
        (Some(have), Some(min)) => have >= min,
        _ => false,
    }
}

fn relay_platform() -> Option<(&'static str, &'static str, &'static str)> {
    // (asset suffix, pack format, contract platform name)
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some(("cloudflared-linux-amd64", "bin", "linux")),
        ("linux", "aarch64") => Some(("cloudflared-linux-arm64", "bin", "linux")),
        ("macos", "x86_64") => Some(("cloudflared-darwin-amd64.tgz", "tgz", "darwin")),
        ("macos", "aarch64") => Some(("cloudflared-darwin-arm64.tgz", "tgz", "darwin")),
        _ => None,
    }
}

fn managed_cloudflared_dir(state: &AppState) -> std::path::PathBuf {
    state.home_dir.join("tools").join("cloudflared")
}

fn relay_install_error(reason: &str, message: &str) -> Value {
    json!({
        "_tag": "RelayClientInstallFailedError",
        "reason": reason,
        "message": message,
    })
}

/// Probe an explicit path, then `cloudflared` on PATH.
async fn probe_cloudflared(path: Option<&str>) -> Result<(String, String), String> {
    let (program, display) = match path {
        Some(p) => (p.to_string(), p.to_string()),
        None => ("cloudflared".to_string(), "cloudflared".to_string()),
    };
    let out = probe_cmd(&program, &["version"], 10).await?;
    if out.code != Some(0) {
        return Err(format!(
            "{display} exited {}: {}",
            out.code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".into()),
            first_line(&out.stderr).unwrap_or_default()
        ));
    }
    match cloudflared_version_out(&out.stdout) {
        Some(v) => Ok((display, v)),
        None => Err(format!("could not parse `{display} version` output")),
    }
}

async fn relay_client_status(state: &Arc<AppState>) -> RpcResult {
    // 1) explicit env override, same precedence as the TS server.
    if let Ok(override_path) = std::env::var("T3CODE_CLOUDFLARED_PATH") {
        let override_path = override_path.trim().to_string();
        if !override_path.is_empty() {
            return match probe_cloudflared(Some(&override_path)).await {
                Ok((display, v)) if version_supported(&v) => RpcResult::ok(json!({
                    "status": "available",
                    "executablePath": display,
                    "source": "override",
                    "version": v,
                })),
                // Broken/too-old override falls through to missing, same as TS
                // (the override is still reported via install errors).
                _ => RpcResult::ok(json!({
                    "status": "missing",
                    "version": CLOUDFLARED_VERSION,
                })),
            };
        }
    }
    // 2) managed install under <home>/tools/cloudflared.
    let managed = managed_cloudflared_dir(state);
    let managed_bin = managed.join(if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    });
    if managed_bin.is_file() {
        if let Ok((display, v)) = probe_cloudflared(Some(&managed_bin.to_string_lossy())).await {
            if version_supported(&v) {
                return RpcResult::ok(json!({
                    "status": "available",
                    "executablePath": display,
                    "source": "managed",
                    "version": v,
                }));
            }
        }
    }
    // 3) PATH lookup.
    if let Ok((_, v)) = probe_cloudflared(None).await {
        if version_supported(&v) {
            // Resolve PATH location for the record.
            let which = probe_cmd("sh", &["-c", "command -v cloudflared"], 5)
                .await
                .ok()
                .and_then(|o| first_line(&o.stdout))
                .unwrap_or_else(|| "cloudflared".to_string());
            return RpcResult::ok(json!({
                "status": "available",
                "executablePath": which,
                "source": "path",
                "version": v,
            }));
        }
    }
    // 4) platform support check → missing vs unsupported.
    match relay_platform() {
        Some((_, _, platform)) => {
            let _ = platform;
            RpcResult::ok(json!({ "status": "missing", "version": CLOUDFLARED_VERSION }))
        }
        None => {
            let platform = match std::env::consts::OS {
                "windows" => "windows",
                "macos" => "darwin",
                _ => "linux",
            };
            let arch = match std::env::consts::ARCH {
                "aarch64" => "arm64",
                "x86_64" => "x64",
                _ => "other",
            };
            RpcResult::ok(json!({
                "status": "unsupported",
                "platform": platform,
                "arch": arch,
                "version": CLOUDFLARED_VERSION,
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// environment.transcribePcm — native extension mirroring the TS server's
// CloudSpeech.transcribe (apps/server/src/speech/CloudSpeech.ts). The TS path
// lives on the environment HTTP API (`POST /api/voice/cloud/:provider/
// transcribe`); this WS RPC gives native clients the same cloud providers.
// ---------------------------------------------------------------------------

const SPEECH_SAMPLE_RATE: u32 = 16_000;
const MAX_SPEECH_SECONDS: u32 = 5 * 60;

fn cloud_speech_error(provider: &str, reason: &str) -> RpcResult {
    RpcResult::err(json!({
        "_tag": "CloudSpeechError",
        "provider": provider,
        "reason": reason,
    }))
}

/// Wrap signed-16-bit LE mono samples in the same RIFF/WAVE header the TS
/// `wave()` writes.
fn wav_wrap_s16le(pcm: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(44 + pcm.len());
    let total = (36 + pcm.len()) as u32;
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SPEECH_SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SPEECH_SAMPLE_RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// Resolve the configured API key for `provider` from settings'
/// `cloudSpeechCredentials`. Only literal strings resolve; secret references
/// need a vault this server does not read, so they report `unreadable-key`.
fn cloud_speech_key(state: &AppState, provider: &str) -> Result<Option<String>, ()> {
    let settings = load_settings(state);
    let creds = settings
        .get("cloudSpeechCredentials")
        .and_then(|c| c.get(provider));
    match creds {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let s = s.trim().to_string();
            Ok((!s.is_empty()).then_some(s))
        }
        // Secret reference object (e.g. {"_tag":"SecretReference",...}) — we
        // cannot resolve it.
        Some(_) => Err(()),
    }
}

async fn transcribe_pcm(state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let provider = payload
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("openai");
    if !matches!(provider, "openai" | "xai" | "openrouter") {
        return client_error("provider must be one of: openai, xai, openrouter").into();
    }
    let pcm_b64 = match payload.get("pcm").and_then(Value::as_str) {
        Some(s) => s,
        None => return client_error("pcm is required").into(),
    };
    use base64::Engine;
    let pcm_bytes = base64::engine::general_purpose::STANDARD
        .decode(pcm_b64.trim())
        .unwrap_or_default();
    let format = payload
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("s16le");
    // The TS wire format is Float32 PCM; this extension accepts either raw
    // s16le (the client-facing default) or f32le samples and wraps both in
    // the same WAV body.
    let s16: Vec<u8> = match format {
        "s16le" => {
            if pcm_bytes.len() % 2 != 0
                || pcm_bytes.is_empty()
                || pcm_bytes.len() > (SPEECH_SAMPLE_RATE * 2 * MAX_SPEECH_SECONDS) as usize
            {
                return client_error(
                    "audio must be non-empty 16 kHz mono s16le PCM of at most five minutes",
                )
                .into();
            }
            pcm_bytes
        }
        "f32le" => {
            if pcm_bytes.len() % 4 != 0
                || pcm_bytes.is_empty()
                || pcm_bytes.len() > (SPEECH_SAMPLE_RATE * 4 * MAX_SPEECH_SECONDS) as usize
            {
                return client_error(
                    "audio must be non-empty 16 kHz mono f32le PCM of at most five minutes",
                )
                .into();
            }
            let mut out = Vec::with_capacity(pcm_bytes.len() / 2);
            for chunk in pcm_bytes.chunks_exact(4) {
                let s = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                if !s.is_finite() || !(-1.0..=1.0).contains(&s) {
                    return client_error("audio samples must be finite and within [-1, 1]").into();
                }
                let scaled = (s * if s < 0.0 { 32768.0 } else { 32767.0 }).round() as i16;
                out.extend_from_slice(&scaled.to_le_bytes());
            }
            out
        }
        _ => return client_error("format must be `s16le` or `f32le`").into(),
    };
    let wav = wav_wrap_s16le(&s16);

    let key = match cloud_speech_key(state, provider) {
        Ok(Some(k)) => k,
        Ok(None) => return cloud_speech_error(provider, "missing-key"),
        Err(()) => return cloud_speech_error(provider, "unreadable-key"),
    };
    let language = payload
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let model_param = payload.get("model").and_then(Value::as_str);

    // Endpoint + body per provider (CloudSpeech.ts endpoints). OpenRouter is
    // JSON; OpenAI and xAI are multipart form uploads.
    let url: String;
    let mut json_body_path: Option<PathBuf> = None;
    match provider {
        "openrouter" => {
            let model = model_param.unwrap_or("").trim();
            if model.is_empty() || !model.contains('/') || model.starts_with("openrouter/") {
                return cloud_speech_error(provider, "model-unavailable");
            }
            let mut body = json!({
                "model": model,
                "input_audio": {
                    "data": base64::engine::general_purpose::STANDARD.encode(&wav),
                    "format": "wav",
                },
                "provider": { "zdr": true },
            });
            if language != "auto" {
                body["language"] = json!(language);
            }
            let path = std::env::temp_dir().join(format!("c67-speech-{}.json", std::process::id()));
            if std::fs::write(&path, serde_json::to_vec(&body).unwrap_or_default()).is_err() {
                return cloud_speech_error(provider, "unavailable");
            }
            json_body_path = Some(path);
            url = "https://openrouter.ai/api/v1/audio/transcriptions".to_string();
        }
        "openai" => url = "https://api.openai.com/v1/audio/transcriptions".to_string(),
        _ => url = "https://api.x.ai/v1/stt".to_string(),
    }

    // Write the WAV to a temp file for curl's multipart upload.
    let wav_path = std::env::temp_dir().join(format!("c67-speech-{}.wav", std::process::id()));
    if std::fs::write(&wav_path, &wav).is_err() {
        if let Some(p) = &json_body_path {
            let _ = std::fs::remove_file(p);
        }
        return cloud_speech_error(provider, "unavailable");
    }
    let cleanup = |path: &std::path::Path| {
        let _ = std::fs::remove_file(path);
    };

    let out = if let Some(body_path) = json_body_path.take() {
        let res = probe_cmd(
            "curl",
            &[
                "-sS",
                "--max-time",
                "120",
                "-X",
                "POST",
                "-H",
                &format!("Authorization: Bearer {key}"),
                "-H",
                "Content-Type: application/json",
                "--data-binary",
                &format!("@{}", body_path.display()),
                "-w",
                "\n%{http_code}",
                &url,
            ],
            130,
        )
        .await;
        cleanup(&body_path);
        res
    } else {
        let mut args: Vec<String> = vec![
            "-sS".into(),
            "--max-time".into(),
            "120".into(),
            "-X".into(),
            "POST".into(),
            "-H".into(),
            format!("Authorization: Bearer {key}"),
        ];
        let model = match provider {
            "openai" => "gpt-transcribe",
            _ => "grok-voice-transcribe-2.0",
        };
        // Speech options mirror DEFAULT_SPEECH_TRANSCRIPTION_OPTIONS and may
        // be overridden by the request payload.
        let opt_bool = |name: &str, default: bool| {
            payload
                .get(name)
                .and_then(Value::as_bool)
                .unwrap_or(default)
        };
        let remove_filler = opt_bool("speechRemoveFillerWords", true);
        let correction_word = payload
            .get("speechCorrectionWord")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let silence_filtering = opt_bool("speechSilenceFiltering", true);
        args.push("-F".into());
        args.push(format!("model={model}"));
        if language != "auto" {
            let field = if provider == "openai" {
                "languages[]"
            } else {
                "language"
            };
            args.push("-F".into());
            args.push(format!("{field}={language}"));
        }
        // Custom vocabulary terms: OpenAI takes `keywords[]`, xAI `keyterm`
        // (and rejects terms containing `<>`, CR, or LF — OpenAI only).
        if let Some(words) = payload.get("speechCustomWords").and_then(Value::as_array) {
            for term in words.iter().filter_map(Value::as_str) {
                if provider == "openai" && term.chars().any(|c| "<>\r\n".contains(c)) {
                    continue;
                }
                args.push("-F".into());
                args.push(format!(
                    "{}={term}",
                    if provider == "openai" {
                        "keywords[]"
                    } else {
                        "keyterm"
                    }
                ));
            }
        }
        if provider == "xai" {
            if language != "auto" {
                args.push("-F".into());
                args.push("format=true".into());
            }
            args.push("-F".into());
            args.push(format!(
                "filler_words={}",
                !remove_filler || correction_word.is_some()
            ));
            if !silence_filtering {
                args.push("-F".into());
                args.push("vad_threshold=0".into());
            }
        }
        // xAI ignores options that follow the file part — keep `file` last.
        args.push("-F".into());
        args.push(format!("file=@{};type=audio/wav", wav_path.display()));
        args.push("-w".into());
        args.push("\n%{http_code}".into());
        args.push(url.clone());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        probe_cmd("curl", &arg_refs, 130).await
    };
    cleanup(&wav_path);

    let out = match out {
        Ok(o) => o,
        Err(_) => return cloud_speech_error(provider, "unavailable"),
    };
    let stdout = out.stdout;
    let (body, code) = match stdout.rfind('\n') {
        Some(i) => (&stdout[..i], stdout[i + 1..].trim().to_string()),
        None => (stdout.as_str(), String::new()),
    };
    let status: u16 = code.parse().unwrap_or(0);
    if !(200..300).contains(&status) {
        let reason = if status == 429 {
            "rate-limit"
        } else if matches!(status, 401 | 402 | 403) {
            "rejected-key"
        } else if status == 404 && provider == "openrouter" {
            "model-unavailable"
        } else {
            "unavailable"
        };
        return cloud_speech_error(provider, reason);
    }
    let parsed: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return cloud_speech_error(provider, "unavailable"),
    };
    match parsed.get("text").and_then(Value::as_str) {
        Some(text) => RpcResult::ok(json!({ "text": text })),
        None => cloud_speech_error(provider, "unavailable"),
    }
}

impl From<Value> for RpcResult {
    fn from(v: Value) -> Self {
        RpcResult::err(v)
    }
}

// ---------------------------------------------------------------------------
// preview.* — session registry. The native server tracks sessions exactly as
// the contract expects and emits PreviewEvents; rendering belongs to the
// desktop client (runtime "desktop"), so no fake browser is launched.
// ---------------------------------------------------------------------------

struct PreviewStore {
    epoch: String,
    revision: u64,
    /// Insertion-ordered (threadId, tabId) → snapshot Value.
    sessions: Vec<Value>,
    profiles: Vec<Value>,
    default_profile_id: Option<String>,
    recording_frame_rate: Option<u64>,
}

fn preview_store() -> &'static std::sync::Mutex<PreviewStore> {
    static STORE: std::sync::OnceLock<std::sync::Mutex<PreviewStore>> = std::sync::OnceLock::new();
    STORE.get_or_init(|| {
        std::sync::Mutex::new(PreviewStore {
            epoch: uuid::Uuid::new_v4().to_string(),
            revision: 0,
            sessions: Vec::new(),
            profiles: Vec::new(),
            default_profile_id: None,
            recording_frame_rate: None,
        })
    })
}

fn preview_events() -> tokio::sync::broadcast::Sender<Value> {
    static TX: std::sync::OnceLock<tokio::sync::broadcast::Sender<Value>> =
        std::sync::OnceLock::new();
    TX.get_or_init(|| tokio::sync::broadcast::channel(256).0)
        .clone()
}

fn preview_emit(
    store: &mut PreviewStore,
    thread_id: &str,
    tab_id: &str,
    ty: &str,
    mut extra: Map<String, Value>,
) {
    store.revision += 1;
    let mut event = Map::new();
    event.insert("threadId".into(), json!(thread_id));
    event.insert("tabId".into(), json!(tab_id));
    event.insert("createdAt".into(), json!(domain::now_iso()));
    event.insert("serverEpoch".into(), json!(store.epoch));
    event.insert("revision".into(), json!(store.revision));
    event.insert("type".into(), json!(ty));
    event.append(&mut extra);
    let _ = preview_events().send(Value::Object(event));
}

fn preview_lookup_error(thread_id: &str, tab_id: &str) -> RpcResult {
    RpcResult::err(json!({
        "_tag": "PreviewSessionLookupError",
        "threadId": thread_id,
        "tabId": tab_id,
    }))
}

fn preview_invalid_url(input: &str, reason: &str, protocol: Option<String>) -> RpcResult {
    RpcResult::err(json!({
        "_tag": "PreviewInvalidUrlError",
        "inputLength": input.chars().count() as u64,
        "reason": reason,
        "protocol": protocol,
        "cause": { "_tag": "Die", "defect": "preview url rejected" },
    }))
}

/// Normalize a preview URL like the TS server: http(s) kept, schemeless
/// `host[:port][/path]` treated as http.
fn normalize_preview_url(raw: &str) -> Result<String, RpcResult> {
    let input = raw.trim();
    if input.is_empty() {
        return Err(preview_invalid_url(raw, "empty", None));
    }
    if let Some((scheme, _)) = input.split_once("://") {
        if scheme == "http" || scheme == "https" {
            return Ok(input.to_string());
        }
        return Err(preview_invalid_url(
            raw,
            "unsupported-protocol",
            Some(format!("{scheme}:")),
        ));
    }
    // Schemeless host[:port][/path] — accept and scheme it.
    let host = input
        .split(|c| matches!(c, '/' | '?' | '#'))
        .next()
        .unwrap_or("")
        .trim();
    let host_ok = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'));
    if !host_ok {
        return Err(preview_invalid_url(raw, "parse", None));
    }
    Ok(format!("http://{input}"))
}

fn preview_find<'a>(
    sessions: &'a mut [Value],
    thread_id: &str,
    tab_id: &str,
) -> Option<&'a mut Value> {
    sessions.iter_mut().find(|s| {
        s.get("threadId").and_then(Value::as_str) == Some(thread_id)
            && s.get("tabId").and_then(Value::as_str) == Some(tab_id)
    })
}

fn preview_open(_state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let thread_id = payload
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if thread_id.is_empty() {
        return RpcResult::err(client_error("threadId is required"));
    }
    let url = match payload.get("url").and_then(Value::as_str) {
        Some(raw) => match normalize_preview_url(raw) {
            Ok(u) => Some(u),
            Err(e) => return e,
        },
        None => None,
    };
    let tab_id = uuid::Uuid::new_v4().to_string();
    let nav_status = match &url {
        Some(u) => json!({ "_tag": "Loading", "url": u, "title": u }),
        None => json!({ "_tag": "Idle" }),
    };
    let mut snapshot = json!({
        "threadId": thread_id,
        "tabId": tab_id,
        "navStatus": nav_status,
        "canGoBack": false,
        "canGoForward": false,
        "updatedAt": domain::now_iso(),
    });
    for key in ["viewport", "profileId", "runtime", "reveal"] {
        if let Some(v) = payload.get(key) {
            snapshot[key] = v.clone();
        }
    }
    let mut store = preview_store().lock().unwrap();
    store.sessions.push(snapshot.clone());
    let mut extra = Map::new();
    extra.insert("snapshot".into(), snapshot.clone());
    preview_emit(&mut store, thread_id, &tab_id, "opened", extra);
    RpcResult::ok(snapshot)
}

fn preview_report_status(_state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let thread_id = payload
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let tab_id = payload.get("tabId").and_then(Value::as_str).unwrap_or("");
    let mut store = preview_store().lock().unwrap();
    let Some(s) = preview_find(&mut store.sessions, thread_id, tab_id) else {
        return preview_lookup_error(thread_id, tab_id);
    };
    if let Some(v) = payload.get("navStatus") {
        s["navStatus"] = v.clone();
    }
    if let Some(v) = payload.get("canGoBack").and_then(Value::as_bool) {
        s["canGoBack"] = json!(v);
    }
    if let Some(v) = payload.get("canGoForward").and_then(Value::as_bool) {
        s["canGoForward"] = json!(v);
    }
    s["updatedAt"] = json!(domain::now_iso());
    let snapshot = s.clone();
    let mut extra = Map::new();
    extra.insert("snapshot".into(), snapshot);
    preview_emit(&mut store, thread_id, tab_id, "navigated", extra);
    RpcResult::ok(Value::Null)
}

fn preview_resize(_state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let thread_id = payload
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let tab_id = payload.get("tabId").and_then(Value::as_str).unwrap_or("");
    let mut store = preview_store().lock().unwrap();
    let Some(s) = preview_find(&mut store.sessions, thread_id, tab_id) else {
        return preview_lookup_error(thread_id, tab_id);
    };
    if let Some(v) = payload.get("viewport") {
        s["viewport"] = v.clone();
    }
    s["updatedAt"] = json!(domain::now_iso());
    let snapshot = s.clone();
    let mut extra = Map::new();
    extra.insert("snapshot".into(), snapshot.clone());
    preview_emit(&mut store, thread_id, tab_id, "resized", extra);
    RpcResult::ok(snapshot)
}

fn preview_adjust(_state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let thread_id = payload
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let tab_id = payload.get("tabId").and_then(Value::as_str).unwrap_or("");
    let mut store = preview_store().lock().unwrap();
    let Some(s) = preview_find(&mut store.sessions, thread_id, tab_id) else {
        return preview_lookup_error(thread_id, tab_id);
    };
    for key in ["colorScheme", "zoomFactor"] {
        if let Some(v) = payload.get(key) {
            s[key] = v.clone();
        }
    }
    s["updatedAt"] = json!(domain::now_iso());
    let snapshot = s.clone();
    let mut request = Map::new();
    if let Some(v) = payload.get("hardReload") {
        request.insert("hardReload".into(), v.clone());
    }
    if let Some(v) = payload.get("clear") {
        request.insert("clear".into(), v.clone());
    }
    let mut extra = Map::new();
    extra.insert("snapshot".into(), snapshot.clone());
    if !request.is_empty() {
        extra.insert("request".into(), Value::Object(request));
    }
    preview_emit(&mut store, thread_id, tab_id, "resized", extra);
    RpcResult::ok(snapshot)
}

fn preview_close(_state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let thread_id = payload
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let tab_id = payload.get("tabId").and_then(Value::as_str);
    let mut store = preview_store().lock().unwrap();
    if let Some(tab_id) = tab_id {
        let pos = store.sessions.iter().position(|s| {
            s.get("threadId").and_then(Value::as_str) == Some(thread_id)
                && s.get("tabId").and_then(Value::as_str) == Some(tab_id)
        });
        let Some(pos) = pos else {
            return preview_lookup_error(thread_id, tab_id);
        };
        store.sessions.remove(pos);
        preview_emit(&mut store, thread_id, tab_id, "closed", Map::new());
    } else {
        let removed: Vec<String> = store
            .sessions
            .iter()
            .filter(|s| s.get("threadId").and_then(Value::as_str) == Some(thread_id))
            .filter_map(|s| s.get("tabId").and_then(Value::as_str).map(str::to_string))
            .collect();
        store
            .sessions
            .retain(|s| s.get("threadId").and_then(Value::as_str) != Some(thread_id));
        for tab in removed {
            preview_emit(&mut store, thread_id, &tab, "closed", Map::new());
        }
    }
    RpcResult::ok(Value::Null)
}

fn preview_list(_state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let thread_id = payload
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let store = preview_store().lock().unwrap();
    let sessions: Vec<Value> = store
        .sessions
        .iter()
        .filter(|s| s.get("threadId").and_then(Value::as_str) == Some(thread_id))
        .cloned()
        .collect();
    RpcResult::ok(json!({
        "sessions": sessions,
        "serverEpoch": store.epoch,
        "revision": store.revision,
    }))
}

fn preview_report_profiles(_state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let mut store = preview_store().lock().unwrap();
    if let Some(profiles) = payload.get("profiles").and_then(Value::as_array) {
        store.profiles = profiles.clone();
    }
    if let Some(id) = payload.get("defaultProfileId").and_then(Value::as_str) {
        store.default_profile_id = Some(id.to_string());
    }
    if let Some(rate) = payload.get("recordingFrameRate").and_then(Value::as_u64) {
        store.recording_frame_rate = Some(rate);
    }
    RpcResult::ok(Value::Null)
}

fn preview_clear_profile(state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let profile_id = payload
        .get("profileId")
        .and_then(Value::as_str)
        .unwrap_or("");
    // Server-runtime browser profiles live under the T3 home; deleting one
    // that does not exist on disk is a no-op success, same as the TS server.
    let dir = state.home_dir.join("browser-profiles").join(profile_id);
    if dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            return RpcResult::err(json!({
                "_tag": "PreviewClearProfileError",
                "profileId": profile_id,
                "cause": { "_tag": "Die", "defect": format!("{e}") },
            }));
        }
    }
    RpcResult::ok(Value::Null)
}

/// `preview.stopLocalServer` — signal a local dev server process. The pid
/// must exist and must not share our process group before a SIGTERM is sent.
fn preview_stop_local_server(_state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let pid = payload.get("pid").and_then(Value::as_i64).unwrap_or(0);
    let port = payload.get("port").and_then(Value::as_u64).unwrap_or(0);
    let err = |reason: &str| {
        RpcResult::err(json!({ "_tag": "PreviewStopLocalServerError", "reason": reason }))
    };
    if pid <= 0 || port == 0 || port >= 65536 {
        return err("not-found");
    }
    if !cfg!(unix) {
        return err("unsupported");
    }
    // The process must exist.
    if !Path::new(&format!("/proc/{pid}")).exists() {
        return err("not-found");
    }
    // Refuse to signal our own process group — stopping a server that shares
    // T3's group would take down agents and terminals with it.
    let stat_pgid = |pid: i64| -> Option<i64> {
        let s = read_proc(&format!("/proc/{pid}/stat"))?;
        let rparen = s.rfind(')')?;
        s[rparen + 1..]
            .split_whitespace()
            .nth(2)
            .and_then(|v| v.parse::<i64>().ok())
    };
    if let (Some(ours), Some(theirs)) = (stat_pgid(std::process::id() as i64), stat_pgid(pid)) {
        if ours == theirs {
            return err("shared-group");
        }
    }
    match std::process::Command::new("kill")
        .arg(pid.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) if s.success() => RpcResult::ok(Value::Null),
        _ => err("signal-failed"),
    }
}

// ---------------------------------------------------------------------------
// device.* — no mobile development hosts exist on this native build. State is
// honest (empty, persisted configure flags) and operations fail with the
// contract's DeviceError variants.
// ---------------------------------------------------------------------------

fn device_settings(state: &AppState) -> (bool, bool, bool) {
    // (enabled, agentAccessEnabled, onboardingCompleted)
    let settings = load_settings(state);
    let d = settings.get("deviceControls").cloned().unwrap_or(json!({}));
    (
        d.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        d.get("agentAccessEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        d.get("onboardingCompleted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    )
}

fn device_service_state(state: &AppState) -> Value {
    let (_enabled, agent_access, onboarding) = device_settings(state);
    json!({
        "hostStatus": "disabled",
        "hostStatuses": {},
        "hosts": [],
        "devices": [],
        "sessions": [],
        "onboardingCompleted": onboarding,
        "agentAccessEnabled": agent_access,
        "hubBasePath": state.home_dir.join("devices").to_string_lossy(),
        "revision": 0,
    })
}

fn device_host_unavailable(host_id: &str, reason: &str) -> RpcResult {
    RpcResult::err(json!({
        "_tag": "DeviceHostUnavailableError",
        "hostId": host_id,
        "reason": reason,
    }))
}

fn device_configure(state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let path = settings_path(state);
    let mut settings = load_settings(state);
    let mut d = settings.get("deviceControls").cloned().unwrap_or(json!({}));
    if !d.is_object() {
        d = json!({});
    }
    for key in ["enabled", "agentAccessEnabled", "onboardingCompleted"] {
        if let Some(v) = payload.get(key).and_then(Value::as_bool) {
            d[key] = json!(v);
        }
    }
    settings.insert("deviceControls".into(), d);
    if let Err(e) = write_json_atomic(&path, &Value::Object(settings)) {
        return internal_error("failed to write settings", e);
    }
    let snapshot = device_service_state(state);
    // Notify subscribers so device.* views stay consistent.
    let _ = device_state_events().send(snapshot.clone());
    RpcResult::ok(snapshot)
}

fn device_state_events() -> tokio::sync::broadcast::Sender<Value> {
    static TX: std::sync::OnceLock<tokio::sync::broadcast::Sender<Value>> =
        std::sync::OnceLock::new();
    TX.get_or_init(|| tokio::sync::broadcast::channel(32).0)
        .clone()
}

fn device_dispatch(state: &Arc<AppState>, tag: &str, payload: &Value) -> RpcResult {
    match tag {
        "device.list" => RpcResult::ok(device_service_state(state)),
        "device.configure" => device_configure(state, payload),
        "device.testHost" => {
            let host_id = payload
                .get("id")
                .or_else(|| payload.get("hostId"))
                .and_then(Value::as_str)
                .unwrap_or("ssh");
            device_host_unavailable(
                host_id,
                "SSH device hosts are not supported by this server build",
            )
        }
        "device.easAccount" => RpcResult::err(json!({
            "_tag": "DeviceEasAccountError",
            "reason": "EAS account lookup is not supported by this server build",
        })),
        "device.simlockIdentity" => RpcResult::err(json!({
            "_tag": "DeviceSimlockIdentityError",
            "reason": "Simlock identities are not supported by this server build",
        })),
        "device.detail" | "device.action" => {
            let host_id = payload
                .get("hostId")
                .and_then(Value::as_str)
                .unwrap_or("local");
            let device_id = payload
                .get("deviceId")
                .and_then(Value::as_str)
                .unwrap_or("");
            RpcResult::err(json!({
                "_tag": "DeviceNotFoundError",
                "hostId": host_id,
                "deviceId": device_id,
            }))
        }
        "device.open" => {
            let host_id = payload
                .get("hostId")
                .and_then(Value::as_str)
                .unwrap_or("local");
            let device_id = payload
                .get("deviceId")
                .and_then(Value::as_str)
                .unwrap_or("");
            RpcResult::err(json!({
                "_tag": "DeviceNotFoundError",
                "hostId": host_id,
                "deviceId": device_id,
            }))
        }
        // No sessions exist on this server; close/shutdown are honest no-ops.
        "device.close" | "device.shutdown" => RpcResult::ok(Value::Null),
        _ => not_supported(tag),
    }
}

// ---------------------------------------------------------------------------
// peerLinks.* — this build stores no links to peer environments.
// ---------------------------------------------------------------------------

fn peer_links_dispatch(_state: &Arc<AppState>, tag: &str, payload: &Value) -> RpcResult {
    match tag {
        "peerLinks.list" => RpcResult::ok(json!({ "links": [] })),
        "peerLinks.link" => RpcResult::err(json!({
            "_tag": "PeerLinkError",
            "reason": "disabled",
            "message": "Linking to peer environments is not supported by this server build.",
        })),
        "peerLinks.unlink" => {
            let _ = payload;
            RpcResult::ok(json!({ "removed": false }))
        }
        "peerLinks.answerRequest" => RpcResult::err(json!({
            "_tag": "PeerLinkRequestError",
            "reason": "not_found",
        })),
        _ => not_supported(tag),
    }
}

// ---------------------------------------------------------------------------
// cloud.installRelayClient — streamed install of the pinned cloudflared
// release, mirroring packages/shared/src/relayClient.ts installCloudflared.
// ---------------------------------------------------------------------------

struct RelayAsset {
    url: &'static str,
    sha256: &'static str,
    archive: &'static str, // "binary" | "tgz"
}

fn cloudflared_asset() -> Option<RelayAsset> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(RelayAsset {
            url: "https://github.com/cloudflare/cloudflared/releases/download/2026.10.4/cloudflared-darwin-arm64.tgz",
            sha256: "68b6bc4080ac1330234db2a9002d07f98b000537a1bd531a687b660d4160fa7f",
            archive: "tgz",
        }),
        ("macos", "x86_64") => Some(RelayAsset {
            url: "https://github.com/cloudflare/cloudflared/releases/download/2026.10.4/cloudflared-darwin-amd64.tgz",
            sha256: "f11fbef4207d715892331917dafd437f032cf4b7137ed64dea3008338c1c613d",
            archive: "tgz",
        }),
        ("linux", "aarch64") => Some(RelayAsset {
            url: "https://github.com/cloudflare/cloudflared/releases/download/2026.10.4/cloudflared-linux-arm64",
            sha256: "2b7203b4fb0ba28a0c1e12f8855ce2c25f76d266a8d5edafac5540b6812acff6",
            archive: "binary",
        }),
        ("linux", "x86_64") => Some(RelayAsset {
            url: "https://github.com/cloudflare/cloudflared/releases/download/2026.10.4/cloudflared-linux-amd64",
            sha256: "ae5e8add7884b0531d66bbca4570d1de491ff6da51e4ebe7eb160cdf241d9e42",
            archive: "binary",
        }),
        ("windows", "x86_64") | ("windows", "aarch64") => Some(RelayAsset {
            url: "https://github.com/cloudflare/cloudflared/releases/download/2026.10.4/cloudflared-windows-amd64.exe",
            sha256: "411b16d40c8d872243efeb52c550053d00af0a8a97fbb32e730fb8aba429eaa8",
            archive: "binary",
        }),
        _ => None,
    }
}

async fn sha256_file(path: &Path) -> Option<String> {
    for (prog, args) in [
        ("sha256sum", vec![path.to_string_lossy().to_string()]),
        (
            "shasum",
            vec![
                "-a".into(),
                "256".into(),
                path.to_string_lossy().to_string(),
            ],
        ),
    ] {
        if let Ok(out) = probe_cmd(
            prog,
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
            30,
        )
        .await
        {
            if out.code == Some(0) {
                if let Some(hex) = out.stdout.split_whitespace().next() {
                    return Some(hex.to_string());
                }
            }
        }
    }
    None
}

/// Drive the install stream. Emits progress chunks, then either a `complete`
/// chunk + stream exit or a RelayClientInstallFailedError failure exit.
async fn install_relay_client_stream(
    state: Arc<AppState>,
    out: OutTx,
    request_id: u64,
    window: Arc<Window>,
) {
    let stage = |s: &str| json!({ "type": "progress", "stage": s });
    let fail = |reason: &str, message: &str| {
        send_json(
            &out,
            &exit_failure(request_id, relay_install_error(reason, message)),
        );
    };

    if !window
        .send_chunk(&out, request_id, vec![stage("checking")])
        .await
    {
        return;
    }
    if window.is_cancelled() {
        return;
    }

    // Policy check first: an override path means the managed install is off.
    if let Ok(p) = std::env::var("T3CODE_CLOUDFLARED_PATH") {
        if !p.trim().is_empty() {
            return fail(
                "managed_by_policy",
                "T3CODE_CLOUDFLARED_PATH is set; the managed relay client cannot be installed while an override is configured.",
            );
        }
    }

    // Already installed at the pinned version → complete immediately.
    let managed_dir = managed_cloudflared_dir(&state);
    let managed_bin = managed_dir.join(if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    });
    if managed_bin.is_file() {
        if let Ok((path, v)) = probe_cloudflared(Some(&managed_bin.to_string_lossy())).await {
            if version_supported(&v) {
                let status = json!({
                    "status": "available",
                    "executablePath": path,
                    "source": "managed",
                    "version": v,
                });
                let _ = window
                    .send_chunk(
                        &out,
                        request_id,
                        vec![json!({ "type": "complete", "status": status })],
                    )
                    .await;
                send_json(
                    &out,
                    &json!({ "_tag": "Exit", "requestId": request_id, "exit": { "_tag": "Success" } }),
                );
                return;
            }
        }
    }

    let Some(asset) = cloudflared_asset() else {
        return fail(
            "unsupported_platform",
            "cloudflared has no pinned release for this platform and architecture.",
        );
    };

    // Simple install lock: a directory we claim atomically.
    let lock_dir = managed_dir.with_extension("lock");
    let _lock_guard = {
        if let Some(parent) = managed_dir.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::create_dir(&lock_dir) {
            Ok(()) => Some(lock_dir.clone()),
            Err(_) => {
                // Stale locks older than ten minutes are reclaimed.
                let stale = std::fs::metadata(&lock_dir)
                    .and_then(|m| m.modified())
                    .map(|t| t.elapsed().unwrap_or_default() > Duration::from_secs(600))
                    .unwrap_or(false);
                if stale && std::fs::remove_dir(&lock_dir).is_ok() {
                    std::fs::create_dir(&lock_dir)
                        .ok()
                        .map(|_| lock_dir.clone())
                } else {
                    None
                }
            }
        }
    };
    let Some(_lock) = _lock_guard else {
        return fail(
            "install_locked",
            "Another relay client installation is already in progress.",
        );
    };
    struct LockGuard(PathBuf);
    impl Drop for LockGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    let _guard = LockGuard(lock_dir.clone());

    if !window
        .send_chunk(&out, request_id, vec![stage("downloading")])
        .await
    {
        return;
    }
    let download_path =
        managed_dir.with_file_name(format!(".cloudflared-download-{}", std::process::id()));
    let dl = probe_cmd(
        "curl",
        &[
            "-fsSL",
            "--max-time",
            "600",
            "--connect-timeout",
            "15",
            "-o",
            &download_path.to_string_lossy(),
            asset.url,
        ],
        620,
    )
    .await;
    match dl {
        Ok(o) if o.code == Some(0) => {}
        _ => {
            let _ = std::fs::remove_file(&download_path);
            return fail(
                "download_failed",
                "Could not download the pinned cloudflared release from GitHub.",
            );
        }
    }

    if !window
        .send_chunk(&out, request_id, vec![stage("verifying")])
        .await
    {
        let _ = std::fs::remove_file(&download_path);
        return;
    }
    match sha256_file(&download_path).await {
        Some(hex) if hex.eq_ignore_ascii_case(asset.sha256) => {}
        Some(_) => {
            let _ = std::fs::remove_file(&download_path);
            return fail(
                "invalid_checksum",
                "Downloaded relay client checksum did not match the pinned release.",
            );
        }
        None => {
            let _ = std::fs::remove_file(&download_path);
            return fail(
                "validation_failed",
                "Could not verify the downloaded relay client checksum.",
            );
        }
    }

    if !window
        .send_chunk(&out, request_id, vec![stage("installing")])
        .await
    {
        let _ = std::fs::remove_file(&download_path);
        return;
    }
    let staged = managed_dir.with_file_name(format!(".cloudflared-staged-{}", std::process::id()));
    let install_result: Result<(), String> = async {
        std::fs::create_dir_all(&managed_dir).map_err(|e| e.to_string())?;
        if asset.archive == "tgz" {
            let out = probe_cmd(
                "tar",
                &[
                    "-xzf",
                    &download_path.to_string_lossy(),
                    "-C",
                    &managed_dir.to_string_lossy(),
                ],
                60,
            )
            .await
            .map_err(|e| e)?;
            if out.code != Some(0) {
                return Err(format!(
                    "tar extract failed: {}",
                    first_line(&out.stderr).unwrap_or_default()
                ));
            }
            // The tgz ships the binary at ./cloudflared.
            let extracted = managed_dir.join("cloudflared");
            std::fs::rename(&extracted, &staged).map_err(|e| e.to_string())?;
        } else {
            std::fs::rename(&download_path, &staged).map_err(|e| e.to_string())?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&staged)
                .map_err(|e| e.to_string())?
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&staged, perms).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    .await;
    let _ = std::fs::remove_file(&download_path);
    if let Err(e) = install_result {
        let _ = std::fs::remove_file(&staged);
        return fail(
            "write_failed",
            &format!("Could not install the relay client: {e}"),
        );
    }

    if !window
        .send_chunk(&out, request_id, vec![stage("validating")])
        .await
    {
        let _ = std::fs::remove_file(&staged);
        return;
    }
    let staged_version = match probe_cloudflared(Some(&staged.to_string_lossy())).await {
        Ok((_, v)) if v == CLOUDFLARED_VERSION => v,
        _ => {
            let _ = std::fs::remove_file(&staged);
            return fail(
                "validation_failed",
                "The installed relay client did not report the pinned version.",
            );
        }
    };

    if !window
        .send_chunk(&out, request_id, vec![stage("activating")])
        .await
    {
        let _ = std::fs::remove_file(&staged);
        return;
    }
    if let Err(e) = std::fs::rename(&staged, &managed_bin) {
        let _ = std::fs::remove_file(&staged);
        return fail(
            "write_failed",
            &format!("Could not activate the relay client: {e}"),
        );
    }
    let status = json!({
        "status": "available",
        "executablePath": managed_bin.to_string_lossy(),
        "source": "managed",
        "version": staged_version,
    });
    let _ = window
        .send_chunk(
            &out,
            request_id,
            vec![json!({ "type": "complete", "status": status })],
        )
        .await;
    send_json(
        &out,
        &json!({ "_tag": "Exit", "requestId": request_id, "exit": { "_tag": "Success" } }),
    );
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn handle(state: Arc<AppState>, tag: &str, payload: Value) -> RpcResult {
    match tag {
        "server.probe" => RpcResult::ok(json!({})),
        "server.getConfig" => RpcResult::ok(server_config(&state)),
        "server.updateSettings" => update_settings(&state, &payload),
        "server.upsertKeybinding" => upsert_keybinding(&state, &payload),
        "server.removeKeybinding" => remove_keybinding(&state, &payload),
        "server.getUsageSummary" => get_usage_summary(&state, &payload),
        // No pricing table on this build — report the honest status; UsagePricing
        // decodes extra-key-free, so nothing else is claimed.
        "server.refreshUsageRates" => RpcResult::ok(json!({
            "status": "unavailable",
            "source": "none",
            "fetchedAt": null,
            "knownModels": 0,
        })),
        "server.getProcessDiagnostics" => process_diagnostics(),
        "server.getHostResources" => host_resources().await,
        "server.getDatabaseStorage" => database_storage(&state),
        "server.getStorageCleanupReport" => {
            let report = read_json_file(&cleanup_report_path(&state)).unwrap_or(Value::Null);
            RpcResult::ok(report)
        }
        "server.runStorageCleanup" => run_storage_cleanup(&state),
        "server.reclaimDatabaseSpace" => reclaim_database_space(&state),
        "server.refreshProviders" => RpcResult::ok(json!({ "providers": provider_catalog() })),
        "server.reportClientActivity" => RpcResult::ok(json!({ "recorded": true })),
        "server.listTextGeneratorModels" => RpcResult::ok(json!({ "models": [] })),
        "server.prepareTriage" => prepare_triage(&state, &payload).await,
        "secrets.answerRequest" => secrets_answer(&state, &payload),
        "guestShare.create" => RpcResult::err(json!({
            "_tag": "GuestShareCreateError",
            "reason": "connect_unavailable",
        })),
        "server.discoverSourceControl" => discover_source_control(&state).await,
        "server.getNpmRelease" => get_npm_release(&state, &payload).await,
        "server.updateServer"
        | "server.updateServerWithProgress"
        | "server.commitDesktopUpdate" => self_update_unsupported(),
        "cloud.getRelayClientStatus" => relay_client_status(&state).await,
        "environment.transcribePcm" => transcribe_pcm(&state, &payload).await,
        "preview.open" => preview_open(&state, &payload),
        "preview.resize" => preview_resize(&state, &payload),
        "preview.adjust" => preview_adjust(&state, &payload),
        "preview.close" => preview_close(&state, &payload),
        "preview.list" => preview_list(&state, &payload),
        "preview.reportProfiles" => preview_report_profiles(&state, &payload),
        "preview.reportStatus" => preview_report_status(&state, &payload),
        "preview.clearProfile" => preview_clear_profile(&state, &payload),
        "preview.stopLocalServer" => preview_stop_local_server(&state, &payload),
        t if t.starts_with("device.") => device_dispatch(&state, t, &payload),
        t if t.starts_with("peerLinks.") => peer_links_dispatch(&state, t, &payload),
        _ => not_supported(tag),
    }
}

// ---------------------------------------------------------------------------
// Subscription streams
// ---------------------------------------------------------------------------

/// Send one snapshot chunk, then hold the stream open until the client
/// cancels (`Interrupt` sets `Window::cancel`).
async fn snapshot_then_hold(
    out: &crate::ws_rpc::OutTx,
    request_id: u64,
    window: Arc<Window>,
    snapshot: Value,
) -> bool {
    if !window.send_chunk(out, request_id, vec![snapshot]).await {
        return true;
    }
    loop {
        if window.is_cancelled() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Claim a subscription stream tag; returns true if this module handled it.
/// `subscribeTerminalMetadata`, `subscribeProjectClones` and
/// `subscribeWorktreeSetup` belong to the terminal/tasks modules — we return
/// false so their owner (which runs before us in `run_stream`) can claim them.
pub async fn handle_subscription(
    state: Arc<AppState>,
    out: crate::ws_rpc::OutTx,
    request_id: u64,
    tag: &str,
    _payload: Value,
    window: Arc<Window>,
) -> bool {
    match tag {
        "cloud.installRelayClient" => {
            install_relay_client_stream(state, out, request_id, window).await;
            true
        }
        "subscribeServerConfig" => {
            snapshot_then_hold(
                &out,
                request_id,
                window,
                json!({ "version": 1, "type": "snapshot", "config": server_config(&state) }),
            )
            .await
        }
        "subscribeDeviceState" => {
            // Snapshot, then publish state updates (e.g. device.configure).
            let mut rx = device_state_events().subscribe();
            if !window
                .send_chunk(&out, request_id, vec![device_service_state(&state)])
                .await
            {
                return true;
            }
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {
                        if window.is_cancelled() {
                            return true;
                        }
                    }
                    msg = rx.recv() => {
                        match msg {
                            Ok(snapshot) => {
                                if !window.send_chunk(&out, request_id, vec![snapshot]).await {
                                    return true;
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return true,
                        }
                    }
                }
            }
        }
        "subscribePreviewEvents" => {
            let mut rx = preview_events().subscribe();
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {
                        if window.is_cancelled() {
                            return true;
                        }
                    }
                    msg = rx.recv() => {
                        match msg {
                            Ok(event) => {
                                if !window.send_chunk(&out, request_id, vec![event]).await {
                                    return true;
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return true,
                        }
                    }
                }
            }
        }
        "subscribeServerLifecycle" => {
            snapshot_then_hold(
                &out,
                request_id,
                window,
                json!({ "type": "snapshot", "status": "running" }),
            )
            .await
        }
        "subscribeDiscoveredLocalServers" => {
            snapshot_then_hold(
                &out,
                request_id,
                window,
                json!({ "type": "snapshot", "servers": [] }),
            )
            .await
        }
        "subscribeResourceTelemetry" => {
            snapshot_then_hold(
                &out,
                request_id,
                window,
                json!({ "type": "snapshot", "samples": [] }),
            )
            .await
        }
        "bedtimeReminder.subscribe" => {
            snapshot_then_hold(&out, request_id, window, json!({ "type": "snapshot" })).await
        }
        "provider.auth.subscribe"
        | "provider.install.subscribe"
        | "provider.chatgpt.handoff.subscribe"
        | "pullRequests.subscribeRefreshes" => {
            snapshot_then_hold(&out, request_id, window, json!({ "type": "snapshot" })).await
        }
        _ => false,
    }
}
