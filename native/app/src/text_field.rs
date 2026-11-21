//! Wrapping multi-line text field.
//!
//! Adapted from the `input.rs` example in the gpui crate
//! (github.com/zed-industries/zed, Apache-2.0) — a minimal `EntityInputHandler`
//! covering caret, selection, IME marked text, clipboard, and mouse selection.
//!
//! The field wraps like the web `composer-tiptap` editor: Enter submits,
//! Shift+Enter inserts a newline, and soft-wrapped rows grow the measured
//! height until the parent clamps it (`max-h-52`) and scrolls.

use std::ops::Range;

use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable, GlobalElementId,
    IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, Render, SharedString, Size, Style, TextRun, UTF16Selection, UnderlineStyle,
    Window, WrappedLine, actions, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme;

actions!(
    c67_text_field,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        Submit,
    ]
);

/// The editor's `p-1` inset — the composer wrapper pulls the field over the
/// card's body padding with `-m-1`, so the painted text lands exactly on the
/// body content box.
pub(crate) const FIELD_PAD: f32 = 4.;

/// The web editor's `max-h-52` viewport height.
pub(crate) const FIELD_MAX_H: f32 = 208.;

pub struct PromptSubmitted(pub SharedString);

/// The shaped text for one paint pass: one `WrappedLine` per hard line, with
/// each line's byte range in `content` and its top edge in text-block space.
struct FieldLayout {
    lines: Vec<WrappedLine>,
    /// utf8 byte offset into `content` where each hard line starts.
    line_starts: Vec<usize>,
    /// Text-block-space top of each hard line, in px.
    line_tops: Vec<Pixels>,
    line_height: Pixels,
    /// Total height of every wrapped row.
    height: Pixels,
    /// Scroll offset applied when this layout was shaped — paint and hit
    /// tests use it so quads and glyphs stay on the same origin.
    scroll_offset: Pixels,
}

impl FieldLayout {
    fn shape(
        display_text: SharedString,
        runs: &[TextRun],
        wrap_width: Option<Pixels>,
        font_size: Pixels,
        line_height: Pixels,
        window: &mut Window,
    ) -> Option<Self> {
        let lines = window
            .text_system()
            .shape_text(display_text, font_size, runs, wrap_width, None)
            .ok()?;
        let mut line_starts = Vec::with_capacity(lines.len());
        let mut line_tops = Vec::with_capacity(lines.len());
        let mut start = 0;
        let mut top = px(0.);
        for line in &lines {
            line_starts.push(start);
            line_tops.push(top);
            // `shape_text` splits on '\n'; the byte itself belongs to the
            // previous line's range.
            start += line.len() + 1;
            top += line.size(line_height).height;
        }
        Some(Self {
            lines: lines.into_vec(),
            line_starts,
            line_tops,
            line_height,
            height: top,
            scroll_offset: px(0.),
        })
    }

    /// The hard line containing `index` (indices on a '\n' resolve to the
    /// preceding line's end).
    fn line_index(&self, index: usize) -> usize {
        match self.line_starts.binary_search(&index) {
            Ok(ix) => ix,
            Err(ix) => ix.saturating_sub(1),
        }
    }

    /// (x, y) of a byte index in text-block space — the wrapped-row local x and
    /// the row's top edge.
    fn position_for_index(&self, index: usize) -> Point<Pixels> {
        let Some(&line_ix) = self
            .lines
            .len()
            .checked_sub(1)
            .map(|last| self.line_index(index).min(last))
            .as_ref()
        else {
            return point(px(0.), px(0.));
        };
        let line = &self.lines[line_ix];
        let local = index
            .saturating_sub(self.line_starts[line_ix])
            .min(line.len());
        let pos = line
            .position_for_index(local, self.line_height)
            .unwrap_or_else(|| point(px(0.), px(0.)));
        point(pos.x, self.line_tops[line_ix] + pos.y)
    }

