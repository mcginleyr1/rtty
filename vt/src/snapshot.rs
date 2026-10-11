use std::fmt::Write;

use crate::cell::{Attrs, Color, Underline};
use crate::modes::{MouseEncoding, MouseMode};
use crate::row::Row;
use crate::term::{
    ALTERNATE, BACKGROUND, CURSOR, Charset, Charsets, Cursor, FOREGROUND, PRIMARY, Pen, Rgb, Saved,
    Terminal, default_tabs, joins,
};

/// The SGR parameters that select `pen`'s rendition from scratch.
pub(crate) fn sgr(pen: &Pen) -> String {
    let mut out = String::from("0");
    for (attr, code) in [
        (Attrs::BOLD, "1"),
        (Attrs::DIM, "2"),
        (Attrs::ITALIC, "3"),
        (Attrs::BLINK, "5"),
        (Attrs::INVERSE, "7"),
        (Attrs::HIDDEN, "8"),
        (Attrs::STRIKE, "9"),
        (Attrs::OVERLINE, "53"),
    ] {
        if pen.attrs.contains(attr) {
            out.push(';');
            out.push_str(code);
        }
    }
    match pen.attrs.underline() {
        Underline::None => {}
        Underline::Single => out.push_str(";4"),
        style => {
            let _ = write!(out, ";4:{}", style as u8);
        }
    }
    let color = |out: &mut String, color: Color, base: u8, bright: u8, extended: u8| match color {
        Color::Default => {}
        Color::Named(n) if n < 8 => {
            let _ = write!(out, ";{}", base + n);
        }
        Color::Named(n) => {
            let _ = write!(out, ";{}", bright + n - 8);
        }
        Color::Indexed(n) => {
            let _ = write!(out, ";{extended};5;{n}");
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";{extended};2;{r};{g};{b}");
        }
    };
    color(&mut out, pen.fg, 30, 90, 38);
    color(&mut out, pen.bg, 40, 100, 48);
    match pen.underline {
        Color::Default => {}
        Color::Indexed(n) | Color::Named(n) => {
            let _ = write!(out, ";58:5:{n}");
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";58:2::{r}:{g}:{b}");
        }
    }
    out
}

/// Writes escape sequences, tracking the rendition it last selected.
struct Encoder {
    out: String,
    pen: Pen,
}

impl Encoder {
    fn pen(&mut self, pen: &Pen) {
        let rendition = |pen: &Pen| (pen.fg, pen.bg, pen.attrs, pen.underline);
        if rendition(pen) != rendition(&self.pen) {
            let _ = write!(self.out, "\x1b[{}m", sgr(pen));
        }
        if pen.link != self.pen.link {
            match &pen.link {
                Some(link) => {
                    let id = link.id.as_deref().map(|id| format!("id={id}")).unwrap_or_default();
                    let _ = write!(self.out, "\x1b]8;{id};{}\x1b\\", link.uri);
                }
                None => self.out.push_str("\x1b]8;;\x1b\\"),
            }
        }
        self.pen = pen.clone();
    }

    fn goto(&mut self, row: u16, col: u16) {
        let _ = write!(self.out, "\x1b[{};{}H", row + 1, col + 1);
    }

    /// Print the cell at `col` in its own rendition.
    fn cell(&mut self, row: &Row, col: usize) {
        self.pen(&pen_of(row, col));
        row.push_grapheme(col, &mut self.out);
    }

