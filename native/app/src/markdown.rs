//! Markdown rendering for assistant messages — headings, bold/italic,
//! inline code, fenced code blocks with syntax highlighting, lists, tables,
//! links, blockquotes. Self-contained; used by the timeline.
//!
//! `render_markdown` returns block-level elements. Inline formatting is
//! rendered with `StyledText` runs; links use `InteractiveText` so clicking a
//! `[label](url)` span opens the URL via `App::open_url` — no entity or
//! listener wiring is needed at the call site.

use std::ops::Range;

use gpui::{
    AnyElement, ElementId, FontStyle, FontWeight, HighlightStyle, Hsla, InteractiveText,
    SharedString, StrikethroughStyle, StyledText, TextAlign, div, prelude::*, px, rems, rgb,
};

use crate::theme;

/// Render a markdown document into block-level elements.
///
/// Contract (keep stable — timeline.rs depends on it):
/// - headings render larger/bold
/// - `inline code` renders mono with a subtle background
/// - fenced ```lang blocks render mono in a raised, bordered card with
///   simple syntax highlighting (keywords/strings/comments/numbers)
/// - lists render with markers and indentation
/// - links render accent-colored/underlined and open the URL on click
/// - tables render as a simple grid
/// - blockquotes render with a left accent border
pub fn render_markdown(text: &str) -> Vec<AnyElement> {
    let blocks = parse_blocks(text);
    let mut ctx = RenderCtx {
        seed: fnv1a(text),
        count: 0,
    };
    render_blocks(&blocks, &mut ctx)
}

/// Render a fenced code block (used both standalone and inside markdown).
///
/// Mirrors the web `MarkdownCodeBlock`: `rounded-lg` card on
/// `--code-background` with a `--contrast-border` hairline, a mono `text-2xs`
/// language header at 72% code-foreground (pt-1.5 pr-1.5 pl-3, no divider),
/// and the `pre` body at `--font-size-code` (13px) with `leading-snug` and
/// `.chat-markdown pre` padding (0.8rem × 0.9rem).
pub fn render_code_block(code: &str, language: Option<&str>) -> AnyElement {
    let code = code.trim_end_matches('\n');
    let mut card = div()
        .w_full()
        .flex()
        .flex_col()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::code_bg())
        .overflow_hidden();

    if let Some(lang) = language.filter(|l| !l.trim().is_empty()) {
        card = card.child(
            div()
                .flex()
                .w_full()
                .pt(px(6.))
                .pr(px(6.))
                .pl_3()
                .font_family(theme::FONT_MONO)
                .text_size(theme::text_size::XS_2)
                .text_color(theme::code_header_fg())
                .child(lang.to_string()),
        );
    }

    let mut text = StyledText::new(code.to_string());
    let highlights = highlight_code(code, language.unwrap_or(""));
    if !highlights.is_empty() {
        text = text.with_highlights(highlights);
    }

    card.child(
        div()
            .id(("c67-md-code", fnv1a(code) as usize))
            .w_full()
            .overflow_x_scroll()
            .px(px(14.4))
            .py(px(12.8))
            .font_family(theme::FONT_MONO)
            .text_size(theme::code_text_size())
            .line_height(px(18.))
            .whitespace_nowrap()
            .text_color(theme::fg())
            .child(text),
    )
    .into_any_element()
}

// ---------------------------------------------------------------------------
// Render context — hands out stable, unique ElementIds for interactive text.
// ---------------------------------------------------------------------------

struct RenderCtx {
    seed: u64,
    count: usize,
}

impl RenderCtx {
    fn next_id(&mut self) -> ElementId {
        self.count += 1;
        ElementId::named_usize("c67-md-link", (self.seed as usize) ^ self.count)
    }
}