    /// Byte index for a point in text-block space (clamped to the text).
    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let line_ix = match self.line_tops.binary_search(&position.y) {
            Ok(ix) => ix,
            Err(ix) => ix.saturating_sub(1).min(self.lines.len() - 1),
        };
        let line = &self.lines[line_ix];
        // gpui's `closest_index_for_position` errors with index 0 for points
        // below a line's last wrapped row — clamp into the line's block first.
        let local_y = (position.y - self.line_tops[line_ix]).clamp(
            px(0.),
            line.size(self.line_height).height - px(0.5),
        );
        let local = line
            .closest_index_for_position(
                point(position.x.max(px(0.)), local_y),
                self.line_height,
            )
            .unwrap_or_else(|index| index);
        self.line_starts[line_ix] + local
    }

    /// Selection rectangles in text-block space for a utf8 range — one quad
    /// per wrapped row, plus a small stub after a selected trailing newline.
    fn selection_bounds(&self, range: &Range<usize>) -> Vec<Bounds<Pixels>> {
        let mut bounds = Vec::new();
        for (line_ix, line) in self.lines.iter().enumerate() {
            let line_start = self.line_starts[line_ix];
            let line_end = line_start + line.len();
            if range.start >= line_end + 1 || range.end <= line_start {
                continue;
            }
            // Wrap rows of this hard line: each row ends at the index of the
            // glyph just before the wrap point.
            let mut row_start = 0;
            let mut row_ix = 0;
            let row_ends = line
                .wrap_boundaries
                .iter()
                .map(|boundary| {
                    let run = &line.unwrapped_layout.runs[boundary.run_ix];
                    run.glyphs[boundary.glyph_ix].index
                })
                .chain(std::iter::once(line.len()));
            for row_end in row_ends {
                let row_top = self.line_tops[line_ix] + row_ix as f32 * self.line_height;
                row_ix += 1;
                let global_row_start = line_start + row_start;
                let global_row_end = line_start + row_end;
                row_start = row_end;
                if range.end <= global_row_start {
                    break;
                }
                if range.start >= global_row_end && !(row_end == line.len() && range.start <= line_end)
                {
                    continue;
                }
                // `position_for_index` resolves a wrap-boundary index to the
                // *previous* row's end, so row edges are taken directly rather
                // than clamped through it.
                let quad_start = if range.start <= global_row_start {
                    px(0.)
                } else {
                    line.position_for_index(
                        range.start.min(global_row_end) - line_start,
                        self.line_height,
                    )
                    .map(|pos| pos.x)
                    .unwrap_or(px(0.))
                };
                let quad_end = if range.end >= global_row_end {
                    line.position_for_index(row_end, self.line_height)
                        .map(|pos| pos.x)
                        .unwrap_or(px(0.))
                } else {
                    line.position_for_index(
                        range.end.max(global_row_start) - line_start,
                        self.line_height,
                    )
                    .map(|pos| pos.x)
                    .unwrap_or(px(0.))
                };
                // A selected '\n' shows as a stub after the line's last glyph.
                let stub = if range.end > line_end && row_end == line.len() {
                    px(4.)
                } else {
                    px(0.)
                };
                bounds.push(Bounds::new(
                    point(quad_start, row_top),
                    size(quad_end - quad_start + stub, self.line_height),
                ));
            }
        }
        bounds
    }
}

pub struct TextField {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<FieldLayout>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    /// Pixels of text-block scrolled above the clipped viewport — the
    /// composer's wrapper clips to `max-h-52` and forwards wheel deltas.
    scroll_offset: Pixels,
    /// Preferred caret column while stepping vertically — keeps repeated
    /// up/down presses on the same x across ragged rows.
    goal_column: Option<Pixels>,
}

impl gpui::EventEmitter<PromptSubmitted> for TextField {}

