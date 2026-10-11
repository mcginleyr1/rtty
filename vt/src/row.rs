use std::ops::Range;
use std::sync::Arc;

use crate::cell::{Cell, Color};
use crate::event::MarkKind;

/// An OSC 8 hyperlink.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Hyperlink {
    /// The `id=` parameter, which ties cells of one link together across
    /// rows.
    pub id: Option<String>,
    pub uri: String,
}

/// What a cell carries beyond its sixteen bytes, held by its row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Extra {
    /// The whole grapheme cluster, when it is more than its first character.
    pub(crate) cluster: Option<Box<str>>,
    pub(crate) underline: Color,
    pub(crate) link: Option<Arc<Hyperlink>>,
}

impl Extra {
    fn same(&self, other: &Extra) -> bool {
        self.cluster == other.cluster
            && self.underline == other.underline
            && match (&self.link, &other.link) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
                (a, b) => a.is_none() && b.is_none(),
            }
    }
}

/// One row of the grid: its cells, whether it soft-wraps into the next, and
/// the shell-integration marks that fell on it.
#[derive(Clone, Debug)]
pub struct Row {
    pub(crate) cells: Vec<Cell>,
    /// Every cell from here on is `Cell::default()`: a reset clears no
    /// further. Writes beyond it `touch` it.
    pub(crate) occupied: usize,
    /// The side entries cells point into (`Cell::extra`); entries no cell
    /// points at any more are dropped when the vector outgrows the row.
    extras: Vec<Extra>,
    pub(crate) marks: Vec<(u16, MarkKind)>,
    pub(crate) wrapped: bool,
}

impl Row {
    pub(crate) fn new(cols: u16, bg: Color) -> Row {
        let cols = usize::from(cols);
        Row {
            cells: vec![Cell::blank(bg); cols],
            occupied: if bg == Color::Default { 0 } else { cols },
            extras: Vec::new(),
            marks: Vec::new(),
            wrapped: false,
        }
    }

    /// Blank the row at `cols` cells. A row recycled from the far end of the
    /// scrollback is cold in the cache: only what was written is cleared.
    pub(crate) fn reset(&mut self, cols: u16, bg: Color) {
        let cols = usize::from(cols);
        if bg == Color::Default {
            let occupied = self.occupied.min(self.cells.len());
            self.cells[..occupied].fill(Cell::default());
            self.cells.resize(cols, Cell::default());
            self.occupied = 0;
        } else {
            self.cells.clear();
            self.cells.resize(cols, Cell::blank(bg));
            self.occupied = cols;
        }
        self.extras.clear();
        self.marks.clear();
        self.wrapped = false;
    }