fn fnv1a(s: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

// ---------------------------------------------------------------------------
// Block-level parsing
// ---------------------------------------------------------------------------

enum Block {
    Heading {
        level: usize,
        text: String,
    },
    Paragraph(String),
    Code {
        language: Option<String>,
        code: String,
    },
    Quote(Vec<Block>),
    List(Vec<ListItem>),
    Table(Table),
    Rule,
}

struct ListItem {
    depth: usize,
    marker: ListMarker,
    text: String,
}

enum ListMarker {
    Bullet,
    Number(u64),
    Task(bool),
}

struct Table {
    header: Vec<String>,
    aligns: Vec<Align>,
    rows: Vec<Vec<String>>,
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
    Right,
}

fn parse_blocks(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }

        // Fenced code block.
        if let Some((fence, language)) = parse_fence_open(line) {
            let mut code = String::new();
            let mut j = i + 1;
            while j < lines.len() && !is_fence_close(lines[j], fence) {
                code.push_str(lines[j]);
                code.push('\n');
                j += 1;
            }
            blocks.push(Block::Code { language, code });
            i = if j < lines.len() { j + 1 } else { j };
            continue;
        }

        // ATX heading.
        if let Some((level, rest)) = parse_heading(line) {
            blocks.push(Block::Heading {
                level,
                text: rest.to_string(),
            });
            i += 1;
            continue;
        }

        // Horizontal rule.
        if is_rule(line) {
            blocks.push(Block::Rule);
            i += 1;
            continue;
        }

        // Blockquote — consecutive `>` lines, recursively parsed.
        if line.trim_start().starts_with('>') {
            let mut inner = String::new();
            while i < lines.len() && lines[i].trim_start().starts_with('>') {
                let stripped = lines[i].trim_start();
                let stripped = stripped.strip_prefix('>').unwrap_or(stripped);
                let stripped = stripped.strip_prefix(' ').unwrap_or(stripped);
                inner.push_str(stripped);
                inner.push('\n');
                i += 1;
            }
            blocks.push(Block::Quote(parse_blocks(&inner)));
            continue;
        }

        // Table — a row containing `|` followed by a delimiter row with the
        // same number of cells (GFM requires the counts to match).
        if i + 1 < lines.len()
            && line.contains('|')
            && let Some(aligns) = table_aligns(lines[i + 1])
        {
            let header = split_table_row(line);
            if header.len() != aligns.len() {
                // Not a table — fall through to paragraph handling below.
                let mut para = String::from(line.trim());
                i += 1;
                while i < lines.len() && !lines[i].trim().is_empty() {
                    if starts_block_construct(&lines, i) {
                        break;
                    }
                    para.push(' ');
                    para.push_str(lines[i].trim());
                    i += 1;
                }
                blocks.push(Block::Paragraph(para));
                continue;
            }
            i += 2;
            let mut rows = Vec::new();
            while i < lines.len() && !lines[i].trim().is_empty() && lines[i].contains('|') {
                rows.push(split_table_row(lines[i]));
                i += 1;
            }
            blocks.push(Block::Table(Table {
                header,
                aligns,
                rows,
            }));
            continue;
        }

        // List item — collect the whole run of consecutive list lines.
        if let Some(item) = parse_list_item(line) {
            let mut items = vec![item];
            i += 1;
            while i < lines.len() {
                if let Some(it) = parse_list_item(lines[i]) {
                    items.push(it);
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(Block::List(items));
            continue;
        }

        // Paragraph — gather until a blank line or a line that starts a
        // non-paragraph construct.
        let mut para = String::new();
        while i < lines.len() && !lines[i].trim().is_empty() {
            let l = lines[i];
            if !para.is_empty() && starts_block_construct(&lines, i) {
                break;
            }
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(l.trim());
            i += 1;
        }
        if !para.is_empty() {
            blocks.push(Block::Paragraph(para));
        }
    }

    blocks
}

/// Does `lines[i]` begin a construct that should break a paragraph?
fn starts_block_construct(lines: &[&str], i: usize) -> bool {
    let l = lines[i];
    parse_fence_open(l).is_some()
        || parse_heading(l).is_some()
        || is_rule(l)
        || l.trim_start().starts_with('>')
        || parse_list_item(l).is_some()
        || (i + 1 < lines.len()
            && l.contains('|')
            && table_aligns(lines[i + 1])
                .is_some_and(|aligns| aligns.len() == split_table_row(l).len()))
}

fn parse_fence_open(line: &str) -> Option<(char, Option<String>)> {
    let t = line.trim_start();
    let c = t.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let n = t.chars().take_while(|&ch| ch == c).count();
    if n < 3 {
        return None;
    }
    let info = t[n..].trim();
    if c == '`' && info.contains('`') {
        return None;
    }
    let language = info
        .split_whitespace()
        .next()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Some((c, language))
}

fn is_fence_close(line: &str, fence: char) -> bool {
    let t = line.trim_start();
    let n = t.chars().take_while(|&ch| ch == fence).count();
    n >= 3 && t[n..].trim().is_empty()
}

fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let t = line.trim_start();
    let n = t.bytes().take_while(|&b| b == b'#').count();
    if n == 0 || n > 6 {
        return None;
    }
    let rest = &t[n..];
    if rest.is_empty() {
        return Some((n, ""));
    }
    let rest = rest.strip_prefix(' ')?;
    // Trim an optional closing ` ###` run (trailing #s preceded by a space).
    let mut text = rest.trim_end();
    if text.ends_with('#') {
        let head = text.trim_end_matches('#');
        if head.ends_with(' ') || head.is_empty() {
            text = head.trim_end();
        }
    }
    Some((n, text))
}

fn is_rule(line: &str) -> bool {
    let t = line.trim();
    if t.len() < 3 {
        return false;
    }
    let Some(c) = t.chars().next() else {
        return false;
    };
    if c != '-' && c != '*' && c != '_' {
        return false;
    }
    t.chars().all(|ch| ch == c || ch == ' ') && t.chars().filter(|&ch| ch == c).count() >= 3
}

fn parse_list_item(line: &str) -> Option<ListItem> {
    let indent = line.len() - line.trim_start().len();
    let depth = indent / 2;
    let t = line.trim_start();

    let (marker, rest) = if let Some(rest) = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .or_else(|| t.strip_prefix("+ "))
    {
        (ListMarker::Bullet, rest)
    } else {
        // Ordered: `1. ` / `1) ` markers (up to 9 digits).
        let digits = t.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 || digits > 9 {
            return None;
        }
        let after = &t[digits..];
        let after = after
            .strip_prefix('.')
            .or_else(|| after.strip_prefix(')'))?;
        let rest = after.strip_prefix(' ')?;
        let n = t[..digits].parse::<u64>().ok()?;
        (ListMarker::Number(n), rest)
    };

    // GitHub task list markers: `- [ ]` / `- [x]`.
    let (marker, rest) = if matches!(marker, ListMarker::Bullet) {
        if let Some(r) = rest.strip_prefix("[ ] ") {
            (ListMarker::Task(false), r)
        } else if let Some(r) = rest
            .strip_prefix("[x] ")
            .or_else(|| rest.strip_prefix("[X] "))
        {
            (ListMarker::Task(true), r)
        } else {
            (marker, rest)
        }
    } else {
        (marker, rest)
    };

    Some(ListItem {
        depth,
        marker,
        text: rest.to_string(),
    })
}

