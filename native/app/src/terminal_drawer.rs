//! Thread terminal drawer + right-panel Terminals tab.
//!
//! There is no xterm-grade grid renderer in GPUI, so [`Screen`] below is a
//! small terminal emulator that turns PTY byte streams into monospaced,
//! styled lines. Scope of what it handles:
//!
//! - `\r\n`, lone `\n`/`\x0b`/`\x0c`, `\r` (line overwrite), `\b`, `\t`, BEL.
//! - CSI: SGR (8/16 palette, 256-color cube, 24-bit color, bold/dim/italic/
//!   underline/strikethrough/inverse), cursor moves (CUU/CUD/CUF/CUB/CNL/CPL/
//!   CHA/CUP/VPA/VPR/CBT), ED, EL, IL/DL, ICH/DCH/ECH, SU/SD, DECSTBM scroll
//!   regions, save/restore cursor, insert mode, and the alternate screen
//!   (`?1049`/`?1047`/`?47`) so full-screen TUIs don't corrupt scrollback.
//! - OSC sequences (titles/hyperlinks), charset/mouse/private modes, and
//!   other escapes are consumed and ignored.
//! - Scrollback is a capped byte buffer ([`SCROLLBACK_CAP`]); the alternate
//!   screen's scroll is dropped instead of entering scrollback.
//! - Not handled: wide chars (treated as width-1), cursor glyph rendering,
//!   mouse reporting, true line reflow on resize.
//!
//! Wire protocol (see `native/server/src/rpc_terminal.rs`):
//! - `terminal.open`/`write`/`resize`/`clear`/`close` are one-shot queries.
//! - `terminal.attach` is a WINDOWED stream (opened via `open_stream`):
//!   `{type:"snapshot", snapshot:{history,…}}` first, then `{type:"output",
//!   data,…}` plus `exited`/`closed`/`cleared`/`restarted`/`activity` events.
//! - `subscribeTerminalMetadata` streams `{type:"snapshot", terminals:[…]}`
//!   then `{type:"upsert",terminal}` / `{type:"remove",…}` diffs.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    Context, Div, HighlightStyle, KeyDownEvent, MouseButton, SharedString, StyledText,
    UniformListScrollHandle, div, prelude::*, px, rgb, uniform_list,
};
use serde_json::{Value, json};

use crate::icons::{Icon, icon};
use crate::model::ConnectionStatus;
use crate::notifications::ToastKind;
use crate::{AppView, theme};

/// Fixed drawer height; the web drawer is drag-resizable and defaults to
/// `DEFAULT_THREAD_TERMINAL_HEIGHT` = 280px (apps/web/src/types.ts).
const DRAWER_HEIGHT: f32 = 280.;
/// PTY grid requested at open/attach until the first layout reports the
/// real body size — live resize then keeps it current.
const TERM_COLS: u16 = 96;
const TERM_ROWS: u16 = 24;
/// Approximate monospace cell advance at the 13px code font — the PTY grid
/// only needs whole cells, so a slightly-off estimate costs one column.
const TERM_CHAR_WIDTH: f32 = 7.8;
/// Horizontal padding inside the terminal body (`px_2` both sides).
const TERM_BODY_PAD_X: f32 = 16.;
/// Layout → `terminal.resize` settle time.
const RESIZE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
/// Approximate byte cap for scrollback text kept per terminal.
const SCROLLBACK_CAP: usize = 200 * 1024;
/// Uniform row height for the monospaced scrollback list (~13px × 1.3).
const LINE_HEIGHT: f32 = 17.;

const DEFAULT_TERMINAL_ID: &str = "t1";

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// One (thread_id, terminal_id) PTY session as the client sees it.
struct Session {
    status: TermStatus,
    /// Live `terminal.attach` stream id, if attached.
    stream_id: Option<String>,
    /// Attach when rendered (first open, reconnect, or a Restart click).
    want_attach: bool,
    label: String,
    pid: Option<i64>,
    screen: Screen,
    /// `terminal.open` already failed once with a TerminalCwdNot* error —
    /// retry without `cwd` so the server falls back to its own project dir
    /// (resolve_cwd in rpc_terminal.rs).
    no_cwd: bool,
    /// Queued PTY input. Writes are pumped one request at a time — the
    /// server doesn't order concurrent `terminal.write` calls, so parallel
    /// queries scramble keystrokes.
    input_queue: String,
    input_writing: bool,
    /// Grid size the last layout pass asked for. Debounced: `terminal.resize`
    /// goes out ~250ms after the value stops changing.
    desired_size: Option<(u16, u16)>,
}

#[derive(Clone)]
enum TermStatus {
    /// Attach requested; no snapshot yet.
    Connecting,
    Running,
    /// Shell exited on the server (exit code when known).
    Exited(Option<i64>),
    /// Server emitted `closed` — the session no longer exists remotely.
    Closed,
    /// Attach stream ended (or never connected) — Restart is offered.
    Dead,
}

impl TermStatus {
    fn dead(&self) -> bool {
        matches!(self, Self::Dead | Self::Closed | Self::Exited(_))
    }
}

/// `TerminalSummary` received from `subscribeTerminalMetadata`.
#[derive(Clone, Default)]
struct Known {
    thread_id: String,
    terminal_id: String,
    label: String,
    status: String,
    pid: Option<i64>,
    cwd: String,
    updated_at: String,
}

pub struct State {
    /// Whether the bottom terminal drawer is open for the selected thread.
    pub drawer_open: bool,
    /// thread_id → terminal_id shown in the drawer.
    active: HashMap<String, String>,
    /// (thread_id, terminal_id) → live session (attach stream + scrollback).
    sessions: HashMap<(String, String), Session>,
    /// (thread_id, terminal_id) → latest metadata summary.
    known: HashMap<(String, String), Known>,
    /// Live `subscribeTerminalMetadata` stream id.
    meta_stream: Option<String>,
    /// The Terminals pane was shown once — keep the metadata stream alive.
    meta_want: bool,
    /// Key-capture focus handle for the drawer.
    focus: Option<gpui::FocusHandle>,
    /// Window focus listeners feeding `focused` for the input-row indicator.
    focus_subs: Vec<gpui::Subscription>,
    focused: bool,
    /// Scroll handle for the scrollback list.
    scroll: UniformListScrollHandle,
    /// Connection flag last frame — detects Disconnected→Connected edges.
    was_connected: bool,
    /// Drawer-open edge tracking — focuses the terminal on open.
    drawer_was_open: bool,
    focus_pending: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            drawer_open: false,
            active: HashMap::new(),
            sessions: HashMap::new(),
            known: HashMap::new(),
            meta_stream: None,
            meta_want: false,
            focus: None,
            focus_subs: Vec::new(),
            focused: false,
            scroll: UniformListScrollHandle::default(),
            was_connected: false,
            drawer_was_open: false,
            focus_pending: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Screen — a minimal terminal emulator
// ---------------------------------------------------------------------------

const ATTR_BOLD: u8 = 1;
const ATTR_DIM: u8 = 2;
const ATTR_ITALIC: u8 = 4;
const ATTR_UNDERLINE: u8 = 8;
const ATTR_INVERSE: u8 = 16;
const ATTR_STRIKE: u8 = 32;

/// Cell color: default, 0-255 palette index, or true color.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum Col {
    #[default]
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
struct Attr {
    fg: Col,
    bg: Col,
    flags: u8,
}

#[derive(Clone, Copy)]
struct Cell {
    ch: char,
    a: Attr,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            a: Attr::default(),
        }
    }
}

/// Pre-rendered form of one line: flat text plus byte-range highlight runs.
struct Rendered {
    text: SharedString,
    runs: Rc<Vec<(Range<usize>, HighlightStyle)>>,
}

struct Line {
    cells: Vec<Cell>,
    cache: Option<Rc<Rendered>>,
    dirty: bool,
}

impl Line {
    fn blank() -> Self {
        Line {
            cells: Vec::new(),
            cache: None,
            dirty: true,
        }
    }

    fn clear(&mut self) {
        self.cells.clear();
        self.dirty = true;
    }

    /// Rough byte size used for scrollback accounting.
    fn est_bytes(&self) -> usize {
        self.cells.iter().map(|c| c.ch.len_utf8()).sum::<usize>() + 1
    }
}

#[derive(Clone, Copy, PartialEq)]
enum PMode {
    Ground,
    Esc,
    /// Consume one character (charset/VT52 legacy sequences).
    Skip1,
    Csi,
    Osc,
    /// OSC ended by ESC \ — saw the ESC.
    OscEsc,
}

/// Saved primary-screen state while the alternate screen is active.
struct AltScreen {
    lines: VecDeque<Line>,
    top: usize,
    scroll_bytes: usize,
    cr: usize,
    cc: usize,
    attr: Attr,
    wrap: bool,
    region: (usize, usize),
    saved: (usize, usize, Attr),
}

struct Screen {
    cols: usize,
    rows: usize,
    /// scrollback ++ visible screen lines; `top` counts the scrollback prefix.
    lines: VecDeque<Line>,
    top: usize,
    scroll_bytes: usize,
    /// Cursor row — absolute index into `lines` (>= top).
    cr: usize,
    /// Cursor col — may equal `cols` while a wrap is pending.
    cc: usize,
    attr: Attr,
    wrap: bool,
    /// DECSC / CSI s cursor save.
    saved: (usize, usize, Attr),
    /// DECSTBM scroll region, screen-relative row indexes.
    region: (usize, usize),
    insert: bool,
    alt: Option<Box<AltScreen>>,
    mode: PMode,
    csi: String,
    /// Escape-sequence tail split across chunks.
    pending: String,
    /// Palette (dark/light) the line caches were rendered with.
    rendered_dark: Option<bool>,
}

