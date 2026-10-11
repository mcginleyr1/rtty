use std::collections::VecDeque;

use crate::cell::Color;
use crate::row::Row;

/// The primary screen with its scrollback, and the alternate screen.
///
/// The primary keeps one ring of rows, scrollback first and the visible
/// screen as its last `rows` entries; the absolute index of `primary[i]` is
/// `oldest + i`. The alternate screen has no scrollback; its rows are
/// numbered on from the primary's screen top.
pub(crate) struct Grid {
    pub(crate) primary: VecDeque<Row>,
    pub(crate) alt: VecDeque<Row>,
    pub(crate) oldest: u64,
    pub(crate) history_limit: usize,
    pub(crate) rows: u16,
    pub(crate) cols: u16,
    pub(crate) alt_active: bool,
    /// Visible rows changed since the last `take_damage`.
    damage: Vec<bool>,
}

impl Grid {
    pub(crate) fn new(rows: u16, cols: u16, history_limit: usize) -> Grid {
        let screen = || (0..rows).map(|_| Row::new(cols, Color::Default)).collect();
        Grid {
            primary: screen(),
            alt: screen(),
            oldest: 0,
            history_limit,
            rows,
            cols,
            alt_active: false,
            damage: vec![true; usize::from(rows)],
        }
    }

    pub(crate) fn history_len(&self) -> usize {
        self.primary.len() - usize::from(self.rows)
    }

    pub(crate) fn screen_top(&self) -> u64 {
        self.oldest + self.history_len() as u64
    }

    /// The active screen's rows and where its row 0 sits in them.
    fn active(&mut self) -> (&mut VecDeque<Row>, usize) {
        if self.alt_active {
            (&mut self.alt, 0)
        } else {
            let top = self.history_len();
            (&mut self.primary, top)
        }
    }

    pub(crate) fn line(&self, row: u16) -> &Row {
        if self.alt_active { &self.alt[usize::from(row)] } else { self.primary_line(row) }
    }

    pub(crate) fn primary_line(&self, row: u16) -> &Row {
        &self.primary[self.history_len() + usize::from(row)]
    }

    pub(crate) fn line_mut(&mut self, row: u16) -> &mut Row {
        self.damage[usize::from(row)] = true;
        let (rows, top) = self.active();
        &mut rows[top + usize::from(row)]
    }

    /// The row above visible row `row`, from the scrollback for the first.
    pub(crate) fn above_mut(&mut self, row: u16) -> Option<&mut Row> {
        if row > 0 {
            return Some(self.line_mut(row - 1));
        }
        let history = self.history_len();
        (!self.alt_active && history > 0).then(|| &mut self.primary[history - 1])
    }

    /// The row with absolute index `index`: scrollback, then the visible
    /// screen.
    pub(crate) fn row(&self, index: u64) -> Option<&Row> {
        let top = self.screen_top();
        if self.alt_active && index >= top {
            return self.alt.get(usize::try_from(index - top).ok()?);
        }
        self.primary.get(usize::try_from(index.checked_sub(self.oldest)?).ok()?)
    }

    /// Scroll rows `top..=bottom` up by `n`, blank rows of `bg` coming in at
    /// the bottom. On the primary screen a region starting at its top row
    /// pushes what leaves it into the scrollback.
    pub(crate) fn scroll_up(&mut self, top: u16, bottom: u16, n: u16, bg: Color) {
        let n = n.min(bottom - top + 1);
        self.damage_rows(top, bottom);
        if self.alt_active || top > 0 {
            let cols = self.cols;
            let (rows, base) = self.active();
            let (top, bottom) = (base + usize::from(top), base + usize::from(bottom));
            rotate_left(rows, top, bottom + 1, usize::from(n));
            for row in rows.range_mut(bottom + 1 - usize::from(n)..=bottom) {
                row.reset(cols, bg);
            }
            return;
        }
        for _ in 0..n {
            let row = if self.primary.len() > self.history_limit + usize::from(self.rows) - 1 {
                self.oldest += 1;
                let mut row = self.primary.pop_front().expect("the screen is never empty");
                row.reset(self.cols, bg);
                row
            } else {
                Row::new(self.cols, bg)
            };
            let at = self.primary.len() + 1 + usize::from(bottom) - usize::from(self.rows);
            self.primary.insert(at, row);
        }
    }

