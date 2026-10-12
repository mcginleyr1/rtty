//! A window: its tabs, each a tree of split panes (`layout.rs` does the arithmetic), the tab bar
//! while there is more than one tab, and the dividers between panes. Cmd chords arrive here as
//! actions; panes tell the window their titles, their focus, and that their programs exited.
//!
//! The layout is in cells, and every pane's grid and pty follow the cells it gets, shown or not.
//! Each pane draws as a cached view, so output in one pane redraws that pane alone.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    Action, AnyView, App, AppContext, Bounds, Context, Div, DragMoveEvent, EmptyView, Entity,
    MouseButton, Pixels, Point, Render, Size, StyleRefinement, TitlebarOptions, Window,
    WindowAppearance, WindowBounds, WindowHandle, WindowOptions, actions, div, point, prelude::*,
    px, size,
};
use portable_pty::CommandBuilder;
use rtty_vt::BACKGROUND;

use crate::cli::RESIZE_STEP;
use crate::control::{PaneInfo, Request, Response, TabInfo, WindowInfo};
use crate::layout::{Amount, Closed, Direction, Orientation, PaneId, Rect, Tabs};
use crate::pane::{Notice, Pane, START_SIZE};
use crate::scheme::Scheme;
use crate::view::{PaneEvent, PaneView, Settings, hsla, metrics};

actions!(
    rtty,
    [NewWindow, NewTab, ClosePane, SplitRight, SplitDown, ToggleZoom, PreviousTab, NextTab]
);

/// Moves the keyboard to the nearest pane that way. `keystroke` is the chord bound to it, which
/// goes to the pane instead when the pane claimed the keys (`PaneView::claimed_chord`).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = rtty, no_json)]
pub struct MoveFocus {
    pub direction: Direction,
    pub keystroke: String,
}

/// Moves the focused pane's nearest divider across that way; claimable as `MoveFocus` is.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = rtty, no_json)]
pub struct ResizePane {
    pub direction: Direction,
    pub keystroke: String,
}

/// Shows the tab at this index.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = rtty, no_json)]
pub struct SelectTab(pub usize);

/// Above and below a tab's title.
const TAB_PADDING: Pixels = px(6.);

/// A divider being dragged, by its path in the layout.
struct DividerDrag(Vec<bool>);

pub struct TerminalWindow {
    tabs: Tabs,
    views: HashMap<PaneId, Entity<PaneView>>,
    /// The theme's scheme for the window's appearance, for the tab bar and dividers.
    scheme: Arc<Scheme>,
}

/// Opens a window with `command` running in its one pane: the first window centered, later ones
/// cascading from the active one.
pub fn open(command: CommandBuilder, cx: &mut App) -> Result<WindowHandle<TerminalWindow>, String> {
    let scheme = cx.global::<Settings>().theme.for_appearance(cx.window_appearance()).clone();
    let id = next_pane_id();
    let (rows, cols) = START_SIZE;
    let (pane, notices) = spawn(command, id, rows, cols, &scheme, cx)?;
    let first = cx.windows().is_empty();
    let options = WindowOptions {
        window_bounds: first
            .then(|| WindowBounds::Windowed(Bounds::centered(None, size(px(900.), px(600.)), cx))),
        titlebar: Some(TitlebarOptions { title: Some("rtty".into()), ..Default::default() }),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        cx.new(|cx| TerminalWindow::new(id, pane, notices, window, cx))
    })
    .map_err(|err| format!("cannot open a window: {err}"))
}

/// The user's login shell, started in `cwd`, else (or where that is gone) in their home
/// directory.
fn shell(cwd: Option<PathBuf>) -> CommandBuilder {
    let mut command = CommandBuilder::new_default_prog();
    if let Some(dir) = cwd {
        command.cwd(dir);
    }
    command
}