fn split_table_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            chars.next();
            cur.push('|');
        } else if c == '|' {
            cells.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

fn table_aligns(line: &str) -> Option<Vec<Align>> {
    if !line.contains('-') {
        return None;
    }
    let cells = split_table_row(line);
    if cells.is_empty() {
        return None;
    }
    let mut aligns = Vec::with_capacity(cells.len());
    for cell in cells {
        let c = cell.trim();
        if c.is_empty() {
            return None;
        }
        let left = c.starts_with(':');
        let right = c.ends_with(':');
        let core = c.trim_matches(':');
        if core.is_empty() || !core.chars().all(|ch| ch == '-') {
            return None;
        }
        aligns.push(match (left, right) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            _ => Align::Left,
        });
    }
    Some(aligns)
}

// ---------------------------------------------------------------------------
// Block-level rendering
// ---------------------------------------------------------------------------

fn render_blocks(blocks: &[Block], ctx: &mut RenderCtx) -> Vec<AnyElement> {
    blocks
        .iter()
        .enumerate()
        .map(|(i, b)| render_block(b, i == 0, ctx))
        .collect()
}

fn render_block(block: &Block, first: bool, ctx: &mut RenderCtx) -> AnyElement {
    // Blocks carry only top margin, reproducing the collapsed-margin rhythm
    // of the web CSS: every `.chat-markdown` block element has `margin:
    // 0.65rem 0`, headings `1.25rem 0 0.5rem`; adjacent vertical margins
    // collapse to their max, so a single top margin per block is equivalent.
    let gap = px(if first { 0. } else { 10.4 });
    match block {
        Block::Heading { level, text } => {
            // Mirrors apps/web chat-markdown heading scale: 600 weight,
            // line-height 1.3, h1 1.25rem, h2 1.125rem, h3 1rem, h4-h6
            // 0.875rem, h6 muted.
            let size = match level {
                1 => rems(1.25),
                2 => rems(1.125),
                3 => rems(1.0),
                _ => rems(0.875),
            };
            let px_size = match level {
                1 => 20.,
                2 => 18.,
                3 => 16.,
                _ => 14.,
            };
            div()
                .mt(px(if first { 0. } else { 20. }))
                .text_size(size)
                .line_height(px(px_size * 1.3))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if *level == 6 {
                    theme::fg_muted()
                } else {
                    theme::fg()
                })
                .child(inline_element(text, ctx))
                .into_any_element()
        }
        Block::Paragraph(text) => div()
            .mt(gap)
            .text_sm()
            .line_height(px(22.75))
            .text_color(theme::chat_fg())
            .child(inline_element(text, ctx))
            .into_any_element(),
        Block::Code { language, code } => div()
            .mt(gap)
            .child(render_code_block(code, language.as_deref()))
            .into_any_element(),
        Block::Quote(inner) => div()
            .mt(gap)
            .border_l_2()
            .border_color(theme::quote_border())
            .pl(px(12.8))
            .flex()
            .flex_col()
            .children(render_blocks(inner, ctx))
            .into_any_element(),
        Block::List(items) => render_list(items, first, ctx),
        Block::Table(table) => render_table(table, first, ctx),
        Block::Rule => div()
            .mt(gap)
            .h(px(1.))
            .w_full()
            .bg(theme::rule_color())
            .into_any_element(),
    }
}

fn bullet_glyph(depth: usize) -> &'static str {
    match depth % 3 {
        0 => "•",
        1 => "◦",
        _ => "▪",
    }
}

fn render_list(items: &[ListItem], first: bool, ctx: &mut RenderCtx) -> AnyElement {
    let mut col = div()
        .flex()
        .flex_col()
        .mt(px(if first { 0. } else { 10.4 }))
        .gap_1()
        .text_sm()
        .line_height(px(22.75))
        .text_color(theme::chat_fg());

    for item in items {
        // Marker column = the web's `--list-gutter` (1.25rem = 20px).
        let marker: AnyElement = match item.marker {
            ListMarker::Number(n) => div()
                .w(px(20.))
                .flex_none()
                .text_color(theme::fg_muted())
                .child(format!("{n}."))
                .into_any_element(),
            ListMarker::Bullet => div()
                .w(px(20.))
                .flex_none()
                .text_color(theme::fg_muted())
                .child(bullet_glyph(item.depth))
                .into_any_element(),
            ListMarker::Task(checked) => div()
                .w(px(20.))
                .flex_none()
                .flex()
                .justify_center()
                .child(
                    div()
                        .size(px(11.))
                        .mt(px(3.))
                        .rounded(px(3.))
                        .border_1()
                        .border_color(if checked {
                            theme::primary()
                        } else {
                            theme::fg_muted()
                        })
                        .when(checked, |d| {
                            d.bg(theme::primary())
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .text_size(px(9.))
                                        .text_color(theme::canvas())
                                        .child("✓"),
                                )
                        }),
                )
                .into_any_element(),
        };

        col = col.child(
            div()
                .flex()
                .pl(px(20. * item.depth as f32))
                .child(marker)
                .child(div().flex_1().child(inline_element(&item.text, ctx))),
        );
    }

    col.into_any_element()
}

