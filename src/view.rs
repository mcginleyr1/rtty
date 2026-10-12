use std::collections::HashMap;
use std::fmt::Write as _;
use std::iter;
use std::mem;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedReceiver;
use futures::{StreamExt, future};
use gpui::{
    App, BorderStyle, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler,
    EventEmitter, FocusHandle, Font, Global, Hsla, KeyDownEvent, KeyUpEvent, LineLayout,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render,
    Rgba, ScrollWheelEvent, ShapedGlyph, ShapedLine, ShapedRun, Size, Task, TextRun,
    UTF16Selection, UnderlineStyle, Window, WindowAppearance, WindowTextSystem, actions, canvas,
    div, fill, outline, point, prelude::*, px, size,
};
use rtty_vt::{Event, FOREGROUND, MouseEncoding, MouseMode, Rgb, Underline};

use crate::config::Theme;
use crate::keys;
use crate::mouse::{self, Action, Button, Report};
use crate::pane::{Notice, Pane, Trace};
use crate::render::{self, CursorShape, Line, Palette, Run, Scene};
use crate::scheme::Scheme;
use crate::selection::{Kind, Point as GridPoint, Selection, Side};

actions!(rtty, [Copy, Paste]);

/// How long a synchronized update (DEC mode 2026) may hold the last frame on screen.
const SYNC_TIMEOUT: Duration = Duration::from_millis(150);

/// A blinking cursor is on for this long, then off for as long; it holds still once the
/// keyboard has been idle for `BLINK_FOR`, so an idle window draws nothing.
const BLINK_MS: u128 = 500;
const BLINK_FOR: Duration = Duration::from_secs(10);

/// What every window and pane shares: the font and its size (Cmd+= and Cmd+- change it for all of
/// them), the color theme, and the diagnostics asked for.
pub struct Settings {
    pub font: Font,
    pub font_size: Pixels,
    pub theme: Theme,
    pub stats: bool,
    pub trace: Option<Trace>,
}

impl Global for Settings {}

/// A pane as the window shows it: the pane itself, its keyboard focus, and what its last frame
/// put where.
pub struct PaneView {
    pane: Pane,
    focus: FocusHandle,
    /// The theme's scheme for the window's current appearance.
    scheme: Arc<Scheme>,
    geometry: Option<Geometry>,
    cache: Cache,
    marked_text: Option<String>,
    scroll_remainder: Pixels,
    /// The cell the last mouse report named.
    mouse_cell: Option<(u16, u16)>,
    /// A button went down in this pane and has not come up: the drag is this pane's.
    pressed: bool,
    /// The grid and cell size in pixels the window last gave the pane.
    grid: (u16, u16, (u16, u16)),
    /// The pane has the keyboard in the active window, as last reported to the program.
    focused: bool,
    /// When the synchronized update now holding the screen began.
    sync_since: Option<Instant>,
    /// When the cursor began blinking, counted again from each key.
    blink_since: Option<Instant>,
    /// The frame that shows the blinking cursor's next phase.
    blink_wake: Option<Task<()>>,
    stats: Option<FrameStats>,
}

/// What a pane tells the window that holds it.
pub enum PaneEvent {
    /// The program named itself.
    Title,
    /// A click gave the pane the keyboard.
    Focused,
    Exited,
}

impl EventEmitter<PaneEvent> for PaneView {}

/// Where the last frame put the cells, for mapping mouse positions and placing the IME window.
#[derive(Clone, Copy)]
struct Geometry {
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    rows: u16,
    cols: u16,
    /// The terminal's cursor cell, visible or not, where composed text goes.
    cursor: Bounds<Pixels>,
}

/// A row as drawn, its runs shaped (`shaped[i]` is `line.runs[i]`).
struct ShapedRow {
    line: Line,
    shaped: Vec<ShapedLine>,
}

/// Rows shaped by earlier frames, by absolute index. A row is shaped again when the engine
/// reports it damaged, when the cursor is or was on it, when it left the screen since the last
/// frame (it may have changed on its way out), or when anything every row depends on changed.
#[derive(Default)]
struct Cache {
    key: Option<CacheKey>,
    rows: HashMap<u64, Rc<ShapedRow>>,
    screen_top: u64,
    cursor_row: Option<u64>,
    last: Option<Frame>,
}

#[derive(PartialEq)]
struct CacheKey {
    palette: Palette,
    colors: (Rgb, Rgb),
    selection: Option<(GridPoint, GridPoint)>,
    focused: bool,
    size: (u16, u16),
    font_size: Pixels,
}

