use std::iter;
use std::mem;
use std::ops::Range;
use std::task::Poll;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use futures::channel::mpsc::UnboundedReceiver;
use futures::{StreamExt, future};
use gpui::{
    App, BorderStyle, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler,
    FocusHandle, Font, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, PaintQuad,
    Pixels, Point, Render, Rgba, ScrollWheelEvent, ShapedLine, Size, StrikethroughStyle, TextRun,
    UTF16Selection, UnderlineStyle, Window, actions, canvas, div, fill, outline, point, prelude::*,
    px, size,
};

use crate::keys;
use crate::palette;
use crate::session::Session;

actions!(rtty, [Quit, Copy, Paste, IncreaseFontSize, DecreaseFontSize]);

pub struct TerminalView {
    session: Session,
    focus: FocusHandle,
    font: Font,
    font_size: Pixels,
    grid: Option<GridGeometry>,
    marked_text: Option<String>,
    scroll_remainder: Pixels,
    stats: Option<FrameStats>,
}

/// Where the last frame put the cells, for mapping mouse positions and placing the IME window.
#[derive(Clone, Copy)]
struct GridGeometry {
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    size: WindowSize,
    cursor: Bounds<Pixels>,
}

struct Frame {
    line_height: Pixels,
    backgrounds: Vec<PaintQuad>,
    text: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Option<PaintQuad>,
    marked_text: Option<(PaintQuad, Point<Pixels>, ShapedLine)>,
    layout_time: Duration,
}

/// Consecutive cells of a row that share a text style; wide and combining cells stand alone so
/// every glyph of a multi-cell span can be pinned to its own cell. `cells` counts the merged
/// single-width cells.
struct TextSpan {
    line: usize,
    column: usize,
    cells: usize,
    text: String,
    fg: Rgb,
    flags: Flags,
    single_width: bool,
}

struct BackgroundSpan {
    line: usize,
    column: usize,
    cells: usize,
    color: Rgb,
}

/// The visible cells, read under the terminal lock; backgrounds only where they differ from the
/// default background.
struct Cells {
    background: Rgb,
    backgrounds: Vec<BackgroundSpan>,
    texts: Vec<TextSpan>,
    cursor_line: usize,
    cursor_column: usize,
    cursor_shape: CursorShape,
    cursor_color: Rgb,
}

#[derive(Default)]
struct FrameStats {
    since: Option<Instant>,
    frames: u32,
    layout: Duration,
    paint: Duration,
    worst: Duration,
}

