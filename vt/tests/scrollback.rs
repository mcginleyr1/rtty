//! Scrollback with absolute row numbers, and resize with reflow.

use proptest::prelude::*;
use rtty_vt::{MarkKind, Terminal};

fn numbered(first: usize, n: usize) -> String {
    (first..first + n).map(|i| format!("L{i}\r\n")).collect()
}

fn texts(term: &Terminal, from: u64, to: u64) -> Vec<String> {
    term.rows(from, to).map(|(_, row)| row.text()).collect()
}

fn screen(term: &Terminal) -> Vec<String> {
    (0..term.size().0).map(|r| term.screen_row(r).text()).collect()
}

/// The held rows' lines, soft-wrapped rows joined, trailing blank ones
/// dropped.
fn lines(term: &Terminal) -> Vec<String> {
    let mut lines = vec![String::new()];
    for (_, row) in term.rows(term.oldest_row(), u64::MAX) {
        let mut text = String::new();
        for col in 0..row.cells().len() {
            if !row.cells()[col].is_pad() {
                row.push_grapheme(col, &mut text);
            }
        }
        let last = lines.last_mut().unwrap();
        last.push_str(&text);
        if !row.wrapped() {
            last.truncate(last.trim_end().len());
            lines.push(String::new());
        }
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

#[test]
fn every_committed_row_has_an_absolute_index() {
    let mut term = Terminal::new(5, 20, 1000);
    term.advance(numbered(0, 40).as_bytes());
    // Four rows fill the screen above the cursor's; each line after pushes
    // one.
    assert_eq!((term.oldest_row(), term.screen_top()), (0, 36));
    assert_eq!(texts(&term, 30, 36), ["L30", "L31", "L32", "L33", "L34", "L35"]);
    assert_eq!(texts(&term, 36, 41), ["L36", "L37", "L38", "L39", ""]);
    assert_eq!(term.row(5).unwrap().text(), "L5");
    assert!(term.row(41).is_none());
}

#[test]
fn the_ring_is_bounded_and_the_numbering_goes_on() {
    let mut term = Terminal::new(5, 20, 10);
    term.advance(numbered(0, 40).as_bytes());
    assert_eq!((term.oldest_row(), term.screen_top()), (26, 36));
    assert_eq!(texts(&term, 0, 28), ["L26", "L27"]);
    assert!(term.row(25).is_none());
}

#[test]
fn the_scrollback_stays_in_reach_under_the_alternate_screen() {
    let mut term = Terminal::new(5, 20, 100);
    term.advance(numbered(0, 10).as_bytes());
    term.advance(format!("\x1b[?1049h{}", numbered(100, 20)).as_bytes());
    assert_eq!(term.screen_top(), 6, "the alternate screen pushes nothing");
    assert_eq!(texts(&term, 0, 6), ["L0", "L1", "L2", "L3", "L4", "L5"]);
    assert_eq!(term.row(6).unwrap().text(), "L116", "the visible screen is the alternate one");
    term.advance(b"\x1b[?1049l");
    assert_eq!(term.row(6).unwrap().text(), "L6");
}

#[test]
fn clearing_the_screen_scrolls_it_back_and_clearing_the_scrollback_drops_it() {
    let mut term = Terminal::new(5, 20, 100);
    term.advance(b"a\r\nb\r\nc\x1b[2J");
    assert_eq!(texts(&term, 0, 3), ["a", "b", "c"]);
    assert_eq!(screen(&term), ["", "", "", "", ""]);
    assert_eq!(term.screen_top(), 3);
    term.advance(b"\x1b[3J");
    assert_eq!((term.oldest_row(), term.screen_top()), (3, 3));
    term.advance(format!("\x1bc{}", numbered(0, 6)).as_bytes());
    assert_eq!((term.oldest_row(), term.screen_top()), (3, 5), "a reset carries the count on");
}

#[test]
fn rows_deleted_at_the_top_go_into_the_scrollback() {
    let mut term = Terminal::new(3, 10, 100);
    term.advance(b"a\r\nb\r\nc\x1b[H\x1b[M");
    assert_eq!(term.row(0).unwrap().text(), "a");
    assert_eq!(screen(&term), ["b", "c", ""]);
}

#[test]
fn narrowing_and_widening_rewrap_lines_and_the_cursor_follows() {
    let mut term = Terminal::new(4, 10, 100);
    term.advance(b"0123456789abcdef\r\nshort\r\n$ ab");
    assert_eq!(screen(&term), ["0123456789", "abcdef", "short", "$ ab"]);
    term.resize(4, 6);
    assert_eq!(lines(&term), ["0123456789abcdef", "short", "$ ab"]);
    // Five rows now: the first goes into the scrollback to keep the cursor
    // in view.
    assert_eq!(screen(&term), ["6789ab", "cdef", "short", "$ ab"]);
    assert_eq!(term.row(0).unwrap().text(), "012345");
    assert_eq!(term.cursor_position(), (3, 4));
    term.advance(b"c");
    assert_eq!(term.screen_row(3).text(), "$ abc");
    term.resize(4, 20);
    assert_eq!(screen(&term), ["0123456789abcdef", "short", "$ abc", ""]);
    assert_eq!(term.cursor_position(), (2, 5));
}

#[test]
fn growing_pulls_rows_back_and_shrinking_keeps_the_cursor_in_view() {
    let mut term = Terminal::new(4, 10, 100);
    term.advance(numbered(0, 8).as_bytes());
    assert_eq!(screen(&term), ["L5", "L6", "L7", ""]);
    term.resize(6, 10);
    assert_eq!(screen(&term), ["L3", "L4", "L5", "L6", "L7", ""]);
    assert_eq!(term.cursor_position(), (5, 0));
    term.resize(2, 10);
    assert_eq!(screen(&term), ["L7", ""]);
    // Rows below the cursor go first.
    let mut term = Terminal::new(6, 10, 100);
    term.advance(b"top\r\nprompt");
    term.resize(3, 10);
    assert_eq!(screen(&term), ["top", "prompt", ""]);
    assert_eq!(term.screen_top(), 0);
}

#[test]
fn wide_characters_reflow_whole() {
    let mut term = Terminal::new(3, 6, 100);
    term.advance("ab日本語".as_bytes());
    assert_eq!(screen(&term), ["ab日本", "語", ""]);
    term.resize(3, 5);
    assert_eq!(screen(&term), ["ab日", "本語", ""]);
    assert!(term.screen_row(0).cells()[4].is_pad());
    term.resize(3, 8);
    assert_eq!(screen(&term), ["ab日本語", "", ""]);
}

#[test]
fn marks_follow_their_characters_through_a_reflow() {
    let mut term = Terminal::new(4, 10, 100);
    term.advance(b"0123456789abc\x1b]133;C\x07def\r\n\x1b]133;D;0\x07");
    let before = term.marks(0, 100);
    assert_eq!((before[0].row, before[0].col), (1, 3));
    term.resize(4, 4);
    let after = term.marks(0, 100);
    let row = term.row(after[0].row).unwrap();
    assert_eq!(row.cells()[usize::from(after[0].col)].ch(), 'd');
    assert_eq!(after[1].kind, MarkKind::CommandEnd { exit: Some(0) });
    assert_eq!(after[1].col, 0);
}

#[test]
fn the_alternate_screen_is_cut_not_reflowed() {
    let mut term = Terminal::new(3, 10, 100);
    term.advance(b"0123456789abc\x1b[?1049h\x1b[Hxxxxxxxxxyyy\x1b[2;3H");
    term.resize(3, 5);
    assert_eq!(screen(&term), ["xxxxx", "yy", ""]);
    assert_eq!(term.cursor_position(), (1, 2));
    term.advance(b"\x1b[?1049l");
    assert_eq!(lines(&term), ["0123456789abc"]);
    assert_eq!(term.cursor_position(), (2, 3), "the saved cursor reflowed too");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, failure_persistence: None, ..ProptestConfig::default() })]

    /// Through any resizes, the lines a program wrote read the same, the
    /// cursor stays on the character after them, and no absolute index goes
    /// backwards.
    #[test]
    fn resizes_keep_lines_and_numbering(
        text in prop::collection::vec("[a-z日本 ]{0,40}", 1..30),
        sizes in prop::collection::vec((2u16..12, 2u16..30), 1..8),
    ) {
        let mut term = Terminal::new(6, 12, 10_000);
        let stream = text.join("\r\n");
        term.advance(stream.as_bytes());
        let want: Vec<String> = text.iter().map(|line| line.trim_end().to_string()).collect();
        let mut want = want;
        while want.last().is_some_and(String::is_empty) {
            want.pop();
        }
        let (mut top, mut oldest) = (term.screen_top(), term.oldest_row());
        for (rows, cols) in sizes {
            term.resize(rows, cols);
            prop_assert_eq!(lines(&term), want.clone());
            prop_assert!(term.screen_top() >= top && term.oldest_row() >= oldest);
            (top, oldest) = (term.screen_top(), term.oldest_row());
            // The cursor sits just past the last line's text.
            let (row, col) = term.cursor_position();
            let last = term.screen_row(row);
            let mut before = String::new();
            for c in 0..usize::from(col) + usize::from(term.wrap_pending()) {
                last.push_grapheme(c, &mut before);
            }
            let tail = text.last().unwrap();
            prop_assert!(tail.ends_with(before.trim_start()), "{:?} vs {:?}", before, tail);
        }
    }
}
