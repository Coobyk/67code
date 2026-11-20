//! `assets.*` + `attachments.*` WS RPCs and the `GET /api/assets/<key>` handler.
//!
//! The TypeScript server (`apps/server/src/assets/*`, `attachmentStore.ts`)
//! stores attachments under `<state>/attachments` and hands out HMAC-signed
//! URLs shaped `/api/assets/<claims>.<sig>/<fileName>`, plus a separate
//! `/api/attachments/upload/<token>` POST route. This server keeps the wire
//! contract but a simpler model:
//!
//! - Blobs live flat under `<home>/assets/`. `index.json` maps an opaque key
//!   (attachment id, `f<hash>` for registered workspace/media files,
//!   `t<hash>` for tool-output images) to `{file|path, mime, name, size,
//!   download}` metadata.
//! - `assets.createUrl` returns `/api/assets/<key>`; fetches are authorized by
//!   the bearer token (`Authorization: Bearer`) instead of a signature, so
//!   `expiresAt` is advisory only.
//! - `attachments.createUploadUrl` mints a `pending-<uuid>[-ext]` id and the
//!   URL it will be fetchable at, but there is no HTTP upload route here:
//!   bytes arrive via `assets.persistChatAttachments`, which honors a
//!   client-supplied `id` (a superset of the TS behavior, which always mints
//!   deterministic ids — the deterministic scheme is preserved so retries
//!   dedupe to the same id).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;

use crate::handlers::RpcResult;
use crate::AppState;

const ASSET_ROUTE_PREFIX: &str = "/api/assets";
/// Same TTL as the TS `ASSET_TOKEN_TTL_MS`.
const ASSET_URL_TTL_MS: i64 = 60 * 60 * 1000;
/// Same TTL as the TS `ATTACHMENT_UPLOAD_URL_TTL_MS`.
const ATTACHMENT_UPLOAD_URL_TTL_MS: i64 = 10 * 60 * 1000;

const PENDING_SEGMENT: &str = "pending";
const MAX_KEY_CHARS: usize = 1024;
const MAX_ATTACHMENTS_PER_CALL: usize = 100; // PROVIDER_SEND_TURN_MAX_ATTACHMENTS
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024; // PROVIDER_SEND_TURN_MAX_IMAGE_BYTES
const MAX_FILE_BYTES: usize = 50 * 1024 * 1024; // PROVIDER_SEND_TURN_MAX_FILE_BYTES
const MAX_DATA_URL_CHARS: usize = 14_000_000; // PROVIDER_SEND_TURN_MAX_IMAGE_DATA_URL_CHARS
const MAX_TOOL_OUTPUT_IMAGES: usize = 8;

/// Serializes index.json read-modify-write cycles.
static INDEX_LOCK: Mutex<()> = Mutex::const_new(());

/// Extensions a stored attachment blob may carry; mirrors the TS
/// `SAFE_IMAGE_FILE_EXTENSIONS` + `.bin` plus common preview file types used
/// when resolving an id with no recorded entry.
const KNOWN_BLOB_EXTENSIONS: &[&str] = &[
    ".avif", ".bmp", ".gif", ".heic", ".heif", ".ico", ".jpeg", ".jpg", ".png", ".svg", ".tiff",
    ".webp", ".bin", ".pdf", ".html", ".txt", ".md", ".json", ".csv", ".rtf", ".zip", ".mp4",
    ".webm", ".mov", ".mp3", ".wav", ".m4a", ".ogg", ".docx", ".ipynb",
];

/// Extensions that may be served inline when `disposition: "inline"` is set
/// (`INLINE_PREVIEW_MIME_TYPES` covers svg/pdf/html; video uses its own mime).
const INLINE_PREVIEW_EXTENSIONS: &[&str] = &["svg", "pdf", "html", "htm"];

// ---------------------------------------------------------------------------
// Small utilities
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn assets_dir(state: &AppState) -> PathBuf {
    state.home_dir.join("assets")
}

fn index_path(state: &AppState) -> PathBuf {
    assets_dir(state).join("index.json")
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&sha256::hash(data))
}

