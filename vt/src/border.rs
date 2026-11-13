//! Cell borders: lines on the edges and centrelines of cells that take no
//! cells of their own, as the cell-border protocol has them (draft, version
//! 1: `DCS 1 + b cell-border ; … ST`). A cell's borders are part of it as its
//! background is: the pen's go to every cell written or erased, and they move,
//! scroll and reflow with their cells. The engine stores them; a host draws
//! them, merging the strokes two cells put on the edge they share.

use std::fmt::Write;
use std::ops::RangeInclusive;
use std::sync::Arc;

use crate::cell::Color;
use crate::term::Terminal;

/// The line of one segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Stroke {
    /// Thickness in 1/256 of the cell's width, 1 to 64.
    pub width: u8,
    /// `Color::Default` is the default foreground (`fg`), `Color::Indexed` a
    /// palette colour (`pN`).
    pub color: Color,
    /// Centred on the cell's edge, where the neighbour's centred stroke
    /// merges with it; else inside the cell. Nothing to a centre arm.
    pub centered: bool,
    /// Drawn over the text, else under it.
    pub above: bool,
    /// Of two centred strokes on one edge the higher is drawn, 0 to 15.
    pub priority: u8,
}

/// A cell's borders, or the pen's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Borders {
    /// The twelve segments, by the protocol's bit: the outer edges' halves
    /// `TOP_LEFT` (0), `TOP_RIGHT`, `RIGHT_TOP`, `RIGHT_BOTTOM`,
    /// `BOTTOM_LEFT`, `BOTTOM_RIGHT`, `LEFT_TOP`, `LEFT_BOTTOM`, then the
    /// centre arms `H_LEFT` (8), `H_RIGHT`, `V_TOP`, `V_BOTTOM`.
    pub segments: [Option<Stroke>; 12],
    /// The rounded joints, by the protocol's bit: the corners `TOP_LEFT` (0),
    /// `TOP_RIGHT`, `BOTTOM_RIGHT`, `BOTTOM_LEFT`, then the centre's quadrants
    /// `TOP_RIGHT` (4), `BOTTOM_RIGHT`, `BOTTOM_LEFT`, `TOP_LEFT`.
    pub joints: u8,
    /// The colour of what a rounded joint cuts off; `Color::Default` is the
    /// default background.
    pub outside: Option<Color>,
    /// The rounded joints' radius in 1/256 of the cell's width, 1 to 256; the
    /// host's own while `None`.
    pub radius: Option<u16>,
}

/// The longest payload read.
const PAYLOAD_MAX: usize = 256;

/// What `q` is answered with: the version, then what a host is given to draw.
/// `style` (double, dashed and dotted lines) is not kept.
const SUPPORT: &str =
    "\x1bP1+bcell-border;ok;1;pen,rect,half,center,palette,round,priority,outside,radius\x1b\\";

/// The right-hand and left-hand edges' segments, and those edges' corners.
const RIGHT: (u16, u8) = (0b1100, 0b0110);
const LEFT: (u16, u8) = (0b1100_0000, 0b1001);

/// What a command does to a border set.
enum Change {
    /// Give the masked segments this stroke, or none.
    Segments(u16, Option<Stroke>),
    /// Round or square the masked joints; an outside colour and a radius
    /// stay unless given.
    Joints { mask: u8, round: bool, outside: Option<Color>, radius: Option<u16> },
}

impl Borders {
    fn apply(&mut self, change: &Change) {
        match *change {
            Change::Segments(mask, stroke) => {
                let masked = (0..12).filter(|bit| mask & (1 << bit) != 0);
                masked.for_each(|bit| self.segments[bit] = stroke);
            }
            Change::Joints { mask, round, outside, radius } => {
                self.joints = if round { self.joints | mask } else { self.joints & !mask };
                self.outside = outside.or(self.outside);
                self.radius = radius.or(self.radius);
            }
        }
    }
}

/// `borders` after `change`; none when nothing is left of them.
fn changed(borders: &Option<Arc<Borders>>, change: &Change) -> Option<Arc<Borders>> {
    let mut borders = borders.as_deref().cloned().unwrap_or_default();
    borders.apply(change);
    (borders != Borders::default()).then(|| Arc::new(borders))
}

