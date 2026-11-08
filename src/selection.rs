//! Selections by absolute `(row, col)`: rows keep their indices while output scrolls them into
//! the scrollback, so a selection stays on its text. A double click selects words, a triple click
//! lines, and either extends by whole words or lines as the pointer drags; an Option+drag selects
//! a block, the same columns on every row.

use std::mem;

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
    Block,
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

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn set_kind(&mut self, kind: Kind) {
        self.kind = kind;
    }

    /// Extends the selection over the whole cell `at` from its anchor's, as copy mode's cursor
    /// moves it: whichever way it goes, both cells are in.
    pub fn extend_over(&mut self, at: Point) {
        let anchor = self.anchor.0;
        let ahead = match self.kind {
            Kind::Block => at.1 >= anchor.1,
            _ => at >= anchor,
        };
        let sides = if ahead { (Side::Left, Side::Right) } else { (Side::Right, Side::Left) };
        (self.anchor, self.head) = ((anchor, sides.0), (at, sides.1));
    }

    /// Swaps the selection's ends, so the other one moves; where that one is.
    pub fn swap(&mut self) -> Point {
        mem::swap(&mut self.anchor, &mut self.head);
        self.head.0
    }

    /// The first and last selected cells, inclusive, in reading order, or a block's top left and
    /// bottom right cells; `None` when the pointer has not covered a whole cell (a block, a whole
    /// column).
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
            Kind::Block => {
                let mut cols = [(self.anchor.0.1, self.anchor.1), (self.head.0.1, self.head.1)];
                cols.sort();
                let first = match cols[0] {
                    (col, Side::Right) => col + 1,
                    (col, Side::Left) => col,
                };
                let last = match cols[1] {
                    (0, Side::Left) => return None,
                    (col, Side::Left) => col - 1,
                    (col, Side::Right) => col,
                };
                (first <= last).then_some(((start.0.0, first), (end.0.0, last)))
            }
        }
    }

    /// The selected text, as it is copied: a block's columns of each of its rows, trailing blanks
    /// trimmed, joined by newlines. A wide character is in a block when its first cell is, as it
    /// is drawn: whole at the block's right edge, left out at its left.
    pub fn text(&self, term: &Terminal) -> Option<String> {
        let (start, end) = self.range(term)?;
        if self.kind != Kind::Block {
            return Some(term.text(start, end));
        }
        let rows = (start.0..=end.0).map(|row| {
            let text = term.text((row, start.1), (row, end.1));
            text.trim_end_matches(' ').to_owned()
        });
        Some(rows.collect::<Vec<_>>().join("\n"))
    }
}

/// The character a cell shows: a wide character's right half shows its left half's, and a
/// scrollback row's cells past those it holds are blank.
pub fn char_at(term: &Terminal, (row, col): Point) -> char {
    let Some(row) = term.row(row) else { return ' ' };
    let cell = |col: usize| row.cells().get(col).copied().unwrap_or_default();
    let col = usize::from(col);
    let cell = if cell(col).is_spacer() && col > 0 { cell(col - 1) } else { cell(col) };
    cell.ch()
}

/// Whether a character is part of a word: neither whitespace nor a separator.
pub fn in_word(c: char) -> bool {
    !c.is_whitespace() && !SEPARATORS.contains(c)
}

/// The cell next to `at` in reading order (`step` −1 or 1), and whether a hard line end lies
/// between them; across a soft wrap none does.
pub fn neighbour(term: &Terminal, (row, col): Point, step: i8) -> Option<(Point, bool)> {
    let last_col = term.size().1 - 1;
    let ends = |row: u64| term.row(row).map(|row| !row.wrapped());
    match step {
        -1 if col > 0 => Some(((row, col - 1), false)),
        -1 => {
            let above = row.checked_sub(1)?;
            Some(((above, last_col), ends(above)?))
        }
        _ if col < last_col => Some(((row, col + 1), false)),
        _ => term.row(row + 1).is_some().then_some(((row + 1, 0), ends(row)?)),
    }
}

/// The far end of the word at `at` going `step`, within its line; a cell outside any word is
/// its own word.
fn word_edge(term: &Terminal, at: Point, step: i8) -> Point {
    let in_word = |at: Point| in_word(char_at(term, at));
    let mut edge = at;
    if in_word(at) {
        let next =
            |edge| neighbour(term, edge, step).filter(|&(next, hard)| !hard && in_word(next));
        while let Some((next, _)) = next(edge) {
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
        selection.text(term)
    }

    fn block(from: (Point, Side), to: (Point, Side)) -> Selection {
        let mut block = Selection::new(Kind::Block, from.0, from.1);
        block.extend(to.0, to.1);
        block
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
    fn a_block_takes_the_same_columns_of_every_row_whichever_way_it_is_dragged() {
        let term = term(b"abcdef\r\nghijkl\r\nmn");
        let (left, right) = (((0, 1), Side::Left), ((2, 3), Side::Right));
        let down = block(left, right);
        assert_eq!(down.range(&term), Some(((0, 1), (2, 3))));
        assert_eq!(selected(&term, down).as_deref(), Some("bcd\nhij\nn"));
        assert_eq!(selected(&term, block(right, left)).as_deref(), Some("bcd\nhij\nn"));
        // From the top right to the bottom left, halves count as for cells: from the left half of
        // "d" to the right half of "n" leaves out the columns of both.
        let across = block(((0, 3), Side::Left), ((2, 1), Side::Right));
        assert_eq!(selected(&term, across).as_deref(), Some("c\ni\n"));
        // A click covers no whole column.
        assert_eq!(block(left, left).range(&term), None);
        assert_eq!(block(((1, 0), Side::Left), ((2, 0), Side::Left)).range(&term), None);
    }

    #[test]
    fn a_wide_character_is_in_a_block_when_its_first_cell_is() {
        // 日 covers columns 1–2 and 本 3–4: columns 2–3 cut both.
        let term = term("a日本c\r\nxyzwvu".as_bytes());
        let cut = block(((0, 2), Side::Left), ((1, 3), Side::Right));
        assert_eq!(selected(&term, cut).as_deref(), Some("本\nzw"));
    }

    #[test]
    fn a_block_reads_past_a_short_scrollback_row_as_blanks() {
        let mut term = Terminal::new(2, 10, 100);
        term.advance(b"ab\r\nabcdefgh\r\nx\r\ny");
        assert_eq!(term.row(0).unwrap().cells().len(), 2, "a short row in the scrollback");
        let wide = block(((0, 1), Side::Left), ((1, 5), Side::Right));
        assert_eq!(selected(&term, wide).as_deref(), Some("b\nbcdef"));
    }

    #[test]
    fn a_selection_stays_on_its_text_while_output_scrolls() {
        let mut term = term(b"keep me\r\n");
        let mut selection = Selection::new(Kind::Cells, (0, 0), Side::Left);
        selection.extend((0, 6), Side::Right);
        term.advance(b"1\r\n2\r\n3\r\n4\r\n5\r\n");
        assert!(term.screen_top() > 0);
        assert_eq!(selected(&term, selection).as_deref(), Some("keep me"));
        // In the scrollback the row holds "keep me" alone; past it are blanks.
        let word = |col| selected(&term, Selection::new(Kind::Words, (0, col), Side::Left));
        assert_eq!((word(1).as_deref(), word(9).as_deref()), (Some("keep"), Some("")));
    }
}
