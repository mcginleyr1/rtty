//! A grammar of the sequences the engine supports, for randomized streams.

#![allow(dead_code)]

use proptest::prelude::*;
use rtty_vt::Terminal;

/// One step of a stream.
#[derive(Clone, Debug)]
pub enum Token {
    Text(String),
    /// A C0 control: BS, HT, LF, VT, FF, CR, BEL, SO, SI.
    Ctrl(u8),
    Cup(u16, u16),
    /// A cursor movement by `n`: `A` `B` `C` `D` `E` `F` `I` `Z`, or an
    /// absolute column or row: `G` `` ` `` `d`.
    Move(char, u16),
    /// An erase, insert, delete, scroll or repeat by `n`: `J` `K` `X` `@`
    /// `P` `L` `M` `S` `T` `b`.
    Edit(char, u16),
    Sgr(String),
    Stbm(u16, u16),
    Esc(&'static str),
    /// DEC private (`?`) or ANSI mode, set or reset.
    Mode(bool, u16, bool),
    /// Designate `B` or `0` into G0 or G1.
    Charset(u8, u8),
    /// OSC 8: open a link to this URI, or close.
    Link(Option<String>),
    /// Anything else the engine answers or records without drawing.
    Quiet(&'static str),
    Resize(u16, u16),
}

/// Which tokens a stream may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// What alacritty_terminal implements as this engine does.
    Oracle,
    /// Everything, including the features alacritty lacks.
    Full,
}

const ORACLE_CHARS: &[char] = &[
    'a', 'b', 'c', 'x', 'y', 'z', 'q', 'l', 'j', '0', '1', ' ', ' ', ' ', '-', '_', '#', '~', '.',
    'é', 'ß', '€', '✓', '│', '日', '本', '語', '中', '😀', '🎉', '\u{301}', '\u{308}', '\u{FE0F}',
    '\u{7f}',
];

const EXTRA_CHARS: &[char] = &['\u{200D}', '🇺', '🇸', '🏽', '👨', '👩', '❤'];

fn text(profile: Profile) -> impl Strategy<Value = Token> {
    let pool: Vec<char> = match profile {
        Profile::Oracle => ORACLE_CHARS.to_vec(),
        Profile::Full => ORACLE_CHARS.iter().chain(EXTRA_CHARS).copied().collect(),
    };
    prop::collection::vec(prop::sample::select(pool), 1..24)
        .prop_map(|chars| Token::Text(chars.into_iter().collect()))
}

fn count() -> impl Strategy<Value = u16> {
    prop_oneof![8 => 0u16..6, 2 => 6u16..40, 1 => 40u16..300]
}

fn sgr_part() -> impl Strategy<Value = String> {
    let simple = prop::sample::select(vec![
        0u16, 1, 2, 3, 4, 5, 7, 8, 9, 22, 23, 24, 25, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37,
        39, 40, 41, 42, 43, 44, 45, 46, 47, 49, 53, 55, 59, 90, 91, 96, 97, 100, 101, 106, 107,
    ])
    .prop_map(|code| code.to_string());
    prop_oneof![
        6 => simple,
        1 => (0u8..=5).prop_map(|style| format!("4:{style}")),
        1 => (prop::sample::select(vec![38u16, 48, 58]), any::<u8>()).prop_map(|(k, n)| format!("{k};5;{n}")),
        1 => (prop::sample::select(vec![38u16, 48, 58]), any::<u8>()).prop_map(|(k, n)| format!("{k}:5:{n}")),
        1 => (prop::sample::select(vec![38u16, 48]), any::<[u8; 3]>())
            .prop_map(|(k, [r, g, b])| format!("{k};2;{r};{g};{b}")),
        1 => (prop::sample::select(vec![38u16, 48, 58]), any::<[u8; 3]>())
            .prop_map(|(k, [r, g, b])| format!("{k}:2::{r}:{g}:{b}")),
    ]
}

fn mode(profile: Profile) -> impl Strategy<Value = Token> {
    let mut dec = vec![1u16, 6, 7, 25, 1049, 2004, 1000, 1006, 1004];
    if profile == Profile::Full {
        dec.extend([47, 1047, 1048, 5, 12, 1002, 1003, 1005, 1015, 2026, 2031, 66, 1007]);
    }
    prop_oneof![
        4 => (prop::sample::select(dec), any::<bool>()).prop_map(|(m, on)| Token::Mode(true, m, on)),
        1 => (prop::sample::select(vec![4u16, 20]), any::<bool>()).prop_map(|(m, on)| Token::Mode(false, m, on)),
    ]
}

fn quiet(profile: Profile) -> impl Strategy<Value = Token> {
    let mut quiet = vec![
        "\x1b]0;title\x07",
        "\x1b]2;a;b\x1b\\",
        "\x1b[c",
        "\x1b[>c",
        "\x1b[6n",
        "\x1b[5n",
        "\x1b[?1;2$p",
        "\x1bP+q544e\x1b\\",
        "\x1b]133;A\x07",
        "\x1b]133;D;0\x07",
        "\x1b]7;file:///tmp\x07",
        "\x1b[2 q",
        "\x1b[18t",
    ];
    if profile == Profile::Full {
        quiet.extend([
            "\x1b[>1u",
            "\x1b[>5u",
            "\x1b[<u",
            "\x1b[=3;2u",
            "\x1b[?u",
            "\x1b]4;1;rgb:ff/00/00\x07",
            "\x1b]11;#102030\x1b\\",
            "\x1b]104\x07",
            "\x1b[>4;2m",
            "\x1b[22t",
            "\x1b[23t",
            "\x1b[!p",
            "\x1bN",
            "\x1b*0\x1bn",
            "\x1b)A",
            "\x1b[3g",
            "\x1b[?5W",
        ]);
    }
    prop::sample::select(quiet).prop_map(Token::Quiet)
}

pub fn token(profile: Profile, rows: u16, cols: u16) -> BoxedStrategy<Token> {
    let moves = prop::sample::select(vec!['A', 'B', 'C', 'D', 'E', 'F', 'G', '`', 'd', 'I', 'Z']);
    let edits = prop::sample::select(vec!['J', 'K', 'X', '@', 'P', 'L', 'M', 'S', 'T', 'b']);
    let escapes = prop::sample::select(vec!["7", "8", "D", "E", "M", "H", "c", "#8", "=", ">"]);
    let tokens = prop_oneof![
        30 => text(profile),
        8 => prop::sample::select(vec![8u8, 9, 10, 11, 12, 13, 13, 10, 7, 14, 15]).prop_map(Token::Ctrl),
        6 => (0..rows + 2, 0..cols + 2).prop_map(|(r, c)| Token::Cup(r, c)),
        6 => (moves, count()).prop_map(|(m, n)| Token::Move(m, n)),
        8 => (edits, count()).prop_map(|(e, n)| Token::Edit(e, if e == 'J' || e == 'K' { n % 4 } else { n })),
        8 => prop::collection::vec(sgr_part(), 1..4).prop_map(|parts| Token::Sgr(parts.join(";"))),
        2 => (0..rows + 2, 0..rows + 2).prop_map(|(t, b)| Token::Stbm(t, b)),
        3 => escapes.prop_map(Token::Esc),
        3 => mode(profile),
        1 => (prop::sample::select(vec![b'(', b')']), prop::sample::select(vec![b'B', b'0']))
            .prop_map(|(slot, set)| Token::Charset(slot, set)),
        1 => prop::option::of(prop::sample::select(vec!["https://a.example/1", "file:///x"]))
            .prop_map(|uri| Token::Link(uri.map(String::from))),
        2 => quiet(profile),
    ];
    match profile {
        Profile::Oracle => tokens.boxed(),
        Profile::Full => prop_oneof![
            80 => tokens,
            1 => (1..rows + 4, 1..cols + 8).prop_map(|(r, c)| Token::Resize(r, c)),
        ]
        .boxed(),
    }
}

/// A stream's size and its tokens.
pub fn stream(profile: Profile) -> impl Strategy<Value = (u16, u16, Vec<Token>)> {
    (1u16..12, 2u16..32).prop_flat_map(move |(rows, cols)| {
        (Just(rows), Just(cols), prop::collection::vec(token(profile, rows, cols), 0..120))
    })
}

impl Token {
    pub fn bytes(&self) -> Vec<u8> {
        let text = match self {
            Token::Text(text) => text.clone(),
            Token::Ctrl(byte) => char::from(*byte).to_string(),
            Token::Cup(r, c) => format!("\x1b[{r};{c}H"),
            Token::Move(m, n) => format!("\x1b[{n}{m}"),
            // IL and DL leave the column where it was in alacritty and at the
            // margin in the VT510 and here: a CR evens that out.
            Token::Edit(e @ ('L' | 'M'), n) => format!("\x1b[{n}{e}\r"),
            Token::Edit(e, n) => format!("\x1b[{n}{e}"),
            Token::Sgr(params) => format!("\x1b[{params}m"),
            Token::Stbm(0, 0) => "\x1b[r".into(),
            Token::Stbm(t, b) => format!("\x1b[{t};{b}r"),
            // DECALN resets the margins and homes the cursor (VT510);
            // alacritty does neither. DECSTBM does both everywhere.
            Token::Esc("#8") => "\x1b#8\x1b[r\x1b[H".into(),
            Token::Esc(e) => format!("\x1b{e}"),
            // Resetting origin mode homes the cursor (VT510); alacritty
            // leaves it. Changing autowrap with a wrap pending differs too.
            Token::Mode(true, m @ (6 | 7), on) => {
                format!("\x1b[?{m}{}\x1b[H", if *on { 'h' } else { 'l' })
            }
            Token::Mode(dec, m, on) => {
                format!("\x1b[{}{m}{}", if *dec { "?" } else { "" }, if *on { 'h' } else { 'l' })
            }
            Token::Charset(slot, set) => format!("\x1b{}{}", char::from(*slot), char::from(*set)),
            Token::Link(Some(uri)) => format!("\x1b]8;;{uri}\x1b\\"),
            Token::Link(None) => "\x1b]8;;\x1b\\".into(),
            Token::Quiet(bytes) => (*bytes).into(),
            Token::Resize(..) => String::new(),
        };
        text.into_bytes()
    }
}

/// What the stream did that the engine's public state does not show but a
/// divergence turns on.
#[derive(Clone, Debug, Default)]
pub struct Model {
    gl: u8,
    /// DECSC slots: origin mode and the invoked character set.
    saved: [Option<(bool, u8, [u8; 2])>; 2],
    /// The invoked set and designations when a character was last printed.
    printed: Option<(u8, [u8; 2])>,
    last_zero_width: bool,
    designated: [u8; 2],
}

/// Nothing on screen alacritty would call content.
fn blank_screen(term: &Terminal) -> bool {
    use rtty_vt::{Attrs, Color, Underline};
    (0..term.size().0).all(|r| {
        let row = term.screen_row(r);
        !row.wrapped()
            && row.cells().iter().enumerate().all(|(col, cell)| {
                let attrs = cell.attrs();
                cell.ch() == ' '
                    && cell.fg() == Color::Default
                    && cell.bg() == Color::Default
                    && cell.width() == 1
                    && !cell.is_pad()
                    && row.cluster(col).is_none()
                    && !attrs.contains(Attrs::INVERSE)
                    && !attrs.contains(Attrs::STRIKE)
                    && attrs.underline() == Underline::None
            })
    })
}

fn zero_width(c: char) -> bool {
    matches!(c, '\u{300}'..='\u{36f}' | '\u{FE00}'..='\u{FE0F}' | '\u{200D}')
}

impl Model {
    pub fn new() -> Model {
        Model { designated: [b'B'; 2], ..Model::default() }
    }