    /// Cells up to `end` may now differ from a blank.
    pub(crate) fn touch(&mut self, end: usize) {
        self.occupied = self.occupied.max(end);
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// The row continues on the next one: it filled up and the cursor wrapped.
    pub fn wrapped(&self) -> bool {
        self.wrapped
    }

    /// The whole grapheme cluster at `col` when it is more than the cell's
    /// first character (a combining mark, a ZWJ emoji sequence, a flag).
    pub fn cluster(&self, col: usize) -> Option<&str> {
        self.extra(&self.cells[col])?.cluster.as_deref()
    }

    pub fn underline_color(&self, col: usize) -> Color {
        self.extra(&self.cells[col]).map_or(Color::Default, |extra| extra.underline)
    }

    pub fn hyperlink(&self, col: usize) -> Option<&Arc<Hyperlink>> {
        self.extra(&self.cells[col])?.link.as_ref()
    }

    /// Append the text of the cell at `col`: its cluster, or nothing for a
    /// spacer.
    pub fn push_grapheme(&self, col: usize, out: &mut String) {
        let cell = &self.cells[col];
        match self.cluster(col) {
            _ if cell.is_spacer() => {}
            Some(cluster) => out.push_str(cluster),
            None => out.push(cell.ch),
        }
    }

    /// The row's text, trailing spaces trimmed.
    pub fn text(&self) -> String {
        let mut text = String::with_capacity(self.cells.len());
        self.push_text(0..self.cells.len(), &mut text);
        text.truncate(text.trim_end_matches(' ').len());
        text
    }

    /// Append the text of cells `cols`: a pad is no text.
    pub(crate) fn push_text(&self, cols: Range<usize>, out: &mut String) {
        for col in cols.start..cols.end.min(self.cells.len()) {
            if !self.cells[col].is_pad() {
                self.push_grapheme(col, out);
            }
        }
    }

    pub(crate) fn extra(&self, cell: &Cell) -> Option<&Extra> {
        cell.extra.checked_sub(1).map(|index| &self.extras[usize::from(index)])
    }

    /// The `Cell::extra` index for `extra`, reusing the newest entry when it
    /// is the same — a run written in one pen shares one.
    pub(crate) fn intern(&mut self, extra: Extra) -> u16 {
        if extra == Extra::default() {
            return 0;
        }
        // Searching a crowded row costs more than a duplicate entry.
        let len = self.extras.len();
        if let Some(back) = self.extras.iter().rev().take(8).position(|old| old.same(&extra)) {
            return (len - back) as u16;
        }
        if len >= (self.cells.len() * 2 + 16).min(usize::from(u16::MAX)) {
            self.compact();
        }
        if self.extras.len() >= usize::from(u16::MAX) {
            return 0;
        }
        self.extras.push(extra);
        self.extras.len() as u16
    }

    /// Drop the side entries no cell points at, renumbering the rest.
    fn compact(&mut self) {
        let mut remap = vec![0u16; self.extras.len()];
        let mut kept = Vec::new();
        for cell in &mut self.cells {
            let Some(old) = cell.extra.checked_sub(1).map(usize::from) else {
                continue;
            };
            if remap[old] == 0 {
                kept.push(std::mem::take(&mut self.extras[old]));
                remap[old] = kept.len() as u16;
            }
            cell.extra = remap[old];
        }
        self.extras = kept;
    }

    /// The side entry index for a cell written with `underline` and `link`.
    pub(crate) fn pen_extra(&mut self, underline: Color, link: &Option<Arc<Hyperlink>>) -> u16 {
        if underline == Color::Default && link.is_none() {
            return 0;
        }
        self.intern(Extra { cluster: None, underline, link: link.clone() })
    }

    /// Append `c` to the cluster of the cell at `col`, keeping its underline
    /// colour and link. A cluster stops growing at 32 characters.
    pub(crate) fn extend_cluster(&mut self, col: usize, c: char) {
        let cell = self.cells[col];
        let mut extra = self.extra(&cell).cloned().unwrap_or_default();
        let mut base = [0; 4];
        let old = extra.cluster.as_deref().unwrap_or_else(|| cell.ch.encode_utf8(&mut base));
        if old.chars().nth(31).is_some() {
            return;
        }
        let mut cluster = String::with_capacity(old.len() + c.len_utf8());
        cluster.push_str(old);
        cluster.push(c);
        extra.cluster = Some(cluster.into_boxed_str());
        self.cells[col].extra = self.intern(extra);
        self.touch(col + 1);
    }

    /// Turn the cell at `col` into a one-column space in the same rendition,
    /// for the half of a wide character that survives the other.
    pub(crate) fn orphan(&mut self, col: usize) {
        let cell = self.cells[col];
        if let Some(extra) = self.extra(&cell).filter(|extra| extra.cluster.is_some()) {
            let kept = Extra { cluster: None, ..extra.clone() };
            self.cells[col].extra = self.intern(kept);
        }
        self.cells[col].orphan();
    }

    /// Before cells `range` are overwritten or erased: the wide characters
    /// they cut in half lose their other halves too.
    pub(crate) fn split_wide(&mut self, range: Range<usize>) {
        let len = self.cells.len();
        if range.start < len && range.start > 0 && self.cells[range.start].is_spacer() {
            self.orphan(range.start - 1);
        }
        if range.end > 0 && range.end < len && self.cells[range.end - 1].is_wide() {
            self.orphan(range.end);
        }
    }

    /// Blank cells `range` in `bg`. Erasing the last cell ends a soft wrap:
    /// the row no longer runs to its edge.
    pub(crate) fn erase(&mut self, range: Range<usize>, bg: Color) {
        let range = range.start.min(self.cells.len())..range.end.min(self.cells.len());
        self.split_wide(range.clone());
        if range.end == self.cells.len() && !range.is_empty() {
            self.wrapped = false;
        }
        if bg != Color::Default {
            self.touch(range.end);
        }
        self.cells[range].fill(Cell::blank(bg));
    }

    /// Nothing on the row a reader would see, and it does not continue.
    pub(crate) fn is_empty(&self) -> bool {
        !self.wrapped
            && self.cells.iter().all(|cell| {
                cell.is_empty() && self.extra(cell).is_none_or(|extra| extra.cluster.is_none())
            })
    }

    /// Copy `cells` from `from` (another row) to the end of this one, side
    /// entries included.
    pub(crate) fn append_from(&mut self, from: &Row, cells: Range<usize>) {
        for cell in &from.cells[cells] {
            let mut cell = *cell;
            cell.extra = from.extra(&cell).cloned().map_or(0, |extra| self.intern(extra));
            self.cells.push(cell);
        }
        self.touch(self.cells.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pen_run_shares_one_side_entry() {
        let link = Some(Arc::new(Hyperlink { id: None, uri: "https://example.com".into() }));
        let mut row = Row::new(80, Color::Default);
        let first = row.pen_extra(Color::Indexed(1), &link);
        for _ in 0..1000 {
            assert_eq!(row.pen_extra(Color::Indexed(1), &link), first);
        }
        assert_eq!(row.extras.len(), 1);
    }

    #[test]
    fn side_entries_stay_bounded_under_rewrites() {
        let mut row = Row::new(10, Color::Default);
        for i in 0..10_000u32 {
            let col = (i % 10) as usize;
            row.cells[col].extra = row.pen_extra(Color::Indexed((i % 251) as u8), &None);
        }
        assert!(row.extras.len() <= 36, "{} entries", row.extras.len());
        for (i, cell) in row.cells.iter().enumerate() {
            let want = Color::Indexed(((9_990 + i) % 251) as u8);
            assert_eq!(row.underline_color(i), want, "cell {i} after {:?}", cell);
        }
    }
}
