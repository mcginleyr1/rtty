//! A window's tabs, each a tree of splits over its panes, in cells: where each pane goes, where
//! the dividers are, which pane has the focus, and every way those change. Nothing here knows
//! GPUI, so all of it is unit tested.

use std::collections::HashMap;

pub type PaneId = u64;

/// A block of cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
}

impl Rect {
    pub fn contains(&self, col: u16, row: u16) -> bool {
        (self.col..self.col + self.cols).contains(&col)
            && (self.row..self.row + self.rows).contains(&row)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// How a split shares its space: side by side, with a vertical divider, or one above the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    SideBySide,
    Stacked,
}

impl Direction {
    fn orientation(self) -> Orientation {
        match self {
            Direction::Left | Direction::Right => Orientation::SideBySide,
            Direction::Up | Direction::Down => Orientation::Stacked,
        }
    }

    /// Toward the end of the axis: right or down.
    fn forward(self) -> bool {
        matches!(self, Direction::Right | Direction::Down)
    }
}

/// A divider is one cell thick; no pane gets fewer cells than this across a split.
const MIN: u16 = 2;

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Pane(PaneId),
    Split {
        orientation: Orientation,
        /// The first child's share of the cells the divider leaves.
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// A split's divider, addressed by the path of choices (`true`: second child) from the root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divider {
    pub path: Vec<bool>,
    pub orientation: Orientation,
    pub rect: Rect,
}

/// One tab: its split tree, the pane with the focus, and whether that pane fills the tab.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    root: Node,
    focus: PaneId,
    zoomed: bool,
    /// When each pane last had the focus, to choose between neighbours.
    focused_at: HashMap<PaneId, u64>,
    clock: u64,
}

impl Layout {
    pub fn new(pane: PaneId) -> Layout {
        Layout {
            root: Node::Pane(pane),
            focus: pane,
            zoomed: false,
            focused_at: HashMap::from([(pane, 0)]),
            clock: 0,
        }
    }

    pub fn focus(&self) -> PaneId {
        self.focus
    }

    pub fn zoomed(&self) -> bool {
        self.zoomed
    }

    pub fn set_focus(&mut self, pane: PaneId) {
        if self.panes().contains(&pane) {
            self.clock += 1;
            self.focused_at.insert(pane, self.clock);
            self.focus = pane;
        }
    }

