//! A window: its tabs, each a tree of split panes (`layout.rs` does the arithmetic), the bar in
//! its titlebar (`tabbar.rs` places it), and the dividers between panes. Cmd chords arrive here as
//! actions; panes tell the window their titles, their focus, and that their programs exited.
//!
//! The layout is in cells, and every pane's grid and pty follow the cells it gets, shown or not.
//! Each pane draws as a cached view, so output in one pane redraws that pane alone.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    Action, AnyElement, AnyView, App, AppContext, AssetSource, Bounds, Context, Div, DragMoveEvent,
    EmptyView, Entity, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, Render, SharedString, Size, Stateful, StyleRefinement, TitlebarOptions, Window,
    WindowAppearance, WindowBounds, WindowHandle, WindowOptions, actions, div, point, prelude::*,
    px, size, svg,
};
use portable_pty::CommandBuilder;
use rtty_vt::{BACKGROUND, FOREGROUND, Rgb};

use crate::cli::RESIZE_STEP;
use crate::control::{PaneInfo, Request, Response, TabInfo, WindowInfo};
use crate::layout::{Amount, Closed, Direction, Orientation, PaneId, Rect, Tabs};
use crate::notify;
use crate::pane::{Notice, Pane, START_SIZE};
use crate::scheme::Scheme;
use crate::tabbar::{self, Bar, Hit};
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

/// How often panes look at their foreground process and directory for their titles.
const TITLE_REFRESH: Duration = Duration::from_secs(1);

/// The bar's text is the system's, as a titlebar's is.
const UI_FONT: &str = ".SystemUIFont";

/// The bar's icons, drawn for rtty on a 16-unit grid; GPUI tints them with the text color.
const ICONS: [(&str, &str); 3] = [
    (
        "folder.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M1 3.75A1.25 1.25 0 0 1 2.25 2.5h3.7a1.25 1.25 0 0 1 .9.38L8 4h5.75A1.25 1.25 0 0 1 15 5.25v7A1.25 1.25 0 0 1 13.75 13.5H2.25A1.25 1.25 0 0 1 1 12.25z"/></svg>"#,
    ),
    (
        "close.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M3.5 3.5l9 9M12.5 3.5l-9 9" stroke="black" stroke-width="2.2" stroke-linecap="round"/></svg>"#,
    ),
    (
        "plus.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M8 2.5v11M2.5 8h11" stroke="black" stroke-width="1.8" stroke-linecap="round"/></svg>"#,
    ),
];

/// Serves `ICONS` to GPUI's `svg()`.
pub struct Icons;

impl AssetSource for Icons {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let icon = ICONS.iter().find(|(name, _)| *name == path);
        Ok(icon.map(|(_, svg)| Cow::Borrowed(svg.as_bytes())))
    }

    fn list(&self, _: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(ICONS.iter().map(|(name, _)| SharedString::from(*name)).collect())
    }
}

/// A divider being dragged, by its path in the layout.
struct DividerDrag(Vec<bool>);

pub struct TerminalWindow {
    tabs: Tabs,
    views: HashMap<PaneId, Entity<PaneView>>,
    /// The theme's scheme for the window's appearance, for the tab bar and dividers.
    scheme: Arc<Scheme>,
    /// What in the bar the pointer is over, and what a press there began on: a close button or
    /// the + acts on a release over the same thing, as a button does.
    bar_hover: Option<Hit>,
    bar_press: Option<Hit>,
    /// What bells and notifications asked of macOS, and what came of it, for `--script`'s dump.
    attention: Vec<String>,
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
        // The window draws its own titlebar, the bar, under the traffic lights.
        titlebar: Some(TitlebarOptions {
            title: Some("rtty".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(
                px(tabbar::TRAFFIC_LIGHTS.0),
                px(tabbar::TRAFFIC_LIGHTS.1),
            )),
        }),
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
    let spawned = Pane::spawn(command, rows, cols, Some(scheme), trace)
        .map_err(|err| format!("cannot start the shell: {err}"));
    match &spawned {
        Ok(_) => crate::note(&format!("pane {id}: started at {rows}x{cols}")),
        Err(err) => crate::note(&format!("pane {id}: {err}")),
    }
    spawned
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
        // Nothing tells a terminal that a command started or the shell changed directory, short
        // of the shell's own reports: look, cheaply, once a second.
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(TITLE_REFRESH).await;
                if this.update_in(cx, |this, window, cx| this.refresh_titles(window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let scheme = cx.global::<Settings>().theme.for_appearance(window.appearance()).clone();
        // The title dims while the window is in the background, as macOS's do.
        cx.observe_window_activation(window, |_, _, cx| cx.notify()).detach();
        let mut this = TerminalWindow {
            tabs: Tabs::new(id),
            views: HashMap::new(),
            scheme,
            bar_hover: None,
            bar_press: None,
            attention: Vec::new(),
        };
        this.add(id, pane, notices, window, cx);
        this
    }

    /// Shows a new pane, which the layout already focuses; it takes the keyboard and names the
    /// window.
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
        self.focus_shown(window, cx);
    }