impl Screen {
    fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            lines: VecDeque::new(),
            top: 0,
            scroll_bytes: 0,
            cr: 0,
            cc: 0,
            attr: Attr::default(),
            wrap: false,
            saved: (0, 0, Attr::default()),
            region: (0, rows.saturating_sub(1)),
            insert: false,
            alt: None,
            mode: PMode::Ground,
            csi: String::new(),
            pending: String::new(),
            rendered_dark: None,
        }
    }

    fn reset(&mut self) {
        let (cols, rows) = (self.cols, self.rows);
        *self = Screen::new(cols, rows);
    }

    /// `terminal.clear` — drop scrollback and the visible screen.
    fn clear_all(&mut self) {
        self.lines.clear();
        self.top = 0;
        self.scroll_bytes = 0;
        self.cr = 0;
        self.cc = 0;
        self.wrap = false;
        self.pending.clear();
        self.mode = PMode::Ground;
    }

    fn screen_bot(&self) -> usize {
        self.top + self.rows.saturating_sub(1)
    }

    fn ensure_line(&mut self, row: usize) {
        while self.lines.len() <= row {
            self.lines.push_back(Line::blank());
        }
    }

    fn ensure_screen(&mut self) {
        let need = self.top + self.rows;
        while self.lines.len() < need {
            self.lines.push_back(Line::blank());
        }
    }

    fn put(&mut self, ch: char) {
        if self.wrap {
            self.lf();
            self.cc = 0;
            self.wrap = false;
        }
        let row = self.cr;
        self.ensure_line(row);
        let line = &mut self.lines[row];
        if line.cells.len() <= self.cc {
            line.cells.resize(self.cc + 1, Cell::default());
        }
        if self.insert {
            line.cells.insert(self.cc, Cell { ch, a: self.attr });
            line.cells.truncate(self.cols);
        } else {
            line.cells[self.cc] = Cell { ch, a: self.attr };
        }
        line.dirty = true;
        self.cc += 1;
        if self.cc >= self.cols {
            self.cc = self.cols - 1;
            self.wrap = true;
        }
    }

    /// Index / linefeed — scrolls the region only at its bottom edge.
    fn lf(&mut self) {
        self.ensure_screen();
        let region_bot = self.top + self.region.1;
        if self.cr == region_bot {
            let full = self.region == (0, self.rows.saturating_sub(1));
            self.scroll_up(1);
            if full {
                // A fresh line was appended at the back; follow it.
                self.cr += 1;
            }
        } else if self.cr < self.screen_bot() {
            self.cr += 1;
        }
    }

    /// Reverse index — scrolls the region down at its top edge.
    fn ri(&mut self) {
        self.ensure_screen();
        let region_top = self.top + self.region.0;
        if self.cr == region_top {
            self.scroll_down(1);
        } else if self.cr > self.top {
            self.cr -= 1;
        }
    }

    fn scroll_up(&mut self, n: usize) {
        self.ensure_screen();
        let full = self.region == (0, self.rows.saturating_sub(1));
        let rt = self.top + self.region.0;
        let rb = self.top + self.region.1;
        for _ in 0..n.min(self.rows) {
            if full {
                if self.alt.is_some() {
                    // Alternate screen: scrolled-off lines are dropped.
                    self.lines.pop_front();
                } else {
                    // The line leaving the screen top enters scrollback.
                    self.scroll_bytes += self.lines[self.top].est_bytes();
                    self.top += 1;
                }
                self.lines.push_back(Line::blank());
            } else {
                self.lines.remove(rt);
                self.lines.insert(rb, Line::blank());
            }
        }
        // Trim scrollback to the byte cap.
        while self.scroll_bytes > SCROLLBACK_CAP && self.top > 0 {
            if let Some(line) = self.lines.pop_front() {
                self.scroll_bytes = self.scroll_bytes.saturating_sub(line.est_bytes());
            }
            self.top -= 1;
            self.cr = self.cr.saturating_sub(1);
            self.saved.0 = self.saved.0.saturating_sub(1);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        self.ensure_screen();
        let rt = self.top + self.region.0;
        let rb = self.top + self.region.1;
        for _ in 0..n.min(self.rows) {
            self.lines.insert(rt, Line::blank());
            // After the insert the old region-bottom sits at rb+1.
            self.lines.remove(rb + 1);
        }
    }

    fn erase_in_line(&mut self, mode: i64) {
        self.ensure_line(self.cr);
        let line = &mut self.lines[self.cr];
        let (s, e) = match mode {
            1 => (0, (self.cc + 1).min(line.cells.len())),
            2 => (0, line.cells.len()),
            _ => (self.cc.min(line.cells.len()), line.cells.len()),
        };
        for c in &mut line.cells[s..e] {
            *c = Cell::default();
        }
        line.dirty = true;
    }

    fn erase_in_display(&mut self, mode: i64) {
        self.ensure_screen();
        match mode {
            1 => {
                for r in self.top..self.cr {
                    self.lines[r].clear();
                }
                self.erase_in_line(1);
            }
            2 => {
                self.ensure_screen();
                for r in self.top..self.top + self.rows {
                    self.lines[r].clear();
                }
            }
            3 => {
                // Drop scrollback entirely.
                let dropped = self.top;
                self.lines.drain(0..dropped);
                self.top = 0;
                self.scroll_bytes = 0;
                self.cr = self.cr.saturating_sub(dropped);
                self.saved.0 = self.saved.0.saturating_sub(dropped);
            }
            _ => {
                self.erase_in_line(0);
                for r in self.cr + 1..self.top + self.rows {
                    self.lines[r].clear();
                }
            }
        }
    }

    fn insert_lines(&mut self, n: usize) {
        let rt = self.top + self.region.0;
        let rb = self.top + self.region.1;
        if self.cr < rt || self.cr > rb {
            return;
        }
        for _ in 0..n.min(rb - self.cr + 1) {
            self.lines.insert(self.cr, Line::blank());
            self.lines.remove(rb + 1);
        }
    }

    fn delete_lines(&mut self, n: usize) {
        let rt = self.top + self.region.0;
        let rb = self.top + self.region.1;
        if self.cr < rt || self.cr > rb {
            return;
        }
        for _ in 0..n.min(rb - self.cr + 1) {
            self.lines.remove(self.cr);
            self.lines.insert(rb, Line::blank());
        }
    }

    fn move_to_rel(&mut self, rel_row: usize, col: usize) {
        self.ensure_screen();
        let rel = rel_row.min(self.rows.saturating_sub(1));
        self.cr = self.top + rel;
        self.cc = col.min(self.cols.saturating_sub(1));
        self.wrap = false;
    }

    fn enter_alt(&mut self) {
        if self.alt.is_some() {
            return;
        }
        self.alt = Some(Box::new(AltScreen {
            lines: std::mem::take(&mut self.lines),
            top: self.top,
            scroll_bytes: self.scroll_bytes,
            cr: self.cr,
            cc: self.cc,
            attr: self.attr,
            wrap: self.wrap,
            region: self.region,
            saved: self.saved,
        }));
        self.top = 0;
        self.scroll_bytes = 0;
        self.cr = 0;
        self.cc = 0;
        self.wrap = false;
        self.region = (0, self.rows.saturating_sub(1));
        self.saved = (0, 0, Attr::default());
    }

    fn exit_alt(&mut self) {
        let Some(alt) = self.alt.take() else { return };
        self.lines = alt.lines;
        self.top = alt.top;
        self.scroll_bytes = alt.scroll_bytes;
        self.cr = alt.cr;
        self.cc = alt.cc;
        self.attr = alt.attr;
        self.wrap = alt.wrap;
        self.region = alt.region;
        self.saved = alt.saved;
    }

    // -- escape-sequence parser ---------------------------------------------

    fn feed(&mut self, data: &str) {
        if data.is_empty() {
            return;
        }
        // Unconsumed input lives in `pending` so escapes can span chunks.
        // `pending` always begins at the sequence's ESC introducer, so it can
        // be replayed from Ground — resuming mid-mode would re-misinterpret
        // the introducer bytes (e.g. ESC inside Csi aborts the sequence).
        let mut s = std::mem::take(&mut self.pending);
        if !s.is_empty() {
            self.mode = PMode::Ground;
        }
        s.push_str(data);
        // Start of the in-flight escape sequence, for the tail save below.
        let mut seq_start = usize::MAX;
        let bytes = s.as_bytes();
        let mut i = 0usize;
        while i < s.len() {
            let b = bytes[i];
            match self.mode {
                PMode::Ground => match b {
                    0x1b => {
                        seq_start = i;
                        self.mode = PMode::Esc;
                        i += 1;
                    }
                    b'\n' | 0x0b | 0x0c => {
                        self.lf();
                        i += 1;
                    }
                    b'\r' => {
                        self.cc = 0;
                        self.wrap = false;
                        i += 1;
                    }
                    0x08 => {
                        self.cc = self.cc.saturating_sub(1);
                        self.wrap = false;
                        i += 1;
                    }
                    b'\t' => {
                        self.cc = ((self.cc / 8) + 1) * 8;
                        if self.cc >= self.cols {
                            self.cc = self.cols - 1;
                        }
                        self.wrap = false;
                        i += 1;
                    }
                    // BEL and other C0/DEL controls.
                    0x00..=0x1f | 0x7f => i += 1,
                    _ => {
                        let ch = s[i..].chars().next().unwrap_or('\u{fffd}');
                        self.put(ch);
                        i += ch.len_utf8();
                    }
                },
                PMode::Esc => match b {
                    b'[' => {
                        self.csi.clear();
                        self.mode = PMode::Csi;
                        i += 1;
                    }
                    b']' => {
                        self.mode = PMode::Osc;
                        i += 1;
                    }
                    // Charset and other single-char legacy sequences.
                    b'(' | b')' | b'*' | b'+' | b'#' | b'%' => {
                        self.mode = PMode::Skip1;
                        i += 1;
                    }
                    b'7' => {
                        self.saved = (self.cr, self.cc, self.attr);
                        self.mode = PMode::Ground;
                        i += 1;
                    }
                    b'8' => {
                        let (r, c, a) = self.saved;
                        self.cr = r.min(self.screen_bot());
                        self.cc = c;
                        self.attr = a;
                        self.wrap = false;
                        self.mode = PMode::Ground;
                        i += 1;
                    }
                    b'D' => {
                        self.lf();
                        self.mode = PMode::Ground;
                        i += 1;
                    }
                    b'M' => {
                        self.ri();
                        self.mode = PMode::Ground;
                        i += 1;
                    }
                    b'E' => {
                        self.cc = 0;
                        self.lf();
                        self.mode = PMode::Ground;
                        i += 1;
                    }
                    b'c' => {
                        self.reset();
                        i += 1;
                    }
                    _ => {
                        self.mode = PMode::Ground;
                        i += 1;
                    }
                },
                PMode::Skip1 => {
                    self.mode = PMode::Ground;
                    i += 1;
                }
                PMode::Csi => match b {
                    // params (0x30-0x3f) and intermediates (0x20-0x2f)
                    0x20..=0x3f => {
                        if self.csi.len() < 96 {
                            self.csi.push(b as char);
                        }
                        i += 1;
                    }
                    // final byte
                    0x40..=0x7e => {
                        let params = std::mem::take(&mut self.csi);
                        self.apply_csi(&params, b as char);
                        self.mode = PMode::Ground;
                        i += 1;
                    }
                    _ => {
                        self.mode = PMode::Ground;
                        i += 1;
                    }
                },
                PMode::Osc => match b {
                    0x07 => {
                        self.mode = PMode::Ground;
                        i += 1;
                    }
                    0x1b => {
                        self.mode = PMode::OscEsc;
                        i += 1;
                    }
                    _ => i += 1,
                },
                PMode::OscEsc => {
                    self.mode = if b == b'\\' {
                        PMode::Ground
                    } else {
                        PMode::Osc
                    };
                    i += 1;
                }
            }
        }
        if self.mode != PMode::Ground && seq_start != usize::MAX {
            self.pending = s[seq_start.min(s.len())..].to_string();
        }
    }

    fn apply_csi(&mut self, raw: &str, final_ch: char) {
        let private = raw.starts_with('?');
        // Colon sub-parameters degrade to plain separators.
        let cleaned: String = raw
            .trim_start_matches('?')
            .chars()
            .map(|c| if c == ':' { ';' } else { c })
            .filter(|c| c.is_ascii_digit() || *c == ';')
            .collect();
        let parts: Vec<i64> = cleaned
            .split(';')
            .map(|p| p.parse::<i64>().unwrap_or(0))
            .collect();
        let p = |i: usize, d: i64| parts.get(i).copied().unwrap_or(d).max(0);
        let n = |d: i64| p(0, d).max(1) as usize;
        match final_ch {
            'm' => self.apply_sgr(&parts),
            'A' => {
                self.ensure_screen();
                self.cr = self.cr.saturating_sub(n(1)).max(self.top);
                self.wrap = false;
            }
            'B' => {
                self.ensure_screen();
                self.cr = (self.cr + n(1)).min(self.screen_bot());
                self.wrap = false;
            }
            'C' => {
                self.cc = (self.cc + n(1)).min(self.cols - 1);
                self.wrap = false;
            }
            'D' => {
                self.cc = self.cc.saturating_sub(n(1));
                self.wrap = false;
            }
            'E' => {
                self.ensure_screen();
                self.cr = (self.cr + n(1)).min(self.screen_bot());
                self.cc = 0;
                self.wrap = false;
            }
            'F' => {
                self.ensure_screen();
                self.cr = self.cr.saturating_sub(n(1)).max(self.top);
                self.cc = 0;
                self.wrap = false;
            }
            'G' | '`' => {
                self.cc = (p(0, 1) as usize).saturating_sub(1).min(self.cols - 1);
                self.wrap = false;
            }
            'H' | 'f' => {
                self.move_to_rel(
                    (p(0, 1) as usize).saturating_sub(1),
                    (p(1, 1) as usize).saturating_sub(1),
                );
            }
            'd' => self.move_to_rel((p(0, 1) as usize).saturating_sub(1), self.cc),
            'e' => {
                self.ensure_screen();
                let rel = (self.cr.saturating_sub(self.top) + n(1)).min(self.rows - 1);
                self.cr = self.top + rel;
                self.wrap = false;
            }
            'J' => self.erase_in_display(p(0, 0)),
            'K' => self.erase_in_line(p(0, 0)),
            'L' => self.insert_lines(n(1)),
            'M' => self.delete_lines(n(1)),
            'P' => {
                // DCH — delete chars in the cursor line.
                self.ensure_line(self.cr);
                let line = &mut self.lines[self.cr];
                for _ in 0..n(1) {
                    if self.cc < line.cells.len() {
                        line.cells.remove(self.cc);
                    }
                }
                line.dirty = true;
            }
            '@' => {
                // ICH — insert blank chars at the cursor.
                self.ensure_line(self.cr);
                let line = &mut self.lines[self.cr];
                if line.cells.len() < self.cc {
                    line.cells.resize(self.cc, Cell::default());
                }
                for _ in 0..n(1) {
                    if self.cc <= line.cells.len() {
                        line.cells.insert(self.cc, Cell::default());
                    }
                }
                line.cells.truncate(self.cols);
                line.dirty = true;
            }
            'X' => {
                // ECH — erase chars without shifting.
                self.ensure_line(self.cr);
                let line = &mut self.lines[self.cr];
                let end = (self.cc + n(1)).min(line.cells.len());
                for c in &mut line.cells[self.cc.min(end)..end] {
                    *c = Cell::default();
                }
                line.dirty = true;
            }
            'S' => self.scroll_up(n(1)),
            'T' => self.scroll_down(n(1)),
            'Z' => {
                // CBT — back-tab.
                self.cc = if self.cc <= 1 {
                    0
                } else {
                    ((self.cc - 1) / 8) * 8
                };
                self.wrap = false;
            }
            'r' => {
                let lo = (p(0, 1) as usize).saturating_sub(1);
                let hi = (p(1, self.rows as i64) as usize)
                    .saturating_sub(1)
                    .min(self.rows - 1);
                if lo < hi {
                    self.region = (lo, hi);
                } else {
                    self.region = (0, self.rows - 1);
                }
                self.move_to_rel(0, 0);
            }
            's' if !private => self.saved = (self.cr, self.cc, self.attr),
            'u' if !private => {
                let (r, c, a) = self.saved;
                self.cr = r.min(self.screen_bot());
                self.cc = c;
                self.attr = a;
                self.wrap = false;
            }
            'h' | 'l' if private => {
                let set = final_ch == 'h';
                for &mode in &parts {
                    match mode {
                        1049 | 1047 | 47 => {
                            if set {
                                self.enter_alt();
                            } else {
                                self.exit_alt();
                            }
                        }
                        1048 => {
                            if set {
                                self.saved = (self.cr, self.cc, self.attr);
                            } else {
                                let (r, c, a) = self.saved;
                                self.cr = r.min(self.screen_bot());
                                self.cc = c;
                                self.attr = a;
                            }
                        }
                        // ?25 cursor, ?1 app cursor keys, ?7 wrap, ?2004
                        // bracketed paste, mouse modes — tracked nowhere.
                        _ => {}
                    }
                }
            }
            'h' | 'l' => {
                // Non-private: only insert mode (4) matters to us.
                if p(0, 0) == 4 {
                    self.insert = final_ch == 'h';
                }
            }
            _ => {}
        }
    }

    fn apply_sgr(&mut self, parts: &[i64]) {
        if parts.is_empty() {
            self.attr = Attr::default();
            return;
        }
        let mut i = 0;
        while i < parts.len() {
            match parts[i] {
                0 => self.attr = Attr::default(),
                1 => self.attr.flags |= ATTR_BOLD,
                2 => self.attr.flags |= ATTR_DIM,
                3 => self.attr.flags |= ATTR_ITALIC,
                4 => self.attr.flags |= ATTR_UNDERLINE,
                7 => self.attr.flags |= ATTR_INVERSE,
                9 => self.attr.flags |= ATTR_STRIKE,
                22 => self.attr.flags &= !(ATTR_BOLD | ATTR_DIM),
                23 => self.attr.flags &= !ATTR_ITALIC,
                24 => self.attr.flags &= !ATTR_UNDERLINE,
                27 => self.attr.flags &= !ATTR_INVERSE,
                29 => self.attr.flags &= !ATTR_STRIKE,
                30..=37 => self.attr.fg = Col::Idx((parts[i] - 30) as u8),
                38 => {
                    i = self.ext_color(parts, i, true);
                }
                39 => self.attr.fg = Col::Default,
                40..=47 => self.attr.bg = Col::Idx((parts[i] - 40) as u8),
                48 => {
                    i = self.ext_color(parts, i, false);
                }
                49 => self.attr.bg = Col::Default,
                90..=97 => self.attr.fg = Col::Idx((parts[i] - 90 + 8) as u8),
                100..=107 => self.attr.bg = Col::Idx((parts[i] - 100 + 8) as u8),
                _ => {}
            }
            i += 1;
        }
    }

    /// `38;5;n` / `38;2;r;g;b` extended colors; returns the consumed index.
    fn ext_color(&mut self, parts: &[i64], i: usize, fg: bool) -> usize {
        let (col, next) = match parts.get(i + 1) {
            Some(5) => (
                Col::Idx(parts.get(i + 2).copied().unwrap_or(0).clamp(0, 255) as u8),
                i + 2,
            ),
            Some(2) => (
                Col::Rgb(
                    parts.get(i + 2).copied().unwrap_or(0).clamp(0, 255) as u8,
                    parts.get(i + 3).copied().unwrap_or(0).clamp(0, 255) as u8,
                    parts.get(i + 4).copied().unwrap_or(0).clamp(0, 255) as u8,
                ),
                i + 4,
            ),
            _ => return i,
        };
        if fg {
            self.attr.fg = col;
        } else {
            self.attr.bg = col;
        }
        next
    }

    // -- rendering -----------------------------------------------------------

    fn render_line(line: &mut Line) -> Rc<Rendered> {
        if !line.dirty && line.cache.is_some() {
            return line.cache.as_ref().unwrap().clone();
        }
        line.dirty = false;
        let mut n = line.cells.len();
        // Trailing default-styled blanks contribute nothing.
        while n > 0 && line.cells[n - 1].ch == ' ' && line.cells[n - 1].a == Attr::default() {
            n -= 1;
        }
        let mut text = String::with_capacity(n + 1);
        let mut runs: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        let mut cur: Option<HighlightStyle> = None;
        let mut start = 0usize;
        for cell in &line.cells[..n] {
            let st = style_of(cell.a);
            if st != cur {
                if let Some(prev) = cur {
                    runs.push((start..text.len(), prev));
                }
                cur = st;
                start = text.len();
            }
            text.push(cell.ch);
        }
        if let Some(prev) = cur {
            runs.push((start..text.len(), prev));
        }
        let rendered = Rc::new(Rendered {
            text: SharedString::from(text),
            runs: Rc::new(runs),
        });
        line.cache = Some(rendered.clone());
        rendered
    }

    /// All lines (scrollback + screen) as renderable rows.
    fn render_rows(&mut self) -> Rc<Vec<Rc<Rendered>>> {
        // Invalidate caches when the palette (theme mode) changed.
        let dark = theme::effective_dark();
        if self.rendered_dark != Some(dark) {
            self.rendered_dark = Some(dark);
            for line in &mut self.lines {
                line.dirty = true;
            }
        }
        // Trailing all-blank rows (unused screen space below the cursor)
        // would push scroll_to_bottom past the content — skip them.
        let mut end = self.lines.len();
        while end > 0 {
            let line = &self.lines[end - 1];
            if line
                .cells
                .iter()
                .any(|c| c.ch != ' ' || c.a != Attr::default())
            {
                break;
            }
            end -= 1;
        }
        Rc::new(
            self.lines
                .iter_mut()
                .take(end)
                .map(Self::render_line)
                .collect(),
        )
    }
}

