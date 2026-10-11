use std::collections::VecDeque;

use crate::cell::{Cell, Color};
use crate::event::MarkKind;
use crate::row::Row;
use crate::term::{Cursor, PRIMARY, Terminal, default_tabs};

/// A grid position: a row of a ring and a column, `col == cols` meaning just
/// past a full row (a pending wrap).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Point {
    row: usize,
    col: usize,
}

impl Terminal {
    /// Resize to `rows`×`cols` (each at least 1).
    ///
    /// The primary screen reflows: soft-wrapped rows rejoin into lines and
    /// rewrap at the new width, scrollback included, the cursor, the saved
    /// cursor and the OSC 133 marks travelling with the characters they sat
    /// on. The screen keeps its first line at the top while the cursor stays
    /// on it; growing pulls rows back from the scrollback, shrinking drops
    /// rows below the cursor first and pushes rows into the scrollback only
    /// to keep the cursor in view. The alternate screen is cut or padded,
    /// not reflowed.
    ///
    /// Absolute row indices never go backwards: when the reflow leaves more
    /// scrollback rows than before, `screen_top` advances by the difference;
    /// when fewer, `oldest_row` advances instead. Retained rows can thus be
    /// renumbered upward, and a reader's `since` cursor may see a row again.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if (rows, cols) == self.size() {
            return;
        }
        self.reflow_primary(rows, cols);
        self.resize_alt(rows, cols);
        self.grid.rows = rows;
        self.grid.cols = cols;
        self.grid.reset_damage();
        self.top = 0;
        self.bottom = rows - 1;
        let old_tabs = std::mem::replace(&mut self.tabs, default_tabs(cols));
        let kept = old_tabs.len().min(usize::from(cols));
        self.tabs[..kept].copy_from_slice(&old_tabs[..kept]);
        for saved in self.saved.iter_mut().flatten() {
            clamp(&mut saved.cursor, rows, cols);
        }
        clamp(&mut self.cursor, rows, cols);
    }

    fn reflow_primary(&mut self, rows: u16, cols: u16) {
        let history = self.grid.history_len();
        let old_top = self.grid.screen_top();
        let on_primary = !self.grid.alt_active;
        // While the alternate screen is up, the primary's cursor is the one
        // its switch back restores.
        let (anchor, tracks_saved) = match (&self.saved[PRIMARY], on_primary) {
            (Some(saved), false) => (saved.cursor.clone(), false),
            _ => (self.cursor.clone(), on_primary),
        };
        let position = |cursor: &Cursor| Point {
            row: history + usize::from(cursor.row),
            col: usize::from(cursor.col) + usize::from(cursor.pending_wrap),
        };
        let mut points = vec![Point { row: history, col: 0 }, position(&anchor)];
        if tracks_saved {
            points.extend(self.saved[PRIMARY].as_ref().map(|saved| position(&saved.cursor)));
        }

        let old = std::mem::take(&mut self.grid.primary);
        let (mut ring, mapped) = if cols == self.grid.cols {
            (old, points)
        } else {
            let (rows, mapped) = rewrap(&old, usize::from(cols), &points);
            (VecDeque::from(rows), mapped)
        };

        let rows = usize::from(rows);
        let mut start = mapped[0].row;
        let cursor_row = mapped[1].row;
        if cursor_row >= start + rows {
            start = cursor_row + 1 - rows;
        }
        if ring.len() < start + rows {
            start -= (start + rows - ring.len()).min(start);
        }
        if ring.len() > start + rows {
            ring.truncate(start + rows);
            // What the bottom row ran on into is gone.
            let bottom = ring.back_mut().expect("the screen is never empty");
            bottom.wrapped = false;
            if let Some(last) = bottom.cells.last_mut() {
                last.clear_pad();
            }
        }
        while ring.len() < start + rows {
            ring.push_back(Row::new(cols, Color::Default));
        }
        let dropped = start.saturating_sub(self.grid.history_limit);
        ring.drain(..dropped);
        let screen_top = old_top + start.saturating_sub(history) as u64;
        self.grid.oldest = screen_top - (start - dropped) as u64;
        self.grid.primary = ring;

        let place = |cursor: &mut Cursor, point: Point| {
            cursor.row = (point.row.saturating_sub(start)).min(rows - 1) as u16;
            cursor.pending_wrap = point.col >= usize::from(cols);
            cursor.col = point.col.min(usize::from(cols) - 1) as u16;
        };
        match (on_primary, &mut self.saved[PRIMARY]) {
            (true, saved) => {
                place(&mut self.cursor, mapped[1]);
                if let (Some(saved), Some(&point)) = (saved, mapped.get(2)) {
                    place(&mut saved.cursor, point);
                }
            }
            (false, Some(saved)) => place(&mut saved.cursor, mapped[1]),
            (false, None) => {}
        }
    }

    fn resize_alt(&mut self, rows: u16, cols: u16) {
        let (old_rows, rows) = (self.grid.alt.len(), usize::from(rows));
        if rows < old_rows {
            // Keep the cursor in view: rows above it go before rows below.
            let cursor = if self.grid.alt_active { usize::from(self.cursor.row) } else { 0 };
            let above = (cursor + 1).saturating_sub(rows);
            self.grid.alt.drain(..above);
            self.grid.alt.truncate(rows);
            if self.grid.alt_active {
                self.cursor.row -= above as u16;
            }
        }
        for row in self.grid.alt.iter_mut().filter(|row| row.cells.len() != usize::from(cols)) {
            if row.cells.len() > usize::from(cols) {
                row.cells.truncate(usize::from(cols));
                if row.cells.last().is_some_and(Cell::is_wide) {
                    row.orphan(usize::from(cols) - 1);
                }
            }
            // Cut or padded, a row no longer runs to its edge.
            if let Some(last) = row.cells.last_mut() {
                last.clear_pad();
            }
            row.wrapped = false;
            row.cells.resize(usize::from(cols), Cell::default());
        }
        while self.grid.alt.len() < rows {
            self.grid.alt.push_back(Row::new(cols, Color::Default));
        }
    }
}

