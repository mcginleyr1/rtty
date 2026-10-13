//! Characters rtty draws itself instead of taking from a font: box drawing (U+2500–257F), block
//! elements (U+2580–259F) and the powerline separators (U+E0B0–E0BF). Each fills its cell exactly,
//! so lines meet the next cell's and blocks tile, whatever the font's metrics; a font's own stop
//! short of the cell or run past it, and most fonts (Menlo among them) have no powerline glyphs.
//!
//! Shapes are in the cell's own device pixels, the cell's size whole pixels, so that edges land
//! on pixels. Drawn from the Unicode code charts' descriptions.

/// A piece of a drawn character.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// A rectangle filled at `alpha` of the text's color (the shades are partial).
    Rect { x: f32, y: f32, w: f32, h: f32, alpha: f32 },
    /// A filled outline.
    Fill(Vec<Seg>),
    /// A line along the path, this thick.
    Stroke(f32, Vec<Seg>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    /// A cubic Bézier to the point, by its two control points.
    Curve {
        to: (f32, f32),
        c1: (f32, f32),
        c2: (f32, f32),
    },
}

/// Whether rtty draws `c` itself.
pub fn drawn(c: char) -> bool {
    matches!(c, '\u{2500}'..='\u{259F}' | '\u{E0B0}'..='\u{E0BF}')
}

/// How far a quarter circle's control points sit along its tangents, as a share of its radius.
const KAPPA: f32 = 0.552_284_8;

/// Line weights in the four directions; light is 1.
const NONE: u8 = 0;
const HEAVY: u8 = 2;
const DOUBLE: u8 = 3;

/// The lines of U+2500–257F that are straight arms from the middle to the edges: their weights
/// up, right, down and left, as digits (0 none, 1 light, 2 heavy, 3 double). Dashes, arcs and
/// diagonals are `None`.
fn arms(c: char) -> Option<[u8; 4]> {
    const TABLE: [&str; 128] = [
        "0101", "0202", "1010", "2020", "", "", "", "", "", "", "", "", // 2500–250B
        "0110", "0210", "0120", "0220", "0011", "0012", "0021", "0022", // 250C–2513
        "1100", "1200", "2100", "2200", "1001", "1002", "2001", "2002", // 2514–251B
        "1110", "1210", "2110", "1120", "2120", "2210", "1220", "2220", // 251C–2523
        "1011", "1012", "2011", "1021", "2021", "2012", "1022", "2022", // 2524–252B
        "0111", "0112", "0211", "0212", "0121", "0122", "0221", "0222", // 252C–2533
        "1101", "1102", "1201", "1202", "2101", "2102", "2201", "2202", // 2534–253B
        "1111", "1112", "1211", "1212", "2111", "1121", "2121", "2112", // 253C–2543
        "2211", "1122", "1221", "2212", "1222", "2122", "2221", "2222", // 2544–254B
        "", "", "", "", // 254C–254F
        "0303", "3030", "0310", "0130", "0330", "0013", "0031", "0033", // 2550–2557
        "1300", "3100", "3300", "1003", "3001", "3003", "1310", "3130", // 2558–255F
        "3330", "1013", "3031", "3033", "0313", "0131", "0333", "1303", // 2560–2567
        "3101", "3303", "1313", "3131", "3333", "", "", "", // 2568–256F
        "", "", "", "", "0001", "1000", "0100", "0010", // 2570–2577
        "0002", "2000", "0200", "0020", "0201", "1020", "0102", "2010", // 2578–257F
    ];
    let index = (c as u32).checked_sub(0x2500).filter(|&index| index < 128)?;
    let digits = TABLE[index as usize].as_bytes();
    (digits.len() == 4).then(|| [0, 1, 2, 3].map(|arm| digits[arm] - b'0'))
}

