use vte::{Params, Perform};

use crate::cell::{Attrs, Color, Underline};
use crate::event::{Event, MarkKind, ModeChange};
use crate::modes::{CursorStyle, Modes, MouseEncoding, MouseMode};
use crate::row::Hyperlink;
use crate::snapshot::sgr;
use crate::term::{CURSOR, Charset, FOREGROUND, Pen, Rgb, Terminal};

/// The `vte::Perform` side of a `Terminal`: everything above the byte
/// state machine. Printed characters gather into a run, written in one go
/// when anything else comes.
pub(crate) struct Performer<'a>(pub(crate) &'a mut Terminal, pub(crate) Vec<char>);

impl Performer<'_> {
    /// Write the run printed so far.
    pub(crate) fn flush(&mut self) {
        if !self.1.is_empty() {
            self.0.print_run(&self.1);
            self.1.clear();
        }
    }

    /// Something other than a printed character: the run before it is
    /// written, and no emoji sequence continues past it.
    fn interrupt(&mut self) {
        self.flush();
        self.0.joinable = false;
    }
}

/// The DCS strings that are answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DcsKind {
    /// XTGETTCAP, `DCS + q`.
    Termcap,
    /// DECRQSS, `DCS $ q`.
    Setting,
}

const DCS_MAX: usize = 1024;

impl Perform for Performer<'_> {
    fn print(&mut self, c: char) {
        self.1.push(c);
    }

    fn execute(&mut self, byte: u8) {
        self.interrupt();
        let term = &mut *self.0;
        match byte {
            0x07 => term.push_event(Event::Bell),
            0x08 => term.backspace(),
            0x09 => term.tab(1),
            0x0a..=0x0c => term.linefeed(),
            0x0d => term.carriage_return(),
            0x0e => term.cursor.charsets.gl = 1,
            0x0f => term.cursor.charsets.gl = 0,
            _ => {}
        }
    }

    fn hook(&mut self, _: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.interrupt();
        self.0.dcs = match (intermediates, action) {
            _ if ignore => None,
            (b"+", 'q') => Some((DcsKind::Termcap, Vec::new())),
            (b"$", 'q') => Some((DcsKind::Setting, Vec::new())),
            _ => None,
        };
    }

    fn put(&mut self, byte: u8) {
        if let Some((_, bytes)) = &mut self.0.dcs
            && bytes.len() < DCS_MAX
        {
            bytes.push(byte);
        }
    }

    fn unhook(&mut self) {
        match self.0.dcs.take() {
            Some((DcsKind::Termcap, names)) => termcap(self.0, &names),
            Some((DcsKind::Setting, setting)) => request_setting(self.0, &setting),
            None => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.interrupt();
        let terminator = if bell_terminated { "\x07" } else { "\x1b\\" };
        osc(self.0, params, terminator);
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.interrupt();
        if ignore {
            return;
        }
        csi(self.0, params, intermediates, action);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.interrupt();
        if ignore {
            return;
        }
        let term = &mut *self.0;
        match (intermediates, byte) {
            (b"", b'7') => term.save_cursor(),
            (b"", b'8') => term.restore_cursor(),
            (b"", b'D') => term.index(),
            (b"", b'E') => {
                term.index();
                term.carriage_return();
            }
            (b"", b'H') => term.set_tab_stop(),
            (b"", b'M') => term.reverse_index(),
            (b"", b'N') => term.cursor.charsets.single_shift = Some(2),
            (b"", b'O') => term.cursor.charsets.single_shift = Some(3),
            (b"", b'n') => term.cursor.charsets.gl = 2,
            (b"", b'o') => term.cursor.charsets.gl = 3,
            (b"", b'Z') => term.reply(PRIMARY_ATTRIBUTES),
            (b"", b'c') => term.full_reset(),
            (b"", b'=') => term.modes.keypad = true,
            (b"", b'>') => term.modes.keypad = false,
            (b"#", b'8') => term.screen_alignment(),
            ([slot @ (b'(' | b')' | b'*' | b'+')], set) => {
                let charset = match set {
                    b'0' => Charset::DecGraphics,
                    b'A' => Charset::Uk,
                    b'B' => Charset::Ascii,
                    _ => return,
                };
                term.cursor.charsets.g[usize::from(slot - b'(')] = charset;
            }
            _ => {}
        }
    }
}