#[derive(Clone)]
struct Frame {
    bounds: Bounds<Pixels>,
    cell: Size<Pixels>,
    /// From a cell's top: where underlines and strikethroughs go.
    underline: Pixels,
    strike: Pixels,
    /// Of every decoration line.
    thickness: Pixels,
    background: Hsla,
    rows: Vec<(u16, Rc<ShapedRow>)>,
    cursor: Option<PaintQuad>,
    marked_text: Option<(PaintQuad, Point<Pixels>, ShapedLine)>,
    layout_time: Duration,
}

#[derive(Default)]
struct FrameStats {
    since: Option<Instant>,
    frames: u32,
    layout: Duration,
    paint: Duration,
    worst: Duration,
}

impl PaneView {
    pub fn new(
        pane: Pane,
        mut notices: UnboundedReceiver<Notice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        cx.observe_window_activation(window, Self::focus_changed).detach();
        cx.on_focus(&focus, window, Self::focus_changed).detach();
        cx.on_blur(&focus, window, Self::focus_changed).detach();
        cx.observe_global_in::<Settings>(window, |_, _, cx| cx.notify()).detach();
        let settings = cx.global::<Settings>();
        let scheme = settings.theme.for_appearance(window.appearance()).clone();
        let stats = settings.stats.then(FrameStats::default);
        pane.set_scheme(&scheme);
        cx.spawn_in(window, async move |this, cx| {
            while let Some(notice) = notices.next().await {
                let batch: Vec<Notice> =
                    iter::once(notice).chain(iter::from_fn(|| notices.try_recv().ok())).collect();
                let applied = this.update_in(cx, |view, _, cx| {
                    batch.into_iter().for_each(|notice| view.notice(notice, cx))
                });
                if applied.is_err() {
                    break;
                }
                // A flood can keep the channel non-empty so `next` never suspends; yield so the
                // main thread still gets to draw frames.
                yield_now().await;
            }
        })
        .detach();
        let (rows, cols) = pane.lock().size();
        Self {
            pane,
            focus,
            scheme,
            geometry: None,
            cache: Cache::default(),
            marked_text: None,
            scroll_remainder: px(0.),
            mouse_cell: None,
            pressed: false,
            grid: (rows, cols, (0, 0)),
            focused: window.is_window_active(),
            sync_since: None,
            blink_since: None,
            blink_wake: None,
            stats,
        }
    }

