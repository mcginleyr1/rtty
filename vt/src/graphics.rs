//! The kitty graphics protocol, while the host has it on (`Terminal::set_graphics`): programs
//! transmit images in APC strings (`ESC _ G keys ; base64 ESC \`), which are decoded and held up
//! to `GRAPHICS_QUOTA`, and place them on the grid. The host reads what to draw from
//! `Terminal::placements`; nothing here draws.
//!
//! A placement is held by the row its top left cell is on, at that cell's column, so it scrolls
//! with the text into the scrollback, reflows with that cell, and goes when its row does: trimmed
//! from the scrollback, scrolled out of a margin region, or reset whole (ED 2, the alternate
//! screen's clearing, RIS). ED 0 and 1 and the other erases leave placements be.
//!
//! Unicode placeholders show an image through text instead, which passes through tmux and tutti
//! as any text does: a virtual placement (`U=1`) lays the image out over a block of cells, and
//! each cell holding `PLACEHOLDER` shows the cell of that block its diacritics name, of the image
//! its foreground colour names (and the placement its underline colour does).

use std::collections::HashSet;
use std::fs::{Metadata, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::ops::Range;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cell::Color;
use crate::dispatch::base64;
use crate::row::Row;
use crate::term::Terminal;

/// The most decoded image data (4 bytes a pixel) one terminal holds: a new image past it evicts
/// the oldest, those placed nowhere first.
pub const GRAPHICS_QUOTA: usize = 256 << 20;

/// The longest APC string kept, in bytes; a longer one is dropped whole. Programs send an image
/// past 4096 bytes of base64 in chunks, each a string of its own.
pub const APC_MAX: usize = 16 << 20;

/// U+10EEEE: a cell showing part of an image, which its colours and diacritics name.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Every way a file transmission fails answers alike, so that a program learns nothing of files
/// it cannot read itself.
const UNREADABLE: &str = "EBADF:Failed to read image file";

/// The diacritics that number a placeholder's row, column and the high byte of its image id, in
/// order from 0: the combining characters of class 230 in Unicode 6.0 with no decomposition, but
/// those normalization may fuse (kitty's rowcolumn-diacritics.txt).
#[rustfmt::skip]
const DIACRITICS: [char; 297] = [
    '\u{305}', '\u{30d}', '\u{30e}', '\u{310}', '\u{312}', '\u{33d}', '\u{33e}', '\u{33f}',
    '\u{346}', '\u{34a}', '\u{34b}', '\u{34c}', '\u{350}', '\u{351}', '\u{352}', '\u{357}',
    '\u{35b}', '\u{363}', '\u{364}', '\u{365}', '\u{366}', '\u{367}', '\u{368}', '\u{369}',
    '\u{36a}', '\u{36b}', '\u{36c}', '\u{36d}', '\u{36e}', '\u{36f}', '\u{483}', '\u{484}',
    '\u{485}', '\u{486}', '\u{487}', '\u{592}', '\u{593}', '\u{594}', '\u{595}', '\u{597}',
    '\u{598}', '\u{599}', '\u{59c}', '\u{59d}', '\u{59e}', '\u{59f}', '\u{5a0}', '\u{5a1}',
    '\u{5a8}', '\u{5a9}', '\u{5ab}', '\u{5ac}', '\u{5af}', '\u{5c4}', '\u{610}', '\u{611}',
    '\u{612}', '\u{613}', '\u{614}', '\u{615}', '\u{616}', '\u{617}', '\u{657}', '\u{658}',
    '\u{659}', '\u{65a}', '\u{65b}', '\u{65d}', '\u{65e}', '\u{6d6}', '\u{6d7}', '\u{6d8}',
    '\u{6d9}', '\u{6da}', '\u{6db}', '\u{6dc}', '\u{6df}', '\u{6e0}', '\u{6e1}', '\u{6e2}',
    '\u{6e4}', '\u{6e7}', '\u{6e8}', '\u{6eb}', '\u{6ec}', '\u{730}', '\u{732}', '\u{733}',
    '\u{735}', '\u{736}', '\u{73a}', '\u{73d}', '\u{73f}', '\u{740}', '\u{741}', '\u{743}',
    '\u{745}', '\u{747}', '\u{749}', '\u{74a}', '\u{7eb}', '\u{7ec}', '\u{7ed}', '\u{7ee}',
    '\u{7ef}', '\u{7f0}', '\u{7f1}', '\u{7f3}', '\u{816}', '\u{817}', '\u{818}', '\u{819}',
    '\u{81b}', '\u{81c}', '\u{81d}', '\u{81e}', '\u{81f}', '\u{820}', '\u{821}', '\u{822}',
    '\u{823}', '\u{825}', '\u{826}', '\u{827}', '\u{829}', '\u{82a}', '\u{82b}', '\u{82c}',
    '\u{82d}', '\u{951}', '\u{953}', '\u{954}', '\u{f82}', '\u{f83}', '\u{f86}', '\u{f87}',
    '\u{135d}', '\u{135e}', '\u{135f}', '\u{17dd}', '\u{193a}', '\u{1a17}', '\u{1a75}', '\u{1a76}',
    '\u{1a77}', '\u{1a78}', '\u{1a79}', '\u{1a7a}', '\u{1a7b}', '\u{1a7c}', '\u{1b6b}', '\u{1b6d}',
    '\u{1b6e}', '\u{1b6f}', '\u{1b70}', '\u{1b71}', '\u{1b72}', '\u{1b73}', '\u{1cd0}', '\u{1cd1}',
    '\u{1cd2}', '\u{1cda}', '\u{1cdb}', '\u{1ce0}', '\u{1dc0}', '\u{1dc1}', '\u{1dc3}', '\u{1dc4}',
    '\u{1dc5}', '\u{1dc6}', '\u{1dc7}', '\u{1dc8}', '\u{1dc9}', '\u{1dcb}', '\u{1dcc}', '\u{1dd1}',
    '\u{1dd2}', '\u{1dd3}', '\u{1dd4}', '\u{1dd5}', '\u{1dd6}', '\u{1dd7}', '\u{1dd8}', '\u{1dd9}',
    '\u{1dda}', '\u{1ddb}', '\u{1ddc}', '\u{1ddd}', '\u{1dde}', '\u{1ddf}', '\u{1de0}', '\u{1de1}',
    '\u{1de2}', '\u{1de3}', '\u{1de4}', '\u{1de5}', '\u{1de6}', '\u{1dfe}', '\u{20d0}', '\u{20d1}',
    '\u{20d4}', '\u{20d5}', '\u{20d6}', '\u{20d7}', '\u{20db}', '\u{20dc}', '\u{20e1}', '\u{20e7}',
    '\u{20e9}', '\u{20f0}', '\u{2cef}', '\u{2cf0}', '\u{2cf1}', '\u{2de0}', '\u{2de1}', '\u{2de2}',
    '\u{2de3}', '\u{2de4}', '\u{2de5}', '\u{2de6}', '\u{2de7}', '\u{2de8}', '\u{2de9}', '\u{2dea}',
    '\u{2deb}', '\u{2dec}', '\u{2ded}', '\u{2dee}', '\u{2def}', '\u{2df0}', '\u{2df1}', '\u{2df2}',
    '\u{2df3}', '\u{2df4}', '\u{2df5}', '\u{2df6}', '\u{2df7}', '\u{2df8}', '\u{2df9}', '\u{2dfa}',
    '\u{2dfb}', '\u{2dfc}', '\u{2dfd}', '\u{2dfe}', '\u{2dff}', '\u{a66f}', '\u{a67c}', '\u{a67d}',
    '\u{a6f0}', '\u{a6f1}', '\u{a8e0}', '\u{a8e1}', '\u{a8e2}', '\u{a8e3}', '\u{a8e4}', '\u{a8e5}',
    '\u{a8e6}', '\u{a8e7}', '\u{a8e8}', '\u{a8e9}', '\u{a8ea}', '\u{a8eb}', '\u{a8ec}', '\u{a8ed}',
    '\u{a8ee}', '\u{a8ef}', '\u{a8f0}', '\u{a8f1}', '\u{aab0}', '\u{aab2}', '\u{aab3}', '\u{aab7}',
    '\u{aab8}', '\u{aabe}', '\u{aabf}', '\u{aac1}', '\u{fe20}', '\u{fe21}', '\u{fe22}', '\u{fe23}',
    '\u{fe24}', '\u{fe25}', '\u{fe26}', '\u{10a0f}', '\u{10a38}', '\u{1d185}', '\u{1d186}',
    '\u{1d187}', '\u{1d188}', '\u{1d189}', '\u{1d1aa}', '\u{1d1ab}', '\u{1d1ac}', '\u{1d1ad}',
    '\u{1d242}', '\u{1d243}', '\u{1d244}',
];

/// Image generations, unique in the process.
static GENERATIONS: AtomicU64 = AtomicU64::new(1);

/// Placements in the order they were made, in the process.
static SERIALS: AtomicU64 = AtomicU64::new(1);

/// The most images one terminal holds, however small: past it the oldest go, as past the quota.
const IMAGES_MAX: usize = 4096;

/// The most placements one terminal holds, however small and wherever they are: past it the
/// oldest goes.
const PLACEMENTS_MAX: usize = 4096;

/// An image a program transmitted, decoded.
#[derive(Debug)]
pub struct Image {
    /// The program's `i`, or the one the terminal chose for an image sent with `I`; 0 for
    /// neither.
    pub id: u32,
    /// Unique to this transmission in the process: what a host derives from the pixels keys on
    /// it.
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    /// Row by row, 4 bytes a pixel: sRGB red, green and blue, and straight alpha.
    pub rgba: Vec<u8>,
    number: u32,
}

/// An image, or part of one, over the grid, as a host draws it.
#[derive(Clone, Debug)]
pub struct Placement {
    pub image: Arc<Image>,
    /// The cell `dest` counts from: an absolute row (`Terminal::row`) and a column.
    pub row: u64,
    pub col: u16,
    /// Where on the grid, in cells from that cell's top left corner: left, top, width, height.
    pub dest: [f32; 4],
    /// The part of the image drawn there, in its pixels: left, top, width, height.
    pub source: [f32; 4],
    /// Below the text when negative, and below cells' own backgrounds too under `i32::MIN / 2`;
    /// above the text otherwise. Equal ones stack by image id.
    pub z: i32,
}

/// How an image covers a block of cells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Fit {
    /// Columns and rows.
    cells: (u16, u16),
    dest: [f32; 4],
    source: [f32; 4],
    z: i32,
}

