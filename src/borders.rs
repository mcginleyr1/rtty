//! The cell-border protocol's lines as rtty draws them: the strokes programs put on the edges and
//! centrelines of cells (`rtty_vt::Borders`), which take no cells of their own. Independent of
//! GPUI: a frame's bordered cells in, shapes in device pixels out, so that straight lines land on
//! whole pixels and meet the next cell's.
//!
//! - A stroke inside its cell lies flush with the edge. A centred one straddles the edge, and of
//!   the two centred strokes neighbours put on a shared edge only one is drawn: the one with the
//!   higher priority, then the wider, then the lower or right-hand cell's.
//! - Centred strokes that meet at a corner point reach across each other, so corners close; a
//!   centred stroke on the grid's own edge is kept on it, a pixel at least.
//! - A rounded joint draws a quarter circle between its two arms, in the horizontal arm's stroke.
//!   An arm stops where its arc begins unless the line goes straight on past the joint (a fork).
//!   What the arc cuts off is filled with the cell's outside color, under every stroke.

use std::array;

use rtty_vt::Rgb;

use crate::glyphs::{KAPPA, Seg, Shape};

/// A cell's borders with their colors resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct Set {
    /// By the protocol's bit: the outer edges' halves (top left, top right, right top, right
    /// bottom, bottom left, bottom right, left top, left bottom), then the centre arms (left,
    /// right, top, bottom).
    pub segments: [Option<Stroke>; 12],
    /// The rounded joints: the corners (top left, top right, bottom right, bottom left), then the
    /// centre's quadrants (top right, bottom right, bottom left, top left).
    pub joints: u8,
    pub outside: Option<Rgb>,
    /// Of the rounded joints, in 1/256 of the cell's width.
    pub radius: u16,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stroke {
    /// In 1/256 of the cell's width.
    pub width: u8,
    pub color: Rgb,
    pub centered: bool,
    pub above: bool,
    pub priority: u8,
}

/// The radius of a rounded joint whose program names none: half the cell's width.
pub const RADIUS: u16 = 128;

/// Where the cells are: the grid's corner and a cell's size in device pixels, neither rounded.
pub struct Grid {
    pub origin: (f32, f32),
    pub cell: (f32, f32),
    pub rows: u16,
    pub cols: u16,
}

/// What a frame's borders paint, in the order it paints them.
#[derive(Debug, Default, PartialEq)]
pub struct Layers {
    /// What rounded joints cut off, each shown within its cell (x, y, width, height).
    pub fills: Vec<(Shape, Rgb, [f32; 4])>,
    /// The strokes under the text.
    pub below: Vec<(Shape, Rgb)>,
    /// The strokes over it.
    pub above: Vec<(Shape, Rgb)>,
}

/// A joint's quadrants by the protocol's bit: the horizontal and the vertical segment it joins,
/// and which way each leaves the joint.
const QUADRANTS: [(usize, usize, f32, f32); 8] = [
    (0, 6, 1., 1.),
    (1, 2, -1., 1.),
    (5, 3, -1., -1.),
    (4, 7, 1., -1.),
    (9, 10, 1., -1.),
    (9, 11, 1., 1.),
    (8, 11, -1., 1.),
    (8, 10, -1., -1.),
];

/// The corner each half of an outer edge starts from, as a quadrant.
const CORNER: [usize; 8] = [0, 1, 1, 2, 3, 2, 0, 3];

/// The quadrants beside each centre arm.
const BESIDE: [[usize; 2]; 4] = [[6, 7], [4, 5], [4, 7], [5, 6]];

/// A rounded joint: where its two lines would have met, and its radius.
#[derive(Clone, Copy)]
struct Round {
    x: f32,
    y: f32,
    r: f32,
}

impl Grid {
    fn x(&self, col: i32) -> f32 {
        (self.origin.0 + self.cell.0 * col as f32).round()
    }

    fn y(&self, row: i32) -> f32 {
        (self.origin.1 + self.cell.1 * row as f32).round()
    }

    /// A stroke's thickness: its share of the cell's width, a pixel at least.
    fn thick(&self, stroke: &Stroke) -> f32 {
        (f32::from(stroke.width) / 256. * self.cell.0).round().max(1.)
    }