/// Single path-component check: no separators, no traversal, no dotfiles.
fn is_safe_file_component(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= MAX_KEY_CHARS
        && !s.starts_with('.')
        && !s.contains("..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}

/// Attachment ids never contain `.`; the extension lives on the file name.
fn is_safe_attachment_id(id: &str) -> bool {
    is_safe_file_component(id) && !id.contains('.')
}

// ---------------------------------------------------------------------------
// Minimal SHA-256 (deterministic attachment ids, content keys)
// ---------------------------------------------------------------------------

mod sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    pub fn hash(data: &[u8]) -> [u8; 32] {
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let bit_len = (data.len() as u64).wrapping_mul(8);
        let mut msg = data.to_vec();
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_be_bytes());

        for chunk in msg.chunks_exact(64) {
            let mut w = [0u32; 64];
            for (i, word) in w.iter_mut().take(16).enumerate() {
                *word = u32::from_be_bytes([
                    chunk[4 * i],
                    chunk[4 * i + 1],
                    chunk[4 * i + 2],
                    chunk[4 * i + 3],
                ]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }

        let mut out = [0u8; 32];
        for (i, word) in h.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Base64 / data-url decoding (no base64 dep)
// ---------------------------------------------------------------------------

fn base64_value(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Strict-enough decode mirroring Node's `Buffer.from(b64)`: alphabet chars and
/// CR/LF/space/tab only, `=` padding allowed solely at the end (max 2).
fn base64_decode(payload: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(payload.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut nbits: u32 = 0;
    let mut pads: usize = 0;
    for &b in payload.as_bytes() {
        if b == b'=' {
            pads += 1;
            if pads > 2 {
                return None;
            }
            continue;
        }
        if matches!(b, b'\r' | b'\n' | b' ' | b'\t') {
            continue;
        }
        if pads > 0 {
            return None; // alphabet chars after padding
        }
        let v = base64_value(b)?;
        acc = (acc << 6) | v as u32;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((acc >> nbits) as u8);
        }
    }
    if nbits >= 6 {
        return None; // dangling bits that cannot form a byte
    }
    Some(out)
}

/// Port of `parseBase64DataUrl` (`apps/server/src/imageMime.ts`).
fn parse_base64_data_url(data_url: &str) -> Option<(String /*mime*/, String /*base64*/)> {
    let trimmed = data_url.trim();
    if !trimmed[..5.min(trimmed.len())].eq_ignore_ascii_case("data:") || trimmed.len() < 5 {
        return None;
    }
    let comma = trimmed.find(',')?;
    let header = &trimmed[5..comma];
    if header.is_empty() {
        return None;
    }
    let parts: Vec<&str> = header
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 2 || !parts.last()?.eq_ignore_ascii_case("base64") {
        return None;
    }
    let mime = parts[0].to_lowercase();
    let payload = &trimmed[comma + 1..];
    // Gather base64 runs split only by whitespace; anything else is invalid.
    let mut b64 = String::with_capacity(payload.len());
    for &b in payload.as_bytes() {
        if base64_value(b).is_some() || b == b'=' {
            b64.push(b as char);
        } else if !matches!(b, b'\r' | b'\n' | b' ') {
            return None;
        }
    }
    if b64.is_empty() || b64.len() % 4 != 0 {
        return None;
    }
    if let Some(pad) = b64.find('=') {
        if b64.len() - pad > 2 || !b64[pad..].bytes().all(|c| c == b'=') {
            return None;
        }
    }
    Some((mime, b64))
}

// ---------------------------------------------------------------------------
// Attachment id / filename helpers (ports of attachmentStore.ts / imageMime.ts)
// ---------------------------------------------------------------------------

/// `toSafeThreadAttachmentSegment`.
fn safe_thread_segment(thread_id: &str) -> Option<String> {
    let mut out = String::with_capacity(thread_id.len().min(80));
    let mut last_dash = false;
    for c in thread_id.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '_');
    let segment: String = trimmed
        .chars()
        .take(80)
        .collect::<String>()
        .trim_end_matches(|c| c == '-' || c == '_')
        .to_string();
    if segment.is_empty() {
        return None;
    }
    Some(if segment == PENDING_SEGMENT {
        "_pending".to_string()
    } else {
        segment
    })
}

/// `attachmentFileExtension`: `.part` or a non `[a-z0-9]{1,10}` ext → `.bin`.
fn attachment_file_extension(file_name: &str) -> String {
    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default();
    if ext == "part"
        || ext.is_empty()
        || ext.len() > 10
        || !ext.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return ".bin".to_string();
    }
    format!(".{ext}")
}

fn image_extension_for_mime(mime: &str) -> Option<&'static str> {
    Some(match mime.to_lowercase().as_str() {
        "image/avif" => ".avif",
        "image/bmp" => ".bmp",
        "image/gif" => ".gif",
        "image/heic" => ".heic",
        "image/heif" => ".heif",
        "image/jpeg" | "image/jpg" => ".jpg",
        "image/png" => ".png",
        "image/svg+xml" => ".svg",
        "image/tiff" => ".tiff",
        "image/webp" => ".webp",
        _ => return None,
    })
}

const SAFE_IMAGE_EXTENSIONS: &[&str] = &[
    ".avif", ".bmp", ".gif", ".heic", ".heif", ".ico", ".jpeg", ".jpg", ".png", ".svg", ".tiff",
    ".webp",
];

/// `inferImageExtension`.
fn infer_image_extension(mime_type: &str, file_name: &str) -> String {
    if let Some(ext) = image_extension_for_mime(mime_type) {
        return ext.to_string();
    }
    let name_ext = attachment_file_extension(file_name);
    if name_ext != ".bin" && SAFE_IMAGE_EXTENSIONS.contains(&name_ext.as_str()) {
        return name_ext;
    }
    ".bin".to_string()
}

fn is_hex_group(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_uuid_shape(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b[8] == b'-'
        && b[13] == b'-'
        && b[18] == b'-'
        && b[23] == b'-'
        && is_hex_group(&s[0..8], 8)
        && is_hex_group(&s[9..13], 4)
        && is_hex_group(&s[14..18], 4)
        && is_hex_group(&s[19..23], 4)
        && is_hex_group(&s[24..36], 12)
}

/// `ATTACHMENT_ID_PATTERN`: `<segment>-<uuid>[-<ext>]`. Returns
/// `(segment, uuid, ext?)`. Scans '-' positions right-to-left, mirroring the
/// regex's greedy segment + backtracking.
fn parse_attachment_id(id: &str) -> Option<(String, String, Option<String>)> {
    let bytes = id.as_bytes();
    for i in (0..bytes.len()).rev() {
        if bytes[i] != b'-' {
            continue;
        }
        let rest = &id[i + 1..];
        let (uuid, ext) = if rest.len() == 36 && is_uuid_shape(rest) {
            (rest, None)
        } else if rest.len() > 37 && rest.as_bytes()[36] == b'-' {
            let (cand, tail) = rest.split_at(36);
            let e = &tail[1..];
            if !(1..=10).contains(&e.len())
                || !e.bytes().all(|b| b.is_ascii_alphanumeric())
                || !is_uuid_shape(cand)
            {
                continue;
            }
            (cand, Some(e))
        } else {
            continue;
        };
        let segment = &id[..i];
        let seg_ok = !segment.is_empty()
            && !segment.starts_with('-')
            && !segment.ends_with('-')
            && !segment.contains("--")
            && segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if seg_ok {
            return Some((
                segment.to_string(),
                uuid.to_lowercase(),
                ext.map(str::to_lowercase),
            ));
        }
    }
    None
}

/// `createDeterministicAttachmentId` — identical ids to the TS server, so a
/// directory shared with it sees the same names.
fn deterministic_attachment_id(thread_id: &str, stable_key: &str) -> Option<String> {
    let segment = safe_thread_segment(thread_id)?;
    let serialized = serde_json::to_string(&json!([thread_id, stable_key])).ok()?;
    let hash = hex_encode(&sha256::hash(serialized.as_bytes())[..16]);
    let uuid = format!(
        "{}-{}-{}-{}-{}",
        &hash[0..8],
        &hash[8..12],
        &hash[12..16],
        &hash[16..20],
        &hash[20..32]
    );
    Some(format!("{segment}-{uuid}"))
}

fn pending_attachment_id(extension: Option<&str>) -> String {
    let suffix = match extension {
        None => String::new(),
        Some(raw) => {
            let norm = raw.trim_start_matches('.').to_lowercase();
            if !norm.is_empty()
                && norm.len() <= 10
                && norm.bytes().all(|b| b.is_ascii_alphanumeric())
            {
                format!("-{norm}")
            } else {
                "-bin".to_string()
            }
        }
    };
    format!("{PENDING_SEGMENT}-{}{suffix}", uuid::Uuid::new_v4())
}

// ---------------------------------------------------------------------------
// index.json — key → {file | path, mime, name, size, download}
// ---------------------------------------------------------------------------

fn empty_index() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("version".into(), json!(1));
    m.insert("assets".into(), json!({}));
    m
}

async fn load_index(state: &AppState) -> Map<String, Value> {
    match tokio::fs::read(index_path(state)).await {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .filter(|m| m.get("assets").is_some_and(Value::is_object))
            .unwrap_or_else(empty_index),
        Err(_) => empty_index(),
    }
}

async fn save_index(state: &AppState, index: &Map<String, Value>) -> std::io::Result<()> {
    let dir = assets_dir(state);
    tokio::fs::create_dir_all(&dir).await?;
    let tmp = dir.join("index.json.tmp");
    tokio::fs::write(&tmp, serde_json::to_vec(index).unwrap_or_default()).await?;
    tokio::fs::rename(&tmp, index_path(state)).await
}

fn index_entry(index: &Map<String, Value>, key: &str) -> Option<Value> {
    index.get("assets")?.as_object()?.get(key).cloned()
}

fn index_assets_mut<'m>(index: &'m mut Map<String, Value>) -> &'m mut Map<String, Value> {
    if !index.get("assets").is_some_and(Value::is_object) {
        index.insert("assets".into(), json!({}));
    }
    index
        .get_mut("assets")
        .and_then(Value::as_object_mut)
        .expect("assets object just ensured")
}