/// A placement, held by the row its top left cell is on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Anchor {
    pub(crate) col: u16,
    generation: u64,
    /// The program's `p`; 0 for none.
    id: u32,
    fit: Fit,
    /// When it was made (`SERIALS`).
    serial: u64,
}

/// A virtual placement (`U=1`): no cells of its own, it lays the image out over the block of
/// cells placeholders show it in.
struct Virtual {
    generation: u64,
    id: u32,
    fit: Fit,
    serial: u64,
}

/// A placeholder cell, decoded: the colours naming its image and placement, the high byte of the
/// image id, and the cell of the virtual placement it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Spot {
    fg: Color,
    underline: Color,
    high: u32,
    row: u32,
    col: u32,
}

#[derive(Default)]
pub(crate) struct Graphics {
    /// Oldest first, the order the quota evicts them in.
    images: Vec<Arc<Image>>,
    /// The bytes of their pixels.
    used: usize,
    virtuals: Vec<Virtual>,
    /// A transmission still arriving in chunks: its first command and the data so far.
    transfer: Option<(Command, Vec<u8>)>,
    /// The most rows one placement covers: how far above a row one reaching it can be held.
    tallest: u16,
    /// The id last chosen for an image sent with `I` alone.
    chosen: u32,
}

/// A command's keys, each at its default when absent.
#[derive(Clone, Debug, Default)]
struct Command {
    /// `a`, when given; `t` otherwise.
    action: Option<u8>,
    /// `q`: 1 keeps OK quiet, 2 errors too.
    quiet: u8,
    /// `f`: 24, 32 or 100; 0 for the default, 32.
    format: u32,
    /// `t`: `d`, `f` or `t`; 0 for the default, `d`.
    medium: u8,
    /// `o`.
    compression: Option<u8>,
    /// `s` and `v`: the pixels across and down raw data holds.
    size: (u32, u32),
    /// `S` and `O`: how much of a file to read, and from where.
    span: (u32, u32),
    /// `i`.
    id: u32,
    /// `I`.
    number: u32,
    /// `p`.
    placement: u32,
    /// `m=1`.
    more: bool,
    /// `x`, `y`, `w` and `h`.
    source: [u32; 4],
    /// `X` and `Y`.
    nudge: (u32, u32),
    /// `c` and `r`.
    cells: (u32, u32),
    /// `C=1`.
    still: bool,
    /// `U=1`.
    virtual_placement: bool,
    z: i32,
    /// `P`.
    parent: u32,
    /// `d`; 0 for the default, `a`.
    delete: u8,
    /// `e`.
    feature: Option<u8>,
}