    /// Where a line `t` thick centred on the grid line at `at` starts, and how thick it is there:
    /// cut to the grid (`across` columns, else rows), with a pixel left on its last line.
    fn centred(&self, at: f32, t: f32, across: bool) -> (f32, f32) {
        let (min, max) = match across {
            true => (self.x(0), self.x(i32::from(self.cols))),
            false => (self.y(0), self.y(i32::from(self.rows))),
        };
        let start = at - (t / 2.).floor();
        let kept = start.max(min).min(max - 1.);
        (kept, (start + t).min(max) - kept)
    }
}

struct Frame<'a> {
    rows: &'a [&'a [(u16, Set)]],
    grid: &'a Grid,
}

impl Frame<'_> {
    fn set(&self, row: i32, col: i32) -> Option<&Set> {
        let cells = self.rows.get(usize::try_from(row).ok()?)?;
        let col = u16::try_from(col).ok()?;
        let at = cells.binary_search_by_key(&col, |(col, _)| *col).ok()?;
        Some(&cells[at].1)
    }

    /// The stroke the cell has on `segment`, if it is the one drawn there: on an outer edge a
    /// centred stroke gives way to the neighbour's centred stroke on the same edge when that one
    /// has the higher priority, else is wider, else is the lower or right-hand cell's.
    fn drawn(&self, row: i32, col: i32, segment: usize) -> Option<Stroke> {
        let stroke = self.set(row, col)?.segments[segment]?;
        let (down, right, other, last) = match segment {
            0 | 1 => (-1, 0, segment + 4, true),
            2 | 3 => (0, 1, segment + 4, false),
            4 | 5 => (1, 0, segment - 4, false),
            6 | 7 => (0, -1, segment - 4, true),
            _ => return Some(stroke),
        };
        let theirs = self.set(row + down, col + right).and_then(|set| set.segments[other]);
        let wins =
            theirs.filter(|theirs| theirs.centered && stroke.centered).is_none_or(|theirs| {
                let (mine, theirs) =
                    ((stroke.priority, stroke.width), (theirs.priority, theirs.width));
                mine > theirs || mine == theirs && last
            });
        wins.then_some(stroke)
    }

    /// The centred stroke drawn on the half edge that leaves the corner point at the top left of
    /// the cell to the right (0), down, to the left or up: one of the two cells' beside it.
    fn arm(&self, row: i32, col: i32, direction: usize) -> Option<Stroke> {
        let cells = match direction {
            0 => [(row, col, 0), (row - 1, col, 4)],
            1 => [(row, col, 6), (row, col - 1, 2)],
            2 => [(row, col - 1, 1), (row - 1, col - 1, 5)],
            _ => [(row - 1, col, 7), (row - 1, col - 1, 3)],
        };
        let centred = |(row, col, segment)| self.drawn(row, col, segment).filter(|s| s.centered);
        cells.into_iter().find_map(centred)
    }

    /// Where a segment's line lies across its direction: its start and thickness.
    fn across(&self, row: i32, col: i32, segment: usize, stroke: &Stroke) -> (f32, f32) {
        let grid = self.grid;
        let t = grid.thick(stroke);
        let (x0, x1, y0, y1) = (grid.x(col), grid.x(col + 1), grid.y(row), grid.y(row + 1));
        let edge = |at: f32, far: bool, across: bool| match (stroke.centered, far) {
            (true, _) => grid.centred(at, t, across),
            (false, false) => (at, t),
            (false, true) => (at - t, t),
        };
        match segment {
            0 | 1 => edge(y0, false, false),
            2 | 3 => edge(x1, true, true),
            4 | 5 => edge(y1, true, false),
            6 | 7 => edge(x0, false, true),
            8 | 9 => (y0 + ((y1 - y0 - t) / 2.).floor(), t),
            _ => (x0 + ((x1 - x0 - t) / 2.).floor(), t),
        }
    }

    fn cell(&self, row: i32, col: i32, set: &Set, out: &mut Layers) {
        let grid = self.grid;
        let (x0, x1, y0, y1) = (grid.x(col), grid.x(col + 1), grid.y(row), grid.y(row + 1));
        let mid = (x0 + ((x1 - x0) / 2.).round(), y0 + ((y1 - y0) / 2.).round());
        let drawn: [Option<Stroke>; 12] = array::from_fn(|s| self.drawn(row, col, s));
        let lines: [Option<(f32, f32)>; 12] =
            array::from_fn(|s| Some(self.across(row, col, s, &drawn[s]?)));
        let away = |at: f32, sign: f32| if sign > 0. { at.ceil() } else { at.floor() };

        // A quadrant rounds when both its arms are drawn, a corner's in the same placement.
        let rounds = |q: usize| {
            let (h, v, ..) = QUADRANTS[q];
            let together = |h: Stroke, v: Stroke| q >= 4 || h.centered == v.centered;
            set.joints & (1 << q) != 0
                && matches!((drawn[h], drawn[v]), (Some(h), Some(v)) if together(h, v))
        };
        let wanted = (f32::from(set.radius) / 256. * grid.cell.0).round();
        let rounded: [Option<Round>; 8] = array::from_fn(|q| {
            let (h, v, sx, sy) = QUADRANTS[q];
            let ((across, wide), (down, tall)) = (lines[v].filter(|_| rounds(q))?, lines[h]?);
            let (x, y) = (across + wide / 2., down + tall / 2.);
            // An arc may take the whole edge when its other half is the same stroke and the far
            // corner does not round too; a centre arm reaches the cell's edge.
            let whole_h = q >= 4 || drawn[h ^ 1] == drawn[h] && !rounds(q ^ 1);
            let whole_v = q >= 4 || drawn[v ^ 1] == drawn[v] && !rounds(3 - q);
            let far = (if sx > 0. { x1 } else { x0 }, if sy > 0. { y1 } else { y0 });
            let (end_h, end_v) =
                (if whole_h { far.0 } else { mid.0 }, if whole_v { far.1 } else { mid.1 });
            let r = wanted.min((end_h - x) * sx).min((end_v - y) * sy);
            (r >= 1.).then_some(Round { x, y, r })
        });
        // Where an arm picks up again past the quadrant's arc, along the arm.
        let resumes = |q: usize, horizontal: bool| {
            let (_, _, sx, sy) = QUADRANTS[q];
            let round = rounded[q].as_ref()?;
            Some(match horizontal {
                true => away(round.x + sx * round.r, sx),
                false => away(round.y + sy * round.r, sy),
            })
        };
        // The corner point an edge's half starts from as `arm` counts it, and which way it leaves.
        let corner = |s: usize| {
            let (h, _, sx, sy) = QUADRANTS[CORNER[s]];
            let direction = match (s == h, sx > 0., sy > 0.) {
                (true, true, _) => 0,
                (true, false, _) => 2,
                (false, _, true) => 1,
                (false, _, false) => 3,
            };
            (row + i32::from(sy < 0.), col + i32::from(sx < 0.), direction)
        };
        // Where an edge's half stops short of its corner: where its arc begins, unless the line
        // goes straight on past the corner point.
        let stops = |s: usize| {
            let (row, col, direction) = corner(s);
            let start = resumes(CORNER[s], direction % 2 == 0)?;
            let on = drawn[s]?.centered && self.arm(row, col, (direction + 2) % 4).is_some();
            (!on).then_some(start)
        };

        for s in 0..12 {
            let (Some(stroke), Some((lo, t))) = (drawn[s], lines[s]) else { continue };
            let horizontal = matches!(s, 0 | 1 | 4 | 5 | 8 | 9);
            let middle = if horizontal { mid.0 } else { mid.1 };
            // Which way the segment leaves its joint, and the nearer to the joint of two places.
            let (_, _, sx, sy) = QUADRANTS[CORNER[s % 8]];
            let sign = match s {
                0..8 if horizontal => sx,
                0..8 => sy,
                _ if s % 2 == 0 => -1.,
                _ => 1.,
            };
            let nearer = |a: f32, b: f32| if sign > 0. { a.min(b) } else { a.max(b) };
            let (near, far) = if s < 8 {
                let start = match (horizontal, sign > 0.) {
                    (true, true) => x0,
                    (true, false) => x1,
                    (false, true) => y0,
                    (false, false) => y1,
                };
                // A centred line reaches across the centred lines it meets at its corner point.
                let (row, col, direction) = corner(s);
                let turns = [1, 3].into_iter().filter(|_| stroke.centered);
                let met = turns.filter_map(|turn| self.arm(row, col, (direction + turn) % 4));
                let reach = met.fold(start, |reach, met| {
                    let (lo, t) = grid.centred(start, grid.thick(&met), horizontal);
                    nearer(reach, if sign > 0. { lo } else { lo + t })
                });
                // The other half's arc may reach past the middle into this one.
                let far = stops(s ^ 1).filter(|far| (far - middle) * sign < 0.).unwrap_or(middle);
                (stops(s).unwrap_or(reach), far)
            } else {
                let arcs = BESIDE[s - 8].into_iter().filter_map(|q| resumes(q, horizontal));
                // To the far side of the arms it meets at the centre.
                let met = if horizontal { [10, 11] } else { [8, 9] }.into_iter();
                let sides = met.filter_map(|s| lines[s]);
                let reach = sides.map(|(lo, t)| if sign > 0. { lo } else { lo + t }).reduce(nearer);
                let near = arcs.reduce(nearer).filter(|_| drawn[s ^ 1].is_none()).or(reach);
                (near.unwrap_or(middle), [x0, x1, y0, y1][s - 8])
            };
            if (far - near) * sign <= 0. {
                continue;
            }
            let (from, length) = (near.min(far), (far - near).abs());
            let shape = match horizontal {
                true => Shape::Rect { x: from, y: lo, w: length, h: t, alpha: 1. },
                false => Shape::Rect { x: lo, y: from, w: t, h: length, alpha: 1. },
            };
            let layer = if stroke.above { &mut out.above } else { &mut out.below };
            layer.push((shape, stroke.color));
        }

        for (q, round) in rounded.iter().enumerate() {
            let (Some(round), (h, v, sx, sy)) = (round, QUADRANTS[q]) else { continue };
            let (Some(stroke), Some((_, tall)), Some((_, wide))) = (drawn[h], lines[h], lines[v])
            else {
                continue;
            };
            let Round { x, y, r } = *round;
            let (to_x, to_y, k) = (x + sx * r, y + sy * r, r * (1. - KAPPA));
            let curve = Seg::Curve { to: (x, to_y), c1: (x + sx * k, y), c2: (x, y + sy * k) };
            // Each end runs straight on to the pixel its arm picks up from.
            let arc = vec![
                Seg::Move(away(to_x, sx), y),
                Seg::Line(to_x, y),
                curve,
                Seg::Line(x, away(to_y, sy)),
            ];
            let layer = if stroke.above { &mut out.above } else { &mut out.below };
            layer.push((Shape::Stroke(tall, arc), stroke.color));
            if let Some(outside) = set.outside {
                // From the outer corner the two lines would have made, in to the arc's middle
                // line: the arc is drawn over its edge.
                let (out_x, out_y) = (x - sx * wide / 2., y - sy * tall / 2.);
                let cut = vec![
                    Seg::Move(out_x, out_y),
                    Seg::Line(to_x, out_y),
                    Seg::Line(to_x, y),
                    curve,
                    Seg::Line(out_x, to_y),
                ];
                out.fills.push((Shape::Fill(cut), outside, [x0, y0, x1 - x0, y1 - y0]));
            }
        }
    }
}

