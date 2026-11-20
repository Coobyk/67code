//! File explorer + preview pane (right-panel Files tab).
//!
//! Mirrors apps/web `FileBrowserPanel` + `FilePreviewPanel` at panel scale:
//! lazy one-level `projects.listEntries {cwd, directoryPath}` directory loads
//! on expand, `projects.readFile` for text/markdown preview, and
//! `assets.createUrl` (`workspace-file` resource) + `GET /api/assets/<key>`
//! for image previews. Files the inline preview can't render (pdf, office
//! docs, notebooks, fonts, audio, video, other binaries) get a small card
//! with an "Open externally" button that hands the file to `xdg-open`.
//!
//! `render_explorer_pane` is `&self` (right_panel.rs calls it that way), so
//! state changes driven from render — project sync, reconnect retries — go
//! through `cx.spawn` deferred updates. Click handlers are `cx.listener`s
//! with `&mut self` as usual.
// INTEGRATE: web refreshes the tree on `workspaceMutationId` bumps (agent
// edits). When a mutation-refresh hook exists, call `explorer_refresh` so
// agent-written files appear without a manual refresh.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    Context, Div, KeystrokeEvent, MouseButton, ObjectFit, RenderImage, ScrollHandle, SharedString,
    Window, div, img, prelude::*, px, relative,
};
use serde_json::{Value, json};

use crate::icons::{Icon, icon};
use crate::model::ConnectionStatus;
use crate::net::ServerEndpoint;
use crate::{AppView, theme};

/// Image extensions routed through the asset pipeline rather than readFile
/// (matches the web preview's image branch; the image crate decodes them all).
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"];
/// Cap on rendered preview characters — the server already caps reads at
/// 1 MB, and a single Text element that large is a layout hazard.
const PREVIEW_CHAR_CAP: usize = 200_000;
/// GET /api/assets body limit for images.
const IMAGE_FETCH_LIMIT: u64 = 64 * 1024 * 1024;

/// Extensions routed to the "open externally" card without a readFile —
/// grouped by the label the card shows. Anything not listed (and not an
/// image) is tried as text; a `binary_file` read failure lands on the same
/// card.
const OFFICE_EXTENSIONS: &[&str] = &[
    "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "pages", "numbers", "key",
];
const FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "woff", "woff2", "eot", "pfb"];
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "wav", "ogg", "flac", "m4a", "aac", "opus", "aiff", "mid", "midi",
];
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "webm", "mov", "mkv", "avi", "m4v", "mpg", "mpeg", "wmv",
];
const BINARY_EXTENSIONS: &[&str] = &[
    "zip", "gz", "tgz", "bz2", "xz", "7z", "rar", "tar", "jar", "war", "exe", "dll", "so", "dylib",
    "bin", "dat", "db", "sqlite", "sqlite3", "wasm", "class", "pyc", "o", "a", "dmg", "iso", "apk",
    "aar", "pack", "idx",
];

/// Files the inline preview can't render — shown as an "open externally"
/// card instead of attempting a text read.
#[derive(Clone, Copy)]
enum ExternalKind {
    Pdf,
    Office,
    Notebook,
    Font,
    Audio,
    Video,
    Binary,
}

impl ExternalKind {
    /// Bundled lucide glyph for the external-file card (documents →
    /// FileText, media/binaries → File).
    fn glyph(self) -> Icon {
        match self {
            Self::Pdf | Self::Office | Self::Notebook | Self::Font => Icon::FileText,
            Self::Audio | Self::Video | Self::Binary => Icon::File,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pdf => "PDF document",
            Self::Office => "Office document",
            Self::Notebook => "Jupyter notebook",
            Self::Font => "Font file",
            Self::Audio => "Audio file",
            Self::Video => "Video file",
            Self::Binary => "Binary file",
        }
    }
}

/// Extension-based external-file classification. Unknown extensions return
/// None and still go through `projects.readFile` — the server's `binary_file`
/// failure is the backstop for unlisted binaries.
fn external_kind(path: &str) -> Option<ExternalKind> {
    let (_, ext) = path.rsplit_once('.')?;
    match ext.to_ascii_lowercase().as_str() {
        "pdf" => Some(ExternalKind::Pdf),
        "ipynb" => Some(ExternalKind::Notebook),
        e if OFFICE_EXTENSIONS.contains(&e) => Some(ExternalKind::Office),
        e if FONT_EXTENSIONS.contains(&e) => Some(ExternalKind::Font),
        e if AUDIO_EXTENSIONS.contains(&e) => Some(ExternalKind::Audio),
        e if VIDEO_EXTENSIONS.contains(&e) => Some(ExternalKind::Video),
        e if BINARY_EXTENSIONS.contains(&e) => Some(ExternalKind::Binary),
        _ => None,
    }
}

/// Fire-and-forget `xdg-open` on a path or URL, with `gio open` as fallback.
/// Only meaningful when the app shares the server's filesystem/browser —
/// remote workspaces keep the button (it opens locally) rather than faking
/// support.
pub(crate) fn open_external(target: &str) {
    if std::process::Command::new("xdg-open")
        .arg(target)
        .spawn()
        .is_err()
    {
        let _ = std::process::Command::new("gio")
            .arg("open")
            .arg(target)
            .spawn();
    }
}

#[derive(Clone)]
struct Entry {
    /// Workspace-relative path (`a/b/c`).
    path: String,
    /// Last path segment.
    name: String,
    is_dir: bool,
    /// `git check-ignore` marked it — render dimmed (web parity).
    ignored: bool,
}

/// One flattened display row of the expanded-prefix tree.
struct ExplorerRow {
    path: String,
    name: String,
    depth: usize,
    is_dir: bool,
    ignored: bool,
    expanded: bool,
    loading: bool,
    error: Option<String>,
}