fn render_table(table: &Table, first: bool, ctx: &mut RenderCtx) -> AnyElement {
    let cols = table
        .header
        .len()
        .max(table.rows.iter().map(|r| r.len()).max().unwrap_or(0))
        .max(1);

    fn cell(text: &str, ix: usize, align: Align, header: bool, ctx: &mut RenderCtx) -> AnyElement {
        let mut d = div().flex_1().px_2().py(px(4.));
        if ix > 0 {
            d = d.border_l_1().border_color(theme::border());
        }
        d = match align {
            Align::Center => d.text_align(TextAlign::Center),
            Align::Right => d.text_align(TextAlign::Right),
            Align::Left => d,
        };
        if header {
            d = d.font_weight(FontWeight::SEMIBOLD);
        }
        d.child(inline_element(text, ctx)).into_any_element()
    }

    let align_at = |ix: usize| table.aligns.get(ix).copied().unwrap_or(Align::Left);

    let mut tbl = div()
        .mt(px(if first { 0. } else { 10.4 }))
        .w_full()
        .flex()
        .flex_col()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .overflow_hidden()
        .bg(theme::canvas())
        .text_xs()
        .text_color(theme::chat_fg());

    let mut hrow = div().flex().w_full().bg(theme::raised());
    for ix in 0..cols {
        hrow = hrow.child(cell(
            table.header.get(ix).map(|s| s.as_str()).unwrap_or(""),
            ix,
            align_at(ix),
            true,
            ctx,
        ));
    }
    tbl = tbl.child(hrow);

    for row in &table.rows {
        let mut r = div()
            .flex()
            .w_full()
            .border_t_1()
            .border_color(theme::border());
        for ix in 0..cols {
            r = r.child(cell(
                row.get(ix).map(|s| s.as_str()).unwrap_or(""),
                ix,
                align_at(ix),
                false,
                ctx,
            ));
        }
        tbl = tbl.child(r);
    }

    tbl.into_any_element()
}

// ---------------------------------------------------------------------------
// Inline parsing — produces flat styled spans which become TextRuns.
// ---------------------------------------------------------------------------

const MAX_INLINE_DEPTH: u8 = 8;

#[derive(Clone, PartialEq, Default)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: Option<SharedString>,
}

fn inline_highlight(st: &InlineStyle) -> HighlightStyle {
    HighlightStyle {
        // `.chat-markdown a` — `--info-foreground`, `text-decoration: none`
        // (the dotted underline only appears on hover/focus, which styled
        // spans can't express).
        color: st.link.is_some().then(|| theme::info_fg().into()),
        font_weight: st.bold.then_some(FontWeight::BOLD),
        font_style: st.italic.then_some(FontStyle::Italic),
        // `:not(pre) > code` — fg·10% fill, 4px radius (radius/padding aren't
        // expressible on a highlight run; the bg is the visible part).
        background_color: st.code.then(|| theme::inline_code_bg().into()),
        strikethrough: st.strike.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: None,
        }),
        ..Default::default()
    }
}

/// Parse `rest` as `[label](url)`; returns (label, url, bytes consumed).
fn parse_link(rest: &str) -> Option<(String, String, usize)> {
    let rest = rest.strip_prefix('[')?;
    let close = rest.find(']')?;
    let label = &rest[..close];
    let after = &rest[close + 1..];
    let after = after.strip_prefix('(')?;
    let end = after.find(')')?;
    let raw = after[..end].trim();
    // Drop an optional `<…>` wrapper and `"title"` suffix.
    let url = raw
        .trim_start_matches('<')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches('>');
    if url.is_empty() {
        return None;
    }
    Some((
        label.to_string(),
        url.to_string(),
        1 + close + 1 + 1 + end + 1, // `[` + label + `]` + `(` + url + `)`
    ))
}

/// Find the next `_` that can close an emphasis span — one not followed by an
/// alphanumeric (keeps `foo_bar` intra-word underscores literal).
fn find_underscore_close(rest: &str) -> Option<usize> {
    let mut start = 0;
    while let Some(pos) = rest[start..].find('_') {
        let abs = start + pos;
        let next = rest[abs + 1..].chars().next();
        if !next.is_some_and(|c| c.is_alphanumeric()) {
            return Some(abs);
        }
        start = abs + 1;
    }
    None
}