/// A command's control data, `key=value` pairs between commas; `None` for a value of the wrong
/// kind. Keys not read here (`N`, `Q`, `H`, `V`, animation's) are ignored.
fn parse(control: &[u8]) -> Option<Command> {
    let mut command = Command::default();
    for pair in control.split(|&b| b == b',').filter(|pair| !pair.is_empty()) {
        let (&key, value) = pair.split_first()?;
        let value = std::str::from_utf8(value.strip_prefix(b"=")?).ok()?;
        let number = || value.parse::<u32>().ok();
        let letter =
            || Some(value.as_bytes()).filter(|value| value.len() == 1).map(|value| value[0]);
        match key {
            b'a' => command.action = Some(letter()?),
            b'q' => command.quiet = number()?.min(2) as u8,
            b'f' => command.format = number()?,
            b't' => command.medium = letter()?,
            b'o' => command.compression = Some(letter()?),
            b's' => command.size.0 = number()?,
            b'v' => command.size.1 = number()?,
            b'S' => command.span.0 = number()?,
            b'O' => command.span.1 = number()?,
            b'i' => command.id = number()?,
            b'I' => command.number = number()?,
            b'p' => command.placement = number()?,
            b'm' => command.more = number()? == 1,
            b'x' => command.source[0] = number()?,
            b'y' => command.source[1] = number()?,
            b'w' => command.source[2] = number()?,
            b'h' => command.source[3] = number()?,
            b'X' => command.nudge.0 = number()?,
            b'Y' => command.nudge.1 = number()?,
            b'c' => command.cells.0 = number()?,
            b'r' => command.cells.1 = number()?,
            b'C' => command.still = number()? == 1,
            b'U' => command.virtual_placement = number()? == 1,
            b'z' => command.z = value.parse().ok()?,
            b'P' => command.parent = number()?,
            b'd' => command.delete = letter()?,
            b'e' => command.feature = Some(letter()?),
            _ => {}
        }
    }
    Some(command)
}

/// A feature query's keys after `i`: `e` and each key of `control` with a value other than its
/// default that `action` takes here; nothing for an action not supported.
fn features(action: u8, control: &[u8]) -> String {
    let (transmits, places) = (matches!(action, b't' | b'T' | b'q'), matches!(action, b'T' | b'p'));
    if !transmits && !places && action != b'd' {
        return String::new();
    }
    let mut keys = format!(",e={}", char::from(action));
    for pair in control.split(|&b| b == b',') {
        let Some((&key, value)) = pair.split_first() else { continue };
        let Some(value) =
            value.strip_prefix(b"=").and_then(|value| std::str::from_utf8(value).ok())
        else {
            continue;
        };
        let set = value != "0";
        let supported = match key {
            b'f' => transmits && matches!(value, "24" | "100"),
            b't' => transmits && matches!(value, "f" | "t"),
            b'o' => transmits && value == "z",
            b's' | b'v' | b'S' | b'O' => transmits && set,
            b'x' | b'y' | b'z' => (places || action == b'd') && set,
            b'p' => (places || action == b'd') && set,
            b'w' | b'h' | b'X' | b'Y' | b'c' | b'r' => places && set,
            b'C' | b'U' => places && value == "1",
            b'd' => action == b'd' && value.len() == 1 && "AcCiInNpPqQrRxXyYzZ".contains(value),
            _ => false,
        };
        if supported {
            keys.push_str(&format!(",{}={value}", char::from(key)));
        }
    }
    keys
}

