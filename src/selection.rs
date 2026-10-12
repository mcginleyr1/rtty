//! Selections by absolute `(row, col)`: rows keep their indices while output scrolls them into
//! the scrollback, so a selection stays on its text. A double click selects words, a triple click
//! lines, and either extends by whole words or lines as the pointer drags.

use rtty_vt::Terminal;

/// A cell by absolute row (`Terminal::row`) and column.
pub type Point = (u64, u16);

/// Which half of a cell the pointer is over: dragging from the right half of a cell leaves that
/// cell out, as it does in a text field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Cells,
    Words,
    Lines,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    kind: Kind,
    anchor: (Point, Side),
    head: (Point, Side),
}

/// What ends a word besides whitespace.
const SEPARATORS: &str = ",│`|:\"'()[]{}<>";

impl Selection {
    pub fn new(kind: Kind, at: Point, side: Side) -> Selection {
        Selection { kind, anchor: (at, side), head: (at, side) }
    }

    pub fn extend(&mut self, at: Point, side: Side) {
        self.head = (at, side);
    }

    /// The first and last selected cells, inclusive, in reading order; `None` when the pointer
    /// has not covered a whole cell.
    pub fn range(&self, term: &Terminal) -> Option<(Point, Point)> {
        let (start, end) = if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        let last_col = term.size().1 - 1;
        match self.kind {
            Kind::Cells => {
                let first = match start {
                    ((row, col), Side::Right) if col == last_col => (row + 1, 0),
                    ((row, col), Side::Right) => (row, col + 1),
                    (point, Side::Left) => point,
                };
                let last = match end {
                    ((0, 0), Side::Left) => return None,
                    ((row, 0), Side::Left) => (row - 1, last_col),
                    ((row, col), Side::Left) => (row, col - 1),
                    (point, Side::Right) => point,
                };
                (first <= last).then_some((first, last))
            }
            Kind::Words => Some((word_edge(term, start.0, -1), word_edge(term, end.0, 1))),
            Kind::Lines => Some((line_edge(term, start.0.0, -1), line_edge(term, end.0.0, 1))),
        }
    }
}

/// Whether the cell is part of a word; a wide character's right half counts as its left.
fn in_word(term: &Terminal, (row, col): Point) -> bool {
    let Some(row) = term.row(row) else { return false };
    let cells = row.cells();
    let cell = if cells[usize::from(col)].is_spacer() && col > 0 {
        cells[usize::from(col) - 1]
    } else {
        cells[usize::from(col)]
    };
    !cell.ch().is_whitespace() && !SEPARATORS.contains(cell.ch())
}

/// The cell next to `at` in reading order (`step` −1 or 1), across a soft wrap but not a hard
/// line end.
fn neighbour(term: &Terminal, (row, col): Point, step: i8) -> Option<Point> {
    let last_col = term.size().1 - 1;
    let wraps_into = |row: u64| term.row(row).is_some_and(|row| row.wrapped());
    match step {
        -1 if col > 0 => Some((row, col - 1)),
        -1 => (row > 0 && wraps_into(row - 1)).then(|| (row - 1, last_col)),
        _ if col < last_col => Some((row, col + 1)),
        _ => (wraps_into(row) && term.row(row + 1).is_some()).then_some((row + 1, 0)),
    }
}

/// The far end of the word at `at` going `step`; a cell outside any word is its own word.
fn word_edge(term: &Terminal, at: Point, step: i8) -> Point {
    let mut edge = at;
    if in_word(term, at) {
        while let Some(next) = neighbour(term, edge, step).filter(|&next| in_word(term, next)) {
            edge = next;
        }
    }
    edge
}