    /// Every pane, left to right and top to bottom as the tree has them.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut panes = Vec::new();
        self.root.collect(&mut panes);
        panes
    }

    /// Puts `new` beside `pane` in `direction`, the two sharing `pane`'s cells, and focuses it.
    pub fn split(&mut self, pane: PaneId, direction: Direction, new: PaneId) {
        let Some(node) = self.root.find(pane) else { return };
        let old = std::mem::replace(node, Node::Pane(pane));
        let (first, second) =
            if direction.forward() { (old, Node::Pane(new)) } else { (Node::Pane(new), old) };
        *node = Node::Split {
            orientation: direction.orientation(),
            ratio: 0.5,
            first: Box::new(first),
            second: Box::new(second),
        };
        self.zoomed = false;
        self.set_focus(new);
    }

    /// Takes `pane` out; its sibling gets its cells. A closed focus passes to the sibling's pane
    /// nearest where it was. `true` when the tab has no panes left.
    pub fn close(&mut self, pane: PaneId) -> bool {
        if self.root == Node::Pane(pane) {
            return true;
        }
        let Some(sibling) = self.root.remove(pane) else { return false };
        self.focused_at.remove(&pane);
        if self.focus == pane {
            self.zoomed = false;
            self.set_focus(sibling);
        }
        false
    }

    /// Where each pane goes in `area`; zoomed, the focused pane takes all of it.
    pub fn rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        if self.zoomed {
            return vec![(self.focus, area)];
        }
        let mut rects = Vec::new();
        self.root.layout(area, &mut Vec::new(), &mut |node, rect, _| {
            if let Node::Pane(pane) = node {
                rects.push((*pane, rect));
            }
        });
        rects
    }

    /// Every pane's grid, shown or not: its cells in the split layout, or all of `area` for the
    /// zoomed pane, so a pane hidden behind zoom keeps the size it will show at again.
    pub fn grids(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut grids = Layout { zoomed: false, ..self.clone() }.rects(area);
        if self.zoomed {
            grids
                .iter_mut()
                .filter(|(pane, _)| *pane == self.focus)
                .for_each(|(_, rect)| *rect = area);
        }
        grids
    }

    /// The dividers in `area`, none while zoomed.
    pub fn dividers(&self, area: Rect) -> Vec<Divider> {
        let mut dividers = Vec::new();
        if !self.zoomed {
            self.root.layout(area, &mut Vec::new(), &mut |node, rect, path| {
                if let Node::Split { orientation, .. } = node {
                    let (first, _) = node.split_rects(rect);
                    let rect = match orientation {
                        Orientation::SideBySide => {
                            Rect { col: first.col + first.cols, cols: 1, ..rect }
                        }
                        Orientation::Stacked => {
                            Rect { row: first.row + first.rows, rows: 1, ..rect }
                        }
                    };
                    let path = path.to_vec();
                    dividers.push(Divider { path, orientation: *orientation, rect });
                }
            });
        }
        dividers
    }

    /// Moves the divider at `path` to the cell `at` (`(col, row)`), as a drag does.
    pub fn drag(&mut self, path: &[bool], area: Rect, at: (u16, u16)) {
        let Some((extent, orientation)) = self.split_at(path, area) else { return };
        let (start, len, at) = match orientation {
            Orientation::SideBySide => (extent.col, extent.cols, at.0),
            Orientation::Stacked => (extent.row, extent.rows, at.1),
        };
        let ratio = ratio_for(at.saturating_sub(start), len);
        if let Some(Node::Split { ratio: old, .. }) = self.root.at_path(path) {
            *old = ratio;
        }
    }

    /// Moves the divider nearest `pane` that runs across `direction` by `cells` that way, as
    /// tmux's `resize-pane` does: the pane grows or shrinks by what its neighbour gives up.
    pub fn resize(&mut self, pane: PaneId, direction: Direction, cells: u16, area: Rect) {
        let Some(path) = self.root.ancestor(pane, direction.orientation()) else { return };
        let Some((extent, _)) = self.split_at(&path, area) else { return };
        let divider = self.dividers(area).into_iter().find(|divider| divider.path == path);
        let Some(divider) = divider else { return };
        let (start, len, now) = match direction.orientation() {
            Orientation::SideBySide => (extent.col, extent.cols, divider.rect.col),
            Orientation::Stacked => (extent.row, extent.rows, divider.rect.row),
        };
        let offset = now - start;
        let moved = if direction.forward() {
            offset.saturating_add(cells)
        } else {
            offset.saturating_sub(cells)
        };
        if let Some(Node::Split { ratio, .. }) = self.root.at_path(&path) {
            *ratio = ratio_for(moved, len);
        }
    }

    /// The pane nearest `from` in `direction`: of the panes beyond its edge that it overlaps
    /// across, the closest; then the one last focused; then the one overlapping it most.
    /// Ignores zoom: it answers for the whole layout.
    pub fn neighbor(&self, from: PaneId, direction: Direction, area: Rect) -> Option<PaneId> {
        let rects = Layout { zoomed: false, ..self.clone() }.rects(area);
        let (_, here) = *rects.iter().find(|(pane, _)| *pane == from)?;
        let span = |r: Rect| match direction.orientation() {
            Orientation::SideBySide => (r.row, r.row + r.rows),
            Orientation::Stacked => (r.col, r.col + r.cols),
        };
        let gap = |r: Rect| -> Option<u16> {
            let (here_end, there_end) = (here.col + here.cols, r.col + r.cols);
            let (here_bottom, there_bottom) = (here.row + here.rows, r.row + r.rows);
            match direction {
                Direction::Left => (there_end <= here.col).then(|| here.col - there_end),
                Direction::Right => (r.col >= here_end).then(|| r.col - here_end),
                Direction::Up => (there_bottom <= here.row).then(|| here.row - there_bottom),
                Direction::Down => (r.row >= here_bottom).then(|| r.row - here_bottom),
            }
        };
        let (here_start, here_end) = span(here);
        rects
            .iter()
            .enumerate()
            .filter(|(_, (pane, _))| *pane != from)
            .filter_map(|(order, &(pane, rect))| {
                let (start, end) = span(rect);
                let overlap = end.min(here_end).saturating_sub(start.max(here_start));
                let recent = self.focused_at.get(&pane).copied().unwrap_or(0);
                (overlap > 0).then_some(())?;
                Some(((gap(rect)?, u64::MAX - recent, u16::MAX - overlap, order), pane))
            })
            .min()
            .map(|(_, pane)| pane)
    }

    /// Moves the focus to the neighbour in `direction`, leaving zoom; `false` when there is none.
    pub fn focus_toward(&mut self, direction: Direction, area: Rect) -> bool {
        let Some(pane) = self.neighbor(self.focus, direction, area) else { return false };
        self.zoomed = false;
        self.set_focus(pane);
        true
    }

    pub fn toggle_zoom(&mut self) {
        self.zoomed = !self.zoomed && self.panes().len() > 1;
    }

    /// The cells the split at `path` covers, and its orientation.
    fn split_at(&self, path: &[bool], area: Rect) -> Option<(Rect, Orientation)> {
        let mut found = None;
        self.root.layout(area, &mut Vec::new(), &mut |node, rect, at| {
            if let Node::Split { orientation, .. } = node
                && at == path
            {
                found = Some((rect, *orientation));
            }
        });
        found
    }
}