/// DA1: a VT220 with ANSI colour.
const PRIMARY_ATTRIBUTES: &str = "\x1b[?62;22c";

/// The xterm patch level DA2 reports, `CSI > 1 ; 277 ; 0 c`. Programs gate
/// xterm features on it: emacs queries colours and turns on modifyOtherKeys
/// from 242, vim asks for SGR mouse reports from 277.
pub const XTERM_VERSION: u16 = 277;

fn csi(term: &mut Terminal, params: &Params, intermediates: &[u8], action: char) {
    let mut args = [0u16; 16];
    let mut count = 0;
    for param in params.iter().take(args.len()) {
        args[count] = param[0];
        count += 1;
    }
    let args = &args[..count];
    let arg = |index: usize, default: u16| match args.get(index) {
        Some(0) | None => default,
        Some(&value) => value,
    };
    match (intermediates, action) {
        (b"", '@') => term.insert_chars(arg(0, 1)),
        (b"", 'A') => term.cursor_up(arg(0, 1)),
        (b"", 'B' | 'e') => term.cursor_down(arg(0, 1)),
        (b"", 'C' | 'a') => term.cursor_forward(arg(0, 1)),
        (b"", 'D') => term.cursor_backward(arg(0, 1)),
        (b"", 'E') => {
            term.cursor_down(arg(0, 1));
            term.carriage_return();
        }
        (b"", 'F') => {
            term.cursor_up(arg(0, 1));
            term.carriage_return();
        }
        (b"", 'G' | '`') => term.goto(term.addressed_row(), arg(0, 1) - 1),
        (b"", 'H' | 'f') => term.goto(arg(0, 1) - 1, arg(1, 1) - 1),
        (b"", 'I') => term.tab(arg(0, 1)),
        (b"" | b"?", 'J') => term.erase_display(arg(0, 0)),
        (b"" | b"?", 'K') => term.erase_line(arg(0, 0)),
        (b"", 'L') => term.insert_lines(arg(0, 1)),
        (b"", 'M') => term.delete_lines(arg(0, 1)),
        (b"", 'P') => term.delete_chars(arg(0, 1)),
        (b"", 'S') => term.scroll_up(arg(0, 1)),
        (b"", 'T') if args.len() <= 1 => term.scroll_down(arg(0, 1)),
        (b"", 'X') => term.erase_chars(arg(0, 1)),
        (b"", 'Z') => term.back_tab(arg(0, 1)),
        (b"", 'b') => {
            if let Some(c) = term.last_printed {
                for _ in 0..arg(0, 1) {
                    term.put_char(c);
                }
            }
        }
        (b"", 'c') if arg(0, 0) == 0 => term.reply(PRIMARY_ATTRIBUTES),
        (b">", 'c') if arg(0, 0) == 0 => term.reply(format!("\x1b[>1;{XTERM_VERSION};0c")),
        (b"", 'd') => term.goto(arg(0, 1) - 1, term.cursor.col),
        (b"", 'g') => match arg(0, 0) {
            0 => term.clear_tab_stops(false),
            3 => term.clear_tab_stops(true),
            _ => {}
        },
        (b"", 'h' | 'l') => {
            for &mode in args {
                set_ansi_mode(term, mode, action == 'h');
            }
        }
        (b"?", 'h' | 'l') => {
            for &mode in args {
                set_dec_mode(term, mode, action == 'h');
            }
        }
        (b"", 'm') => select_graphic_rendition(term, params),
        // XTMODKEYS sets modifyOtherKeys, `CSI > 4 n` turns it off,
        // XTQMODKEYS asks for it.
        (b">", 'm') if arg(0, 0) == 4 => term.set_modify_other_keys(arg(1, 0) as u8),
        (b">", 'n') if arg(0, 0) == 4 => term.set_modify_other_keys(0),
        (b"?", 'm') if arg(0, 0) == 4 => {
            term.reply(format!("\x1b[>4;{}m", term.modes.modify_other_keys));
        }
        (b"", 'n') => match arg(0, 0) {
            5 => term.reply("\x1b[0n"),
            6 => {
                let (row, col) = (term.addressed_row() + 1, term.cursor.col + 1);
                term.reply(format!("\x1b[{row};{col}R"));
            }
            _ => {}
        },
        (b"?", 'n') => match arg(0, 0) {
            6 => {
                let (row, col) = (term.addressed_row() + 1, term.cursor.col + 1);
                term.reply(format!("\x1b[?{row};{col};1R"));
            }
            996 => term.report_color_scheme(),
            _ => {}
        },
        (b"$", 'p') => {
            let state = match arg(0, 0) {
                4 => report(term.modes.insert),
                20 => report(term.modes.newline),
                _ => 0,
            };
            term.reply(format!("\x1b[{};{state}$y", arg(0, 0)));
        }
        (b"?$", 'p') => {
            let state = dec_mode_state(term, arg(0, 0));
            term.reply(format!("\x1b[?{};{state}$y", arg(0, 0)));
        }
        (b"!", 'p') => term.soft_reset(),
        (b" ", 'q') => {
            if let Some(style) = CursorStyle::from_decscusr(arg(0, 0)) {
                term.cursor_style = style;
                term.push_event(Event::CursorStyle(style));
            }
        }
        (b">", 'q') if arg(0, 0) == 0 => {
            term.reply(format!("\x1bP>|rtty-vt {}\x1b\\", env!("CARGO_PKG_VERSION")));
        }
        (b"", 'r') => {
            let bottom = args.get(1).copied().filter(|&bottom| bottom > 0);
            term.set_scroll_region(arg(0, 1), bottom);
        }
        (b"", 's') => term.save_cursor(),
        (b"", 'u') => term.restore_cursor(),
        (b"", 't') => window_op(term, arg(0, 0)),
        (b"?", 'u') => {
            let flags = term.kitty_keyboard();
            term.reply(format!("\x1b[?{flags}u"));
        }
        (b">", 'u') => term.kitty_push(arg(0, 0) as u8),
        (b"<", 'u') => term.kitty_pop(arg(0, 1)),
        (b"=", 'u') => term.kitty_set(arg(0, 0) as u8, arg(1, 1)),
        _ => {}
    }
}