/// What the bottom preview split shows for `preview.path`.
enum PreviewKind {
    Loading,
    Text {
        contents: String,
        byte_length: u64,
        /// Server read past 1 MB — show the "preview limited" notice.
        truncated: bool,
        /// Extra display-side cap (PREVIEW_CHAR_CAP) kicked in.
        display_capped: bool,
        markdown: bool,
    },
    Image {
        image: Arc<RenderImage>,
        byte_length: u64,
    },
    /// Non-inline-previewable file — emoji/name/size card + "Open externally".
    External {
        kind: ExternalKind,
        /// On-disk size when the workspace root is statable locally.
        byte_length: Option<u64>,
    },
    Error {
        message: String,
    },
}

struct Preview {
    path: String,
    kind: PreviewKind,
}

/// Inline editing session for a text preview. `text_field::TextField` is
/// single-line only, so editing is a minimal buffer editor driven by an
/// `intercept_keystrokes` hook: printable `key_char`s insert at the caret,
/// arrows/home/end navigate by line and column, ctrl-s/cmd-s saves through
/// `projects.writeFile`, and Escape cancels back to the preview.
struct EditBuffer {
    /// Workspace-relative path being edited (`selected` while open).
    path: String,
    /// Working contents.
    content: String,
    /// Contents at last load/save — `content != saved` is the dirty flag.
    saved: String,
    /// Caret byte offset (always a char boundary).
    cursor: usize,
    /// A `projects.writeFile` request is in flight.
    saving: bool,
    /// Last save error, rendered inline.
    error: Option<String>,
    scroll: ScrollHandle,
}

/// `explorer::State` — owned by `AppView.explorer_state`.
#[derive(Default)]
pub struct State {
    /// Project id the tree belongs to; `None` until synced (or no thread).
    project_key: Option<String>,
    /// Project workspace root — sent as `cwd` on every fs RPC.
    project_root: String,
    /// Selected thread id — `workspace-file` assets resolve through it.
    thread_id: String,
    /// Connection snapshot at last sync; a false→true edge retries the tree.
    online: bool,
    /// Expanded directory paths (root "" is implicit, never stored).
    expanded: HashSet<String>,
    /// directoryPath → sorted children (cached per directory).
    dirs: HashMap<String, Vec<Entry>>,
    /// directoryPath with a request in flight.
    loading: HashSet<String>,
    /// directoryPath ever requested — the refresh re-fetches this set.
    requested: HashSet<String>,
    /// directoryPath → last list error (cleared on retry).
    errors: HashMap<String, String>,
    /// Selected (previewed) workspace-relative file path.
    selected: Option<String>,
    /// Bottom-split preview state for `selected`.
    preview: Option<Preview>,
    /// Active inline editing session for `selected`, if any.
    editing: Option<EditBuffer>,
    /// The editor keystroke interceptor was installed once.
    editor_hook: bool,
    /// Keeps the interceptor subscription alive (drop = unregister).
    editor_sub: Option<gpui::Subscription>,
    /// Bumps on project switch; stale async replies are dropped.
    generation: u64,
    tree_scroll: ScrollHandle,
    preview_scroll: ScrollHandle,
}

/// `(project_id, workspace_root, thread_id)` for the selected thread.
fn explorer_target(state: &crate::model::AppState) -> Option<(String, String, String)> {
    let thread = state.selected_thread()?;
    let project = state.projects.iter().find(|p| p.id == thread.project_id)?;
    Some((
        project.id.to_string(),
        project.path.to_string(),
        thread.id.to_string(),
    ))
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_image_path(path: &str) -> bool {
    let Some((_, ext)) = path.rsplit_once('.') else {
        return false;
    };
    IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

/// Web `isMarkdownPreviewFile` — md/mdx/markdown/mdown/mkd.
fn is_markdown_path(path: &str) -> bool {
    let Some((_, ext)) = path.rsplit_once('.') else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "md" | "mdx" | "markdown" | "mdown" | "mkd"
    )
}

impl EditBuffer {
    /// Byte range of the line containing `cursor`.
    fn line_range(&self) -> (usize, usize) {
        let start = self.content[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let end = self.content[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.content.len());
        (start, end)
    }

    fn insert(&mut self, text: &str) {
        self.content.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.error = None;
    }

    fn backspace(&mut self) {
        let Some(prev) = self.content[..self.cursor].chars().next_back() else {
            return;
        };
        let start = self.cursor - prev.len_utf8();
        self.content.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    fn delete_forward(&mut self) {
        let Some(next) = self.content[self.cursor..].chars().next() else {
            return;
        };
        self.content
            .replace_range(self.cursor..self.cursor + next.len_utf8(), "");
    }

    fn move_left(&mut self) {
        if let Some(prev) = self.content[..self.cursor].chars().next_back() {
            self.cursor -= prev.len_utf8();
        }
    }

    fn move_right(&mut self) {
        if let Some(next) = self.content[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
        }
    }

    fn move_home(&mut self) {
        self.cursor = self.line_range().0;
    }

    fn move_end(&mut self) {
        self.cursor = self.line_range().1;
    }

    /// Up/down: keep the caret's character column, clamped to the target
    /// line's length.
    fn move_vert(&mut self, dir: i32) {
        let (line_start, line_end) = self.line_range();
        let column = self.content[line_start..self.cursor].chars().count();
        let (target_start, target_end) = if dir < 0 {
            if line_start == 0 {
                self.cursor = 0;
                return;
            }
            let end = line_start - 1; // the '\n' before this line
            let start = self.content[..end].rfind('\n').map(|i| i + 1).unwrap_or(0);
            (start, end)
        } else {
            if line_end == self.content.len() {
                self.cursor = self.content.len();
                return;
            }
            let start = line_end + 1;
            let end = self.content[start..]
                .find('\n')
                .map(|i| start + i)
                .unwrap_or(self.content.len());
            (start, end)
        };
        let line: &str = &self.content[target_start..target_end];
        let mut byte = 0usize;
        for (ix, ch) in line.chars().enumerate() {
            if ix == column {
                break;
            }
            byte += ch.len_utf8();
        }
        self.cursor = target_start + byte;
    }

    /// 0-based row of the caret (for `scroll_to_item`).
    fn cursor_row(&self) -> usize {
        self.content[..self.cursor]
            .bytes()
            .filter(|b| *b == b'\n')
            .count()
    }
}

fn format_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.)
    } else {
        format!("{:.1} MB", n as f64 / (1024. * 1024.))
    }
}

