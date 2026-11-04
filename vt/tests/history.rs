//! `history(from, to)` prints scrollback rows for another terminal to replay
//! into its own scrollback: logical lines, each in its renditions, links and
//! clusters, that terminal wrapping them at its own width.

mod common;

use common::{Profile, build, stream};
use proptest::prelude::*;
use rtty_vt::{Attrs, Color, Hyperlink, Row, Terminal};

/// One cell as a reader sees it: its text, colours, rendition, width,
/// underline colour and link.
type Seen = (String, Color, Color, Attrs, u8, Color, Option<Hyperlink>);

fn seen(row: &Row, col: usize) -> Seen {
    let cell = &row.cells()[col];
    let mut text = String::new();
    row.push_grapheme(col, &mut text);
    let link = row.hyperlink(col).map(|link| (**link).clone());
    (text, cell.fg(), cell.bg(), cell.attrs(), cell.width(), row.underline_color(col), link)
}

/// `text` in a one-column cell of the plain rendition.
fn plain(text: &str) -> Seen {
    (text.into(), Color::Default, Color::Default, Attrs::default(), 1, Color::Default, None)
}

/// The logical lines of rows `from..to`: soft-wrapped rows joined, spacers
/// and pads left out, each line's trailing plain blanks trimmed.
fn logical(term: &Terminal, from: u64, to: u64) -> Vec<Vec<Seen>> {
    let (mut lines, mut line) = (Vec::new(), Vec::new());
    let mut rows = term.rows(from, to).peekable();
    while let Some((_, row)) = rows.next() {
        let cells = row.cells();
        line.extend(
            (0..cells.len())
                .filter(|&col| !cells[col].is_spacer() && !cells[col].is_pad())
                .map(|col| seen(row, col)),
        );
        if !(row.wrapped() && rows.peek().is_some()) {
            while line.last() == Some(&plain(" ")) {
                line.pop();
            }
            lines.push(std::mem::take(&mut line));
        }
    }
    lines
}

/// `history` replayed on a terminal `cols` wide: its logical lines, from its
/// first row to the cursor's, which each line's end leaves at column 0. The
/// terminal is tall enough never to scroll: one that scrolls a wrapping
/// line's next row in fills it with the line's background (BCE), past the
/// line's end — the host's doing, as with the program's own output.
fn replayed(history: &[u8], cols: u16) -> (Vec<Vec<Seen>>, Terminal) {
    let mut host = Terminal::new((history.len() + 1).min(4096) as u16, cols, 0);
    host.advance(history);
    let (row, col) = host.cursor_position();
    assert_eq!((host.screen_top(), col), (0, 0), "no scroll, and the last line ended");
    (logical(&host, 0, u64::from(row)), host)
}

/// Where each line of `history` ends: the cursor's column after it, on a
/// terminal too wide for any line to wrap. The cell comparisons cannot see
/// printed trailing blanks — on the host they are cells like any unwritten
/// one — but the cursor stands past them.
fn ends(history: &[u8]) -> Vec<u32> {
    let mut host = Terminal::new(1, u16::MAX, 0);
    let mut ends = Vec::new();
    let mut rest = history;
    while let Some(at) = rest.windows(2).position(|pair| pair == b"\r\n") {
        host.advance(&rest[..at]);
        ends.push(u32::from(host.cursor_position().1));
        host.advance(b"\r\n");
        rest = &rest[at + 2..];
    }
    ends
}

/// The width of each of `lines`.
fn widths(lines: &[Vec<Seen>]) -> Vec<u32> {
    lines.iter().map(|line| line.iter().map(|seen| u32::from(seen.4)).sum()).collect()
}

/// After the history the host prints in the plain rendition, no link open.
fn left_plain(host: &mut Terminal) -> Seen {
    host.advance(b"x");
    let (row, col) = host.cursor_position();
    seen(host.screen_row(row), usize::from(col) - 1)
}