// ---------------------------------------------------------------------------
// Stored-blob resolution
// ---------------------------------------------------------------------------

/// Resolve an attachment id to `(file_name, absolute_path, index_entry)`.
async fn resolve_attachment_file(
    state: &AppState,
    attachment_id: &str,
) -> Option<(String, PathBuf, Option<Value>)> {
    if !is_safe_attachment_id(attachment_id) {
        return None;
    }
    let dir = assets_dir(state);
    let entry = {
        let _g = INDEX_LOCK.lock().await;
        load_index(state).await
    };
    if let Some(e) = index_entry(&entry, attachment_id) {
        if let Some(file) = e.get("file").and_then(Value::as_str) {
            if is_safe_file_component(file) {
                let path = dir.join(file);
                if path.is_file() {
                    return Some((file.to_string(), path, Some(e)));
                }
            }
        }
    }
    // `<id>-<ext>` ids store `<id>.<ext>`; plain ids probe known extensions.
    if let Some((_, _, Some(ext))) = parse_attachment_id(attachment_id) {
        let file = format!("{attachment_id}.{ext}");
        let path = dir.join(&file);
        if path.is_file() {
            return Some((file, path, None));
        }
    }
    for ext in KNOWN_BLOB_EXTENSIONS {
        let file = format!("{attachment_id}{ext}");
        let path = dir.join(&file);
        if path.is_file() {
            return Some((file, path, None));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// MIME + image dimensions
// ---------------------------------------------------------------------------

fn mime_for_extension(ext: &str) -> Option<&'static str> {
    Some(match ext.to_lowercase().as_str() {
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "css" => "text/css",
        "csv" => "text/csv",
        "gif" => "image/gif",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "htm" | "html" => "text/html",
        "ico" => "image/x-icon",
        "ipynb" | "json" => "application/json",
        "jpeg" | "jpg" => "image/jpeg",
        "js" | "mjs" => "text/javascript",
        "m4a" => "audio/mp4",
        "md" => "text/markdown",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "mp4" | "m4v" => "video/mp4",
        "ogg" => "audio/ogg",
        "otf" => "font/otf",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "rtf" => "application/rtf",
        "svg" => "image/svg+xml",
        "tiff" | "tif" => "image/tiff",
        "ttf" => "font/ttf",
        "txt" => "text/plain",
        "wav" => "audio/wav",
        "webm" => "video/webm",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "xml" => "application/xml",
        "zip" => "application/zip",
        _ => return None,
    })
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default()
}

/// Best-effort width/height for png/gif/jpeg/webp headers; `None` when the
/// format is not recognized (the field is optional on the wire).
fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // PNG: 8-byte signature, then IHDR length+tag, then width/height BE.
    if bytes.len() >= 24 && bytes[..8] == [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a] {
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return (w >= 1 && h >= 1).then_some((w, h));
    }
    // GIF: "GIF8xx" then u16 LE width/height.
    if bytes.len() >= 10 && &bytes[..3] == b"GIF" {
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        return (w >= 1 && h >= 1).then_some((w, h));
    }
    // JPEG: scan markers for a start-of-frame segment.
    if bytes.len() >= 4 && bytes[0] == 0xff && bytes[1] == 0xd8 {
        let mut i = 2usize;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xff {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            if (0xc0..=0xcf).contains(&marker) && marker != 0xc4 && marker != 0xc8 && marker != 0xcc
            {
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                return (w >= 1 && h >= 1).then_some((w, h));
            }
            if marker == 0xd8 || marker == 0xd9 || (0xd0..=0xd7).contains(&marker) {
                i += 2;
                continue;
            }
            let seg_len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            if seg_len < 2 {
                break;
            }
            i += 2 + seg_len;
        }
        return None;
    }
    // WebP: RIFF container, VP8 / VP8L / VP8X variants.
    if bytes.len() >= 30 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        match &bytes[12..16] {
            b"VP8X" => {
                let w = (u32::from(bytes[24])
                    | u32::from(bytes[25]) << 8
                    | u32::from(bytes[26]) << 16)
                    + 1;
                let h = (u32::from(bytes[27])
                    | u32::from(bytes[28]) << 8
                    | u32::from(bytes[29]) << 16)
                    + 1;
                return Some((w, h));
            }
            b"VP8 " if bytes.len() >= 30 => {
                let w = (u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3fff) as u32;
                let h = (u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3fff) as u32;
                return (w >= 1 && h >= 1).then_some((w, h));
            }
            b"VP8L" if bytes.len() >= 25 && bytes[20] == 0x2f => {
                let b0 = u32::from(bytes[21]);
                let b1 = u32::from(bytes[22]);
                let b2 = u32::from(bytes[23]);
                let b3 = u32::from(bytes[24]);
                let w = (b0 | (b1 & 0x3f) << 8) + 1;
                let h = ((b1 >> 6) | (b2 << 2) | ((b3 & 0x0f) << 10)) + 1;
                return (w >= 1 && h >= 1).then_some((w, h));
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// File registration (external files referenced by assets.createUrl)
// ---------------------------------------------------------------------------

/// Canonicalize `path`, require it to be a regular file outside the 67 home,
/// and record it in the index under `f<sha256(path)[..32]>`. Returns the key.
async fn register_external_file(
    state: &AppState,
    path: &Path,
    name: &str,
    mime: Option<String>,
    download: bool,
) -> Option<String> {
    let canonical = tokio::fs::canonicalize(path).await.ok()?;
    if !canonical.is_file() {
        return None;
    }
    // Never serve the server's own state directory.
    let home_canonical = tokio::fs::canonicalize(&state.home_dir)
        .await
        .unwrap_or_else(|_| state.home_dir.clone());
    if canonical.starts_with(&home_canonical) {
        return None;
    }
    let meta = tokio::fs::metadata(&canonical).await.ok()?;
    let mime = mime.or_else(|| mime_for_extension(&extension_of(&canonical)).map(str::to_owned));
    let hash = sha256_hex(canonical.to_string_lossy().as_bytes());
    let key = format!("f{}", &hash[..32]);
    let mut entry = json!({
        "path": canonical.to_string_lossy(),
        "name": name,
        "size": meta.len(),
    });
    if let Some(m) = &mime {
        entry["mime"] = json!(m);
    }
    if download {
        entry["download"] = json!(true);
    }
    let _g = INDEX_LOCK.lock().await;
    let mut index = load_index(state).await;
    index_assets_mut(&mut index).insert(key.clone(), entry);
    save_index(state, &index).await.ok()?;
    Some(key)
}

/// Resolve `requested` under `root` and confirm the canonical result stays
/// inside the canonical root (port of `resolveCanonicalWorkspaceFile`).
async fn canonical_under_root(root: &Path, requested: &Path) -> Option<PathBuf> {
    let joined = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let (canonical_root, canonical_file) =
        tokio::join!(tokio::fs::canonicalize(root), tokio::fs::canonicalize(&joined));
    let (canonical_root, canonical_file) = (canonical_root.ok()?, canonical_file.ok()?);
    if !canonical_file.is_file() || !canonical_file.starts_with(&canonical_root) {
        return None;
    }
    Some(canonical_file)
}

/// The TS `media-file`/`workspace-file` workspace context:
/// `thread.worktreePath ?? project.workspaceRoot`.
async fn workspace_root_for_thread(state: &AppState, thread_id: &str) -> Option<String> {
    let thread = state.store.get_thread(thread_id).await?;
    if let Some(w) = thread
        .get("worktreePath")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        return Some(w.to_string());
    }
    let project_id = thread.get("projectId").and_then(Value::as_str)?;
    let project = state.store.project_shell(project_id).await?;
    project
        .get("workspaceRoot")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}

async fn file_revision(path: &Path) -> Option<String> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Some(format!("{}:{}", meta.len(), mtime_ms))
}

async fn read_image_dimensions(path: &Path) -> Option<(u32, u32)> {
    let mut f = tokio::fs::File::open(path).await.ok()?;
    let mut head = [0u8; 512];
    let n = {
        use tokio::io::AsyncReadExt;
        f.read(&mut head).await.ok()?
    };
    image_dimensions(&head[..n])
}

// ---------------------------------------------------------------------------
// Error helpers — contract `_tag`s so clients can decode failures
// ---------------------------------------------------------------------------

fn err(tag: &str, fields: Value) -> RpcResult {
    let mut e = fields.as_object().cloned().unwrap_or_default();
    e.insert("_tag".into(), json!(tag));
    RpcResult::err(Value::Object(e))
}

fn rpc_client_error(message: impl Into<String>) -> RpcResult {
    err("RpcClientError", json!({ "message": message.into() }))
}

fn persist_error(message: impl Into<String>) -> RpcResult {
    err("PersistChatAttachmentsError", json!({ "message": message.into() }))
}

fn asset_not_found(resource: &Value) -> RpcResult {
    err("AssetWorkspaceAssetNotFoundError", json!({ "resource": resource }))
}

// ---------------------------------------------------------------------------
// assets.persistChatAttachments
// ---------------------------------------------------------------------------

async fn persist_chat_attachments(state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let thread_id = payload.get("threadId").and_then(Value::as_str).unwrap_or("");
    let message_id = payload
        .get("messageId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let attachments = payload
        .get("attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if thread_id.trim().is_empty() || message_id.trim().is_empty() {
        return persist_error("threadId and messageId are required.");
    }
    if attachments.len() > MAX_ATTACHMENTS_PER_CALL {
        return persist_error(format!(
            "You can attach up to {MAX_ATTACHMENTS_PER_CALL} files per message or question response."
        ));
    }
    let dir = assets_dir(state);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return persist_error(format!("Could not create assets directory: {e}"));
    }

    let mut persisted = Vec::with_capacity(attachments.len());
    for (index, attachment) in attachments.iter().enumerate() {
        let name = attachment.get("name").and_then(Value::as_str).unwrap_or("");
        let mime = attachment
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("");
        let declared_size = attachment
            .get("sizeBytes")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let data_url = attachment
            .get("dataUrl")
            .and_then(Value::as_str)
            .unwrap_or("");
        if attachment.get("type").and_then(Value::as_str) != Some("image")
            || name.is_empty()
            || !mime.to_lowercase().starts_with("image/")
            || data_url.len() > MAX_DATA_URL_CHARS
        {
            return persist_error(format!("Attachment {name} has an invalid image payload."));
        }
        let Some((parsed_mime, b64)) = parse_base64_data_url(data_url) else {
            return persist_error(format!("Attachment {name} has an invalid image payload."));
        };
        if parsed_mime != mime.to_lowercase() {
            return persist_error(format!("Attachment {name} has an invalid image payload."));
        }
        let Some(bytes) = base64_decode(&b64) else {
            return persist_error(format!("Attachment {name} is not valid base64."));
        };
        if bytes.len() != declared_size || bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return persist_error(format!(
                "Attachment {name} size does not match its payload."
            ));
        }

        // Honor a well-formed client id (the pending-upload flow substitutes
        // this RPC for the TS server's HTTP upload route); otherwise use the
        // same deterministic id the TS server derives.
        let stable_key = format!("{message_id}:{index}:{}", sha256_hex(&bytes));
        let provided = attachment.get("id").and_then(Value::as_str).unwrap_or("");
        let id = if is_safe_attachment_id(provided) && parse_attachment_id(provided).is_some() {
            provided.to_string()
        } else {
            match deterministic_attachment_id(thread_id, &stable_key) {
                Some(id) => id,
                None => {
                    return persist_error("Could not allocate an attachment identifier.");
                }
            }
        };

        let ext = infer_image_extension(mime, name);
        let file_name = format!("{id}{ext}");
        if let Err(e) = tokio::fs::write(dir.join(&file_name), &bytes).await {
            return persist_error(format!("Could not persist attachment {name}: {e}"));
        }
        {
            let _g = INDEX_LOCK.lock().await;
            let mut index_map = load_index(state).await;
            index_assets_mut(&mut index_map).insert(
                id.clone(),
                json!({
                    "file": file_name,
                    "mime": mime,
                    "name": name,
                    "size": bytes.len(),
                }),
            );
            let _ = save_index(state, &index_map).await;
        }
        persisted.push(json!({
            "type": "image",
            "id": id,
            "name": name,
            "mimeType": mime,
            "sizeBytes": bytes.len(),
        }));
    }
    RpcResult::ok(json!({ "attachments": persisted }))
}

// ---------------------------------------------------------------------------
// assets.createUrl
// ---------------------------------------------------------------------------

fn create_url_result(key: &str, expires_at: i64) -> Value {
    json!({
        "relativeUrl": format!("{ASSET_ROUTE_PREFIX}/{key}"),
        "expiresAt": expires_at,
    })
}

async fn create_url_for_attachment(state: &Arc<AppState>, resource: &Value) -> RpcResult {
    let attachment_id = resource
        .get("attachmentId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let Some((file_name, path, entry)) = resolve_attachment_file(state, attachment_id).await else {
        return err("AssetAttachmentNotFoundError", json!({ "resource": resource }));
    };

    let ext = extension_of(Path::new(&file_name));
    let parsed_ext = parse_attachment_id(attachment_id).and_then(|(_, _, e)| e);
    let is_generic_file = parsed_ext.is_some();
    let mime_hint = resource.get("mimeType").and_then(Value::as_str).unwrap_or("");
    let is_video = mime_hint
        .split(';')
        .next()
        .map(str::trim)
        .is_some_and(|m| m.to_lowercase().starts_with("video/"));
    let wants_inline = resource.get("disposition").and_then(Value::as_str) == Some("inline")
        && parsed_ext
            .as_deref()
            .is_some_and(|e| INLINE_PREVIEW_EXTENSIONS.contains(&e));

    // Update the index entry so the fetcher gets mime + download intent.
    let index_mime = entry
        .as_ref()
        .and_then(|e| e.get("mime").and_then(Value::as_str))
        .map(str::to_owned);
    let mime = if wants_inline {
        mime_for_extension(&ext).map(str::to_owned)
    } else {
        index_mime
            .or_else(|| (!mime_hint.is_empty()).then(|| mime_hint.to_string()))
            .or_else(|| mime_for_extension(&ext).map(str::to_owned))
    };
    let download = is_generic_file && !is_video && !wants_inline;
    {
        let _g = INDEX_LOCK.lock().await;
        let mut index = load_index(state).await;
        let mut e = entry.unwrap_or_else(|| json!({ "file": file_name }));
        if let Some(m) = &mime {
            e["mime"] = json!(m);
        }
        e["download"] = json!(download);
        if let Some(name) = resource.get("fileName").and_then(Value::as_str) {
            e["name"] = json!(name);
        }
        index_assets_mut(&mut index).insert(attachment_id.to_string(), e);
        let _ = save_index(state, &index).await;
    }

    let mut result = create_url_result(attachment_id, now_ms() + ASSET_URL_TTL_MS);
    if !is_generic_file {
        if let Some((width, height)) = read_image_dimensions(&path).await {
            result["imageDimensions"] = json!({ "width": width, "height": height });
        }
    }
    if let Some(rev) = file_revision(&path).await {
        result["fileRevision"] = json!(rev);
    }
    RpcResult::ok(result)
}

async fn create_url_for_file(
    state: &Arc<AppState>,
    resource: &Value,
    resolved: PathBuf,
    download: bool,
) -> RpcResult {
    let Some(canonical) = tokio::fs::canonicalize(&resolved).await.ok() else {
        return asset_not_found(resource);
    };
    if !canonical.is_file() {
        return asset_not_found(resource);
    }
    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let Some(key) = register_external_file(state, &canonical, &name, None, download).await else {
        return asset_not_found(resource);
    };
    let mut result = create_url_result(&key, now_ms() + ASSET_URL_TTL_MS);
    result["sourcePath"] = json!(canonical.to_string_lossy());
    if let Some((width, height)) = read_image_dimensions(&canonical).await {
        result["imageDimensions"] = json!({ "width": width, "height": height });
    }
    if let Some(rev) = file_revision(&canonical).await {
        result["fileRevision"] = json!(rev);
    }
    RpcResult::ok(result)
}

async fn create_url(state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let resource = payload.get("resource").cloned().unwrap_or(json!({}));
    let kind = resource.get("_tag").and_then(Value::as_str).unwrap_or("");
    match kind {
        "attachment" => create_url_for_attachment(state, &resource).await,
        "media-file" => {
            let path_str = resource.get("path").and_then(Value::as_str).unwrap_or("");
            if path_str.is_empty() {
                return asset_not_found(&resource);
            }
            let requested = PathBuf::from(path_str);
            let resolved = if requested.is_absolute() {
                requested
            } else {
                let thread_id = resource.get("threadId").and_then(Value::as_str).unwrap_or("");
                let Some(root) = workspace_root_for_thread(state, thread_id).await else {
                    return err(
                        "AssetWorkspaceContextNotFoundError",
                        json!({ "resource": resource }),
                    );
                };
                Path::new(&root).join(&requested)
            };
            let download =
                resource.get("disposition").and_then(Value::as_str) == Some("attachment");
            create_url_for_file(state, &resource, resolved, download).await
        }
        "workspace-file" => {
            let thread_id = resource.get("threadId").and_then(Value::as_str).unwrap_or("");
            let path_str = resource.get("path").and_then(Value::as_str).unwrap_or("");
            let Some(root) = workspace_root_for_thread(state, thread_id).await else {
                return err(
                    "AssetWorkspaceContextNotFoundError",
                    json!({ "resource": resource }),
                );
            };
            let Some(canonical) =
                canonical_under_root(Path::new(&root), Path::new(path_str)).await
            else {
                return asset_not_found(&resource);
            };
            create_url_for_file(state, &resource, canonical, false).await
        }
        "draft-workspace-file" => {
            let cwd = resource.get("cwd").and_then(Value::as_str).unwrap_or("");
            let path_str = resource.get("path").and_then(Value::as_str).unwrap_or("");
            let requested = PathBuf::from(path_str);
            if requested.is_absolute() {
                return create_url_for_file(state, &resource, requested, false).await;
            }
            if cwd.is_empty() {
                return err(
                    "AssetWorkspaceContextNotFoundError",
                    json!({ "resource": resource }),
                );
            }
            let Some(canonical) =
                canonical_under_root(Path::new(cwd), &requested).await
            else {
                return asset_not_found(&resource);
            };
            create_url_for_file(state, &resource, canonical, false).await
        }
        "project-favicon" => {
            let cwd = resource.get("cwd").and_then(Value::as_str).unwrap_or("");
            let path_str = resource.get("path").and_then(Value::as_str).unwrap_or("");
            if cwd.is_empty() || path_str.is_empty() {
                return err(
                    "AssetProjectFaviconNotFoundError",
                    json!({ "resource": resource }),
                );
            }
            let requested = PathBuf::from(path_str);
            let resolved = if requested.is_absolute() {
                requested
            } else {
                Path::new(cwd).join(&requested)
            };
            let Some(canonical) = tokio::fs::canonicalize(&resolved).await.ok() else {
                return err(
                    "AssetProjectFaviconNotFoundError",
                    json!({ "resource": resource }),
                );
            };
            create_url_for_file(state, &resource, canonical, false).await
        }
        "tool-output-image" => create_url_for_tool_image(state, &resource).await,
        // `native-app-icon` needs platform icon resolution and `github-media`
        // needs a credentialed fetch; neither is implemented on this server.
        "native-app-icon" | "github-media" => asset_not_found(&resource),
        _ => rpc_client_error(format!(
            "assets.createUrl: missing or unsupported resource kind {kind:?}"
        )),
    }
}

async fn create_url_for_tool_image(state: &Arc<AppState>, resource: &Value) -> RpcResult {
    let thread_id = resource.get("threadId").and_then(Value::as_str).unwrap_or("");
    let item_id = resource.get("itemId").and_then(Value::as_str).unwrap_or("");
    let index = resource.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    let Some(item) = state.store.get_turn_item(thread_id, item_id).await else {
        return asset_not_found(resource);
    };
    if item.get("type").and_then(Value::as_str) != Some("dynamic_tool") {
        return asset_not_found(resource);
    }
    let output = item.get("output").cloned().unwrap_or(Value::Null);

    // `outputBlocks`: array → itself; `{content: [...]}` → content; else [v].
    let blocks: Vec<&Value> = match &output {
        Value::Array(items) => items.iter().collect(),
        v @ Value::Object(_) => v
            .get("content")
            .and_then(Value::as_array)
            .map(|c| c.iter().collect())
            .unwrap_or_else(|| vec![v]),
        v => vec![v],
    };
    let mut images: Vec<(String, String)> = Vec::new(); // (mime, base64)
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("image") {
            continue;
        }
        let source = block.get("source").filter(|s| s.is_object());
        if let Some(s) = source {
            if s.get("type").and_then(Value::as_str) != Some("base64") {
                continue;
            }
        }
        let mime = match source {
            Some(s) => s.get("media_type").and_then(Value::as_str),
            None => block.get("mimeType").and_then(Value::as_str),
        };
        let data = match source {
            Some(s) => s.get("data").and_then(Value::as_str),
            None => block.get("data").and_then(Value::as_str),
        };
        let (Some(mime), Some(data)) = (mime, data) else { continue };
        if !matches!(
            mime.to_lowercase().as_str(),
            "image/gif" | "image/jpeg" | "image/png" | "image/webp"
        ) {
            continue;
        }
        images.push((mime.to_lowercase(), data.to_string()));
        if images.len() >= MAX_TOOL_OUTPUT_IMAGES {
            break;
        }
    }
    let Some((mime, b64)) = images.get(index) else {
        return asset_not_found(resource);
    };
    let Some(bytes) = base64_decode(b64) else {
        return asset_not_found(resource);
    };
    let ext = image_extension_for_mime(mime).unwrap_or(".bin");
    let key = format!("t{}", &sha256_hex(&bytes)[..32]);
    let file_name = format!("{key}{ext}");
    let dir = assets_dir(state);
    if tokio::fs::create_dir_all(&dir).await.is_err()
        || tokio::fs::write(dir.join(&file_name), &bytes).await.is_err()
    {
        return asset_not_found(resource);
    }
    {
        let _g = INDEX_LOCK.lock().await;
        let mut idx = load_index(state).await;
        index_assets_mut(&mut idx).insert(
            key.clone(),
            json!({
                "file": file_name,
                "mime": mime,
                "name": format!("image-{}{}", index + 1, ext),
                "size": bytes.len(),
            }),
        );
        let _ = save_index(state, &idx).await;
    }
    let mut result = create_url_result(&key, now_ms() + ASSET_URL_TTL_MS);
    if let Some((width, height)) = image_dimensions(&bytes) {
        result["imageDimensions"] = json!({ "width": width, "height": height });
    }
    result["fileRevision"] = json!(sha256_hex(&bytes));
    RpcResult::ok(result)
}

