//! A window's tabs, each a tree of splits over its panes, in cells: where each pane goes, where
//! the dividers are, which pane has the focus, and every way those change. A window that is a
//! tutti session takes its trees from tutti's (`Layout::mirror`), the same binary trees with
//! ratios. Nothing here knows GPUI, so all of it is unit tested.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::tutti::{Axis, Tree, Workspace};

pub type PaneId = u64;

/// rtty's id for each tutti pane a window shows, by tutti's.
pub type Named = HashMap<u64, PaneId>;

/// Each of a window's tabs as tutti's workspace and tab ids, in the tabs' order.
pub type TabIds = Vec<(u64, u64)>;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// How a split shares its space: side by side, with a vertical divider, or one above the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    SideBySide,
    Stacked,
}

impl Direction {
    pub const ALL: [Direction; 4] =
        [Direction::Left, Direction::Right, Direction::Up, Direction::Down];

    pub fn name(self) -> &'static str {
        match self {
            Direction::Left => "left",
            Direction::Right => "right",
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }

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

/// How far a resize moves a divider.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Amount {
    /// A share of the split's cells, at least one: tutti's measure.
    Fraction(f32),
    Cells(u16),
}

/// A divider is one cell thick; no pane gets fewer cells than this across a split.
const MIN: u16 = 2;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

/// Where a pane sat in its split, to put it back there once it has closed (`Layout::reopen`).
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    /// The path to its split, whose cells its sibling took when it closed.
    path: Vec<bool>,
    /// It was the split's second child: right of or below its sibling.
    second: bool,
    orientation: Orientation,
    ratio: f32,
    /// A pane of its sibling's, which finds the sibling when the path no longer leads there.
    pub beside: PaneId,
}

/// One tab: its split tree, the pane with the focus, and whether that pane fills the tab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    root: Node,
    focus: PaneId,
    /// The pane that had the focus before, while it is open.
    previous: Option<PaneId>,
    zoomed: bool,
    /// When each pane last had the focus, to choose between neighbours.
    focused_at: BTreeMap<PaneId, u64>,
    clock: u64,
}

impl Layout {
    pub fn new(pane: PaneId) -> Layout {
        Layout {
            root: Node::Pane(pane),
            focus: pane,
            previous: None,
            zoomed: false,
            focused_at: BTreeMap::from([(pane, 0)]),
            clock: 0,
        }
    }

    /// A tab laid out as tutti's `tree`, its panes named by `id` from tutti's ids: as `was` was,
    /// its focus, the pane before it and its zoom, while their panes are still there; else
    /// focused where tutti last focused (`active`), else on its first pane.
    pub fn mirror(
        was: Option<&Layout>,
        tree: &Tree,
        active: Option<u64>,
        id: &mut impl FnMut(u64) -> PaneId,
    ) -> Layout {
        let root = Node::mirror(tree, id);
        let mut panes = Vec::new();
        root.collect(&mut panes);
        let active = active.map(id).filter(|pane| panes.contains(pane)).unwrap_or(panes[0]);
        let mut layout = was.cloned().unwrap_or_else(|| Layout::new(active));
        layout.root = root;
        layout.focused_at.retain(|pane, _| panes.contains(pane));
        layout.previous = layout.previous.filter(|pane| panes.contains(pane));
        if !panes.contains(&layout.focus) {
            (layout.focus, layout.zoomed) = (active, false);
        }
        layout.zoomed &= panes.len() > 1;
        layout
    }

    /// The split at `path`: a pane from each side of it, and its ratio.
    pub fn divided(&self, path: &[bool]) -> Option<(PaneId, PaneId, f32)> {
        match self.root.clone().at_path(path)? {
            Node::Split { ratio, first, second, .. } => {
                Some((first.first_pane(false), second.first_pane(false), *ratio))
            }
            Node::Pane(_) => None,
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
            if pane != self.focus {
                self.previous = Some(self.focus);
            }
            self.focus = pane;
        }
    }

    /// Moves the focus back to the pane that had it before, as Vim's `wincmd p` does, leaving
    /// zoom; the pane it moved to, or `None` when there is none or it has closed.
    pub fn focus_previous(&mut self) -> Option<PaneId> {
        let pane = self.previous?;
        self.zoomed = false;
        self.set_focus(pane);
        Some(pane)
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
        if self.previous == Some(pane) {
            self.previous = None;
        }
        false
    }