/// Recursively scan `src`, appending flat (text, style) spans to `out`.
fn parse_inline(src: &str, style: &InlineStyle, depth: u8, out: &mut Vec<(String, InlineStyle)>) {
    let mut plain = String::new();
    let mut i = 0;

    macro_rules! flush {
        () => {
            if !plain.is_empty() {
                out.push((std::mem::take(&mut plain), style.clone()));
            }
        };
    }

    while i < src.len() {
        let rest = &src[i..];
        let c = rest.chars().next().unwrap();

        if !style.code && depth < MAX_INLINE_DEPTH {
            // Backslash escape: `\x` where x is ASCII punctuation is literal.
            if c == '\\'
                && let Some(n) = rest[1..].chars().next()
                && n.is_ascii_punctuation()
            {
                plain.push(n);
                i += 1 + n.len_utf8();
                continue;
            }

            // Code span: `` `code` `` (runs of 1+ backticks).
            if c == '`' {
                let ticks = rest.bytes().take_while(|&b| b == b'`').count();
                let fence = "`".repeat(ticks);
                if let Some(pos) = rest[ticks..].find(fence.as_str()) {
                    flush!();
                    let content = rest[ticks..ticks + pos].to_string();
                    out.push((
                        content,
                        InlineStyle {
                            code: true,
                            ..style.clone()
                        },
                    ));
                    i += ticks + pos + ticks;
                    continue;
                }
            }

            // Image: `![alt](src)` — render alt text plainly.
            if rest.starts_with("![")
                && let Some((label, _url, consumed)) = parse_link(&rest[1..])
            {
                flush!();
                if !label.is_empty() {
                    out.push((label, style.clone()));
                }
                i += 1 + consumed;
                continue;
            }

            // Link: `[label](url)`.
            if c == '['
                && let Some((label, url, consumed)) = parse_link(rest)
            {
                flush!();
                let mut link_style = style.clone();
                link_style.link = Some(SharedString::from(url));
                parse_inline(&label, &link_style, depth + 1, out);
                i += consumed;
                continue;
            }

            // Autolink: `<https://…>` / `<mailto:…>`.
            if c == '<'
                && let Some(end) = rest.find('>')
            {
                let inner = &rest[1..end];
                if (inner.starts_with("http://")
                    || inner.starts_with("https://")
                    || inner.starts_with("mailto:"))
                    && !inner.chars().any(|ch| ch.is_whitespace())
                {
                    flush!();
                    let mut link_style = style.clone();
                    link_style.link = Some(SharedString::from(inner.to_string()));
                    out.push((inner.to_string(), link_style));
                    i += end + 1;
                    continue;
                }
            }

            let at_word_boundary = i == 0
                || !src[..i]
                    .chars()
                    .next_back()
                    .is_some_and(|p| p.is_alphanumeric());
            let opens = |marker_len: usize| -> bool {
                rest.len() > marker_len
                    && !rest[marker_len..]
                        .chars()
                        .next()
                        .is_some_and(|ch| ch.is_whitespace())
            };

            // Bold: `**x**` / `__x__`.
            let mut consumed = false;
            for marker in ["**", "__"] {
                if rest.starts_with(marker)
                    && (marker == "**" || at_word_boundary)
                    && opens(2)
                    && let Some(pos) = rest[2..].find(marker)
                {
                    flush!();
                    let mut s = style.clone();
                    s.bold = true;
                    parse_inline(&rest[2..2 + pos], &s, depth + 1, out);
                    i += 2 + pos + 2;
                    consumed = true;
                    break;
                }
            }
            if consumed {
                continue;
            }

            // Strikethrough: `~~x~~`.
            if rest.starts_with("~~")
                && opens(2)
                && let Some(pos) = rest[2..].find("~~")
            {
                flush!();
                let mut s = style.clone();
                s.strike = true;
                parse_inline(&rest[2..2 + pos], &s, depth + 1, out);
                i += 2 + pos + 2;
                continue;
            }

            // Italic: `*x*` / `_x_`.
            if c == '*'
                && !rest.starts_with("**")
                && opens(1)
                && let Some(pos) = rest[1..].find('*')
            {
                flush!();
                let mut s = style.clone();
                s.italic = true;
                parse_inline(&rest[1..1 + pos], &s, depth + 1, out);
                i += 1 + pos + 1;
                continue;
            }
            if c == '_'
                && !rest.starts_with("__")
                && at_word_boundary
                && opens(1)
                && let Some(pos) = find_underscore_close(&rest[1..])
            {
                flush!();
                let mut s = style.clone();
                s.italic = true;
                parse_inline(&rest[1..1 + pos], &s, depth + 1, out);
                i += 1 + pos + 1;
                continue;
            }
        }

        plain.push(c);
        i += c.len_utf8();
    }
    flush!();
}

struct InlineLayout {
    text: String,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    code_ranges: Vec<Range<usize>>,
    links: Vec<(Range<usize>, SharedString)>,
}

fn layout_inline(src: &str) -> InlineLayout {
    let mut spans = Vec::new();
    parse_inline(src, &InlineStyle::default(), 0, &mut spans);

    // Merge adjacent spans with identical styling.
    let mut merged: Vec<(String, InlineStyle)> = Vec::new();
    for (t, s) in spans {
        if let Some((last_text, last_style)) = merged.last_mut()
            && *last_style == s
        {
            last_text.push_str(&t);
            continue;
        }
        merged.push((t, s));
    }

    let mut layout = InlineLayout {
        text: String::new(),
        highlights: Vec::new(),
        code_ranges: Vec::new(),
        links: Vec::new(),
    };

    for (t, st) in merged {
        let start = layout.text.len();
        layout.text.push_str(&t);
        let range = start..layout.text.len();
        if range.is_empty() {
            continue;
        }
        let hl = inline_highlight(&st);
        if hl != HighlightStyle::default() {
            layout.highlights.push((range.clone(), hl));
        }
        if st.code {
            layout.code_ranges.push(range.clone());
        }
        if let Some(url) = st.link {
            layout.links.push((range, url));
        }
    }

    layout
}