// ---------------------------------------------------------------------------
// attachments.createUploadUrl / attachments.delete
// ---------------------------------------------------------------------------

async fn create_upload_url(_state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let kind = payload.get("type").and_then(Value::as_str).unwrap_or("image");
    let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
    let mime = payload
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("");
    let size = payload.get("sizeBytes").and_then(Value::as_u64).unwrap_or(0);
    if name.trim().is_empty() || mime.trim().is_empty() || size == 0 {
        return rpc_client_error(
            "attachments.createUploadUrl: name, mimeType and sizeBytes are required",
        );
    }
    let max = if kind == "file" {
        MAX_FILE_BYTES
    } else {
        MAX_IMAGE_BYTES
    };
    if size as usize > max {
        return rpc_client_error(format!(
            "attachments.createUploadUrl: {size} bytes exceeds the {max} byte limit"
        ));
    }
    // `isSvgAttachment`: file uploads of svg keep a .svg name.
    let mime_base = mime.split(';').next().map(str::trim).unwrap_or("");
    let mut name = name.to_string();
    if kind == "file"
        && (mime_base.eq_ignore_ascii_case("image/svg+xml")
            || ((mime_base.is_empty()
                || matches!(
                    mime_base.to_lowercase().as_str(),
                    "application/octet-stream"
                        | "binary/octet-stream"
                        | "application/unknown"
                        | "video/mp2t"
                        | "video/vnd.dlna.mpeg-tts"
                )) && name.to_lowercase().ends_with(".svg")))
        && !name.to_lowercase().ends_with(".svg")
    {
        name.push_str(".svg");
    }
    let ext_suffix = if kind == "file" {
        Some(attachment_file_extension(&name))
    } else {
        None
    };
    let attachment_id = pending_attachment_id(ext_suffix.as_deref());
    RpcResult::ok(json!({
        "attachmentId": attachment_id,
        "relativeUrl": format!("{ASSET_ROUTE_PREFIX}/{attachment_id}"),
        "expiresAt": now_ms() + ATTACHMENT_UPLOAD_URL_TTL_MS,
    }))
}