impl Terminal {
    /// Turn the kitty graphics protocol on or off; it starts off. While it is off, its APC
    /// strings are dropped as every APC string is; turning it off forgets every image.
    ///
    /// Programs transmit images (`a=t`, `a=T`; RGB, RGBA or PNG, zlib compressed or not, in
    /// chunks or not, in the string or from a file: `t=d`, `t=f`, `t=t`), query (`a=q`, feature
    /// queries with `e`), place (`a=p`, `a=T`, virtual placements for Unicode placeholders with
    /// `U=1`) and delete them (`a=d`). Placements size themselves by the cell size
    /// `set_cell_pixels` gave; one before it fails. `placements` says what to draw.
    pub fn set_graphics(&mut self, on: bool) {
        if on == self.graphics.is_some() {
            return;
        }
        self.graphics = on.then(Box::default);
        self.apc = Default::default();
        for row in self.grid.primary.iter_mut().chain(&mut self.grid.alt) {
            row.images.clear();
        }
    }

    /// The images to draw over rows `from..to`, lowest first: each placement reaching into them,
    /// whatever row holds it, and what their placeholder cells show, a run of neighbouring cells
    /// in one row of one image as one piece. Nothing while graphics are off.
    pub fn placements(&self, from: u64, to: u64) -> Vec<Placement> {
        let Some(graphics) = self.graphics.as_deref() else { return Vec::new() };
        let mut placements = Vec::new();
        // The primary screen's, its scrollback's included, lie under the alternate screen.
        let first = from.saturating_sub(graphics.tallest.into());
        let first = if self.grid.alt_active { first.max(self.screen_top()) } else { first };
        for (index, row) in self.rows(first, to) {
            for anchor in &row.images {
                let Some(image) = graphics.image(anchor.generation) else { continue };
                if index + u64::from(anchor.fit.cells.1) > from {
                    placements.push(Placement {
                        image: image.clone(),
                        row: index,
                        col: anchor.col,
                        dest: anchor.fit.dest,
                        source: anchor.fit.source,
                        z: anchor.fit.z,
                    });
                }
            }
            if index >= from {
                graphics.placeholders(row, index, &mut placements);
            }
        }
        placements.sort_by_key(|placement| (placement.z, placement.image.id));
        placements
    }

    /// The images held, oldest first; none while graphics are off. What a host derives from one
    /// can go once its generation is no longer here.
    pub fn images(&self) -> &[Arc<Image>] {
        self.graphics.as_deref().map_or(&[], |graphics| &graphics.images)
    }

    /// A graphics command: what follows `ESC _ G`, up to ST.
    pub(crate) fn graphics_command(&mut self, body: &[u8]) {
        let (control, payload) =
            body.split_at(body.iter().position(|&b| b == b';').unwrap_or(body.len()));
        let payload = payload.get(1..).unwrap_or_default();
        let (Some(mut command), Some(graphics)) = (parse(control), self.graphics.as_deref_mut())
        else {
            return;
        };
        // Keys but `m` and `q` come with a transmission's first chunk; a command with an action
        // of its own (a delete, say) drops one left unfinished.
        let mut data = Vec::new();
        if let Some((first, sent)) = graphics.transfer.take().filter(|_| command.action.is_none()) {
            let quiet = if command.quiet > 0 { command.quiet } else { first.quiet };
            command = Command { quiet, more: command.more, ..first };
            data = sent;
        }
        let action = command.action.unwrap_or(b't');
        if matches!(action, b't' | b'T' | b'q') && command.feature.is_none() {
            match base64(payload) {
                Some(chunk) if data.len() + chunk.len() <= GRAPHICS_QUOTA => data.extend(chunk),
                Some(_) => {
                    return self.answer(&command, Err("EFBIG:past the storage quota".into()));
                }
                None => {
                    return self.answer(&command, Err("EINVAL:the payload is not base64".into()));
                }
            }
            if command.more {
                if let Some(graphics) = self.graphics.as_deref_mut() {
                    graphics.transfer = Some((command, data));
                }
                return;
            }
        }
        let result = match action {
            _ if command.id != 0 && command.number != 0 => Err("EINVAL:both i and I".into()),
            b'q' if command.feature.is_some() => return self.answer_features(&command, control),
            b'q' => decode(&command, data).map(drop),
            b't' | b'T' => self.transmit(&mut command, data),
            b'p' => self.put(&command),
            b'd' => return self.delete(&command),
            _ => Err("EINVAL:animation is not supported".into()),
        };
        self.answer(&command, result);
    }

    /// Tell the program how its command went, unless it named no image or asked for quiet.
    fn answer(&mut self, command: &Command, result: Result<(), String>) {
        let quiet = if result.is_ok() { command.quiet >= 1 } else { command.quiet >= 2 };
        if quiet || command.id == 0 && command.number == 0 {
            return;
        }
        let keys = [("i", command.id), ("I", command.number), ("p", command.placement)];
        let keys: Vec<String> =
            keys.iter().filter(|(_, n)| *n != 0).map(|(key, n)| format!("{key}={n}")).collect();
        let message = result.err().unwrap_or_else(|| "OK".into());
        self.reply(format!("\x1b_G{};{message}\x1b\\", keys.join(",")));
    }

    fn answer_features(&mut self, command: &Command, control: &[u8]) {
        if command.id != 0 && command.quiet == 0 {
            let keys = command.feature.map(|action| features(action, control)).unwrap_or_default();
            self.reply(format!("\x1b_Gi={}{keys};OK\x1b\\", command.id));
        }
    }