/// DECRPM values: 1 set, 2 reset.
fn report(set: bool) -> u8 {
    if set { 1 } else { 2 }
}

fn set_ansi_mode(term: &mut Terminal, mode: u16, on: bool) {
    match mode {
        4 => term.modes.insert = on,
        20 => term.modes.newline = on,
        _ => {}
    }
}

fn set_dec_mode(term: &mut Terminal, mode: u16, on: bool) {
    let modes = &mut term.modes;
    match mode {
        1 => modes.cursor_keys = on,
        5 => {
            modes.reverse_video = on;
            term.grid.damage_all();
        }
        6 => {
            modes.origin = on;
            term.goto(0, 0);
        }
        7 => modes.autowrap = on,
        12 => modes.cursor_blink = on,
        25 => modes.cursor_visible = on,
        66 => modes.keypad = on,
        9 | 1000 | 1002 | 1003 => {
            let mouse = match mode {
                9 => MouseMode::X10,
                1000 => MouseMode::Click,
                1002 => MouseMode::Drag,
                _ => MouseMode::Motion,
            };
            let mouse = if on {
                mouse
            } else if modes.mouse == mouse {
                MouseMode::None
            } else {
                return;
            };
            if modes.mouse != mouse {
                modes.mouse = mouse;
                term.mode_changed(ModeChange::Mouse(mouse));
            }
        }
        1005 | 1006 | 1015 | 1016 => {
            let encoding = match mode {
                1005 => MouseEncoding::Utf8,
                1006 => MouseEncoding::Sgr,
                1015 => MouseEncoding::Urxvt,
                _ => MouseEncoding::SgrPixels,
            };
            let encoding = if on {
                encoding
            } else if modes.mouse_encoding == encoding {
                MouseEncoding::Default
            } else {
                return;
            };
            if modes.mouse_encoding != encoding {
                modes.mouse_encoding = encoding;
                term.mode_changed(ModeChange::MouseEncoding(encoding));
            }
        }
        1007 => modes.alternate_scroll = on,
        47 | 1047 | 1049 => term.set_alt_screen(on, mode),
        1048 => {
            if on {
                term.save_cursor();
            } else {
                term.restore_cursor();
            }
        }
        1004 => toggle(term, |modes| &mut modes.focus_reports, on, ModeChange::FocusReports),
        2004 => toggle(term, |modes| &mut modes.bracketed_paste, on, ModeChange::BracketedPaste),
        2026 => toggle(term, |modes| &mut modes.sync_update, on, ModeChange::SyncUpdate),
        2031 => toggle(
            term,
            |modes| &mut modes.color_scheme_reports,
            on,
            ModeChange::ColorSchemeReports,
        ),
        _ => {}
    }
}

