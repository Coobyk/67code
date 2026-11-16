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
use crate::ws_rpc::Window;
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
    let model = |slug: &str, name: &str| {
        json!({ "slug": slug, "name": name, "isCustom": false, "capabilities": null })
    };
    json!([
        provider("devin", "devin", "Devin", json!([
            model("swe-2-high", "SWE-2 High"),
        ])),
        provider("devReplay", "devReplay", "Replay (dev)", json!([
            model("showcase", "Showcase"),
        ])),
        provider("codex", "codex", "Codex", json!([
            model("o4-mini", "o4-mini"),
            model("gpt-5", "GPT-5"),
        ])),
    ])
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
        "environment": { "kind": "local" },
        "auth": { "kind": "none" },
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
    if when.as_deref().is_some_and(|w| w.is_empty() || w.len() > 256) {
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
    (
        entries.into_iter().filter_map(|(_, r)| r).collect(),
        issues,
    )
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
    let elapsed_ms = ((uptime_secs - (starttime as f64 / CLOCK_TICKS as f64)) * 1000.0).max(0.0) as u64;
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
        "pgid": { "_tag": "Some", "value": pgid },
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
        "error": { "_tag": "None" },
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
        num_field(usage, &["uncachedInputTokens", "inputTokens", "input", "promptTokens"]),
        num_field(usage, &["cachedInputTokens", "cacheReadInputTokens", "cacheReadTokens"]),
        num_field(
            usage,
            &["cacheCreationTokens", "cacheCreationInputTokens", "cacheWriteTokens"],
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
    let since_day = payload.get("sinceDay").and_then(Value::as_str).unwrap_or("");
    let until_day = payload.get("untilDay").and_then(Value::as_str).unwrap_or("9999-12-31");
    let time_zone = payload
        .get("timeZone")
        .and_then(Value::as_str)
        .unwrap_or("UTC");
    let hourly = payload.get("resolution").and_then(Value::as_str) == Some("hour");

    // key: (day, hourStart, provider, model)
    let mut buckets: BTreeMap<(String, Option<String>, String, String), UsageAgg> = BTreeMap::new();
    let mut record_row = |timestamp: &str, provider: &str, model: &str, thread_id: &str, t: [u64; 5]| {
        let day = timestamp.get(..10).unwrap_or("").to_string();
        if day.is_empty()
            || (!since_day.is_empty() && day.as_str() < since_day)
            || day.as_str() > until_day
        {
            return;
        }
        let hour_start = if hourly {
            timestamp
                .get(..13)
                .map(|h| format!("{h}:00"))
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

    let mut out = json!({
        "contractVersion": USAGE_CONTRACT_VERSION,
        "readAt": domain::now_iso(),
        "timeZone": time_zone,
        "sinceDay": since_day,
        "untilDay": if until_day == "9999-12-31" { Value::from(since_day) } else { Value::from(until_day) },
        "buckets": bucket_values,
        "sources": [],
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
    let label = client.get("label").and_then(Value::as_str).unwrap_or("an unknown client");
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
    let session_dir = state
        .home_dir
        .join("userdata")
        .join("triage")
        .join(&stamp);
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
        "server.refreshUsageRates" => RpcResult::ok(json!({
            "status": "unavailable",
            "source": "none",
            "fetchedAt": null,
            "knownModels": 0,
            "refreshed": true,
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
    payload: Value,
    window: Arc<Window>,
) -> bool {
    let _ = (state, payload);
    match tag {
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
        | "pullRequests.subscribeRefreshes"
        | "subscribePreviewEvents"
        | "subscribeDeviceState" => {
            snapshot_then_hold(&out, request_id, window, json!({ "type": "snapshot" })).await
        }
        _ => false,
    }
}