    fn screen(term: &Terminal) -> usize {
        usize::from(term.modes().alt_screen)
    }

    fn restored(&self, slot: usize) -> (bool, u8, [u8; 2]) {
        self.saved[slot].unwrap_or((false, 0, [b'B'; 2]))
    }

    /// Whether alacritty_terminal is known to part from this engine on
    /// `token` in `term`'s state: each such case is listed in
    /// DIVERGENCES.md.
    pub fn diverges(&self, token: &Token, term: &Terminal) -> bool {
        let (rows, cols) = term.size();
        let (row, col) = term.cursor_position();
        let line = term.screen_row(row).cells();
        let at = |c: u16| line.get(usize::from(c)).copied();
        let spacer = |c: u16| at(c).is_some_and(|cell| cell.is_spacer());
        let wide = |c: u16| at(c).is_some_and(|cell| cell.is_wide());
        let pending = term.wrap_pending();
        let full_region = term.scroll_region() == (0, rows - 1);
        let origin = term.modes().origin;
        let any_wide =
            || (0..rows).any(|r| term.screen_row(r).cells().iter().any(|cell| cell.width() != 1));
        let n = |n: u16| n.max(1);
        match token {
            Token::Stbm(top, bottom) => {
                let bottom = if *bottom == 0 { rows } else { *bottom };
                bottom > rows && (*top).max(1) >= rows
            }
            Token::Ctrl(9) => pending,
            Token::Ctrl(10..=12) => pending || term.modes().newline,
            Token::Esc("D" | "M") => pending,
            Token::Move('A' | 'B' | 'E' | 'F', _) => !full_region,
            Token::Move('G' | '`', _) => origin && term.scroll_region().0 > 0,
            Token::Move('Z', _) => pending,
            Token::Edit('P', k) => {
                let pad = line.last().is_some_and(|cell| cell.is_pad());
                n(*k) > cols - col || spacer(col) || wide(col + n(*k) - 1) || pad
            }
            Token::Edit('X', k) => spacer(col) || wide(col.saturating_add(n(*k)) - 1),
            Token::Edit('@', _) => line[usize::from(col)..].iter().any(|cell| cell.width() != 1),
            Token::Edit('J', 0) => spacer(col),
            // alacritty pushes one blank row when an empty screen with no
            // scrollback is cleared.
            Token::Edit('J', 2) => {
                !term.modes().alt_screen
                    && term.oldest_row() == term.screen_top()
                    && blank_screen(term)
            }
            Token::Edit('J', 1) => row == 1 || wide(col),
            Token::Edit('K', 0) => !pending && spacer(col),
            Token::Edit('K', 1) => wide(col),
            // Inserting shifts wide characters apart in alacritty.
            Token::Edit('b', _) => {
                self.printed != Some((self.gl, self.designated))
                    || self.last_zero_width
                    || term.modes().insert
            }
            Token::Text(text) => {
                // A cluster stops growing at 32 characters here.
                let mut cluster = String::new();
                term.screen_row(row)
                    .push_grapheme(usize::from(col.saturating_sub(1)), &mut cluster);
                let marks = text.chars().filter(|&c| zero_width(c)).count();
                let wide_text =
                    text.chars().any(|c| matches!(c, '日' | '本' | '語' | '中' | '😀' | '🎉'));
                // A combining mark is dropped on a pad here; alacritty puts
                // it on its leading spacer. Only a pending wrap or a wide
                // character dropped with autowrap off lands the cursor on one.
                let pad = line.last().is_some_and(|cell| cell.is_pad());
                let on_pad = pad && marks > 0 && (pending || !term.modes().autowrap);
                term.modes().insert && (any_wide() || wide_text)
                    || cluster.chars().count() + marks > 31
                    || on_pad
            }
            // DECRC restores origin mode and the invoked set here, not in
            // alacritty.
            Token::Esc("8") => {
                let (saved_origin, saved_gl, _) = self.restored(Model::screen(term));
                (saved_origin, saved_gl) != (origin, self.gl)
            }
            Token::Mode(true, 1049, false) => {
                let (saved_origin, saved_gl, _) = self.restored(0);
                term.modes().alt_screen && (saved_origin, saved_gl) != (origin, self.gl)
            }
            _ => false,
        }
    }