/// The borders of the rows in view: `rows[r]` holds visible row `r`'s bordered cells in column
/// order. Rows top to bottom, cells left to right, a cell's segments in the protocol's order and
/// then its arcs: what comes later is painted over what came before.
pub fn layers(rows: &[&[(u16, Set)]], grid: &Grid) -> Layers {
    let frame = Frame { rows, grid };
    let mut out = Layers::default();
    for (row, cells) in rows.iter().enumerate() {
        for (col, set) in cells.iter() {
            frame.cell(row as i32, i32::from(*col), set, &mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOP: u16 = 3;
    const RIGHT: u16 = 12;
    const BOTTOM: u16 = 48;
    const LEFT: u16 = 192;

    /// A stroke `width` wide in a color the pictures show as the letter `name`.
    fn stroke(width: u8, name: char) -> Stroke {
        let color = Rgb(name as u8, 0, 0);
        Stroke { width, color, centered: false, above: false, priority: 0 }
    }

    fn centred(width: u8, name: char) -> Stroke {
        Stroke { centered: true, ..stroke(width, name) }
    }

    fn set(strokes: &[(u16, Stroke)]) -> Set {
        let segment = |bit: usize| strokes.iter().find(|(mask, _)| *mask & (1 << bit) != 0);
        let segments = array::from_fn(|bit| segment(bit).map(|(_, stroke)| *stroke));
        Set { segments, joints: 0, outside: None, radius: RADIUS }
    }

    /// The borders of `cells` (row, column, set) on a grid of cells 8 by 16 pixels.
    fn drawn(size: (u16, u16), cell: (f32, f32), cells: &[(u16, u16, Set)]) -> Layers {
        let mut rows = vec![Vec::new(); usize::from(size.0)];
        for (row, col, set) in cells {
            rows[usize::from(*row)].push((*col, set.clone()));
        }
        rows.iter_mut().for_each(|row| row.sort_by_key(|(col, _)| *col));
        let rows: Vec<&[(u16, Set)]> = rows.iter().map(Vec::as_slice).collect();
        layers(&rows, &Grid { origin: (0., 0.), cell, rows: size.0, cols: size.1 })
    }

    /// The pixels `x` by `y` as the rectangles paint them: each stroke's letter, the last one
    /// painted on top.
    fn picture(
        strokes: &[(Shape, Rgb)],
        x: std::ops::Range<i32>,
        y: std::ops::Range<i32>,
    ) -> Vec<String> {
        let at = |px: f32, py: f32| {
            let hit = strokes.iter().rev().find(|(shape, _)| match *shape {
                Shape::Rect { x, y, w, h, .. } => {
                    (x..x + w).contains(&px) && (y..y + h).contains(&py)
                }
                _ => false,
            });
            hit.map_or('.', |(_, color)| char::from(color.0))
        };
        y.map(|py| x.clone().map(|px| at(px as f32 + 0.5, py as f32 + 0.5)).collect()).collect()
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Shape {
        Shape::Rect { x, y, w, h, alpha: 1. }
    }

    #[test]
    fn inside_strokes_lie_flush_in_their_cell_and_close_its_corners() {
        let all = set(&[(TOP | RIGHT | BOTTOM | LEFT, stroke(16, 'a'))]);
        let layers = drawn((3, 3), (8., 16.), &[(1, 1, all)]);
        let mut want = vec!["a......a".to_string(); 16];
        (want[0], want[15]) = ("aaaaaaaa".into(), "aaaaaaaa".into());
        assert_eq!(picture(&layers.below, 8..16, 16..32), want);
        assert_eq!(picture(&layers.below, 0..24, 0..48).concat().matches('a').count(), 44);
        assert_eq!((layers.above, layers.fills), (vec![], vec![]));
        // A quarter of the cell's width at most, and each half on its own: the left half of the
        // top edge over the text, two pixels thick.
        let half = Stroke { above: true, ..stroke(64, 'b') };
        let layers = drawn((1, 1), (8., 16.), &[(0, 0, set(&[(1, half)]))]);
        assert_eq!(layers.above, [(rect(0., 0., 4., 2.), half.color)]);
    }

    #[test]
    fn neighbours_centred_strokes_on_a_shared_edge_draw_one_line() {
        let shared = |left: Stroke, right: Stroke| {
            let cells = [(0, 0, set(&[(RIGHT, left)])), (0, 1, set(&[(LEFT, right)]))];
            picture(&drawn((1, 2), (8., 16.), &cells).below, 6..10, 0..1).concat()
        };
        assert_eq!(shared(centred(16, 'a'), centred(16, 'b')), "..b.", "the right-hand cell's");
        let first = Stroke { priority: 1, ..centred(16, 'a') };
        assert_eq!(
            shared(first, centred(64, 'b')),
            "..a.",
            "the higher priority, whatever the width"
        );
        assert_eq!(shared(centred(64, 'a'), centred(16, 'b')), ".aa.", "else the wider");
        // Strokes inside their cells never merge, with each other or with a centred one.
        assert_eq!(shared(stroke(16, 'a'), stroke(16, 'b')), ".ab.");
        assert_eq!(shared(stroke(16, 'a'), centred(64, 'b')), ".bb.", "both, the later on top");
        // Below and above: the lower cell's on a tie.
        let cells =
            [(0, 0, set(&[(BOTTOM, centred(16, 'a'))])), (1, 0, set(&[(TOP, centred(16, 'b'))]))];
        let layers = drawn((2, 1), (8., 16.), &cells);
        assert_eq!(
            layers.below,
            [(rect(0., 16., 4., 1.), Rgb(b'b', 0, 0)), (rect(4., 16., 4., 1.), Rgb(b'b', 0, 0))]
        );
    }

    #[test]
    fn centred_strokes_close_their_corner_and_stay_on_the_grid() {
        let corner = set(&[(TOP | LEFT, centred(64, 'a'))]);
        let layers = drawn((3, 3), (8., 16.), &[(1, 1, corner)]);
        assert_eq!(
            picture(&layers.below, 6..13, 14..19),
            [".......", ".aaaaaa", ".aaaaaa", ".aa....", ".aa...."]
        );
        // On the grid's own edges a centred line keeps a pixel, on either side.
        let frame = set(&[(TOP | RIGHT | BOTTOM | LEFT, centred(16, 'a'))]);
        let layers = drawn((1, 1), (8., 16.), &[(0, 0, frame)]);
        let mut want = vec!["a......a".to_string(); 16];
        (want[0], want[15]) = ("aaaaaaaa".into(), "aaaaaaaa".into());
        assert_eq!(picture(&layers.below, 0..8, 0..16), want);
    }

    #[test]
    fn centre_arms_meet_where_box_drawing_characters_draw_theirs() {
        let cross = set(&[(768 | 3072, stroke(16, 'a'))]);
        let layers = drawn((1, 1), (5., 5.), &[(0, 0, cross)]);
        assert_eq!(
            picture(&layers.below, 0..5, 0..5),
            ["..a..", "..a..", "aaaaa", "..a..", "..a.."]
        );
        // └: the top arm and the right one, with no tail past the corner.
        let corner = set(&[(1024 | 512, stroke(16, 'a'))]);
        let layers = drawn((1, 1), (5., 5.), &[(0, 0, corner)]);
        assert_eq!(
            picture(&layers.below, 0..5, 0..5),
            ["..a..", "..a..", "..aaa", ".....", "....."]
        );
        // A quadrant's outline: half edges and centre arms.
        let quadrant = set(&[(1 | 64 | 256 | 1024, stroke(16, 'a'))]);
        let layers = drawn((1, 1), (6., 6.), &[(0, 0, quadrant)]);
        assert_eq!(
            picture(&layers.below, 0..6, 0..6),
            ["aaa...", "a.a...", "aaa...", "......", "......", "......"]
        );
    }

    fn curve(from: (f32, f32), to: (f32, f32), toward: (f32, f32)) -> Seg {
        let pull = |from: f32, toward: f32| from + (toward - from) * KAPPA;
        Seg::Curve {
            to,
            c1: (pull(from.0, toward.0), pull(from.1, toward.1)),
            c2: (pull(to.0, toward.0), pull(to.1, toward.1)),
        }
    }

    #[test]
    fn a_rounded_corner_is_an_arc_its_arms_stop_at_and_fills_what_it_cuts_off() {
        let lines = [(TOP | LEFT, stroke(16, 'a'))];
        let rounded = Set { joints: 1, outside: Some(Rgb(9, 9, 9)), ..set(&lines) };
        let layers = drawn((1, 1), (16., 32.), &[(0, 0, rounded.clone())]);
        let color = Rgb(b'a', 0, 0);
        // Half the cell's width: the arc takes the top edge's left half and runs into the right.
        let bend = curve((8.5, 0.5), (0.5, 8.5), (0.5, 0.5));
        let arc = vec![Seg::Move(9., 0.5), Seg::Line(8.5, 0.5), bend, Seg::Line(0.5, 9.)];
        assert_eq!(
            layers.below,
            [
                (rect(9., 0., 7., 1.), color),
                (rect(0., 9., 1., 7.), color),
                (rect(0., 16., 1., 16.), color),
                (Shape::Stroke(1., arc), color),
            ]
        );
        let cut = vec![
            Seg::Move(0., 0.),
            Seg::Line(8.5, 0.),
            Seg::Line(8.5, 0.5),
            bend,
            Seg::Line(0., 8.5),
        ];
        assert_eq!(layers.fills, [(Shape::Fill(cut), Rgb(9, 9, 9), [0., 0., 16., 32.])]);
        // A radius of its own, and one too long for the arms there are: with the top's right
        // half another stroke, the arc keeps to the left half.
        let radius = |layers: &Layers| match &layers.below.last().unwrap().0 {
            Shape::Stroke(_, arc) => match arc[1] {
                Seg::Line(x, _) => x - 0.5,
                _ => unreachable!(),
            },
            other => panic!("{other:?}"),
        };
        let small = Set { radius: 64, ..rounded.clone() };
        assert_eq!(radius(&drawn((1, 1), (16., 32.), &[(0, 0, small)])), 4.);
        let whole = Set { radius: 256, ..rounded.clone() };
        assert_eq!(radius(&drawn((1, 1), (16., 32.), &[(0, 0, whole)])), 15.5);
        let other = [(1 | LEFT, stroke(16, 'a')), (2, stroke(16, 'b'))];
        let split = Set { joints: 1, radius: 256, ..set(&other) };
        assert_eq!(radius(&drawn((1, 1), (16., 32.), &[(0, 0, split)])), 7.5);
        // Square until both arms are there, and with one inside and one centred.
        let lone = Set { joints: 1, ..set(&[(TOP, stroke(16, 'a'))]) };
        assert_eq!(drawn((1, 1), (16., 32.), &[(0, 0, lone)]).below.len(), 2);
        let mixed = Set { joints: 1, ..set(&[(TOP, stroke(16, 'a')), (LEFT, centred(16, 'a'))]) };
        assert_eq!(drawn((1, 1), (16., 32.), &[(0, 0, mixed)]).below.len(), 4);
    }

    #[test]
    fn a_fork_curves_the_stem_into_a_line_that_goes_straight_on() {
        let lines = [(768 | 2048, stroke(16, 'a'))];
        let fork = Set { joints: 32, ..set(&lines) };
        let layers = drawn((1, 1), (16., 32.), &[(0, 0, fork)]);
        let color = Rgb(b'a', 0, 0);
        let bend = curve((15.5, 15.5), (7.5, 23.5), (7.5, 15.5));
        let arc = vec![Seg::Move(16., 15.5), Seg::Line(15.5, 15.5), bend, Seg::Line(7.5, 24.)];
        assert_eq!(
            layers.below,
            [
                (rect(0., 15., 8., 1.), color),
                (rect(7., 15., 9., 1.), color),
                (rect(7., 24., 1., 8.), color),
                (Shape::Stroke(1., arc), color),
            ]
        );
        // With no line going on, the horizontal arm stops at the arc too.
        let turn = Set { joints: 32, ..set(&[(512 | 2048, stroke(16, 'a'))]) };
        let layers = drawn((1, 1), (16., 32.), &[(0, 0, turn)]);
        assert_eq!(
            layers.below[..2],
            [(rect(7., 24., 1., 8.), color), (layers.below[1].0.clone(), color)]
        );
        assert_eq!(
            layers.below.len(),
            2,
            "the arc reaches the cell's edge: nothing is left of the arm"
        );
    }

    #[test]
    fn a_centred_corner_rounds_only_with_its_own_cells_strokes() {
        let rounded = |priority: u8| {
            let corner = Stroke { priority, ..centred(16, 'a') };
            let cells = [
                (1, 1, Set { joints: 1, ..set(&[(TOP | LEFT, corner)]) }),
                (0, 1, set(&[(BOTTOM, centred(64, 'b'))])),
            ];
            let layers = drawn((3, 3), (16., 32.), &cells);
            layers.below.iter().filter(|(shape, _)| matches!(shape, Shape::Stroke(..))).count()
        };
        assert_eq!(rounded(0), 0, "the cell above's wider line is the top edge");
        assert_eq!(rounded(1), 1);
        // The line going on past the corner point makes it a fork: the top edge runs to it.
        let cells = [
            (1, 1, Set { joints: 1, ..set(&[(TOP | LEFT, centred(16, 'a'))]) }),
            (1, 0, set(&[(TOP, centred(16, 'a'))])),
        ];
        let layers = drawn((3, 3), (16., 32.), &cells);
        let top = picture(&layers.below, 0..32, 32..33).concat();
        assert_eq!(top, "a".repeat(32));
        let left = picture(&layers.below, 16..17, 32..64).concat();
        assert_eq!(
            left,
            format!("a{}{}", ".".repeat(8), "a".repeat(23)),
            "the stem stops at its arc"
        );
    }
}