    /// Where `pane` sits, while it shares a split.
    pub fn place(&self, pane: PaneId) -> Option<Place> {
        self.root.place(pane, &mut Vec::new())
    }

    /// Puts `pane` back where `place` says it sat, beside what took its cells when it closed and
    /// sharing them as before, and focuses it; false when that has left this tab.
    pub fn reopen(&mut self, pane: PaneId, place: &Place) -> bool {
        let path_leads =
            self.root.at_path(&place.path).is_some_and(|node| node.contains(place.beside));
        let sibling = match path_leads {
            true => self.root.at_path(&place.path),
            false => self.root.find(place.beside),
        };
        let Some(sibling) = sibling else { return false };
        let old = std::mem::replace(sibling, Node::Pane(pane));
        let (first, second) =
            if place.second { (old, Node::Pane(pane)) } else { (Node::Pane(pane), old) };
        *sibling = Node::Split {
            orientation: place.orientation,
            ratio: place.ratio,
            first: Box::new(first),
            second: Box::new(second),
        };
        self.zoomed = false;
        self.set_focus(pane);
        true
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

    /// How far a resize of `pane` toward `direction` by `amount` moves the ratio of the split it
    /// moves (`resize`'s, the deepest across that way, as tutti's is), in `area`: a fraction is
    /// itself, cells their share of the cells that split shares out. `None` with no such split.
    pub fn share(
        &self,
        pane: PaneId,
        direction: Direction,
        amount: Amount,
        area: Rect,
    ) -> Option<f32> {
        let path = self.root.ancestor(pane, direction.orientation())?;
        let (extent, orientation) = self.split_at(&path, area)?;
        let len = match orientation {
            Orientation::SideBySide => extent.cols,
            Orientation::Stacked => extent.rows,
        };
        Some(match amount {
            Amount::Fraction(share) => share,
            Amount::Cells(cells) => f32::from(cells) / f32::from(len.saturating_sub(1).max(1)),
        })
    }

    /// Moves the divider nearest `pane` that runs across `direction` that way by `amount`, as
    /// tmux's `resize-pane` and tutti's Alt+h/j/k/l do: the pane grows or shrinks by what its
    /// neighbour gives up.
    pub fn resize(&mut self, pane: PaneId, direction: Direction, amount: Amount, area: Rect) {
        let Some(path) = self.root.ancestor(pane, direction.orientation()) else { return };
        let Some((extent, _)) = self.split_at(&path, area) else { return };
        let divider = self.dividers(area).into_iter().find(|divider| divider.path == path);
        let Some(divider) = divider else { return };
        let (start, len, now) = match direction.orientation() {
            Orientation::SideBySide => (extent.col, extent.cols, divider.rect.col),
            Orientation::Stacked => (extent.row, extent.rows, divider.rect.row),
        };
        let cells = match amount {
            Amount::Cells(cells) => cells,
            Amount::Fraction(share) => {
                ((f32::from(len.saturating_sub(1)) * share).round() as u16).max(1)
            }
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
    /// across, the closest; then the one last focused; then the one overlapping it most. With
    /// `wrap`, when there is none, the farthest of those it overlaps on the other side, as if
    /// the tab went round. Ignores zoom: it answers for the whole layout.
    pub fn neighbor(
        &self,
        from: PaneId,
        direction: Direction,
        area: Rect,
        wrap: bool,
    ) -> Option<PaneId> {
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
        // Round the far side: how near a pane comes to the edge `direction` wraps to.
        let far = |r: Rect| match direction {
            Direction::Right => r.col,
            Direction::Left => u16::MAX - (r.col + r.cols),
            Direction::Down => r.row,
            Direction::Up => u16::MAX - (r.row + r.rows),
        };
        let (here_start, here_end) = span(here);
        let nearest = |distance: &dyn Fn(Rect) -> Option<u16>| {
            rects
                .iter()
                .enumerate()
                .filter(|(_, (pane, _))| *pane != from)
                .filter_map(|(order, &(pane, rect))| {
                    let (start, end) = span(rect);
                    let overlap = end.min(here_end).saturating_sub(start.max(here_start));
                    let recent = self.focused_at.get(&pane).copied().unwrap_or(0);
                    (overlap > 0).then_some(())?;
                    Some(((distance(rect)?, u64::MAX - recent, u16::MAX - overlap, order), pane))
                })
                .min()
                .map(|(_, pane)| pane)
        };
        nearest(&gap).or_else(|| wrap.then(|| nearest(&|rect| Some(far(rect)))).flatten())
    }

    /// Moves the focus from `from` to its neighbour in `direction` (see `neighbor`), leaving
    /// zoom; the pane it moved to.
    pub fn focus_toward(
        &mut self,
        from: PaneId,
        direction: Direction,
        area: Rect,
        wrap: bool,
    ) -> Option<PaneId> {
        let pane = self.neighbor(from, direction, area, wrap)?;
        self.zoomed = false;
        self.set_focus(pane);
        Some(pane)
    }

    /// Trades `pane`'s place with its neighbour in `direction` (see `neighbor`), leaving zoom: the
    /// splits stay as they are and the focus stays with `pane`. The pane it traded with.
    pub fn swap(&mut self, pane: PaneId, direction: Direction, area: Rect) -> Option<PaneId> {
        let other = self.neighbor(pane, direction, area, false)?;
        self.root.trade(pane, other);
        self.zoomed = false;
        Some(other)
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        self.root.contains(pane)
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
    /// tutti's `tree`, its panes named by `id`.
    fn mirror(tree: &Tree, id: &mut impl FnMut(u64) -> PaneId) -> Node {
        match tree {
            Tree::Leaf(pane) => Node::Pane(id(*pane)),
            Tree::Split { direction, ratio, first, second } => Node::Split {
                orientation: match direction {
                    Axis::Horizontal => Orientation::SideBySide,
                    Axis::Vertical => Orientation::Stacked,
                },
                ratio: ratio.clamp(0., 1.),
                first: Box::new(Node::mirror(first, id)),
                second: Box::new(Node::mirror(second, id)),
            },
        }
    }

    /// Every split's ratio is a share, from 0 to 1.
    fn shares(&self) -> bool {
        match self {
            Node::Pane(_) => true,
            Node::Split { ratio, first, second, .. } => {
                (0. ..=1.).contains(ratio) && first.shares() && second.shares()
            }
        }
    }

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

    /// Puts `a` where `b` is and `b` where `a` is.
    fn trade(&mut self, a: PaneId, b: PaneId) {
        match self {
            Node::Pane(id) if *id == a => *id = b,
            Node::Pane(id) if *id == b => *id = a,
            Node::Pane(_) => {}
            Node::Split { first, second, .. } => {
                first.trade(a, b);
                second.trade(a, b);
            }
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

    /// Where `pane` sits below this node, which `path` leads to.
    fn place(&self, pane: PaneId, path: &mut Vec<bool>) -> Option<Place> {
        let Node::Split { orientation, ratio, first, second } = self else { return None };
        let side = second.contains(pane);
        let (child, sibling) = if side { (second, first) } else { (first, second) };
        if **child != Node::Pane(pane) {
            path.push(side);
            return child.place(pane, path);
        }
        Some(Place {
            path: path.clone(),
            second: side,
            orientation: *orientation,
            ratio: *ratio,
            beside: sibling.first_pane(side),
        })
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

    /// The tabs tutti's `workspaces` have (those with a pane, workspace by workspace) as these
    /// tabs, whose tutti workspace and tab ids `ids` lists in order, would show them: each laid
    /// out by `Layout::mirror` from the tab here with its tutti id, wherever it moved; rtty's id
    /// for each tutti pane from `panes`, else `next`. The tab that shows here shows still, else
    /// tutti's current one, else the first. With the new tabs' ids and the panes they hold; none
    /// when no tab has a pane.
    pub fn mirror(
        &self,
        ids: &[(u64, u64)],
        workspaces: &[Workspace],
        panes: &Named,
        mut next: impl FnMut() -> PaneId,
    ) -> Option<(Tabs, TabIds, Named)> {
        let shown = ids.get(self.active).map(|(_, tab)| *tab);
        let was = |tab| ids.iter().position(|(_, id)| *id == tab).and_then(|at| self.tabs.get(at));
        let (mut layouts, mut tabs, mut current) = (Vec::new(), Vec::new(), None);
        let mut named = HashMap::new();
        for workspace in workspaces {
            for tab in &workspace.tabs {
                let Some(tree) = &tab.layout else { continue };
                let mut id = |pane| {
                    let known = panes.get(&pane).copied();
                    *named.entry(pane).or_insert_with(|| known.unwrap_or_else(&mut next))
                };
                layouts.push(Layout::mirror(was(tab.id), tree, tab.active_pane, &mut id));
                if tab.active {
                    current = Some(tabs.len());
                }
                tabs.push((workspace.id, tab.id));
            }
        }
        if layouts.is_empty() {
            return None;
        }
        let present: Vec<PaneId> = layouts.iter().flat_map(Layout::panes).collect();
        named.retain(|_, id| present.contains(id));
        let index = tabs.iter().position(|(_, tab)| Some(*tab) == shown).or(current).unwrap_or(0);
        Some((Tabs::of(layouts, index), tabs, named))
    }

    /// `tabs`, tab `active` showing (the last, past them); at least one tab.
    pub fn of(tabs: Vec<Layout>, active: usize) -> Tabs {
        assert!(!tabs.is_empty(), "a window has a tab");
        Tabs { active: active.min(tabs.len() - 1), tabs }
    }

    /// Whether tabs read back from a file hold together: one of them shows, each one's focus,
    /// and the focus before it, is one of its panes, and every split's ratio is a share.
    pub fn sound(&self) -> bool {
        self.active < self.tabs.len()
            && self.tabs.iter().all(|tab| {
                let panes = tab.panes();
                let held = |pane: &PaneId| panes.contains(pane);
                held(&tab.focus) && tab.previous.as_ref().is_none_or(held) && tab.root.shares()
            })
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

    /// The tab holding `pane`.
    pub fn find(&self, pane: PaneId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.contains(pane))
    }

    /// The tab holding any of `panes`, which finds a tab again after some of them closed.
    pub fn find_any(&self, panes: &[PaneId]) -> Option<usize> {
        panes.iter().find_map(|&pane| self.find(pane))
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
        let index = self.find(pane)?;
        if !self.tabs[index].close(pane) {
            return Some(Closed::Pane);
        }
        self.close_tab(index).map(|(_, closed)| closed)
    }

    /// Puts a closed tab back at `index` (last, when there are fewer tabs now) and shows it.
    pub fn reopen(&mut self, index: usize, tab: Layout) {
        self.active = index.min(self.tabs.len());
        self.tabs.insert(self.active, tab);
    }

    /// Closes tab `index` and every pane in it, as `close` does a tab left empty; the panes it
    /// held, and whether it was the window's last.
    pub fn close_tab(&mut self, index: usize) -> Option<(Vec<PaneId>, Closed)> {
        let panes = self.tabs.get(index)?.panes();
        if self.tabs.len() == 1 {
            return Some((panes, Closed::Window));
        }
        self.tabs.remove(index);
        if self.active > index || self.active == self.tabs.len() {
            self.active -= 1;
        }
        Some((panes, Closed::Tab))
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
    fn a_closed_pane_goes_back_into_its_split() {
        // 3, below 2 in the right column of 1 | (2 / 3), the dividers moved off the middle.
        let mut layout = three();
        layout.drag(&[], AREA, (30, 0));
        layout.drag(&[true], AREA, (50, 8));
        let before = layout.rects(AREA);
        let place = layout.place(3).unwrap();
        assert_eq!(place.beside, 2);
        layout.close(3);
        assert!(layout.reopen(3, &place));
        assert_eq!((layout.rects(AREA), layout.focus()), (before, 3));
        // 1 goes back beside the whole column, which split again meanwhile.
        let place = layout.place(1).unwrap();
        layout.close(1);
        layout.split(2, Direction::Right, 4);
        assert!(layout.reopen(1, &place));
        assert_eq!(layout.panes(), [1, 2, 4, 3]);
        assert_eq!(layout.rects(AREA)[0].1, rect(0, 0, 30, 25));
        // With its split's path gone, beside the pane it sat by.
        let mut layout = three();
        let place = layout.place(3).unwrap();
        layout.close(3);
        layout.close(1);
        assert!(layout.reopen(3, &place));
        assert_eq!(layout.rects(AREA), [(2, rect(0, 0, 81, 12)), (3, rect(0, 13, 81, 12))]);
        // A lone pane sits in no split, and a pane whose sibling left the tab has no place.
        assert_eq!(Layout::new(1).place(1), None);
        let mut other = Layout::new(9);
        assert!(!other.reopen(3, &place));
        assert_eq!(other, Layout::new(9));
    }

    #[test]
    fn a_tab_is_found_by_any_pane_left_in_it() {
        let mut tabs = Tabs::new(1);
        tabs.open(2);
        tabs.active_mut().split(2, Direction::Right, 3);
        let panes = tabs.all()[1].panes();
        assert_eq!(tabs.close(3), Some(Closed::Pane));
        assert_eq!(tabs.find(3), None);
        assert_eq!(tabs.find_any(&[3, 2]), Some(1));
        assert_eq!(tabs.find_any(&panes), Some(1));
        tabs.close(2);
        assert_eq!(tabs.find_any(&panes), None);
    }

    #[test]
    fn a_closed_tab_goes_back_where_it_was() {
        let mut tabs = Tabs::new(1);
        tabs.open(2);
        tabs.open(3);
        let tab = tabs.all()[1].clone();
        tabs.close_tab(1);
        tabs.select(0);
        tabs.reopen(1, tab);
        let firsts: Vec<_> = tabs.all().iter().map(|tab| tab.panes()[0]).collect();
        assert_eq!((firsts, tabs.index()), (vec![1, 2, 3], 1));
        // Past the tabs left, it goes last.
        let tab = tabs.all()[2].clone();
        tabs.close_tab(2);
        tabs.close_tab(1);
        tabs.reopen(2, tab);
        assert_eq!((tabs.all().len(), tabs.index()), (2, 1));
        assert_eq!(tabs.active().panes(), [3]);
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
    fn closing_a_tab_takes_its_panes_and_keeps_the_shown_tab_showing() {
        let mut tabs = Tabs::new(1);
        tabs.active_mut().split(1, Direction::Right, 2);
        tabs.open(3);
        tabs.open(4);
        assert_eq!(tabs.close_tab(0), Some((vec![1, 2], Closed::Tab)));
        assert_eq!(tabs.active().panes(), [4]);
        // The shown tab, last: the one before it shows.
        assert_eq!(tabs.close_tab(1), Some((vec![4], Closed::Tab)));
        assert_eq!(tabs.active().panes(), [3]);
        assert_eq!(tabs.close_tab(5), None);
        assert_eq!(tabs.close_tab(0), Some((vec![3], Closed::Window)));
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
        // 3 (bottom right) growing up moves the stacked divider up: an eighth of the 24 rows
        // the right column's divider leaves.
        layout.resize(3, Direction::Up, Amount::Fraction(0.125), AREA);
        assert_eq!(layout.rects(AREA)[2].1, rect(41, 10, 40, 15));
        // 3 growing left moves the side-by-side divider, its nearest across that way: tutti's
        // step, 5% of 80 columns.
        layout.resize(3, Direction::Left, Amount::Fraction(0.05), AREA);
        assert_eq!(layout.rects(AREA)[0].1.cols, 36);
        // 1 has no stacked split above it: nothing moves.
        let before = layout.clone();
        layout.resize(1, Direction::Down, Amount::Fraction(0.2), AREA);
        assert_eq!(layout, before);
        // Right from 1 moves the same divider back; a sliver of the split is still a cell.
        layout.resize(1, Direction::Right, Amount::Cells(4), AREA);
        assert_eq!(layout.rects(AREA)[0].1.cols, 40);
        layout.resize(1, Direction::Right, Amount::Fraction(0.001), AREA);
        assert_eq!(layout.rects(AREA)[0].1.cols, 41);
    }

    #[test]
    fn focus_moves_to_the_nearest_overlapping_pane() {
        let mut layout = three();
        let neighbor =
            |layout: &Layout, from, direction| layout.neighbor(from, direction, AREA, false);
        assert_eq!(neighbor(&layout, 3, Direction::Up), Some(2));
        assert_eq!(neighbor(&layout, 3, Direction::Left), Some(1));
        assert_eq!(neighbor(&layout, 1, Direction::Left), None);
        assert_eq!(neighbor(&layout, 2, Direction::Down), Some(3));
        // From 1 both 2 and 3 are beside it: the last focused wins.
        assert_eq!(neighbor(&layout, 1, Direction::Right), Some(3));
        layout.set_focus(2);
        assert_eq!(neighbor(&layout, 1, Direction::Right), Some(2));
        assert_eq!(layout.focus_toward(2, Direction::Left, AREA, false), Some(1));
        assert_eq!(layout.focus(), 1);
        assert_eq!(layout.focus_toward(1, Direction::Up, AREA, false), None);
        // From any pane, not only the focused one.
        assert_eq!(layout.focus_toward(3, Direction::Up, AREA, false), Some(2));
    }

    #[test]
    fn wrapping_goes_round_to_the_farthest_pane_level_with_it() {
        // 1 | ((2 | 4) / 3): 2 and 4 share the top of the right column, 3 its bottom.
        let mut layout = three();
        layout.split(2, Direction::Right, 4);
        let wrap = |layout: &Layout, from, direction| layout.neighbor(from, direction, AREA, true);
        // At the right edge, round to the leftmost pane level with it.
        assert_eq!(layout.neighbor(4, Direction::Right, AREA, false), None);
        assert_eq!(wrap(&layout, 4, Direction::Right), Some(1));
        assert_eq!(wrap(&layout, 3, Direction::Right), Some(1));
        // Not at an edge: the plain neighbour.
        assert_eq!(wrap(&layout, 2, Direction::Right), Some(4));
        // Down from 3 round to the top of its column, where 2 and 4 tie: the last focused, 4;
        // without that history, the one overlapping it most, 2 (20 columns to 4's 19).
        assert_eq!(wrap(&layout, 3, Direction::Down), Some(4));
        let mut fresh = layout.clone();
        fresh.focused_at.clear();
        assert_eq!(wrap(&fresh, 3, Direction::Down), Some(2));
        // Up from 1, which nothing overlaps across: an edge still.
        assert_eq!(wrap(&layout, 1, Direction::Up), None);
        assert_eq!(layout.focus_toward(4, Direction::Right, AREA, true), Some(1));
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
        let rects: BTreeMap<_, _> = layout.rects(AREA).into_iter().collect();
        assert_eq!((rects[&1].rows, rects[&3].rows, rects[&4].rows, rects[&5].rows), (5, 12, 6, 5));
        // From 2 (rows 6–24) to the right: 3 (0–11), 4 (13–18) and 5 (20–24) all overlap it;
        // 5 was focused last of them, so it wins though it overlaps least.
        assert_eq!(layout.neighbor(2, Direction::Right, AREA, false), Some(5));
        // Without that history, the most overlap, then the first: 3 (6 rows, as 4).
        let mut fresh = layout.clone();
        fresh.focused_at.clear();
        assert_eq!(fresh.neighbor(2, Direction::Right, AREA, false), Some(3));
        // From 3 to the left only 1 and 2 overlap; 1 was focused last.
        assert_eq!(layout.neighbor(3, Direction::Left, AREA, false), Some(1));
        // From 5 to the left only 2 overlaps.
        assert_eq!(layout.neighbor(5, Direction::Left, AREA, false), Some(2));
    }

    #[test]
    fn the_previous_focus_comes_back_until_it_closes() {
        // Focused 1, then 2, then 3.
        let mut layout = three();
        assert_eq!(layout.focus_previous(), Some(2));
        assert_eq!(layout.focus(), 2);
        // Back and forth, as Vim's `wincmd p` goes; focusing the focused pane is no move.
        assert_eq!(layout.focus_previous(), Some(3));
        layout.set_focus(3);
        assert_eq!(layout.focus_previous(), Some(2));
        // It leaves zoom.
        layout.toggle_zoom();
        assert_eq!(layout.focus_previous(), Some(3));
        assert!(!layout.zoomed());
        // A closed pane is not replaced by one focused longer ago.
        layout.close(2);
        assert_eq!(layout.focus_previous(), None);
        assert_eq!(layout.focus(), 3);
        // Closing the focused pane passes the focus on; the closed one was the previous.
        layout.set_focus(1);
        layout.close(1);
        assert_eq!(layout.focus(), 3);
        assert_eq!(layout.focus_previous(), None);
        assert_eq!(Layout::new(1).focus_previous(), None);
    }

    #[test]
    fn swapping_trades_places_with_the_neighbour_focus_would_go_to() {
        let mut layout = three();
        // 3, bottom right, swaps up with 2: the panes trade cells, and 3 keeps the focus.
        assert_eq!(layout.swap(3, Direction::Up, AREA), Some(2));
        assert_eq!(
            layout.rects(AREA),
            [(1, rect(0, 0, 40, 25)), (3, rect(41, 0, 40, 12)), (2, rect(41, 13, 40, 12))]
        );
        assert_eq!(layout.focus(), 3);
        // The splits stay where they are.
        layout.drag(&[], AREA, (20, 0));
        let dividers = layout.dividers(AREA);
        assert_eq!(layout.swap(3, Direction::Left, AREA), Some(1));
        assert_eq!(
            layout.rects(AREA),
            [(3, rect(0, 0, 20, 25)), (1, rect(21, 0, 60, 12)), (2, rect(21, 13, 60, 12))]
        );
        assert_eq!(layout.dividers(AREA), dividers);
        // At an edge nothing moves.
        let before = layout.clone();
        assert_eq!(layout.swap(3, Direction::Left, AREA), None);
        assert_eq!(layout, before);
        // Right from 3 both 1 and 2 are beside it: the last focused, 2, as for focus. A swap
        // leaves zoom.
        layout.toggle_zoom();
        assert_eq!(layout.swap(3, Direction::Right, AREA), Some(2));
        assert!(!layout.zoomed());
        assert_eq!(layout.panes(), [2, 1, 3]);
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
        assert_eq!(layout.focus_toward(3, Direction::Up, AREA, false), Some(2));
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

    /// tutti's tree for 1 | (2 / 3), its panes numbered `base` and on.
    fn tutti_three(base: u64) -> Tree {
        let leaf = |pane: u64| Box::new(Tree::Leaf(base + pane));
        let column =
            Tree::Split { direction: Axis::Vertical, ratio: 0.5, first: leaf(2), second: leaf(3) };
        Tree::Split {
            direction: Axis::Horizontal,
            ratio: 0.25,
            first: leaf(1),
            second: Box::new(column),
        }
    }

    /// A tab mirrors tutti's tree as rtty's own, ratios and all, its panes named as the window
    /// names them; it keeps its focus, the one before and zoom while their panes stay, else takes
    /// tutti's focus, else its first pane.
    #[test]
    fn a_tab_mirrors_tuttis_tree_and_keeps_its_own_focus() {
        let mut named = |pane: u64| pane - 10;
        let fresh = Layout::mirror(None, &tutti_three(10), Some(13), &mut named);
        let mut three = three();
        three.drag(&[], AREA, (20, 0));
        assert_eq!(fresh.rects(AREA), three.rects(AREA));
        assert_eq!(fresh.focus(), 3, "a tab rtty had not shown starts where tutti focused");
        let first = Layout::mirror(None, &tutti_three(10), None, &mut named);
        assert_eq!(first.focus(), 1, "else on its first pane");
        let mut was = fresh.clone();
        was.set_focus(2);
        was.set_focus(3);
        was.toggle_zoom();
        let kept = Layout::mirror(Some(&was), &tutti_three(10), Some(11), &mut named);
        assert_eq!((kept.focus(), kept.zoomed()), (3, true));
        // 3 gone: tutti's focus, zoom left; 2 stays the one before.
        let two = Tree::Split {
            direction: Axis::Horizontal,
            ratio: 0.5,
            first: Box::new(Tree::Leaf(11)),
            second: Box::new(Tree::Leaf(12)),
        };
        let mut after = Layout::mirror(Some(&kept), &two, Some(11), &mut named);
        assert_eq!((after.panes(), after.focus(), after.zoomed()), (vec![1, 2], 1, false));
        assert_eq!(after.focus_previous(), Some(2));
        // tutti's focus not among them: the first pane.
        let lone = Layout::mirror(Some(&after), &Tree::Leaf(14), Some(19), &mut named);
        assert_eq!((lone.panes(), lone.focus()), (vec![4], 4));
        let tabs = Tabs::of(vec![lone, after], 5);
        assert_eq!(tabs.index(), 1);
    }

    /// tutti's tabs as a window's, then reordered: each tab keeps its own zoom and rtty's pane ids
    /// by its tutti id, wherever it moved; the tab that showed shows still; a new pane gets the
    /// next id, a tab with no pane is left out, and a session with none gives no tabs.
    #[test]
    fn tabs_mirror_tuttis_by_tab_id_whatever_their_order() {
        use crate::tutti::Tab;
        let tab = |id, layout, active| Tab { id, active, layout, active_pane: None };
        let column = Tree::Split {
            direction: Axis::Vertical,
            ratio: 0.5,
            first: Box::new(Tree::Leaf(21)),
            second: Box::new(Tree::Leaf(22)),
        };
        let first = [Workspace {
            id: 5,
            tabs: vec![tab(10, Some(tutti_three(10)), true), tab(20, Some(Tree::Leaf(21)), false)],
        }];
        let mut next = 100..;
        let mirrored = Tabs::new(1).mirror(&[], &first, &HashMap::new(), || next.next().unwrap());
        let (mut tabs, ids, panes) = mirrored.unwrap();
        assert_eq!((ids.as_slice(), tabs.index()), ([(5, 10), (5, 20)].as_slice(), 0));
        assert_eq!(
            (tabs.all()[0].panes(), tabs.all()[1].panes()),
            (vec![100, 101, 102], vec![103])
        );
        tabs.active_mut().toggle_zoom();
        tabs.select(1);
        let reordered = [Workspace {
            id: 5,
            tabs: vec![
                tab(20, Some(column), false),
                tab(10, Some(tutti_three(10)), true),
                tab(30, None, false),
            ],
        }];
        let mirrored = tabs.mirror(&ids, &reordered, &panes, || next.next().unwrap());
        let (again, ids, panes) = mirrored.unwrap();
        assert_eq!(ids, [(5, 20), (5, 10)]);
        assert_eq!(again.index(), 0, "tab 20 shows still, first now");
        assert_eq!(
            (again.all()[0].panes(), again.all()[1].panes()),
            (vec![103, 104], vec![100, 101, 102])
        );
        assert!(!again.all()[0].zoomed() && again.all()[1].zoomed(), "zoom stays with tab 10");
        assert_eq!(panes.len(), 5);
        let none = [Workspace { id: 5, tabs: vec![tab(30, None, true)] }];
        assert!(again.mirror(&ids, &none, &panes, || 0).is_none());
    }

    /// A resize by cells is their share of the split it moves, not of the window: in 1 | (2 | 3),
    /// 4 cells left from 3 move the inner split, 40 cells across, by 0.1, and that ratio moved
    /// so (as tutti moves it) gives 3 four more columns.
    #[test]
    fn a_resize_by_cells_is_its_share_of_the_split_it_moves() {
        let area = Rect { cols: 83, ..AREA };
        let mut layout = Layout::new(1);
        layout.split(1, Direction::Right, 2);
        layout.split(2, Direction::Right, 3);
        let delta = layout.share(3, Direction::Left, Amount::Cells(4), area).unwrap();
        assert_eq!(delta, 0.1);
        let width = |layout: &Layout| layout.rects(area)[2].1.cols;
        let before = width(&layout);
        if let Some(Node::Split { ratio, .. }) = layout.root.at_path(&[true]) {
            *ratio -= delta;
        }
        assert_eq!(width(&layout), before + 4);
        let fraction = layout.share(3, Direction::Left, Amount::Fraction(0.05), area);
        assert_eq!(fraction, Some(0.05));
        assert_eq!(
            layout.share(3, Direction::Up, Amount::Cells(4), area),
            None,
            "no split that way"
        );
    }

    /// A divider names the split it moves by a pane from either side, as tutti's border drag does.
    #[test]
    fn a_divider_names_a_pane_on_either_side() {
        let layout = three();
        assert_eq!(layout.divided(&[]), Some((1, 2, 0.5)));
        assert_eq!(layout.divided(&[true]), Some((2, 3, 0.5)));
        assert_eq!(layout.divided(&[false]), None);
        assert_eq!(layout.divided(&[true, true, true]), None);
    }

    #[test]
    fn a_tiny_area_still_gives_every_pane_its_cells() {
        let mut layout = Layout::new(1);
        layout.split(1, Direction::Right, 2);
        let rects = layout.rects(Rect { col: 0, row: 0, cols: 3, rows: 1 });
        assert_eq!(rects, [(1, rect(0, 0, 1, 1)), (2, rect(2, 0, 1, 1))]);
    }
}