// -- colors ----------------------------------------------------------------

/// The xterm 16-color palettes from apps/web (TERMINAL_PALETTES).
const DARK16: [u32; 16] = [
    0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666,
    0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xe5e5e5,
];
const LIGHT16: [u32; 16] = [
    0x000000, 0xcd3131, 0x107c10, 0x949800, 0x0451a5, 0xbc05bc, 0x0598bc, 0x555555, 0x666666,
    0xcd3131, 0x14ce14, 0xb5ba00, 0x0451a5, 0xbc05bc, 0x0598bc, 0xa5a5a5,
];

/// `--terminal-background: var(--background)` — the web terminal surface is
/// plain background, not a dedicated terminal color.
fn term_bg() -> gpui::Rgba {
    theme::canvas()
}

fn idx_to_rgb(i: u8) -> u32 {
    let palette = if theme::effective_dark() {
        &DARK16
    } else {
        &LIGHT16
    };
    match i {
        0..=15 => palette[i as usize],
        16..=231 => {
            let v = i - 16;
            let (r, g, b) = (v / 36, (v / 6) % 6, v % 6);
            let c = |x: u8| if x == 0 { 0u32 } else { 55 + 40 * x as u32 };
            (c(r) << 16) | (c(g) << 8) | c(b)
        }
        _ => {
            let g = 8 + (i - 232) as u32 * 10;
            (g << 16) | (g << 8) | g
        }
    }
}