/// The ratio that puts a split's divider `offset` cells into its `len`, each side keeping `MIN`.
fn ratio_for(offset: u16, len: u16) -> f32 {
    let available = len.saturating_sub(1);
    let first =
        offset.clamp(MIN.min(available), available.saturating_sub(MIN).max(MIN.min(available)));
    if available == 0 { 0.5 } else { f32::from(first) / f32::from(available) }
}

impl Node {
    fn collect(&self, panes: &mut Vec<PaneId>) {
        match self {
            Node::Pane(pane) => panes.push(*pane),
            Node::Split { first, second, .. } => {
                first.collect(panes);
                second.collect(panes);
            }
        }
    }

    fn find(&mut self, pane: PaneId) -> Option<&mut Node> {
        match self {
            Node::Pane(id) if *id == pane => Some(self),
            Node::Pane(_) => None,
            Node::Split { first, second, .. } => {
                if first.contains(pane) {
                    first.find(pane)
                } else {
                    second.find(pane)
                }
            }
        }
    }

    fn contains(&self, pane: PaneId) -> bool {
        match self {
            Node::Pane(id) => *id == pane,
            Node::Split { first, second, .. } => first.contains(pane) || second.contains(pane),
        }
    }

    fn first_pane(&self, last: bool) -> PaneId {
        match self {
            Node::Pane(pane) => *pane,
            Node::Split { first, second, .. } => {
                if last {
                    second.first_pane(last)
                } else {
                    first.first_pane(last)
                }
            }
        }
    }

    /// Replaces the split holding `pane` with the pane's sibling; the sibling's pane nearest the
    /// removed one.
    fn remove(&mut self, pane: PaneId) -> Option<PaneId> {
        let Node::Split { first, second, .. } = self else { return None };
        let (removed_first, sibling) = if **first == Node::Pane(pane) {
            (true, std::mem::replace(second.as_mut(), Node::Pane(pane)))
        } else if **second == Node::Pane(pane) {
            (false, std::mem::replace(first.as_mut(), Node::Pane(pane)))
        } else if first.contains(pane) {
            return first.remove(pane);
        } else {
            return second.remove(pane);
        };
        let nearest = sibling.first_pane(!removed_first);
        *self = sibling;
        Some(nearest)
    }

    fn at_path(&mut self, path: &[bool]) -> Option<&mut Node> {
        let Some((&second_child, rest)) = path.split_first() else { return Some(self) };
        match self {
            Node::Split { first, second, .. } => {
                if second_child {
                    second.at_path(rest)
                } else {
                    first.at_path(rest)
                }
            }
            Node::Pane(_) => None,
        }
    }

    /// The path to the deepest split above `pane` with `orientation`.
    fn ancestor(&self, pane: PaneId, orientation: Orientation) -> Option<Vec<bool>> {
        let Node::Split { orientation: here, first, second, .. } = self else { return None };
        let (child, side) = if first.contains(pane) {
            (first, false)
        } else if second.contains(pane) {
            (second, true)
        } else {
            return None;
        };
        match child.ancestor(pane, orientation) {
            Some(mut below) => {
                below.insert(0, side);
                Some(below)
            }
            None => (*here == orientation).then(Vec::new),
        }
    }