/// What to paint for `c` in a `w`×`h` cell, lines `light` thick and heavy ones twice that; empty
/// for a character rtty does not draw.
pub fn shapes(c: char, w: f32, h: f32, light: f32) -> Vec<Shape> {
    if let Some(arms) = arms(c) {
        return lines(arms, w, h, light);
    }
    let rect = |x: f32, y: f32, rw: f32, rh: f32| Shape::Rect { x, y, w: rw, h: rh, alpha: 1. };
    let (mid_x, mid_y) = ((w / 2.).round(), (h / 2.).round());
    let eighths = |n: u32, of: f32| (of * n as f32 / 8.).round();
    let code = c as u32;
    match c {
        // Dashed lines: triple, quadruple and double, light and heavy, across and down.
        '\u{2504}'..='\u{250B}' | '\u{254C}'..='\u{254F}' => {
            let (dashes, rest) = match code {
                0x2504..=0x2507 => (3, code - 0x2504),
                0x2508..=0x250B => (4, code - 0x2508),
                _ => (2, code - 0x254C),
            };
            let thick = if rest % 2 == 1 { light * 2. } else { light };
            dashed(dashes, rest >= 2, w, h, thick)
        }
        '\u{256D}'..='\u{2570}' => vec![arc(code - 0x256D, w, h, light)],
        '\u{2571}' => vec![Shape::Stroke(light, vec![Seg::Move(w, 0.), Seg::Line(0., h)])],
        '\u{2572}' => vec![Shape::Stroke(light, vec![Seg::Move(0., 0.), Seg::Line(w, h)])],
        '\u{2573}' => [shapes('\u{2571}', w, h, light), shapes('\u{2572}', w, h, light)].concat(),
        '\u{2580}' => vec![rect(0., 0., w, mid_y)],
        '\u{2581}'..='\u{2588}' => {
            let tall = eighths(code - 0x2580, h);
            vec![rect(0., h - tall, w, tall)]
        }
        '\u{2589}'..='\u{258F}' => vec![rect(0., 0., eighths(0x2590 - code, w), h)],
        '\u{2590}' => vec![rect(mid_x, 0., w - mid_x, h)],
        '\u{2591}'..='\u{2593}' => {
            let alpha = (code - 0x2590) as f32 / 4.;
            vec![Shape::Rect { x: 0., y: 0., w, h, alpha }]
        }
        '\u{2594}' => vec![rect(0., 0., w, eighths(1, h))],
        '\u{2595}' => {
            let wide = eighths(1, w);
            vec![rect(w - wide, 0., wide, h)]
        }
        '\u{2596}'..='\u{259F}' => {
            // Upper left, upper right, lower left, lower right.
            let quadrants = [
                rect(0., 0., mid_x, mid_y),
                rect(mid_x, 0., w - mid_x, mid_y),
                rect(0., mid_y, mid_x, h - mid_y),
                rect(mid_x, mid_y, w - mid_x, h - mid_y),
            ];
            const LIT: [u8; 10] =
                [0b0100, 0b1000, 0b0001, 0b1101, 0b1001, 0b0111, 0b1011, 0b0010, 0b0110, 0b1110];
            let lit = LIT[(code - 0x2596) as usize];
            let quadrants = quadrants.into_iter().enumerate();
            quadrants.filter(|(index, _)| lit & (1 << index) != 0).map(|(_, rect)| rect).collect()
        }
        '\u{E0B0}'..='\u{E0BF}' => powerline(code - 0xE0B0, w, h, light),
        _ => Vec::new(),
    }
}

