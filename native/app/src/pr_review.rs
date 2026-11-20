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

use gpui::{Context, Div, FontWeight, MouseButton, SharedString, div, prelude::*, px};
use serde_json::{Value, json};

use crate::icons::{Icon, icon};
use crate::right_panel::Tab;
use crate::{AppView, theme};

/// Cap the parsed patch so a huge diff cannot stall the renderer.
const MAX_FILES: usize = 500;
const MAX_LINES: usize = 60_000;
/// Per-file render cap — each line is a small div tree now, so very long
/// files render their head plus a truncation notice.
const RENDER_LINE_CAP: usize = 4_000;
/// Uniform diff row height (13px mono on a ~1.45 line-height, like the web
/// pierre surface).
const DIFF_ROW_H: f32 = 19.;
/// Monospace cell advance at the 13px code font (gutter + content widths).
const DIFF_CHAR_W: f32 = 7.8;

/// `text-diff-addition` — "+N" counters (`--success`).
fn add_count() -> gpui::Rgba {
    theme::diff_add_count()
}
/// `text-diff-deletion` — "−N" counters (`--destructive`).
fn del_count() -> gpui::Rgba {
    theme::diff_del_count()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    /// File-header/meta lines (`index`, `---`, `+++`, `similarity`, `\`).
    Meta,
    /// `@@ …` hunk separator.
    Hunk,
    Add,
    Del,
    /// Unchanged line inside a hunk.
    Context,
}

/// One rendered row inside a `diff --git` section: the patch line without
/// its trailing '\n', plus old/new gutter numbers for hunk lines.
struct DiffLine {
    kind: LineKind,
    old_no: Option<u32>,
    new_no: Option<u32>,
    text: String,
}

/// One `diff --git` section of a patch: parsed display rows colored with
/// the web's `--diffs-*` tokens at render time.
pub(crate) struct DiffFile {
    /// Display path — the new path, or the old path for deletions.
    path: String,
    /// Set when a rename/copy left a different `---` path.
    old_path: Option<String>,
    adds: usize,
    dels: usize,
    binary: bool,
    lines: Vec<DiffLine>,
    /// Largest gutter number seen — sizes the number columns.
    max_lineno: u32,
}

/// `@@ -a[,b] +c[,d] @@` → `(a, c)` — seeds the gutter counters.
fn hunk_starts(line: &str) -> Option<(u32, u32)> {
    let body = line.strip_prefix("@@")?;
    let scan_num = |s: &str, sigil: char| -> Option<u32> {
        let tail = s.split(sigil).nth(1)?;
        tail.split([',', ' '])
            .next()?
            .trim_start_matches('@')
            .parse()
            .ok()
    };
    let old = scan_num(body, '-')?;
    let new = scan_num(body, '+')?;
    Some((old, new))
}

/// One numbered gutter cell (old or new side).
fn gutter_cell(no: Option<u32>, width: f32, bg: gpui::Rgba, fg: gpui::Rgba) -> Div {
    div()
        .w(px(width))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_end()
        .pr_2()
        .bg(bg)
        .text_color(fg)
        .child(match no {
            Some(n) => n.to_string(),
            None => String::new(),
        })
}

/// One rendered diff line: hunk/meta rows span the row width; hunk content
/// lines get old+new number lanes plus the tinted content cell — mirroring
/// the web pierre rows (`--diffs-bg-*` fills, `--diffs-fg-number` digits).
fn diff_row(line: &DiffLine, gutter_w: f32, row_w: f32) -> Div {
    match line.kind {
        LineKind::Meta => div()
            .h(px(DIFF_ROW_H))
            .w(px(row_w))
            .flex()
            .items_center()
            .px_2()
            .text_color(theme::fg_muted())
            .child(SharedString::from(line.text.clone())),
        LineKind::Hunk => div()
            .h(px(DIFF_ROW_H))
            .w(px(row_w))
            .flex()
            .items_center()
            .px_2()
            .bg(theme::diff_hunk_bg())
            .text_color(theme::fg_muted())
            .child(SharedString::from(line.text.clone())),
        LineKind::Context | LineKind::Add | LineKind::Del => {
            let (line_bg, num_bg, num_fg, line_fg) = match line.kind {
                LineKind::Add => (
                    theme::diff_add_bg(),
                    theme::diff_add_num_bg(),
                    add_count(),
                    theme::diff_add_fg(),
                ),
                LineKind::Del => (
                    theme::diff_del_bg(),
                    theme::diff_del_num_bg(),
                    del_count(),
                    theme::diff_del_fg(),
                ),
                _ => (
                    theme::diff_context_bg(),
                    theme::diff_context_bg(),
                    theme::diff_gutter_fg(),
                    theme::fg(),
                ),
            };
            div()
                .h(px(DIFF_ROW_H))
                .w(px(row_w))
                .flex()
                .whitespace_nowrap()
                .child(gutter_cell(line.old_no, gutter_w, num_bg, num_fg))
                .child(gutter_cell(line.new_no, gutter_w, num_bg, num_fg))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .pl_2()
                        .flex()
                        .items_center()
                        .bg(line_bg)
                        .text_color(line_fg)
                        .child(SharedString::from(line.text.clone())),
                )
        }
    }
}