    /// The cells of a split's two children within `rect`.
    fn split_rects(&self, rect: Rect) -> (Rect, Rect) {
        let Node::Split { orientation, ratio, .. } = self else { return (rect, rect) };
        let len = match orientation {
            Orientation::SideBySide => rect.cols,
            Orientation::Stacked => rect.rows,
        };
        let available = len.saturating_sub(1);
        let first = if available >= 2 * MIN {
            ((f32::from(available) * ratio).round() as u16).clamp(MIN, available - MIN)
        } else {
            available.div_ceil(2)
        };
        let second = available - first;
        match orientation {
            Orientation::SideBySide => (
                Rect { cols: first, ..rect },
                Rect { col: rect.col + first + 1, cols: second, ..rect },
            ),
            Orientation::Stacked => (
                Rect { rows: first, ..rect },
                Rect { row: rect.row + first + 1, rows: second, ..rect },
            ),
        }
    }

    /// Visits every node with its cells and path.
    fn layout(
        &self,
        rect: Rect,
        path: &mut Vec<bool>,
        visit: &mut impl FnMut(&Node, Rect, &[bool]),
    ) {
        visit(self, rect, path);
        if let Node::Split { first, second, .. } = self {
            let (first_rect, second_rect) = self.split_rects(rect);
            path.push(false);
            first.layout(first_rect, path, visit);
            path.pop();
            path.push(true);
            second.layout(second_rect, path, visit);
            path.pop();
        }
    }
}

/// A window's tabs and which one shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Tabs {
    tabs: Vec<Layout>,
    active: usize,
}

/// What closing a pane closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    Pane,
    /// Its tab, which had no other pane.
    Tab,
    /// The window's last tab.
    Window,
}

impl Tabs {
    pub fn new(pane: PaneId) -> Tabs {
        Tabs { tabs: vec![Layout::new(pane)], active: 0 }
    }

    pub fn active(&self) -> &Layout {
        &self.tabs[self.active]
    }

    pub fn active_mut(&mut self) -> &mut Layout {
        &mut self.tabs[self.active]
    }

    pub fn index(&self) -> usize {
        self.active
    }

    pub fn all(&self) -> &[Layout] {
        &self.tabs
    }

    /// A tab holding `pane`, after the current one, shown.
    pub fn open(&mut self, pane: PaneId) {
        self.active += 1;
        self.tabs.insert(self.active, Layout::new(pane));
    }

