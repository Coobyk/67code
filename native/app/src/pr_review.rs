//! Unified-diff review pane — the right-panel `Tab::Diff` surface.
//!
//! Feeds on `review.getDiffPreview` (local git — no forge credential needed):
//! one call returns `sources[]` for the working tree ("Uncommitted") and the
//! branch range ("Changes vs <base>"), each with a `files[]` numstat list and
//! the raw unified `diff` patch. The pane parses the patch into per-file
//! sections and renders mono, line-colored (+ green / − red / @@ accent)
//! rows inside a scrollable list; file headers collapse on click.
//!
//! Entry points live in vcs.rs: "Review changes" on the branch status card
//! and "Files changed" on an expanded pull-request row. The wire carries no
//! per-PR file list (`pullRequests.diff` is an HTTP endpoint this server
//! does not implement), so a PR's sheet is the local branch-range comparison
//! — honest when the checked-out branch is the PR head, labeled otherwise.

use std::collections::BTreeSet;
use std::ops::Range;

use gpui::{
    Context, Div, FontWeight, HighlightStyle, MouseButton, SharedString, StyledText, div,
    prelude::*, px, rgb,
};
use serde_json::{Value, json};

use crate::right_panel::Tab;
use crate::{AppView, theme};

/// Cap the parsed patch so a huge diff cannot stall the renderer.
const MAX_FILES: usize = 500;
const MAX_LINES: usize = 60_000;

fn add_color() -> gpui::Rgba {
    rgb(0x3fb950)
}
fn del_color() -> gpui::Rgba {
    rgb(0xf85149)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Meta,
    Hunk,
    Add,
    Del,
}

impl LineKind {
    fn color(self) -> gpui::Rgba {
        match self {
            LineKind::Meta => theme::fg_muted(),
            LineKind::Hunk => theme::primary(),
            LineKind::Add => add_color(),
            LineKind::Del => del_color(),
        }
    }
}

/// One `diff --git` section of a patch: rendered mono text + byte-range
/// highlight classes (resolved to colors at render so theme swaps repaint).
pub(crate) struct DiffFile {
    /// Display path — the new path, or the old path for deletions.
    path: String,
    /// Set when a rename/copy left a different `---` path.
    old_path: Option<String>,
    adds: usize,
    dels: usize,
    binary: bool,
    /// Patch body text (everything after the `diff --git` line).
    text: String,
    spans: Vec<(Range<usize>, LineKind)>,
}

/// Split a unified patch into per-file sections.
fn parse_patch(patch: &str) -> (Vec<DiffFile>, bool) {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut cur: Option<DiffFile> = None;
    let mut old_path: Option<String> = None;
    let mut truncated = false;

    let strip = |p: &str| -> String {
        let p = p.trim_end_matches('\t');
        // `a/`/`b/` prefixes and quoted paths (`"a/x y"`).
        let p = p.trim_matches('"');
        p.strip_prefix("a/")
            .or_else(|| p.strip_prefix("b/"))
            .unwrap_or(p)
            .to_string()
    };

    for line in patch.lines() {
        if files.len() + usize::from(cur.is_some()) >= MAX_FILES || truncated {
            truncated = true;
            break;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(f) = cur.take() {
                files.push(f);
            }
            // `diff --git a/old b/new` (paths may be quoted).
            let mut parts = rest.split(' ');
            let a = strip(parts.next().unwrap_or(""));
            let b = strip(parts.next().unwrap_or(""));
            cur = Some(DiffFile {
                path: b.clone(),
                old_path: (a != b).then_some(a),
                adds: 0,
                dels: 0,
                binary: false,
                text: String::new(),
                spans: Vec::new(),
            });
            old_path = None;
            continue;
        }
        let Some(f) = cur.as_mut() else {
            continue; // preamble before the first file header
        };
        if f.spans.len() + files.iter().map(|g| g.spans.len()).sum::<usize>() >= MAX_LINES {
            truncated = true;
            break;
        }

        let start = f.text.len();
        f.text.push_str(line);
        f.text.push('\n');
        let range = start..f.text.len();

        let kind = if line.starts_with("@@") {
            LineKind::Hunk
        } else if let Some(rest) = line.strip_prefix("--- ") {
            old_path = (rest != "/dev/null").then(|| strip(rest));
            LineKind::Meta
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            if rest == "/dev/null" {
                if let Some(old) = old_path.clone() {
                    f.path = old;
                }
            } else {
                f.path = strip(rest);
            }
            LineKind::Meta
        } else if line.starts_with('+') {
            f.adds += 1;
            LineKind::Add
        } else if line.starts_with('-') {
            f.dels += 1;
            LineKind::Del
        } else if line.starts_with("Binary files") || line.starts_with("GIT binary patch") {
            f.binary = true;
            LineKind::Meta
        } else if line.starts_with("index ")
            || line.ends_with(" mode")
            || line.starts_with("similarity index")
            || line.starts_with("dissimilarity index")
            || line.starts_with("rename from")
            || line.starts_with("rename to")
            || line.starts_with("copy from")
            || line.starts_with("copy to")
            || line.starts_with('\\')
        {
            LineKind::Meta
        } else {
            // Context lines (` ` prefix); the span covers the whole line so
            // colors for add/del apply to content only? Keep whole-line
            // ranges — context gets no span at all.
            continue;
        };
        // Merge with the previous span when the kind repeats (fewer ranges).
        if let Some(last) = f.spans.last_mut() {
            if last.1 == kind && last.0.end == range.start {
                last.0.end = range.end;
                continue;
            }
        }
        f.spans.push((range, kind));
    }
    if let Some(f) = cur.take() {
        files.push(f);
    }
    (files, truncated)
}