fn col_to_hsla(c: Col) -> Option<gpui::Hsla> {
    match c {
        Col::Default => None,
        Col::Idx(i) => Some(rgb(idx_to_rgb(i)).into()),
        Col::Rgb(r, g, b) => Some(rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32).into()),
    }
}

fn style_of(a: Attr) -> Option<HighlightStyle> {
    let mut fg = col_to_hsla(a.fg);
    let mut bg = col_to_hsla(a.bg);
    if a.flags & ATTR_INVERSE != 0 {
        // Reversed video: unset sides fall back to the default colors.
        let d_fg = col_to_hsla(Col::Default).or(Some(theme::fg().into()));
        let d_bg = Some(term_bg().into());
        let eff_fg = bg.or(d_bg);
        let eff_bg = fg.or(d_fg);
        fg = eff_fg;
        bg = eff_bg;
    }
    let bold = a.flags & ATTR_BOLD != 0;
    // Bold + a dark palette index traditionally brightens the color.
    if bold && let Col::Idx(i @ 0..=7) = a.fg {
        fg = col_to_hsla(Col::Idx(i + 8));
    }
    let style = HighlightStyle {
        color: fg,
        font_weight: bold.then_some(gpui::FontWeight::BOLD),
        font_style: (a.flags & ATTR_ITALIC != 0).then_some(gpui::FontStyle::Italic),
        background_color: bg,
        underline: (a.flags & ATTR_UNDERLINE != 0).then_some(gpui::UnderlineStyle {
            color: None,
            thickness: px(1.0),
            wavy: false,
        }),
        strikethrough: (a.flags & ATTR_STRIKE != 0).then_some(gpui::StrikethroughStyle {
            color: None,
            thickness: px(1.0),
        }),
        fade_out: (a.flags & ATTR_DIM != 0).then_some(0.55),
    };
    (style != HighlightStyle::default()).then_some(style)
}

// ---------------------------------------------------------------------------
// Session helpers on AppView
// ---------------------------------------------------------------------------

/// `getTerminalLabel` from `@t3tools/shared/terminalLabels`, mirrored from
/// `label_for` in `native/server/src/rpc_terminal.rs`.
fn label_for(terminal_id: &str) -> String {
    let lower = terminal_id.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("terminal-")
        .or_else(|| lower.strip_prefix("term-"));
    if let Some(rest) = rest {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let tail = &rest[digits.len()..];
            let ok = tail.is_empty()
                || (tail.len() == 37
                    && tail.starts_with('-')
                    && tail[1..].chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
            if ok {
                return format!("Terminal {digits}");
            }
        }
    }
    terminal_id.to_string()
}

fn status_label(session: &Session) -> String {
    match &session.status {
        TermStatus::Connecting => "connecting".into(),
        TermStatus::Running => "running".into(),
        TermStatus::Exited(code) => match code {
            Some(c) => format!("exited ({c})"),
            None => "exited".into(),
        },
        TermStatus::Closed => "closed".into(),
        TermStatus::Dead => "disconnected".into(),
    }
}

/// Status-dot tone — same semantic roles as the row dot below.
fn status_color(session: &Session) -> gpui::Hsla {
    match &session.status {
        TermStatus::Running => theme::success().into(),
        TermStatus::Connecting => theme::warning().into(),
        TermStatus::Exited(_) | TermStatus::Closed => theme::fg_muted().into(),
        TermStatus::Dead => theme::error().into(),
    }
}

impl AppView {
    /// Project workspace root for a thread (payload `cwd` for `terminal.open`).
    fn terminal_cwd_for(&self, thread_id: &str) -> Option<String> {
        let thread = self
            .state
            .threads
            .iter()
            .find(|t| t.id.as_ref() == thread_id)?;
        let project = self
            .state
            .projects
            .iter()
            .find(|p| p.id.as_ref() == thread.project_id.as_ref())?;
        let path = project.path.to_string();
        (!path.is_empty()).then_some(path)
    }

    /// Mint a fresh terminal id (`t2`, `t3`, …) not colliding with live or
    /// known sessions for the thread.
    fn mint_terminal_id(&self, thread_id: &str) -> String {
        for n in 1u32.. {
            let id = format!("t{n}");
            let key = (thread_id.to_string(), id.clone());
            if !self.terminal_state.sessions.contains_key(&key)
                && !self.terminal_state.known.contains_key(&key)
            {
                return id;
            }
        }
        unreachable!()
    }