fn toggle(
    term: &mut Terminal,
    flag: fn(&mut Modes) -> &mut bool,
    on: bool,
    change: fn(bool) -> ModeChange,
) {
    let flag = flag(&mut term.modes);
    if *flag != on {
        *flag = on;
        term.mode_changed(change(on));
    }
}

/// DECRPM for a DEC private mode: 0 unknown, 1 set, 2 reset.
fn dec_mode_state(term: &Terminal, mode: u16) -> u8 {
    let modes = &term.modes;
    match mode {
        1 => report(modes.cursor_keys),
        5 => report(modes.reverse_video),
        6 => report(modes.origin),
        7 => report(modes.autowrap),
        9 => report(modes.mouse == MouseMode::X10),
        12 => report(modes.cursor_blink),
        25 => report(modes.cursor_visible),
        47 | 1047 | 1049 => report(modes.alt_screen),
        66 => report(modes.keypad),
        // Saving the cursor is an action, not a state.
        1048 => report(false),
        1000 => report(modes.mouse == MouseMode::Click),
        1002 => report(modes.mouse == MouseMode::Drag),
        1003 => report(modes.mouse == MouseMode::Motion),
        1004 => report(modes.focus_reports),
        1005 => report(modes.mouse_encoding == MouseEncoding::Utf8),
        1006 => report(modes.mouse_encoding == MouseEncoding::Sgr),
        1007 => report(modes.alternate_scroll),
        1015 => report(modes.mouse_encoding == MouseEncoding::Urxvt),
        1016 => report(modes.mouse_encoding == MouseEncoding::SgrPixels),
        2004 => report(modes.bracketed_paste),
        2026 => report(modes.sync_update),
        2031 => report(modes.color_scheme_reports),
        _ => 0,
    }
}