    /// Shows tab `index` if there is one.
    pub fn select(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
        }
    }

    /// The next tab (`step` 1) or the previous (−1), round the ends.
    pub fn cycle(&mut self, step: isize) {
        let len = self.tabs.len() as isize;
        self.active = (self.active as isize + step).rem_euclid(len) as usize;
    }

    /// Closes `pane` wherever it is. A tab left empty goes; the tab after it, else before it,
    /// shows if it was showing.
    pub fn close(&mut self, pane: PaneId) -> Option<Closed> {
        let index = self.tabs.iter().position(|tab| tab.panes().contains(&pane))?;
        if !self.tabs[index].close(pane) {
            return Some(Closed::Pane);
        }
        if self.tabs.len() == 1 {
            return Some(Closed::Window);
        }
        self.tabs.remove(index);
        if self.active > index || self.active == self.tabs.len() {
            self.active -= 1;
        }
        Some(Closed::Tab)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { col: 0, row: 0, cols: 81, rows: 25 };

    fn rect(col: u16, row: u16, cols: u16, rows: u16) -> Rect {
        Rect { col, row, cols, rows }
    }

    /// 1 | (2 / 3): 2 above 3 on the right.
    fn three() -> Layout {
        let mut layout = Layout::new(1);
        layout.split(1, Direction::Right, 2);
        layout.split(2, Direction::Down, 3);
        layout
    }

    #[test]
    fn splits_share_the_cells_a_divider_leaves() {
        let layout = three();
        assert_eq!(layout.panes(), [1, 2, 3]);
        assert_eq!(layout.focus(), 3);
        assert_eq!(
            layout.rects(AREA),
            [(1, rect(0, 0, 40, 25)), (2, rect(41, 0, 40, 12)), (3, rect(41, 13, 40, 12))]
        );
        let dividers = layout.dividers(AREA);
        assert_eq!(dividers.len(), 2);
        assert_eq!(dividers[0].rect, rect(40, 0, 1, 25));
        assert_eq!(dividers[1].rect, rect(41, 12, 40, 1));
        assert_eq!(dividers[1].path, [true]);
    }

    #[test]
    fn splitting_left_or_up_puts_the_new_pane_first() {
        let mut layout = Layout::new(1);
        layout.split(1, Direction::Left, 2);
        layout.split(1, Direction::Up, 3);
        assert_eq!(layout.panes(), [2, 3, 1]);
    }

    #[test]
    fn closing_gives_the_cells_to_the_sibling_and_the_focus_to_the_nearest_pane() {
        let mut layout = three();
        assert!(!layout.close(3));
        assert_eq!(layout.focus(), 2);
        assert_eq!(layout.rects(AREA), [(1, rect(0, 0, 40, 25)), (2, rect(41, 0, 40, 25))]);
        assert!(!layout.close(1));
        assert_eq!(layout.rects(AREA), [(2, AREA)]);
        assert!(layout.close(2));
        // Closing the left of 1 | (2 / 3) focuses 2, the top of what was beside it.
        let mut layout = three();
        layout.set_focus(1);
        layout.close(1);
        assert_eq!(layout.focus(), 2);
        // An unknown pane changes nothing.
        let mut layout = three();
        assert!(!layout.close(9));
        assert_eq!(layout, three());
    }

    #[test]
    fn closing_cascades_from_pane_to_tab_to_window() {
        let mut tabs = Tabs::new(1);
        tabs.active_mut().split(1, Direction::Right, 2);
        tabs.open(3);
        tabs.open(4);
        assert_eq!((tabs.all().len(), tabs.index()), (3, 2));
        assert_eq!(tabs.close(1), Some(Closed::Pane));
        assert_eq!(tabs.close(4), Some(Closed::Tab));
        assert_eq!(tabs.index(), 1, "the tab before the last shows");
        tabs.select(0);
        assert_eq!(tabs.close(2), Some(Closed::Tab));
        assert_eq!((tabs.all().len(), tabs.index()), (1, 0));
        assert_eq!(tabs.active().panes(), [3]);
        assert_eq!(tabs.close(7), None);
        assert_eq!(tabs.close(3), Some(Closed::Window));
    }

    #[test]
    fn tabs_open_after_the_current_one_and_cycle_round_the_ends() {
        let mut tabs = Tabs::new(1);
        tabs.open(2);
        tabs.select(0);
        tabs.open(3);
        let firsts: Vec<_> = tabs.all().iter().map(|tab| tab.panes()[0]).collect();
        assert_eq!(firsts, [1, 3, 2]);
        tabs.cycle(1);
        tabs.cycle(1);
        assert_eq!(tabs.index(), 0);
        tabs.cycle(-1);
        assert_eq!(tabs.index(), 2);
        tabs.select(9);
        assert_eq!(tabs.index(), 2);
        // Closing a tab before the shown one keeps the same tab showing.
        tabs.close(1);
        assert_eq!(tabs.active().panes(), [2]);
    }

    #[test]
    fn ratios_follow_a_drag_and_keep_each_side_two_cells() {
        let mut layout = three();
        layout.drag(&[], AREA, (20, 3));
        assert_eq!(layout.rects(AREA)[0], (1, rect(0, 0, 20, 25)));
        layout.drag(&[], AREA, (0, 3));
        assert_eq!(layout.rects(AREA)[0].1.cols, MIN);
        layout.drag(&[], AREA, (80, 3));
        assert_eq!(layout.rects(AREA)[1].1.cols, MIN);
        // The nested split's divider by its row.
        layout.drag(&[true], AREA, (60, 5));
        assert_eq!(layout.rects(AREA)[1].1.rows, 5);
        // Ratios hold as the area grows: the left half stays half.
        let mut layout = three();
        let wide = Rect { cols: 161, ..AREA };
        assert_eq!(layout.rects(wide)[0].1.cols, 80);
        layout.drag(&[], wide, (40, 0));
        assert_eq!(layout.rects(AREA)[0].1.cols, 20);
    }

    #[test]
    fn resizing_moves_the_nearest_divider_across_the_direction() {
        let mut layout = three();
        // 3 (bottom right) growing up moves the stacked divider up.
        layout.resize(3, Direction::Up, 3, AREA);
        assert_eq!(layout.rects(AREA)[2].1, rect(41, 10, 40, 15));
        // 3 growing left moves the side-by-side divider, its nearest across that way.
        layout.resize(3, Direction::Left, 5, AREA);
        assert_eq!(layout.rects(AREA)[0].1.cols, 35);
        // 1 has no stacked split above it: nothing moves.
        let before = layout.clone();
        layout.resize(1, Direction::Down, 4, AREA);
        assert_eq!(layout, before);
        // Right from 1 moves the same divider back.
        layout.resize(1, Direction::Right, 5, AREA);
        assert_eq!(layout.rects(AREA)[0].1.cols, 40);
    }

    #[test]
    fn focus_moves_to_the_nearest_overlapping_pane() {
        let mut layout = three();
        assert_eq!(layout.neighbor(3, Direction::Up, AREA), Some(2));
        assert_eq!(layout.neighbor(3, Direction::Left, AREA), Some(1));
        assert_eq!(layout.neighbor(1, Direction::Left, AREA), None);
        assert_eq!(layout.neighbor(2, Direction::Down, AREA), Some(3));
        // From 1 both 2 and 3 are beside it: the last focused wins.
        assert_eq!(layout.neighbor(1, Direction::Right, AREA), Some(3));
        layout.set_focus(2);
        assert_eq!(layout.neighbor(1, Direction::Right, AREA), Some(2));
        assert!(layout.focus_toward(Direction::Left, AREA));
        assert_eq!(layout.focus(), 1);
        assert!(!layout.focus_toward(Direction::Up, AREA));
    }

    #[test]
    fn focus_in_an_uneven_layout_prefers_the_pane_overlapping_most() {
        // (1 / 2) | (3 / 4 / 5), the dividers at different rows; none focused but 1, 5.
        let mut layout = Layout::new(1);
        layout.split(1, Direction::Right, 3);
        layout.split(1, Direction::Down, 2);
        layout.split(3, Direction::Down, 4);
        layout.split(4, Direction::Down, 5);
        layout.drag(&[false], AREA, (0, 5));
        layout.set_focus(1);
        let rects: HashMap<_, _> = layout.rects(AREA).into_iter().collect();
        assert_eq!((rects[&1].rows, rects[&3].rows, rects[&4].rows, rects[&5].rows), (5, 12, 6, 5));
        // From 2 (rows 6–24) to the right: 3 (0–11), 4 (13–18) and 5 (20–24) all overlap it;
        // 5 was focused last of them, so it wins though it overlaps least.
        assert_eq!(layout.neighbor(2, Direction::Right, AREA), Some(5));
        // Without that history, the most overlap, then the first: 3 (6 rows, as 4).
        let mut fresh = layout.clone();
        fresh.focused_at.clear();
        assert_eq!(fresh.neighbor(2, Direction::Right, AREA), Some(3));
        // From 3 to the left only 1 and 2 overlap; 1 was focused last.
        assert_eq!(layout.neighbor(3, Direction::Left, AREA), Some(1));
        // From 5 to the left only 2 overlaps.
        assert_eq!(layout.neighbor(5, Direction::Left, AREA), Some(2));
    }

    #[test]
    fn zoom_fills_the_tab_with_the_focused_pane_until_focus_moves_or_a_split() {
        let mut layout = three();
        layout.toggle_zoom();
        assert!(layout.zoomed());
        assert_eq!(layout.rects(AREA), [(3, AREA)]);
        assert!(layout.dividers(AREA).is_empty());
        // The panes behind it keep their grids.
        let grids = layout.grids(AREA);
        assert_eq!(grids, [(1, rect(0, 0, 40, 25)), (2, rect(41, 0, 40, 12)), (3, AREA)]);
        assert!(grids[1].1.contains(41, 11) && !grids[1].1.contains(41, 12));
        // Focus by direction still sees the whole layout, and leaves zoom.
        assert!(layout.focus_toward(Direction::Up, AREA));
        assert!(!layout.zoomed());
        layout.toggle_zoom();
        layout.split(2, Direction::Right, 4);
        assert!(!layout.zoomed());
        layout.toggle_zoom();
        layout.toggle_zoom();
        assert!(!layout.zoomed());
        // A lone pane has nothing to zoom over.
        let mut single = Layout::new(1);
        single.toggle_zoom();
        assert!(!single.zoomed());
        // Closing the zoomed pane leaves zoom.
        layout.toggle_zoom();
        layout.close(4);
        assert!(!layout.zoomed());
    }

    #[test]
    fn a_tiny_area_still_gives_every_pane_its_cells() {
        let mut layout = Layout::new(1);
        layout.split(1, Direction::Right, 2);
        let rects = layout.rects(Rect { col: 0, row: 0, cols: 3, rows: 1 });
        assert_eq!(rects, [(1, rect(0, 0, 1, 1)), (2, rect(2, 0, 1, 1))]);
    }
}