    /// Open (or re-attach) a PTY for `thread`/`term`. `restart` re-uses the
    /// attach stream with `restartIfNotRunning`, which respawns a dead shell.
    fn ensure_terminal(
        &mut self,
        thread_id: &str,
        terminal_id: &str,
        restart: bool,
        cx: &mut Context<Self>,
    ) {
        let key = (thread_id.to_string(), terminal_id.to_string());
        let stale_stream = {
            let st = &mut self.terminal_state;
            let s = st.sessions.entry(key.clone()).or_insert_with(|| Session {
                status: TermStatus::Connecting,
                stream_id: None,
                want_attach: true,
                label: label_for(terminal_id),
                pid: None,
                screen: Screen::new(TERM_COLS as usize, TERM_ROWS as usize),
                no_cwd: false,
                input_queue: String::new(),
                input_writing: false,
                desired_size: None,
            });
            s.want_attach = true;
            if s.stream_id.is_some() && !restart {
                return;
            }
            s.status = TermStatus::Connecting;
            s.stream_id.take()
        };
        if let Some(id) = stale_stream {
            self.close_stream(&id);
        }
        if !matches!(self.state.connection, ConnectionStatus::Connected) {
            // Reattach happens in terminal_sync when Connected lands.
            return;
        }
        // Real terminal use ⇒ keep the metadata stream alive too.
        self.terminal_state.meta_want = true;

        // Explicit `terminal.open` per the contract — attach alone would
        // also open-or-attach, but open surfaces spawn errors as RPC failures.
        let no_cwd = self
            .terminal_state
            .sessions
            .get(&key)
            .map(|s| s.no_cwd)
            .unwrap_or(false);
        let cwd = if no_cwd {
            None
        } else {
            self.terminal_cwd_for(thread_id).or_else(|| {
                // Server-reported cwd, for threads whose project isn't loaded.
                self.terminal_state
                    .known
                    .get(&(thread_id.to_string(), terminal_id.to_string()))
                    .map(|k| k.cwd.clone())
            })
        };
        // The layout canvas may have measured the body before the attach
        // completed — honor that size from the first request.
        let (cols, rows) = self
            .terminal_state
            .sessions
            .get(&key)
            .and_then(|s| s.desired_size)
            .unwrap_or((TERM_COLS, TERM_ROWS));
        let mut open_payload = json!({
            "threadId": thread_id,
            "terminalId": terminal_id,
            "cols": cols,
            "rows": rows,
        });
        if let Some(cwd) = &cwd {
            open_payload["cwd"] = json!(cwd);
        }
        if !restart && let Some(rx) = self.send_query("terminal.open", open_payload) {
            let open_tid = thread_id.to_string();
            let open_term = terminal_id.to_string();
            cx.spawn(async move |this, cx| {
                let Ok(reply) = rx.recv().await else { return };
                if reply.get("_tag").and_then(Value::as_str) == Some("Success") {
                    return;
                }
                let err_tag = reply["cause"]["error"]["_tag"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let _ = this.update(cx, |this, cx| {
                    if err_tag.starts_with("TerminalCwdNot") {
                        // The project path doesn't exist — retry once without
                        // `cwd` so the server falls back to its project dir.
                        let key = (open_tid.clone(), open_term.clone());
                        if let Some(s) = this.terminal_state.sessions.get_mut(&key) {
                            s.no_cwd = true;
                        }
                        this.ensure_terminal(&open_tid, &open_term, true, cx);
                    } else {
                        this.push_toast(
                            ToastKind::Error,
                            "Couldn't open terminal",
                            Some(describe_rpc_error(&reply)),
                            None,
                            cx,
                        );
                    }
                });
            })
            .detach();
            // Seed the PTY grid at open; live resize continues via
            // terminal_note_size once the layout canvas reports bounds.
            let _ = self.send_query(
                "terminal.resize",
                json!({
                    "threadId": thread_id,
                    "terminalId": terminal_id,
                    "cols": cols,
                    "rows": rows,
                }),
            );
        }

        let mut attach_payload = json!({
            "threadId": thread_id,
            "terminalId": terminal_id,
            "cols": cols,
            "rows": rows,
        });
        if let Some(cwd) = cwd {
            attach_payload["cwd"] = json!(cwd);
        }
        if restart {
            attach_payload["restartIfNotRunning"] = json!(true);
        }
        match self.open_stream("terminal.attach", attach_payload) {
            Some((sid, rx)) => {
                let session = self.terminal_state.sessions.get_mut(&key).unwrap();
                session.stream_id = Some(sid.clone());
                let tid = thread_id.to_string();
                let term = terminal_id.to_string();
                cx.spawn(async move |this, cx| {
                    while let Ok(value) = rx.recv().await {
                        let alive = this.update(cx, |this, cx| {
                            this.on_terminal_chunk(&tid, &term, &value, cx);
                        });
                        if alive.is_err() {
                            return;
                        }
                    }
                    let _ = this.update(cx, |this, cx| {
                        this.terminal_stream_done(&tid, &term, &sid, cx);
                    });
                })
                .detach();
            }
            None => {
                if let Some(s) = self.terminal_state.sessions.get_mut(&key) {
                    s.status = TermStatus::Dead;
                }
            }
        }
    }

    /// One `terminal.attach` chunk → session state. `cx.notify()` afterwards.
    fn on_terminal_chunk(
        &mut self,
        thread_id: &str,
        terminal_id: &str,
        v: &Value,
        cx: &mut Context<Self>,
    ) {
        let key = (thread_id.to_string(), terminal_id.to_string());
        let Some(s) = self.terminal_state.sessions.get_mut(&key) else {
            return;
        };
        let snap_fields = |snap: &Value, s: &mut Session| {
            s.screen.reset();
            if let Some(h) = snap["history"].as_str() {
                s.screen.feed(h);
            }
            s.status = match snap["status"].as_str().unwrap_or("") {
                "running" | "starting" => TermStatus::Running,
                "exited" => TermStatus::Exited(snap["exitCode"].as_i64()),
                "error" => TermStatus::Dead,
                _ => TermStatus::Running,
            };
            s.pid = snap["pid"].as_i64();
            if let Some(l) = snap["label"].as_str() {
                s.label = l.to_string();
            }
        };
        match v["type"].as_str().unwrap_or("") {
            "snapshot" => {
                snap_fields(&v["snapshot"], s);
                self.terminal_state.scroll.scroll_to_bottom();
            }
            "restarted" => {
                snap_fields(&v["snapshot"], s);
                self.terminal_state.scroll.scroll_to_bottom();
            }
            "output" => {
                let at_end = self.terminal_state.scroll.is_scrolled_to_end() != Some(false);
                if let Some(data) = v["data"].as_str() {
                    s.screen.feed(data);
                }
                if at_end {
                    self.terminal_state.scroll.scroll_to_bottom();
                }
            }
            "exited" => {
                s.status = TermStatus::Exited(v["exitCode"].as_i64());
                self.terminal_state.scroll.scroll_to_bottom();
            }
            "closed" => {
                s.status = TermStatus::Closed;
                s.want_attach = false;
            }
            "cleared" => {
                s.screen.clear_all();
                self.terminal_state.scroll.scroll_to_bottom();
            }
            "activity" => {
                if let Some(l) = v["label"].as_str() {
                    s.label = l.to_string();
                }
            }
            "error" => {
                let msg = v["message"].as_str().unwrap_or("terminal error");
                s.screen.feed(&format!("\r\n[error] {msg}\r\n"));
                s.status = TermStatus::Dead;
            }
            _ => {}
        }
        cx.notify();
    }

    /// The attach stream exited (end, failure, or socket drop cleanup).
    fn terminal_stream_done(
        &mut self,
        thread_id: &str,
        terminal_id: &str,
        stream_id: &str,
        cx: &mut Context<Self>,
    ) {
        let key = (thread_id.to_string(), terminal_id.to_string());
        let Some(s) = self.terminal_state.sessions.get_mut(&key) else {
            return;
        };
        if s.stream_id.as_deref() != Some(stream_id) {
            return; // a newer stream superseded this pump
        }
        s.stream_id = None;
        s.want_attach = false;
        if matches!(s.status, TermStatus::Running | TermStatus::Connecting) {
            s.status = TermStatus::Dead;
        }
        cx.notify();
    }

    /// `subscribeTerminalMetadata` → the `known` summary map.
    fn open_meta_stream(&mut self, cx: &mut Context<Self>) {
        let Some((sid, rx)) = self.open_stream("subscribeTerminalMetadata", json!({})) else {
            return;
        };
        self.terminal_state.meta_stream = Some(sid.clone());
        cx.spawn(async move |this, cx| {
            while let Ok(value) = rx.recv().await {
                let alive = this.update(cx, |this, cx| {
                    this.on_terminal_meta_chunk(&value, cx);
                });
                if alive.is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.terminal_state.meta_stream.as_deref() == Some(sid.as_str()) {
                    this.terminal_state.meta_stream = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn on_terminal_meta_chunk(&mut self, v: &Value, cx: &mut Context<Self>) {
        let parse = |s: &Value| {
            let thread_id = s["threadId"].as_str().unwrap_or("").to_string();
            let terminal_id = s["terminalId"].as_str().unwrap_or("").to_string();
            (
                (thread_id.clone(), terminal_id.clone()),
                Known {
                    thread_id,
                    terminal_id,
                    label: s["label"].as_str().unwrap_or("").to_string(),
                    status: s["status"].as_str().unwrap_or("").to_string(),
                    pid: s["pid"].as_i64(),
                    cwd: s["cwd"].as_str().unwrap_or("").to_string(),
                    updated_at: s["updatedAt"].as_str().unwrap_or("").to_string(),
                },
            )
        };
        match v["type"].as_str().unwrap_or("") {
            "snapshot" => {
                self.terminal_state.known.clear();
                if let Some(list) = v["terminals"].as_array() {
                    for t in list {
                        let (k, known) = parse(t);
                        self.terminal_state.known.insert(k, known);
                    }
                }
            }
            "upsert" => {
                let (k, known) = parse(&v["terminal"]);
                if !k.0.is_empty() {
                    self.terminal_state.known.insert(k, known);
                }
            }
            "remove" => {
                let k = (
                    v["threadId"].as_str().unwrap_or("").to_string(),
                    v["terminalId"].as_str().unwrap_or("").to_string(),
                );
                self.terminal_state.known.remove(&k);
            }
            _ => {}
        }
        cx.notify();
    }

    /// Lazily create the drawer's focus handle plus its focus subscriptions.
    fn terminal_init_focus(&mut self, cx: &mut Context<Self>) {
        let handle = cx.focus_handle();
        let weak = cx.weak_entity();
        let subs = cx
            .with_window(cx.entity_id(), |window, app| {
                let weak_in = weak.clone();
                let weak_out = weak.clone();
                vec![
                    window.on_focus_in(&handle, app, move |_w, cx| {
                        let _ = weak_in.update(cx, |this, cx| {
                            this.terminal_state.focused = true;
                            cx.notify();
                        });
                    }),
                    window.on_focus_out(&handle, app, move |_ev, _w, cx| {
                        let _ = weak_out.update(cx, |this, cx| {
                            this.terminal_state.focused = false;
                            cx.notify();
                        });
                    }),
                ]
            })
            .unwrap_or_default();
        let st = &mut self.terminal_state;
        st.focus = Some(handle);
        st.focus_subs = subs;
    }

    /// Render-time housekeeping: reconnect bookkeeping, session ensures, the
    /// metadata stream, and focus-handle init. Idempotent; called from both
    /// renderers and from deferred spawns.
    fn terminal_sync(&mut self, cx: &mut Context<Self>) {
        if self.terminal_state.focus.is_none() {
            self.terminal_init_focus(cx);
        }
        let connected = matches!(self.state.connection, ConnectionStatus::Connected);
        let mut stale_streams: Vec<String> = Vec::new();
        {
            let st = &mut self.terminal_state;
            if st.was_connected != connected {
                st.was_connected = connected;
                if !connected {
                    // The net loop drops dead subscriptions without StreamEnd,
                    // so receivers never close on their own — pull the sinks.
                    for s in st.sessions.values_mut() {
                        if let Some(id) = s.stream_id.take() {
                            stale_streams.push(id);
                            s.want_attach = true; // re-attach on reconnect
                            if matches!(s.status, TermStatus::Running | TermStatus::Connecting) {
                                s.status = TermStatus::Dead;
                            }
                        }
                    }
                    if let Some(id) = st.meta_stream.take() {
                        stale_streams.push(id);
                    }
                }
            }
            if st.drawer_open != st.drawer_was_open {
                st.drawer_was_open = st.drawer_open;
                if st.drawer_open {
                    st.focus_pending = true;
                }
            }
        }
        for id in stale_streams {
            self.close_stream(&id);
        }
        if self.terminal_state.focus_pending {
            self.terminal_state.focus_pending = false;
            if let Some(h) = self.terminal_state.focus.clone() {
                let _ = cx.with_window(cx.entity_id(), |window, app| h.focus(window, app));
            }
        }
        if !connected {
            return;
        }
        // Drawer's terminal for the selected thread.
        if self.terminal_state.drawer_open
            && let Some(tid) = self.state.selected.clone()
        {
            let term = self
                .terminal_state
                .active
                .get(tid.as_ref())
                .cloned()
                .or_else(|| self.newest_known_terminal(&tid))
                .unwrap_or_else(|| DEFAULT_TERMINAL_ID.to_string());
            self.ensure_terminal(&tid, &term, false, cx);
        }
        // Metadata stream once the pane (or a terminal) has been used.
        if self.terminal_state.meta_want && self.terminal_state.meta_stream.is_none() {
            self.open_meta_stream(cx);
        }
    }

    /// Most recently updated known terminal for a thread (drawer default).
    fn newest_known_terminal(&self, thread_id: &str) -> Option<String> {
        self.terminal_state
            .known
            .values()
            .filter(|k| k.thread_id == thread_id)
            .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
            .map(|k| k.terminal_id.clone())
    }

    /// Queue PTY input and start the single-flight write pump. Each queued
    /// chunk waits for the previous `terminal.write` reply before the next
    /// request goes out, so server-side ordering matches keystroke order.
    fn terminal_write(
        &mut self,
        thread_id: &str,
        terminal_id: &str,
        data: String,
        cx: &mut Context<Self>,
    ) {
        let key = (thread_id.to_string(), terminal_id.to_string());
        {
            let Some(s) = self.terminal_state.sessions.get_mut(&key) else {
                return;
            };
            if !matches!(s.status, TermStatus::Running) {
                return;
            }
            s.input_queue.push_str(&data);
            if s.input_writing {
                return;
            }
            s.input_writing = true;
        }
        let tid = thread_id.to_string();
        let term = terminal_id.to_string();
        cx.spawn(async move |this, cx| {
            loop {
                let next = this.update(cx, |this, _cx| {
                    let key = (tid.clone(), term.clone());
                    let s = this.terminal_state.sessions.get_mut(&key)?;
                    if s.input_queue.is_empty() {
                        s.input_writing = false;
                        return None;
                    }
                    // Contract allows 64 KiB; keep chunks small and char-safe.
                    let queue = &mut s.input_queue;
                    let mut n = queue.len().min(4096);
                    while !queue.is_char_boundary(n) {
                        n -= 1;
                    }
                    let chunk: String = queue.drain(..n).collect();
                    this.send_query(
                        "terminal.write",
                        json!({
                            "threadId": tid,
                            "terminalId": term,
                            "data": chunk,
                        }),
                    )
                });
                match next {
                    Ok(Some(rx)) => {
                        // Wait for the reply (or a dropped query) before the
                        // next write — keeps writes strictly ordered.
                        let _ = rx.recv().await;
                    }
                    _ => {
                        // Entity gone, disconnected, or queue drained.
                        let _ = this.update(cx, |this, _cx| {
                            let key = (tid.clone(), term.clone());
                            if let Some(s) = this.terminal_state.sessions.get_mut(&key) {
                                s.input_writing = false;
                            }
                        });
                        return;
                    }
                }
            }
        })
        .detach();
    }

    /// `terminal.close` + drop the local session.
    fn terminal_close(&mut self, thread_id: &str, terminal_id: &str, cx: &mut Context<Self>) {
        let key = (thread_id.to_string(), terminal_id.to_string());
        let _ = self.send_query(
            "terminal.close",
            json!({ "threadId": thread_id, "terminalId": terminal_id }),
        );
        let stale = {
            let st = &mut self.terminal_state;
            let stream = st.sessions.remove(&key).and_then(|s| s.stream_id);
            st.known.remove(&key);
            if st.active.get(thread_id) == Some(&terminal_id.to_string()) {
                st.active.remove(thread_id);
            }
            stream
        };
        if let Some(id) = stale {
            self.close_stream(&id);
        }
        cx.notify();
    }

    /// "+" — open a new terminal (`t2`, `t3`, …) for the selected thread.
    fn terminal_new_for_selected(&mut self, cx: &mut Context<Self>) {
        let Some(tid) = self.state.selected.clone() else {
            self.push_toast(
                ToastKind::Info,
                "No thread selected",
                Some("Select a thread to open a terminal.".into()),
                None,
                cx,
            );
            return;
        };
        let term = self.mint_terminal_id(&tid);
        let st = &mut self.terminal_state;
        st.active.insert(tid.to_string(), term.clone());
        st.drawer_open = true;
        self.ensure_terminal(&tid, &term, false, cx);
        cx.notify();
    }

    /// Key-capture → PTY bytes for the drawer's active terminal.
    fn on_terminal_key(
        &mut self,
        ev: &KeyDownEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread_id) = self.state.selected.clone() else {
            return;
        };
        let Some(terminal_id) = self
            .terminal_state
            .active
            .get(thread_id.as_ref())
            .cloned()
            .or_else(|| self.newest_known_terminal(&thread_id))
            .or_else(|| Some(DEFAULT_TERMINAL_ID.to_string()))
        else {
            return;
        };
        let ks = &ev.keystroke;
        let mods = ks.modifiers;
        let key = ks.key.as_str();
        // App-level combos keep working while the terminal is focused.
        if mods.platform
            || (mods.control && matches!(key, "`" | "b" | "," | "j" | "{" | "}"))
            || (mods.control && mods.alt && key == "b")
            || (mods.control && mods.shift && matches!(key, "f" | "m" | "p"))
        {
            return;
        }
        if mods.control && key == "w" {
            // Web `terminal.close` — mod+w while the terminal is focused.
            self.terminal_close(&thread_id, &terminal_id, cx);
            cx.stop_propagation();
            return;
        }
        if mods.control && key == "l" {
            // ctrl-l — server-side `terminal.clear` (also emits `cleared`,
            // which resets the local buffer through the attach stream).
            let _ = self.send_query(
                "terminal.clear",
                json!({ "threadId": thread_id.as_ref(), "terminalId": terminal_id }),
            );
            cx.stop_propagation();
            return;
        }
        if mods.control && key == "v" {
            // ctrl-v / ctrl-shift-v → paste into the PTY.
            if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                // PTYs want CR line endings; normalize \r\n and bare \n to \r.
                let text = text.replace("\r\n", "\r").replace('\n', "\r");
                self.terminal_write(&thread_id, &terminal_id, text, cx);
            }
            cx.stop_propagation();
            return;
        }
        let data = key_to_bytes(ks);
        if let Some(data) = data {
            self.terminal_write(&thread_id, &terminal_id, data, cx);
            cx.stop_propagation();
        }
    }

    /// The layout canvas reports the terminal body's pixel bounds each
    /// frame; if the implied grid changed, remember it and arm the
    /// `terminal.resize` debounce.
    fn terminal_note_size(
        &mut self,
        thread_id: &str,
        terminal_id: &str,
        cols: u16,
        rows: u16,
        cx: &mut Context<Self>,
    ) {
        let key = (thread_id.to_string(), terminal_id.to_string());
        let Some(session) = self.terminal_state.sessions.get_mut(&key) else {
            return;
        };
        if session.desired_size == Some((cols, rows)) {
            return;
        }
        session.desired_size = Some((cols, rows));
        let thread_id = thread_id.to_string();
        let terminal_id = terminal_id.to_string();
        cx.spawn(async move |this, cx| {
            smol::Timer::after(RESIZE_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                this.terminal_apply_size(&thread_id, &terminal_id, cx);
            });
        })
        .detach();
    }

    /// Send `terminal.resize` if the debounced desired size still differs
    /// from what the session's screen tracks.
    fn terminal_apply_size(&mut self, thread_id: &str, terminal_id: &str, _cx: &mut Context<Self>) {
        let key = (thread_id.to_string(), terminal_id.to_string());
        let Some(session) = self.terminal_state.sessions.get(&key) else {
            return;
        };
        let Some((cols, rows)) = session.desired_size else {
            return;
        };
        if session.screen.cols == cols as usize && session.screen.rows == rows as usize {
            return;
        }
        if let Some(session) = self.terminal_state.sessions.get_mut(&key) {
            session.screen.cols = cols as usize;
            session.screen.rows = rows as usize;
        }
        let _ = self.send_query(
            "terminal.resize",
            json!({
                "threadId": thread_id,
                "terminalId": terminal_id,
                "cols": cols,
                "rows": rows,
            }),
        );
    }

    /// ctrl-w away from the terminal — web `rightPanel.close`: close the
    /// right panel if open, else the terminal drawer.
    pub(crate) fn close_panel_or_drawer(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.panel_state.tab.is_some() {
            self.panel_state.tab = None;
            cx.notify();
        } else if self.terminal_state.drawer_open {
            self.terminal_drawer_set_open(false, window, cx);
        }
    }
}

/// Map a keystroke to the bytes a real terminal would send.
fn key_to_bytes(ks: &gpui::Keystroke) -> Option<String> {
    let mods = ks.modifiers;
    let key = ks.key.as_str();
    if mods.control {
        // Control bytes: ctrl-a..z → 0x01-0x1a, ctrl-[/\/]/^/_ → 0x1b-0x1f.
        let c = key.chars().next()?;
        if key.chars().count() == 1 && c.is_ascii() {
            let b = (c as u8) & 0x1f;
            if b > 0 {
                return Some((b as char).to_string());
            }
        }
        return None;
    }
    if mods.alt {
        // Meta keys arrive as ESC-prefixed sequences.
        return ks.key_char.as_ref().map(|c| format!("\x1b{c}"));
    }
    Some(match key {
        "enter" => "\r".to_string(),
        "backspace" => "\x7f".to_string(),
        "tab" => {
            if mods.shift {
                "\x1b[Z".to_string()
            } else {
                "\t".to_string()
            }
        }
        "escape" => "\x1b".to_string(),
        "up" => "\x1b[A".to_string(),
        "down" => "\x1b[B".to_string(),
        "right" => "\x1b[C".to_string(),
        "left" => "\x1b[D".to_string(),
        "home" => "\x1b[H".to_string(),
        "end" => "\x1b[F".to_string(),
        "delete" => "\x1b[3~".to_string(),
        "insert" => "\x1b[2~".to_string(),
        "pageup" => "\x1b[5~".to_string(),
        "pagedown" => "\x1b[6~".to_string(),
        _ => ks.key_char.clone()?,
    })
}

/// Pull a readable message out of an RPC Failure exit frame —
/// `{_tag:"Failure", cause:{_tag:"Fail", error:{_tag:…, …}}}`.
fn describe_rpc_error(reply: &Value) -> String {
    let err = &reply["cause"]["error"];
    let tag = err["_tag"].as_str().unwrap_or("terminal error");
    let detail = err["message"]
        .as_str()
        .or_else(|| err["cwd"].as_str())
        .or_else(|| err["cause"].as_str())
        .unwrap_or("");
    if detail.is_empty() {
        tag.to_string()
    } else {
        format!("{tag}: {detail}")
    }
}

// ---------------------------------------------------------------------------
// Drawer rendering
// ---------------------------------------------------------------------------

impl AppView {
    pub(crate) fn render_terminal_drawer(&mut self, cx: &mut Context<Self>) -> Div {
        self.terminal_sync(cx);
        if !self.terminal_state.drawer_open {
            return div();
        }
        let st = &self.terminal_state;
        let tid = self.state.selected.clone();
        let term = tid.as_ref().and_then(|t| {
            st.active
                .get(t.as_ref())
                .cloned()
                .or_else(|| self.newest_known_terminal(t))
        });
        // Owned snapshot of the active session's header state — the body
        // renderer borrows `self` mutably, so nothing may stay borrowed.
        let session_info: Option<(String, String, Option<i64>, gpui::Hsla, bool)> =
            tid.as_ref().and_then(|t| {
                term.as_ref().and_then(|term| {
                    st.sessions.get(&(t.to_string(), term.clone())).map(|s| {
                        (
                            s.label.clone(),
                            status_label(s),
                            s.pid,
                            status_color(s),
                            s.status.dead(),
                        )
                    })
                })
            });

        // Header row ------------------------------------------------------------
        let mut header = div()
            .h(px(30.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(theme::border())
            .text_xs()
            .child(icon(Icon::Terminal, 12.).text_color(theme::fg_muted()));
        match (tid.as_ref(), session_info.as_ref()) {
            (Some(_), Some((label, status, pid, dot, _dead))) => {
                header = header.child(div().text_color(theme::fg()).child(label.clone()));
                header = header.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(div().w(px(6.)).h(px(6.)).rounded_full().bg(*dot))
                        .child(div().text_color(theme::fg_muted()).child(status.clone())),
                );
                if let Some(pid) = pid {
                    header = header.child(
                        div()
                            .text_color(theme::fg_muted())
                            .child(format!("pid {pid}")),
                    );
                }
            }
            (Some(_), None) => {
                header = header.child(
                    div()
                        .text_color(theme::fg_muted())
                        .child("terminal starting…"),
                );
            }
            (None, _) => {
                header = header.child(
                    div()
                        .text_color(theme::fg_muted())
                        .child("no thread selected"),
                );
            }
        }
        header = header.child(div().flex_1());
        // +: new terminal for the selected thread.
        header = header.child(
            drawer_button(Icon::Plus, "new-terminal")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _w, cx| {
                        this.terminal_new_for_selected(cx);
                    }),
                )
                .into_any_element(),
        );
        // Restart the active session.
        if let (Some(t), Some(term)) = (tid.clone(), term.clone()) {
            header = header.child(
                drawer_button(Icon::RefreshCw, "restart-terminal")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _w, cx| {
                            this.ensure_terminal(&t, &term, true, cx);
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
            );
        }
        // Hide the drawer (streams stay attached — persistent terminals).
        header = header.child(
            drawer_button(Icon::X, "close-drawer")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, w, cx| {
                        this.terminal_drawer_set_open(false, w, cx);
                    }),
                )
                .into_any_element(),
        );

        // Body ---------------------------------------------------------------
        let body = self.render_terminal_body(tid.as_deref(), term.as_deref(), cx);

        // Input row ------------------------------------------------------------
        let focused = self.terminal_state.focused;
        let hint = match session_info.as_ref() {
            Some((_, status, _, _, false)) if status == "running" => {
                "type to send input · ctrl-c interrupt · ctrl-l clear"
            }
            Some((_, _, _, _, true)) => "session ended — use the restart button above",
            Some(_) => "connecting…",
            None => "",
        };
        let input_row = div()
            .h(px(26.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(theme::border())
            .text_xs()
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(icon(Icon::Terminal, 12.).text_color(if focused {
                        theme::primary()
                    } else {
                        theme::fg_muted()
                    })),
            )
            .child(
                div()
                    .text_color(if focused {
                        theme::fg()
                    } else {
                        theme::fg_muted()
                    })
                    .child(if focused {
                        hint.to_string()
                    } else {
                        format!("click to focus the terminal — {hint}")
                    }),
            );

        let focus = self.terminal_state.focus.clone();
        let focus2 = focus.clone();
        // Web drawer: `border-t border-border/80` over `--background`.
        div()
            .flex()
            .flex_col()
            .h(px(DRAWER_HEIGHT))
            .border_t_1()
            .border_color(theme::border_soft())
            .bg(term_bg())
            .key_context("Terminal")
            .when_some(focus, |el, h| el.track_focus(&h))
            .on_key_down(cx.listener(AppView::on_terminal_key))
            .on_mouse_down(MouseButton::Left, move |_ev, window, cx| {
                if let Some(h) = &focus2 {
                    h.focus(window, cx);
                }
            })
            .child(header)
            .child(body)
            .child(input_row)
    }

    fn render_terminal_body(
        &mut self,
        thread_id: Option<&str>,
        terminal_id: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Div {
        let connected = matches!(self.state.connection, ConnectionStatus::Connected);
        if !connected {
            return center_note("Connect to a server to use terminals.");
        }
        let Some(tid) = thread_id else {
            return center_note("Select a thread to open a terminal.");
        };
        let Some(term) = terminal_id else {
            return center_note("Opening terminal…");
        };
        let key = (tid.to_string(), term.to_string());
        let Some(session) = self.terminal_state.sessions.get_mut(&key) else {
            return center_note("Opening terminal…");
        };
        if session.status.dead() {
            let (t, tm) = (tid.to_string(), term.to_string());
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .child(div().text_xs().text_color(theme::fg_muted()).child(format!(
                    "{} — {}",
                    session.label,
                    status_label(session)
                )))
                .child(
                    div()
                        .id("terminal-restart")
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .text_xs()
                        .bg(theme::primary())
                        .text_color(rgb(0xffffff))
                        .child("Restart")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _w, cx| {
                                this.ensure_terminal(&t, &tm, true, cx);
                                cx.notify();
                            }),
                        ),
                );
        }
        // VT cursor (row index into `lines`, column) → block overlay drawn
        // inside that row below.
        let cursor = (session.screen.cr, session.screen.cc);
        let rows = session.screen.render_rows();
        let count = rows.len();
        if count == 0 {
            return center_note(match session.status {
                TermStatus::Connecting => "connecting…",
                _ => "(empty)",
            });
        }
        let scroll = self.terminal_state.scroll.clone();
        let measure_tid = tid.to_string();
        let measure_term = term.to_string();
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .px_2()
            .py_1()
            .font_family(theme::FONT_MONO)
            .text_size(theme::code_text_size())
            .line_height(px(LINE_HEIGHT))
            .child(
                // Zero-visual canvas that measures the body's laid-out size
                // every frame and feeds the debounced PTY resize.
                gpui::canvas(
                    move |bounds, window, cx| {
                        let size = bounds.size;
                        let cols = ((f32::from(size.width) - TERM_BODY_PAD_X) / TERM_CHAR_WIDTH)
                            .floor()
                            .clamp(20., 500.) as u16;
                        let rows = (f32::from(size.height) / LINE_HEIGHT)
                            .floor()
                            .clamp(4., 200.) as u16;
                        if let Some(Some(view)) = window.root::<AppView>() {
                            view.update(cx, |this, cx| {
                                this.terminal_note_size(
                                    measure_tid.as_str(),
                                    measure_term.as_str(),
                                    cols,
                                    rows,
                                    cx,
                                );
                            });
                        }
                    },
                    |_bounds, _prepaint, _window, _cx| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(
                uniform_list("terminal-scrollback", count, move |range, _window, _cx| {
                    // render_rows drops trailing blank lines; the cursor can
                    // sit on one, so it lands on the last rendered row.
                    let cursor_row = cursor.0.min(count.saturating_sub(1));
                    range
                        .map(|i| {
                            let row = &rows[i];
                            let styled = StyledText::new(row.text.clone())
                                .with_highlights(row.runs.iter().cloned().collect::<Vec<_>>());
                            let mut el = div()
                                .h(px(LINE_HEIGHT))
                                .relative()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child(styled);
                            // Block cursor at the VT cursor cell, colored
                            // with --terminal-cursor (theme::terminal_cursor).
                            if i == cursor_row {
                                el = el.child(
                                    div()
                                        .absolute()
                                        .left(px(cursor.1.min(200) as f32 * TERM_CHAR_WIDTH))
                                        .top(px(0.))
                                        .w(px(TERM_CHAR_WIDTH))
                                        .h(px(LINE_HEIGHT))
                                        .bg(theme::terminal_cursor()),
                                );
                            }
                            el
                        })
                        .collect()
                })
                .track_scroll(&scroll)
                .h_full(),
            )
    }
}

fn center_note(text: &str) -> Div {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(theme::fg_muted())
        .child(text.to_string())
}

/// Toolbar icon button — the web drawer's `size="icon"` ghost buttons
/// (h-6 w-6, muted glyph, `hover:bg-accent` + fg).
fn drawer_button(button_icon: Icon, id: &'static str) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .w(px(24.))
        .h(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .text_xs()
        .text_color(theme::fg_muted())
        .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
        .child(icon(button_icon, 13.).text_color(theme::fg_muted()))
}

// (kept small on purpose — the drawer is a fixed 280px, not drag-resizable)

// ---------------------------------------------------------------------------
// Right-panel Terminals tab
// ---------------------------------------------------------------------------
//
// Drawer visibility goes through `AppView::terminal_drawer_set_open`
// (lib.rs): closing hands focus back to the composer so keystrokes never
// land on a hidden terminal focus handle; opening relies on
// `focus_pending` to focus the terminal on the next render.

impl AppView {
    pub(crate) fn render_terminal_pane(&self, cx: &mut Context<Self>) -> Div {
        // &self cannot open streams — defer the sync one task tick.
        let connected = matches!(self.state.connection, ConnectionStatus::Connected);
        if !self.terminal_state.meta_want
            || (connected && self.terminal_state.meta_stream.is_none())
        {
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.terminal_state.meta_want = true;
                    this.terminal_sync(cx);
                });
            })
            .detach();
        }

        let st = &self.terminal_state;
        let selected = self.state.selected.clone();
        // Pane subheader — matches the Files pane (web h-9 inline subheader).
        let mut header = div()
            .h(px(36.))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme::fg_muted())
            .child("Terminals")
            .child(div().flex_1());
        header = header.child(
            drawer_button(Icon::Plus, "pane-new-terminal")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _w, cx| {
                        this.terminal_new_for_selected(cx);
                    }),
                )
                .into_any_element(),
        );

        // Merge metadata summaries with local sessions not yet reported.
        // (thread, terminal, label, status, pid, updated_at, thread_title)
        let mut rows: Vec<(String, String, String, String, Option<i64>, String)> = st
            .known
            .values()
            .map(|k| {
                (
                    k.thread_id.clone(),
                    k.terminal_id.clone(),
                    if k.label.is_empty() {
                        label_for(&k.terminal_id)
                    } else {
                        k.label.clone()
                    },
                    k.status.clone(),
                    k.pid,
                    k.updated_at.clone(),
                )
            })
            .collect();
        for ((tid, term), s) in &st.sessions {
            if !st.known.contains_key(&(tid.clone(), term.clone())) {
                rows.push((
                    tid.clone(),
                    term.clone(),
                    s.label.clone(),
                    status_label(s),
                    s.pid,
                    String::new(),
                ));
            }
        }
        rows.sort_by(|a, b| b.5.cmp(&a.5));

        let selected_tid: Option<&str> = selected.as_deref();
        let (mine, others): (Vec<_>, Vec<_>) = rows
            .into_iter()
            .partition(|r| Some(r.0.as_str()) == selected_tid);

        let mut list = div()
            .id("terminals-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_2()
            .gap_1();

        if mine.is_empty() {
            if connected {
                // Web drawer empty state: centered text-sm muted copy + an
                // outline xs "New terminal" button.
                list = list.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .px_4()
                        .py_6()
                        .child(
                            div()
                                .text_color(theme::fg_muted())
                                .child("No terminal sessions for this thread yet."),
                        )
                        .child(
                            div()
                                .id("empty-new-terminal")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(theme::border())
                                .cursor_pointer()
                                .text_xs()
                                .text_color(theme::fg())
                                .hover(|s| s.bg(theme::hover()))
                                .child("New terminal")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _ev, _w, cx| {
                                        this.terminal_new_for_selected(cx);
                                    }),
                                ),
                        ),
                );
            } else {
                list = list.child(
                    div()
                        .p_2()
                        .text_xs()
                        .text_color(theme::fg_muted())
                        .child("Connect to a server to list terminals."),
                );
            }
        }
        for (tid, term, label, status, pid, _upd) in mine {
            list = list.child(self.render_terminal_row(tid, term, label, status, pid, cx));
        }
        if !others.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Other threads"),
            );
            for (tid, term, label, status, pid, _upd) in others {
                let thread_title = self
                    .state
                    .threads
                    .iter()
                    .find(|t| t.id.as_ref() == tid)
                    .map(|t| t.title.to_string());
                list = list.child(self.render_terminal_row_full(
                    tid,
                    term,
                    label,
                    status,
                    pid,
                    thread_title,
                    cx,
                ));
            }
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(header)
            .child(list)
    }

    fn render_terminal_row(
        &self,
        thread_id: String,
        terminal_id: String,
        label: String,
        status: String,
        pid: Option<i64>,
        cx: &mut Context<Self>,
    ) -> Div {
        self.render_terminal_row_full(thread_id, terminal_id, label, status, pid, None, cx)
    }

    fn render_terminal_row_full(
        &self,
        thread_id: String,
        terminal_id: String,
        label: String,
        status: String,
        pid: Option<i64>,
        thread_title: Option<String>,
        cx: &mut Context<Self>,
    ) -> Div {
        let sel_tid = thread_id.clone();
        let sel_term = terminal_id.clone();
        let close_tid = thread_id.clone();
        let close_term = terminal_id.clone();
        let is_active = self.terminal_state.active.get(&thread_id) == Some(&terminal_id);
        let dot = if status == "running" {
            theme::success()
        } else if status.starts_with("exited") || status == "closed" || status == "disconnected" {
            theme::fg_muted()
        } else {
            theme::warning()
        };
        let mut sub = format!("{status}");
        if let Some(pid) = pid {
            sub.push_str(&format!(" · pid {pid}"));
        }
        if let Some(title) = thread_title {
            sub.push_str(&format!(" · {title}"));
        }
        // Web sidebar terminal row: rounded-md pl-1.5 pr-2 text-xs, active
        // `bg-accent text-foreground`, hover `bg-accent/60`.
        div()
            .pl_1p5()
            .pr_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .when(is_active, |el| el.bg(theme::accent()))
            .hover(|s| s.bg(theme::accent_soft()))
            .flex()
            .items_center()
            .gap_2()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, w, cx| {
                    // Click selects the drawer's terminal (and its thread).
                    if Some(sel_tid.as_str()) != this.state.selected.as_deref() {
                        this.select_thread(sel_tid.clone().into(), cx);
                    }
                    this.terminal_state
                        .active
                        .insert(sel_tid.clone(), sel_term.clone());
                    this.terminal_drawer_set_open(true, w, cx);
                    this.ensure_terminal(&sel_tid, &sel_term, false, cx);
                    cx.notify();
                }),
            )
            .child(div().w(px(6.)).h(px(6.)).rounded_full().bg(dot))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg())
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(label),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(sub),
                    ),
            )
            .child(
                div()
                    .id(format!("term-close-{thread_id}-{terminal_id}"))
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .hover(|s| s.bg(theme::accent_soft()).text_color(theme::fg()))
                    .child(icon(Icon::X, 11.).text_color(theme::fg_muted()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _w, cx| {
                            cx.stop_propagation();
                            this.terminal_close(&close_tid, &close_term, cx);
                        }),
                    ),
            )
    }
}