fn select_graphic_rendition(term: &mut Terminal, params: &Params) {
    let pen = &mut term.cursor.pen;
    let mut params = params.iter();
    while let Some(param) = params.next() {
        match param {
            [0] => {
                let link = pen.link.take();
                *pen = Pen { link, ..Default::default() };
            }
            [1] => pen.attrs.insert(Attrs::BOLD),
            [2] => pen.attrs.insert(Attrs::DIM),
            [3] => pen.attrs.insert(Attrs::ITALIC),
            [4, 0] => pen.attrs.set_underline(Underline::None),
            [4, 2] => pen.attrs.set_underline(Underline::Double),
            [4, 3] => pen.attrs.set_underline(Underline::Curly),
            [4, 4] => pen.attrs.set_underline(Underline::Dotted),
            [4, 5] => pen.attrs.set_underline(Underline::Dashed),
            [4, ..] => pen.attrs.set_underline(Underline::Single),
            [5 | 6] => pen.attrs.insert(Attrs::BLINK),
            [7] => pen.attrs.insert(Attrs::INVERSE),
            [8] => pen.attrs.insert(Attrs::HIDDEN),
            [9] => pen.attrs.insert(Attrs::STRIKE),
            [21] => pen.attrs.set_underline(Underline::Double),
            [22] => {
                pen.attrs.remove(Attrs::BOLD);
                pen.attrs.remove(Attrs::DIM);
            }
            [23] => pen.attrs.remove(Attrs::ITALIC),
            [24] => pen.attrs.set_underline(Underline::None),
            [25] => pen.attrs.remove(Attrs::BLINK),
            [27] => pen.attrs.remove(Attrs::INVERSE),
            [28] => pen.attrs.remove(Attrs::HIDDEN),
            [29] => pen.attrs.remove(Attrs::STRIKE),
            [n @ 30..=37] => pen.fg = Color::Named((n - 30) as u8),
            [n @ 40..=47] => pen.bg = Color::Named((n - 40) as u8),
            [n @ 90..=97] => pen.fg = Color::Named((n - 90 + 8) as u8),
            [n @ 100..=107] => pen.bg = Color::Named((n - 100 + 8) as u8),
            [39] => pen.fg = Color::Default,
            [49] => pen.bg = Color::Default,
            [53] => pen.attrs.insert(Attrs::OVERLINE),
            [55] => pen.attrs.remove(Attrs::OVERLINE),
            [59] => pen.underline = Color::Default,
            [kind @ (38 | 48 | 58)] => {
                let mut values = params.by_ref().map(|param| param[0]);
                if let Some(color) = extended_color(&mut values) {
                    set_extended(pen, *kind, color);
                }
            }
            [kind @ (38 | 48 | 58), rest @ ..] => {
                // The colon form may carry a colour space id: 2:cs:r:g:b.
                let skip = if rest.len() > 4 { 2 } else { 1 };
                let mut values = rest[..1].iter().chain(&rest[skip..]).copied();
                if let Some(color) = extended_color(&mut values) {
                    set_extended(pen, *kind, color);
                }
            }
            _ => {}
        }
    }
}

fn set_extended(pen: &mut Pen, kind: u16, color: Color) {
    match kind {
        38 => pen.fg = color,
        48 => pen.bg = color,
        _ => pen.underline = color,
    }
}

/// `5;n` or `2;r;g;b`.
fn extended_color(values: &mut dyn Iterator<Item = u16>) -> Option<Color> {
    let mut byte = || u8::try_from(values.next()?).ok();
    match byte()? {
        5 => Some(Color::Indexed(byte()?)),
        2 => Some(Color::Rgb(byte()?, byte()?, byte()?)),
        _ => None,
    }
}

fn window_op(term: &mut Terminal, op: u16) {
    let (rows, cols) = term.size();
    match op {
        14 => {
            if let Some((width, height)) = term.cell_pixels() {
                let (height, width) =
                    (u32::from(height) * u32::from(rows), u32::from(width) * u32::from(cols));
                term.reply(format!("\x1b[4;{height};{width}t"));
            }
        }
        16 => {
            if let Some((width, height)) = term.cell_pixels() {
                term.reply(format!("\x1b[6;{height};{width}t"));
            }
        }
        18 => term.reply(format!("\x1b[8;{rows};{cols}t")),
        22 => term.push_title(),
        23 => term.pop_title(),
        _ => {}
    }
}