/// The first cell of the line `row` belongs to (`step` −1) or its last (1): a line runs on
/// through soft wraps.
fn line_edge(term: &Terminal, mut row: u64, step: i8) -> Point {
    let wrapped = |row: u64| term.row(row).is_some_and(|row| row.wrapped());
    if step < 0 {
        while row > 0 && wrapped(row - 1) {
            row -= 1;
        }
        (row, 0)
    } else {
        while wrapped(row) && term.row(row + 1).is_some() {
            row += 1;
        }
        (row, term.size().1 - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(bytes: &[u8]) -> Terminal {
        let mut term = Terminal::new(4, 10, 100);
        term.advance(bytes);
        term
    }

    fn selected(term: &Terminal, selection: Selection) -> Option<String> {
        selection.range(term).map(|(start, end)| term.text(start, end))
    }

    #[test]
    fn a_click_selects_nothing_and_a_drag_covers_whole_cells() {
        let term = term(b"hello world");
        let mut selection = Selection::new(Kind::Cells, (0, 1), Side::Left);
        assert_eq!(selected(&term, selection), None);
        selection.extend((0, 1), Side::Right);
        assert_eq!(selected(&term, selection).as_deref(), Some("e"));
        selection.extend((0, 4), Side::Left);
        assert_eq!(selected(&term, selection).as_deref(), Some("ell"));
        // Backwards from the right half of "w" to the right half of "o": "o" is left out.
        let mut backwards = Selection::new(Kind::Cells, (0, 6), Side::Right);
        backwards.extend((0, 4), Side::Right);
        assert_eq!(selected(&term, backwards).as_deref(), Some(" w"));
    }

    #[test]
    fn a_drag_runs_across_rows_and_joins_soft_wraps() {
        let term = term(b"0123456789abc\r\nnext");
        let mut selection = Selection::new(Kind::Cells, (0, 8), Side::Left);
        selection.extend((2, 1), Side::Right);
        assert_eq!(selected(&term, selection).as_deref(), Some("89abc\nne"));
    }

    #[test]
    fn words_stop_at_spaces_and_separators_and_cross_soft_wraps() {
        let term = term(b"(foo bar)\r\nlongerword1234");
        let word = |row, col| selected(&term, Selection::new(Kind::Words, (row, col), Side::Left));
        assert_eq!(word(0, 2).as_deref(), Some("foo"));
        assert_eq!(word(0, 6).as_deref(), Some("bar"));
        assert_eq!(word(0, 0).as_deref(), Some("("));
        assert_eq!(word(0, 4).as_deref(), Some(""));
        assert_eq!(word(2, 1).as_deref(), Some("longerword1234"));
        let mut two = Selection::new(Kind::Words, (0, 1), Side::Left);
        two.extend((0, 5), Side::Left);
        assert_eq!(selected(&term, two).as_deref(), Some("foo bar"));
    }

    #[test]
    fn a_wide_character_is_one_word_cell() {
        let term = term("ab 日本 cd".as_bytes());
        let word = Selection::new(Kind::Words, (0, 4), Side::Left);
        assert_eq!(selected(&term, word).as_deref(), Some("日本"));
    }

    #[test]
    fn lines_take_whole_logical_lines() {
        let term = term(b"first\r\n0123456789wrapped\r\nlast");
        let line = |row| selected(&term, Selection::new(Kind::Lines, (row, 3), Side::Left));
        assert_eq!(line(0).as_deref(), Some("first"));
        assert_eq!(line(2).as_deref(), Some("0123456789wrapped"));
        assert_eq!(line(1).as_deref(), Some("0123456789wrapped"));
    }

    #[test]
    fn a_selection_stays_on_its_text_while_output_scrolls() {
        let mut term = term(b"keep me\r\n");
        let mut selection = Selection::new(Kind::Cells, (0, 0), Side::Left);
        selection.extend((0, 6), Side::Right);
        term.advance(b"1\r\n2\r\n3\r\n4\r\n5\r\n");
        assert!(term.screen_top() > 0);
        assert_eq!(selected(&term, selection).as_deref(), Some("keep me"));
    }
}
