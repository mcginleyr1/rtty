//! Finding text in a terminal's scrollback and screen, independent of GPUI. Matches are cells by
//! absolute row, so they stay on their text while output scrolls it up.

use regex::{Regex, RegexBuilder};
use rtty_vt::Terminal;

use crate::selection::Point;

/// A match's first and last cell, inclusive: a wide character's spacer is its last cell.
pub type Match = (Point, Point);

/// What to look for. Smart-case: a query without an upper-case letter ignores case.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub text: String,
    pub regex: bool,
}

impl Query {
    /// The pattern, or why it is not one (a regex that does not parse). `None` for nothing.
    pub fn pattern(&self) -> Option<Result<Regex, String>> {
        if self.text.is_empty() {
            return None;
        }
        let source = if self.regex { self.text.clone() } else { regex::escape(&self.text) };
        let caseless = !self.text.chars().any(char::is_uppercase);
        let built = RegexBuilder::new(&source).case_insensitive(caseless).build();
        Some(built.map_err(|err| err.to_string()))
    }
}

/// Every match of `pattern` in the held rows (scrollback and screen), top to bottom. A line that
/// soft-wraps is searched as one, so a match runs on across the wrap; an empty match is none.
pub fn find(term: &Terminal, pattern: &Regex) -> Vec<Match> {
    let end = term.screen_top() + u64::from(term.size().0);
    let mut matches = Vec::new();
    // The logical line so far, and where each grapheme in it starts and the cells it covers.
    let mut text = String::new();
    let mut cells: Vec<(usize, Point, Point)> = Vec::new();
    for (index, row) in term.rows(term.oldest_row(), end) {
        let row_cells = row.cells();
        for (col, cell) in row_cells.iter().enumerate() {
            if cell.is_spacer() || cell.is_pad() {
                continue;
            }
            let col = col as u16;
            let last = col + u16::from(cell.width().max(1)) - 1;
            cells.push((text.len(), (index, col), (index, last)));
            row.push_grapheme(usize::from(col), &mut text);
        }
        if row.wrapped() {
            continue;
        }
        let trimmed = text.trim_end_matches(' ').len();
        for found in pattern.find_iter(&text[..trimmed]).filter(|found| !found.is_empty()) {
            let first = cells.partition_point(|&(start, ..)| start <= found.start()) - 1;
            let last = cells.partition_point(|&(start, ..)| start < found.end()) - 1;
            matches.push((cells[first].1, cells[last].2));
        }
        text.clear();
        cells.clear();
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(rows: u16, cols: u16, bytes: &[u8]) -> Terminal {
        let mut term = Terminal::new(rows, cols, 100);
        term.advance(bytes);
        term
    }

    fn query(text: &str, regex: bool) -> Regex {
        Query { text: text.into(), regex }.pattern().unwrap().unwrap()
    }

    #[test]
    fn smart_case_ignores_case_until_the_query_has_a_capital() {
        let term = term(3, 20, b"Error error ERROR");
        assert_eq!(find(&term, &query("error", false)).len(), 3);
        assert_eq!(find(&term, &query("Error", false)), [((0, 0), (0, 4))]);
        // Plain text is literal; a regex is a regex, and a bad one says why.
        assert!(find(&term, &query("e.r", false)).is_empty());
        assert_eq!(find(&term, &query("e.r", true)).len(), 3);
        assert_eq!(find(&term, &query("R+O", true)), [((0, 13), (0, 15))]);
        assert!(Query { text: "(".into(), regex: true }.pattern().unwrap().is_err());
        assert!(Query::default().pattern().is_none());
    }

    #[test]
    fn a_match_runs_on_across_a_soft_wrap() {
        // 10 columns: each line fills its first row and wraps at "nee|".
        let wrapped = term(4, 10, b"1234567nee dle\r\nabcdefgneedle");
        assert_eq!(find(&wrapped, &query("nee dle", false)), [((0, 7), (1, 3))]);
        assert_eq!(find(&wrapped, &query("needle", false)), [((2, 7), (3, 2))]);
        // A hard line break is no wrap: no match across it.
        let lines = term(3, 10, b"one\r\ntwo");
        assert!(find(&lines, &query("onetwo", false)).is_empty());
    }

    #[test]
    fn wide_characters_take_both_their_cells() {
        // 漢 and 字 are two cells each: "a漢字b" covers columns 0, 1–2, 3–4, 5.
        let wide = term(2, 10, "a漢字b".as_bytes());
        assert_eq!(find(&wide, &query("漢字", false)), [((0, 1), (0, 4))]);
        assert_eq!(find(&wide, &query("字b", false)), [((0, 3), (0, 5))]);
        // A wide character that did not fit leaves a pad, which is no text.
        let padded = term(3, 4, "abc字d".as_bytes());
        assert_eq!(find(&padded, &query("c字", false)), [((0, 2), (1, 1))]);
    }

    #[test]
    fn matches_stay_on_their_text_while_output_scrolls() {
        let mut term = term(3, 20, b"first marker\r\n");
        let before = find(&term, &query("marker", false));
        assert_eq!(before, [((0, 6), (0, 11))]);
        term.advance(b"a\r\nb\r\nc\r\nd\r\nsecond marker");
        assert!(term.screen_top() > 0, "the first line went into the scrollback");
        let after = find(&term, &query("marker", false));
        assert_eq!(after[0], before[0], "the old match keeps its cells");
        assert_eq!(after[1], ((5, 7), (5, 12)));
        for (start, end) in after {
            assert_eq!(term.text(start, end), "marker");
        }
    }

    #[test]
    fn trailing_blanks_are_no_text_and_an_empty_match_is_none() {
        let term = term(2, 10, b"ab");
        assert!(find(&term, &query("b ", false)).is_empty());
        assert!(find(&term, &query("x*", true)).is_empty());
    }
}