    /// `a=t` and `a=T`: hold the image, in place of one of the same id and its placements; `T`
    /// places it too.
    fn transmit(&mut self, command: &mut Command, data: Vec<u8>) -> Result<(), String> {
        let (width, height, rgba) = decode(command, data)?;
        let graphics = self.graphics.as_deref_mut().expect("graphics are on");
        if command.id == 0 && command.number != 0 {
            command.id = graphics.choose_id();
        }
        let (id, number) = (command.id, command.number);
        if id != 0 {
            self.drop_images(|image| image.id == id);
        }
        let generation = GENERATIONS.fetch_add(1, Ordering::Relaxed);
        let image = Arc::new(Image { id, generation, width, height, rgba, number });
        self.hold(image.clone());
        match command.action {
            Some(b'T') => self.place(command, &image),
            _ => Ok(()),
        }
    }

    /// Keep `image` under the quota and `IMAGES_MAX`: the oldest images make room, those placed
    /// nowhere first, placed ones taking their placements with them.
    fn hold(&mut self, image: Arc<Image>) {
        let placed = self.placed();
        let graphics = self.graphics.as_deref_mut().expect("graphics are on");
        let (unplaced, placed): (Vec<_>, Vec<_>) =
            graphics.images.iter().partition(|old| !placed.contains(&old.generation));
        let (mut used, mut held) = (graphics.used, graphics.images.len());
        let mut evicted = HashSet::new();
        for old in unplaced.into_iter().chain(placed) {
            if used + image.rgba.len() <= GRAPHICS_QUOTA && held < IMAGES_MAX {
                break;
            }
            (used, held) = (used - old.rgba.len(), held - 1);
            evicted.insert(old.generation);
        }
        self.drop_images(|old| evicted.contains(&old.generation));
        let graphics = self.graphics.as_deref_mut().expect("graphics are on");
        graphics.used += image.rgba.len();
        graphics.images.push(image);
    }

    /// Forget the images `doomed` picks, and every placement of them.
    fn drop_images(&mut self, doomed: impl Fn(&Image) -> bool) {
        let graphics = self.graphics.as_deref_mut().expect("graphics are on");
        let gone: HashSet<u64> = graphics
            .images
            .iter()
            .filter(|image| doomed(image))
            .map(|image| image.generation)
            .collect();
        if gone.is_empty() {
            return;
        }
        graphics.images.retain(|image| !gone.contains(&image.generation));
        graphics.used = graphics.images.iter().map(|image| image.rgba.len()).sum();
        graphics.virtuals.retain(|placement| !gone.contains(&placement.generation));
        self.unplace(false, |anchor, _, _| gone.contains(&anchor.generation));
    }

    /// The generations of the images placed somewhere, virtually included.
    fn placed(&self) -> HashSet<u64> {
        let rows = self.grid.primary.iter().chain(&self.grid.alt);
        let anchored = rows.flat_map(|row| &row.images).map(|anchor| anchor.generation);
        let virtuals = self.graphics.iter().flat_map(|graphics| &graphics.virtuals);
        anchored.chain(virtuals.map(|placement| placement.generation)).collect()
    }

    /// Take the placements `doomed` picks, given each one's rows and columns, off their rows:
    /// with `on_screen`, only the rows of the screen that is up (the primary's with its
    /// scrollback), as `placements` draws them. The generations of the images they were of.
    fn unplace(
        &mut self,
        on_screen: bool,
        doomed: impl Fn(&Anchor, Range<u64>, Range<u16>) -> bool,
    ) -> HashSet<u64> {
        let (top, alt) = (self.screen_top(), self.grid.alt_active);
        let grid = &mut self.grid;
        let primary = grid.primary.iter_mut().zip(grid.oldest..).map(|(row, i)| (row, i, !alt));
        let alternate = grid.alt.iter_mut().zip(top..).map(|(row, i)| (row, i, alt));
        let mut gone = HashSet::new();
        for (row, index, shown) in primary.chain(alternate) {
            if on_screen && !shown {
                continue;
            }
            row.images.retain(|anchor| {
                let (cols, rows) = anchor.fit.cells;
                let rows = index..index + u64::from(rows);
                let doomed = doomed(anchor, rows, anchor.col..anchor.col.saturating_add(cols));
                if doomed {
                    gone.insert(anchor.generation);
                }
                !doomed
            });
        }
        gone
    }

    /// `a=p`: place an image held.
    fn put(&mut self, command: &Command) -> Result<(), String> {
        let graphics = self.graphics.as_deref().expect("graphics are on");
        let image = graphics.find(command.id, command.number).ok_or("ENOENT:no such image")?;
        self.place(command, &image)
    }