/// Straight arms from the middle to the edges, with weights `[up, right, down, left]`. Where
/// arms meet, a single line reaches across the lines it meets; a double line's two strokes turn
/// corners inside each other, run on where the line goes straight on, and stop at a single line.
fn lines([up, right, down, left]: [u8; 4], w: f32, h: f32, t: f32) -> Vec<Shape> {
    let thick = |weight: u8| if weight == HEAVY { t * 2. } else { t };
    // Where a single line of that weight, or a double line's first stroke, starts across an axis.
    let single = |weight: u8, across: f32| ((across - thick(weight)) / 2.).floor();
    let double = |across: f32| ((across - 3. * t) / 2.).floor();
    let (dx, dy) = (double(w), double(h));
    let (mid_x, mid_y) = ((w / 2.).floor(), (h / 2.).floor());
    let is = |weight: u8| weight != NONE;
    let lone = |weight: u8| is(weight) && weight != DOUBLE;
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Shape::Rect {
        x: x0,
        y: y0,
        w: (x1 - x0).max(0.),
        h: (y1 - y0).max(0.),
        alpha: 1.,
    };

    // The extent of the junction across each axis: the lines that meet there.
    let edges = |arms: [u8; 2], across: f32, d: f32| {
        let spans = arms.into_iter().filter(|&weight| is(weight)).map(|weight| match weight {
            DOUBLE => (d, d + 3. * t),
            weight => (single(weight, across), single(weight, across) + thick(weight)),
        });
        spans.fold(None, |span: Option<(f32, f32)>, (from, to)| {
            Some(span.map_or((from, to), |(a, b)| (a.min(from), b.max(to))))
        })
    };
    let rows = edges([left, right], h, dy);
    let cols = edges([up, down], w, dx);
    // A single line that meets doubles on one side only turns into the far stroke; between
    // doubles on both sides it stops at the near one.
    let horizontal_doubles = u8::from(left == DOUBLE) + u8::from(right == DOUBLE);
    let vertical_doubles = u8::from(up == DOUBLE) + u8::from(down == DOUBLE);

    let mut shapes = Vec::new();
    if lone(up) {
        let x = single(up, w);
        let end = match (horizontal_doubles, is(down)) {
            (2, false) => dy + t,
            (1, false) => dy + 3. * t,
            _ => rows.map_or(single(up, h) + thick(up), |(_, bottom)| bottom),
        };
        shapes.push(rect(x, 0., x + thick(up), end));
    }
    if lone(down) {
        let x = single(down, w);
        let start = match (horizontal_doubles, is(up)) {
            (2, false) => dy + 2. * t,
            (1, false) => dy,
            _ => rows.map_or(single(down, h), |(top, _)| top),
        };
        shapes.push(rect(x, start, x + thick(down), h));
    }
    if lone(left) {
        let y = single(left, h);
        let end = match (vertical_doubles, is(right)) {
            (2, false) => dx + t,
            (1, false) => dx + 3. * t,
            _ => cols.map_or(single(left, w) + thick(left), |(_, right_edge)| right_edge),
        };
        shapes.push(rect(0., y, end, y + thick(left)));
    }
    if lone(right) {
        let y = single(right, h);
        let start = match (vertical_doubles, is(left)) {
            (2, false) => dx + 2. * t,
            (1, false) => dx,
            _ => cols.map_or(single(right, w), |(left_edge, _)| left_edge),
        };
        shapes.push(rect(start, y, w, y + thick(right)));
    }

    // A double arm's stroke on the `near` side of the junction (toward the arm `side`), and the
    // far one: where each ends, by what else meets there.
    let bottom_of_singles = |arms: [u8; 2], across: f32| {
        let lone_arms = arms.into_iter().filter(|&weight| lone(weight));
        lone_arms.map(|weight| single(weight, across) + thick(weight)).reduce(f32::max)
    };
    let top_of_singles = |arms: [u8; 2], across: f32| {
        let lone_arms = arms.into_iter().filter(|&weight| lone(weight));
        lone_arms.map(|weight| single(weight, across)).reduce(f32::min)
    };
    // An arm's stroke nearer the arm `turn` ends at the inner corner when `turn` is double, goes
    // straight on when the opposite arm is, reaches the outer corner when the other side's arm
    // is, and meets a single line where one crosses.
    let reach =
        |turn: u8, straight: u8, other: u8, inner: f32, outer: f32, singles: Option<f32>| {
            if turn == DOUBLE {
                inner
            } else if straight == DOUBLE {
                f32::NAN
            } else if other == DOUBLE {
                outer
            } else {
                singles.unwrap_or(f32::NAN)
            }
        };
    if up == DOUBLE {
        let singles = bottom_of_singles([left, right], h);
        for (x, turn, other) in [(dx, left, right), (dx + 2. * t, right, left)] {
            let end = reach(turn, down, other, dy + t, dy + 3. * t, singles);
            shapes.push(rect(x, 0., x + t, if end.is_nan() { mid_y } else { end }));
        }
    }
    if down == DOUBLE {
        let singles = top_of_singles([left, right], h);
        for (x, turn, other) in [(dx, left, right), (dx + 2. * t, right, left)] {
            let start = reach(turn, up, other, dy + 2. * t, dy, singles);
            shapes.push(rect(x, if start.is_nan() { mid_y } else { start }, x + t, h));
        }
    }
    if left == DOUBLE {
        let singles = bottom_of_singles([up, down], w);
        for (y, turn, other) in [(dy, up, down), (dy + 2. * t, down, up)] {
            let end = reach(turn, right, other, dx + t, dx + 3. * t, singles);
            shapes.push(rect(0., y, if end.is_nan() { mid_x } else { end }, y + t));
        }
    }
    if right == DOUBLE {
        let singles = top_of_singles([up, down], w);
        for (y, turn, other) in [(dy, up, down), (dy + 2. * t, down, up)] {
            let start = reach(turn, left, other, dx + 2. * t, dx, singles);
            shapes.push(rect(if start.is_nan() { mid_x } else { start }, y, w, y + t));
        }
    }
    shapes
}