    /// The pane with the keyboard, in the tab that shows.
    pub fn focused(&self) -> Option<Entity<PaneView>> {
        self.views.get(&self.tabs.active().focus()).cloned()
    }

    /// The focused pane's working directory (`PaneView::cwd`), for a new pane, tab or window.
    fn focused_cwd(&self, cx: &App) -> Option<PathBuf> {
        self.focused().and_then(|view| view.read(cx).cwd())
    }

    /// The bar in the titlebar, for the window's width and tabs.
    fn bar(&self, window: &Window) -> Bar {
        let width = f32::from(window.viewport_size().width);
        Bar::new(width, self.tabs.all().len(), window.is_fullscreen())
    }

    /// The cells the panes share, the size of a cell, and the height of the bar above them.
    fn area(&self, window: &Window, cx: &App) -> (Rect, Size<Pixels>, Pixels) {
        let settings = cx.global::<Settings>();
        let cell = metrics(window.text_system(), &settings.font, settings.font_size).cell;
        let bar = px(self.bar(window).height);
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
        self.views.get(&id).map_or("rtty".into(), |view| view.read(cx).title().text.clone())
    }

    /// Every pane, shown or not, looks again at what it is called (a command started or ended,
    /// the shell changed directory); the bar and the window title follow.
    fn refresh_titles(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut changed = false;
        for view in self.views.values() {
            changed |= view.update(cx, |view, _| view.refresh_title());
        }
        if changed {
            window.set_window_title(&self.title(self.tabs.active().focus(), cx));
            cx.notify();
        }
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
            PaneEvent::Bell if !window.is_window_active() => {
                notify::request_attention();
                self.attention.push(format!("bell in pane {id}: the dock asked for attention"));
            }
            PaneEvent::Bell => {}
            // A notification is for someone looking elsewhere: not posted from the pane they are
            // typing in.
            PaneEvent::Notification { title, body } => {
                let seen = window.is_window_active() && self.tabs.active().focus() == id;
                let fate = if seen {
                    "not posted: its pane has the keyboard"
                } else if notify::post(id, title.as_deref(), body.as_deref()) {
                    "posted"
                } else {
                    "not posted: rtty is not in an app bundle"
                };
                self.attention
                    .push(format!("notification from pane {id} {title:?} {body:?}: {fate}"));
            }
            PaneEvent::Exited(status) => {
                crate::note(&format!("pane {id}: its program exited: {status}"));
                self.close(id, window, cx)
            }
        }
    }

    /// Closes a pane, its tab when it was the last there, and the window when that was the last
    /// tab. Its program, if still running, is hung up on.
    fn close(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(closed) = self.tabs.close(id) else { return };
        self.views.remove(&id);
        let with = match closed {
            Closed::Pane => "",
            Closed::Tab => ", and its tab",
            Closed::Window => ", and its window",
        };
        crate::note(&format!("pane {id}: closed{with}"));
        match closed {
            Closed::Window => window.remove_window(),
            Closed::Pane | Closed::Tab => self.focus_shown(window, cx),
        }
    }

    /// Closes tab `index` and hangs up on every program in it; the last tab closes the window.
    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((panes, closed)) = self.tabs.close_tab(index) else { return };
        let with = if closed == Closed::Window { ", and its window" } else { "" };
        crate::note(&format!("tab {}: its close button closed panes {panes:?}{with}", index + 1));
        for pane in panes {
            self.views.remove(&pane);
        }
        match closed {
            Closed::Window => window.remove_window(),
            Closed::Pane | Closed::Tab => self.focus_shown(window, cx),
        }
    }

    /// A press in the bar: a tab shows at once, a close button or the + waits for the release,
    /// and a double click on the bare bar zooms the window, as a titlebar's does.
    pub fn bar_down(
        &mut self,
        position: Point<Pixels>,
        clicks: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hit = self.bar(window).hit(f32::from(position.x), f32::from(position.y));
        match hit {
            Some(Hit::Tab(index)) => self.select_tab(index, window, cx),
            None if clicks == 2 => window.titlebar_double_click(),
            _ => {}
        }
        self.bar_press = hit;
    }

    /// A release in the bar over what the press began on: close that tab, or open a new one.
    pub fn bar_up(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let hit = self.bar(window).hit(f32::from(position.x), f32::from(position.y));
        if self.bar_press.take() != hit {
            return;
        }
        match hit {
            Some(Hit::Close(index)) => self.close_tab(index, window, cx),
            Some(Hit::New) => self.new_tab(&NewTab, window, cx),
            _ => return,
        }
        // The pills moved under the pointer.
        self.bar_move(Some(position), window, cx);
    }

    /// The middle of what `hit` names in the bar, for `--script`.
    pub fn bar_point(&self, hit: Hit, window: &Window) -> Option<Point<Pixels>> {
        let bar = self.bar(window);
        let (x, y) = match hit {
            Hit::Tab(index) => bar.tabs.get(index)?.center(),
            Hit::Close(index) => bar.tabs.get(index).map(|_| bar.close(index).center())?,
            Hit::New => bar.new?.center(),
        };
        Some(point(px(x), px(y)))
    }

    /// The pointer is over tab `index`'s pill, which shows its close button.
    fn pill_hovered(&self, index: usize) -> bool {
        matches!(self.bar_hover, Some(Hit::Tab(i) | Hit::Close(i)) if i == index)
    }

    /// Tab `index`'s pill's text and fill: the shown tab's, else one under the pointer's, else
    /// the others'.
    fn pill_colors(&self, index: usize) -> (Rgb, Rgb) {
        match (index == self.tabs.index(), self.pill_hovered(index)) {
            (true, _) => self.scheme.active_tab,
            (false, true) => self.scheme.inactive_tab_hover,
            (false, false) => self.scheme.inactive_tab,
        }
    }

    fn new_tab_colors(&self) -> (Rgb, Rgb) {
        if self.bar_hover == Some(Hit::New) {
            self.scheme.new_tab_hover
        } else {
            self.scheme.new_tab
        }
    }

    /// The pointer moved over the bar, or (`None`) left it: a pill under it shows its close
    /// button, and the + its own color.
    pub fn bar_move(
        &mut self,
        position: Option<Point<Pixels>>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let bar = self.bar(window);
        let hit = position.and_then(|at| bar.hit(f32::from(at.x), f32::from(at.y)));
        if hit != self.bar_hover {
            self.bar_hover = hit;
            cx.notify();
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
        crate::note(&format!("pane {from}: splitting {} into pane {id}", direction.name()));
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
            title: self.title(tab.focus(), cx),
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
                        title: view.title().text.clone(),
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
        let bundled = if notify::bundled() { "an app bundle" } else { "no app bundle" };
        let _ = writeln!(out, "windows {} tabs {labels:?} in {bundled}", cx.windows().len());
        // The bar as the next frame draws it, and the title macOS has (the Window menu's).
        let hex = |Rgb(r, g, b): Rgb| format!("#{r:02x}{g:02x}{b:02x}");
        let bar = self.bar(window);
        let _ = writeln!(
            out,
            "bar {}px bg {} hover {:?} window title {:?}",
            bar.height,
            hex(self.scheme.tab_bar),
            self.bar_hover,
            window.window_title()
        );
        if bar.tabs.is_empty() && bar.height > 0. {
            let title = self.views.get(&layout.focus()).map(|view| view.read(cx).title().clone());
            let folder = title.as_ref().is_some_and(|title| title.dir.is_some());
            let text = title.map(|title| title.text);
            let _ = writeln!(out, "bar title {text:?} folder {folder}");
        }
        for (index, pill) in bar.tabs.iter().enumerate() {
            let (fg, bg) = self.pill_colors(index);
            let close = self.pill_hovered(index);
            let _ = writeln!(
                out,
                "bar tab {} at {},{} {}x{} {:?} fg {} bg {}{}",
                index + 1,
                pill.x,
                pill.y,
                pill.w,
                pill.h,
                self.title(self.tabs.all()[index].focus(), cx),
                hex(fg),
                hex(bg),
                if close { " close shown" } else { "" },
            );
        }
        if let Some(new) = bar.new {
            let (fg, bg) = self.new_tab_colors();
            let _ = writeln!(out, "bar new at {},{} fg {} bg {}", new.x, new.y, hex(fg), hex(bg));
        }
        for asked in &self.attention {
            let _ = writeln!(out, "attention: {asked}");
        }
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

    /// The bar in the titlebar, where `Bar` puts its parts: one tab's title, centered, with a
    /// folder when its directory is known; or a pill for each tab, its title centered, its
    /// Cmd+digit at its right end and, under the pointer, a close button at its left; and the +.
    fn bar_view(&self, bar: &Bar, window: &Window, cx: &App) -> Stateful<Div> {
        let scheme = &self.scheme;
        let at = |rect: tabbar::Rect| {
            div().absolute().left(px(rect.x)).top(px(rect.y)).w(px(rect.w)).h(px(rect.h))
        };
        let label = |text: String| div().min_w_0().truncate().child(text);
        let mut parts: Vec<AnyElement> = Vec::new();
        if bar.tabs.is_empty() {
            let title =
                self.views.get(&self.tabs.active().focus()).map(|view| view.read(cx).title());
            let fg = if window.is_window_active() {
                scheme.color(FOREGROUND)
            } else {
                scheme.inactive_tab.0
            };
            let folder = title.and_then(|title| title.dir.as_ref()).map(|_| {
                svg().path("folder.svg").flex_none().size(px(14.)).text_color(hsla(scheme.color(4)))
            });
            let text = title.map_or("rtty".into(), |title| title.text.clone());
            let centered = div()
                .size_full()
                .px(px(tabbar::LIGHTS_END))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(hsla(fg))
                .children(folder)
                .child(label(text));
            parts.push(centered.into_any_element());
        }
        for (index, &pill) in bar.tabs.iter().enumerate() {
            let (fg, bg) = self.pill_colors(index);
            let close = self.pill_hovered(index).then(|| {
                let button = bar.close(index);
                at(tabbar::Rect { x: button.x - pill.x, y: button.y - pill.y, ..button })
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(self.bar_hover == Some(Hit::Close(index)), |button| {
                        button.bg(hsla(fg).opacity(0.18))
                    })
                    .child(svg().path("close.svg").size(px(8.)).text_color(hsla(fg)))
            });
            let digit = (index < 9).then(|| {
                div()
                    .absolute()
                    .top_0()
                    .right(px(11.))
                    .h_full()
                    .flex()
                    .items_center()
                    .text_size(px(11.))
                    .text_color(hsla(fg).opacity(0.5))
                    .child(format!("\u{2318}{}", index + 1))
            });
            let title = self.title(self.tabs.all()[index].focus(), cx);
            let pill = at(pill)
                .rounded_full()
                .bg(hsla(bg))
                .text_color(hsla(fg))
                .flex()
                .items_center()
                .justify_center()
                .px(px(32.))
                .child(label(title))
                .children(close)
                .children(digit);
            parts.push(pill.into_any_element());
        }
        if let Some(new) = bar.new {
            let (fg, bg) = self.new_tab_colors();
            let plus = at(new)
                .rounded_full()
                .bg(hsla(bg))
                .flex()
                .items_center()
                .justify_center()
                .child(svg().path("plus.svg").size(px(12.)).text_color(hsla(fg)));
            parts.push(plus.into_any_element());
        }
        div()
            .id("bar")
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .h(px(bar.height))
            .bg(hsla(scheme.tab_bar))
            .font_family(UI_FONT)
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .children(parts)
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
        let titlebar = self.bar(window);
        let titlebar = (titlebar.height > 0.).then(|| {
            self.bar_view(&titlebar, window, cx)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        this.bar_down(event.position, event.click_count, window, cx)
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseUpEvent, window, cx| {
                        this.bar_up(event.position, window, cx)
                    }),
                )
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                    this.bar_move(Some(event.position), window, cx)
                }))
                .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                    if !hovered {
                        this.bar_move(None, window, cx)
                    }
                }))
        });
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
                crate::note(&format!("pane {}: Cmd+W", this.tabs.active().focus()));
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
            .children(titlebar)
            .children(panes)
            .children(dividers)
    }
}