/// Pane ids are unique across windows, so `--trace` lines name one pane.
fn next_pane_id() -> PaneId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Starts pane `id`'s program, which finds its pane and the way to `rtty cli` in `RTTY_PANE` and
/// `RTTY_SOCKET`.
fn spawn(
    mut command: CommandBuilder,
    id: PaneId,
    rows: u16,
    cols: u16,
    scheme: &Scheme,
    cx: &App,
) -> Result<(Pane, UnboundedReceiver<Notice>), String> {
    let settings = cx.global::<Settings>();
    command.env("RTTY_PANE", id.to_string());
    command.env("RTTY_SOCKET", &settings.socket);
    let trace = settings.trace.as_ref().map(|trace| trace.for_pane(id));
    Pane::spawn(command, rows, cols, Some(scheme), trace)
        .map_err(|err| format!("cannot start the shell: {err}"))
}

/// Answers `rtty cli` in the window holding the request's pane, or for none, the active window
/// (else the first) at its focused pane.
pub fn answer(request: Request, cx: &mut App) -> Response {
    let windows: Vec<WindowHandle<TerminalWindow>> =
        cx.windows().into_iter().filter_map(|window| window.downcast()).collect();
    if request == Request::List {
        let info = windows
            .iter()
            .filter_map(|handle| handle.update(cx, |this, window, cx| this.info(window, cx)).ok());
        return Response::Windows(info.collect());
    }
    let pane = match request {
        Request::Focus { pane } => Some(pane),
        Request::FocusToward { from, .. } | Request::Resize { from, .. } => from,
        Request::Split { pane, .. } | Request::Zoom { pane } => pane,
        Request::List => None,
    };
    let holds = |handle: &&WindowHandle<TerminalWindow>, cx: &App| match pane {
        Some(pane) => handle.read(cx).is_ok_and(|this| this.views.contains_key(&pane)),
        None => cx.active_window().map(|active| active.window_id()) == Some(handle.window_id()),
    };
    let target = windows.iter().find(|handle| holds(handle, cx)).or(match pane {
        Some(_) => None,
        None => windows.first(),
    });
    let Some(target) = target else {
        return Response::Error(pane.map_or("no window".into(), |pane| format!("no pane {pane}")));
    };
    target
        .update(cx, |this, window, cx| this.answer(request, window, cx))
        .unwrap_or_else(|err| Response::Error(err.to_string()))
}

impl TerminalWindow {
    fn new(
        id: PaneId,
        pane: Pane,
        notices: UnboundedReceiver<Notice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe_window_appearance(window, |this, window, cx| {
            this.follow_appearance(window.appearance(), cx)
        })
        .detach();
        // The font size sets the cells the layout shares out.
        cx.observe_global_in::<Settings>(window, |_, _, cx| cx.notify()).detach();
        let scheme = cx.global::<Settings>().theme.for_appearance(window.appearance()).clone();
        let mut this = TerminalWindow { tabs: Tabs::new(id), views: HashMap::new(), scheme };
        this.add(id, pane, notices, window, cx);
        this
    }