fn osc(term: &mut Terminal, params: &[&[u8]], terminator: &str) {
    let Some((&command, args)) = params.split_first() else {
        return;
    };
    let joined = || String::from_utf8_lossy(&args.join(&b';')).into_owned();
    match command {
        b"0" => {
            term.icon_name = joined();
            term.push_event(Event::IconName(joined()));
            term.set_title(joined());
        }
        b"1" => {
            term.icon_name = joined();
            term.push_event(Event::IconName(joined()));
        }
        b"2" => term.set_title(joined()),
        b"4" => {
            for [index, spec] in args.as_chunks::<2>().0 {
                let Some(index) = number(index).filter(|&index| index < 256) else {
                    continue;
                };
                color_request(term, index, spec, &format!("4;{index}"), terminator);
            }
        }
        b"7" => {
            if let Some(cwd) = file_url_path(&joined()) {
                term.set_cwd(cwd);
            }
        }
        b"8" if args.len() >= 2 => {
            let uri = String::from_utf8_lossy(&args[1..].join(&b';')).into_owned();
            let id = args[0]
                .split(|&b| b == b':')
                .find_map(|pair| pair.strip_prefix(b"id="))
                .map(|id| String::from_utf8_lossy(id).into_owned());
            term.set_hyperlink((!uri.is_empty()).then_some(Hyperlink { id, uri }));
        }
        b"9" => notification(term, &joined()),
        b"99" => kitty_notification(term, args),
        b"10" | b"11" | b"12" => {
            let first = usize::from(command[1] - b'0');
            for (offset, spec) in args.iter().enumerate().take(3 - first) {
                let code = 10 + first + offset;
                let slot = FOREGROUND + first + offset;
                color_request(term, slot, spec, &code.to_string(), terminator);
            }
        }
        b"52" if args.len() >= 2 && args[1] != b"?" => {
            let text = base64(args[1]).and_then(|bytes| String::from_utf8(bytes).ok());
            if let Some(text) = text {
                let selection = String::from_utf8_lossy(args[0]).into_owned();
                term.push_event(Event::Clipboard { selection, text });
            }
        }
        b"104" => {
            let indices: Vec<usize> = args.iter().filter_map(|arg| number(arg)).collect();
            if indices.is_empty() {
                term.colors[..256].fill(None);
            }
            for index in indices.into_iter().filter(|&index| index < 256) {
                term.colors[index] = None;
            }
            term.grid.damage_all();
        }
        b"110" | b"111" | b"112" => {
            term.colors[FOREGROUND + usize::from(command[2] - b'0')] = None;
            term.grid.damage_all();
        }
        b"133" => {
            let kind = match args.first().and_then(|mark| mark.first()) {
                Some(b'A') => MarkKind::PromptStart,
                Some(b'B') => MarkKind::CommandStart,
                Some(b'C') => MarkKind::OutputStart,
                Some(b'D') => {
                    let exit =
                        args.get(1).and_then(|code| std::str::from_utf8(code).ok()?.parse().ok());
                    MarkKind::CommandEnd { exit }
                }
                _ => return,
            };
            term.mark(kind);
        }
        b"777" if args.first() == Some(&&b"notify"[..]) => {
            let field = |index: usize| {
                let text = String::from_utf8_lossy(&args.get(index..)?.join(&b';')).into_owned();
                (!text.is_empty()).then_some(text)
            };
            let title = args.get(1).map(|title| String::from_utf8_lossy(title).into_owned());
            let title = title.filter(|title| !title.is_empty());
            let body = field(2);
            if title.is_some() || body.is_some() {
                term.push_event(Event::Notification { title, body });
            }
        }
        b"1337" => {
            let body = joined();
            let Some((name, value)) =
                body.strip_prefix("SetUserVar=").and_then(|var| var.split_once('='))
            else {
                return;
            };
            let named =
                !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            let value = base64(value.as_bytes()).and_then(|bytes| String::from_utf8(bytes).ok());
            if let Some(value) = value.filter(|_| named) {
                term.push_event(Event::UserVar { name: name.to_string(), value });
            }
        }
        b"7501" => term.push_event(Event::ProgramStatus(joined())),
        _ => {}
    }
}

/// OSC 99, kitty's notifications: `99 ; key=value:key=value ; payload`.
/// The payload is the title (`p=title`, the default) or the body (`p=body`),
/// base64 with `e=1`. Chunks with one `i` and `d=0` add up until one without
/// `d=0` ends the notification. Other payloads (closing, queries, icons,
/// buttons) and other keys are not read.
fn kitty_notification(term: &mut Terminal, args: &[&[u8]]) {
    let metadata = String::from_utf8_lossy(args.first().copied().unwrap_or_default());
    let raw = args.get(1..).map(|rest| rest.join(&b';')).unwrap_or_default();
    let (mut id, mut done, mut kind, mut encoded) = (String::new(), true, "title", false);
    for (key, value) in metadata.split(':').filter_map(|pair| pair.split_once('=')) {
        match key {
            "i" => id = value.to_string(),
            "d" => done = value != "0",
            "p" => kind = value,
            "e" => encoded = value == "1",
            _ => {}
        }
    }
    let payload = if encoded { base64(&raw) } else { Some(raw) };
    let payload = payload.map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    let (title, body) = match term.kitty_notification.take() {
        Some((pending, title, body)) if pending == id => (title, body),
        _ => (None, None),
    };
    let add = |part: Option<String>| match (part, &payload) {
        (Some(part), Some(more)) => Some(part + more),
        (part, more) => part.or(more.clone()),
    };
    let (title, body) = match kind {
        "title" => (add(title), body),
        "body" => (title, add(body)),
        _ => return,
    };
    let (title, body) = (title.filter(|t| !t.is_empty()), body.filter(|b| !b.is_empty()));
    if !done {
        term.kitty_notification = Some((id, title, body));
    } else if title.is_some() || body.is_some() {
        term.push_event(Event::Notification { title, body });
    }
}