    /// The keyboard came to or left this pane, or the window was activated or deactivated: draw
    /// the cursor accordingly and tell a program that asked (mode 1004) with `CSI I` / `CSI O`.
    fn focus_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.focus.is_focused(window) && window.is_window_active();
        if mem::replace(&mut self.focused, focused) != focused
            && self.pane.lock().modes().focus_reports
        {
            self.pane.write(if focused { b"\x1b[I" } else { b"\x1b[O" }.to_vec());
        }
        cx.notify();
    }

    /// Gives this pane the keyboard.
    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    /// Gives the terminal, and through the pty its program, `rows`×`cols` cells of `cell`: the
    /// window decides from its layout, for shown and hidden panes alike.
    pub fn resize(&mut self, rows: u16, cols: u16, cell: Size<Pixels>) {
        let grid = (rows, cols, (f32::from(cell.width) as u16, f32::from(cell.height) as u16));
        if mem::replace(&mut self.grid, grid) != grid {
            self.pane.resize(rows, cols, grid.2);
        }
    }

    pub fn title(&self) -> String {
        self.pane.lock().title().to_owned()
    }

    /// The directory the shell last reported (OSC 7).
    pub fn cwd(&self) -> Option<PathBuf> {
        self.pane.lock().cwd().map(PathBuf::from)
    }

    /// The OS switched between light and dark: repaint with the other scheme (cells keep color
    /// indices, so what is on screen recolors) and tell the terminal, which answers color queries
    /// with it and tells programs that asked (mode 2031). Colors a program set itself with
    /// OSC 4/10/11/12 are the program's, not the scheme's, and stay.
    pub fn follow_appearance(&mut self, appearance: WindowAppearance, cx: &mut Context<Self>) {
        let scheme = cx.global::<Settings>().theme.for_appearance(appearance);
        if Arc::ptr_eq(scheme, &self.scheme) {
            return;
        }
        self.scheme = scheme.clone();
        self.pane.set_scheme(&self.scheme);
        cx.notify();
    }

    fn notice(&mut self, notice: Notice, cx: &mut Context<Self>) {
        match notice {
            Notice::Output | Notice::Event(Event::CursorStyle(_) | Event::Mode(_)) => cx.notify(),
            Notice::Event(Event::Title(_)) => cx.emit(PaneEvent::Title),
            Notice::Event(Event::Clipboard { text, .. }) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text))
            }
            Notice::Event(_) => {}
            Notice::Exit => {
                if let Some(stats) = self.stats.as_mut() {
                    stats.report();
                }
                cx.emit(PaneEvent::Exited)
            }
        }
    }

    fn input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.pane.input(bytes);
        // Typing shows the cursor and starts its blinking over.
        self.blink_since = None;
        cx.notify();
    }

    /// How the program asked for keys.
    fn key_modes(&self) -> keys::Modes {
        let term = self.pane.lock();
        let modes = term.modes();
        keys::Modes {
            cursor_keys: modes.cursor_keys,
            modify_other_keys: modes.modify_other_keys,
            kitty: term.kitty_keyboard(),
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let kind = if event.is_held { keys::Event::Repeat } else { keys::Event::Press };
        // A held key repeats here: the input handler would open the accent picker instead.
        let bytes = keys::encode(keystroke, self.key_modes(), kind).or_else(|| {
            event.is_held.then(|| keystroke.key_char.clone()).flatten().map(String::into_bytes)
        });
        if bytes.is_some() || (keystroke.modifiers.control && !keystroke.modifiers.platform) {
            cx.stop_propagation();
        }
        if let Some(bytes) = bytes {
            self.input(bytes, cx);
        }
    }

    /// Releases matter only to a program that asked for kitty's event types.
    pub fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if let Some(bytes) = keys::encode(&event.keystroke, self.key_modes(), keys::Event::Release)
        {
            self.pane.write(bytes);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.pane.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let bracketed = self.pane.lock().modes().bracketed_paste;
        self.input(keys::paste(&text, bracketed), cx);
    }

    /// The cell under `position` as (row in view, column), and which half of it.
    fn cell_at(&self, position: Point<Pixels>) -> Option<((u16, u16), Side)> {
        let grid = self.geometry?;
        let x = ((position.x - grid.origin.x) / grid.cell.width).max(0.);
        let y = ((position.y - grid.origin.y) / grid.cell.height).max(0.);
        let col = (x as u16).min(grid.cols - 1);
        let row = (y as u16).min(grid.rows - 1);
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        Some(((row, col), side))
    }

    /// How long the cursor has been blinking, while it still blinks.
    fn blinking(&self) -> Option<Duration> {
        self.blink_since.map(|since| since.elapsed()).filter(|&elapsed| elapsed < BLINK_FOR)
    }

    /// The middle of the cell at (row in view, column), where the last frame put it.
    pub fn cell_center(&self, row: u16, col: u16) -> Option<Point<Pixels>> {
        let grid = self.geometry?;
        let cell = grid.cell;
        Some(
            grid.origin
                + point(cell.width * (f32::from(col) + 0.5), cell.height * (f32::from(row) + 0.5)),
        )
    }

    pub fn screen_text(&self) -> String {
        self.pane.lock().contents()
    }

    /// What the next frame draws, as text, with the state it is drawn from: `--script`'s `dump`.
    pub fn dump(&self, window: &Window) -> String {
        let term = self.pane.lock();
        let palette = Palette::new(&term, &self.scheme);
        let (rows, cols) = term.size();
        let top = self.pane.top(&term);
        let selection = self.pane.selection.and_then(|selection| selection.range(&term));
        let focused = self.focus.is_focused(window) && window.is_window_active();
        let scene = Scene::new(&term, &palette, top, rows, selection, focused);
        let hex = |Rgb(r, g, b): Rgb| format!("#{r:02x}{g:02x}{b:02x}");
        let mut out = String::new();
        let _ = writeln!(out, "size {rows}x{cols} top {top} screen_top {}", term.screen_top());
        let _ = writeln!(
            out,
            "scheme {:?} focused {focused} title {:?}",
            self.scheme.name,
            term.title()
        );
        let _ =
            writeln!(out, "cursor {:?} style {:?}", term.cursor_position(), term.cursor_style());
        let _ = writeln!(out, "kitty_keyboard {} {:?}", term.kitty_keyboard(), term.modes());
        let _ = writeln!(out, "default fg {} bg {}", hex(scene.fg), hex(scene.bg));
        if let Some(cursor) = scene.cursor {
            let row = cursor.row - top;
            let _ = writeln!(
                out,
                "drawn cursor {row},{} {:?} {}",
                cursor.col,
                cursor.shape,
                hex(cursor.color)
            );
        }
        if let Some(blinking) = self.blinking() {
            let phase = if blinked_off(blinking) { "off" } else { "on" };
            let _ = writeln!(out, "cursor blinking for {} ms, {phase}", blinking.as_millis());
        }
        for (index, row) in term.rows(top, top + u64::from(rows)) {
            let line = scene.line(row, index);
            if line.backgrounds.is_empty() && line.runs.is_empty() {
                continue;
            }
            let _ = writeln!(out, "row {} {:?}", index - top, row.text());
            for span in &line.backgrounds {
                let _ = writeln!(out, "  bg {}+{} {}", span.col, span.cells, hex(span.color));
            }
            for run in &line.runs {
                let style = run.style;
                let _ = write!(
                    out,
                    "  run {}+{} {:?} fg {} bg {}",
                    run.col,
                    run.cells,
                    run.text,
                    hex(style.fg),
                    hex(style.bg)
                );
                for (on, name) in
                    [(style.bold, "bold"), (style.italic, "italic"), (style.strike, "strike")]
                {
                    if on {
                        let _ = write!(out, " {name}");
                    }
                }
                if style.underline != Underline::None {
                    let color = hex(style.underline_color);
                    let _ = write!(out, " underline {:?} {color}", style.underline);
                }
                if style.overline {
                    let _ = write!(out, " overline");
                }
                let _ = writeln!(out);
            }
            // As the last frame shaped the row: each glyph and the cell it sits on.
            if let (Some(shaped), Some(grid)) = (self.cache.rows.get(&index), self.geometry) {
                for (run, line) in shaped.line.runs.iter().zip(&shaped.shaped) {
                    let glyphs = line.runs.iter().flat_map(|run| &run.glyphs);
                    let glyphs: Vec<String> = glyphs
                        .map(|glyph| {
                            format!("{:?}@{:.2}", glyph.id, glyph.position.x / grid.cell.width)
                        })
                        .collect();
                    let _ = writeln!(out, "  glyphs {} {}", run.col, glyphs.join(" "));
                }
            }
        }
        if let Some((start, end)) = selection {
            let _ = writeln!(out, "selected {start:?}..={end:?} {:?}", term.text(start, end));
        }
        out
    }

    /// The cell under `position` by absolute row.
    fn grid_point(&self, position: Point<Pixels>) -> Option<(GridPoint, Side)> {
        let ((row, col), side) = self.cell_at(position)?;
        let top = self.pane.top(&self.pane.lock());
        Some(((top + u64::from(row), col), side))
    }

    /// Reports a mouse event to a program that asked for mouse reports (mode 9, 1000, 1002 or
    /// 1003) and whether the program owns the mouse; Shift keeps it for selecting.
    fn report_mouse(
        &mut self,
        button: Button,
        action: Action,
        position: Point<Pixels>,
        modifiers: gpui::Modifiers,
    ) -> bool {
        let modes = self.pane.lock().modes().clone();
        let (Some(grid), Some((cell, _))) = (self.geometry, self.cell_at(position)) else {
            return false;
        };
        let wheel = matches!(button, Button::WheelUp | Button::WheelDown);
        if modes.mouse == MouseMode::None || modifiers.shift && !wheel {
            return false;
        }
        // Motion within a cell tells a cell-addressed program nothing new.
        let moved = self.mouse_cell.replace(cell) != Some(cell);
        if action == Action::Motion && !moved && modes.mouse_encoding != MouseEncoding::SgrPixels {
            return true;
        }
        let offset = position - grid.origin;
        let report = Report {
            button,
            action,
            modifiers: mouse::Modifiers {
                shift: modifiers.shift,
                alt: modifiers.alt,
                control: modifiers.control,
            },
            cell,
            pixel: (f32::from(offset.x).max(0.) as u32, f32::from(offset.y).max(0.) as u32),
        };
        if mouse::wanted(modes.mouse, &report)
            && let Some(bytes) = mouse::encode(&report, modes.mouse, modes.mouse_encoding)
        {
            self.pane.write(bytes);
        }
        true
    }

    pub fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        cx.emit(PaneEvent::Focused);
        self.pressed = true;
        let button = mouse_button(event.button);
        if self.report_mouse(button, Action::Press, event.position, event.modifiers)
            || event.button != MouseButton::Left
        {
            return;
        }
        let Some((point, side)) = self.grid_point(event.position) else {
            return;
        };
        let kind = match event.click_count {
            2 => Kind::Words,
            3 => Kind::Lines,
            _ => Kind::Cells,
        };
        self.pane.selection = Some(Selection::new(kind, point, side));
        cx.notify();
    }

    /// A release over this pane, or anywhere after a press in it (the program hears it at the
    /// nearest cell); a release that ends another pane's press is not this pane's.
    pub fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if mem::take(&mut self.pressed) {
            let button = mouse_button(event.button);
            self.report_mouse(button, Action::Release, event.position, event.modifiers);
        }
    }

    /// Motion over this pane, with the button held only when it went down here.
    pub fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let pressed = event.pressed_button.filter(|_| self.pressed);
        let button = pressed.map_or(Button::None, mouse_button);
        if self.report_mouse(button, Action::Motion, event.position, event.modifiers)
            || pressed != Some(MouseButton::Left)
        {
            return;
        }
        let Some((point, side)) = self.grid_point(event.position) else {
            return;
        };
        if let Some(selection) = self.pane.selection.as_mut() {
            selection.extend(point, side);
            cx.notify();
        }
    }

    /// The wheel scrolls the scrollback, unless a full-screen program owns it: one that asked for
    /// mouse reports gets wheel buttons, any other gets arrow keys (alternate scroll, mode 1007).
    pub fn scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(grid) = self.geometry else { return };
        self.scroll_remainder += event.delta.pixel_delta(grid.cell.height).y;
        let lines = (self.scroll_remainder / grid.cell.height) as i32;
        if lines == 0 {
            return;
        }
        self.scroll_remainder -= grid.cell.height * lines as f32;
        let count = lines.unsigned_abs() as usize;
        let button = if lines > 0 { Button::WheelUp } else { Button::WheelDown };
        let modes = self.pane.lock().modes().clone();
        if modes.mouse != MouseMode::None {
            for _ in 0..count {
                self.report_mouse(button, Action::Press, event.position, event.modifiers);
            }
        } else if modes.alt_screen && modes.alternate_scroll {
            let arrow = match (lines > 0, modes.cursor_keys) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            self.pane.write(arrow.repeat(count).into_bytes());
        } else {
            self.pane.scroll(lines);
            cx.notify();
        }
    }

    /// Shapes the rows that changed since the last frame, holding the terminal lock only while
    /// reading cells. The window sized the grid to `bounds` (`resize`).
    fn layout(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Frame {
        let started = Instant::now();
        self.pane.drawing();
        let settings = cx.global::<Settings>();
        let (font, font_size) = (settings.font.clone(), settings.font_size);
        let text_system = window.text_system().clone();
        let Metrics { cell, ascent, descent } = metrics(&text_system, &font, font_size);
        let (rows, cols, _) = self.grid;
        let focused = self.focus.is_focused(window) && window.is_window_active();

        let mut term = self.pane.lock();
        if !term.modes().sync_update {
            self.sync_since = None;
        } else if let Some(frame) = self.cache.last.clone().filter(|frame| frame.bounds == bounds) {
            // DEC mode 2026: the program is drawing a frame; keep showing the last one until it
            // is done, or for a while if it never says so.
            if self.sync_since.is_none() {
                self.sync_since = Some(Instant::now());
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SYNC_TIMEOUT).await;
                    this.update(cx, |_, cx| cx.notify()).ok();
                })
                .detach();
            }
            if self.sync_since.is_some_and(|since| since.elapsed() < SYNC_TIMEOUT) {
                return frame;
            }
        }
        let palette = Palette::new(&term, &self.scheme);
        let top = self.pane.top(&term);
        let selection = self.pane.selection.and_then(|selection| selection.range(&term));
        let mut scene = Scene::new(&term, &palette, top, rows, selection, focused);
        self.blink_since = (focused && render::blinks(&term))
            .then(|| self.blink_since.unwrap_or_else(Instant::now));
        let blinking = self.blinking();
        if blinking.is_some_and(blinked_off) {
            scene.cursor = None;
        }
        self.blink_wake = blinking.map(|elapsed| {
            let next = Duration::from_millis((BLINK_MS - elapsed.as_millis() % BLINK_MS) as u64);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(next).await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
        });
        let screen_top = term.screen_top();
        let (cursor_row, cursor_col) = term.cursor_position();
        let cache = &mut self.cache;
        let key = CacheKey {
            palette: palette.clone(),
            colors: (scene.fg, scene.bg),
            selection,
            focused,
            size: (rows, cols),
            font_size,
        };
        if cache.key.as_ref() != Some(&key) {
            cache.rows.clear();
            cache.key = Some(key);
        }
        let cursor = scene.cursor.map(|cursor| cursor.row);
        let damaged = term.take_damage().into_iter().map(|row| screen_top + u64::from(row));
        for row in damaged.chain(cache.cursor_row).chain(cursor) {
            cache.rows.remove(&row);
        }
        let left_screen = cache.screen_top..screen_top;
        let in_view = top..top + u64::from(rows);
        cache.rows.retain(|row, _| in_view.contains(row) && !left_screen.contains(row));
        (cache.screen_top, cache.cursor_row) = (screen_top, cursor);
        let missing: Vec<(u64, Line)> = term
            .rows(top, in_view.end)
            .filter(|(index, _)| !cache.rows.contains_key(index))
            .map(|(index, row)| (index, scene.line(row, index)))
            .collect();
        drop(term);

        for (index, line) in missing {
            let shaped = line
                .runs
                .iter()
                .map(|run| shape(&text_system, &font, font_size, cell.width, run))
                .collect();
            cache.rows.insert(index, Rc::new(ShapedRow { line, shaped }));
        }
        let at = |row: u16, col: u16| {
            point(
                bounds.origin.x + cell.width * f32::from(col),
                bounds.origin.y + cell.height * f32::from(row),
            )
        };
        let screen_row = (screen_top + u64::from(cursor_row)).saturating_sub(top);
        let cursor_bounds =
            Bounds::new(at(screen_row.min(u64::from(rows - 1)) as u16, cursor_col), cell);
        self.geometry =
            Some(Geometry { origin: bounds.origin, cell, rows, cols, cursor: cursor_bounds });

        let cursor = scene.cursor.and_then(|cursor| {
            let width = cell.width * if cursor.wide { 2. } else { 1. };
            let origin = at((cursor.row - top) as u16, cursor.col);
            let color = hsla(cursor.color);
            match cursor.shape {
                // The cell's own background and text are the block.
                CursorShape::Block => None,
                CursorShape::Bar => {
                    Some(fill(Bounds::new(origin, size(px(2.), cell.height)), color))
                }
                CursorShape::Underline => {
                    let origin = origin + point(px(0.), cell.height - px(2.));
                    Some(fill(Bounds::new(origin, size(width, px(2.))), color))
                }
                CursorShape::Hollow => Some(outline(
                    Bounds::new(origin, size(width, cell.height)),
                    color,
                    BorderStyle::Solid,
                )),
            }
        });
        let marked_text = self.marked_text.clone().map(|text| {
            let fg = hsla(palette.color(FOREGROUND));
            let underline = UnderlineStyle { color: Some(fg), thickness: px(1.), wavy: false };
            let run = TextRun {
                underline: Some(underline),
                ..text_run(&font, text.len(), palette.color(FOREGROUND), false, false)
            };
            let line = text_system.shape_line(text.into(), font_size, &[run], None);
            let backdrop = fill(
                Bounds::new(cursor_bounds.origin, size(line.width, cell.height)),
                hsla(scene.bg),
            );
            (backdrop, cursor_bounds.origin, line)
        });
        let padding = (cell.height - ascent - descent) / 2.;
        let baseline = padding + ascent;
        let frame = Frame {
            bounds,
            cell,
            underline: baseline + descent * 0.618,
            strike: (ascent * 0.5 + baseline) * 0.5,
            thickness: px((f32::from(font_size) / 14.).round().max(1.)),
            background: hsla(scene.bg),
            rows: (0..rows)
                .filter_map(|row| Some((row, cache.rows.get(&(top + u64::from(row)))?.clone())))
                .collect(),
            cursor,
            marked_text,
            layout_time: started.elapsed(),
        };
        cache.last = Some(frame.clone());
        frame
    }

    fn paint(
        &mut self,
        bounds: Bounds<Pixels>,
        frame: Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let started = Instant::now();
        window.handle_input(&self.focus, ElementInputHandler::new(bounds, cx.entity()), cx);
        let cell = frame.cell;
        let at = |row: u16, col: u16| {
            point(
                frame.bounds.origin.x + cell.width * f32::from(col),
                frame.bounds.origin.y + cell.height * f32::from(row),
            )
        };
        window.paint_quad(fill(bounds, frame.background));
        for (row, shaped) in &frame.rows {
            for span in &shaped.line.backgrounds {
                let width = cell.width * f32::from(span.cells);
                window.paint_quad(fill(
                    Bounds::new(at(*row, span.col), size(width, cell.height)),
                    hsla(span.color),
                ));
            }
        }
        for (row, shaped) in &frame.rows {
            for (run, line) in shaped.line.runs.iter().zip(&shaped.shaped) {
                let origin = at(*row, run.col);
                if let Err(err) = line.paint(origin, cell.height, window, cx) {
                    eprintln!("rtty: painting {:?} failed: {err}", line.text);
                }
                decorate(run, origin, &frame, window);
            }
        }
        if let Some(cursor) = frame.cursor {
            window.paint_quad(cursor);
        }
        if let Some((backdrop, origin, line)) = frame.marked_text {
            window.paint_quad(backdrop);
            line.paint(origin, cell.height, window, cx).ok();
        }
        if let Some(stats) = self.stats.as_mut() {
            stats.record(frame.layout_time, started.elapsed());
        }
    }
}