    /// Shows a new pane; it takes the keyboard.
    fn add(
        &mut self,
        id: PaneId,
        pane: Pane,
        notices: UnboundedReceiver<Notice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| PaneView::new(pane, notices, window, cx));
        cx.subscribe_in(&view, window, move |this, _, event, window, cx| {
            this.pane_event(id, event, window, cx)
        })
        .detach();
        self.views.insert(id, view);
        cx.notify();
    }

    /// The pane with the keyboard, in the tab that shows.
    pub fn focused(&self) -> Option<Entity<PaneView>> {
        self.views.get(&self.tabs.active().focus()).cloned()
    }

    /// The focused pane's working directory (`PaneView::cwd`), for a new pane, tab or window.
    fn focused_cwd(&self, cx: &App) -> Option<PathBuf> {
        self.focused().and_then(|view| view.read(cx).cwd())
    }

    /// The cells the panes share, the size of a cell, and the height of the tab bar above them
    /// (none with one tab).
    fn area(&self, window: &Window, cx: &App) -> (Rect, Size<Pixels>, Pixels) {
        let settings = cx.global::<Settings>();
        let cell = metrics(window.text_system(), &settings.font, settings.font_size).cell;
        let bar = if self.tabs.all().len() > 1 { cell.height + TAB_PADDING * 2. } else { px(0.) };
        let viewport = window.viewport_size();
        // Half a pixel short of a cell still counts: window sizes round.
        let fit = |pixels: Pixels, cell: Pixels| ((pixels + px(0.5)) / cell).max(1.) as u16;
        let area = Rect {
            col: 0,
            row: 0,
            cols: fit(viewport.width, cell.width),
            rows: fit(viewport.height - bar, cell.height),
        };
        (area, cell, bar)
    }

    /// The window size that fits `rows`×`cols` cells of panes.
    pub fn size_for(&self, rows: u16, cols: u16, window: &Window, cx: &App) -> Size<Pixels> {
        let (_, cell, bar) = self.area(window, cx);
        let cells = |count: u16, cell: Pixels| cell * (f32::from(count) + 0.5);
        size(cells(cols, cell.width), bar + cells(rows, cell.height))
    }

    /// The middle of the window's cell (`row`, `col`), counted from the top left of the panes.
    pub fn cell_center(&self, row: u16, col: u16, window: &Window, cx: &App) -> Point<Pixels> {
        let (_, cell, bar) = self.area(window, cx);
        let at = |index: u16, cell: Pixels| cell * (f32::from(index) + 0.5);
        point(at(col, cell.width), bar + at(row, cell.height))
    }

    /// The shown pane under `position`, as the mouse finds it.
    pub fn pane_at(
        &self,
        position: Point<Pixels>,
        window: &Window,
        cx: &App,
    ) -> Option<Entity<PaneView>> {
        let (area, cell, bar) = self.area(window, cx);
        let (x, y) = (position.x / cell.width, (position.y - bar) / cell.height);
        if x < 0. || y < 0. {
            return None;
        }
        let rects = self.tabs.active().rects(area);
        let (id, _) = rects.iter().find(|(_, rect)| rect.contains(x as u16, y as u16))?;
        self.views.get(id).cloned()
    }

    /// The panes the tab that shows draws.
    pub fn shown(&self, window: &Window, cx: &App) -> Vec<Entity<PaneView>> {
        let (area, ..) = self.area(window, cx);
        let rects = self.tabs.active().rects(area);
        rects.iter().filter_map(|(id, _)| self.views.get(id).cloned()).collect()
    }

    fn title(&self, id: PaneId, cx: &App) -> String {
        let title = self.views.get(&id).map(|view| view.read(cx).title()).unwrap_or_default();
        if title.is_empty() { "rtty".into() } else { title }
    }

    /// The keyboard goes to the shown tab's focused pane, whose title names the window.
    fn focus_shown(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.tabs.active().focus();
        if let Some(view) = self.views.get(&focus) {
            view.read(cx).focus(window);
        }
        window.set_window_title(&self.title(focus, cx));
        cx.notify();
    }

    fn pane_event(
        &mut self,
        id: PaneId,
        event: &PaneEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            PaneEvent::Title => {
                if id == self.tabs.active().focus() {
                    window.set_window_title(&self.title(id, cx));
                }
                cx.notify();
            }
            PaneEvent::Focused => {
                if self.tabs.active().panes().contains(&id) {
                    self.tabs.active_mut().set_focus(id);
                    window.set_window_title(&self.title(id, cx));
                    cx.notify();
                }
            }
            PaneEvent::Exited => self.close(id, window, cx),
        }
    }

    /// Closes a pane, its tab when it was the last there, and the window when that was the last
    /// tab. Its program, if still running, is hung up on.
    fn close(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(closed) = self.tabs.close(id) else { return };
        self.views.remove(&id);
        match closed {
            Closed::Window => window.remove_window(),
            Closed::Pane | Closed::Tab => self.focus_shown(window, cx),
        }
    }

    /// Splits `from`, showing its tab; the new pane starts in `from`'s working directory and
    /// takes the keyboard.
    fn split(
        &mut self,
        from: PaneId,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PaneId, String> {
        self.tabs.select(self.tabs.find(from).ok_or(format!("no pane {from}"))?);
        let (area, ..) = self.area(window, cx);
        let id = next_pane_id();
        let mut layout = self.tabs.active().clone();
        layout.split(from, direction, id);
        let rects = layout.rects(area);
        let (_, rect) =
            rects.iter().find(|(pane, _)| *pane == id).expect("the new pane is laid out");
        let cwd = self.views.get(&from).and_then(|view| view.read(cx).cwd());
        let (pane, notices) = spawn(shell(cwd), id, rect.rows, rect.cols, &self.scheme, cx)?;
        *self.tabs.active_mut() = layout;
        self.add(id, pane, notices, window, cx);
        Ok(id)
    }

    fn split_focused(&mut self, direction: Direction, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.split(self.tabs.active().focus(), direction, window, cx) {
            eprintln!("rtty: {err}");
        }
    }

    /// Shows `pane`'s tab and gives it the keyboard.
    fn show(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.find(pane) {
            self.tabs.select(tab);
            self.tabs.active_mut().set_focus(pane);
            self.focus_shown(window, cx);
        }
    }

    /// `rtty cli`'s request, for a pane in this window (or with none, the focused pane).
    fn answer(
        &mut self,
        request: Request,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Response {
        let focused = self.tabs.active().focus();
        match request {
            Request::List => Response::Windows(vec![self.info(window, cx)]),
            Request::Focus { pane } => {
                self.show(pane, window, cx);
                Response::Done
            }
            Request::FocusToward { from, direction, wrap } => {
                let from = from.unwrap_or(focused);
                match self.focus_toward(from, direction, wrap, window, cx) {
                    Some(pane) => Response::Pane(pane),
                    None => Response::Edge { from, direction },
                }
            }
            Request::Resize { from, direction, amount } => {
                self.resize(from.unwrap_or(focused), direction, amount, window, cx);
                Response::Done
            }
            Request::Split { pane, direction } => self
                .split(pane.unwrap_or(focused), direction, window, cx)
                .map_or_else(Response::Error, Response::Pane),
            Request::Zoom { pane } => {
                self.show(pane.unwrap_or(focused), window, cx);
                self.tabs.active_mut().toggle_zoom();
                cx.notify();
                Response::Done
            }
        }
    }

    /// The window's tabs and panes, for `rtty cli list`.
    fn info(&self, window: &Window, cx: &App) -> WindowInfo {
        let tabs = self.tabs.all().iter().enumerate().map(|(index, tab)| TabInfo {
            active: index == self.tabs.index(),
            zoomed: tab.zoomed(),
            panes: tab
                .panes()
                .into_iter()
                .filter_map(|id| {
                    let view = self.views.get(&id)?.read(cx);
                    let (rows, cols) = view.size();
                    Some(PaneInfo {
                        id,
                        focused: id == tab.focus(),
                        title: view.title(),
                        cwd: view.cwd(),
                        rows,
                        cols,
                        claims_keys: view.claims_keys(),
                    })
                })
                .collect(),
        });
        WindowInfo {
            id: window.window_handle().window_id().as_u64(),
            active: window.is_window_active(),
            tabs: tabs.collect(),
        }
    }

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let (area, ..) = self.area(window, cx);
        let id = next_pane_id();
        match spawn(shell(self.focused_cwd(cx)), id, area.rows, area.cols, &self.scheme, cx) {
            Ok((pane, notices)) => {
                self.tabs.open(id);
                self.add(id, pane, notices, window, cx);
            }
            Err(err) => eprintln!("rtty: {err}"),
        }
    }

    fn new_window(&mut self, _: &NewWindow, _: &mut Window, cx: &mut Context<Self>) {
        let command = shell(self.focused_cwd(cx));
        // Opened once this window's update is done, so it cascades from this one.
        cx.defer(move |cx| {
            if let Err(err) = open(command, cx) {
                eprintln!("rtty: {err}");
            }
        });
    }

    /// Moves the keyboard from `from` to the nearest pane in `direction` (with `wrap`, round to
    /// the farthest on the other side), showing their tab; the pane it went to, or `None` at an
    /// edge. The one call for everything that navigates: Cmd+Opt+arrows, `rtty cli pane focus`,
    /// and so an editor moving on past its own last window.
    pub fn focus_toward(
        &mut self,
        from: PaneId,
        direction: Direction,
        wrap: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        self.tabs.select(self.tabs.find(from)?);
        let (area, ..) = self.area(window, cx);
        let moved = self.tabs.active_mut().focus_toward(from, direction, area, wrap);
        self.focus_shown(window, cx);
        moved
    }

    /// Moves the divider nearest `from` across `direction` by `amount`.
    fn resize(
        &mut self,
        from: PaneId,
        direction: Direction,
        amount: Amount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.find(from) else { return };
        self.tabs.select(tab);
        let (area, ..) = self.area(window, cx);
        self.tabs.active_mut().resize(from, direction, amount, area);
        self.focus_shown(window, cx);
    }

    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.select(index);
        self.focus_shown(window, cx);
    }

    fn cycle_tab(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.cycle(step);
        self.focus_shown(window, cx);
    }

    fn drag_divider(
        &mut self,
        event: &DragMoveEvent<DividerDrag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (area, cell, bar) = self.area(window, cx);
        let position = event.event.position;
        let col = (position.x / cell.width).max(0.) as u16;
        let row = ((position.y - bar) / cell.height).max(0.) as u16;
        let path = event.drag(cx).0.clone();
        self.tabs.active_mut().drag(&path, area, (col, row));
        cx.notify();
    }

    /// macOS switched between light and dark: every pane, shown or not, recolors and tells its
    /// program; the tab bar and dividers recolor.
    pub fn follow_appearance(&mut self, appearance: WindowAppearance, cx: &mut Context<Self>) {
        self.scheme = cx.global::<Settings>().theme.for_appearance(appearance).clone();
        for view in self.views.values() {
            view.update(cx, |view, cx| view.follow_appearance(appearance, cx));
        }
        cx.notify();
    }

    /// `--script`'s `dump`: the window's tabs and where the shown panes are, then what each of
    /// them draws.
    pub fn dump(&self, window: &Window, cx: &App) -> String {
        let (area, ..) = self.area(window, cx);
        let layout = self.tabs.active();
        let mut out = String::new();
        let _ = writeln!(
            out,
            "window {}x{} tab {} of {} focus {} zoomed {} title {:?}",
            area.rows,
            area.cols,
            self.tabs.index() + 1,
            self.tabs.all().len(),
            layout.focus(),
            layout.zoomed(),
            self.title(layout.focus(), cx),
        );
        let labels: Vec<String> =
            (0..self.tabs.all().len()).map(|index| self.tab_label(index, cx)).collect();
        let _ = writeln!(out, "windows {} tabs {labels:?}", cx.windows().len());
        let rects = layout.rects(area);
        for (id, rect) in &rects {
            let _ =
                writeln!(out, "pane {id} at {},{} {}x{}", rect.row, rect.col, rect.rows, rect.cols);
        }
        for (id, _) in &rects {
            if let Some(view) = self.views.get(id) {
                let _ = write!(out, "\n== pane {id}\n{}", view.read(cx).dump(window));
            }
        }
        out
    }

    /// A tab's number, for Cmd+1–9, and the title of its focused pane.
    fn tab_label(&self, index: usize, cx: &App) -> String {
        format!("{}: {}", index + 1, self.title(self.tabs.all()[index].focus(), cx))
    }

    fn tab_bar(&self, height: Pixels, cx: &mut Context<Self>) -> Div {
        let settings = cx.global::<Settings>();
        let (font, font_size) = (settings.font.clone(), settings.font_size);
        let tabs = (0..self.tabs.all().len()).map(|index| {
            let (fg, bg) = if index == self.tabs.index() {
                self.scheme.active_tab
            } else {
                self.scheme.inactive_tab
            };
            div()
                .id(("tab", index))
                .flex_1()
                .max_w(px(240.))
                .h_full()
                .px(TAB_PADDING * 2.)
                .flex()
                .items_center()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .bg(hsla(bg))
                .text_color(hsla(fg))
                .child(self.tab_label(index, cx))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| this.select_tab(index, window, cx)),
                )
        });
        div()
            .flex()
            .w_full()
            .h(height)
            .font(font)
            .text_size(font_size)
            .bg(hsla(self.scheme.tab_bar))
            .children(tabs.collect::<Vec<_>>())
    }
}