/// OSC 9: a notification body, or one of ConEmu's numbered subcommands, of
/// which only 4 (progress) is read.
fn notification(term: &mut Terminal, body: &str) {
    let mut fields = body.split(';');
    let first = fields.next().unwrap_or_default();
    if first.is_empty() || !first.bytes().all(|b| b.is_ascii_digit()) {
        if !body.is_empty() {
            term.push_event(Event::Notification { title: None, body: Some(body.to_string()) });
        }
        return;
    }
    if first != "4" {
        return;
    }
    let Some(state) =
        fields.next().and_then(|state| state.parse().ok()).filter(|&state| state <= 4)
    else {
        return;
    };
    let percent = fields.next().and_then(|percent| percent.parse().ok()).filter(|&p| p <= 100);
    term.push_event(Event::Progress { state, percent });
}

/// Set palette slot `slot` from `spec`, or answer `?` with what it shows.
fn color_request(term: &mut Terminal, slot: usize, spec: &[u8], prefix: &str, terminator: &str) {
    if spec == b"?" {
        if let Some(Rgb(r, g, b)) = term.color(slot) {
            term.reply(format!(
                "\x1b]{prefix};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{terminator}"
            ));
        }
    } else if let Some(rgb) = parse_color(spec) {
        term.colors[slot] = Some(rgb);
        if slot != CURSOR {
            term.grid.damage_all();
        }
    }
}

/// An X11 colour spec: `rgb:r/g/b` (1–4 hex digits each, scaled) or
/// `#rgb`…`#rrrrggggbbbb` (most significant bits).
pub(crate) fn parse_color(spec: &[u8]) -> Option<Rgb> {
    let spec = std::str::from_utf8(spec).ok()?;
    if let Some(rgb) = spec.strip_prefix("rgb:") {
        let mut channels = rgb.split('/').map(|hex| {
            let value =
                u32::from_str_radix(hex, 16).ok().filter(|_| (1..=4).contains(&hex.len()))?;
            let max = (1u32 << (4 * hex.len())) - 1;
            Some((value * 255 / max) as u8)
        });
        let rgb = Rgb(channels.next()??, channels.next()??, channels.next()??);
        return channels.next().is_none().then_some(rgb);
    }
    let hex = spec.strip_prefix('#')?;
    if hex.is_empty() || hex.len() % 3 != 0 || hex.len() > 12 {
        return None;
    }
    let digits = hex.len() / 3;
    let channel = |i: usize| {
        let value = u32::from_str_radix(&hex[i * digits..(i + 1) * digits], 16).ok()?;
        Some(if digits == 1 { value << 4 } else { value >> (4 * (digits - 2)) } as u8)
    };
    Some(Rgb(channel(0)?, channel(1)?, channel(2)?))
}

fn number(bytes: &[u8]) -> Option<usize> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

/// The path of an OSC 7 `file://<host>/<path>`, `%XX` escapes decoded; the
/// host is ignored. `None` unless it decodes to UTF-8.
fn file_url_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let mut bytes = rest[rest.find('/')?..].bytes();
    let mut path = Vec::with_capacity(bytes.len());
    while let Some(b) = bytes.next() {
        path.push(match b {
            b'%' => {
                let mut digit = || char::from(bytes.next()?).to_digit(16);
                (digit()? * 16 + digit()?) as u8
            }
            b => b,
        });
    }
    String::from_utf8(path).ok()
}