/// The run's underline in its style and color, its strikethrough and its overline, across all of
/// its cells.
fn decorate(run: &Run, origin: Point<Pixels>, frame: &Frame, window: &mut Window) {
    let (cell, thick) = (frame.cell, frame.thickness);
    let width = cell.width * f32::from(run.cells);
    let line = |window: &mut Window, y: Pixels, color: Rgb| {
        window.paint_quad(fill(
            Bounds::new(origin + point(px(0.), y), size(width, thick)),
            hsla(color),
        ))
    };
    let underline = run.style.underline_color;
    let y = frame.underline;
    match run.style.underline {
        Underline::None => {}
        Underline::Single => line(window, y, underline),
        Underline::Double => {
            line(window, y - thick, underline);
            line(window, y + thick, underline);
        }
        Underline::Curly => {
            let style =
                UnderlineStyle { color: Some(hsla(underline)), thickness: thick, wavy: true };
            window.paint_underline(origin + point(px(0.), y - thick), width, &style);
        }
        Underline::Dotted => {
            let dots = (width / (thick * 2.)) as usize;
            for dot in 0..dots {
                let at = origin + point(thick * 2. * dot as f32, y);
                window.paint_quad(fill(Bounds::new(at, size(thick, thick)), hsla(underline)));
            }
        }
        // Half a cell of dash in the middle of each cell.
        Underline::Dashed => {
            for col in 0..run.cells {
                let at = origin + point(cell.width * (f32::from(col) + 0.25), y);
                window.paint_quad(fill(
                    Bounds::new(at, size(cell.width / 2., thick)),
                    hsla(underline),
                ));
            }
        }
    }
    if run.style.strike {
        line(window, frame.strike, run.style.fg);
    }
    if run.style.overline {
        line(window, px(0.), run.style.fg);
    }
}