impl TextField {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            scroll_offset: px(0.),
            goal_column: None,
        }
    }

    /// Wheel-scroll the clipped text block (gpui's convention: downward
    /// wheel deltas are negative, so they raise the scroll amount).
    pub fn scroll_by(&mut self, delta_y: Pixels, cx: &mut Context<Self>) {
        let new_offset = (self.scroll_offset - delta_y).clamp(px(0.), self.max_scroll_offset());
        if new_offset != self.scroll_offset {
            self.scroll_offset = new_offset;
            cx.notify();
        }
    }

    /// Content height beyond the `max-h-52` viewport — the scrollable span.
    fn max_scroll_offset(&self) -> Pixels {
        self.last_bounds
            .map(|bounds| (bounds.size.height - px(FIELD_MAX_H)).max(px(0.)))
            .unwrap_or(px(0.))
    }

    /// Painted display rows (hard lines × wraps) — the composer's key
    /// interceptor only steals up/down when more than one row exists.
    pub fn display_rows(&self) -> usize {
        self.last_layout
            .as_ref()
            .map(|layout| {
                layout
                    .lines
                    .iter()
                    .map(|line| line.wrap_boundaries.len() + 1)
                    .sum()
            })
            .unwrap_or(1)
    }

    /// Swap the ghost text — the composer mirrors the web's state-specific
    /// placeholders, which live outside the field itself.
    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let placeholder = placeholder.into();
        if self.placeholder != placeholder {
            self.placeholder = placeholder;
            cx.notify();
        }
    }

    fn submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.content.trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.reset(window, cx);
        cx.emit(PromptSubmitted(text.into()));
    }

    fn left(&mut self, _: &Left, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), window, cx);
        } else {
            self.move_to(self.selected_range.start, window, cx)
        }
    }

    fn right(&mut self, _: &Right, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), window, cx);
        } else {
            self.move_to(self.selected_range.end, window, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), window, cx);
    }

    fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), window, cx);
    }

    fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
        self.select_to(self.content.len(), window, cx)
    }

    fn home(&mut self, _: &Home, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
    }

    fn end(&mut self, _: &End, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), window, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                window.play_system_bell();
                return;
            }
            self.select_to(prev, window, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                window.play_system_bell();
                return;
            }
            self.select_to(next, window, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    /// Vertical caret step used by the composer's key interceptor (the
    /// field's own keymap binds only horizontal motion). `back` steps up a
    /// display row, `!back` down; shift extends the selection.
    pub fn move_vertical(&mut self, back: bool, select: bool, window: &Window, cx: &mut Context<Self>) {
        let Some(layout) = self.last_layout.as_ref() else {
            return;
        };
        let cursor = self.cursor_offset();
        let pos = layout.position_for_index(cursor);
        let x = self.goal_column.unwrap_or(pos.x);
        let target_y = if back {
            pos.y - layout.line_height * 0.5
        } else {
            pos.y + layout.line_height + layout.line_height * 0.5
        };
        let target = layout.index_for_position(point(x, target_y));
        if select {
            self.select_to(target, window, cx);
        } else {
            self.move_to(target, window, cx);
        }
        self.goal_column = Some(x);
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = true;
        window.request_virtual_keyboard();

        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), window, cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), window, cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), window, cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, window: &Window, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.goal_column = None;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(layout)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        layout.index_for_position(point(
            position.x - bounds.left() - px(FIELD_PAD),
            position.y - bounds.top() - px(FIELD_PAD) + self.scroll_offset,
        ))
    }

    fn select_to(&mut self, offset: usize, window: &Window, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    /// Unifies the line endings the platform hands us: X11 reports Return's
    /// `key_char` as "\r", the clipboard may carry "\r\n".
    fn normalize_newlines(text: &str) -> String {
        if !text.contains('\r') {
            return text.to_string();
        }
        text.replace("\r\n", "\n").replace('\r', "\n")
    }

    pub fn reset(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.last_layout = None;
        self.last_bounds = None;
        self.is_selecting = false;
        self.goal_column = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_range = self.range_from_utf16(&range_utf16);
        self.selection_reversed = false;
        self.goal_column = None;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.content.encode_utf16().count())
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked_range = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let new_text = Self::normalize_newlines(new_text);

        self.content =
            (self.content[0..range.start].to_owned() + &new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.goal_column = None;
        self.marked_range.take();
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let new_text = Self::normalize_newlines(new_text);

        self.content =
            (self.content[0..range.start].to_owned() + &new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        let insertion_start_utf16 = self.offset_to_utf16(range.start);
        let inserted_length_utf16 = new_text.encode_utf16().count();
        self.selected_range = new_selected_range_utf16
            .map(|selected| {
                self.range_from_utf16(
                    &(insertion_start_utf16 + selected.start.min(inserted_length_utf16)
                        ..insertion_start_utf16 + selected.end.min(inserted_length_utf16)),
                )
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;
        self.goal_column = None;

        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let origin = bounds.origin + point(px(FIELD_PAD), px(FIELD_PAD) - self.scroll_offset);
        let start = origin + layout.position_for_index(range.start);
        let end = origin + layout.position_for_index(range.end);
        Some(Bounds::from_corners(
            point(start.x, start.y),
            point(
                if end.y == start.y { end.x } else { bounds.right() },
                end.y + layout.line_height,
            ),
        ))
    }

    fn selection_bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Vec<Bounds<Pixels>> {
        let Some(layout) = self.last_layout.as_ref() else {
            return Vec::new();
        };
        let range = self.range_from_utf16(&range_utf16);
        let origin = bounds.origin + point(px(FIELD_PAD), px(FIELD_PAD) - self.scroll_offset);
        layout
            .selection_bounds(&range)
            .into_iter()
            .map(|quad| Bounds::new(quad.origin + origin, quad.size))
            .collect()
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let layout = self.last_layout.as_ref()?;
        Some(layout.index_for_position(point(
            position.x - bounds.left() - px(FIELD_PAD),
            position.y - bounds.top() - px(FIELD_PAD) + self.scroll_offset,
        )))
    }
}

struct FieldElement {
    input: Entity<TextField>,
}

struct PrepaintState {
    layout: Option<FieldLayout>,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
}

impl Element for FieldElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let line_height = window.pixel_snap(
            text_style
                .line_height
                .to_pixels(font_size.into(), window.rem_size()),
        );
        let input = self.input.clone();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        (
            window.request_measured_layout(
                style,
                move |known_dimensions, available_space, window, cx| {
                    let wrap_width = known_dimensions.width.or(match available_space.width {
                        AvailableSpace::Definite(x) => Some(x - px(FIELD_PAD) * 2.),
                        _ => None,
                    });
                    let input = input.read(cx);
                    let display_text = if input.content.is_empty() {
                        input.placeholder.clone()
                    } else {
                        input.content.clone()
                    };
                    let mut run = text_style.to_run(display_text.len());
                    if input.content.is_empty() {
                        run.color = theme::fg_muted().into();
                    }
                    let Some(layout) = FieldLayout::shape(
                        display_text,
                        &[run],
                        wrap_width,
                        font_size,
                        line_height,
                        window,
                    ) else {
                        return Size::default();
                    };
                    Size {
                        width: match available_space.width {
                            AvailableSpace::Definite(x) => x,
                            _ => layout
                                .lines
                                .iter()
                                .map(|line| line.size(line_height).width)
                                .fold(px(0.), |a, b| a.max(b)),
                        },
                        // p-1 top + bottom.
                        height: layout.height + px(FIELD_PAD) * 2.,
                    }
                },
            ),
            (),
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let scroll_offset = input.scroll_offset;
        let style = window.text_style();

        let (display_text, text_color) = if content.is_empty() {
            // Web --placeholder is the muted-foreground token.
            (input.placeholder.clone(), theme::fg_muted().into())
        } else {
            (content, style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = input.marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let wrap_width = Some((bounds.size.width - px(FIELD_PAD) * 2.).max(px(1.)));
        let layout = FieldLayout::shape(
            display_text,
            &runs,
            wrap_width,
            font_size,
            line_height,
            window,
        )
        .map(|mut layout| {
            layout.scroll_offset = scroll_offset;
            layout
        });

        let origin = bounds.origin + point(px(FIELD_PAD), px(FIELD_PAD) - scroll_offset);
        let (selection, cursor) = if let Some(layout) = layout.as_ref() {
            let selection = layout
                .selection_bounds(&selected_range)
                .into_iter()
                .map(|quad| fill(Bounds::new(quad.origin + origin, quad.size), theme::selection()))
                .collect::<Vec<_>>();
            let mut cursor_quad = None;
            if selected_range.is_empty() {
                let pos = layout.position_for_index(cursor);
                cursor_quad = Some(fill(
                    Bounds::new(
                        point(origin.x + pos.x, origin.y + pos.y),
                        size(px(2.), line_height),
                    ),
                    theme::cursor(),
                ));
                // Keep the caret row inside the `max-h-52` viewport — the
                // wrapper clips the element box, so the visible strip is the
                // element's top edge through `FIELD_MAX_H` below it.
                let viewport_bottom = bounds.top() + bounds.size.height.min(px(FIELD_MAX_H));
                let caret_top = origin.y + pos.y;
                let mut new_offset = scroll_offset;
                if caret_top < bounds.top() {
                    new_offset -= bounds.top() - caret_top;
                } else if caret_top + line_height > viewport_bottom {
                    new_offset += caret_top + line_height - viewport_bottom;
                }
                new_offset = new_offset.clamp(
                    px(0.),
                    (bounds.size.height - px(FIELD_MAX_H)).max(px(0.)),
                );
                if new_offset != scroll_offset {
                    self.input.update(cx, |input, cx| {
                        input.scroll_offset = new_offset;
                        cx.notify();
                    });
                }
            }
            (selection, cursor_quad)
        } else {
            (Vec::new(), None)
        };
        PrepaintState {
            layout,
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        for selection in prepaint.selection.drain(..) {
            window.paint_quad(selection)
        }
        let Some(layout) = prepaint.layout.take() else {
            return;
        };
        let mut origin =
            bounds.origin + point(px(FIELD_PAD), px(FIELD_PAD) - layout.scroll_offset);
        for line in &layout.lines {
            line.paint(
                origin,
                layout.line_height,
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            )
            .unwrap();
            origin.y += line.size(layout.line_height).height;
        }

        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(layout);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextField {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .w_full()
            .key_context("TextField")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(FieldElement { input: cx.entity() })
    }
}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