impl TerminalView {
    pub fn new(
        session: Session,
        mut events: UnboundedReceiver<Event>,
        font: Font,
        font_size: Pixels,
        stats: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        cx.observe_window_activation(window, |_, _, cx| cx.notify()).detach();
        cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.next().await {
                let batch: Vec<Event> =
                    iter::once(event).chain(iter::from_fn(|| events.try_recv().ok())).collect();
                let applied = this.update_in(cx, |view, window, cx| {
                    batch.into_iter().for_each(|event| view.handle_event(event, window, cx))
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
        Self {
            session,
            focus,
            font,
            font_size,
            grid: None,
            marked_text: None,
            scroll_remainder: px(0.),
            stats: stats.then(FrameStats::default),
        }
    }

    fn handle_event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::Wakeup => cx.notify(),
            Event::Title(title) => window.set_window_title(&title),
            Event::ResetTitle => window.set_window_title("rtty"),
            Event::PtyWrite(text) => self.session.write(text.into_bytes()),
            Event::ClipboardStore(_, text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text))
            }
            Event::ClipboardLoad(_, format) => {
                let text =
                    cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default();
                self.session.write(format(&text).into_bytes());
            }
            Event::ColorRequest(index, format) => {
                let color = self.session.term.lock().colors()[index];
                let rgb = color.unwrap_or_else(|| palette::default(index));
                self.session.write(format(rgb).into_bytes());
            }
            Event::TextAreaSizeRequest(format) => {
                if let Some(grid) = self.grid {
                    self.session.write(format(grid.size).into_bytes());
                }
            }
            Event::ChildExit(_) | Event::Exit => {
                if let Some(stats) = self.stats.as_mut() {
                    stats.report();
                }
                cx.quit()
            }
            Event::MouseCursorDirty | Event::CursorBlinkingChange | Event::Bell => {}
        }
    }

    fn input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let mut term = self.session.term.lock();
        term.selection = None;
        term.scroll_display(Scroll::Bottom);
        drop(term);
        self.session.write(bytes);
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let app_cursor = self.session.term.lock().mode().contains(TermMode::APP_CURSOR);
        // A held key repeats here: the input handler would open the accent picker instead.
        let bytes = keys::encode(keystroke, app_cursor).or_else(|| {
            event.is_held.then(|| keystroke.key_char.clone()).flatten().map(String::into_bytes)
        });
        if bytes.is_some() || (keystroke.modifiers.control && !keystroke.modifiers.platform) {
            cx.stop_propagation();
        }
        if let Some(bytes) = bytes {
            self.input(bytes, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.session.term.lock().selection_to_string() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let bracketed = self.session.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        self.input(keys::paste(&text, bracketed), cx);
    }

    fn increase_font_size(&mut self, _: &IncreaseFontSize, _: &mut Window, cx: &mut Context<Self>) {
        self.font_size = (self.font_size + px(1.)).min(px(72.));
        cx.notify();
    }

    fn decrease_font_size(&mut self, _: &DecreaseFontSize, _: &mut Window, cx: &mut Context<Self>) {
        self.font_size = (self.font_size - px(1.)).max(px(6.));
        cx.notify();
    }

    /// The cell under `position`, as a point `display_offset` lines into the scrollback.
    fn grid_point(
        &self,
        position: Point<Pixels>,
        display_offset: usize,
    ) -> Option<(GridPoint, Side)> {
        let grid = self.grid?;
        let x = ((position.x - grid.origin.x) / grid.cell.width).max(0.);
        let y = ((position.y - grid.origin.y) / grid.cell.height).max(0.);
        let column = (x as usize).min(usize::from(grid.size.num_cols) - 1);
        let line = (y as usize).min(usize::from(grid.size.num_lines) - 1);
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        let line = Line(line as i32 - display_offset as i32);
        Some((GridPoint::new(line, Column(column)), side))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        let mut term = self.session.term.lock();
        let Some((point, side)) = self.grid_point(event.position, term.grid().display_offset())
        else {
            return;
        };
        let kind = match event.click_count {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        term.selection = Some(Selection::new(kind, point, side));
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let mut term = self.session.term.lock();
        let Some((point, side)) = self.grid_point(event.position, term.grid().display_offset())
        else {
            return;
        };
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
            cx.notify();
        }
    }

    /// The wheel scrolls the scrollback, unless a full-screen program owns it: one that asked for
    /// mouse reports gets wheel buttons, any other gets arrow keys (xterm's alternate scroll).
    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(grid) = self.grid else { return };
        self.scroll_remainder += event.delta.pixel_delta(grid.cell.height).y;
        let lines = (self.scroll_remainder / grid.cell.height) as i32;
        if lines == 0 {
            return;
        }
        self.scroll_remainder -= grid.cell.height * lines as f32;
        let count = lines.unsigned_abs() as usize;
        let mut term = self.session.term.lock();
        let mode = *term.mode();
        if mode.intersects(TermMode::MOUSE_MODE) {
            let Some((point, _)) = self.grid_point(event.position, 0) else {
                return;
            };
            let (column, line) = (point.column.0 + 1, point.line.0 as usize + 1);
            let button = if lines > 0 { 64 } else { 65 };
            let report = if mode.contains(TermMode::SGR_MOUSE) {
                format!("\x1b[<{button};{column};{line}M").into_bytes()
            } else {
                let encode = |n: usize| (32 + n).min(255) as u8;
                vec![0x1b, b'[', b'M', 32 + button, encode(column), encode(line)]
            };
            self.session.write(report.repeat(count));
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            let arrow = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            self.session.write(arrow.repeat(count).into_bytes());
        } else {
            term.scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }

    /// Fits the grid to `bounds`, then turns the visible cells into background quads and shaped
    /// text runs, holding the terminal lock only while reading cells.
    fn layout(&mut self, bounds: Bounds<Pixels>, window: &mut Window) -> Frame {
        let started = Instant::now();
        let text_system = window.text_system().clone();
        let font_id = text_system.resolve_font(&self.font);
        let cell_width = text_system
            .advance(font_id, self.font_size, 'm')
            .expect("the terminal font has an 'm'")
            .width;
        let ascent = text_system.ascent(font_id, self.font_size);
        let descent = text_system.descent(font_id, self.font_size);
        let cell = size(cell_width, (ascent + descent.abs()).ceil());
        let columns = ((bounds.size.width / cell.width) as u16).max(2);
        let lines = ((bounds.size.height / cell.height) as u16).max(1);
        let window_size = WindowSize {
            num_lines: lines,
            num_cols: columns,
            cell_width: f32::from(cell.width) as u16,
            cell_height: f32::from(cell.height) as u16,
        };
        if self
            .grid
            .is_none_or(|grid| (grid.size.num_cols, grid.size.num_lines) != (columns, lines))
        {
            self.session.resize(window_size);
        }
        let focused = self.focus.is_focused(window) && window.is_window_active();
        let cells = self.read_cells(focused);

        let at = |line: usize, column: usize| {
            point(
                bounds.origin.x + cell.width * column as f32,
                bounds.origin.y + cell.height * line as f32,
            )
        };
        let cursor_bounds = Bounds::new(at(cells.cursor_line, cells.cursor_column), cell);
        self.grid = Some(GridGeometry {
            origin: bounds.origin,
            cell,
            size: window_size,
            cursor: cursor_bounds,
        });

        let backgrounds = iter::once(fill(bounds, hsla(cells.background)))
            .chain(cells.backgrounds.into_iter().map(|span| {
                let width = cell.width * span.cells as f32;
                fill(
                    Bounds::new(at(span.line, span.column), size(width, cell.height)),
                    hsla(span.color),
                )
            }))
            .collect();
        let decorated = Flags::ALL_UNDERLINES | Flags::STRIKEOUT;
        let text = cells
            .texts
            .into_iter()
            .filter(|span| !span.text.trim().is_empty() || span.flags.intersects(decorated))
            .map(|span| {
                let run = self.text_run(span.text.len(), span.fg, span.flags);
                let force_width = span.single_width.then_some(cell.width);
                let line =
                    text_system.shape_line(span.text.into(), self.font_size, &[run], force_width);
                (at(span.line, span.column), line)
            })
            .collect();
        let cursor_color = hsla(cells.cursor_color);
        let cursor_quad = match cells.cursor_shape {
            CursorShape::Beam => Some(fill(
                Bounds::new(cursor_bounds.origin, size(px(2.), cell.height)),
                cursor_color,
            )),
            CursorShape::Underline => Some(fill(
                Bounds::new(
                    cursor_bounds.bottom_left() - point(px(0.), px(2.)),
                    size(cell.width, px(2.)),
                ),
                cursor_color,
            )),
            CursorShape::HollowBlock => {
                Some(outline(cursor_bounds, cursor_color, BorderStyle::Solid))
            }
            CursorShape::Block | CursorShape::Hidden => None,
        };
        let marked_text = self.marked_text.clone().map(|text| {
            let fg = palette::default(NamedColor::Foreground as usize);
            let run = self.text_run(text.len(), fg, Flags::UNDERLINE);
            let line = text_system.shape_line(text.into(), self.font_size, &[run], None);
            let backdrop = fill(
                Bounds::new(cursor_bounds.origin, size(line.width, cell.height)),
                hsla(cells.background),
            );
            (backdrop, cursor_bounds.origin, line)
        });
        Frame {
            line_height: cell.height,
            backgrounds,
            text,
            cursor: cursor_quad,
            marked_text,
            layout_time: started.elapsed(),
        }
    }

    fn read_cells(&self, focused: bool) -> Cells {
        let term = self.session.term.lock();
        let content = term.renderable_content();
        let colors = content.colors;
        let default_bg = palette::resolve(Color::Named(NamedColor::Background), colors);
        let cursor_color = palette::resolve(Color::Named(NamedColor::Cursor), colors);
        let cursor = content.cursor;
        let shape = match cursor.shape {
            CursorShape::Hidden => CursorShape::Hidden,
            _ if !focused => CursorShape::HollowBlock,
            shape => shape,
        };
        let offset = content.display_offset;
        let selection = content.selection;
        let under_block_cursor = |point: GridPoint, flags: Flags| {
            shape == CursorShape::Block
                && (point == cursor.point
                    || flags.contains(Flags::WIDE_CHAR_SPACER)
                        && GridPoint::new(point.line, point.column - 1) == cursor.point)
        };
        let mut backgrounds: Vec<BackgroundSpan> = Vec::new();
        let mut texts: Vec<TextSpan> = Vec::new();
        for indexed in content.display_iter {
            let cell = indexed.cell;
            let line = (indexed.point.line.0 + offset as i32) as usize;
            let column = indexed.point.column.0;
            let mut fg = palette::resolve(cell.fg, colors);
            let mut bg = palette::resolve(cell.bg, colors);
            if cell.flags.contains(Flags::DIM) {
                fg = palette::dim(fg);
            }
            let selected = selection.is_some_and(|range| range.contains(indexed.point));
            if cell.flags.contains(Flags::INVERSE) != selected {
                mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            if under_block_cursor(indexed.point, cell.flags) {
                (fg, bg) = (bg, cursor_color);
            }

            match backgrounds.last_mut() {
                Some(span)
                    if span.line == line
                        && span.column + span.cells == column
                        && span.color == bg =>
                {
                    span.cells += 1
                }
                _ if bg != default_bg => {
                    backgrounds.push(BackgroundSpan { line, column, cells: 1, color: bg })
                }
                _ => {}
            }

            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let flags =
                cell.flags & (Flags::BOLD_ITALIC | Flags::ALL_UNDERLINES | Flags::STRIKEOUT);
            let single_width = !cell.flags.contains(Flags::WIDE_CHAR) && cell.zerowidth().is_none();
            match texts.last_mut() {
                Some(span)
                    if single_width
                        && span.single_width
                        && span.line == line
                        && span.column + span.cells == column
                        && span.fg == fg
                        && span.flags == flags =>
                {
                    span.text.push(cell.c);
                    span.cells += 1;
                }
                _ => texts.push(TextSpan {
                    line,
                    column,
                    cells: 1,
                    text: iter::once(cell.c)
                        .chain(cell.zerowidth().into_iter().flatten().copied())
                        .collect(),
                    fg,
                    flags,
                    single_width,
                }),
            }
        }
        let cursor_line = (cursor.point.line.0 + offset as i32).max(0) as usize;
        Cells {
            background: default_bg,
            backgrounds,
            texts,
            cursor_line,
            cursor_column: cursor.point.column.0,
            cursor_shape: if cursor_line < term.screen_lines() {
                shape
            } else {
                CursorShape::Hidden
            },
            cursor_color,
        }
    }

    fn text_run(&self, len: usize, fg: Rgb, flags: Flags) -> TextRun {
        let color = hsla(fg);
        let mut font = self.font.clone();
        if flags.contains(Flags::BOLD) {
            font = font.bold();
        }
        if flags.contains(Flags::ITALIC) {
            font = font.italic();
        }
        TextRun {
            len,
            font,
            color,
            background_color: None,
            underline: flags.intersects(Flags::ALL_UNDERLINES).then_some(UnderlineStyle {
                color: Some(color),
                thickness: px(1.),
                wavy: flags.contains(Flags::UNDERCURL),
            }),
            strikethrough: flags
                .contains(Flags::STRIKEOUT)
                .then_some(StrikethroughStyle { color: Some(color), thickness: px(1.) }),
        }
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
        frame.backgrounds.into_iter().for_each(|quad| window.paint_quad(quad));
        for (origin, line) in &frame.text {
            if let Err(err) = line.paint(*origin, frame.line_height, window, cx) {
                eprintln!("rtty: painting {:?} failed: {err}", line.text);
            }
        }
        if let Some(cursor) = frame.cursor {
            window.paint_quad(cursor);
        }
        if let Some((backdrop, origin, line)) = frame.marked_text {
            window.paint_quad(backdrop);
            line.paint(origin, frame.line_height, window, cx).ok();
        }
        if let Some(stats) = self.stats.as_mut() {
            stats.record(frame.layout_time, started.elapsed());
        }
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

fn hsla(rgb: Rgb) -> Hsla {
    let channel = |value: u8| f32::from(value) / 255.;
    Rgba { r: channel(rgb.r), g: channel(rgb.g), b: channel(rgb.b), a: 1. }.into()
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout_view = cx.entity();
        let paint_view = cx.entity();
        div()
            .size_full()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::increase_font_size))
            .on_action(cx.listener(Self::decrease_font_size))
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .child(
                canvas(
                    move |bounds, window, cx: &mut App| {
                        layout_view.update(cx, |view, _| view.layout(bounds, window))
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
impl EntityInputHandler for TerminalView {
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
        self.grid.map(|grid| grid.cursor)
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
