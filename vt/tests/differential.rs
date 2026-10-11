//! Differential tests against alacritty_terminal: identical byte streams fed
//! to both, then the visible grid, colours, attributes, wrap flags, cursor,
//! modes and scrollback compared. Randomized streams come from the grammar
//! in `common`, minus the cases DIVERGENCES.md lists; recorded program
//! output comes from `fixtures/`.

mod common;

use std::path::Path;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell as TheirCell, Flags};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{self, Color as TheirColor, NamedColor};
use common::{Oracle, Profile, Token, build_with, stream};
use proptest::prelude::*;
use rtty_vt::{Attrs, Color, Row, Terminal, Underline};

/// One cell as both engines can describe it.
#[derive(Clone, Debug, PartialEq)]
struct Seen {
    text: String,
    fg: Color,
    bg: Color,
    /// Bold, dim, italic, inverse, hidden, strike: blink and overline are
    /// not kept by alacritty.
    flags: [bool; 6],
    underline: Underline,
    underline_color: Color,
    /// 0 narrow, 1 wide, 2 a wide character's spacer, 3 a pad.
    layout: u8,
    link: Option<String>,
}

#[derive(Debug, PartialEq)]
struct State {
    screen: Vec<(Vec<Seen>, bool)>,
    history: Vec<(String, bool)>,
    cursor: (u16, u16),
    pending: Option<bool>,
    modes: Vec<(&'static str, bool)>,
}

fn ours(term: &Terminal) -> State {
    let seen = |row: &Row| -> (Vec<Seen>, bool) {
        let cells = row
            .cells()
            .iter()
            .enumerate()
            .map(|(col, cell)| {
                let mut text = String::new();
                row.push_grapheme(col, &mut text);
                let attrs = cell.attrs();
                Seen {
                    text,
                    fg: cell.fg(),
                    bg: cell.bg(),
                    flags: [
                        Attrs::BOLD,
                        Attrs::DIM,
                        Attrs::ITALIC,
                        Attrs::INVERSE,
                        Attrs::HIDDEN,
                        Attrs::STRIKE,
                    ]
                    .map(|attr| attrs.contains(attr)),
                    underline: attrs.underline(),
                    underline_color: row.underline_color(col),
                    layout: match (cell.is_wide(), cell.is_spacer(), cell.is_pad()) {
                        (true, ..) => 1,
                        (_, true, _) => 2,
                        (.., true) => 3,
                        _ => 0,
                    },
                    link: row.hyperlink(col).map(|link| link.uri.clone()),
                }
            })
            .collect();
        (cells, row.wrapped())
    };
    let (rows, _) = term.size();
    let top = term.screen_top();
    let modes = term.modes();
    State {
        screen: (0..rows).map(|r| seen(term.screen_row(r))).collect(),
        // alacritty's grid is the alternate one while it is up, with no
        // scrollback; this engine keeps the primary's in reach.
        // Every cell's text, as alacritty's cells give it: a pad's
        // included, which `Row::text` leaves out.
        history: if modes.alt_screen {
            Vec::new()
        } else {
            term.rows(term.oldest_row(), top)
                .map(|(_, row)| {
                    let mut text = String::new();
                    (0..row.cells().len()).for_each(|col| row.push_grapheme(col, &mut text));
                    (text.trim_end_matches(' ').to_string(), row.wrapped())
                })
                .collect()
        },
        cursor: term.cursor_position(),
        pending: Some(term.wrap_pending()),
        modes: vec![
            ("origin", modes.origin),
            ("autowrap", modes.autowrap),
            ("insert", modes.insert),
            ("newline", modes.newline),
            ("cursor visible", modes.cursor_visible),
            ("alt screen", modes.alt_screen),
            ("cursor keys", modes.cursor_keys),
            ("keypad", modes.keypad),
            ("bracketed paste", modes.bracketed_paste),
            ("focus", modes.focus_reports),
            ("mouse click", modes.mouse == rtty_vt::MouseMode::Click),
            ("sgr mouse", modes.mouse_encoding == rtty_vt::MouseEncoding::Sgr),
        ],
    }
}

fn their_color(color: TheirColor) -> Color {
    match color {
        TheirColor::Named(NamedColor::Foreground | NamedColor::Background) => Color::Default,
        TheirColor::Named(named) => Color::Named(named as u8),
        TheirColor::Indexed(index) => Color::Indexed(index),
        TheirColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

fn their_cell(cell: &TheirCell) -> Seen {
    let flags = cell.flags;
    let text = if flags.contains(Flags::WIDE_CHAR_SPACER) {
        String::new()
    } else {
        let base = if cell.c == '\t' { ' ' } else { cell.c };
        std::iter::once(base).chain(cell.zerowidth().into_iter().flatten().copied()).collect()
    };
    let underline = match () {
        _ if flags.contains(Flags::UNDERLINE) => Underline::Single,
        _ if flags.contains(Flags::DOUBLE_UNDERLINE) => Underline::Double,
        _ if flags.contains(Flags::UNDERCURL) => Underline::Curly,
        _ if flags.contains(Flags::DOTTED_UNDERLINE) => Underline::Dotted,
        _ if flags.contains(Flags::DASHED_UNDERLINE) => Underline::Dashed,
        _ => Underline::None,
    };
    Seen {
        text,
        fg: their_color(cell.fg),
        bg: their_color(cell.bg),
        flags: [
            Flags::BOLD,
            Flags::DIM,
            Flags::ITALIC,
            Flags::INVERSE,
            Flags::HIDDEN,
            Flags::STRIKEOUT,
        ]
        .map(|flag| flags.contains(flag)),
        underline,
        underline_color: cell.underline_color().map_or(Color::Default, their_color),
        layout: if flags.contains(Flags::WIDE_CHAR) {
            1
        } else if flags.contains(Flags::WIDE_CHAR_SPACER) {
            2
        } else if flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
            3
        } else {
            0
        },
        link: cell.hyperlink().map(|link| link.uri().to_string()),
    }
}

fn theirs(term: &Term<VoidListener>) -> State {
    let grid = term.grid();
    let cols = grid.columns();
    let row = |line: Line| -> (Vec<Seen>, bool) {
        let cells = (0..cols).map(|col| their_cell(&grid[line][Column(col)])).collect();
        (cells, grid[line][Column(cols - 1)].flags.contains(Flags::WRAPLINE))
    };
    let text = |(cells, wrapped): (Vec<Seen>, bool)| {
        let text: String = cells.iter().map(|cell| cell.text.as_str()).collect();
        (text.trim_end_matches(' ').to_string(), wrapped)
    };
    let mode = term.mode();
    let cursor = grid.cursor.point;
    State {
        screen: (0..grid.screen_lines()).map(|line| row(Line(line as i32))).collect(),
        history: (1..=grid.history_size())
            .rev()
            .map(|back| text(row(Line(-(back as i32)))))
            .collect(),
        cursor: (cursor.line.0 as u16, cursor.column.0 as u16),
        pending: Some(grid.cursor.input_needs_wrap),
        modes: vec![
            ("origin", mode.contains(TermMode::ORIGIN)),
            ("autowrap", mode.contains(TermMode::LINE_WRAP)),
            ("insert", mode.contains(TermMode::INSERT)),
            ("newline", mode.contains(TermMode::LINE_FEED_NEW_LINE)),
            ("cursor visible", mode.contains(TermMode::SHOW_CURSOR)),
            ("alt screen", mode.contains(TermMode::ALT_SCREEN)),
            ("cursor keys", mode.contains(TermMode::APP_CURSOR)),
            ("keypad", mode.contains(TermMode::APP_KEYPAD)),
            ("bracketed paste", mode.contains(TermMode::BRACKETED_PASTE)),
            ("focus", mode.contains(TermMode::FOCUS_IN_OUT)),
            ("mouse click", mode.contains(TermMode::MOUSE_REPORT_CLICK)),
            ("sgr mouse", mode.contains(TermMode::SGR_MOUSE)),
        ],
    }
}

/// alacritty fed `stream` in reads of about `chunk` bytes, each read ending
/// on a whole UTF-8 character: vte 0.15 can drop the character after one
/// split across reads, which this engine works around.
fn alacritty(
    rows: u16,
    cols: u16,
    history: usize,
    stream: &[u8],
    chunk: usize,
) -> Term<VoidListener> {
    let config = Config { scrolling_history: history, kitty_keyboard: true, ..Config::default() };
    let mut term =
        Term::new(config, &TermSize::new(usize::from(cols), usize::from(rows)), VoidListener);
    let mut parser: ansi::Processor = ansi::Processor::new();
    let mut rest = stream;
    while !rest.is_empty() {
        let mut end = chunk.max(1).min(rest.len());
        while end < rest.len() && rest[end] & 0xc0 == 0x80 {
            end += 1;
        }
        parser.advance(&mut term, &rest[..end]);
        rest = &rest[end..];
    }
    term
}

/// The first difference between two states, described.
fn difference(ours: &State, theirs: &State) -> Option<String> {
    for (r, (a, b)) in ours.screen.iter().zip(&theirs.screen).enumerate() {
        // A wide character rewritten where a pad above made room for it
        // keeps the pad here; alacritty turns it into a plain blank, which
        // it stays as whatever happens to the row after.
        let mut b = b.clone();
        if let (Some(ours_last), Some(their_last)) = (a.0.last(), b.0.last_mut())
            && ours_last.layout == 3
            && their_last.layout == 0
        {
            their_last.layout = 3;
        }
        let b = &b;
        if a.1 != b.1 {
            return Some(format!("row {r} wrapped: ours {} theirs {}", a.1, b.1));
        }
        for (c, (x, y)) in a.0.iter().zip(&b.0).enumerate() {
            if x != y {
                return Some(format!("cell {r},{c}:\n ours   {x:?}\n theirs {y:?}"));
            }
        }
    }
    if ours.cursor != theirs.cursor || ours.pending != theirs.pending {
        return Some(format!(
            "cursor: ours {:?} pending {:?}, theirs {:?} pending {:?}",
            ours.cursor, ours.pending, theirs.cursor, theirs.pending
        ));
    }
    if ours.modes != theirs.modes {
        return Some(format!("modes: ours {:?}\n theirs {:?}", ours.modes, theirs.modes));
    }
    if ours.history != theirs.history {
        return Some(format!("history:\n ours   {:?}\n theirs {:?}", ours.history, theirs.history));
    }
    None
}

fn screen_text(state: &State) -> String {
    state
        .screen
        .iter()
        .map(|(cells, wrapped)| {
            let text: String = cells.iter().map(|cell| cell.text.as_str()).collect();
            format!("{text:?}{}\n", if *wrapped { " ↩" } else { "" })
        })
        .collect()
}

/// alacritty fed token by token as the stream is built.
struct Alacritty {
    term: Term<VoidListener>,
    parser: ansi::Processor,
}

impl Oracle for Alacritty {
    /// alacritty keeps a row's soft wrap on its last cell, so deleting
    /// characters moves it along the row and inserting can move it back to
    /// the end, wrapping the row again.
    fn vetoes(&self, token: &Token) -> bool {
        let inserts = match token {
            Token::Edit('@', _) => true,
            Token::Text(_) | Token::Edit('b', _) => self.term.mode().contains(TermMode::INSERT),
            _ => false,
        };
        let grid = self.term.grid();
        let last = grid.columns() - 1;
        inserts
            && (0..grid.screen_lines()).any(|line| {
                (0..last)
                    .any(|col| grid[Line(line as i32)][Column(col)].flags.contains(Flags::WRAPLINE))
            })
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }
}

fn check(
    rows: u16,
    cols: u16,
    tokens: &[common::Token],
    chunk: usize,
) -> Result<(), TestCaseError> {
    let mut oracle =
        Alacritty { term: alacritty(rows, cols, 100, b"", 1), parser: ansi::Processor::new() };
    let (stream, term) = build_with(rows, cols, tokens, true, &mut oracle);
    let ours = ours(&term);
    let mut chunked = Terminal::new(rows, cols, 100);
    for bytes in stream.chunks(chunk) {
        chunked.advance(bytes);
    }
    prop_assert_eq!(&self::ours(&chunked), &ours, "reads of {} bytes", chunk);
    let theirs_chunked = theirs(&alacritty(rows, cols, 100, &stream, chunk));
    prop_assert_eq!(
        &theirs(&oracle.term),
        &theirs_chunked,
        "alacritty in reads of {} bytes",
        chunk
    );
    if let Some(difference) = difference(&ours, &theirs_chunked) {
        return Err(TestCaseError::fail(format!(
            "{difference}\nstream {:?}\nours:\n{}theirs:\n{}",
            String::from_utf8_lossy(&stream),
            screen_text(&ours),
            screen_text(&theirs_chunked)
        )));
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 1000, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn random_streams_match_alacritty((rows, cols, tokens) in stream(Profile::Oracle), chunk in 1usize..64) {
        check(rows, cols, &tokens, chunk)?;
    }
}

/// The long campaign behind the report: `cargo test --release -- --ignored`.
#[test]
#[ignore]
fn many_random_streams_match_alacritty() {
    let config =
        ProptestConfig { cases: 100_000, failure_persistence: None, ..ProptestConfig::default() };
    proptest!(config, |((rows, cols, tokens) in stream(Profile::Oracle), chunk in 1usize..64)| {
        check(rows, cols, &tokens, chunk)?;
    });
}

/// Every recorded program in `fixtures/`: `<name>.out` raw output and
/// `<name>.json` with its `rows` and `cols`.
#[test]
fn fixtures_match_alacritty() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut failures = Vec::new();
    let mut names: Vec<_> = entries
        .filter_map(|entry| entry.ok()?.path().file_stem()?.to_str().map(String::from))
        .filter(|name| dir.join(format!("{name}.out")).exists())
        .collect();
    names.sort();
    names.dedup();
    for name in names {
        let output = std::fs::read(dir.join(format!("{name}.out"))).expect("readable fixture");
        let meta =
            std::fs::read_to_string(dir.join(format!("{name}.json"))).expect("fixture metadata");
        let rows = json_number(&meta, "rows").expect("rows") as u16;
        let cols = json_number(&meta, "cols").expect("cols") as u16;
        let mut term = Terminal::new(rows, cols, 10_000);
        for chunk in output.chunks(4096) {
            term.advance(chunk);
        }
        let theirs = theirs(&alacritty(rows, cols, 10_000, &output, 4096));
        if let Some(difference) = difference(&ours(&term), &theirs) {
            failures.push(format!("{name}: {difference}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// The number after `"key":` in a flat JSON object.
fn json_number(json: &str, key: &str) -> Option<u64> {
    let rest = &json[json.find(&format!("\"{key}\""))? + key.len() + 2..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}
