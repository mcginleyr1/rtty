use std::sync::Arc;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use crate::cell::{Attrs, Cell, Color};
use crate::dispatch::Performer;
use crate::event::{Event, Mark, MarkKind, ModeChange};
use crate::grid::Grid;
use crate::modes::{CursorStyle, Modes};
use crate::row::{CLUSTER_MAX, Extra, Hyperlink, Row};

/// An RGB colour from or for the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Palette slots past the 256 indexed colours, as OSC 10, 11 and 12 name
/// them.
pub const FOREGROUND: usize = 256;
pub const BACKGROUND: usize = 257;
pub const CURSOR: usize = 258;
pub(crate) const PALETTE_LEN: usize = 259;

/// Which way the host's colours lean, for contour's `?996` query and
/// `?2031` reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorScheme {
    Dark,
    Light,
}

/// The rendition new cells are written in.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Pen {
    pub(crate) fg: Color,
    pub(crate) bg: Color,
    pub(crate) attrs: Attrs,
    pub(crate) underline: Color,
    pub(crate) link: Option<Arc<Hyperlink>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Charset {
    #[default]
    Ascii,
    DecGraphics,
    Uk,
}

impl Charset {
    fn map(self, c: char) -> char {
        match (self, c) {
            (Charset::Ascii, c) => c,
            (Charset::Uk, '#') => '£',
            (Charset::Uk, c) => c,
            (Charset::DecGraphics, '_'..='~') => DEC_GRAPHICS[usize::from(c as u8 - b'_')],
            (Charset::DecGraphics, c) => c,
        }
    }
}

/// DEC Special Graphics for `_` through `~`.
const DEC_GRAPHICS: [char; 32] = [
    ' ', '◆', '▒', '␉', '␌', '␍', '␊', '°', '±', '␤', '␋', '┘', '┐', '┌', '└', '┼', '⎺', '⎻', '─',
    '⎼', '⎽', '├', '┤', '┴', '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
];

/// G0–G3, which of them GL invokes, and a pending single shift.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Charsets {
    pub(crate) g: [Charset; 4],
    pub(crate) gl: u8,
    pub(crate) single_shift: Option<u8>,
}

/// Where the next character goes and how it is written.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Cursor {
    pub(crate) row: u16,
    pub(crate) col: u16,
    /// A character filled the last column: the next one wraps first.
    pub(crate) pending_wrap: bool,
    pub(crate) pen: Pen,
    pub(crate) charsets: Charsets,
}

/// What DECSC saves.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Saved {
    pub(crate) cursor: Cursor,
    pub(crate) origin: bool,
}

/// Index into per-screen state.
pub(crate) const PRIMARY: usize = 0;
pub(crate) const ALTERNATE: usize = 1;

const KITTY_STACK_MAX: usize = 16;
const TITLE_STACK_MAX: usize = 16;
const EVENTS_MAX: usize = 4096;

/// A terminal's state, fed the bytes a program writes. It does no IO:
/// `advance` mutates the grid and queues events — replies for the pty among
/// them — that the host drains with `take_events`.
pub struct Terminal {
    parser: vte::Parser,
    /// The start of a UTF-8 character the last `advance` ended in.
    held: Vec<u8>,
    /// The printed characters gathered for one write, kept for its
    /// allocation.
    run: Vec<char>,
    pub(crate) grid: Grid,
    pub(crate) cursor: Cursor,
    /// DECSC slots, one per screen.
    pub(crate) saved: [Option<Saved>; 2],
    pub(crate) modes: Modes,
    /// Kitty keyboard flag stacks, one per screen.
    pub(crate) kitty: [Vec<u8>; 2],
    pub(crate) tabs: Vec<bool>,
    /// The scroll region, inclusive rows.
    pub(crate) top: u16,
    pub(crate) bottom: u16,
    pub(crate) title: String,
    pub(crate) icon_name: String,
    title_stack: Vec<String>,
    cwd: Option<String>,
    pub(crate) cursor_style: Option<CursorStyle>,
    /// Colours the program set (OSC 4, 10–12), over the host's.
    pub(crate) colors: Box<[Option<Rgb>; PALETTE_LEN]>,
    host_colors: Box<[Option<Rgb>; PALETTE_LEN]>,
    scheme: Option<ColorScheme>,
    cell_pixels: Option<(u16, u16)>,
    events: Vec<Event>,
    /// What REP repeats.
    pub(crate) last_printed: Option<char>,
    /// The last thing in the stream was a printed character, which an emoji
    /// sequence's next character may join.
    pub(crate) joinable: bool,
    /// A DCS string being collected: its kind and bytes.
    pub(crate) dcs: Option<(crate::dispatch::DcsKind, Vec<u8>)>,
}

impl Terminal {
    /// A blank terminal of `rows`×`cols` (each at least 1) keeping up to
    /// `scrollback` rows of history.
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Terminal {
        let (rows, cols) = (rows.max(1), cols.max(1));
        Terminal {
            parser: vte::Parser::new(),
            held: Vec::new(),
            run: Vec::new(),
            grid: Grid::new(rows, cols, scrollback),
            cursor: Cursor::default(),
            saved: [None, None],
            modes: Modes::default(),
            kitty: [Vec::new(), Vec::new()],
            tabs: default_tabs(cols),
            top: 0,
            bottom: rows - 1,
            title: String::new(),
            icon_name: String::new(),
            title_stack: Vec::new(),
            cwd: None,
            cursor_style: None,
            colors: Box::new([None; PALETTE_LEN]),
            host_colors: Box::new([None; PALETTE_LEN]),
            scheme: None,
            cell_pixels: None,
            events: Vec::new(),
            last_printed: None,
            joinable: false,
            dcs: None,
        }
    }

