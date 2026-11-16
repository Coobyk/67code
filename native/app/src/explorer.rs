//! File explorer + preview pane (right-panel Files tab).
//!
//! Mirrors apps/web `FileBrowserPanel` + `FilePreviewPanel` at panel scale:
//! lazy one-level `projects.listEntries {cwd, directoryPath}` directory loads
//! on expand, `projects.readFile` for text/markdown preview, and
//! `assets.createUrl` (`workspace-file` resource) + `GET /api/assets/<key>`
//! for image previews. Binary files show a placeholder.
//!
//! `render_explorer_pane` is `&self` (right_panel.rs calls it that way), so
//! state changes driven from render — project sync, reconnect retries — go
//! through `cx.spawn` deferred updates. Click handlers are `cx.listener`s
//! with `&mut self` as usual.
// INTEGRATE: web refreshes the tree on `workspaceMutationId` bumps (agent
// edits). When a mutation-refresh hook exists, call `explorer_refresh` so
// agent-written files appear without a manual ↻.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    Context, Div, MouseButton, ObjectFit, RenderImage, ScrollHandle, SharedString, div, img,
    prelude::*, px, relative,
};
use serde_json::{Value, json};

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
    Binary,
    Error {
        message: String,
    },
}

struct Preview {
    path: String,
    kind: PreviewKind,
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
fn rpc_error(exit: &Value) -> (Option<String>, String) {
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
fn decode_image(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let mut data = image::load_from_memory(bytes).ok()?.into_rgba8();
    for pixel in data.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new([image::Frame::new(data)])))
}

/// Blocking bearer-authed GET → decoded image (runs inside `smol::unblock`).
fn fetch_image(endpoint: &ServerEndpoint, url: &str) -> Result<(Arc<RenderImage>, u64), String> {
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

    /// Single-click a file row: select it and start the preview.
    /// Reveal `path` in the tree (expanding+loading ancestor dirs so the row
    /// is visible) and preview it. Used by the search dialog's activate.
    pub(crate) fn explorer_reveal(&mut self, path: String, cx: &mut Context<Self>) {
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
        self.explorer_state.selected = Some(path.clone());
        self.explorer_state.preview = Some(Preview {
            path: path.clone(),
            kind: PreviewKind::Loading,
        });
        self.explorer_state.preview_scroll = ScrollHandle::new();
        cx.notify();
        if is_image_path(&path) {
            self.explorer_load_image(path, cx);
        } else {
            self.explorer_read_file(path, cx);
        }
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
                PreviewKind::Binary
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
        pane = pane.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .h(px(28.))
                .px_2()
                .border_b_1()
                .border_color(theme::border())
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
                        .px_1p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .text_color(theme::fg_muted())
                        .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                        .child("↻")
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
            let el = self.explorer_empty(&format!("Couldn't load files — {err}")).child(
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
        let chevron = if row.is_dir {
            if row.expanded { "▾" } else { "▸" }
        } else {
            ""
        };
        let path = row.path.clone();
        let is_dir = row.is_dir;
        let mut el = div()
            .id(("xpl-row", ix))
            .flex()
            .items_center()
            .pl(px(4. + row.depth as f32 * 12.))
            .pr_2()
            .py(px(2.5))
            .rounded_sm()
            .cursor_pointer()
            .whitespace_nowrap()
            .when(is_selected, |s| s.bg(theme::selected()))
            .when(!is_selected, |s| s.hover(|s| s.bg(theme::hover())))
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
            .child(
                div()
                    .w(px(14.))
                    .flex_none()
                    .text_color(theme::fg_muted())
                    .child(chevron),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .text_ellipsis()
                    .when(row.ignored || row.is_dir, |s| {
                        s.text_color(theme::fg_muted())
                    })
                    .when(!row.ignored && !row.is_dir, |s| {
                        s.text_color(theme::fg())
                    })
                    .when(is_selected, |s| s.text_color(theme::fg()))
                    .child(row.name.clone()),
            );
        if row.loading {
            el = el.child(
                div()
                    .flex_none()
                    .pl_1()
                    .text_color(theme::fg_muted())
                    .child("…"),
            );
        }
        if let Some(err) = &row.error {
            let short: String = err.chars().take(40).collect();
            el = el.child(
                div()
                    .flex_none()
                    .pl_1()
                    .text_color(gpui::rgb(0xf87171))
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
            _ => (0, false),
        };
        let mut pane = div()
            .flex_none()
            .h(relative(0.45))
            .min_h(px(140.))
            .border_t_1()
            .border_color(theme::border())
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(26.))
                    .px_2()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .overflow_x_hidden()
                            .text_ellipsis()
                            .font_family("monospace")
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
                    .child(
                        div()
                            .id("explorer-preview-close")
                            .flex_none()
                            .px_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_color(theme::fg_muted())
                            .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                            .child("✕")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _ev, _w, cx| {
                                    this.explorer_state.selected = None;
                                    this.explorer_state.preview = None;
                                    cx.notify();
                                }),
                            ),
                    ),
            );
        if truncated {
            pane = pane.child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(theme::fg_muted())
                    .border_b_1()
                    .border_color(theme::border())
                    .child(format!(
                        "Preview limited to the first part of a {} file.",
                        format_bytes(byte_length)
                    )),
            );
        }
        let body = div()
            .id("explorer-preview-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.explorer_state.preview_scroll);
        pane.child(match &preview.kind {
            PreviewKind::Loading => body
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme::fg_muted())
                .child("Loading…"),
            PreviewKind::Binary => body
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .p_4()
                .text_color(theme::fg_muted())
                .child("Binary file — can't display as text.")
                .child(
                    div()
                        .font_family("monospace")
                        .child(preview.path.clone()),
                ),
            PreviewKind::Error { message } => body
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .p_4()
                .text_color(theme::fg_muted())
                .child(message.clone()),
            PreviewKind::Image { image, .. } => body
                .flex()
                .items_center()
                .justify_center()
                .p_2()
                .child(
                    img(gpui::ImageSource::Render(image.clone()))
                        .object_fit(ObjectFit::Contain)
                        .max_w_full()
                        .max_h_full(),
                ),
            PreviewKind::Text {
                contents,
                markdown,
                ..
            } => {
                if *markdown {
                    body.p_3()
                        .children(crate::markdown::render_markdown(contents))
                } else {
                    body.p_2().child(
                        div()
                            .font_family("monospace")
                            .text_color(theme::fg())
                            .child(SharedString::from(contents.clone())),
                    )
                }
            }
        })
    }
}