    /// Draw a screen, letting soft-wrapped rows wrap so the terminal marks
    /// them as the grid does: onto a cleared one, or, given `only`, those
    /// rows over what the terminal shows.
    fn screen<'a>(&mut self, rows: u16, line: impl Fn(u16) -> &'a Row, only: Option<&[u16]>) {
        let mut flowing = false;
        // The cluster printed last: an emoji sequence's next character
        // printed straight after it would join it.
        let mut last = String::new();
        for r in 0..rows {
            let row = line(r);
            let len = row.cells.len();
            if only.is_some_and(|only| !only.contains(&r)) {
                if flowing {
                    // A row above wrapped into this one, unchanged: its first
                    // two columns land the wrap (and mend a stand-in's).
                    for col in (0..len.min(2)).filter(|&col| !row.cells[col].is_spacer()) {
                        if joins(&last, row.cells[col].ch) {
                            let _ = write!(self.out, "\x1b[{}m", sgr(&self.pen));
                        }
                        self.cell(row, col);
                        last.clear();
                        row.push_grapheme(col, &mut last);
                    }
                }
                flowing = false;
                continue;
            }
            let flows = row.wrapped && r + 1 < rows;
            // A pad is the terminal's own doing when a wide character does
            // not fit: leave it to the terminal.
            let pad = flows && row.cells[len - 1].is_pad();
            // Printing the next row's first cell makes the pad, in its
            // rendition, when it is wide; any other pad is made with a
            // stand-in.
            let dangling = pad && {
                let next = line(r + 1);
                !next.cells[0].is_wide() || pen_of(next, 0) != pen_of(row, len - 1)
            };
            let end = if flows {
                len - usize::from(pad)
            } else {
                let used = row.cells.iter().rposition(|cell| !cell.is_default());
                // Flowing in from above, a character must land to wrap.
                used.map_or(usize::from(flowing), |last| last + 1)
            };
            if only.is_some() && !flowing {
                self.goto(r, 0);
                self.pen(&Pen::default());
                self.out.push_str("\x1b[2K");
                last.clear();
            } else if !flowing && end > 0 {
                self.goto(r, 0);
                last.clear();
            }
            let mut col = 0;
            while col < end {
                let cell = row.cells[col];
                if !cell.is_spacer() {
                    if joins(&last, cell.ch) {
                        // Any sequence in between keeps them apart; an SGR
                        // leaves a pending wrap be.
                        let _ = write!(self.out, "\x1b[{}m", sgr(&self.pen));
                    }
                    self.cell(row, col);
                    last.clear();
                    row.push_grapheme(col, &mut last);
                }
                col += 1;
            }
            if dangling {
                // Write a wide character to make the pad, then erase it.
                self.pen(&pen_of(row, len - 1));
                self.out.push('日');
                self.pen(&Pen::default());
                self.out.push_str("\x1b[2D\x1b[2X");
                last.clear();
            }
            if only.is_some() && flowing && !flows && end < len {
                // Flowed into, the row's old end is cleared after its text.
                self.pen(&Pen::default());
                self.out.push_str("\x1b[K");
            }
            flowing = flows;
        }
        if only.is_none_or(|only| only.contains(&(rows - 1))) {
            self.wrap_bottom(rows, line(rows - 1));
        }
    }

    /// Soft-wrap the bottom row, drawn already, if it is: below a scroll
    /// region a row wraps onto itself, so a character printed past its end
    /// lands at its start, and what it covers is put back.
    fn wrap_bottom(&mut self, rows: u16, row: &Row) {
        let len = row.cells.len();
        if !row.wrapped || rows < 3 || len < 3 {
            return;
        }
        let _ = write!(self.out, "\x1b[1;{}r", rows - 1);
        let wide_first = row.cells[0].is_wide();
        // The columns the character printed past the end covers, then
        // blanks; a cell is rewritten over a blank to leave a pad above be.
        let covered = if row.cells[len - 1].is_pad() {
            self.goto(rows - 1, (len - 1) as u16);
            self.pen(&pen_of(row, len - 1));
            self.out.push('日');
            if wide_first || !row.cells[1].is_wide() { 2 } else { 3 }
        } else {
            let first = usize::from(row.cells[0].width());
            self.pen(&Pen::default());
            self.goto(rows - 1, 0);
            let _ = write!(self.out, "\x1b[{first}X");
            let col = if row.cells[len - 1].is_spacer() { len - 2 } else { len - 1 };
            self.goto(rows - 1, col as u16);
            let _ = write!(self.out, "\x1b[{}X", row.cells[col].width());
            self.cell(row, col);
            self.out.push_str("\x1b[0m ");
            self.pen = Pen { link: self.pen.link.take(), ..Pen::default() };
            first
        };
        self.pen(&Pen::default());
        self.goto(rows - 1, 0);
        let _ = write!(self.out, "\x1b[{covered}X");
        self.cell(row, 0);
        if covered > usize::from(row.cells[0].width()) {
            let mut first = String::new();
            row.push_grapheme(0, &mut first);
            if joins(&first, row.cells[1].ch) {
                let _ = write!(self.out, "\x1b[{}m", sgr(&self.pen));
            }
            self.cell(row, 1);
        }
        self.out.push_str("\x1b[r");
    }

    /// Put the terminal's cursor where `cursor` is, a pending wrap and the
    /// rendition and character sets included. `origin` is whether origin
    /// mode will be in force (with the region at `top`).
    fn cursor(&mut self, cursor: &Cursor, line: &Row, top: Option<u16>) {
        let row = cursor.row - top.unwrap_or(0).min(cursor.row);
        if cursor.pending_wrap {
            let mut col = line.cells.len() - 1;
            if line.cells[col].is_spacer() && col > 0 {
                col -= 1;
            }
            // Erased first, a cell rewritten over itself leaves any pad
            // above be.
            self.goto(row, col as u16);
            let _ = write!(self.out, "\x1b[{}X", line.cells[col].width());
            self.cell(line, col);
        } else {
            self.goto(row, cursor.col);
        }
        self.pen(&cursor.pen);
        charsets(&mut self.out, &cursor.charsets);
    }
}