impl Render for TerminalWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (area, cell, bar) = self.area(window, cx);
        for tab in self.tabs.all() {
            for (id, rect) in tab.grids(area) {
                if let Some(view) = self.views.get(&id) {
                    view.update(cx, |view, _| view.resize(rect.rows, rect.cols, cell));
                }
            }
        }
        let place = |rect: Rect| {
            let origin =
                point(cell.width * f32::from(rect.col), bar + cell.height * f32::from(rect.row));
            Bounds::new(
                origin,
                size(cell.width * f32::from(rect.cols), cell.height * f32::from(rect.rows)),
            )
        };
        let positioned = |bounds: Bounds<Pixels>| {
            div()
                .absolute()
                .left(bounds.origin.x)
                .top(bounds.origin.y)
                .w(bounds.size.width)
                .h(bounds.size.height)
        };
        let layout = self.tabs.active();
        let panes: Vec<_> = layout
            .rects(area)
            .into_iter()
            .filter_map(|(id, rect)| {
                let view = AnyView::from(self.views.get(&id)?.clone());
                Some(
                    positioned(place(rect))
                        .child(view.cached(StyleRefinement::default().size_full())),
                )
            })
            .collect();
        let split = hsla(self.scheme.split);
        // A divider is a cell across, with a line down (or along) its middle.
        let dividers: Vec<_> = layout
            .dividers(area)
            .into_iter()
            .enumerate()
            .map(|(index, divider)| {
                let line = match divider.orientation {
                    Orientation::SideBySide => div().ml(cell.width / 2.).w(px(1.)).h_full(),
                    Orientation::Stacked => div().mt(cell.height / 2.).h(px(1.)).w_full(),
                };
                let handle = positioned(place(divider.rect)).id(("divider", index));
                let handle = match divider.orientation {
                    Orientation::SideBySide => handle.cursor_col_resize(),
                    Orientation::Stacked => handle.cursor_row_resize(),
                };
                handle
                    .on_drag(DividerDrag(divider.path), |_, _, _, cx| cx.new(|_| EmptyView))
                    .child(line.bg(split))
            })
            .collect();
        let tab_bar = (bar > px(0.)).then(|| self.tab_bar(bar, cx));
        div()
            .relative()
            .size_full()
            .bg(hsla(self.scheme.color(BACKGROUND)))
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                this.split_focused(Direction::Right, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                this.split_focused(Direction::Down, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ClosePane, window, cx| {
                this.close(this.tabs.active().focus(), window, cx)
            }))
            .on_action(cx.listener(|this, action: &MoveFocus, window, cx| {
                this.focus_toward(this.tabs.active().focus(), action.direction, false, window, cx);
            }))
            .on_action(cx.listener(|this, action: &ResizePane, window, cx| {
                this.resize(this.tabs.active().focus(), action.direction, RESIZE_STEP, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleZoom, _, cx| {
                this.tabs.active_mut().toggle_zoom();
                cx.notify();
            }))
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::new_window))
            .on_action(cx.listener(|this, action: &SelectTab, window, cx| {
                this.select_tab(action.0, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &PreviousTab, window, cx| this.cycle_tab(-1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle_tab(1, window, cx)))
            .on_drag_move(cx.listener(Self::drag_divider))
            .children(tab_bar)
            .children(panes)
            .children(dividers)
    }
}