/// Build the inline text element for a line of markdown — `StyledText` with
/// style runs, wrapped in `InteractiveText` when links are present so clicks
/// open the URL.
fn inline_element(src: &str, ctx: &mut RenderCtx) -> AnyElement {
    let laid = layout_inline(src);
    let mut styled = StyledText::new(laid.text);
    if !laid.highlights.is_empty() {
        styled = styled.with_highlights(laid.highlights);
    }
    if !laid.code_ranges.is_empty() {
        styled = styled.with_font_family_overrides(
            laid.code_ranges
                .into_iter()
                .map(|r| (r, SharedString::from("monospace"))),
        );
    }
    if laid.links.is_empty() {
        styled.into_any_element()
    } else {
        let (ranges, urls): (Vec<Range<usize>>, Vec<SharedString>) = laid.links.into_iter().unzip();
        InteractiveText::new(ctx.next_id(), styled)
            .on_click(ranges, move |ix, _window, cx| cx.open_url(&urls[ix]))
            .into_any_element()
    }
}

// ---------------------------------------------------------------------------
// Syntax highlighting — a small per-language tokenizer coloring keywords,
// string literals, comments and numbers.
// ---------------------------------------------------------------------------

fn syn_keyword() -> Hsla {
    rgb(0x82aaff).into()
}
fn syn_string() -> Hsla {
    rgb(0xc3e88d).into()
}
fn syn_comment() -> Hsla {
    rgb(0x5f6a8a).into()
}
fn syn_number() -> Hsla {
    rgb(0xf78c6c).into()
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];

const JS_KEYWORDS: &[&str] = &[
    "abstract",
    "arguments",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "implements",
    "import",
    "in",
    "infer",
    "instanceof",
    "interface",
    "keyof",
    "let",
    "namespace",
    "new",
    "null",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "satisfies",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

const PY_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def", "del",
    "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is",
    "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with",
    "yield", "self", "cls", "True", "False", "None",
];

const SH_KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "in", "do", "done", "case", "esac", "while",
    "until", "function", "select", "echo", "cd", "export", "local", "readonly", "return", "exit",
    "set", "shift", "source", "eval", "exec", "trap", "wait", "read", "printf", "test", "true",
    "false",
];

const JSON_KEYWORDS: &[&str] = &["true", "false", "null"];
const TOML_KEYWORDS: &[&str] = &["true", "false"];
const YAML_KEYWORDS: &[&str] = &[
    "true", "false", "null", "yes", "no", "on", "off", "True", "False", "Null", "Yes", "No", "On",
    "Off",
];

struct LangSpec {
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    string_delims: &'static [char],
    triple_strings: bool,
    /// `[section]` headers at line start (TOML).
    section_headers: bool,
    keywords: &'static [&'static str],
}

fn lang_spec(lang: &str) -> LangSpec {
    match lang.trim().to_ascii_lowercase().as_str() {
        "rust" | "rs" => LangSpec {
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            string_delims: &['"'],
            triple_strings: false,
            section_headers: false,
            keywords: RUST_KEYWORDS,
        },
        "ts" | "tsx" | "typescript" | "js" | "jsx" | "javascript" | "mjs" | "cjs" | "mts"
        | "cts" => LangSpec {
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            string_delims: &['"', '\'', '`'],
            triple_strings: false,
            section_headers: false,
            keywords: JS_KEYWORDS,
        },
        "json" => LangSpec {
            line_comments: &[],
            block_comment: None,
            string_delims: &['"'],
            triple_strings: false,
            section_headers: false,
            keywords: JSON_KEYWORDS,
        },
        "jsonc" | "json5" => LangSpec {
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            string_delims: &['"', '\''],
            triple_strings: false,
            section_headers: false,
            keywords: JSON_KEYWORDS,
        },
        "bash" | "sh" | "shell" | "zsh" | "console" => LangSpec {
            line_comments: &["#"],
            block_comment: None,
            string_delims: &['"', '\''],
            triple_strings: false,
            section_headers: false,
            keywords: SH_KEYWORDS,
        },
        "toml" => LangSpec {
            line_comments: &["#"],
            block_comment: None,
            string_delims: &['"', '\''],
            triple_strings: false,
            section_headers: true,
            keywords: TOML_KEYWORDS,
        },
        "yaml" | "yml" => LangSpec {
            line_comments: &["#"],
            block_comment: None,
            string_delims: &['"', '\''],
            triple_strings: false,
            section_headers: false,
            keywords: YAML_KEYWORDS,
        },
        "python" | "py" | "python3" => LangSpec {
            line_comments: &["#"],
            block_comment: None,
            string_delims: &['"', '\''],
            triple_strings: true,
            section_headers: false,
            keywords: PY_KEYWORDS,
        },
        _ => LangSpec {
            line_comments: &[],
            block_comment: None,
            string_delims: &[],
            triple_strings: false,
            section_headers: false,
            keywords: &[],
        },
    }
}