    /// Put `image` at the cursor as `command` lays it out, in place of a placement of the same
    /// image and id, or lay it out for placeholders (`U=1`). Unless `C=1`, the cursor goes down to
    /// the last row it covers (scrolling as a linefeed does) and past its right edge.
    fn place(&mut self, command: &Command, image: &Image) -> Result<(), String> {
        if command.parent != 0 {
            return Err("EINVAL:relative placements are not supported".into());
        }
        let cell = self.cell_pixels().ok_or("EINVAL:the cell size is not known")?;
        let fit = fit(command, image, cell)?;
        let (generation, id) =
            (image.generation, if image.id == 0 { 0 } else { command.placement });
        let graphics = self.graphics.as_deref_mut().expect("graphics are on");
        if command.virtual_placement {
            graphics.virtuals.retain(|old| (old.generation, old.id) != (generation, id));
            let serial = SERIALS.fetch_add(1, Ordering::Relaxed);
            graphics.virtuals.push(Virtual { generation, id, fit, serial });
            self.bound_placements();
            return Ok(());
        }
        graphics.tallest = graphics.tallest.max(fit.cells.1);
        if id != 0 {
            self.unplace(false, |anchor, _, _| (anchor.generation, anchor.id) == (generation, id));
        }
        let (row, col) = (self.cursor.row, self.cursor.col);
        let serial = SERIALS.fetch_add(1, Ordering::Relaxed);
        self.grid.line_mut(row).images.push(Anchor { col, generation, id, fit, serial });
        self.bound_placements();
        if !command.still {
            for _ in 1..fit.cells.1 {
                self.index();
            }
            self.cursor.col = col.saturating_add(fit.cells.0).min(self.grid.cols - 1);
            self.cursor.pending_wrap = false;
        }
        Ok(())
    }

    /// At most `PLACEMENTS_MAX` placements, virtual ones included: past it the oldest goes.
    fn bound_placements(&mut self) {
        let graphics = self.graphics.as_deref_mut().expect("graphics are on");
        let rows = self.grid.primary.iter().chain(&self.grid.alt);
        let anchored = rows.flat_map(|row| &row.images).map(|anchor| anchor.serial);
        let serials = anchored.chain(graphics.virtuals.iter().map(|placement| placement.serial));
        let (count, oldest) =
            serials.fold((0, u64::MAX), |(count, oldest), serial| (count + 1, oldest.min(serial)));
        if count > PLACEMENTS_MAX {
            graphics.virtuals.retain(|placement| placement.serial != oldest);
            self.unplace(false, |anchor, _, _| anchor.serial == oldest);
        }
    }

    /// `a=d`: placements by where they are, on the screen only (`a`, `c`, `p`, `q`, `x`, `y`,
    /// `z`), or images by id (`i`, `n`, `r`), virtual placements too. Upper case forgets the
    /// images then placed nowhere, scrollback included.
    fn delete(&mut self, command: &Command) {
        let top = self.screen_top();
        let screen = top..top + u64::from(self.grid.rows);
        let cursor = (top + u64::from(self.cursor.row), self.cursor.col);
        let [x, y, ..] = command.source;
        let cell = (top + u64::from(y.max(1) - 1), (x.max(1) - 1).min(u16::MAX.into()) as u16);
        let shown = |rows: &Range<u64>| rows.start < screen.end && screen.start < rows.end;
        let over = |rows: &Range<u64>, cols: &Range<u16>, (row, col): (u64, u16)| {
            rows.contains(&row) && cols.contains(&col)
        };
        let (z, what) = (command.z, if command.delete == 0 { b'a' } else { command.delete });
        let gone = match what.to_ascii_lowercase() {
            b'a' => self.unplace(true, |_, rows, _| shown(&rows)),
            b'c' => self.unplace(true, |_, rows, cols| over(&rows, &cols, cursor)),
            b'p' => self.unplace(true, |_, rows, cols| over(&rows, &cols, cell)),
            b'q' => self
                .unplace(true, |anchor, rows, cols| over(&rows, &cols, cell) && anchor.fit.z == z),
            b'x' => self.unplace(true, |_, rows, cols| shown(&rows) && cols.contains(&cell.1)),
            b'y' => self.unplace(true, |_, rows, _| rows.contains(&cell.0)),
            b'z' => self.unplace(true, |anchor, rows, _| shown(&rows) && anchor.fit.z == z),
            b'i' | b'n' | b'r' => {
                let graphics = self.graphics.as_deref_mut().expect("graphics are on");
                let ids = |image: &&Arc<Image>| match what.to_ascii_lowercase() {
                    b'i' => command.id != 0 && image.id == command.id,
                    _ => (x..=y).contains(&image.id),
                };
                let images: HashSet<u64> = match what.to_ascii_lowercase() {
                    b'n' => graphics
                        .find(0, command.number)
                        .iter()
                        .map(|image| image.generation)
                        .collect(),
                    _ => graphics.images.iter().filter(ids).map(|image| image.generation).collect(),
                };
                let p = command.placement;
                let ours = |generation, id| images.contains(&generation) && (p == 0 || id == p);
                graphics.virtuals.retain(|placement| !ours(placement.generation, placement.id));
                self.unplace(false, |anchor, _, _| ours(anchor.generation, anchor.id));
                images
            }
            _ => return,
        };
        if what.is_ascii_uppercase() {
            let placed = self.placed();
            self.drop_images(|image| {
                gone.contains(&image.generation) && !placed.contains(&image.generation)
            });
        }
    }

    /// ED 2: the images on the screen go with its text.
    pub(crate) fn erase_images(&mut self) {
        if self.graphics.is_some() {
            self.delete(&Command::default());
        }
    }

    /// RIS: every image goes.
    pub(crate) fn reset_graphics(&mut self) {
        if let Some(graphics) = self.graphics.as_deref_mut() {
            *graphics = Graphics::default();
        }
    }
}

impl Graphics {
    fn image(&self, generation: u64) -> Option<&Arc<Image>> {
        self.images.iter().find(|image| image.generation == generation)
    }

    /// The image `id` names, else the newest that `number` does.
    fn find(&self, id: u32, number: u32) -> Option<Arc<Image>> {
        let named = |image: &&Arc<Image>| match id {
            0 => number != 0 && image.number == number,
            id => image.id == id,
        };
        self.images.iter().rev().find(named).cloned()
    }