/// `count` dashes across the cell (or down it), each two thirds of its share.
fn dashed(count: u32, down: bool, w: f32, h: f32, thick: f32) -> Vec<Shape> {
    let (along, across) = if down { (h, w) } else { (w, h) };
    let offset = ((across - thick) / 2.).floor();
    let share = along / count as f32;
    (0..count)
        .map(|index| {
            let from = (share * index as f32 + share / 6.).round();
            let to = (share * (index + 1) as f32 - share / 6.).round();
            let (x, y, rw, rh) = match down {
                false => (from, offset, to - from, thick),
                true => (offset, from, thick, to - from),
            };
            Shape::Rect { x, y, w: rw, h: rh, alpha: 1. }
        })
        .collect()
}

/// ╭ ╮ ╯ ╰: a light line from one edge's middle to another's, round the corner.
fn arc(which: u32, w: f32, h: f32, t: f32) -> Shape {
    let (cx, cy) = (((w - t) / 2.).floor() + t / 2., ((h - t) / 2.).floor() + t / 2.);
    let r = (w / 2.).min(h / 2.);
    // Toward the right (or left) edge, and toward the bottom (or top).
    let (sx, sy) = match which {
        0 => (1., 1.),
        1 => (-1., 1.),
        2 => (-1., -1.),
        _ => (1., -1.),
    };
    let edge_x = if sx > 0. { w } else { 0. };
    let edge_y = if sy > 0. { h } else { 0. };
    let k = r * (1. - KAPPA);
    Shape::Stroke(
        t,
        vec![
            Seg::Move(edge_x, cy),
            Seg::Line(cx + sx * r, cy),
            Seg::Curve { to: (cx, cy + sy * r), c1: (cx + sx * k, cy), c2: (cx, cy + sy * k) },
            Seg::Line(cx, edge_y),
        ],
    )
}