/// Standard-alphabet base64, padding optional.
pub(crate) fn base64(text: &[u8]) -> Option<Vec<u8>> {
    let text = text.strip_suffix(b"==").or_else(|| text.strip_suffix(b"=")).unwrap_or(text);
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut held) = (0u32, 0);
    for &byte in text {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        bits = bits << 6 | u32::from(value);
        held += 6;
        if held >= 8 {
            held -= 8;
            out.push((bits >> held) as u8);
        }
    }
    (held < 6).then_some(out)
}

fn hex(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02X}")).collect()
}

fn unhex(hex: &[u8]) -> Option<String> {
    let bytes: Option<Vec<u8>> = hex
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

/// XTGETTCAP: each name answered on its own, the few capabilities a program
/// probes for.
fn termcap(term: &mut Terminal, names: &[u8]) {
    for name in names.split(|&b| b == b';') {
        let value = unhex(name).and_then(|name| match name.as_str() {
            "TN" | "name" => Some("xterm-256color"),
            "Co" | "colors" => Some("256"),
            "RGB" => Some("8/8/8"),
            "Smulx" => Some("\x1b[4:%p1%dm"),
            "Setulc" => Some("\x1b[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m"),
            "Ms" => Some("\x1b]52;%p1%s;%p2%s\x07"),
            _ => None,
        });
        let name = String::from_utf8_lossy(name);
        match value {
            Some(value) => term.reply(format!("\x1bP1+r{name}={}\x1b\\", hex(value))),
            None => term.reply(format!("\x1bP0+r{name}\x1b\\")),
        }
    }
}

/// DECRQSS for SGR, DECSTBM and DECSCUSR.
fn request_setting(term: &mut Terminal, setting: &[u8]) {
    let answer = match setting {
        b"m" => Some(format!("{}m", sgr(&term.cursor.pen))),
        b"r" => Some(format!("{};{}r", term.top + 1, term.bottom + 1)),
        b" q" => Some(format!("{} q", term.cursor_style.map_or(0, CursorStyle::decscusr))),
        _ => None,
    };
    match answer {
        Some(answer) => term.reply(format!("\x1bP1$r{answer}\x1b\\")),
        None => term.reply("\x1bP0$r\x1b\\"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_specs_parse_as_xparsecolor_does() {
        assert_eq!(parse_color(b"rgb:ff/80/00"), Some(Rgb(255, 128, 0)));
        assert_eq!(parse_color(b"rgb:f/8/0"), Some(Rgb(255, 136, 0)));
        assert_eq!(parse_color(b"rgb:ffff/0000/8080"), Some(Rgb(255, 0, 128)));
        assert_eq!(parse_color(b"#f80"), Some(Rgb(0xf0, 0x80, 0)));
        assert_eq!(parse_color(b"#ff8000"), Some(Rgb(255, 128, 0)));
        assert_eq!(parse_color(b"#ffff80800000"), Some(Rgb(255, 128, 0)));
        for bad in [&b"rgb:ff/80"[..], b"rgb:fffff/0/0", b"#ff80", b"red", b"rgb:/0/0"] {
            assert_eq!(parse_color(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn base64_decodes_with_or_without_padding() {
        assert_eq!(base64(b"aGVsbG8=").as_deref(), Some(&b"hello"[..]));
        assert_eq!(base64(b"aGVsbG8").as_deref(), Some(&b"hello"[..]));
        assert_eq!(base64(b"").as_deref(), Some(&b""[..]));
        assert_eq!(base64(b"a"), None);
        assert_eq!(base64(b"a$=="), None);
    }

    #[test]
    fn file_urls_decode_and_bad_ones_are_refused() {
        assert_eq!(file_url_path("file://host/a%20dir").as_deref(), Some("/a dir"));
        assert_eq!(file_url_path("file:///tmp/caf%C3%A9").as_deref(), Some("/tmp/café"));
        for bad in
            ["http://h/x", "/bare", "file://host", "file://h/a%2", "file://h/a%zz", "file://h/%ff"]
        {
            assert_eq!(file_url_path(bad), None, "{bad}");
        }
    }
}