fn pen_of(row: &Row, col: usize) -> Pen {
    let cell = row.cells[col];
    Pen {
        fg: cell.fg,
        bg: cell.bg,
        attrs: cell.attrs(),
        underline: row.underline_color(col),
        link: row.hyperlink(col).cloned(),
    }
}

fn charsets(out: &mut String, charsets: &Charsets) {
    for (slot, set) in charsets.g.iter().enumerate() {
        let designator = match set {
            Charset::Ascii => 'B',
            Charset::DecGraphics => '0',
            Charset::Uk => 'A',
        };
        let _ = write!(out, "\x1b{}{designator}", ['(', ')', '*', '+'][slot]);
    }
    out.push_str(["\x0f", "\x0e", "\x1bn", "\x1bo"][usize::from(charsets.gl)]);
    match charsets.single_shift {
        Some(2) => out.push_str("\x1bN"),
        Some(3) => out.push_str("\x1bO"),
        _ => {}
    }
}

/// Back to the plain state drawing goes on in, after a saved one: origin
/// mode off, no rendition or link, ASCII.
fn plain(e: &mut Encoder, origin: bool) {
    if origin {
        e.out.push_str("\x1b[?6l");
    }
    e.out.push_str("\x1b[0m\x1b]8;;\x1b\\");
    e.pen = Pen::default();
    charsets(&mut e.out, &Charsets::default());
}

fn rgb(Rgb(r, g, b): Rgb) -> String {
    format!("rgb:{r:02x}/{g:02x}/{b:02x}")
}

impl Terminal {
    /// Escape sequences that bring a fresh terminal of the same size to this
    /// one's visible state: both screens when the alternate one is up (the
    /// primary under it, so the program's own switch back lands on it), the
    /// saved cursors, the scroll region, tab stops, the cursor (position,
    /// pending wrap, shape, visibility), the rendition and character sets,
    /// the modes a program depends on (cursor keys, keypad, mouse, focus,
    /// bracketed paste, kitty flags…), colours the program set and the
    /// title. The scrollback is not carried.
    pub fn snapshot(&self) -> Vec<u8> {
        let (rows, cols) = self.size();
        let mut e = Encoder { out: String::new(), pen: Pen::default() };
        e.out.push_str("\x1b[?25l\x1b[?1049l\x1b[r\x1b[?6l\x1b[?7h\x1b[4l\x1b[0m\x1b]8;;\x1b\\");
        charsets(&mut e.out, &Charsets::default());
        e.out.push_str("\x1b[H\x1b[2J");

        // A DECSC slot is filled on the empty screen, before it is drawn: a
        // pending wrap is made by reprinting a cell, which would end a soft
        // wrap that drawing then puts back.
        let primary = |r: u16| self.grid.primary_line(r);
        let alternate = |r: u16| &self.grid.alt[usize::from(r)];
        if let Some(saved) = &self.saved[PRIMARY] {
            self.saved_cursor(&mut e, saved, primary);
        }
        e.screen(rows, primary, None);
        self.kitty_stack(&mut e, PRIMARY);
        if self.grid.alt_active {
            // 1049 saves the cursor as DECSC does: DECRC brings the saved
            // state back for it to save again, for the program's own switch
            // back to restore.
            match &self.saved[PRIMARY] {
                Some(saved) => {
                    e.out.push_str("\x1b8\x1b[?1049h");
                    plain(&mut e, saved.origin);
                }
                None => e.out.push_str("\x1b[?1047h"),
            }
            e.pen(&Pen::default());
            e.out.push_str("\x1b[2J");
            if let Some(saved) = &self.saved[ALTERNATE] {
                self.saved_cursor(&mut e, saved, alternate);
            }
            e.screen(rows, alternate, None);
            self.kitty_stack(&mut e, ALTERNATE);
        }

        if self.tabs != default_tabs(cols) {
            e.out.push_str("\x1b[3g");
            for col in (0..cols).filter(|&col| self.tabs[usize::from(col)]) {
                e.goto(0, col);
                e.out.push_str("\x1bH");
            }
        }
        for (slot, color) in self.colors.iter().enumerate() {
            let Some(color) = *color else { continue };
            let _ = match slot {
                FOREGROUND | BACKGROUND | CURSOR => {
                    write!(e.out, "\x1b]{};{}\x1b\\", slot - FOREGROUND + 10, rgb(color))
                }
                _ => write!(e.out, "\x1b]4;{slot};{}\x1b\\", rgb(color)),
            };
        }
        if !self.icon_name.is_empty() {
            let _ = write!(e.out, "\x1b]1;{}\x1b\\", self.icon_name);
        }
        if !self.title.is_empty() {
            let _ = write!(e.out, "\x1b]2;{}\x1b\\", self.title);
        }
        self.state(&mut e, false);
        e.out.into_bytes()
    }