/// U+E0B0–E0BF: the powerline separators, each across the whole cell.
fn powerline(which: u32, w: f32, h: f32, t: f32) -> Vec<Shape> {
    let mid = h / 2.;
    let polygon = |points: &[(f32, f32)]| {
        let mut path = vec![Seg::Move(points[0].0, points[0].1)];
        path.extend(points[1..].iter().map(|&(x, y)| Seg::Line(x, y)));
        path
    };
    // A half ellipse bulging away from the edge at `from` to the other, through its middle.
    let half_ellipse = |from: f32, to: f32| {
        let bulge = (to - from) * KAPPA;
        vec![
            Seg::Move(from, 0.),
            Seg::Curve { to: (to, mid), c1: (from + bulge, 0.), c2: (to, mid - mid * KAPPA) },
            Seg::Curve { to: (from, h), c1: (to, mid + mid * KAPPA), c2: (from + bulge, h) },
        ]
    };
    let shape = match which {
        0x0 => Shape::Fill(polygon(&[(0., 0.), (w, mid), (0., h)])),
        0x1 => Shape::Stroke(t, polygon(&[(0., 0.), (w, mid), (0., h)])),
        0x2 => Shape::Fill(polygon(&[(w, 0.), (0., mid), (w, h)])),
        0x3 => Shape::Stroke(t, polygon(&[(w, 0.), (0., mid), (w, h)])),
        0x4 => Shape::Fill(half_ellipse(0., w)),
        0x5 => Shape::Stroke(t, half_ellipse(0., w)),
        0x6 => Shape::Fill(half_ellipse(w, 0.)),
        0x7 => Shape::Stroke(t, half_ellipse(w, 0.)),
        0x8 => Shape::Fill(polygon(&[(0., 0.), (w, h), (0., h)])),
        0xA => Shape::Fill(polygon(&[(w, 0.), (w, h), (0., h)])),
        0xC => Shape::Fill(polygon(&[(0., 0.), (w, 0.), (0., h)])),
        0xE => Shape::Fill(polygon(&[(0., 0.), (w, 0.), (w, h)])),
        0x9 | 0xF => Shape::Stroke(t, polygon(&[(0., 0.), (w, h)])),
        _ => Shape::Stroke(t, polygon(&[(0., h), (w, 0.)])),
    };
    vec![shape]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which pixels of a `w`×`h` cell the rectangles cover, as rows of `#` and `.`.
    fn pixels(c: char, w: f32, h: f32, t: f32) -> Vec<String> {
        let shapes = shapes(c, w, h, t);
        (0..h as usize)
            .map(|y| {
                (0..w as usize)
                    .map(|x| {
                        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                        let hit = shapes.iter().any(|shape| match *shape {
                            Shape::Rect { x, y, w, h, .. } => {
                                (x..x + w).contains(&px) && (y..y + h).contains(&py)
                            }
                            _ => false,
                        });
                        if hit { '#' } else { '.' }
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn rtty_draws_box_drawing_blocks_and_powerline_separators() {
        assert!(drawn('─') && drawn('▟') && drawn('\u{E0B0}') && drawn('\u{E0BF}'));
        assert!(!drawn('a') && !drawn('\u{E0A0}') && !drawn('\u{E0C0}') && !drawn('\u{25A0}'));
        let every = ('\u{2500}'..='\u{259F}').chain('\u{E0B0}'..='\u{E0BF}');
        for c in every {
            assert!(!shapes(c, 8., 16., 1.).is_empty(), "{c:?} draws nothing");
        }
    }

    #[test]
    fn light_lines_cross_the_cell_edge_to_edge_through_its_middle() {
        assert_eq!(
            pixels('┼', 5., 5., 1.),
            ["..#..", "..#..", "#####", "..#..", "..#.."].map(String::from)
        );
        assert_eq!(pixels('└', 5., 5., 1.), ["..#..", "..#..", "..###", ".....", "....."]);
        assert_eq!(pixels('╴', 5., 5., 1.), [".....", ".....", "###..", ".....", "....."]);
    }

    #[test]
    fn heavy_lines_are_twice_as_thick_and_a_light_line_reaches_across_them() {
        assert_eq!(
            pixels('┝', 6., 6., 1.),
            ["..#...", "..#...", "..####", "..####", "..#...", "..#..."].map(String::from)
        );
    }

    #[test]
    fn double_lines_turn_corners_inside_each_other_and_cross_open() {
        assert_eq!(
            pixels('╔', 7., 7., 1.),
            [".......", ".......", "..#####", "..#....", "..#.###", "..#.#..", "..#.#.."]
                .map(String::from)
        );
        assert_eq!(
            pixels('╬', 7., 7., 1.),
            ["..#.#..", "..#.#..", "###.###", ".......", "###.###", "..#.#..", "..#.#.."]
                .map(String::from)
        );
        assert_eq!(
            pixels('╦', 7., 7., 1.),
            [".......", ".......", "#######", ".......", "###.###", "..#.#..", "..#.#.."]
                .map(String::from)
        );
        // A single line hangs from the near stroke between doubles, turns into the far one at a
        // corner, and crosses both where it goes on.
        assert_eq!(
            pixels('╤', 7., 7., 1.),
            [".......", ".......", "#######", ".......", "#######", "...#...", "...#..."]
                .map(String::from)
        );
        assert_eq!(
            pixels('╕', 7., 7., 1.),
            [".......", ".......", "####...", "...#...", "####...", "...#...", "...#..."]
                .map(String::from)
        );
        assert_eq!(
            pixels('╪', 7., 7., 1.),
            ["...#...", "...#...", "#######", "...#...", "#######", "...#...", "...#..."]
                .map(String::from)
        );
    }

    #[test]
    fn blocks_fill_their_share_of_the_cell_and_tile() {
        assert_eq!(pixels('█', 3., 4., 1.), ["###"; 4]);
        assert_eq!(pixels('▀', 3., 4., 1.), ["###", "###", "...", "..."]);
        assert_eq!(pixels('▂', 2., 8., 1.), ["..", "..", "..", "..", "..", "..", "##", "##"]);
        assert_eq!(pixels('▐', 4., 2., 1.), ["..##", "..##"]);
        assert_eq!(pixels('▚', 4., 4., 1.), ["##..", "##..", "..##", "..##"]);
        assert_eq!(pixels('▟', 4., 4., 1.), ["..##", "..##", "####", "####"]);
        assert_eq!(
            shapes('▒', 3., 4., 1.),
            [Shape::Rect { x: 0., y: 0., w: 3., h: 4., alpha: 0.5 }]
        );
    }

    #[test]
    fn dashes_split_the_line_with_gaps() {
        assert_eq!(pixels('┄', 12., 3., 1.), ["............", ".##..##..##.", "............"]);
    }

    #[test]
    fn powerline_separators_span_the_whole_cell() {
        let points = |shape: &Shape| match shape {
            Shape::Fill(path) | Shape::Stroke(_, path) => path
                .iter()
                .map(|seg| match *seg {
                    Seg::Move(x, y) | Seg::Line(x, y) => (x, y),
                    Seg::Curve { to, .. } => to,
                })
                .collect::<Vec<_>>(),
            Shape::Rect { .. } => Vec::new(),
        };
        let solid = shapes('\u{E0B0}', 8., 16., 1.);
        assert!(matches!(solid[0], Shape::Fill(_)));
        assert_eq!(points(&solid[0]), [(0., 0.), (8., 8.), (0., 16.)]);
        let round = shapes('\u{E0B6}', 8., 16., 1.);
        assert_eq!(points(&round[0]), [(8., 0.), (0., 8.), (8., 16.)]);
        assert!(matches!(shapes('\u{E0B5}', 8., 16., 1.)[0], Shape::Stroke(1., _)));
        assert_eq!(points(&shapes('\u{E0BC}', 8., 16., 1.)[0]), [(0., 0.), (8., 0.), (0., 16.)]);
    }
}