/// Sort like the web tree: directories first, then case-insensitive name.
fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.path.cmp(&b.path))
    });
}

fn parse_entries(value: &Value) -> Vec<Entry> {
    let mut entries: Vec<Entry> = value["entries"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let path = e["path"].as_str()?.to_string();
                    Some(Entry {
                        name: file_name(&path).to_string(),
                        is_dir: e["kind"].as_str() == Some("directory"),
                        ignored: e["ignored"].as_bool() == Some(true),
                        path,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    sort_entries(&mut entries);
    entries
}

/// Pull `(failure, message)` out of an Exit failure frame. Server errors land
/// at `exit.cause.error` (RpcResult::err values) or `exit.cause.failure`.
pub(crate) fn rpc_error(exit: &Value) -> (Option<String>, String) {
    let cause = &exit["cause"];
    for err in [
        &cause["error"],
        &cause["failure"],
        &exit["error"],
        &exit["failure"],
        cause,
    ] {
        if err.is_object() {
            let message = err["message"]
                .as_str()
                .unwrap_or("The request failed.")
                .to_string();
            let failure = err["failure"].as_str().map(str::to_owned);
            return (failure, message);
        }
        if let Some(message) = err.as_str() {
            return (None, message.to_string());
        }
    }
    (None, "The request failed.".to_string())
}

/// Decode image bytes into a gpui-renderable image. gpui's software buffers
/// are BGRA, so R and B are swapped (same as composer.rs::decode_thumbnail).
pub(crate) fn decode_image(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let mut data = image::load_from_memory(bytes).ok()?.into_rgba8();
    for pixel in data.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new([image::Frame::new(data)])))
}

/// Blocking bearer-authed GET → decoded image (runs inside `smol::unblock`).
pub(crate) fn fetch_image(
    endpoint: &ServerEndpoint,
    url: &str,
) -> Result<(Arc<RenderImage>, u64), String> {
    let mut response = ureq::get(url)
        .header("authorization", format!("Bearer {}", endpoint.token))
        .call()
        .map_err(|e| format!("image fetch failed: {e}"))?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(IMAGE_FETCH_LIMIT)
        .read_to_vec()
        .map_err(|e| format!("image read failed: {e}"))?;
    let len = bytes.len() as u64;
    let image = decode_image(&bytes).ok_or_else(|| "image decode failed".to_string())?;
    Ok((image, len))
}

impl AppView {
    /// Deferred reconcile: runs after render when the selected project changed
    /// or the connection came back. Resets per-project state, then (re)loads
    /// the root and any requested directories still missing data.
    fn explorer_sync(&mut self, cx: &mut Context<Self>) {
        let target = explorer_target(&self.state);
        let online = matches!(self.state.connection, ConnectionStatus::Connected);
        let key = target.as_ref().map(|t| t.0.clone());

        if self.explorer_state.project_key != key {
            let s = &mut self.explorer_state;
            s.project_key = key;
            s.project_root = target.clone().map(|t| t.1).unwrap_or_default();
            s.thread_id = target.map(|t| t.2).unwrap_or_default();
            s.expanded.clear();
            s.dirs.clear();
            s.loading.clear();
            s.requested.clear();
            s.errors.clear();
            s.selected = None;
            s.preview = None;
            s.editing = None;
            s.generation = s.generation.wrapping_add(1);
            s.tree_scroll = ScrollHandle::new();
            s.preview_scroll = ScrollHandle::new();
            if s.project_key.is_some() {
                self.explorer_load_dir(String::new(), false, cx);
            }
        } else {
            // Same project — keep thread_id fresh (worktree assets resolve
            // through it) without touching the tree.
            self.explorer_state.thread_id = target.map(|t| t.2).unwrap_or_default();
        }
        self.explorer_state.online = online;

        // Reconnect edge: retry requested dirs that never loaded or errored.
        if online {
            let retry: Vec<String> = self
                .explorer_state
                .requested
                .iter()
                .filter(|d| {
                    !self.explorer_state.dirs.contains_key(*d)
                        && !self.explorer_state.loading.contains(*d)
                })
                .cloned()
                .collect();
            for dir in retry {
                self.explorer_load_dir(dir, true, cx);
            }
        }
        cx.notify();
    }

    /// `projects.listEntries` for one directory. `refresh` re-issues even when
    /// cached; in-flight duplicates are coalesced.
    fn explorer_load_dir(&mut self, dir: String, refresh: bool, cx: &mut Context<Self>) {
        if self.explorer_state.project_key.is_none() {
            return;
        }
        if self.explorer_state.loading.contains(&dir) {
            return;
        }
        if !refresh && self.explorer_state.dirs.contains_key(&dir) {
            return;
        }
        self.explorer_state.loading.insert(dir.clone());
        self.explorer_state.requested.insert(dir.clone());
        self.explorer_state.errors.remove(&dir);
        cx.notify();
        let Some(rx) = self.send_query(
            "projects.listEntries",
            json!({
                "cwd": self.explorer_state.project_root,
                "projectId": self.explorer_state.project_key,
                "directoryPath": dir,
            }),
        ) else {
            self.explorer_state.loading.remove(&dir);
            self.explorer_state
                .errors
                .insert(dir, "Not connected to a server.".to_string());
            cx.notify();
            return;
        };
        let generation = self.explorer_state.generation;
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let _ = this.update(cx, |this, cx| {
                this.explorer_apply_dir(dir, exit, generation, cx);
            });
        })
        .detach();
    }

    fn explorer_apply_dir(
        &mut self,
        dir: String,
        exit: Value,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        if self.explorer_state.generation != generation {
            return;
        }
        self.explorer_state.loading.remove(&dir);
        if exit["_tag"].as_str() == Some("Success") {
            self.explorer_state
                .dirs
                .insert(dir.clone(), parse_entries(&exit["value"]));
            self.explorer_state.errors.remove(&dir);
        } else {
            let (_, message) = rpc_error(&exit);
            self.explorer_state.errors.insert(dir, message);
        }
        cx.notify();
    }

    /// Expand/collapse a directory row; expanding lazy-loads its children.
    fn explorer_toggle_dir(&mut self, dir: String, cx: &mut Context<Self>) {
        if !self.explorer_state.expanded.insert(dir.clone()) {
            self.explorer_state.expanded.remove(&dir);
            cx.notify();
            return;
        }
        self.explorer_load_dir(dir, false, cx);
    }

    /// Reveal `path` in the tree (expanding+loading ancestor dirs so the row
    /// is visible) and preview it. Used by the search dialog's activate.
    /// If the pane never synced this project (Files tab not opened yet),
    /// reconcile first so `project_root`/`project_key` back the RPCs.
    pub(crate) fn explorer_reveal(&mut self, path: String, cx: &mut Context<Self>) {
        let target_key = explorer_target(&self.state).map(|t| t.0);
        if self.explorer_state.project_key != target_key {
            self.explorer_sync(cx);
        }
        let parts: Vec<&str> = path.split('/').collect();
        let mut ancestor = String::new();
        for seg in &parts[..parts.len().saturating_sub(1)] {
            if !ancestor.is_empty() {
                ancestor.push('/');
            }
            ancestor.push_str(seg);
            if self.explorer_state.expanded.insert(ancestor.clone()) {
                self.explorer_load_dir(ancestor.clone(), false, cx);
            }
        }
        self.explorer_open_file(path, cx);
    }

    fn explorer_open_file(&mut self, path: String, cx: &mut Context<Self>) {
        // Opening (or reloading) a file leaves any editing session — the
        // buffer is either stale (other path) or about to be re-read.
        self.explorer_state.editing = None;
        self.explorer_state.selected = Some(path.clone());
        self.explorer_state.preview = Some(Preview {
            path: path.clone(),
            kind: PreviewKind::Loading,
        });
        self.explorer_state.preview_scroll = ScrollHandle::new();
        cx.notify();
        if is_image_path(&path) {
            self.explorer_load_image(path, cx);
        } else if let Some(kind) = external_kind(&path) {
            let byte_length = self.explorer_stat_len(&path);
            self.explorer_state.preview = Some(Preview {
                path,
                kind: PreviewKind::External { kind, byte_length },
            });
            cx.notify();
        } else {
            self.explorer_read_file(path, cx);
        }
    }

    /// Absolute host path for a workspace-relative file. Only meaningful
    /// when the server shares this machine's filesystem — the same caveat
    /// `xdg-open` carries.
    fn explorer_abs_path(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.explorer_state.project_root.trim_end_matches('/'),
            path
        )
    }

    /// Local stat of the workspace file, for the external card's size line.
    /// None when the root isn't on this machine (remote environment).
    fn explorer_stat_len(&self, path: &str) -> Option<u64> {
        std::fs::metadata(self.explorer_abs_path(path))
            .ok()
            .map(|m| m.len())
    }

    fn explorer_read_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(rx) = self.send_query(
            "projects.readFile",
            json!({
                "cwd": self.explorer_state.project_root,
                "projectId": self.explorer_state.project_key,
                "relativePath": path,
            }),
        ) else {
            self.explorer_state.preview = Some(Preview {
                path,
                kind: PreviewKind::Error {
                    message: "Not connected to a server.".to_string(),
                },
            });
            cx.notify();
            return;
        };
        let generation = self.explorer_state.generation;
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let _ = this.update(cx, |this, cx| {
                this.explorer_apply_file(path, exit, generation, cx);
            });
        })
        .detach();
    }

    fn explorer_apply_file(
        &mut self,
        path: String,
        exit: Value,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        // Drop stale replies: project switched or a newer file was selected.
        if self.explorer_state.generation != generation
            || self.explorer_state.selected.as_deref() != Some(path.as_str())
        {
            return;
        }
        let kind = if exit["_tag"].as_str() == Some("Success") {
            let value = &exit["value"];
            let mut contents = value["contents"].as_str().unwrap_or_default().to_string();
            let byte_length = value["byteLength"].as_u64().unwrap_or(0);
            let truncated = value["truncated"].as_bool() == Some(true);
            let display_capped = contents.len() > PREVIEW_CHAR_CAP;
            if display_capped {
                let mut end = PREVIEW_CHAR_CAP.min(contents.len());
                while !contents.is_char_boundary(end) {
                    end -= 1;
                }
                contents.truncate(end);
            }
            PreviewKind::Text {
                contents,
                byte_length,
                truncated,
                display_capped,
                markdown: is_markdown_path(&path),
            }
        } else {
            let (failure, message) = rpc_error(&exit);
            if failure.as_deref() == Some("binary_file") {
                let byte_length = self.explorer_stat_len(&path);
                PreviewKind::External {
                    kind: ExternalKind::Binary,
                    byte_length,
                }
            } else {
                PreviewKind::Error { message }
            }
        };
        self.explorer_state.preview = Some(Preview { path, kind });
        cx.notify();
    }

    /// Image preview: `assets.createUrl` signs the workspace file, then a
    /// bearer-authed GET fetches the bytes (blocking → `smol::unblock`).
    fn explorer_load_image(&mut self, path: String, cx: &mut Context<Self>) {
        let fail = |this: &mut Self, message: String, cx: &mut Context<Self>| {
            this.explorer_state.preview = Some(Preview {
                path: path.clone(),
                kind: PreviewKind::Error { message },
            });
            cx.notify();
        };
        let Some(endpoint) = self.active_endpoint.clone() else {
            fail(self, "Not connected to a server.".to_string(), cx);
            return;
        };
        let Some(rx) = self.send_query(
            "assets.createUrl",
            json!({
                "resource": {
                    "_tag": "workspace-file",
                    "threadId": self.explorer_state.thread_id,
                    "path": path,
                }
            }),
        ) else {
            fail(self, "Not connected to a server.".to_string(), cx);
            return;
        };
        let generation = self.explorer_state.generation;
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let result: Result<(Arc<RenderImage>, u64), String> =
                if exit["_tag"].as_str() == Some("Success") {
                    match exit["value"]["relativeUrl"].as_str() {
                        Some(rel) => {
                            let url = format!(
                                "{}{}",
                                endpoint.http_url.trim_end_matches('/'),
                                rel
                            );
                            let ep = endpoint.clone();
                            smol::unblock(move || fetch_image(&ep, &url)).await
                        }
                        None => Err("The server did not return an asset URL.".to_string()),
                    }
                } else {
                    let (_, message) = rpc_error(&exit);
                    Err(message)
                };
            let _ = this.update(cx, |this, cx| {
                if this.explorer_state.generation != generation
                    || this.explorer_state.selected.as_deref() != Some(path.as_str())
                {
                    return;
                }
                let kind = match result {
                    Ok((image, byte_length)) => PreviewKind::Image { image, byte_length },
                    Err(message) => PreviewKind::Error { message },
                };
                this.explorer_state.preview = Some(Preview { path, kind });
                cx.notify();
            });
        })
        .detach();
    }

    // ------------------------------------------------------------------
    // Inline file editing (projects.writeFile)
    // ------------------------------------------------------------------

    /// Install the editor keystroke interceptor once. It runs before keymap
    /// dispatch so printable keys land in the buffer even though focus
    /// stays on the composer/terminal — the editor is modal while open.
    fn ensure_editor_hook(&mut self, cx: &mut Context<Self>) {
        if self.explorer_state.editor_hook {
            return;
        }
        self.explorer_state.editor_hook = true;
        let subscription = cx.intercept_keystrokes(|event, window, cx| {
            let Some(view) = window.root::<AppView>().flatten() else {
                return;
            };
            view.update(cx, |this, cx| this.on_explorer_edit_key(event, window, cx));
        });
        self.explorer_state.editor_sub = Some(subscription);
    }

    /// "Edit" on a text preview: swap the read-only preview for the buffer.
    /// Truncated/display-capped previews never reach this — saving a partial
    /// read back would silently truncate the file.
    fn explorer_begin_edit(&mut self, cx: &mut Context<Self>) {
        let Some((path, contents)) =
            self.explorer_state
                .preview
                .as_ref()
                .and_then(|preview| match &preview.kind {
                    PreviewKind::Text {
                        contents,
                        truncated: false,
                        display_capped: false,
                        ..
                    } => Some((preview.path.clone(), contents.clone())),
                    _ => None,
                })
        else {
            return;
        };
        self.ensure_editor_hook(cx);
        self.explorer_state.editing = Some(EditBuffer {
            path,
            content: contents.clone(),
            saved: contents,
            cursor: 0,
            saving: false,
            error: None,
            scroll: ScrollHandle::new(),
        });
        cx.notify();
    }

    /// Escape / Cancel — drop the buffer, back to the read-only preview.
    fn explorer_end_edit(&mut self, cx: &mut Context<Self>) {
        self.explorer_state.editing = None;
        cx.notify();
    }

    /// ctrl-s/cmd-s — `projects.writeFile`, then re-read so the preview shows
    /// exactly what landed on disk.
    fn explorer_save_edit(&mut self, cx: &mut Context<Self>) {
        let Some(ed) = self.explorer_state.editing.as_mut() else {
            return;
        };
        if ed.saving {
            return;
        }
        ed.saving = true;
        ed.error = None;
        let path = ed.path.clone();
        let contents = ed.content.clone();
        let cwd = self.explorer_state.project_root.clone();
        let Some(rx) = self.send_query(
            "projects.writeFile",
            json!({
                "cwd": cwd,
                "relativePath": path,
                "contents": contents,
            }),
        ) else {
            if let Some(ed) = self.explorer_state.editing.as_mut() {
                ed.saving = false;
                ed.error = Some("Not connected to a server.".to_string());
            }
            cx.notify();
            return;
        };
        cx.notify();
        let generation = self.explorer_state.generation;
        cx.spawn(async move |this, cx| {
            let exit = rx.recv().await.unwrap_or_else(|_| {
                json!({"_tag": "Failure", "cause": {"error": {"message": "Connection closed before the reply arrived."}}})
            });
            let _ = this.update(cx, |this, cx| {
                if this.explorer_state.generation != generation {
                    return;
                }
                if exit["_tag"].as_str() == Some("Success") {
                    this.explorer_state.editing = None;
                    this.explorer_refresh(cx);
                } else {
                    let (_, message) = rpc_error(&exit);
                    if let Some(ed) = this.explorer_state.editing.as_mut() {
                        ed.saving = false;
                        ed.error = Some(message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Editor keystrokes, captured app-wide before dispatch. Unhandled keys
    /// fall through so palette/overlay shortcuts keep working.
    fn on_explorer_edit_key(
        &mut self,
        event: &KeystrokeEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.explorer_state.editing.is_none() {
            return;
        }
        // Another surface owns the keyboard right now.
        if self.palette_state.open
            || self.search_state.open
            || self.settings_state.open
            || self.welcome_state.open
        {
            return;
        }
        let ks = &event.keystroke;
        let mods = ks.modifiers;
        if (mods.control || mods.platform) && ks.key == "s" {
            self.explorer_save_edit(cx);
            cx.stop_propagation();
            return;
        }
        if mods.control || mods.platform || mods.function {
            return;
        }
        let consumed = {
            let Some(ed) = self.explorer_state.editing.as_mut() else {
                return;
            };
            match ks.key.as_str() {
                "escape" => true,
                "enter" => {
                    ed.insert("\n");
                    true
                }
                "tab" => {
                    ed.insert("\t");
                    true
                }
                "backspace" => {
                    ed.backspace();
                    true
                }
                "delete" => {
                    ed.delete_forward();
                    true
                }
                "left" => {
                    ed.move_left();
                    true
                }
                "right" => {
                    ed.move_right();
                    true
                }
                "up" => {
                    ed.move_vert(-1);
                    true
                }
                "down" => {
                    ed.move_vert(1);
                    true
                }
                "home" => {
                    ed.move_home();
                    true
                }
                "end" => {
                    ed.move_end();
                    true
                }
                _ => {
                    if let Some(text) = &ks.key_char {
                        ed.insert(text);
                        true
                    } else {
                        false
                    }
                }
            }
        };
        if !consumed {
            return;
        }
        cx.stop_propagation();
        if event.keystroke.key == "escape" {
            self.explorer_state.editing = None;
        }
        cx.notify();
    }

    /// Refresh button: re-fetch every requested directory plus the open file.
    fn explorer_refresh(&mut self, cx: &mut Context<Self>) {
        let dirs: Vec<String> = self.explorer_state.requested.iter().cloned().collect();
        for dir in dirs {
            self.explorer_load_dir(dir, true, cx);
        }
        if self.explorer_state.requested.is_empty() && self.explorer_state.project_key.is_some() {
            self.explorer_load_dir(String::new(), true, cx);
        }
        if let Some(path) = self.explorer_state.selected.clone() {
            self.explorer_open_file(path, cx);
        }
    }

    /// Flatten the expanded-prefix tree into display rows.
    fn explorer_rows(&self) -> Vec<ExplorerRow> {
        let mut rows = Vec::new();
        self.explorer_walk("", 0, &mut rows);
        rows
    }

    fn explorer_walk(&self, dir: &str, depth: usize, rows: &mut Vec<ExplorerRow>) {
        let Some(entries) = self.explorer_state.dirs.get(dir) else {
            return;
        };
        for e in entries {
            let expanded = e.is_dir && self.explorer_state.expanded.contains(&e.path);
            rows.push(ExplorerRow {
                path: e.path.clone(),
                name: e.name.clone(),
                depth,
                is_dir: e.is_dir,
                ignored: e.ignored,
                expanded,
                loading: self.explorer_state.loading.contains(&e.path),
                error: self.explorer_state.errors.get(&e.path).cloned(),
            });
            if expanded {
                self.explorer_walk(&e.path, depth + 1, rows);
            }
        }
    }

    // ------------------------------------------------------------------
    // Rendering
    // ------------------------------------------------------------------

    pub(crate) fn render_explorer_pane(&self, cx: &mut Context<Self>) -> Div {
        // Reconcile scheduled after this render: `&self` can't mutate here.
        let target_key = explorer_target(&self.state).map(|t| t.0);
        let online = matches!(self.state.connection, ConnectionStatus::Connected);
        if self.explorer_state.project_key != target_key || (online && !self.explorer_state.online)
        {
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| this.explorer_sync(cx));
            })
            .detach();
        }

        let mut pane = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .text_xs();

        // Header: project name + refresh.
        let project_name = explorer_target(&self.state)
            .and_then(|(pid, _, _)| {
                self.state
                    .projects
                    .iter()
                    .find(|p| p.id.as_ref() == pid)
                    .map(|p| p.name.to_string())
            })
            .unwrap_or_else(|| "Files".to_string());
        // Header: project name + refresh. Matches the web
        // FILE_SURFACE_SUBHEADER inline variant: h-9 (36px), px-2, no
        // bottom border, 4px gap before the content.
        pane = pane.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .h(px(36.))
                .px_2()
                .mb_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_nowrap()
                        .overflow_x_hidden()
                        .text_ellipsis()
                        .text_color(theme::fg_muted())
                        .child(project_name),
                )
                .child(
                    div()
                        .id("explorer-refresh")
                        .w(px(22.))
                        .h(px(22.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .cursor_pointer()
                        .text_color(theme::fg_muted())
                        .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                        .child(icon(Icon::RefreshCw, 12.).text_color(theme::fg_muted()))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _w, cx| this.explorer_refresh(cx)),
                        ),
                ),
        );

        // Tree body.
        if self.explorer_state.project_key.is_none() {
            pane = pane.child(self.explorer_empty("Select a thread"));
        } else if let Some(err) = self.explorer_state.errors.get("").cloned() {
            let el = self
                .explorer_empty(&format!("Couldn't load files — {err}"))
                .child(
                    div()
                        .id("explorer-retry")
                        .mt_2()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .cursor_pointer()
                        .bg(theme::raised())
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::hover()))
                        .child("Retry")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _w, cx| this.explorer_refresh(cx)),
                        ),
                );
            pane = pane.child(el);
        } else if !self.explorer_state.dirs.contains_key("") {
            pane = pane.child(self.explorer_empty("Loading…"));
        } else {
            let rows = self.explorer_rows();
            if rows.is_empty() {
                pane = pane.child(self.explorer_empty("No files"));
            } else {
                let mut tree = div()
                    .id("explorer-tree")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.explorer_state.tree_scroll)
                    .px_1()
                    .py_1();
                for (ix, row) in rows.iter().enumerate() {
                    tree = tree.child(self.explorer_row(ix, row, cx));
                }
                pane = pane.child(tree);
            }
        }

        // Preview split.
        if let Some(preview) = &self.explorer_state.preview {
            pane = pane.child(self.render_explorer_preview(preview, cx));
        }
        pane
    }

    fn explorer_empty(&self, message: &str) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .p_4()
            .text_color(theme::fg_muted())
            .child(message.to_string())
    }

    fn explorer_row(
        &self,
        ix: usize,
        row: &ExplorerRow,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let is_selected =
            !row.is_dir && self.explorer_state.selected.as_deref() == Some(row.path.as_str());
        let path = row.path.clone();
        let is_dir = row.is_dir;
        // Pierre FileTree glyphs: 13px carets (ChevronRight/ChevronDown) in
        // --trees-fg-muted, then the type icon — FolderOpen/Folder for dirs,
        // File for files — with ignored names dimmed to --trees-fg at 40%.
        let type_icon = if row.is_dir {
            if row.expanded {
                Icon::FolderOpen
            } else {
                Icon::Folder
            }
        } else {
            Icon::File
        };
        // Pierre trees: 30px rows (--trees-item-height), 8px level gap,
        // 8px item padding-x, radius-md, fg·7% hover / fg·12% selected.
        let mut el = div()
            .id(("xpl-row", ix))
            .flex()
            .items_center()
            .h(px(30.))
            .pl(px(8. + row.depth as f32 * 8.))
            .pr_2()
            .rounded(theme::radius::MD)
            .cursor_pointer()
            .whitespace_nowrap()
            .when(is_selected, |s| s.bg(theme::tree_selected()))
            .when(!is_selected, |s| s.hover(|s| s.bg(theme::tree_hover())))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _w, cx| {
                    if is_dir {
                        this.explorer_toggle_dir(path.clone(), cx);
                    } else {
                        this.explorer_open_file(path.clone(), cx);
                    }
                }),
            )
            // 16px caret lane (--trees-icon-width) — chevron for dirs only.
            .child(
                div()
                    .w(px(16.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(row.is_dir, |el| {
                        el.child(
                            if row.expanded {
                                icon(Icon::ChevronDown, 13.)
                            } else {
                                icon(Icon::ChevronRight, 13.)
                            }
                            .text_color(theme::tree_chevron_fg()),
                        )
                    }),
            )
            // Type icon + 6px row gap.
            .child(
                div()
                    .w(px(14.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .mr(px(6.))
                    .child(icon(type_icon, 14.).text_color(if row.is_dir {
                        theme::fg_muted()
                    } else {
                        theme::tree_icon_fg()
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .text_ellipsis()
                    // Web tree: git-ignored names dim to --trees-fg at 40%;
                    // directories keep the normal foreground.
                    .text_color(if row.ignored {
                        theme::tree_ignored()
                    } else {
                        theme::fg()
                    })
                    .child(row.name.clone()),
            );
        if row.loading {
            el = el.child(
                div()
                    .flex_none()
                    .pl_1()
                    .child(icon(Icon::LoaderCircle, 12.).text_color(theme::tree_chevron_fg())),
            );
        }
        if let Some(err) = &row.error {
            let short: String = err.chars().take(40).collect();
            el = el.child(
                div()
                    .flex_none()
                    .pl_1()
                    .text_color(theme::error_fg())
                    .child(if err.chars().count() > 40 {
                        format!("{short}…")
                    } else {
                        short
                    }),
            );
        }
        el
    }

    fn render_explorer_preview(&self, preview: &Preview, cx: &mut Context<Self>) -> Div {
        let (byte_length, truncated) = match &preview.kind {
            PreviewKind::Text {
                byte_length,
                truncated,
                display_capped,
                ..
            } => (*byte_length, *truncated || *display_capped),
            PreviewKind::Image { byte_length, .. } => (*byte_length, false),
            PreviewKind::External { byte_length, .. } => (byte_length.unwrap_or(0), false),
            _ => (0, false),
        };
        let editing = self
            .explorer_state
            .editing
            .as_ref()
            .filter(|ed| ed.path == preview.path);
        // Only a complete text read is editable — writing a truncated
        // preview back would destroy the tail of the file.
        let editable = matches!(
            &preview.kind,
            PreviewKind::Text {
                truncated: false,
                display_capped: false,
                ..
            }
        ) && editing.is_none();
        let mut pane = div()
            .flex_none()
            .h(relative(0.45))
            .min_h(px(140.))
            .border_t_1()
            .border_color(theme::border())
            .flex()
            .flex_col()
            // Inline preview subheader: web h-7 (28px), no bottom border.
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(28.))
                    .px_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .overflow_x_hidden()
                            .text_ellipsis()
                            .font_family(theme::FONT_MONO)
                            .child(SharedString::from(preview.path.clone())),
                    )
                    .when(byte_length > 0, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .text_color(theme::fg_muted())
                                .child(format_bytes(byte_length)),
                        )
                    })
                    .when(editable, |el| {
                        el.child(
                            div()
                                .id("explorer-preview-edit")
                                .flex_none()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .cursor_pointer()
                                .text_color(theme::fg_muted())
                                .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                                .child("Edit")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _ev, _w, cx| {
                                        this.explorer_begin_edit(cx);
                                    }),
                                ),
                        )
                    })
                    .when_some(editing, |el, ed| {
                        let dirty = ed.content != ed.saved;
                        el.child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_color(if dirty {
                                    gpui::rgb(0xf5a97f)
                                } else {
                                    theme::fg_muted()
                                })
                                .when(dirty, |el| {
                                    el.child(icon(Icon::Dot, 14.).text_color(gpui::rgb(0xf5a97f)))
                                })
                                .child(if ed.saving {
                                    "saving…"
                                } else if dirty {
                                    "modified · ctrl-s save"
                                } else {
                                    "ctrl-s save"
                                }),
                        )
                        .child(
                            div()
                                .id("explorer-edit-save")
                                .flex_none()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .cursor_pointer()
                                .text_color(theme::fg_muted())
                                .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                                .child("Save")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _ev, _w, cx| {
                                        this.explorer_save_edit(cx);
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .id("explorer-edit-cancel")
                                .flex_none()
                                .px_1()
                                .rounded_sm()
                                .cursor_pointer()
                                .text_color(theme::fg_muted())
                                .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                                .child("esc")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _ev, _w, cx| {
                                        this.explorer_end_edit(cx);
                                    }),
                                ),
                        )
                    })
                    .child(
                        div()
                            .id("explorer-preview-close")
                            .flex_none()
                            .w(px(20.))
                            .h(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_color(theme::fg_muted())
                            .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                            .child(icon(Icon::X, 12.).text_color(theme::fg_muted()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _w, cx| {
                                    this.explorer_state.selected = None;
                                    this.explorer_state.preview = None;
                                    this.explorer_state.editing = None;
                                    cx.notify();
                                }),
                            ),
                    ),
            );
        if truncated {
            let abs = self.explorer_abs_path(&preview.path);
            pane = pane.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .text_color(theme::fg_muted())
                    .border_b_1()
                    .border_color(theme::border())
                    .child(div().flex_1().min_w_0().child(format!(
                        "File truncated — preview shows the first part of a {} file.",
                        format_bytes(byte_length)
                    )))
                    .child(
                        div()
                            .id("explorer-truncated-open")
                            .flex_none()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .cursor_pointer()
                            .bg(theme::raised())
                            .text_color(theme::fg())
                            .hover(|s| s.bg(theme::hover()))
                            .child("Open externally")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |_this, _ev, _w, _cx| {
                                    open_external(&abs);
                                }),
                            ),
                    ),
            );
        }
        // Web file previews sit on `--code-background` at the 13px code
        // font (--diffs-font-size = DEFAULT_CODE_FONT_SIZE).
        let body = div()
            .id("explorer-preview-body")
            .flex_1()
            .min_h_0()
            .bg(theme::code_bg())
            .overflow_y_scroll()
            .track_scroll(&self.explorer_state.preview_scroll);
        if let Some(ed) = editing {
            return pane.child(self.render_explorer_editor(ed, cx));
        }
        pane.child(match &preview.kind {
            PreviewKind::Loading => body
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme::fg_muted())
                .child("Loading…"),
            PreviewKind::External { kind, byte_length } => {
                let abs = self.explorer_abs_path(&preview.path);
                body.flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .p_4()
                    .text_color(theme::fg_muted())
                    .child(div().child(icon(kind.glyph(), 32.).text_color(theme::icon_muted())))
                    .child(
                        div()
                            .font_family(theme::FONT_MONO)
                            .text_color(theme::fg())
                            .child(file_name(&preview.path).to_string()),
                    )
                    .child(div().child(match byte_length {
                        Some(n) => format!("{} · {}", kind.label(), format_bytes(*n)),
                        None => kind.label().to_string(),
                    }))
                    .child(
                        div()
                            .id("explorer-open-external")
                            .mt_1()
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .bg(theme::raised())
                            .text_color(theme::fg())
                            .hover(|s| s.bg(theme::hover()))
                            .child("Open externally")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |_this, _ev, _w, _cx| {
                                    open_external(&abs);
                                }),
                            ),
                    )
            }
            PreviewKind::Error { message } => body
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .p_4()
                .text_color(theme::fg_muted())
                .child(message.clone()),
            PreviewKind::Image { image, .. } => {
                body.flex().items_center().justify_center().p_2().child(
                    img(gpui::ImageSource::Render(image.clone()))
                        .object_fit(ObjectFit::Contain)
                        .max_w_full()
                        .max_h_full(),
                )
            }
            PreviewKind::Text {
                contents, markdown, ..
            } => {
                if *markdown {
                    body.p_3()
                        .children(crate::markdown::render_markdown(contents))
                } else {
                    body.p_2().child(
                        div()
                            .font_family(theme::FONT_MONO)
                            .text_size(theme::code_text_size())
                            .text_color(theme::fg())
                            .child(SharedString::from(contents.clone())),
                    )
                }
            }
        })
    }

    /// The edit-mode body: one fixed-height row per buffer line, with a
    /// 1px caret drawn on the cursor line (text before / caret / text after
    /// — monospace, so the split lands exactly on the column).
    fn render_explorer_editor(&self, ed: &EditBuffer, _cx: &mut Context<Self>) -> Div {
        const EDIT_LINE_HEIGHT: f32 = 17.;
        let cursor_row = ed.cursor_row();
        let caret_start: usize = ed
            .content
            .split_inclusive('\n')
            .take(cursor_row)
            .map(|piece| piece.len())
            .sum();
        let caret_col = ed.cursor - caret_start;

        let mut list = div()
            .id("explorer-editor")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&ed.scroll)
            .px_2()
            .py_1()
            .font_family(theme::FONT_MONO)
            .text_size(theme::code_text_size())
            .text_color(theme::fg());
        for (row, line) in ed.content.split('\n').enumerate() {
            let mut line_row = div()
                .h(px(EDIT_LINE_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .whitespace_nowrap()
                .overflow_x_hidden();
            if row == cursor_row {
                let (before, after) = line.split_at(caret_col.min(line.len()));
                line_row = line_row
                    .child(div().child(SharedString::from(before.to_string())))
                    .child(div().w(px(1.)).h_full().bg(theme::cursor()))
                    .child(div().child(SharedString::from(after.to_string())));
            } else {
                line_row = line_row.child(div().child(SharedString::from(if line.is_empty() {
                    " ".to_string()
                } else {
                    line.to_string()
                })));
            }
            list = list.child(line_row);
        }
        ed.scroll.scroll_to_item(cursor_row);

        let mut pane = div().flex().flex_col().flex_1().min_h_0();
        if let Some(err) = &ed.error {
            pane = pane.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(gpui::rgb(0xf87171))
                    .border_b_1()
                    .border_color(theme::border())
                    .child(format!("Couldn't save: {err}")),
            );
        }
        pane.child(list)
    }
}