async fn delete_attachment(state: &Arc<AppState>, payload: &Value) -> RpcResult {
    let attachment_id = payload
        .get("attachmentId")
        .and_then(Value::as_str)
        .unwrap_or("");
    // TS parity: only pending uploads are deletable through this RPC.
    if parse_attachment_id(attachment_id).map(|(seg, _, _)| seg) != Some(PENDING_SEGMENT.into()) {
        return RpcResult::ok(Value::Null);
    }
    if let Some((_, path, _)) = resolve_attachment_file(state, attachment_id).await {
        let _ = tokio::fs::remove_file(&path).await;
        let _ = tokio::fs::remove_file(format!("{}.txt", path.display())).await;
    }
    let _g = INDEX_LOCK.lock().await;
    let mut index = load_index(state).await;
    index_assets_mut(&mut index).remove(attachment_id);
    let _ = save_index(state, &index).await;
    RpcResult::ok(Value::Null)
}

// ---------------------------------------------------------------------------
// RPC dispatch
// ---------------------------------------------------------------------------

pub async fn handle(state: Arc<AppState>, tag: &str, payload: Value) -> RpcResult {
    match tag {
        "assets.persistChatAttachments" => persist_chat_attachments(&state, &payload).await,
        "assets.createUrl" => create_url(&state, &payload).await,
        "attachments.createUploadUrl" => create_upload_url(&state, &payload).await,
        "attachments.delete" => delete_attachment(&state, &payload).await,
        other => rpc_client_error(format!("not implemented: {other}")),
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
// GET /api/assets/<key>
// ---------------------------------------------------------------------------

fn asset_response(status: StatusCode, mime: &str, bytes: Vec<u8>, download_name: Option<&str>) -> Response {
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, bytes.len())
        .header(header::CACHE_CONTROL, "private, max-age=3600")
        .header("x-content-type-options", "nosniff");
    if let Some(name) = download_name {
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        builder = builder
            .header(
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{safe}\""),
            )
            .header(header::CONTENT_SECURITY_POLICY, "default-src 'none'; sandbox");
    }
    builder.body(Body::from(bytes)).unwrap_or_else(|_| {
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })
}