// ---------------------------------------------------------------------------
// Tests — pure Screen logic (no gpui context needed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.cells.iter().map(|c| c.ch).collect::<String>()
    }

    fn visible(screen: &Screen) -> Vec<String> {
        (screen.top..screen.top + screen.rows)
            .map(|r| {
                screen
                    .lines
                    .get(r)
                    .map(text)
                    .unwrap_or_default()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn plain_output_and_newlines() {
        let mut s = Screen::new(80, 24);
        s.feed("hello\r\nworld\n");
        let v = visible(&s);
        assert_eq!(v[0], "hello");
        assert_eq!(v[1], "world");
    }

    #[test]
    fn carriage_return_overwrites_line() {
        let mut s = Screen::new(80, 24);
        s.feed("building...\rdone        ");
        assert_eq!(visible(&s)[0], "done");
    }

    #[test]
    fn progress_bar_style_redraw() {
        let mut s = Screen::new(80, 24);
        // npm/cargo style: \r rewrites, then a cursor-up + erase-line pass.
        s.feed("line1\r\n");
        s.feed("downloading  10%\rdownloading  90%\r");
        s.feed("complete            \r\n");
        let v = visible(&s);
        assert_eq!(v[0], "line1");
        assert_eq!(v[1], "complete");
    }

    #[test]
    fn cursor_up_and_erase_line() {
        let mut s = Screen::new(80, 24);
        s.feed("aaa\r\nbbb\r\nccc");
        // Up 1 to "bbb", erase the line, home, write X → middle becomes X.
        s.feed("\x1b[1A\x1b[2K\x1b[GX");
        let v = visible(&s);
        assert_eq!(v[0], "aaa");
        assert_eq!(v[1], "X");
        assert_eq!(v[2], "ccc");
    }

    #[test]
    fn sgr_color_run() {
        let mut s = Screen::new(80, 24);
        s.feed("\x1b[31mred\x1b[0m plain");
        let line = &s.lines[s.top];
        assert_eq!(line.cells[0].a.fg, Col::Idx(1));
        assert_eq!(line.cells[4].a, Attr::default());
        // 256-color and true-color.
        s.feed("\x1b[38;5;196m\x1b[48;2;1;2;3m!");
        let line = &s.lines[s.top];
        assert_eq!(line.cells[9].a.fg, Col::Idx(196));
        assert_eq!(line.cells[9].a.bg, Col::Rgb(1, 2, 3));
    }

    #[test]
    fn scroll_produces_scrollback() {
        let mut s = Screen::new(80, 4);
        for i in 0..10 {
            s.feed(&format!("line{i}\r\n"));
        }
        assert!(s.top >= 6, "scrollback should hold scrolled-off lines");
        let v = visible(&s);
        // The final \r\n scrolled once more — line9 sits one above the bottom.
        assert_eq!(v[2], "line9");
    }

    #[test]
    fn scrollback_is_capped() {
        let mut s = Screen::new(80, 4);
        let chunk = "x".repeat(4096);
        for _ in 0..(SCROLLBACK_CAP / 4096 + 64) {
            s.feed(&chunk);
            s.feed("\r\n");
        }
        assert!(s.scroll_bytes <= SCROLLBACK_CAP);
    }

    #[test]
    fn escape_split_across_chunks() {
        let mut s = Screen::new(80, 24);
        s.feed("\x1b[3");
        s.feed("1mred\x1b[0m");
        let line = &s.lines[s.top];
        assert_eq!(line.cells[0].a.fg, Col::Idx(1));
        assert_eq!(text(line).trim_end(), "red");
    }

    #[test]
    fn alternate_screen_restores() {
        let mut s = Screen::new(80, 4);
        s.feed("normal\r\n");
        s.feed("\x1b[?1049h");
        s.feed("tui stuff\r\n");
        s.feed("\x1b[?1049l");
        assert_eq!(visible(&s)[0], "normal");
    }

    #[test]
    fn wrap_continues_on_next_line() {
        let mut s = Screen::new(8, 4);
        s.feed("12345678tail");
        assert_eq!(text(&s.lines[s.top]), "12345678");
        assert_eq!(text(&s.lines[s.top + 1]).trim_end(), "tail");
    }

    #[test]
    fn clear_resets_screen() {
        let mut s = Screen::new(80, 4);
        s.feed("junk\r\nmore\r\n");
        s.clear_all();
        assert_eq!(s.lines.len(), 0);
        assert_eq!(s.top, 0);
    }

    #[test]
    fn decstbm_region_scrolls_independently() {
        let mut s = Screen::new(80, 4);
        s.feed("top\r\na\r\nb\r\nc");
        // Region = rows 2-4 (1-based). Home into row 2, overwrite "a".
        s.feed("\x1b[2;4r\x1b[2;1Ha1\r\n");
        let v = visible(&s);
        assert_eq!(v[0], "top");
        assert_eq!(v[1], "a1");
        assert_eq!(v[2], "b");
        assert_eq!(v[3], "c");
    }
}