    /// An id for an image sent with `I` alone, from 2³¹ up, clear of the small ones programs
    /// pick themselves.
    fn choose_id(&mut self) -> u32 {
        loop {
            self.chosen = self.chosen.wrapping_add(1).max(1 << 31);
            if !self.images.iter().any(|image| image.id == self.chosen) {
                return self.chosen;
            }
        }
    }

    /// What the placeholder cells of `row`, absolute row `index`, show: each run of neighbouring
    /// cells in one row of one virtual placement as one piece.
    fn placeholders(&self, row: &Row, index: u64, out: &mut Vec<Placement>) {
        let mut left = None;
        let spots: Vec<Option<Spot>> = (0..row.cells().len())
            .map(|col| {
                left = spot(row, col, left);
                left
            })
            .collect();
        let next = |a: &Option<Spot>, b: &Option<Spot>| matches!((a, b), (Some(a), Some(b)) if Spot { col: a.col + 1, ..*a } == *b);
        let mut col = 0;
        for run in spots.chunk_by(next) {
            if let Some(spot) = run[0] {
                self.piece((col, spot, run.len() as u32), index, out);
            }
            col += run.len() as u16;
        }
    }

    /// The part of an image a run of `len` placeholder cells from column `col` shows: the cells
    /// `spot` and those after it name in the virtual placement's block, where the image covers
    /// them. A high byte past 255 (the list has 297 diacritics) names no image.
    fn piece(&self, (col, spot, len): (u16, Spot, u32), index: u64, out: &mut Vec<Placement>) {
        let id = (spot.high < 256).then(|| bits(spot.fg) | spot.high << 24);
        let Some(image) = id.and_then(|id| self.find(id, 0)) else { return };
        let placement = bits(spot.underline);
        let mut virtuals = self.virtuals.iter().rev();
        let Some(layout) = virtuals.find(|virtual_placement| {
            virtual_placement.generation == image.generation
                && (placement == 0 || virtual_placement.id == placement)
        }) else {
            return;
        };
        let ([dx, dy, dw, dh], [sx, sy, sw, sh]) = (layout.fit.dest, layout.fit.source);
        let (first, top) = (spot.col as f32, spot.row as f32);
        let (left, right) = (first.max(dx), (first + len as f32).min(dx + dw));
        let (upper, lower) = (top.max(dy), (top + 1.).min(dy + dh));
        if left < right && upper < lower {
            out.push(Placement {
                image,
                row: index,
                col,
                dest: [left - first, upper - top, right - left, lower - upper],
                source: [
                    sx + (left - dx) * sw / dw,
                    sy + (upper - dy) * sh / dh,
                    (right - left) * sw / dw,
                    (lower - upper) * sh / dh,
                ],
                z: layout.fit.z,
            });
        }
    }
}

/// The placeholder at `col` of `row`, if it holds one: its first three diacritics number its
/// row, its column and its image id's high byte; one missing is carried on from a placeholder
/// just `left` of it in the same colours, as the specification lays out, else 0.
fn spot(row: &Row, col: usize, left: Option<Spot>) -> Option<Spot> {
    let cell = &row.cells()[col];
    if cell.ch() != PLACEHOLDER {
        return None;
    }
    let (fg, underline) = (cell.fg(), row.underline_color(col));
    let number = |c: char| DIACRITICS.binary_search(&c).ok().map(|n| n as u32);
    let mut marks = row.cluster(col).unwrap_or_default().chars().skip(1).map(number);
    let (r, c, high) = (marks.next().flatten(), marks.next().flatten(), marks.next().flatten());
    let left = left.filter(|left| (left.fg, left.underline) == (fg, underline));
    let (row, col, high) = match (r, c, high, left) {
        (None, _, _, Some(left)) => (left.row, left.col + 1, left.high),
        (Some(r), None, _, Some(left)) if left.row == r => (r, left.col + 1, left.high),
        (Some(r), Some(c), None, Some(left)) if left.row == r && left.col + 1 == c => {
            (r, c, left.high)
        }
        (r, c, high, _) => (r.unwrap_or(0), c.unwrap_or(0), high.unwrap_or(0)),
    };
    Some(Spot { fg, underline, high, row, col })
}

/// The bits of an image or placement id a colour holds: a palette index's 8, an RGB colour's 24.
fn bits(color: Color) -> u32 {
    match color {
        Color::Default => 0,
        Color::Named(index) | Color::Indexed(index) => index.into(),
        Color::Rgb(r, g, b) => u32::from_be_bytes([0, r, g, b]),
    }
}