    /// Feed the program's output. A sequence split across calls is resumed.
    ///
    /// A UTF-8 character split across calls is held back until it is whole:
    /// `vte` 0.15 resumes one itself, but can then swallow the character
    /// after it.
    pub fn advance(&mut self, mut bytes: &[u8]) {
        let mut parser = std::mem::take(&mut self.parser);
        let held = if let Some(&lead) = self.held.first() {
            let take = (utf8_len(lead) - self.held.len()).min(bytes.len());
            self.held.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.held.len() == utf8_len(lead) {
                std::mem::take(&mut self.held)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let whole = whole_chars(bytes);
        let run = std::mem::take(&mut self.run);
        let mut performer = Performer(self, run);
        parser.advance(&mut performer, &held);
        parser.advance(&mut performer, &bytes[..whole]);
        performer.flush();
        let Performer(term, run) = performer;
        term.run = run;
        term.held.extend_from_slice(&bytes[whole..]);
        term.parser = parser;
    }

    /// The events queued since the last call, in stream order. Drain after
    /// every `advance`: past 4096 the oldest are dropped.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// `(rows, cols)`.
    pub fn size(&self) -> (u16, u16) {
        (self.grid.rows, self.grid.cols)
    }

    /// The oldest row still held. Every row ever on the primary screen has
    /// an absolute index; these never decrease (see `resize`).
    pub fn oldest_row(&self) -> u64 {
        self.grid.oldest
    }

    /// The absolute index of the visible screen's first row: the number of
    /// rows the primary screen has committed to its scrollback. It never
    /// decreases.
    pub fn screen_top(&self) -> u64 {
        self.grid.screen_top()
    }

    /// The row with absolute index `index`: the primary's scrollback below
    /// `screen_top`, the visible screen (either one) from there.
    pub fn row(&self, index: u64) -> Option<&Row> {
        self.grid.row(index)
    }

    /// The held rows with absolute index in `from..to`, with their indices.
    pub fn rows(&self, from: u64, to: u64) -> impl Iterator<Item = (u64, &Row)> {
        let end = to.min(self.screen_top() + u64::from(self.grid.rows));
        (from.max(self.grid.oldest)..end).filter_map(|index| Some((index, self.row(index)?)))
    }

    /// Visible row `row` (0 at the top).
    pub fn screen_row(&self, row: u16) -> &Row {
        self.grid.line(row)
    }

    /// `(row, col)` on the visible screen. After a character filled the last
    /// column the cursor stays on it until the next one wraps.
    pub fn cursor_position(&self) -> (u16, u16) {
        (self.cursor.row, self.cursor.col)
    }

    /// A character filled the last column: the next one wraps first, if
    /// autowrap is on, or overwrites it.
    pub fn wrap_pending(&self) -> bool {
        self.cursor.pending_wrap
    }

    pub fn cursor_style(&self) -> Option<CursorStyle> {
        self.cursor_style
    }

    pub fn modes(&self) -> &Modes {
        &self.modes
    }

    /// The active screen's kitty keyboard flags.
    pub fn kitty_keyboard(&self) -> u8 {
        self.kitty[self.screen()].last().copied().unwrap_or(0)
    }

    /// The scroll region, inclusive rows.
    pub fn scroll_region(&self) -> (u16, u16) {
        (self.top, self.bottom)
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn icon_name(&self) -> &str {
        &self.icon_name
    }

    /// The directory the shell last reported (OSC 7).
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// The OSC 133 marks on the primary screen's rows `from..to` still held
    /// (its screen included while the alternate one is up), oldest first.
    /// Marks travel with their rows through scrolling and reflow.
    pub fn marks(&self, from: u64, to: u64) -> Vec<Mark> {
        let oldest = self.grid.oldest;
        let start = from.saturating_sub(oldest).min(self.grid.primary.len() as u64) as usize;
        let end = to.saturating_sub(oldest).min(self.grid.primary.len() as u64) as usize;
        (start..end.max(start))
            .flat_map(|i| {
                let row = oldest + i as u64;
                self.grid.primary[i].marks.iter().map(move |&(col, kind)| Mark { kind, row, col })
            })
            .collect()
    }

    /// The visible rows changed since the last call, top to bottom.
    pub fn take_damage(&mut self) -> Vec<u16> {
        self.grid.take_damage()
    }

    /// The colour palette slot `index` shows: what the program set, else
    /// what the host set.
    pub fn color(&self, index: usize) -> Option<Rgb> {
        self.colors.get(index).copied().flatten().or(self.host_colors.get(index).copied().flatten())
    }

    /// What the program set slot `index` to (OSC 4, 10–12), if anything.
    pub fn color_override(&self, index: usize) -> Option<Rgb> {
        self.colors.get(index).copied().flatten()
    }

    /// The host's colour for palette slot `index`, which colour queries
    /// answer with when the program set none. Unset slots are not answered.
    pub fn set_host_color(&mut self, index: usize, rgb: Option<Rgb>) {
        if let Some(slot) = self.host_colors.get_mut(index) {
            *slot = rgb;
        }
    }

    /// The host's light or dark leaning; a program that turned on `?2031`
    /// is sent a report when it changes.
    pub fn set_color_scheme(&mut self, scheme: Option<ColorScheme>) {
        let changed = scheme != self.scheme;
        self.scheme = scheme;
        if changed && self.modes.color_scheme_reports {
            self.report_color_scheme();
        }
    }

    /// The cell size in pixels, for XTWINOPS 14 and 16.
    pub fn set_cell_pixels(&mut self, width: u16, height: u16) {
        self.cell_pixels = Some((width, height));
    }

    /// The visible screen as text: rows joined by newlines, a soft-wrapped
    /// row run on into the next, trailing spaces and blank rows trimmed.
    pub fn contents(&self) -> String {
        let top = self.screen_top();
        let end = top + u64::from(self.grid.rows) - 1;
        let mut text = self.text((top, 0), (end, self.grid.cols - 1));
        text.truncate(text.trim_end_matches('\n').len());
        text
    }

    /// The text between two absolute positions, inclusive, as a selection
    /// copies it: soft-wrapped rows joined, others ended by a newline,
    /// trailing spaces dropped.
    pub fn text(&self, start: (u64, u16), end: (u64, u16)) -> String {
        let mut text = String::new();
        for (index, row) in self.rows(start.0, end.0 + 1) {
            let first = if index == start.0 { usize::from(start.1) } else { 0 };
            let last = if index == end.0 { usize::from(end.1) + 1 } else { row.cells.len() };
            let from = text.len();
            row.push_text(first..last, &mut text);
            let continues = row.wrapped && last >= row.cells.len();
            if !continues {
                text.truncate(from + text[from..].trim_end_matches(' ').len());
                if index != end.0 {
                    text.push('\n');
                }
            }
        }
        text
    }

    pub(crate) fn screen(&self) -> usize {
        if self.grid.alt_active { ALTERNATE } else { PRIMARY }
    }

    pub(crate) fn push_event(&mut self, event: Event) {
        if event == Event::Bell && self.events.last() == Some(&Event::Bell) {
            return;
        }
        if self.events.len() >= EVENTS_MAX {
            self.events.remove(0);
        }
        self.events.push(event);
    }

    pub(crate) fn reply(&mut self, bytes: impl Into<Vec<u8>>) {
        self.push_event(Event::Reply(bytes.into()));
    }

    pub(crate) fn mode_changed(&mut self, change: ModeChange) {
        self.push_event(Event::Mode(change));
    }

    pub(crate) fn report_color_scheme(&mut self) {
        if let Some(scheme) = self.scheme {
            let code = if scheme == ColorScheme::Dark { 1 } else { 2 };
            self.reply(format!("\x1b[?997;{code}n"));
        }
    }

    pub(crate) fn cell_pixels(&self) -> Option<(u16, u16)> {
        self.cell_pixels
    }

    pub(crate) fn set_cwd(&mut self, cwd: String) {
        self.cwd = Some(cwd.clone());
        self.push_event(Event::Cwd(cwd));
    }

    pub(crate) fn set_title(&mut self, title: String) {
        self.title.clone_from(&title);
        self.push_event(Event::Title(title));
    }

    pub(crate) fn push_title(&mut self) {
        if self.title_stack.len() >= TITLE_STACK_MAX {
            self.title_stack.remove(0);
        }
        self.title_stack.push(self.title.clone());
    }

    pub(crate) fn pop_title(&mut self) {
        if let Some(title) = self.title_stack.pop() {
            self.set_title(title);
        }
    }

    // Writing characters.

    /// Print a run of characters with nothing else between them.
    pub(crate) fn print_run(&mut self, run: &[char]) {
        let mut rest = run;
        while let Some((&c, tail)) = rest.split_first() {
            match self.write_simple(rest) {
                0 => {
                    self.print(c);
                    rest = tail;
                }
                n => rest = &rest[n..],
            }
        }
    }

    /// Write the characters `run` starts with that need nothing but a cell
    /// or two — no character set, no insert mode, nothing that joins a
    /// cluster — as many as fit on the cursor's row, in one go. How many
    /// were written: 0 when the first is not that simple.
    fn write_simple(&mut self, run: &[char]) -> usize {
        let charsets = &self.cursor.charsets;
        let plain = charsets.single_shift.is_none()
            && charsets.g[usize::from(charsets.gl)] == Charset::Ascii
            && !self.modes.insert
            && self.last_printed != Some('\u{200D}');
        if !plain || self.cursor.pending_wrap && !self.modes.autowrap {
            return 0;
        }
        let cols = usize::from(self.grid.cols);
        let width = |c: char| match c {
            ' '..='~' => 1,
            _ if c.is_ascii() || regional_indicator(c) || emoji_modifier(c) => 0,
            _ if cols < 2 => 0,
            _ => c.width().unwrap_or(0),
        };
        let first = run.first().map_or(0, |&c| width(c));
        if first == 0 {
            return 0;
        }
        if self.cursor.pending_wrap {
            self.wrap();
        }
        let (row, col) = (self.cursor.row, usize::from(self.cursor.col));
        // Up to the row's end, or to the first character that needs more.
        let (mut n, mut end) = (0, col);
        for &c in run {
            match width(c) {
                0 => break,
                w if end + w > cols => break,
                w => {
                    n += 1;
                    end += w;
                }
            }
        }
        if n == 0 {
            // A wide character at the last column: it wraps first.
            return 0;
        }
        let pen = &self.cursor.pen;
        let line = self.grid.line_mut(row);
        let split = line.cells[col].width() != 1 || line.cells[end - 1].width() != 1;
        let unpads = col <= 1
            && !(col == 0 && first == 2)
            && line.cells[col..end.min(2)].iter().any(|cell| cell.width() != 1);
        if split {
            line.split_wide(col..end);
        }
        let extra = line.pen_extra(pen.underline, &pen.link);
        let mut at = col;
        for &c in &run[..n] {
            let mut cell = Cell::new(c, pen.fg, pen.bg, pen.attrs, extra);
            if c.is_ascii() || width(c) == 1 {
                line.cells[at] = cell;
                at += 1;
            } else {
                cell.set_wide();
                let mut spacer = Cell::new(' ', pen.fg, pen.bg, pen.attrs, extra);
                spacer.set_spacer();
                line.cells[at] = cell;
                line.cells[at + 1] = spacer;
                at += 2;
            }
        }
        line.touch(end);
        line.wrapped &= end < cols;
        if end < cols {
            self.cursor.col = end as u16;
        } else {
            self.cursor.col = (cols - 1) as u16;
            self.cursor.pending_wrap = true;
        }
        if unpads && let Some(last) = self.grid.above_mut(row).and_then(|row| row.cells.last_mut())
        {
            last.clear_pad();
        }
        self.last_printed = Some(run[n - 1]);
        self.joinable = true;
        n
    }

    pub(crate) fn print(&mut self, c: char) {
        let charsets = &mut self.cursor.charsets;
        let set = match charsets.single_shift {
            None => charsets.g[usize::from(charsets.gl)],
            Some(shift) => {
                charsets.single_shift = None;
                charsets.g[usize::from(shift)]
            }
        };
        self.put_char(if set == Charset::Ascii { c } else { set.map(c) });
    }

    /// Write `c`, already through the character sets: REP repeats from here.
    pub(crate) fn put_char(&mut self, c: char) {
        let width = if c.is_ascii() {
            if c < ' ' || c == '\x7f' {
                return;
            }
            1
        } else {
            match c.width() {
                Some(width) => width as u16,
                None => return,
            }
        };
        // Only a cluster ending in a ZWJ takes more than a modifier or the
        // second half of a flag.
        let may_join = self.joinable
            && (self.last_printed == Some('\u{200D}')
                || regional_indicator(c)
                || emoji_modifier(c));
        self.last_printed = Some(c);
        if width == 0 {
            self.join_zero_width(c);
        } else if !may_join || !self.join_cluster(c) {
            self.write(c, width);
        }
        self.joinable = true;
    }

    /// The cell a combining character joins: the one before the cursor, or
    /// under it after a character filled the last column (or at column 0).
    fn previous_cell(&self) -> usize {
        let mut col = usize::from(self.cursor.col);
        if !self.cursor.pending_wrap {
            col = col.saturating_sub(1);
        }
        if col > 0 && self.grid.line(self.cursor.row).cells[col].is_spacer() {
            col -= 1;
        }
        col
    }

    fn join_zero_width(&mut self, c: char) {
        let col = self.previous_cell();
        // A pad holds no character to combine with.
        if !self.grid.line(self.cursor.row).cells[col].is_pad() {
            self.grid.line_mut(self.cursor.row).extend_cluster(col, c);
        }
    }

    /// Join `c` to the cluster printed just before it (nothing else in
    /// between) when Unicode makes it part of it — a ZWJ sequence, an emoji
    /// modifier, the second half of a flag. A narrow cluster that gains a
    /// character of its own width widens to two columns, as `wcwidth` sums
    /// it; at the last column it moves to the next row to, as a wide
    /// character that does not fit does.
    fn join_cluster(&mut self, c: char) -> bool {
        if self.cursor.col == 0 && !self.cursor.pending_wrap {
            return false;
        }
        let row = self.cursor.row;
        let col = self.previous_cell();
        let line = self.grid.line(row);
        let mut cluster = String::new();
        line.push_grapheme(col, &mut cluster);
        // A full cluster takes nothing more: written on its own instead.
        if !joins(&cluster, c) || cluster.chars().nth(CLUSTER_MAX - 1).is_some() {
            return false;
        }
        let narrow = line.cells[col].width() == 1;
        let cols = usize::from(self.grid.cols);
        if narrow && col + 1 >= cols && !(self.modes.autowrap && cols >= 2) {
            // Nowhere to widen to: written on its own instead.
            return false;
        }
        let line = self.grid.line_mut(row);
        line.extend_cluster(col, c);
        let cell = line.cells[col];
        let extra = line.extra(&cell).cloned();
        if narrow && col + 1 < cols {
            self.widen(col, cell, extra);
        } else if narrow && self.modes.autowrap && cols >= 2 {
            line.orphan(col);
            line.cells[col].set_pad();
            self.wrap();
            self.widen(0, cell, extra);
        }
        true
    }

    /// Lay the cluster `cell` (side entry `extra`) over two columns from
    /// `col` of the cursor's row, and move the cursor past it.
    fn widen(&mut self, col: usize, cell: Cell, extra: Option<Extra>) {
        let cols = usize::from(self.grid.cols);
        let line = self.grid.line_mut(self.cursor.row);
        line.split_wide(col..col + 2);
        let (underline, link) = extra
            .as_ref()
            .map_or((Color::Default, None), |extra| (extra.underline, extra.link.clone()));
        let mut wide = cell;
        wide.extra = extra.map_or(0, |extra| line.intern(extra));
        wide.set_wide();
        let mut spacer =
            Cell::new(' ', cell.fg, cell.bg, cell.attrs(), line.pen_extra(underline, &link));
        spacer.set_spacer();
        line.cells[col] = wide;
        line.cells[col + 1] = spacer;
        line.touch(col + 2);
        line.wrapped &= col + 2 < cols;
        if col + 2 < cols {
            self.cursor.col = (col + 2) as u16;
        } else {
            self.cursor.col = (cols - 1) as u16;
            self.cursor.pending_wrap = true;
        }
    }

    fn write(&mut self, c: char, width: u16) {
        let cols = self.grid.cols;
        if self.cursor.pending_wrap && self.modes.autowrap {
            self.wrap();
        }
        // A wide character has no room on a one-column screen.
        let (c, width) = if cols < 2 && width == 2 { ('\u{FFFD}', 1) } else { (c, width) };
        if width == 2 && self.cursor.col + 1 >= cols {
            if !self.modes.autowrap {
                self.cursor.pending_wrap = true;
                return;
            }
            let pen = self.cursor.pen.clone();
            let (row, col) = (self.cursor.row, usize::from(self.cursor.col));
            let line = self.grid.line_mut(row);
            line.split_wide(col..col + 1);
            let extra = line.pen_extra(pen.underline, &pen.link);
            line.cells[col] = Cell::new(' ', pen.fg, pen.bg, pen.attrs, extra);
            line.cells[col].set_pad();
            line.touch(col + 1);
            self.wrap();
        }
        let (row, col) = (self.cursor.row, usize::from(self.cursor.col));
        let end = col + usize::from(width);
        let pen = &self.cursor.pen;
        let line = self.grid.line_mut(row);
        if self.modes.insert {
            line.insert_blanks(col, usize::from(width), pen.bg);
        }
        // Writing over part of a wide character, rarely: it loses its other
        // half, and the pad above that made room for it is going unless
        // another wide one takes its place.
        let split = line.cells[col].width() != 1 || width == 2 && line.cells[col + 1].width() != 1;
        let unpads = split && {
            let unpads = col <= 1
                && !(col == 0 && width == 2)
                && line.cells[col..end.min(2)].iter().any(|cell| cell.width() != 1);
            line.split_wide(col..end);
            unpads
        };
        let extra = line.pen_extra(pen.underline, &pen.link);
        let mut cell = Cell::new(c, pen.fg, pen.bg, pen.attrs, extra);
        if width == 2 {
            cell.set_wide();
            let mut spacer = Cell::new(' ', pen.fg, pen.bg, pen.attrs, extra);
            spacer.set_spacer();
            line.cells[col + 1] = spacer;
        }
        line.cells[col] = cell;
        line.touch(end);
        // A soft wrap lives in the last cell: rewritten, the row ends there
        // until the next character wraps it again.
        line.wrapped &= end < usize::from(cols);
        if end < usize::from(cols) {
            self.cursor.col = end as u16;
        } else {
            self.cursor.col = cols - 1;
            self.cursor.pending_wrap = true;
        }
        if unpads && let Some(last) = self.grid.above_mut(row).and_then(|row| row.cells.last_mut())
        {
            last.clear_pad();
        }
    }

    /// Soft-wrap onto the next row.
    fn wrap(&mut self) {
        self.grid.line_mut(self.cursor.row).wrapped = true;
        self.index();
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    // Cursor movement.

    /// Down a row, scrolling the region when at its bottom.
    pub(crate) fn index(&mut self) {
        if self.cursor.row == self.bottom {
            self.grid.scroll_up(self.top, self.bottom, 1, self.cursor.pen.bg);
        } else if self.cursor.row + 1 < self.grid.rows {
            self.cursor.row += 1;
        }
        self.cursor.pending_wrap = false;
    }

    pub(crate) fn reverse_index(&mut self) {
        if self.cursor.row == self.top {
            self.grid.scroll_down(self.top, self.bottom, 1, self.cursor.pen.bg);
        } else {
            self.cursor.row = self.cursor.row.saturating_sub(1);
        }
        self.cursor.pending_wrap = false;
    }

    pub(crate) fn linefeed(&mut self) {
        self.index();
        if self.modes.newline {
            self.carriage_return();
        }
    }

    pub(crate) fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    pub(crate) fn backspace(&mut self) {
        self.cursor.col = self.cursor.col.saturating_sub(1);
        self.cursor.pending_wrap = false;
    }

    /// CUP: `row` and `col` 0-based, `row` relative to the scroll region in
    /// origin mode.
    pub(crate) fn goto(&mut self, row: u16, col: u16) {
        let (base, last) =
            if self.modes.origin { (self.top, self.bottom) } else { (0, self.grid.rows - 1) };
        self.cursor.row = base.saturating_add(row).min(last);
        self.cursor.col = col.min(self.grid.cols - 1);
        self.cursor.pending_wrap = false;
    }

    /// The row as CUP numbers it: relative to the region in origin mode.
    pub(crate) fn addressed_row(&self) -> u16 {
        if self.modes.origin { self.cursor.row.saturating_sub(self.top) } else { self.cursor.row }
    }

    /// CUU: stops at the top margin from inside the region.
    pub(crate) fn cursor_up(&mut self, n: u16) {
        let limit = if self.cursor.row >= self.top { self.top } else { 0 };
        self.cursor.row = self.cursor.row.saturating_sub(n).max(limit);
        self.cursor.pending_wrap = false;
    }

    /// CUD: stops at the bottom margin from inside the region.
    pub(crate) fn cursor_down(&mut self, n: u16) {
        let limit = if self.cursor.row <= self.bottom { self.bottom } else { self.grid.rows - 1 };
        self.cursor.row = self.cursor.row.saturating_add(n).min(limit);
        self.cursor.pending_wrap = false;
    }

    pub(crate) fn cursor_forward(&mut self, n: u16) {
        self.cursor.col = self.cursor.col.saturating_add(n).min(self.grid.cols - 1);
        self.cursor.pending_wrap = false;
    }

    pub(crate) fn cursor_backward(&mut self, n: u16) {
        self.cursor.col = self.cursor.col.saturating_sub(n);
        self.cursor.pending_wrap = false;
    }

    /// HT, CHT: to the next tab stop, or the last column.
    pub(crate) fn tab(&mut self, n: u16) {
        for _ in 0..n {
            let next = (usize::from(self.cursor.col) + 1..usize::from(self.grid.cols))
                .find(|&col| self.tabs[col])
                .unwrap_or(usize::from(self.grid.cols) - 1);
            self.cursor.col = next as u16;
        }
    }

    /// CBT: back to the previous tab stop, or column 0.
    pub(crate) fn back_tab(&mut self, n: u16) {
        for _ in 0..n {
            let previous =
                (0..usize::from(self.cursor.col)).rev().find(|&col| self.tabs[col]).unwrap_or(0);
            self.cursor.col = previous as u16;
        }
        self.cursor.pending_wrap = false;
    }

    pub(crate) fn set_tab_stop(&mut self) {
        self.tabs[usize::from(self.cursor.col)] = true;
    }

    pub(crate) fn clear_tab_stops(&mut self, all: bool) {
        if all {
            self.tabs.fill(false);
        } else {
            self.tabs[usize::from(self.cursor.col)] = false;
        }
    }

    // Erasing, inserting and deleting.

    /// ED: 0 below, 1 above, 2 all (the primary screen's contents scroll
    /// into its scrollback first), 3 the scrollback (from the primary
    /// screen only: the alternate one has none).
    pub(crate) fn erase_display(&mut self, mode: u16) {
        let bg = self.cursor.pen.bg;
        let (row, col) = (self.cursor.row, usize::from(self.cursor.col));
        let cols = usize::from(self.grid.cols);
        match mode {
            0 => {
                self.grid.line_mut(row).erase(col..cols, bg);
                for below in row + 1..self.grid.rows {
                    self.reset_line(below, bg);
                }
            }
            1 => {
                for above in 0..row {
                    self.reset_line(above, bg);
                }
                self.grid.line_mut(row).erase(0..col + 1, bg);
            }
            2 => {
                if !self.grid.alt_active {
                    let used = (0..self.grid.rows).rev().find(|&r| !self.grid.line(r).is_empty());
                    if let Some(used) = used {
                        self.grid.scroll_up(0, self.grid.rows - 1, used + 1, bg);
                    }
                }
                for r in 0..self.grid.rows {
                    self.reset_line(r, bg);
                }
            }
            3 if !self.grid.alt_active => self.grid.clear_history(),
            _ => {}
        }
    }

    fn reset_line(&mut self, row: u16, bg: Color) {
        let cols = self.grid.cols;
        let line = self.grid.line_mut(row);
        line.reset(cols, bg);
    }

    /// EL: 0 right, 1 left, 2 all. With a wrap pending, the last column's
    /// character is kept from a right erase.
    pub(crate) fn erase_line(&mut self, mode: u16) {
        let bg = self.cursor.pen.bg;
        let (row, col) = (self.cursor.row, usize::from(self.cursor.col));
        let cols = usize::from(self.grid.cols);
        let range = match mode {
            0 if self.cursor.pending_wrap => return,
            0 => col..cols,
            1 => 0..col + 1,
            2 => 0..cols,
            _ => return,
        };
        self.grid.line_mut(row).erase(range, bg);
    }

    /// ECH.
    pub(crate) fn erase_chars(&mut self, n: u16) {
        let col = usize::from(self.cursor.col);
        let bg = self.cursor.pen.bg;
        self.grid.line_mut(self.cursor.row).erase(col..col + usize::from(n), bg);
    }

    /// ICH.
    pub(crate) fn insert_chars(&mut self, n: u16) {
        let col = usize::from(self.cursor.col);
        let bg = self.cursor.pen.bg;
        self.grid.line_mut(self.cursor.row).insert_blanks(col, usize::from(n), bg);
    }

    /// DCH.
    pub(crate) fn delete_chars(&mut self, n: u16) {
        let col = usize::from(self.cursor.col);
        let bg = self.cursor.pen.bg;
        self.grid.line_mut(self.cursor.row).delete_cells(col, usize::from(n), bg);
    }

    /// IL: from the cursor's row down, inside the region only.
    pub(crate) fn insert_lines(&mut self, n: u16) {
        if (self.top..=self.bottom).contains(&self.cursor.row) {
            self.grid.scroll_down(self.cursor.row, self.bottom, n, self.cursor.pen.bg);
            self.cursor.col = 0;
            self.cursor.pending_wrap = false;
        }
    }

    /// DL.
    pub(crate) fn delete_lines(&mut self, n: u16) {
        if (self.top..=self.bottom).contains(&self.cursor.row) {
            // From the top row, the deleted rows go into the scrollback.
            self.grid.scroll_up(self.cursor.row, self.bottom, n, self.cursor.pen.bg);
            self.cursor.col = 0;
            self.cursor.pending_wrap = false;
        }
    }

    /// SU.
    pub(crate) fn scroll_up(&mut self, n: u16) {
        self.grid.scroll_up(self.top, self.bottom, n, self.cursor.pen.bg);
    }

    /// SD.
    pub(crate) fn scroll_down(&mut self, n: u16) {
        self.grid.scroll_down(self.top, self.bottom, n, self.cursor.pen.bg);
    }

    /// DECSTBM, 1-based inclusive; homes the cursor.
    pub(crate) fn set_scroll_region(&mut self, top: u16, bottom: Option<u16>) {
        let bottom = bottom.unwrap_or(self.grid.rows).min(self.grid.rows);
        let top = top.max(1);
        if top >= bottom {
            return;
        }
        self.top = top - 1;
        self.bottom = bottom - 1;
        self.goto(0, 0);
    }

    /// DECALN: the screen filled with `E`, margins reset, cursor home.
    pub(crate) fn screen_alignment(&mut self) {
        for row in 0..self.grid.rows {
            let line = self.grid.line_mut(row);
            line.reset(line.cells.len() as u16, Color::Default);
            line.cells.iter_mut().for_each(|cell| cell.ch = 'E');
            line.touch(line.cells.len());
        }
        self.top = 0;
        self.bottom = self.grid.rows - 1;
        self.goto(0, 0);
    }

    // Saving, screens and resets.

    pub(crate) fn save_cursor(&mut self) {
        let saved = Saved { cursor: self.cursor.clone(), origin: self.modes.origin };
        self.saved[self.screen()] = Some(saved);
    }

    pub(crate) fn restore_cursor(&mut self) {
        let saved = self.saved[self.screen()].clone().unwrap_or_default();
        self.cursor = saved.cursor;
        self.modes.origin = saved.origin;
        self.cursor.row = self.cursor.row.min(self.grid.rows - 1);
        self.cursor.col = self.cursor.col.min(self.grid.cols - 1);
    }

    /// Switch screens: 47 and 1047 (which clears the alternate screen on the
    /// way out) keep the cursor; 1049 saves it, clears the alternate screen
    /// and on the way back restores it.
    pub(crate) fn set_alt_screen(&mut self, on: bool, mode: u16) {
        if on == self.grid.alt_active {
            return;
        }
        let flags = self.kitty_keyboard();
        if on && mode == 1049 {
            self.save_cursor();
        }
        if !on && mode == 1047 {
            self.clear_alt();
        }
        self.grid.alt_active = on;
        if on && mode == 1049 {
            self.clear_alt();
        }
        self.modes.alt_screen = on;
        self.grid.damage_all();
        if !on && mode == 1049 {
            self.restore_cursor();
        }
        self.mode_changed(ModeChange::AltScreen(on));
        if self.kitty_keyboard() != flags {
            self.mode_changed(ModeChange::KittyKeyboard(self.kitty_keyboard()));
        }
    }

    fn clear_alt(&mut self) {
        let (cols, bg) = (self.grid.cols, self.cursor.pen.bg);
        for row in self.grid.alt.iter_mut() {
            row.reset(cols, bg);
        }
    }

    /// RIS: everything back to power-on, scrollback included. The absolute
    /// numbering carries on past it.
    pub(crate) fn full_reset(&mut self) {
        if self.grid.alt_active {
            self.grid.alt_active = false;
            self.mode_changed(ModeChange::AltScreen(false));
        }
        self.grid.clear_history();
        for row in self.grid.primary.iter_mut().chain(self.grid.alt.iter_mut()) {
            row.reset(self.grid.cols, Color::Default);
        }
        self.grid.damage_all();
        let old = std::mem::take(&mut self.modes);
        self.announce_mode_resets(&old);
        self.cursor = Cursor::default();
        self.saved = [None, None];
        self.kitty = [Vec::new(), Vec::new()];
        self.tabs = default_tabs(self.grid.cols);
        self.top = 0;
        self.bottom = self.grid.rows - 1;
        self.title_stack.clear();
        self.cursor_style = None;
        *self.colors = [None; PALETTE_LEN];
        self.last_printed = None;
    }

    /// DECSTR: modes, pen, margins and the saved cursor reset; the screen and
    /// cursor position stay.
    pub(crate) fn soft_reset(&mut self) {
        let old = self.modes.clone();
        self.modes.cursor_visible = true;
        self.modes.insert = false;
        self.modes.origin = false;
        self.modes.autowrap = true;
        self.modes.cursor_keys = false;
        self.modes.keypad = false;
        self.announce_mode_resets(&old);
        self.top = 0;
        self.bottom = self.grid.rows - 1;
        self.cursor.pen = Pen::default();
        self.cursor.charsets = Charsets::default();
        self.cursor.pending_wrap = false;
        self.saved[self.screen()] = None;
    }

    fn announce_mode_resets(&mut self, old: &Modes) {
        let new = self.modes.clone();
        if old.mouse != new.mouse {
            self.mode_changed(ModeChange::Mouse(new.mouse));
        }
        if old.mouse_encoding != new.mouse_encoding {
            self.mode_changed(ModeChange::MouseEncoding(new.mouse_encoding));
        }
        if old.modify_other_keys != new.modify_other_keys {
            self.mode_changed(ModeChange::ModifyOtherKeys(new.modify_other_keys));
        }
        let flags = [
            (old.focus_reports, new.focus_reports, ModeChange::FocusReports as fn(bool) -> _),
            (old.bracketed_paste, new.bracketed_paste, ModeChange::BracketedPaste),
            (old.sync_update, new.sync_update, ModeChange::SyncUpdate),
            (old.color_scheme_reports, new.color_scheme_reports, ModeChange::ColorSchemeReports),
        ];
        for (was, is, change) in flags {
            if was != is {
                self.mode_changed(change(is));
            }
        }
    }

    // Keyboard protocols: xterm's modifyOtherKeys, kitty's flags.

    pub(crate) fn set_modify_other_keys(&mut self, level: u8) {
        if self.modes.modify_other_keys != level {
            self.modes.modify_other_keys = level;
            self.mode_changed(ModeChange::ModifyOtherKeys(level));
        }
    }

    pub(crate) fn kitty_push(&mut self, flags: u8) {
        let stack = &mut self.kitty[self.screen()];
        if stack.len() >= KITTY_STACK_MAX {
            stack.remove(0);
        }
        stack.push(flags & 0x1f);
        self.mode_changed(ModeChange::KittyKeyboard(flags & 0x1f));
    }

    pub(crate) fn kitty_pop(&mut self, n: u16) {
        let stack = &mut self.kitty[self.screen()];
        stack.truncate(stack.len().saturating_sub(usize::from(n)));
        let flags = self.kitty_keyboard();
        self.mode_changed(ModeChange::KittyKeyboard(flags));
    }

    /// `CSI = flags ; mode u`: 1 replace, 2 set the bits, 3 clear them.
    pub(crate) fn kitty_set(&mut self, flags: u8, mode: u16) {
        let current = self.kitty_keyboard();
        let flags = match mode {
            2 => current | flags,
            3 => current & !flags,
            _ => flags,
        } & 0x1f;
        let stack = &mut self.kitty[self.screen()];
        match stack.last_mut() {
            Some(top) => *top = flags,
            None => stack.push(flags),
        }
        self.mode_changed(ModeChange::KittyKeyboard(flags));
    }

    // Shell integration.

    pub(crate) fn mark(&mut self, kind: MarkKind) {
        let (row, col) = (self.cursor.row, self.cursor.col);
        let index = self.screen_top() + u64::from(row);
        if !self.grid.alt_active {
            let line = self.grid.line_mut(row);
            line.marks.push((col, kind));
            if line.marks.len() > 64 {
                line.marks.remove(0);
            }
        }
        self.push_event(Event::Mark(Mark { kind, row: index, col }));
    }

    pub(crate) fn set_hyperlink(&mut self, link: Option<Hyperlink>) {
        self.cursor.pen.link = match (link, self.cursor.pen.link.take()) {
            (Some(link), Some(old)) if *old == link => Some(old),
            (link, _) => link.map(Arc::new),
        };
    }
}

impl Row {
    /// ICH: `n` blanks at `col`, the cells from there shifting right and off
    /// the end.
    pub(crate) fn insert_blanks(&mut self, col: usize, n: usize, bg: Color) {
        let len = self.cells.len();
        if col >= len {
            return;
        }
        let n = n.min(len - col);
        if col > 0 && self.cells[col].is_spacer() {
            self.orphan(col - 1);
            self.orphan(col);
        }
        if n < len - col && self.cells[len - n - 1].is_wide() {
            self.orphan(len - n - 1);
        }
        self.cells[col..].rotate_right(n);
        self.cells[col..col + n].fill(Cell::blank(bg));
        self.occupied = (self.occupied + n).min(len);
        if bg != Color::Default {
            self.touch(col + n);
        }
        self.wrapped &= n == 0;
    }

    /// DCH: `n` cells at `col` removed, the rest shifting left, blanks
    /// coming in at the end.
    pub(crate) fn delete_cells(&mut self, col: usize, n: usize, bg: Color) {
        let len = self.cells.len();
        if col >= len {
            return;
        }
        let n = n.min(len - col);
        self.split_wide(col..col + n);
        // A pad means nothing away from the end of its row.
        self.cells[len - 1].clear_pad();
        self.cells[col..].rotate_left(n);
        self.cells[len - n..].fill(Cell::blank(bg));
        if bg != Color::Default {
            self.touch(len);
        }
        self.wrapped &= n == 0;
    }
}

/// The length of the UTF-8 sequence `lead` starts; 1 for anything that
/// starts none.
fn utf8_len(lead: u8) -> usize {
    match lead {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// `bytes` up to a UTF-8 sequence its end cuts short.
fn whole_chars(bytes: &[u8]) -> usize {
    for start in (bytes.len().saturating_sub(3)..bytes.len()).rev() {
        match bytes[start] {
            0x80..=0xbf => continue,
            lead if start + utf8_len(lead) > bytes.len() => return start,
            _ => break,
        }
    }
    bytes.len()
}

pub(crate) fn default_tabs(cols: u16) -> Vec<bool> {
    (0..cols).map(|col| col % 8 == 0).collect()
}

/// Whether `c`, printed after the cell holding `cluster`, joins it: a ZWJ
/// sequence, an emoji modifier, the second regional indicator of a flag —
/// when Unicode's segmentation agrees. Combining marks (zero width) always
/// join and are not asked about here.
pub(crate) fn joins(cluster: &str, c: char) -> bool {
    let candidate = cluster.ends_with('\u{200D}')
        || emoji_modifier(c)
        || regional_indicator(c)
            && cluster.chars().count() == 1
            && cluster.chars().all(regional_indicator);
    candidate && {
        let mut joined = cluster.to_owned();
        joined.push(c);
        joined.graphemes(true).nth(1).is_none()
    }
}

fn regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

fn emoji_modifier(c: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
}