    /// Scroll rows `top..=bottom` down by `n`, blank rows of `bg` coming in
    /// at the top.
    pub(crate) fn scroll_down(&mut self, top: u16, bottom: u16, n: u16, bg: Color) {
        let n = usize::from(n.min(bottom - top + 1));
        self.damage_rows(top, bottom);
        let cols = self.cols;
        let (rows, base) = self.active();
        let (top, bottom) = (base + usize::from(top), base + usize::from(bottom));
        rotate_left(rows, top, bottom + 1, bottom + 1 - top - n);
        for row in rows.range_mut(top..top + n) {
            row.reset(cols, bg);
        }
    }

    /// Drop the primary screen's scrollback; its rows' indices are not
    /// reused.
    pub(crate) fn clear_history(&mut self) {
        let history = self.history_len();
        self.primary.drain(..history);
        self.oldest += history as u64;
    }

    pub(crate) fn damage_rows(&mut self, top: u16, bottom: u16) {
        self.damage[usize::from(top)..=usize::from(bottom)].fill(true);
    }

    pub(crate) fn damage_all(&mut self) {
        self.damage.fill(true);
    }

    pub(crate) fn take_damage(&mut self) -> Vec<u16> {
        let damaged = (0..self.rows).filter(|&row| self.damage[usize::from(row)]).collect();
        self.damage.fill(false);
        damaged
    }

    /// After a resize: every row of the new size is damaged.
    pub(crate) fn reset_damage(&mut self) {
        self.damage = vec![true; usize::from(self.rows)];
    }
}

/// Rotate `rows[start..end]` left by `n` with three reversals of swaps:
/// `VecDeque` swaps are O(1) where a ring that has wrapped has no slice.
fn rotate_left(rows: &mut VecDeque<Row>, start: usize, end: usize, n: usize) {
    let reverse = |rows: &mut VecDeque<Row>, mut a: usize, mut b: usize| {
        while a + 1 < b {
            rows.swap(a, b - 1);
            a += 1;
            b -= 1;
        }
    };
    if n == 0 || n >= end - start {
        return;
    }
    reverse(rows, start, start + n);
    reverse(rows, start + n, end);
    reverse(rows, start, end);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labelled(grid: &mut Grid) {
        for r in 0..grid.rows {
            grid.line_mut(r).cells[0].ch = char::from(b'a' + r as u8);
        }
    }

    fn screen(grid: &Grid) -> String {
        (0..grid.rows).map(|r| grid.line(r).cells[0].ch).collect()
    }

    #[test]
    fn a_full_scroll_pushes_into_a_bounded_history() {
        let mut grid = Grid::new(3, 4, 2);
        labelled(&mut grid);
        grid.scroll_up(0, 2, 1, Color::Default);
        assert_eq!(screen(&grid), "bc ");
        assert_eq!((grid.oldest, grid.screen_top()), (0, 1));
        grid.scroll_up(0, 2, 3, Color::Default);
        assert_eq!(screen(&grid), "   ");
        assert_eq!((grid.oldest, grid.screen_top(), grid.history_len()), (2, 4, 2));
        assert_eq!(grid.row(2).unwrap().cells[0].ch, 'c');
        assert!(grid.row(1).is_none());
    }

    #[test]
    fn a_region_at_the_top_pushes_and_keeps_the_rows_below() {
        let mut grid = Grid::new(4, 4, 10);
        labelled(&mut grid);
        grid.scroll_up(0, 1, 1, Color::Default);
        assert_eq!(screen(&grid), "b cd");
        assert_eq!(grid.row(0).unwrap().cells[0].ch, 'a');
    }

    #[test]
    fn an_inner_region_rotates_without_history() {
        let mut grid = Grid::new(5, 4, 10);
        labelled(&mut grid);
        grid.scroll_up(1, 3, 2, Color::Default);
        assert_eq!(screen(&grid), "ad  e");
        grid.scroll_down(1, 3, 1, Color::Default);
        assert_eq!(screen(&grid), "a d e");
        assert_eq!(grid.history_len(), 0);
    }

    #[test]
    fn rotation_spans_a_wrapped_ring() {
        let mut rows: VecDeque<Row> = (0..6).map(|_| Row::new(1, Color::Default)).collect();
        rows.rotate_left(4);
        for (i, row) in rows.iter_mut().enumerate() {
            row.cells[0].ch = char::from(b'0' + i as u8);
        }
        rotate_left(&mut rows, 1, 6, 2);
        let got: String = rows.iter().map(|row| row.cells[0].ch).collect();
        assert_eq!(got, "034512");
    }
}