fn not_found() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::NOT_FOUND.into_response())
}

/// GET /api/assets/<key> — serve a persisted attachment/asset blob.
///
/// `key` is the `{*key}` wildcard (everything after `/api/assets/`). Keys are
/// single safe path components: attachment ids resolve to `<id><ext>` blobs or
/// index entries; `f<hash>`/`t<hash>` keys are index records pointing at an
/// external file or stored blob.
pub async fn serve_asset(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(key): AxumPath<String>,
) -> Response {
    // Bearer auth, mirroring `auth::authorized`: open when no dev token is set.
    if let Some(expected) = &state.auth_token {
        let credential = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        let ok = credential
            .map(|c| c == expected.as_str() || state.auth_access.pairing_token_valid(c))
            .unwrap_or(false);
        if !ok {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }

    let key = key.trim_start_matches('/').to_string();
    if !is_safe_file_component(&key) || key == "index.json" {
        return not_found();
    }

    let dir = assets_dir(&state);
    let index = {
        let _g = INDEX_LOCK.lock().await;
        load_index(&state).await
    };
    let entry = index_entry(&index, &key);

    // Resolve the key to a file: an index `path` record (canonical external
    // file), an index `file` record or the literal blob name, then the id →
    // `<id><ext>` probe used for attachment blobs.
    let resolved: Option<(PathBuf, Option<Value>)> = if let Some(e) = &entry {
        if let Some(raw) = e.get("path").and_then(Value::as_str) {
            match tokio::fs::canonicalize(raw).await {
                // Refuse symlink swaps to a different file after registration.
                Ok(c) if c.is_file() && c == Path::new(raw) => Some((c, entry.clone())),
                _ => None,
            }
        } else {
            match e.get("file").and_then(Value::as_str) {
                Some(f) if is_safe_file_component(f) => {
                    let p = dir.join(f);
                    p.is_file().then_some((p, entry.clone()))
                }
                _ => None,
            }
        }
    } else {
        None
    };
    let (path, entry) = match resolved {
        Some(v) => v,
        None => {
            let direct = dir.join(&key);
            if direct.is_file() {
                (direct, None)
            } else {
                match resolve_attachment_file(&state, &key).await {
                    Some((_, p, e)) => (p, e),
                    None => return not_found(),
                }
            }
        }
    };

    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(_) => return not_found(),
    };

    let entry_mime = entry
        .as_ref()
        .and_then(|e| e.get("mime").and_then(Value::as_str))
        .map(str::to_owned);
    let mime = entry_mime
        .or_else(|| mime_for_extension(&extension_of(&path)).map(str::to_owned))
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let download = entry
        .as_ref()
        .and_then(|e| e.get("download").and_then(Value::as_bool))
        .unwrap_or(false);
    let name = entry
        .as_ref()
        .and_then(|e| e.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "file".to_string());

    // Without the TS CSP injection machinery, html/svg are served as downloads
    // rather than executable documents on the API origin.
    if download || mime == "text/html" || mime == "image/svg+xml" {
        let shown = if download && mime != "application/octet-stream" {
            mime.as_str()
        } else {
            "application/octet-stream"
        };
        return asset_response(StatusCode::OK, shown, bytes, Some(&name));
    }
    asset_response(StatusCode::OK, &mime, bytes, None)
}