fn blinked_off(blinking: Duration) -> bool {
    blinking.as_millis() / BLINK_MS % 2 == 1
}

fn mouse_button(button: MouseButton) -> Button {
    match button {
        MouseButton::Middle => Button::Middle,
        MouseButton::Right => Button::Right,
        _ => Button::Left,
    }
}

/// A cell of the font, and where its glyphs sit in it.
pub struct Metrics {
    pub cell: Size<Pixels>,
    pub ascent: Pixels,
    pub descent: Pixels,
}

/// The cell is the font's advance of `m` wide and its ascent plus descent tall, whole pixels.
pub fn metrics(text_system: &WindowTextSystem, font: &Font, font_size: Pixels) -> Metrics {
    let font_id = text_system.resolve_font(font);
    let width =
        text_system.advance(font_id, font_size, 'm').expect("the terminal font has an 'm'").width;
    let ascent = text_system.ascent(font_id, font_size);
    let descent = text_system.descent(font_id, font_size).abs();
    Metrics { cell: size(width, (ascent + descent).ceil()), ascent, descent }
}

/// The run shaped as one string, every glyph on its cell.
fn shape(
    text_system: &WindowTextSystem,
    font: &Font,
    font_size: Pixels,
    cell_width: Pixels,
    run: &Run,
) -> ShapedLine {
    let style = &run.style;
    let text_run = text_run(font, run.text.len(), style.fg, style.bold, style.italic);
    let mut line = text_system.shape_line(run.text.clone().into(), font_size, &[text_run], None);
    if run.single_width
        && let Some(snapped) = snap_to_cells(&line, &line.text, cell_width)
    {
        *line = Arc::new(snapped);
    }
    line
}