    /// Follow `token`, about to be applied to `term`.
    pub fn apply(&mut self, token: &Token, term: &Terminal) {
        let screen = Model::screen(term);
        let alt = term.modes().alt_screen;
        match token {
            Token::Ctrl(14) => self.gl = 1,
            Token::Ctrl(15) => self.gl = 0,
            Token::Charset(slot, set) => self.designated[usize::from(*slot == b')')] = *set,
            // alacritty repeats a DEL it ignored; this engine the character
            // before it.
            Token::Text(text) if text.ends_with('\u{7f}') => self.printed = None,
            Token::Text(text) => {
                self.printed = Some((self.gl, self.designated));
                self.last_zero_width = text.chars().last().is_some_and(zero_width);
            }
            Token::Esc("7") => {
                self.saved[screen] = Some((term.modes().origin, self.gl, self.designated))
            }
            Token::Esc("8") => (_, self.gl, self.designated) = self.restored(screen),
            Token::Mode(true, 1049, true) if !alt => {
                self.saved[0] = Some((term.modes().origin, self.gl, self.designated))
            }
            Token::Mode(true, 1049, false) if alt => {
                (_, self.gl, self.designated) = self.restored(0)
            }
            Token::Esc("c") => *self = Model::new(),
            _ => {}
        }
    }
}

/// Another engine fed the stream as it is built, with a say in which tokens
/// it holds.
pub trait Oracle {
    /// Whether `token` would reach a known difference in its state.
    fn vetoes(&self, token: &Token) -> bool;
    fn feed(&mut self, bytes: &[u8]);
}

impl Oracle for () {
    fn vetoes(&self, _: &Token) -> bool {
        false
    }

    fn feed(&mut self, _: &[u8]) {}
}

/// The stream `tokens` make, the divergent ones dropped when `filter`, the
/// engine fed it token by token alongside.
pub fn build(rows: u16, cols: u16, tokens: &[Token], filter: bool) -> (Vec<u8>, Terminal) {
    build_with(rows, cols, tokens, filter, &mut ())
}

/// `build`, feeding `oracle` each token kept too.
pub fn build_with(
    rows: u16,
    cols: u16,
    tokens: &[Token],
    filter: bool,
    oracle: &mut impl Oracle,
) -> (Vec<u8>, Terminal) {
    let mut term = Terminal::new(rows, cols, 100);
    let mut model = Model::new();
    let mut stream = Vec::new();
    for token in tokens {
        if filter && (model.diverges(token, &term) || oracle.vetoes(token)) {
            continue;
        }
        if let Token::Resize(r, c) = token {
            term.resize(*r, *c);
            continue;
        }
        model.apply(token, &term);
        let bytes = token.bytes();
        term.advance(&bytes);
        oracle.feed(&bytes);
        stream.extend(bytes);
    }
    (stream, term)
}