/// The borders the two cells of a wide character take from `borders`: the
/// first without its right-hand edge and corners, the second without its
/// left-hand ones, so that no line runs through the glyph.
pub(crate) fn halves(borders: &Option<Arc<Borders>>) -> [Option<Arc<Borders>>; 2] {
    [RIGHT, LEFT].map(|(segments, joints)| {
        let mut half = borders.as_deref()?.clone();
        half.apply(&Change::Segments(segments, None));
        half.joints &= !joints;
        (half != Borders::default()).then(|| Arc::new(half))
    })
}

/// The borders that, printed as a wide character's, leave its two cells
/// with `first` and `second`: what `halves` cut from each comes from the
/// other, and the rest is the first's.
pub(crate) fn whole(
    first: &Option<Arc<Borders>>,
    second: &Option<Arc<Borders>>,
) -> Option<Arc<Borders>> {
    let right = second.as_deref().cloned().unwrap_or_default();
    let mut whole = first.as_deref().cloned().unwrap_or_default();
    for bit in (0..12).filter(|bit| RIGHT.0 & (1 << bit) != 0) {
        whole.segments[bit] = right.segments[bit];
    }
    whole.joints = whole.joints & !RIGHT.1 | right.joints & RIGHT.1;
    (whole != Borders::default()).then(|| Arc::new(whole))
}

/// Append the commands that bring an empty pen to `borders`.
pub(crate) fn pen(out: &mut String, borders: &Borders) {
    let color = |color: Color, default: &str| match color {
        Color::Default => default.to_string(),
        Color::Named(n) | Color::Indexed(n) => format!("p{n}"),
        Color::Rgb(r, g, b) => format!("{r:02x}{g:02x}{b:02x}"),
    };
    let mut left: u16 =
        (0..12).filter(|&bit| borders.segments[bit].is_some()).map(|bit| 1 << bit).sum();
    while left != 0 {
        let stroke = borders.segments[left.trailing_zeros() as usize];
        let mask: u16 =
            (0..12).filter(|&bit| borders.segments[bit] == stroke).map(|bit| 1 << bit).sum();
        let stroke = stroke.expect("a segment that is set");
        let _ = write!(
            out,
            "\x1bP1+bcell-border;pen;{mask};{};{};{};{}",
            stroke.width,
            color(stroke.color, "fg"),
            u8::from(stroke.centered),
            u8::from(stroke.above),
        );
        if stroke.priority != 0 {
            let _ = write!(out, ";priority={}", stroke.priority);
        }
        out.push_str("\x1b\\");
        left &= !mask;
    }
    if borders.joints != 0 || borders.outside.is_some() || borders.radius.is_some() {
        // Squaring every joint of an empty pen changes nothing but what it carries.
        let _ = match borders.joints {
            0 => write!(out, "\x1bP1+bcell-border;pen-square;255"),
            joints => write!(out, "\x1bP1+bcell-border;pen-round;{joints}"),
        };
        if let Some(outside) = borders.outside {
            let _ = write!(out, ";outside={}", color(outside, "default"));
        }
        if let Some(radius) = borders.radius {
            let _ = write!(out, ";radius={radius}");
        }
        out.push_str("\x1b\\");
    }
}

/// A decimal field: digits only, in `range`.
fn decimal(field: &str, range: RangeInclusive<u32>) -> Option<u32> {
    let digits = !field.is_empty() && field.bytes().all(|b| b.is_ascii_digit());
    field.parse().ok().filter(|value| digits && range.contains(value))
}

/// `pN`, or six hex digits.
fn color(field: &str) -> Option<Color> {
    if let Some(index) = field.strip_prefix('p') {
        return decimal(index, 0..=255).map(|index| Color::Indexed(index as u8));
    }
    let hex = field.len() == 6 && field.bytes().all(|b| b.is_ascii_hexdigit());
    let rgb = u32::from_str_radix(field, 16).ok().filter(|_| hex)?;
    Some(Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8))
}

/// `WIDTH;COLOR;PLACEMENT;LAYER`.
fn stroke(fields: &[&str]) -> Option<Stroke> {
    Some(Stroke {
        width: decimal(fields[0], 1..=64)? as u8,
        color: if fields[1] == "fg" { Color::Default } else { color(fields[1])? },
        centered: decimal(fields[2], 0..=1)? == 1,
        above: decimal(fields[3], 0..=1)? == 1,
        priority: 0,
    })
}