/// How `image` covers cells of `cell` pixels as `command` asks: the part `x`, `y`, `w` and `h`
/// pick, at its own size in pixels, or scaled to `c` columns or `r` rows (keeping its shape) or
/// fitted into both (keeping its shape, centred), from `X` and `Y` pixels into the first cell.
fn fit(command: &Command, image: &Image, cell: (u16, u16)) -> Result<Fit, String> {
    let [x, y, w, h] = command.source;
    let (x, y) = (x.min(image.width), y.min(image.height));
    let pick = |want: u32, left: u32| if want == 0 { left } else { want.min(left) };
    let (w, h) = (pick(w, image.width - x), pick(h, image.height - y));
    if w == 0 || h == 0 {
        return Err("EINVAL:the source rectangle is empty".into());
    }
    let (cw, ch) = (f64::from(cell.0.max(1)), f64::from(cell.1.max(1)));
    let (nx, ny) =
        (f64::from(command.nudge.0).min(cw - 1.), f64::from(command.nudge.1).min(ch - 1.));
    let (sw, sh) = (f64::from(w), f64::from(h));
    let (area_w, area_h) = (f64::from(command.cells.0) * cw, f64::from(command.cells.1) * ch);
    let (width, height) = match command.cells {
        (0, 0) => (sw, sh),
        (_, 0) => (area_w, sh * area_w / sw),
        (0, _) => (sw * area_h / sh, area_h),
        _ => {
            let scale = (area_w / sw).min(area_h / sh);
            (sw * scale, sh * scale)
        }
    };
    let (left, top) = match command.cells {
        (0, _) | (_, 0) => (nx, ny),
        _ => (nx + (area_w - width) / 2., ny + (area_h - height) / 2.),
    };
    let span = |given: u32, extent: f64, size: f64| {
        let span = if given > 0 { given } else { (extent / size).ceil() as u32 };
        span.clamp(1, u16::MAX.into()) as u16
    };
    Ok(Fit {
        cells: (span(command.cells.0, left + width, cw), span(command.cells.1, top + height, ch)),
        dest: [left / cw, top / ch, width / cw, height / ch].map(|value| value as f32),
        source: [x, y, w, h].map(|value| value as f32),
        z: command.z,
    })
}

/// The image a transmission carries, as its width, height and RGBA pixels.
fn decode(command: &Command, data: Vec<u8>) -> Result<(u32, u32, Vec<u8>), String> {
    let data = match command.medium {
        0 | b'd' => data,
        b'f' | b't' => read_file(command, &data).ok_or(UNREADABLE)?,
        _ => return Err("EINVAL:unsupported transmission medium".into()),
    };
    let data = match command.compression {
        None => data,
        Some(b'z') => {
            miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&data, GRAPHICS_QUOTA)
                .map_err(|_| "EINVAL:the data does not inflate")?
        }
        Some(_) => return Err("EINVAL:unsupported compression".into()),
    };
    match command.format {
        24 | 32 | 0 => {
            let channels = if command.format == 24 { 3 } else { 4 };
            let (width, height) = command.size;
            let pixels = (width as usize).saturating_mul(height as usize);
            if pixels == 0 || pixels > GRAPHICS_QUOTA / 4 || data.len() != pixels * channels {
                return Err("EINVAL:the data is not s by v pixels".into());
            }
            Ok((width, height, if channels == 4 { data } else { rgba(&data, channels) }))
        }
        100 => png(&data).ok_or_else(|| "EINVAL:not a PNG this terminal reads".into()),
        _ => Err("EINVAL:unsupported format".into()),
    }
}

/// Pixels of 1 (grey), 2 (grey and alpha), 3 (RGB) or 4 (RGBA) channels as RGBA.
fn rgba(data: &[u8], channels: usize) -> Vec<u8> {
    let pixel = |p: &[u8]| match channels {
        1 => [p[0], p[0], p[0], 255],
        2 => [p[0], p[0], p[0], p[1]],
        3 => [p[0], p[1], p[2], 255],
        _ => [p[0], p[1], p[2], p[3]],
    };
    data.chunks_exact(channels).flat_map(pixel).collect()
}

fn png(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let limits = png::Limits { bytes: GRAPHICS_QUOTA };
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(data), limits);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = (reader.info().width as usize, reader.info().height as usize);
    if width.saturating_mul(height) > GRAPHICS_QUOTA / 4 {
        return None;
    }
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut pixels).ok()?;
    let channels = frame.color_type.samples();
    pixels.truncate(frame.buffer_size());
    Some((frame.width, frame.height, rgba(&pixels, channels)))
}

/// `t=f` and `t=t`: the regular file the payload names, symlinks followed, outside `/proc`, `/sys`
/// and `/dev`; `S` bytes of it (all of it for 0) from `O` on, at most the quota. `t=t` takes only a
/// file in a temporary directory with `tty-graphics-protocol` in its path, and deletes it once
/// read. The path is checked before the file is opened, and the file again once it is: opened
/// without blocking (a FIFO put in its place waits for no writer), it must still be a regular file
/// within the quota.
fn read_file(command: &Command, path: &[u8]) -> Option<Vec<u8>> {
    let path = Path::new(std::str::from_utf8(path).ok()?).canonicalize().ok()?;
    let sensitive = ["/proc", "/sys", "/dev"].iter().any(|dir| path.starts_with(dir));
    let quota = GRAPHICS_QUOTA as u64;
    let regular = |metadata: Metadata| metadata.is_file() && metadata.len() <= quota;
    let temporary = command.medium == b't';
    let named = path.to_str().is_some_and(|path| path.contains("tty-graphics-protocol"));
    let in_temporary = [std::env::temp_dir(), PathBuf::from("/tmp")]
        .iter()
        .filter_map(|dir| dir.canonicalize().ok())
        .any(|dir| path.starts_with(dir));
    if sensitive
        || !regular(std::fs::metadata(&path).ok()?)
        || temporary && !(named && in_temporary)
    {
        return None;
    }
    let mut file = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&path).ok()?;
    if !regular(file.metadata().ok()?) {
        return None;
    }
    let (size, offset) = command.span;
    file.seek(SeekFrom::Start(offset.into())).ok()?;
    let mut data = Vec::new();
    let wanted = if size == 0 { quota } else { u64::from(size).min(quota) };
    file.take(wanted).read_to_end(&mut data).ok()?;
    if temporary {
        std::fs::remove_file(&path).ok()?;
    }
    (size == 0 || data.len() == size as usize).then_some(data)
}