#[derive(Clone, Copy, PartialEq)]
enum TokenKind {
    Keyword,
    Str,
    Comment,
    Number,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Tokenize `code` and produce sorted, non-overlapping highlight ranges.
fn highlight_code(code: &str, lang: &str) -> Vec<(Range<usize>, HighlightStyle)> {
    let spec = lang_spec(lang);
    // Plain / unknown languages render unhighlighted.
    if spec.line_comments.is_empty()
        && spec.block_comment.is_none()
        && spec.string_delims.is_empty()
        && spec.keywords.is_empty()
    {
        return Vec::new();
    }
    let bytes = code.as_bytes();
    let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::new();

    fn color_of(kind: TokenKind) -> Hsla {
        match kind {
            TokenKind::Keyword => syn_keyword(),
            TokenKind::Str => syn_string(),
            TokenKind::Comment => syn_comment(),
            TokenKind::Number => syn_number(),
        }
    }

    let mut push = |kind: TokenKind, range: Range<usize>| {
        if range.is_empty() {
            return;
        }
        let color = color_of(kind);
        if let Some((last_range, last_hl)) = out.last_mut()
            && last_hl.color == Some(color)
            && last_range.end == range.start
        {
            last_range.end = range.end;
            return;
        }
        out.push((
            range,
            HighlightStyle {
                color: Some(color),
                ..Default::default()
            },
        ));
    };

    let mut i = 0;
    while i < code.len() {
        let rest = &code[i..];
        let at_line_start = i == 0 || bytes[i - 1] == b'\n';

        // Line comments.
        if spec.line_comments.iter().any(|p| rest.starts_with(p)) {
            let end = rest.find('\n').map(|o| i + o).unwrap_or(code.len());
            push(TokenKind::Comment, i..end);
            i = end;
            continue;
        }

        // Block comments.
        if let Some((open, close)) = spec.block_comment
            && rest.starts_with(open)
        {
            let end = rest[open.len()..]
                .find(close)
                .map(|o| i + open.len() + o + close.len())
                .unwrap_or(code.len());
            push(TokenKind::Comment, i..end);
            i = end;
            continue;
        }

        let c = rest.chars().next().unwrap();

        // TOML `[section]` headers.
        if spec.section_headers
            && c == '['
            && at_line_start
            && let Some(close) = rest.find(']')
        {
            push(TokenKind::Keyword, i..i + close + 1);
            i += close + 1;
            continue;
        }

        // Triple-quoted strings (Python docstrings).
        if spec.triple_strings && (rest.starts_with("\"\"\"") || rest.starts_with("'''")) {
            let delim = &rest[..3];
            let end = rest[3..]
                .find(delim)
                .map(|o| i + 3 + o + 3)
                .unwrap_or(code.len());
            push(TokenKind::Str, i..end);
            i = end;
            continue;
        }

        // String literals — backtick templates may span lines, others stop
        // at EOL when unterminated.
        if spec.string_delims.contains(&c) {
            let delim = c as u8;
            let mut j = i + 1;
            while j < code.len() {
                let b = bytes[j];
                if b == b'\\' {
                    j += 2;
                    continue;
                }
                if b == delim {
                    j += 1;
                    break;
                }
                if b == b'\n' && delim != b'`' {
                    break;
                }
                j += 1;
            }
            push(TokenKind::Str, i..j.min(code.len()));
            i = j.min(code.len());
            continue;
        }

        // Numbers — not immediately following an identifier char.
        let prev_is_ident = i > 0 && is_ident_byte(bytes[i - 1]);
        if c.is_ascii_digit() && !prev_is_ident {
            let start = i;
            while i < code.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
            {
                i += 1;
            }
            push(TokenKind::Number, start..i);
            continue;
        }

        // Identifiers / keywords.
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < code.len() && is_ident_byte(bytes[i]) {
                i += 1;
            }
            let word = &code[start..i];
            if spec.keywords.contains(&word) {
                push(TokenKind::Keyword, start..i);
            }
            continue;
        }

        i += c.len_utf8();
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(code: &str, hl: &[(Range<usize>, HighlightStyle)], text: &str, color: Hsla) -> bool {
        hl.iter()
            .any(|(r, h)| &code[r.clone()] == text && h.color == Some(color))
    }

    fn has_prefix(
        code: &str,
        hl: &[(Range<usize>, HighlightStyle)],
        prefix: &str,
        color: Hsla,
    ) -> bool {
        hl.iter()
            .any(|(r, h)| code[r.clone()].starts_with(prefix) && h.color == Some(color))
    }

    #[test]
    fn parses_block_structure() {
        let md =
            "# Title\n\npara one\ntwo\n\n```rust\nlet x = 1;\n```\n\n- a\n- b\n\n> quoted\n\n---\n";
        let blocks = parse_blocks(md);
        assert!(matches!(blocks[0], Block::Heading { level: 1, .. }));
        if let Block::Paragraph(t) = &blocks[1] {
            assert_eq!(t, "para one two");
        } else {
            panic!("expected paragraph");
        }
        assert!(
            matches!(&blocks[2], Block::Code { language: Some(l), .. } if l.as_str() == "rust")
        );
        if let Block::List(items) = &blocks[3] {
            assert_eq!(items.len(), 2);
            assert_eq!(items[1].text, "b");
        } else {
            panic!("expected list");
        }
        assert!(matches!(&blocks[4], Block::Quote(_)));
        assert!(matches!(&blocks[5], Block::Rule));
    }

    #[test]
    fn parses_table_with_alignment() {
        let md = "| name | age |\n|:-----|----:|\n| alice | 3 |\n";
        let blocks = parse_blocks(md);
        let Block::Table(t) = &blocks[0] else {
            panic!("expected table")
        };
        assert_eq!(t.header, vec!["name", "age"]);
        assert!(matches!(t.aligns[0], Align::Left));
        assert!(matches!(t.aligns[1], Align::Right));
        assert_eq!(t.rows, vec![vec!["alice".to_string(), "3".to_string()]]);
    }

    #[test]
    fn pipe_line_without_matching_delimiter_is_not_a_table() {
        let blocks = parse_blocks("a | b\n---\n");
        assert!(blocks.iter().all(|b| !matches!(b, Block::Table(_))));
    }

    #[test]
    fn parses_ordered_and_task_lists() {
        let blocks = parse_blocks("3. third\n- [x] done\n");
        let Block::List(items) = &blocks[0] else {
            panic!("expected list")
        };
        assert!(matches!(items[0].marker, ListMarker::Number(3)));
        assert!(matches!(items[1].marker, ListMarker::Task(true)));
        assert_eq!(items[1].text, "done");
    }

    #[test]
    fn inline_styles_and_code_spans() {
        let l = layout_inline("plain **bold** *it* ~~del~~ `code` end");
        assert_eq!(l.text, "plain bold it del code end");
        let b = l.text.find("bold").unwrap();
        assert!(
            l.highlights
                .iter()
                .any(|(r, h)| *r == (b..b + 4) && h.font_weight.is_some())
        );
        let it = l.text.find("it").unwrap();
        assert!(
            l.highlights
                .iter()
                .any(|(r, h)| *r == (it..it + 2) && h.font_style == Some(FontStyle::Italic))
        );
        let d = l.text.find("del").unwrap();
        assert!(
            l.highlights
                .iter()
                .any(|(r, h)| *r == (d..d + 3) && h.strikethrough.is_some())
        );
        let c = l.text.find("code").unwrap();
        assert!(l.code_ranges.iter().any(|r| *r == (c..c + 4)));
    }

    #[test]
    fn inline_links_collect_ranges_and_urls() {
        let l = layout_inline("see [the docs](https://example.com) and <https://x.y>");
        assert_eq!(l.links.len(), 2);
        let pos = l.text.find("the docs").unwrap();
        assert_eq!(l.links[0].0, pos..pos + 8);
        assert_eq!(l.links[0].1.as_str(), "https://example.com");
        assert_eq!(l.links[1].1.as_str(), "https://x.y");
    }

    #[test]
    fn intra_word_underscores_and_unclosed_markers_stay_literal() {
        let l = layout_inline("foo_bar_baz and **unclosed");
        assert_eq!(l.text, "foo_bar_baz and **unclosed");
    }

    #[test]
    fn highlights_rust_constructs() {
        let code = "let x = \"s\"; // c\n/* block */ let n = 42;";
        let hl = highlight_code(code, "rust");
        assert!(has(code, &hl, "let", syn_keyword()));
        assert!(has(code, &hl, "\"s\"", syn_string()));
        assert!(has_prefix(code, &hl, "// c", syn_comment()));
        assert!(has(code, &hl, "/* block */", syn_comment()));
        assert!(has(code, &hl, "42", syn_number()));
    }

    #[test]
    fn rust_lifetimes_are_not_strings() {
        let code = "fn f<'a>(x: &'a str) -> &'a str { x }";
        let hl = highlight_code(code, "rust");
        assert!(hl.iter().all(|(r, _)| !code[r.clone()].starts_with('\'')));
    }

    #[test]
    fn highlights_python_toml_and_shell() {
        let py = "def f():\n    \"\"\"doc\n    str\"\"\"\n    return 1 # ok";
        let hl = highlight_code(py, "python");
        assert!(has_prefix(py, &hl, "\"\"\"", syn_string()));
        assert!(has(py, &hl, "# ok", syn_comment()));
        assert!(has(py, &hl, "def", syn_keyword()));

        let toml = "[package]\nname = \"x\" # hi\n";
        let hl = highlight_code(toml, "toml");
        assert!(has(toml, &hl, "[package]", syn_keyword()));
        assert!(has(toml, &hl, "# hi", syn_comment()));

        let sh = "if true; then\n  echo \"hi\"\nfi";
        let hl = highlight_code(sh, "sh");
        assert!(has(sh, &hl, "if", syn_keyword()));
        assert!(has(sh, &hl, "\"hi\"", syn_string()));
    }

    #[test]
    fn unknown_language_stays_unhighlighted() {
        assert!(highlight_code("let x = 1", "").is_empty());
        assert!(highlight_code("let x = 1", "text").is_empty());
    }

    #[test]
    fn highlight_ranges_land_on_char_boundaries() {
        let code = "fn f() { let s = \"héllo\\n\"; let n = 3.14; } // oké";
        let hl = highlight_code(code, "rust");
        for (r, _) in &hl {
            assert!(code.is_char_boundary(r.start) && code.is_char_boundary(r.end));
        }
    }
}