/// A command's change and the rectangle it is for (rows, columns), the pen
/// when none; `None` for a malformed one.
fn parse(op: &str, fields: &[&str]) -> Option<(Change, Option<(u16, u16)>)> {
    let (positional, extended) = match op {
        "pen" => (5, true),
        "set" => (7, true),
        "pen-clear" => (1, false),
        "clear" => (3, false),
        "pen-round" | "pen-square" => (1, true),
        "round" | "square" => (3, true),
        _ => return None,
    };
    if fields.len() < positional || !extended && fields.len() > positional {
        return None;
    }
    let (fields, keys) = fields.split_at(positional);
    // `key=value`, letters and dashes to letters, digits and dashes: a field of any other shape
    // spoils the command, and a key or value not known is passed over.
    let keys: Vec<(&str, &str)> = keys
        .iter()
        .map(|field| {
            let (key, value) = field.split_once('=')?;
            let key_ok =
                !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'-');
            let value_ok = !value.is_empty()
                && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            (key_ok && value_ok).then_some((key, value))
        })
        .collect::<Option<_>>()?;
    let rect = match positional {
        1 | 5 => None,
        n => {
            let size = |field: &str| decimal(field, 1..=65535).map(|size| size as u16);
            Some((size(fields[n - 2])?, size(fields[n - 1])?))
        }
    };
    let change = match op {
        "pen" | "set" => {
            let mut stroke = stroke(&fields[1..5])?;
            for (_, value) in keys.iter().filter(|(key, _)| *key == "priority") {
                stroke.priority = decimal(value, 0..=15).map_or(stroke.priority, |p| p as u8);
            }
            Change::Segments(decimal(fields[0], 1..=4095)? as u16, Some(stroke))
        }
        "pen-clear" | "clear" => Change::Segments(decimal(fields[0], 1..=4095)? as u16, None),
        _ => {
            let (mut outside, mut radius) = (None, None);
            for &(key, value) in &keys {
                match key {
                    "outside" if value == "default" => outside = Some(Color::Default),
                    "outside" => outside = color(value).or(outside),
                    "radius" => radius = decimal(value, 1..=256).map(|r| r as u16).or(radius),
                    _ => {}
                }
            }
            let mask = decimal(fields[0], 1..=255)? as u8;
            Change::Joints { mask, round: op.ends_with("round"), outside, radius }
        }
    };
    Some((change, rect))
}

impl Terminal {
    /// With `on`, the terminal takes the cell-border protocol: it answers
    /// the capability query and keeps the borders programs give the pen and
    /// the cells, for the host to draw (`Row::borders`). Off, the default,
    /// the commands are dropped unanswered, as a terminal without the
    /// protocol drops them, and a program falls back to box-drawing
    /// characters; borders kept before stay on their cells.
    pub fn set_cell_borders(&mut self, on: bool) {
        self.cell_borders = on;
    }

    /// The borders the next cell written or erased takes.
    pub fn border_pen(&self) -> Option<&Borders> {
        self.cursor.pen.borders.as_deref()
    }

    /// A `DCS 1 + b` string's payload. A malformed command does nothing.
    pub(crate) fn border_command(&mut self, payload: &[u8]) {
        let Some(payload) = std::str::from_utf8(payload).ok().filter(|p| p.len() <= PAYLOAD_MAX)
        else {
            return;
        };
        let mut fields = payload.split(';');
        if fields.next() != Some("cell-border") {
            return;
        }
        let op = fields.next().unwrap_or_default();
        let fields: Vec<&str> = fields.collect();
        if op == "q" && fields.is_empty() {
            return self.reply(SUPPORT);
        }
        match parse(op, &fields) {
            Some((change, None)) => {
                self.cursor.pen.borders = changed(&self.cursor.pen.borders, &change)
            }
            Some((change, Some((rows, cols)))) => self.border_rectangle(rows, cols, &change),
            None => {}
        }
    }

    /// Change the borders of the cells `rows` down and `cols` across from
    /// the cursor, as far as the screen goes.
    fn border_rectangle(&mut self, rows: u16, cols: u16, change: &Change) {
        let (top, left) = (self.cursor.row, usize::from(self.cursor.col));
        let right = (left + usize::from(cols)).min(usize::from(self.grid.cols));
        for row in top..top.saturating_add(rows).min(self.grid.rows) {
            let line = self.grid.line_mut(row);
            for col in left..right {
                let old = line.extra(&line.cells[col]).and_then(|extra| extra.borders.clone());
                line.set_borders(col, &changed(&old, change));
            }
        }
    }
}