fn text_run(font: &Font, len: usize, fg: Rgb, bold: bool, italic: bool) -> TextRun {
    let mut font = font.clone();
    if bold {
        font = font.bold();
    }
    if italic {
        font = font.italic();
    }
    TextRun {
        len,
        font,
        color: hsla(fg),
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

impl FrameStats {
    fn record(&mut self, layout: Duration, paint: Duration) {
        let since = *self.since.get_or_insert_with(Instant::now);
        self.frames += 1;
        self.layout += layout;
        self.paint += paint;
        self.worst = self.worst.max(layout + paint);
        if since.elapsed() >= Duration::from_secs(1) {
            self.report();
        }
    }

    fn report(&mut self) {
        let Some(since) = self.since else { return };
        let average = |total: Duration| total.as_secs_f64() * 1000. / f64::from(self.frames);
        eprintln!(
            "rtty: {} frames in {:.2}s, layout {:.2} ms/frame, paint {:.2} ms/frame, worst frame {:.2} ms",
            self.frames,
            since.elapsed().as_secs_f64(),
            average(self.layout),
            average(self.paint),
            self.worst.as_secs_f64() * 1000.,
        );
        *self = Self::default();
    }
}

async fn yield_now() {
    let mut yielded = false;
    future::poll_fn(|cx| {
        if mem::replace(&mut yielded, true) {
            return Poll::Ready(());
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    })
    .await
}

/// The shaped run of single-width cells with every glyph on the cell of the character it was
/// shaped from, or `None` when the shaper already put them there.
fn snap_to_cells(layout: &LineLayout, text: &str, cell_width: Pixels) -> Option<LineLayout> {
    let glyphs = layout.runs.iter().flat_map(|run| &run.glyphs);
    let shaped: Vec<_> = glyphs.map(|glyph| (glyph.index, glyph.position.x)).collect();
    let mut snapped = grid_positions(text, &shaped, cell_width)?.into_iter();
    Some(LineLayout {
        font_size: layout.font_size,
        width: cell_width * text.chars().count() as f32,
        ascent: layout.ascent,
        descent: layout.descent,
        len: layout.len,
        runs: layout
            .runs
            .iter()
            .map(|run| ShapedRun {
                font_id: run.font_id,
                glyphs: run
                    .glyphs
                    .iter()
                    .map(|glyph| ShapedGlyph {
                        position: point(snapped.next().unwrap(), glyph.position.y),
                        ..glyph.clone()
                    })
                    .collect(),
            })
            .collect(),
    })
}

/// Glyphs come from the shaper as (byte index of the first character they draw, x). In a run
/// with one cell per character, each belongs at its character's cell: a ligature over N cells
/// starts on its first cell (the font draws it across all N) and the glyph after it starts N
/// cells later, whatever advances the font or a fallback font reported. `None` when every glyph
/// is already there.
fn grid_positions(
    text: &str,
    glyphs: &[(usize, Pixels)],
    cell_width: Pixels,
) -> Option<Vec<Pixels>> {
    let mut columns = vec![0; text.len()];
    for (column, (byte, c)) in text.char_indices().enumerate() {
        columns[byte..byte + c.len_utf8()].fill(column);
    }
    let snapped: Vec<Pixels> =
        glyphs.iter().map(|&(index, _)| cell_width * columns[index] as f32).collect();
    let on_grid =
        glyphs.iter().zip(&snapped).all(|(&(_, x), &cell_x)| (x - cell_x).abs() < px(0.01));
    (!on_grid).then_some(snapped)
}

pub fn hsla(Rgb(r, g, b): Rgb) -> Hsla {
    let channel = |value: u8| f32::from(value) / 255.;
    Rgba { r: channel(r), g: channel(g), b: channel(b), a: 1. }.into()
}

impl Render for PaneView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout_view = cx.entity();
        let paint_view = cx.entity();
        div()
            .size_full()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .child(
                canvas(
                    move |bounds, window, cx: &mut App| {
                        layout_view.update(cx, |view, cx| view.layout(bounds, window, cx))
                    },
                    move |bounds, frame, window, cx: &mut App| {
                        paint_view.update(cx, |view, cx| view.paint(bounds, frame, window, cx))
                    },
                )
                .size_full(),
            )
    }
}

/// The platform's text input: committed text goes to the pty, composing (marked) text is drawn
/// at the cursor until the input method commits it.
impl EntityInputHandler for PaneView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.marked_text.as_ref().map_or(0, |text| text.encode_utf16().count());
        Some(UTF16Selection { range: end..end, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_text.as_ref().map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        self.input(text.as_bytes().to_vec(), cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!text.is_empty()).then(|| text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.geometry.map(|grid| grid.cursor)
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ligature_keeps_the_glyphs_after_it_on_their_cells() {
        // "->" shaped into one glyph advancing 1.5 cells: "b" belongs on cell 3, not at 2.5 where
        // the advances put it, nor on cell 2 where snapping by glyph order would.
        let shaped = [(0, px(0.)), (1, px(10.)), (3, px(25.))];
        assert_eq!(grid_positions("a->b", &shaped, px(10.)), Some(vec![px(0.), px(10.), px(30.)]));
    }

    #[test]
    fn glyphs_already_on_their_cells_are_left_alone() {
        // Fira Code's way: "->" becomes a spacer glyph plus an arrow glyph, one per cell.
        let shaped = [(0, px(0.)), (1, px(10.)), (2, px(20.)), (3, px(30.))];
        assert_eq!(grid_positions("a->b", &shaped, px(10.)), None);
    }

    #[test]
    fn multibyte_characters_take_one_cell_each() {
        // "λ" is two bytes and "→" three; "=>" ligates; "→" came from a wider fallback font.
        let shaped = [(0, px(0.)), (2, px(10.)), (5, px(21.)), (7, px(29.5))];
        let expected = vec![px(0.), px(10.), px(20.), px(40.)];
        assert_eq!(grid_positions("λ→=>x", &shaped, px(10.)), Some(expected));
    }
}