#[test]
fn history_replays_logical_lines_for_the_host_to_wrap() {
    let mut term = Terminal::new(3, 10, 100);
    term.advance(
        "\x1b[31mred\x1b[m plain\r\n\
         \x1b]8;id=a;https://a.example/\x1b\\link\x1b]8;;\x1b\\ after\r\n\
         日本 e\u{301} 👩\u{200d}❤\r\n\
         0123456789abcdefghij-xyz\r\n\
         \x1b[44mblue\x1b[K\x1b[m\r\n\
         \r\n\r\n\r\n"
            .as_bytes(),
    );
    let (from, to) = (term.oldest_row(), term.screen_top());
    let history = term.history(from, to);
    let want = [
        "red plain",
        "link after",
        "日本 e\u{301} 👩\u{200d}❤",
        "0123456789abcdefghij-xyz",
        "blue      ",
        "",
    ];
    for cols in [40, 6, 3] {
        let (lines, mut host) = replayed(&history, cols);
        let texts: Vec<String> =
            lines.iter().map(|line| line.iter().map(|seen| seen.0.as_str()).collect()).collect();
        assert_eq!(texts, want, "{cols} wide");
        assert_eq!(lines, logical(&term, from, to), "{cols} wide");
        assert_eq!(lines[0][0].1, Color::Named(1), "red");
        assert!(lines[1][3].6.is_some() && lines[1][4].6.is_none(), "the link ends after it");
        assert_eq!(lines[4][9].2, Color::Named(4), "the erased row's blue runs to its end");
        assert_eq!(left_plain(&mut host), plain("x"), "{cols} wide");
    }
    let (lines, host) = replayed(&history, 40);
    assert_eq!(lines.len(), 6);
    assert!(host.rows(0, 6).all(|(_, row)| !row.wrapped()), "40 wide, nothing wraps");
    let (_, host) = replayed(&history, 6);
    let wrapped: Vec<bool> = host.rows(0, 4).map(|(_, row)| row.wrapped()).collect();
    assert_eq!(wrapped, [true, false, true, false], "the host wraps at its own width");
}

/// A range that starts or ends inside a logical line prints the part it
/// holds as a line of its own.
#[test]
fn a_range_cuts_its_lines_at_its_ends() {
    let mut term = Terminal::new(2, 4, 100);
    term.advance(b"abcdefghij\r\nk\r\n\r\n");
    assert_eq!(term.history(1, 2), b"efgh\r\n");
    assert_eq!(term.history(0, 2), b"abcdefgh\r\n");
    assert_eq!(term.history(2, 4), b"ij\r\nk\r\n");
    assert_eq!(term.history(0, 0), b"");
}

/// A line's trailing plain blanks are left out though they run across a soft
/// wrap; blanks inside it stay, though a soft wrap ends their row.
#[test]
fn trailing_blanks_go_across_a_soft_wrap() {
    let mut term = Terminal::new(2, 4, 100);
    term.advance(b"abc  \r\nab  cd\r\n\r\n");
    let (from, to) = (term.oldest_row(), term.screen_top());
    assert_eq!(term.history(from, to), b"abc\r\nab  cd\r\n");
    assert_eq!(ends(&term.history(from, to)), widths(&logical(&term, from, to)));
}

/// Every recorded program in `fixtures/`: its whole history replayed
/// narrower and wider than it ran.
#[test]
fn fixtures_replay_their_history() {
    for common::Fixture { name, output, rows, cols, .. } in common::fixtures() {
        let mut term = Terminal::new(rows, cols, 1_000);
        term.advance(&output);
        let (from, to) = (term.oldest_row(), term.screen_top());
        let history = term.history(from, to);
        let want = logical(&term, from, to);
        for width in [cols / 2 + 1, cols + 7] {
            assert_eq!(replayed(&history, width).0, want, "{name}, {width} wide");
        }
        assert_eq!(ends(&history), widths(&want), "{name}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 500, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn a_history_replays_its_lines_at_any_width(
        (rows, cols, tokens) in stream(Profile::Full),
        width in 2u16..80,
        back in 0u64..120,
    ) {
        let (_, term) = build(rows, cols, &tokens, false);
        let to = term.screen_top();
        let from = to.saturating_sub(back).max(term.oldest_row());
        let history = term.history(from, to);
        let (lines, mut host) = replayed(&history, width);
        let want = logical(&term, from, to);
        prop_assert_eq!(&lines, &want, "history {:?}", String::from_utf8_lossy(&history));
        prop_assert_eq!(ends(&history), widths(&want), "history {:?}", String::from_utf8_lossy(&history));
        prop_assert_eq!(left_plain(&mut host), plain("x"));
    }
}
