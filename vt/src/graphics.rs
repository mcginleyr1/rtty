//! The kitty graphics protocol, while the host has it on (`Terminal::set_graphics`): programs
//! transmit images in APC strings (`ESC _ G keys ; base64 ESC \`), which are decoded and held up
//! to `GRAPHICS_QUOTA`, and place them on the grid. The host reads what to draw from
//! `Terminal::placements`; nothing here draws.
//!
//! A placement is held by the row its top left cell is on, at that cell's column, so it scrolls
//! with the text into the scrollback, reflows with that cell, and goes when its row does: trimmed
//! from the scrollback, scrolled out of a margin region, or reset whole (ED 2, the alternate
//! screen's clearing, RIS). ED 0 and 1 and the other erases leave placements be.

use std::collections::HashSet;
use std::fs::{Metadata, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::ops::Range;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::dispatch::base64;
use crate::term::Terminal;

/// The most decoded image data (4 bytes a pixel) one terminal holds: a new image past it evicts
/// the oldest, those placed nowhere first.
pub const GRAPHICS_QUOTA: usize = 256 << 20;

/// The longest APC string kept, in bytes; a longer one is dropped whole. Programs send an image
/// past 4096 bytes of base64 in chunks, each a string of its own.
pub const APC_MAX: usize = 16 << 20;

/// Every way a file transmission fails answers alike, so that a program learns nothing of files
/// it cannot read itself.
const UNREADABLE: &str = "EBADF:Failed to read image file";

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

#[derive(Default)]
pub(crate) struct Graphics {
    /// Oldest first, the order the quota evicts them in.
    images: Vec<Arc<Image>>,
    /// The bytes of their pixels.
    used: usize,
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
            b'C' => places && value == "1",
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
    /// queries with `e`), place (`a=p`, `a=T`) and delete them (`a=d`). Placements size
    /// themselves by the cell size `set_cell_pixels` gave; one before it fails. `placements`
    /// says what to draw.
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
    /// whatever row holds it. Nothing while graphics are off.
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
        self.unplace(false, |anchor, _, _| gone.contains(&anchor.generation));
    }

    /// The generations of the images placed somewhere.
    fn placed(&self) -> HashSet<u64> {
        let rows = self.grid.primary.iter().chain(&self.grid.alt);
        rows.flat_map(|row| &row.images).map(|anchor| anchor.generation).collect()
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
    /// image and id. Unless `C=1`, the cursor goes down to the last row it covers (scrolling as a
    /// linefeed does) and past its right edge.
    fn place(&mut self, command: &Command, image: &Image) -> Result<(), String> {
        if command.parent != 0 {
            return Err("EINVAL:relative placements are not supported".into());
        }
        let cell = self.cell_pixels().ok_or("EINVAL:the cell size is not known")?;
        let fit = fit(command, image, cell)?;
        let (generation, id) =
            (image.generation, if image.id == 0 { 0 } else { command.placement });
        let graphics = self.graphics.as_deref_mut().expect("graphics are on");
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

    /// At most `PLACEMENTS_MAX` placements: past it the oldest goes.
    fn bound_placements(&mut self) {
        let rows = self.grid.primary.iter().chain(&self.grid.alt);
        let serials = rows.flat_map(|row| &row.images).map(|anchor| anchor.serial);
        let (count, oldest) =
            serials.fold((0, u64::MAX), |(count, oldest), serial| (count + 1, oldest.min(serial)));
        if count > PLACEMENTS_MAX {
            self.unplace(false, |anchor, _, _| anchor.serial == oldest);
        }
    }

    /// `a=d`: placements by where they are, on the screen only (`a`, `c`, `p`, `q`, `x`, `y`,
    /// `z`), or images by id (`i`, `n`, `r`). Upper case forgets the images then placed nowhere,
    /// scrollback included.
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