/// One `review.getDiffPreview` source (working-tree / branch-range / commit).
pub(crate) struct DiffSource {
    id: String,
    title: String,
    base_ref: String,
    /// `files[]` numstat rows (path, additions, deletions) — survives even
    /// when `patchesDeferred` left `diff` empty.
    stats: Vec<(String, i64, i64)>,
    files: Vec<DiffFile>,
    /// Server truncated or we capped the parse.
    truncated: bool,
    /// `patchesDeferred` — stats but no patch text.
    patch_deferred: bool,
}

#[derive(Default)]
pub(crate) struct DiffState {
    /// Header label — "Local changes" or "PR #n · owner/repo".
    pub title: String,
    /// Optional caveat line (e.g. local checkout isn't the PR head).
    pub note: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    pub sources: Vec<DiffSource>,
    pub selected: usize,
    /// Display paths the user collapsed.
    pub collapsed: BTreeSet<String>,
    /// Payload for the ↻ refresh (`review.getDiffPreview`).
    request: Option<Value>,
    /// Replies stamped with an older generation are dropped.
    pub generation: u64,
}

impl DiffState {
    fn reset(&mut self, title: String, note: Option<String>, request: Value) {
        *self = DiffState {
            title,
            note,
            loading: true,
            request: Some(request),
            generation: self.generation.wrapping_add(1),
            ..Default::default()
        };
    }
}