    /// Escape sequences that redraw visible rows `rows` — what
    /// `take_damage` named — and set the scroll region, cursor, rendition,
    /// character sets and the modes input is encoded by (cursor keys, keypad,
    /// mouse, focus, bracketed paste, kitty flags) on a terminal that showed
    /// this one's state before: the update to send after a `snapshot`. The
    /// same screen must be up as before: a switch of screens or a resize
    /// calls for a new `snapshot`.
    pub fn redraw(&self, rows: &[u16]) -> Vec<u8> {
        let mut e = Encoder { out: String::new(), pen: Pen::default() };
        e.out.push_str("\x1b[?25l\x1b[r\x1b[?6l\x1b[?7h\x1b[4l\x1b[0m\x1b]8;;\x1b\\");
        charsets(&mut e.out, &Charsets::default());
        if !rows.is_empty() {
            e.screen(self.grid.rows, |r| self.grid.line(r), Some(rows));
        }
        self.state(&mut e, true);
        e.out.into_bytes()
    }

    /// The scroll region, cursor, rendition, character sets and modes, onto
    /// a screen drawn in the plain state. `explicit` also turns off what is
    /// off, for a terminal that may have had it on.
    fn state(&self, e: &mut Encoder, explicit: bool) {
        let (rows, _) = self.size();
        let modes = &self.modes;
        if (self.top, self.bottom) != (0, rows - 1) {
            let _ = write!(e.out, "\x1b[{};{}r", self.top + 1, self.bottom + 1);
        }
        if modes.origin {
            e.out.push_str("\x1b[?6h");
        }
        let top = modes.origin.then_some(self.top);
        e.cursor(&self.cursor, self.grid.line(self.cursor.row), top);
        let flags = [
            (modes.cursor_keys, "?1"),
            (modes.reverse_video, "?5"),
            (modes.autowrap, "?7"),
            (modes.cursor_blink, "?12"),
            (modes.alternate_scroll, "?1007"),
            (modes.focus_reports, "?1004"),
            (modes.bracketed_paste, "?2004"),
            (modes.color_scheme_reports, "?2031"),
            (modes.insert, "4"),
            (modes.newline, "20"),
        ];
        for (on, mode) in flags {
            if on != (mode == "?7") || explicit {
                let _ = write!(e.out, "\x1b[{mode}{}", if on { 'h' } else { 'l' });
            }
        }
        if modes.keypad || explicit {
            e.out.push_str(if modes.keypad { "\x1b=" } else { "\x1b>" });
        }
        let mouse = match modes.mouse {
            MouseMode::None => None,
            MouseMode::X10 => Some(9),
            MouseMode::Click => Some(1000),
            MouseMode::Drag => Some(1002),
            MouseMode::Motion => Some(1003),
        };
        let encoding = match modes.mouse_encoding {
            MouseEncoding::Default => None,
            MouseEncoding::Utf8 => Some(1005),
            MouseEncoding::Sgr => Some(1006),
            MouseEncoding::Urxvt => Some(1015),
            MouseEncoding::SgrPixels => Some(1016),
        };
        if explicit {
            // Turning off the one that is on turns it off: any of them.
            for mode in [9, 1000, 1002, 1003, 1005, 1006, 1015, 1016] {
                let _ = write!(e.out, "\x1b[?{mode}l");
            }
            let _ = write!(e.out, "\x1b[={};1u", self.kitty_keyboard());
        }
        for mode in mouse.into_iter().chain(encoding) {
            let _ = write!(e.out, "\x1b[?{mode}h");
        }
        if modes.modify_other_keys != 0 || explicit {
            let _ = write!(e.out, "\x1b[>4;{}m", modes.modify_other_keys);
        }
        if self.cursor_style.is_some() || explicit {
            let _ =
                write!(e.out, "\x1b[{} q", self.cursor_style.map_or(0, |style| style.decscusr()));
        }
        if modes.cursor_visible {
            e.out.push_str("\x1b[?25h");
        }
    }

    /// Bring the cursor to a saved state and DECSC it, then put back the
    /// plain state drawing goes on in.
    fn saved_cursor<'a>(&'a self, e: &mut Encoder, saved: &Saved, line: impl Fn(u16) -> &'a Row) {
        if saved.origin {
            e.out.push_str("\x1b[?6h");
        }
        let cursor = &saved.cursor;
        e.cursor(cursor, line(cursor.row.min(self.grid.rows - 1)), None);
        e.out.push_str("\x1b7");
        plain(e, saved.origin);
    }

    fn kitty_stack(&self, e: &mut Encoder, screen: usize) {
        for flags in &self.kitty[screen] {
            let _ = write!(e.out, "\x1b[>{flags}u");
        }
    }
}