fn clamp(cursor: &mut Cursor, rows: u16, cols: u16) {
    cursor.row = cursor.row.min(rows - 1);
    if cursor.col >= cols {
        cursor.col = cols - 1;
        cursor.pending_wrap = false;
    }
    // A wrap is pending only at the last column.
    cursor.pending_wrap &= cursor.col == cols - 1;
}

/// Rejoin `old`'s soft-wrapped rows into lines and wrap them again at
/// `cols`, carrying `points` and the rows' marks along. A line's trailing
/// blanks are dropped, but not before a point or mark on it.
fn rewrap(old: &VecDeque<Row>, cols: usize, points: &[Point]) -> (Vec<Row>, Vec<Point>) {
    let mut out: Vec<Row> = Vec::with_capacity(old.len());
    let mut mapped = vec![Point::default(); points.len()];
    let mut marks: Vec<(Point, MarkKind)> = Vec::new();
    let mut row = Row::new(0, Color::Default);
    let mut i = 0;
    while i < old.len() {
        let mut last = i;
        while old[last].wrapped && last + 1 < old.len() {
            last += 1;
        }
        let tail = &old[last];
        let len = tail.cells.len();
        let content = |cell: &Cell| !cell.is_default() && !cell.is_pad();
        let used = tail.cells.iter().rposition(content).map_or(0, |col| col + 1);
        let held = points
            .iter()
            .filter(|point| point.row == last)
            .map(|point| point.col)
            .chain(tail.marks.iter().map(|&(col, _)| usize::from(col)))
            .map(|col| (col + 1).min(len))
            .max()
            .unwrap_or(0);
        let line_end = used.max(held);

        for (r, source) in old.range(i..=last).enumerate().map(|(k, source)| (i + k, source)) {
            let end = if r < last { source.cells.len() } else { line_end };
            let mut col = 0;
            while col < end {
                let cell = source.cells[col];
                if cell.is_pad() {
                    // No text: what sat on it lands where the next cell does.
                    let here = Point { row: out.len(), col: row.cells.len() };
                    for (target, point) in mapped.iter_mut().zip(points) {
                        if point.row == r && point.col == col {
                            *target = here;
                        }
                    }
                    marks.extend(
                        source
                            .marks
                            .iter()
                            .filter(|&&(at, _)| usize::from(at) == col)
                            .map(|&(_, kind)| (here, kind)),
                    );
                    col += 1;
                    continue;
                }
                let width =
                    if cell.is_wide() && cols >= 2 && col + 1 < source.cells.len() { 2 } else { 1 };
                if row.cells.len() + width > cols {
                    if width == 2 && row.cells.len() + 1 == cols {
                        // A pad in the wide character's rendition, as
                        // writing it would have left.
                        row.append_from(source, col..col + 1);
                        let pad = row.cells.len() - 1;
                        row.orphan(pad);
                        row.cells[pad].set_pad();
                    }
                    row.wrapped = true;
                    out.push(std::mem::replace(&mut row, Row::new(0, Color::Default)));
                }
                let here = Point { row: out.len(), col: row.cells.len() };
                for (target, point) in mapped.iter_mut().zip(points) {
                    if point.row == r && (col..col + width).contains(&point.col) {
                        *target = Point { col: here.col + point.col - col, ..here };
                    }
                }
                for &(mark_col, kind) in &source.marks {
                    let mark_col = usize::from(mark_col);
                    if (col..col + width).contains(&mark_col) {
                        marks.push((Point { col: here.col + mark_col - col, ..here }, kind));
                    }
                }
                row.append_from(source, col..col + width);
                if width == 1 && (cell.is_wide() || cell.is_spacer()) {
                    // A wide character with no room (one column) becomes a
                    // replacement character, as writing it would; a stray
                    // spacer a blank.
                    let last = row.cells.len() - 1;
                    row.orphan(last);
                    if cell.is_wide() {
                        row.cells[last].ch = '\u{FFFD}';
                    }
                }
                col += width;
            }
            // Points and marks past the cells taken — just past a full row —
            // land where the next cell would.
            let here = Point { row: out.len(), col: row.cells.len() };
            for (target, point) in mapped.iter_mut().zip(points) {
                if point.row == r && point.col >= end {
                    *target = here;
                }
            }
            for &(mark_col, kind) in &source.marks {
                if usize::from(mark_col) >= end {
                    marks.push((here, kind));
                }
            }
        }
        row.cells.resize(cols.max(row.cells.len()), Cell::default());
        out.push(std::mem::replace(&mut row, Row::new(0, Color::Default)));
        i = last + 1;
    }
    for (point, kind) in marks {
        if let Some(row) = out.get_mut(point.row) {
            row.marks.push((point.col.min(cols - 1) as u16, kind));
        }
    }
    (out, mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, cols: u16, wrapped: bool) -> Row {
        let mut row = Row::new(cols, Color::Default);
        for (cell, c) in row.cells.iter_mut().zip(text.chars()) {
            cell.ch = c;
        }
        row.wrapped = wrapped;
        row
    }

    fn texts(rows: &[Row]) -> Vec<(String, bool)> {
        rows.iter().map(|row| (row.text(), row.wrapped)).collect()
    }

    #[test]
    fn wrapped_rows_rejoin_and_rewrap() {
        let old: VecDeque<Row> =
            [line("abcde", 5, true), line("fgh", 5, false), line("ij", 5, false)].into();
        let (rows, _) = rewrap(&old, 3, &[]);
        let want = [("abc", true), ("def", true), ("gh", false), ("ij", false)];
        assert_eq!(texts(&rows), want.map(|(t, w)| (t.to_string(), w)));
        let (rows, _) = rewrap(&old, 8, &[]);
        let want = [("abcdefgh", false), ("ij", false)];
        assert_eq!(texts(&rows), want.map(|(t, w)| (t.to_string(), w)));
    }

    #[test]
    fn points_follow_their_characters() {
        let old: VecDeque<Row> = [line("abcde", 5, true), line("fgh", 5, false)].into();
        let points = [Point { row: 1, col: 1 }, Point { row: 1, col: 3 }, Point { row: 0, col: 4 }];
        let (_, mapped) = rewrap(&old, 3, &points);
        assert_eq!(mapped[0], Point { row: 2, col: 0 }, "on g");
        assert_eq!(mapped[1], Point { row: 2, col: 2 }, "after h");
        assert_eq!(mapped[2], Point { row: 1, col: 1 }, "on e");
    }

    #[test]
    fn a_pending_wrap_maps_past_a_full_row() {
        let old: VecDeque<Row> = [line("abcd", 4, false)].into();
        let (_, mapped) = rewrap(&old, 2, &[Point { row: 0, col: 4 }]);
        assert_eq!(mapped[0], Point { row: 1, col: 2 });
    }
}