impl AppView {
    /// Fire `review.getDiffPreview` and switch the right panel to `Tab::Diff`.
    /// `prefer` picks the initially selected source id (e.g. "branch-range").
    pub(crate) fn open_review_diff(
        &mut self,
        title: String,
        request: Value,
        prefer: Option<&str>,
        note: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.vcs_state.diff.reset(title, note, request.clone());
        let generation = self.vcs_state.diff.generation;
        let prefer = prefer.map(str::to_owned);
        let Some(rx) = self.send_query("review.getDiffPreview", request) else {
            self.vcs_state.diff.loading = false;
            self.vcs_state.diff.error = Some("Not connected to a server.".to_string());
            self.panel_state.tab = Some(Tab::Diff);
            cx.notify();
            return;
        };
        self.panel_state.tab = Some(Tab::Diff);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let exit = rx
                .recv()
                .await
                .unwrap_or_else(|_| json!({"_tag": "Failure"}));
            let _ = this.update(cx, |this, cx| {
                let s = &mut this.vcs_state.diff;
                if s.generation != generation {
                    return;
                }
                s.loading = false;
                if exit["_tag"].as_str() != Some("Success") {
                    s.error = Some(super::failure_message(&exit));
                    cx.notify();
                    return;
                }
                let sources = exit["value"]["sources"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                s.sources = sources
                    .iter()
                    .map(|src| {
                        let patch = src["diff"].as_str().unwrap_or("");
                        let (files, mut truncated) = parse_patch(patch);
                        truncated |= src["truncated"].as_bool() == Some(true);
                        let stats = src["files"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .map(|f| {
                                        (
                                            f["path"].as_str().unwrap_or("").to_string(),
                                            f["additions"].as_i64().unwrap_or(0),
                                            f["deletions"].as_i64().unwrap_or(0),
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        DiffSource {
                            id: src["id"].as_str().unwrap_or("").to_string(),
                            title: src["title"].as_str().unwrap_or("Changes").to_string(),
                            base_ref: src["baseRef"].as_str().unwrap_or("").to_string(),
                            stats,
                            files,
                            truncated,
                            patch_deferred: src["patchesDeferred"].as_bool() == Some(true),
                        }
                    })
                    .collect();
                s.selected = prefer
                    .as_deref()
                    .and_then(|id| s.sources.iter().position(|src| src.id == id))
                    .unwrap_or(0);
                // Fall forward to the first source that actually has files.
                if s.sources
                    .get(s.selected)
                    .is_some_and(|src| src.stats.is_empty())
                {
                    if let Some(ix) = s.sources.iter().position(|src| !src.stats.is_empty()) {
                        s.selected = ix;
                    }
                }
                s.error = None;
                cx.notify();
            });
        })
        .detach();
    }

    /// ↻ — resend the stored `review.getDiffPreview` request.
    fn diff_reload(&mut self, cx: &mut Context<Self>) {
        let Some(request) = self.vcs_state.diff.request.clone() else {
            return;
        };
        let title = self.vcs_state.diff.title.clone();
        let note = self.vcs_state.diff.note.clone();
        let prefer = self
            .vcs_state
            .diff
            .sources
            .get(self.vcs_state.diff.selected)
            .map(|s| s.id.clone());
        self.open_review_diff(title, request, prefer.as_deref(), note, cx);
    }

    /// Right-panel `Tab::Diff` body.
    pub(crate) fn render_diff_pane(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.vcs_state.diff;
        let mut pane = div().flex().flex_col().flex_1().min_h_0();

        // Header: back to PRs + title + refresh.
        let mut head = div()
            .flex()
            .items_center()
            .gap_1p5()
            .px_2()
            .py_1p5()
            .border_b_1()
            .border_color(theme::border());
        head = head.child(
            div()
                .id("diff-back")
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .cursor_pointer()
                .text_xs()
                .text_color(theme::fg_muted())
                .hover(|h| h.bg(theme::hover()))
                .child("← PRs")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _window, cx| {
                        this.panel_state.tab = Some(Tab::PullRequests);
                        cx.notify();
                    }),
                ),
        );
        head = head.child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .child(if s.title.is_empty() {
                    SharedString::from("Files changed")
                } else {
                    SharedString::from(s.title.clone())
                }),
        );
        if s.request.is_some() {
            head = head.child(
                div()
                    .id("diff-refresh")
                    .px_1p5()
                    .py_0p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .hover(|h| h.bg(theme::hover()))
                    .child("↻")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            this.diff_reload(cx);
                        }),
                    ),
            );
        }
        pane = pane.child(head);

        if let Some(note) = &s.note {
            pane = pane.child(
                div()
                    .px_3()
                    .py_1p5()
                    .text_xs()
                    .text_color(rgb(0xd29922))
                    .border_b_1()
                    .border_color(theme::border())
                    .child(format!("⚠ {note}")),
            );
        }

        if s.loading && s.sources.is_empty() {
            return pane.child(super::muted_status_row("Loading diff…"));
        }
        if let Some(err) = &s.error {
            return pane.child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(del_color())
                    .child(err.clone()),
            );
        }
        if s.sources.is_empty() {
            return pane.child(super::muted_status_row(if s.title.is_empty() {
                "Open a diff from the pull-requests pane."
            } else {
                "No changes."
            }));
        }

        // Source chips ("Uncommitted" / "Changes vs origin/main").
        if s.sources.len() > 1 {
            let mut chips = div()
                .flex()
                .flex_wrap()
                .gap_1()
                .px_3()
                .py_1p5()
                .border_b_1()
                .border_color(theme::border());
            for (ix, src) in s.sources.iter().enumerate() {
                let active = ix == s.selected;
                chips = chips.child(
                    div()
                        .id(("diff-source", ix))
                        .px_1p5()
                        .py_0p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .text_xs()
                        .text_color(if active {
                            theme::fg()
                        } else {
                            theme::fg_muted()
                        })
                        .bg(if active {
                            theme::selected()
                        } else {
                            gpui::rgba(0x00000000)
                        })
                        .hover(|h| h.bg(theme::hover()))
                        .child(src.title.clone())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.vcs_state.diff.selected = ix;
                                cx.notify();
                            }),
                        ),
                );
            }
            pane = pane.child(chips);
        }

        let Some(src) = s.sources.get(s.selected) else {
            return pane.child(super::muted_status_row("No changes."));
        };

        if !src.base_ref.is_empty() {
            pane = pane.child(
                div()
                    .px_3()
                    .pb_1()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(format!("base: {}", src.base_ref)),
            );
        }

        let mut list = div()
            .id("diff-file-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pb_2();

        if src.stats.is_empty() && src.files.is_empty() {
            list = list.child(super::muted_status_row("No changes in this source."));
        }
        if src.patch_deferred && src.files.is_empty() && !src.stats.is_empty() {
            // Metadata-only answer: file stats without patch bodies.
            for (path, adds, dels) in &src.stats {
                list = list.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .px_3()
                        .py_0p5()
                        .text_xs()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family("monospace")
                                .child(path.clone()),
                        )
                        .child(div().text_color(add_color()).child(format!("+{adds}")))
                        .child(div().text_color(del_color()).child(format!("−{dels}"))),
                );
            }
        }

        for (ix, file) in src.files.iter().enumerate() {
            let collapsed = s.collapsed.contains(&file.path);
            let path_label = match &file.old_path {
                Some(old) if old != &file.path => format!("{old} → {}", file.path),
                _ => file.path.clone(),
            };
            let mut header = div()
                .id(("diff-file", ix))
                .flex()
                .items_center()
                .gap_1p5()
                .px_2()
                .py_1()
                .cursor_pointer()
                .bg(theme::canvas())
                .hover(|h| h.bg(theme::hover()))
                .child(
                    div()
                        .w(px(12.))
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child(if collapsed { "▸" } else { "▾" }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family("monospace")
                        .text_xs()
                        .child(path_label),
                );
            if file.binary {
                header = header.child(
                    div()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child("binary"),
                );
            }
            header = header
                .child(
                    div()
                        .text_xs()
                        .text_color(add_color())
                        .child(format!("+{}", file.adds)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(del_color())
                        .child(format!("−{}", file.dels)),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        let path = this
                            .vcs_state
                            .diff
                            .sources
                            .get(this.vcs_state.diff.selected)
                            .and_then(|src| src.files.get(ix))
                            .map(|f| f.path.clone());
                        if let Some(path) = path {
                            if !this.vcs_state.diff.collapsed.remove(&path) {
                                this.vcs_state.diff.collapsed.insert(path);
                            }
                            cx.notify();
                        }
                    }),
                );
            list = list.child(header);

            if collapsed {
                continue;
            }
            if file.text.is_empty() {
                continue;
            }
            let highlights: Vec<(Range<usize>, HighlightStyle)> = file
                .spans
                .iter()
                .map(|(range, kind)| {
                    (
                        range.clone(),
                        HighlightStyle {
                            color: Some(kind.color().into()),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            let mut text = StyledText::new(file.text.clone());
            if !highlights.is_empty() {
                text = text.with_highlights(highlights);
            }
            list = list.child(
                div()
                    .id(("diff-body", ix))
                    .w_full()
                    .overflow_x_scroll()
                    .px_2()
                    .pb_1()
                    .font_family("monospace")
                    .text_xs()
                    .whitespace_nowrap()
                    .text_color(theme::fg_muted())
                    .child(text),
            );
        }
        if src.truncated {
            list = list.child(
                div()
                    .px_3()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("… diff truncated"),
            );
        }
        pane.child(list)
    }
}