/// Append a row, keeping `max_lineno` current for gutter sizing.
fn push_line(
    f: &mut DiffFile,
    kind: LineKind,
    old_no: Option<u32>,
    new_no: Option<u32>,
    text: &str,
) {
    let hi = old_no.unwrap_or(0).max(new_no.unwrap_or(0));
    f.max_lineno = f.max_lineno.max(hi);
    f.lines.push(DiffLine {
        kind,
        old_no,
        new_no,
        text: text.to_string(),
    });
}

/// Split a unified patch into per-file sections, tracking `@@` counters so
/// each hunk line carries old/new gutter numbers like the web diff.
fn parse_patch(patch: &str) -> (Vec<DiffFile>, bool) {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut cur: Option<DiffFile> = None;
    let mut old_path: Option<String> = None;
    let mut truncated = false;
    // Line counters inside the current hunk region of `cur`.
    let mut in_hunk = false;
    let mut old_ln = 0u32;
    let mut new_ln = 0u32;
    let mut total_lines = 0usize;

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
                lines: Vec::new(),
                max_lineno: 0,
            });
            old_path = None;
            in_hunk = false;
            continue;
        }
        let Some(f) = cur.as_mut() else {
            continue; // preamble before the first file header
        };
        if total_lines >= MAX_LINES {
            truncated = true;
            break;
        }
        total_lines += 1;

        if let Some(starts) = hunk_starts(line) {
            (old_ln, new_ln) = starts;
            in_hunk = true;
            push_line(f, LineKind::Hunk, None, None, line);
            continue;
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            old_path = (rest != "/dev/null").then(|| strip(rest));
            push_line(f, LineKind::Meta, None, None, line);
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            if rest == "/dev/null" {
                if let Some(old) = old_path.clone() {
                    f.path = old;
                }
            } else {
                f.path = strip(rest);
            }
            push_line(f, LineKind::Meta, None, None, line);
            continue;
        }
        if line.starts_with("Binary files") || line.starts_with("GIT binary patch") {
            f.binary = true;
            push_line(f, LineKind::Meta, None, None, line);
            continue;
        }
        if line.starts_with("index ")
            || line.ends_with(" mode")
            || line.starts_with("similarity index")
            || line.starts_with("dissimilarity index")
            || line.starts_with("rename from")
            || line.starts_with("rename to")
            || line.starts_with("copy from")
            || line.starts_with("copy to")
            || line.starts_with('\\')
        {
            push_line(f, LineKind::Meta, None, None, line);
            continue;
        }
        if !in_hunk {
            // Stray header text (e.g. `new file mode` variants not matched
            // above) — keep it as muted meta rather than dropping it.
            push_line(f, LineKind::Meta, None, None, line);
            continue;
        }
        let (kind, old_no, new_no) = match line.as_bytes().first() {
            Some(b'+') => {
                f.adds += 1;
                let n = new_ln;
                new_ln += 1;
                (LineKind::Add, None, Some(n))
            }
            Some(b'-') => {
                f.dels += 1;
                let n = old_ln;
                old_ln += 1;
                (LineKind::Del, Some(n), None)
            }
            _ => {
                let (o, n) = (old_ln, new_ln);
                old_ln += 1;
                new_ln += 1;
                (LineKind::Context, Some(o), Some(n))
            }
        };
        // The web shows no `+`/`-`/` ` sign column — the row tint and the
        // number lanes carry that information instead.
        push_line(f, kind, old_no, new_no, &line[1.min(line.len())..]);
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
    /// Payload for the refresh action (`review.getDiffPreview`).
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

    /// Resend the stored `review.getDiffPreview` request.
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
        // The web diff surface is `--code-background` + `--code-foreground`
        // mono text, not the plain canvas.
        let mut pane = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .bg(theme::code_bg());

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
                .flex()
                .items_center()
                .gap_1()
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .cursor_pointer()
                .text_xs()
                .text_color(theme::fg_muted())
                .hover(|h| h.bg(theme::hover()).text_color(theme::fg()))
                .child(icon(Icon::ArrowLeft, 11.).text_color(theme::fg_muted()))
                .child("PRs")
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
                    .w(px(22.))
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .hover(|h| h.bg(theme::hover()).text_color(theme::fg()))
                    .child(icon(Icon::RefreshCw, 12.).text_color(theme::fg_muted()))
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
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_3()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::warning_fg())
                    .border_b_1()
                    .border_color(theme::border())
                    .child(icon(Icon::TriangleAlert, 12.).text_color(theme::warning_fg()))
                    .child(SharedString::from(note.clone())),
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
                    .text_color(theme::error())
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
                            theme::accent()
                        } else {
                            gpui::rgba(0x00000000)
                        })
                        .hover(|h| h.bg(theme::accent_soft()))
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
                                .font_family(theme::FONT_MONO)
                                .child(path.clone()),
                        )
                        .child(div().text_color(add_count()).child(format!("+{adds}")))
                        .child(div().text_color(del_count()).child(format!("−{dels}"))),
                );
            }
        }

        for (ix, file) in src.files.iter().enumerate() {
            let collapsed = s.collapsed.contains(&file.path);
            let path_label = match &file.old_path {
                Some(old) if old != &file.path => format!("{old} → {}", file.path),
                _ => file.path.clone(),
            };
            // Web pierre file header: 32px sticky bar on `--code-background`
            // with a bottom hairline, mono path, semantic +/- counts.
            let mut header = div()
                .id(("diff-file", ix))
                .flex()
                .items_center()
                .gap_1p5()
                .h(px(32.))
                .px_2()
                .cursor_pointer()
                .border_b_1()
                .border_color(theme::border())
                .hover(|h| h.bg(theme::accent_soft()))
                .child(
                    div().w(px(12.)).flex().items_center().child(
                        if collapsed {
                            icon(Icon::ChevronRight, 10.)
                        } else {
                            icon(Icon::ChevronDown, 10.)
                        }
                        .text_color(theme::fg_muted()),
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(theme::FONT_MONO)
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
                        .text_color(add_count())
                        .child(format!("+{}", file.adds)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(del_count())
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
            if file.lines.is_empty() {
                continue;
            }
            // Old/new number columns sized to the widest line number
            // (like pierre's `--diffs-column-number-width`); rows are all
            // the same width so the per-line tints read as full-width bars.
            let digits = file.max_lineno.max(1).to_string().len().max(2) as f32;
            let gutter_w = digits * DIFF_CHAR_W + 16.;
            let max_chars = file
                .lines
                .iter()
                .map(|l| l.text.chars().count())
                .max()
                .unwrap_or(0) as f32;
            let row_w = 2. * gutter_w + 8. + max_chars * DIFF_CHAR_W;
            let mut rows = div()
                .flex()
                .flex_col()
                .w(px(row_w))
                .font_family(theme::FONT_MONO)
                .text_size(theme::code_text_size())
                .whitespace_nowrap();
            let total = file.lines.len();
            for line in file.lines.iter().take(RENDER_LINE_CAP) {
                rows = rows.child(diff_row(line, gutter_w, row_w));
            }
            if total > RENDER_LINE_CAP {
                rows = rows.child(
                    div()
                        .h(px(DIFF_ROW_H))
                        .w(px(row_w))
                        .flex()
                        .items_center()
                        .px_2()
                        .text_color(theme::fg_muted())
                        .child(format!("… {} more lines", total - RENDER_LINE_CAP)),
                );
            }
            list = list.child(
                div()
                    .id(("diff-body", ix))
                    .w_full()
                    .overflow_x_scroll()
                    .pb_1()
                    .child(rows),
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
