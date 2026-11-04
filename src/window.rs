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
use std::time::{Duration, Instant, SystemTime};

use futures::channel::mpsc::UnboundedReceiver;
use futures::channel::oneshot;
use futures::future::{self, Either};
use gpui::{
    Action, AnyElement, AnyView, App, AppContext, AssetSource, Bounds, Context, DisplayId, Div,
    DragMoveEvent, EmptyView, Entity, FontWeight, Global, Hsla, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, PromptButton, PromptLevel, Render, SharedString,
    Size, Stateful, StyleRefinement, TitlebarOptions, Window, WindowAppearance, WindowBounds,
    WindowHandle, WindowId, WindowOptions, actions, div, point, prelude::*, px, relative, size,
    svg, transparent_black,
};
use objc2::rc::Retained;
use objc2_app_kit::{NSView, NSWindow};
use portable_pty::CommandBuilder;
use raw_window_handle::RawWindowHandle;
use rtty_vt::{BACKGROUND, FOREGROUND, Rgb};

use crate::cli::RESIZE_STEP;
use crate::config;
use crate::control::{PaneInfo, Request, Response, TabInfo, WindowInfo};
use crate::editor;
use crate::layout::{Amount, Closed, Direction, Layout, Orientation, PaneId, Place, Rect, Tabs};
use crate::links::Location;
use crate::notify;
use crate::pane::{Notice, Pane, START_SIZE};
use crate::progress::{self, Progress, State};
use crate::scheme::Scheme;
use crate::session;
use crate::tabbar::{self, Bar, Hit};
use crate::view::{PaneEvent, PaneView, Settings, hsla, metrics};

actions!(
    rtty,
    [
        NewWindow,
        NewTab,
        SplitRight,
        SplitDown,
        ToggleZoom,
        PreviousTab,
        NextTab,
        UndoClose,
        OpenConfig,
        CloseWindow,
        Minimize,
        ZoomWindow
    ]
);

/// What `open-config` has the user's login shell run, as an interactive login shell so that its
/// dotfiles set `EDITOR` (a GUI app's environment has none): the editor on the file, else
/// `open -t`. In the words zsh, bash and fish share.
const EDIT_CONFIG: &str = r#"test -z "$EDITOR" && exec open -t "$RTTY_CONFIG_FILE"; eval "$EDITOR \"\$RTTY_CONFIG_FILE\"""#;

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

/// Closes the focused pane; claimable as `MoveFocus` is.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = rtty, no_json)]
pub struct ClosePane {
    pub keystroke: String,
}

/// Moves the keyboard back to the pane the focus came from; claimable as `MoveFocus` is.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = rtty, no_json)]
pub struct LastPane {
    pub keystroke: String,
}

/// Trades the focused pane's place with its neighbour that way; the focus stays with it.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = rtty, no_json)]
pub struct SwapPane(pub Direction);

/// Shows the tab at this index.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = rtty, no_json)]
pub struct SelectTab(pub usize);

/// How often panes look at their foreground process and directory for their titles.
const TITLE_REFRESH: Duration = Duration::from_secs(1);

/// How long what came of a config reload shows.
const SAID: Duration = Duration::from_secs(6);

/// The bar's text is the system's, as a titlebar's is.
const UI_FONT: &str = ".SystemUIFont";

/// The bar's icons, drawn for rtty on a 16-unit grid; GPUI tints them with the text color.
const ICONS: [(&str, &str); 4] = [
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
    (
        "lock.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M5 7.5V5a3 3 0 0 1 6 0v2.5" fill="none" stroke="black" stroke-width="1.6"/><rect x="3" y="7" width="10" height="7.5" rx="1.5"/></svg>"#,
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
    /// Under `--script`, the question asked before closing, waiting for the script's `answer`.
    asked: Option<oneshot::Sender<usize>>,
    /// What came of the last config reload, and when, shown along the window's bottom for
    /// `SAID`.
    said: Option<(String, Instant)>,
}

/// What closing ends: a pane, a tab, a window, or every window.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Closing {
    Pane,
    Tab,
    Window,
    Quit,
}

/// What to ask before `closing` ends `running`, the command lines of what runs in the panes it
/// closes (`PaneView::running`): the question, what it is about, and the button that goes on.
/// Nothing to ask while nothing runs there but shells at their prompts.
fn question(closing: Closing, running: &[String]) -> Option<(String, String, &'static str)> {
    if running.is_empty() {
        return None;
    }
    let named: Vec<&str> = running
        .iter()
        .map(|command| if command.is_empty() { "a program" } else { command.as_str() })
        .collect();
    let (question, within) = match closing {
        Closing::Pane => {
            let detail = format!("It is still running {}.", named[0]);
            return Some(("Close this pane?".into(), detail, "Close"));
        }
        Closing::Tab => ("Close this tab?", " in it"),
        Closing::Window => ("Close this window?", " in it"),
        Closing::Quit => ("Quit rtty?", ""),
    };
    let panes = match named.len() {
        1 => format!("1 pane{within} is still running a program"),
        count => format!("{count} panes{within} are still running programs"),
    };
    let button = if closing == Closing::Quit { "Quit" } else { "Close" };
    Some((question.into(), format!("{panes}: {}.", named.join(", ")), button))
}

/// The last close, which `undo-close` takes back until `undo_timeout` runs out. One at a time: a
/// close ends the one before it.
struct Undo {
    window: WindowHandle<TerminalWindow>,
    reopen: Reopen,
    /// The closed panes, hidden, their programs running on.
    views: Vec<(PaneId, Entity<PaneView>)>,
    /// When it runs out, which tells it from a later close.
    until: Instant,
}

/// What a close took away, and where it goes back.
#[derive(Debug)]
enum Reopen {
    /// A pane, into its split.
    Pane(PaneId, Place),
    /// A tab, at its index.
    Tab(usize, Layout),
    /// A window, hidden meanwhile.
    Window,
}

impl Reopen {
    /// Pane `pane` has gone from what the close took away: whether anything is left to bring back.
    fn without(&mut self, pane: PaneId) -> bool {
        match self {
            Reopen::Pane(id, _) => *id != pane,
            Reopen::Tab(_, tab) => !tab.close(pane),
            Reopen::Window => true,
        }
    }
}

#[derive(Default)]
struct LastClose(Option<Undo>);

impl Global for LastClose {}

/// Keeps a close for `undo-close` until `undo_timeout` runs out, ending the one before it.
fn remember(
    window: WindowHandle<TerminalWindow>,
    reopen: Reopen,
    views: Vec<(PaneId, Entity<PaneView>)>,
    cx: &mut App,
) {
    forget(cx);
    let timeout = cx.global::<Settings>().undo_timeout;
    let until = Instant::now() + timeout;
    cx.default_global::<LastClose>().0 = Some(Undo { window, reopen, views, until });
    cx.spawn(async move |cx| {
        cx.background_executor().timer(timeout).await;
        let _ = cx.update(|cx| {
            let last = cx.default_global::<LastClose>().0.as_ref();
            if last.is_some_and(|undo| undo.until == until) {
                forget(cx)
            }
        });
    })
    .detach();
}

/// The last close is for good: its panes go, which hangs up on their programs, and a hidden
/// window closes.
fn forget(cx: &mut App) {
    let Some(undo) = cx.default_global::<LastClose>().0.take() else { return };
    let panes: Vec<PaneId> = undo.views.iter().map(|(pane, _)| *pane).collect();
    crate::note(&format!("closed for good: {:?} with panes {panes:?}", undo.reopen));
    if matches!(undo.reopen, Reopen::Window) {
        let _ = undo.window.update(cx, |_, window, _| window.remove_window());
    }
}

/// Pane `pane`'s program exited. If the pane was closed and waits for `undo-close`, it leaves that
/// close, whose other panes wait on; a close left with nothing to bring back ends.
fn leave_undo(pane: PaneId, cx: &mut App) {
    let Some(undo) = cx.default_global::<LastClose>().0.as_mut() else { return };
    let Some(at) = undo.views.iter().position(|(id, _)| *id == pane) else { return };
    undo.views.remove(at);
    if !undo.reopen.without(pane) {
        forget(cx);
    }
}

/// The window a close hid, while `undo-close` can bring it back.
pub fn hidden_window(cx: &App) -> Option<WindowId> {
    let undo = cx.try_global::<LastClose>()?.0.as_ref()?;
    matches!(undo.reopen, Reopen::Window).then(|| undo.window.window_id())
}

/// `undo-close`, from window `from`: brings the last close back (`TerminalWindow::reopen`).
fn undo_close(from: WindowId, cx: &mut App) {
    let Some(Undo { window, reopen, views, .. }) = cx.default_global::<LastClose>().0.take() else {
        return;
    };
    let reopened =
        window.update(cx, |this, window, cx| this.reopen(reopen, views, from, window, cx));
    if reopened.is_err() {
        crate::note("undo: the closed panes' window is gone");
    }
    refresh_badge(cx);
}

/// The window's AppKit window.
fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI's window handle is its live NSView, used on the main thread.
    let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
    view.window()
}

/// Quitting, however asked for (Cmd+Q, the menu's Quit, and through `app::should_terminate` the
/// dock's Quit and logging out): quits once what runs in any window is confirmed, asked on the
/// active window (else the first). A window a close hid is closed already.
pub fn quit(cx: &mut App) {
    let hidden = hidden_window(cx);
    let windows: Vec<WindowHandle<TerminalWindow>> = cx
        .windows()
        .into_iter()
        .filter(|window| Some(window.window_id()) != hidden)
        .filter_map(|window| window.downcast())
        .collect();
    let running: Vec<String> = windows
        .iter()
        .filter_map(|handle| handle.read(cx).ok())
        .flat_map(|this| this.running(&this.panes(), cx))
        .collect();
    let active = cx.active_window().and_then(|window| window.downcast::<TerminalWindow>());
    let quit = |_: &mut TerminalWindow, _: &mut Window, cx: &mut Context<TerminalWindow>| {
        crate::app::quit("as asked", cx)
    };
    let asked = active.or(windows.first().copied()).map(|asker| {
        asker.update(cx, |this, window, cx| this.confirm(Closing::Quit, running, window, cx, quit))
    });
    if !matches!(asked, Some(Ok(()))) {
        crate::app::quit("as asked, with no window to ask on", cx)
    }
}

/// Opens a window with `command` running in its one pane: the first window centered, later ones
/// cascading from the active one.
pub fn open(command: CommandBuilder, cx: &mut App) -> Result<WindowHandle<TerminalWindow>, String> {
    let id = next_pane_id();
    let first = cx.windows().is_empty();
    let bounds = first.then(|| Bounds::centered(None, size(px(900.), px(600.)), cx));
    open_tabs(Tabs::new(id), vec![(id, command)], bounds, None, cx)
}

/// Opens a window as `saved` has it: its frame on its display (centered on the main one, that
/// display gone or the frame's titlebar off it), its tabs, splits, zoom and focus, and in each
/// pane a new login shell in the pane's directory (the home directory, that one gone). Its panes
/// keep their ids.
pub fn restore(
    saved: session::Window,
    cx: &mut App,
) -> Result<WindowHandle<TerminalWindow>, String> {
    let ids: Vec<PaneId> = saved.tabs.all().iter().flat_map(|tab| tab.panes()).collect();
    NEXT_PANE.fetch_max(ids.iter().max().map_or(1, |id| id + 1), Ordering::Relaxed);
    let commands = ids.into_iter().map(|id| (id, shell(saved.cwds.get(&id).cloned()))).collect();
    // The middle of its titlebar on its display, where it can be taken hold of.
    let frame = saved.frame;
    let titlebar = point(frame.origin.x + frame.size.width / 2., frame.origin.y);
    let display = cx.displays().into_iter().find(|display| {
        let uuid = display.uuid().map(|uuid| uuid.to_string());
        let on_it = Bounds::new(point(px(0.), px(0.)), display.bounds().size).contains(&titlebar);
        on_it && uuid.is_ok_and(|uuid| saved.display.as_ref() == Some(&uuid))
    });
    let (frame, display) = match display {
        Some(display) => (saved.frame, Some(display.id())),
        None => (Bounds::centered(None, saved.frame.size, cx), None),
    };
    open_tabs(saved.tabs, commands, Some(frame), display, cx)
}

/// Opens a window holding `tabs`, each pane running its command, at `bounds` on `display`; with
/// no bounds, cascading from the active window.
fn open_tabs(
    tabs: Tabs,
    commands: Vec<(PaneId, CommandBuilder)>,
    bounds: Option<Bounds<Pixels>>,
    display_id: Option<DisplayId>,
    cx: &mut App,
) -> Result<WindowHandle<TerminalWindow>, String> {
    let scheme = cx.global::<Settings>().theme.for_appearance(cx.window_appearance()).clone();
    let (rows, cols) = START_SIZE;
    let panes = commands.into_iter().map(|(id, command)| {
        spawn(command, id, rows, cols, &scheme, cx).map(|(pane, notices)| (id, pane, notices))
    });
    let panes = panes.collect::<Result<Vec<_>, String>>()?;
    let options = WindowOptions {
        window_bounds: bounds.map(WindowBounds::Windowed),
        display_id,
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
    cx.open_window(options, |window, cx| cx.new(|cx| TerminalWindow::new(tabs, panes, window, cx)))
        .map_err(|err| format!("cannot open a window: {err}"))
}

/// Every window shown (not one a close hid), as `session` keeps them for the next launch.
pub fn session(cx: &mut App) -> Vec<session::Window> {
    let hidden = hidden_window(cx);
    let windows = cx.windows().into_iter().filter(|window| Some(window.window_id()) != hidden);
    let windows: Vec<WindowHandle<TerminalWindow>> =
        windows.filter_map(|window| window.downcast()).collect();
    let saved = windows
        .into_iter()
        .map(|handle| handle.update(cx, |this, window, cx| this.saved(window, cx)));
    saved.filter_map(Result::ok).collect()
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

/// The next pane's id, past every restored pane's.
static NEXT_PANE: AtomicU64 = AtomicU64::new(1);

/// Pane ids are unique across windows, so `--trace` lines name one pane.
fn next_pane_id() -> PaneId {
    NEXT_PANE.fetch_add(1, Ordering::Relaxed)
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
    command.env("RTTY_SOCKET", &settings.launch.socket);
    let trace = settings.launch.trace.as_ref().map(|trace| trace.for_pane(id));
    let integration = settings.shell_integration.as_deref();
    let spawned =
        Pane::spawn(command, rows, cols, Some(scheme), trace, integration, settings.graphics)
            .map_err(|err| format!("cannot start the shell: {err}"));
    match &spawned {
        Ok(_) => crate::note(&format!("pane {id}: started at {rows}x{cols}")),
        Err(err) => crate::note(&format!("pane {id}: {err}")),
    }
    spawned
}

/// The label last put on the dock's icon.
#[derive(Default)]
struct DockBadge(Option<String>);

impl Global for DockBadge {}

/// The dock's badge sums up every pane's progress in every window (`progress::badge`), and goes
/// once every program is done. It shows whether rtty is in front or not: a pill shows its own
/// tab's progress, and the badge is the one place an error in a tab or window out of sight shows.
pub fn refresh_badge(cx: &mut App) {
    let hidden = hidden_window(cx);
    let mut all = Vec::new();
    for window in cx.windows().into_iter().filter(|window| Some(window.window_id()) != hidden) {
        if let Some(Ok(this)) = window.downcast::<TerminalWindow>().map(|handle| handle.read(cx)) {
            let tabs = 0..this.tabs.all().len();
            all.extend(tabs.filter_map(|index| this.tab_progress(index, cx)));
        }
    }
    let label = progress::badge(progress::combine(all));
    let shown = &mut cx.default_global::<DockBadge>().0;
    if *shown != label {
        crate::note(&format!("dock badge {label:?}"));
        notify::badge(label.as_deref());
        *shown = label;
    }
}

/// The config was read again: every window and pane takes up its schemes (the same ones after a
/// mistake), and each window says what came of it along its bottom for a moment.
pub fn reloaded(said: &str, cx: &mut App) {
    let windows = cx.windows().into_iter().filter_map(|window| window.downcast::<TerminalWindow>());
    for handle in windows {
        let _ = handle.update(cx, |this, window, cx| {
            this.follow_appearance(window.appearance(), cx);
            this.say(said, cx);
        });
    }
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
        tabs: Tabs,
        panes: Vec<(PaneId, Pane, UnboundedReceiver<Notice>)>,
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
        cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active() {
                let handle = window.window_handle().downcast().expect("a terminal window");
                crate::app::key_window(handle, cx);
            }
            cx.notify()
        })
        .detach();
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| this.close_button(window, cx)).is_err()
        });
        let mut this = TerminalWindow {
            tabs,
            views: HashMap::new(),
            scheme,
            bar_hover: None,
            bar_press: None,
            attention: Vec::new(),
            asked: None,
            said: None,
        };
        for (id, pane, notices) in panes {
            this.add(id, pane, notices, window, cx);
        }
        this
    }

    /// The window as `session` keeps it: its frame (the one it goes back to from full screen),
    /// its display, its tabs, and its panes' directories.
    fn saved(&self, window: &Window, cx: &App) -> session::Window {
        let display = window.display(cx).and_then(|display| display.uuid().ok());
        let cwds = self.views.iter().filter_map(|(id, view)| Some((*id, view.read(cx).cwd()?)));
        session::Window {
            frame: window.window_bounds().get_bounds(),
            display: display.map(|uuid| uuid.to_string()),
            tabs: self.tabs.clone(),
            cwds: cwds.collect(),
        }
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
        let cell = metrics(window.text_system(), settings).cell;
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

    /// The middle of the window's cell (`row`, `col`), counted from the top left of the panes;
    /// negative ones are above or left of them.
    pub fn cell_center(&self, row: i32, col: i32, window: &Window, cx: &App) -> Point<Pixels> {
        let (_, cell, bar) = self.area(window, cx);
        let at = |index: i32, cell: Pixels| cell * (index as f32 + 0.5);
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
    /// the shell changed directory); the bar and the window title follow. And whether the focused
    /// one reads a password, for Secure Keyboard Entry.
    fn refresh_titles(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.defer(crate::app::refresh_secure_input);
        let mut changed = false;
        for view in self.views.values() {
            changed |= view.update(cx, |view, _| view.refresh_title());
        }
        if changed {
            window.set_window_title(&self.title(self.tabs.active().focus(), cx));
            cx.notify();
        }
    }

    /// The keyboard goes to the shown tab's focused pane, whose title names the window, and
    /// whose password prompt holds Secure Keyboard Entry.
    fn focus_shown(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.tabs.active().focus();
        if let Some(view) = self.views.get(&focus) {
            view.read(cx).focus(window);
        }
        window.set_window_title(&self.title(focus, cx));
        cx.defer(crate::app::refresh_secure_input);
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
                    cx.defer(crate::app::refresh_secure_input);
                    cx.notify();
                }
            }
            PaneEvent::Progress => {
                cx.notify();
                cx.defer(refresh_badge);
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
                leave_undo(id, cx);
                self.close(id, false, window, cx)
            }
            PaneEvent::Edit(at) => self.edit(id, at, window, cx),
        }
    }

    /// Edits `at` for pane `from`: in the Neovim with rtty's plugin running there, else in one
    /// running in another pane of its tab, which takes the keyboard; else in a new pane to
    /// `from`'s right (`editor::command`). The Neovims are asked one after another on a
    /// background thread, all by one deadline (`editor::NVIM_WAIT`), and the window waits for
    /// them no longer than that and its grace.
    fn edit(&mut self, from: PaneId, at: &Location, window: &mut Window, cx: &mut Context<Self>) {
        let mut panes = self.tabs.active().panes();
        panes.sort_by_key(|&pane| pane != from);
        let server = |pane| Some((pane, self.views.get(&pane)?.read(cx).nvim()?));
        let servers: Vec<_> = panes.into_iter().filter_map(server).collect();
        if servers.is_empty() {
            return self.edit_in_pane(from, at, window, cx);
        }
        let (asked, deadline) = (at.clone(), SystemTime::now() + editor::NVIM_WAIT);
        let opened = cx.background_spawn(async move {
            let open =
                |(pane, server): (PaneId, PathBuf)| match editor::nvim(&server, &asked, deadline) {
                    Ok(()) => Some(pane),
                    Err(err) => {
                        crate::note(&format!(
                            "pane {from}: pane {pane}'s Neovim opened nothing: {err}"
                        ));
                        None
                    }
                };
            servers.into_iter().find_map(open)
        });
        let at = at.clone();
        cx.spawn_in(window, async move |this, cx| {
            let waited = cx.background_executor().timer(editor::NVIM_WAIT + editor::NVIM_GRACE);
            let opened = match future::select(opened, waited).await {
                Either::Left((opened, _)) => opened,
                Either::Right(_) => None,
            };
            this.update_in(cx, |this, window, cx| match opened {
                Some(pane) => {
                    crate::note(&format!("pane {from}: opened a file in pane {pane}'s Neovim"));
                    this.show(pane, window, cx)
                }
                None => this.edit_in_pane(from, &at, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Edits `at` in a new pane to `from`'s right (`editor::command`).
    fn edit_in_pane(
        &mut self,
        from: PaneId,
        at: &Location,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let command = editor::command(cx.global::<Settings>().editor.as_deref(), at);
        if let Err(err) = self.split(from, Direction::Right, Some(command), window, cx) {
            eprintln!("rtty: {err}");
        }
    }

    /// Every pane in the window's tabs.
    fn panes(&self) -> Vec<PaneId> {
        self.tabs.all().iter().flat_map(|tab| tab.panes()).collect()
    }

    /// What runs in `panes` other than shells at their prompts (`PaneView::running`).
    fn running(&self, panes: &[PaneId], cx: &App) -> Vec<String> {
        panes.iter().filter_map(|id| self.views.get(id)?.read(cx).running()).collect()
    }

    /// Does `then` at once while nothing but shells runs in what `closing` ends, or closing does
    /// not ask (`confirm_close = false`); else asks first, with a sheet on the window (or under
    /// `--script` in the dump, for the script's `answer`), and does it on the go-ahead.
    fn confirm(
        &mut self,
        closing: Closing,
        running: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let settings = cx.global::<Settings>();
        let (asks, native) = (settings.confirm_close, settings.launch.prompts);
        let Some((question, detail, button)) = question(closing, &running).filter(|_| asks) else {
            return then(self, window, cx);
        };
        crate::note(&format!("{question} asked first: {} running", running.len()));
        self.attention.push(format!("asked {question:?} {detail:?} [{button}, Cancel]"));
        let answer = if native {
            let buttons = [PromptButton::ok(button), PromptButton::cancel("Cancel")];
            window.prompt(PromptLevel::Warning, &question, Some(&detail), &buttons, cx)
        } else {
            let (asked, answer) = oneshot::channel();
            self.asked = Some(asked);
            answer
        };
        cx.spawn_in(window, async move |this, cx| {
            let go = answer.await == Ok(0);
            crate::note(&format!("{question} {}", if go { button } else { "Cancel" }));
            if go {
                this.update_in(cx, |this, window, cx| then(this, window, cx)).ok();
            } else if closing == Closing::Quit {
                crate::app::stay();
            }
        })
        .detach();
    }

    /// `--script`'s `answer`: the button `button` (0 goes on, 1 cancels) pressed on the question
    /// waiting.
    pub fn answer_question(&mut self, button: usize) -> Result<(), String> {
        let asked = self.asked.take().ok_or("no question is waiting")?;
        asked.send(button).map_err(|_| "the question is gone".into())
    }

    /// Closes a pane, its tab when it was the last there, and the window when that was the last
    /// tab. With `undo`, what closed waits, hidden and its programs running, for `undo-close`
    /// (`take_out`); else its program, if still running, is hung up on.
    fn close(&mut self, id: PaneId, undo: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.find(id) else { return };
        let tab = self.tabs.all()[index].clone();
        let Some(closed) = self.tabs.close(id) else { return };
        let with = match closed {
            Closed::Pane => "",
            Closed::Tab => ", and its tab",
            Closed::Window => ", and its window",
        };
        crate::note(&format!("pane {id}: closed{with}"));
        let reopen = match closed {
            Closed::Window => return self.close_window(undo, window, cx),
            Closed::Pane => Reopen::Pane(id, tab.place(id).expect("a pane beside another")),
            Closed::Tab => Reopen::Tab(index, tab),
        };
        self.take_out(&[id], undo.then_some(reopen), window, cx);
        self.focus_shown(window, cx);
    }

    /// Tab `index`'s close button: closes the tab, once what runs in it is confirmed, and its
    /// panes wait for `undo-close`; the last tab closes the window.
    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.all().get(index) else { return };
        let panes = tab.panes();
        let closing = if self.tabs.all().len() == 1 { Closing::Window } else { Closing::Tab };
        let running = self.running(&panes, cx);
        self.confirm(closing, running, window, cx, move |this, window, cx| {
            // While it asked, tabs may have moved and some of its panes closed: the tab is the
            // one holding any of them still.
            let Some(index) = this.tabs.find_any(&panes) else { return };
            let tab = this.tabs.all()[index].clone();
            let Some((panes, closed)) = this.tabs.close_tab(index) else { return };
            let with = if closed == Closed::Window { ", and its window" } else { "" };
            let number = index + 1;
            crate::note(&format!("tab {number}: its close button closed panes {panes:?}{with}"));
            if closed == Closed::Window {
                return this.close_window(true, window, cx);
            }
            this.take_out(&panes, Some(Reopen::Tab(index, tab)), window, cx);
            this.focus_shown(window, cx);
        });
    }

    /// The window's close button (and `--script`'s `window close`): it asks first too, as closing
    /// the window's panes one by one would, and the window waits for `undo-close` as theirs would.
    pub fn close_button(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let running = self.running(&self.panes(), cx);
        self.confirm(Closing::Window, running, window, cx, |this, window, cx| {
            this.close_window(true, window, cx)
        });
    }

    /// Takes closed `panes` out of the window. With `reopen` they wait, hidden and their programs
    /// running, for `undo-close` until `undo_timeout` runs out; else, or with no timeout, they go
    /// now, which hangs up on their programs.
    fn take_out(
        &mut self,
        panes: &[PaneId],
        reopen: Option<Reopen>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let views = panes.iter().filter_map(|pane| Some((*pane, self.views.remove(pane)?)));
        let views: Vec<_> = views.collect();
        cx.defer(refresh_badge);
        let waits = !cx.global::<Settings>().undo_timeout.is_zero();
        if let Some(reopen) = reopen.filter(|_| waits) {
            let handle = window.window_handle().downcast().expect("a terminal window");
            remember(handle, reopen, views, cx);
        }
    }

    /// Closes the window; with `undo`, it hides instead and waits for `undo-close` until
    /// `undo_timeout` runs out. The last window shown closes for good, and the last close with
    /// it, so rtty quits; so does a hidden window whose last pane's program exited, and the close
    /// that hid it.
    fn close_window(&mut self, undo: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (this, hidden) = (window.window_handle().window_id(), hidden_window(cx));
        let others = cx.windows().into_iter().map(|other| other.window_id());
        let shown = others.filter(|&other| other != this && Some(other) != hidden).count();
        let waits = undo && shown > 0 && !cx.global::<Settings>().undo_timeout.is_zero();
        crate::note(if waits { "window hidden for undo-close" } else { "window closed" });
        if !waits {
            if shown == 0 || hidden == Some(this) {
                forget(cx);
            }
            return window.remove_window();
        }
        if let Some(ns_window) = ns_window(window) {
            ns_window.orderOut(None);
        }
        let handle = window.window_handle().downcast().expect("a terminal window");
        remember(handle, Reopen::Window, Vec::new(), cx);
        cx.defer(refresh_badge);
    }

    /// Brings back what a close took away, with its panes: a pane into its split (or, with that
    /// gone, into a tab of its own), a tab at its index, a hidden window. A window that was
    /// hidden, or where the chord did not come from (`from`), comes to the front.
    fn reopen(
        &mut self,
        reopen: Reopen,
        views: Vec<(PaneId, Entity<PaneView>)>,
        from: WindowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::note(&format!("undo-close reopened {reopen:?}"));
        let front = matches!(reopen, Reopen::Window) || from != window.window_handle().window_id();
        match reopen {
            Reopen::Pane(id, place) => {
                if let Some(tab) = self.tabs.find(place.beside) {
                    self.tabs.select(tab);
                }
                if !self.tabs.active_mut().reopen(id, &place) {
                    self.tabs.open(id);
                }
            }
            Reopen::Tab(index, tab) => self.tabs.reopen(index, tab),
            Reopen::Window => {}
        }
        // Showing a hidden window as well.
        if front {
            window.activate_window();
        }
        self.views.extend(views);
        // The schemes may have flipped or been reloaded while they were hidden.
        self.follow_appearance(window.appearance(), cx);
        self.focus_shown(window, cx);
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

    /// Splits `from`, showing its tab; the new pane runs `program`, else the login shell, in
    /// `from`'s working directory and takes the keyboard.
    fn split(
        &mut self,
        from: PaneId,
        direction: Direction,
        program: Option<CommandBuilder>,
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
        let mut command = program.unwrap_or_else(CommandBuilder::new_default_prog);
        if let Some(cwd) = self.views.get(&from).and_then(|view| view.read(cx).cwd()) {
            command.cwd(cwd);
        }
        crate::note(&format!("pane {from}: splitting {} into pane {id}", direction.name()));
        let (pane, notices) = spawn(command, id, rect.rows, rect.cols, &self.scheme, cx)?;
        *self.tabs.active_mut() = layout;
        self.add(id, pane, notices, window, cx);
        Ok(id)
    }

    fn split_focused(&mut self, direction: Direction, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.split(self.tabs.active().focus(), direction, None, window, cx) {
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
                .split(pane.unwrap_or(focused), direction, None, window, cx)
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

    /// Shows `said` along the window's bottom for a moment: what came of the config's reload or
    /// of opening it.
    fn say(&mut self, said: &str, cx: &mut Context<Self>) {
        self.said = Some((said.to_owned(), Instant::now()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAID).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }

    /// `open-config`: the config file in a new tab, after the current one, in the user's editor
    /// (`EDIT_CONFIG`); first written from rtty's commented template when there is none.
    fn open_config(&mut self, _: &OpenConfig, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = cx.global::<Settings>().launch.config.clone() else {
            return self.say("no config file: neither XDG_CONFIG_HOME nor HOME is set", cx);
        };
        if let Err(err) = config::create(&path) {
            return self.say(&format!("{}: {err}", path.display()), cx);
        }
        let shell = CommandBuilder::new_default_prog().get_shell();
        let mut command = CommandBuilder::from_argv(
            [shell.as_str(), "-l", "-i", "-c", EDIT_CONFIG].map(Into::into).to_vec(),
        );
        command.env("RTTY_CONFIG_FILE", &path);
        if let Some(dir) = path.parent() {
            command.cwd(dir);
        }
        let (area, ..) = self.area(window, cx);
        let id = next_pane_id();
        match spawn(command, id, area.rows, area.cols, &self.scheme, cx) {
            Ok((pane, notices)) => {
                self.tabs.open(id);
                self.add(id, pane, notices, window, cx);
            }
            Err(err) => self.say(&err, cx),
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
        let badge = cx.try_global::<DockBadge>().and_then(|badge| badge.0.as_deref());
        let visible = ns_window(window).map(|ns_window| ns_window.isVisible());
        let _ = writeln!(out, "dock badge {badge:?}, this window on screen {visible:?}");
        let _ = writeln!(out, "frame {:?}", window.window_bounds().get_bounds());
        if let Some(undo) = cx.try_global::<LastClose>().and_then(|last| last.0.as_ref()) {
            let panes: Vec<PaneId> = undo.views.iter().map(|(pane, _)| *pane).collect();
            let left = undo.until.saturating_duration_since(Instant::now()).as_millis();
            let _ = writeln!(out, "undo-close {:?} panes {panes:?} for {left} ms", undo.reopen);
        }
        // The bar as the next frame draws it, and the title macOS has (the Window menu's).
        let hex = |Rgb(r, g, b): Rgb| format!("#{r:02x}{g:02x}{b:02x}");
        let bar = self.bar(window);
        let _ = writeln!(
            out,
            "bar {}px bg {} hover {:?} lock {} window title {:?}",
            bar.height,
            hex(self.scheme.tab_bar),
            self.bar_hover,
            crate::app::secure_input_held(cx),
            window.window_title()
        );
        if bar.tabs.is_empty() && bar.height > 0. {
            let title = self.views.get(&layout.focus()).map(|view| view.read(cx).title().clone());
            let folder = title.as_ref().is_some_and(|title| title.dir.is_some());
            let text = title.map(|title| title.text);
            let progress = self.tab_progress(0, cx);
            let _ = writeln!(out, "bar title {text:?} folder {folder} progress {progress:?}");
        }
        for (index, pill) in bar.tabs.iter().enumerate() {
            let (fg, bg) = self.pill_colors(index);
            let close = self.pill_hovered(index);
            let _ = writeln!(
                out,
                "bar tab {} at {},{} {}x{} {:?} fg {} bg {}{} progress {:?}",
                index + 1,
                pill.x,
                pill.y,
                pill.w,
                pill.h,
                self.title(self.tabs.all()[index].focus(), cx),
                hex(fg),
                hex(bg),
                if close { " close shown" } else { "" },
                self.tab_progress(index, cx),
            );
        }
        if let Some(new) = bar.new {
            let (fg, bg) = self.new_tab_colors();
            let _ = writeln!(out, "bar new at {},{} fg {} bg {}", new.x, new.y, hex(fg), hex(bg));
        }
        for asked in &self.attention {
            let _ = writeln!(out, "attention: {asked}");
        }
        if let Some((said, at)) = &self.said {
            let _ = writeln!(out, "said {said:?} {} ms ago", at.elapsed().as_millis());
        }
        out.push_str(&crate::app::dump(cx));
        let rects = layout.rects(area);
        for (id, rect) in &rects {
            let _ =
                writeln!(out, "pane {id} at {},{} {}x{}", rect.row, rect.col, rect.rows, rect.cols);
        }
        for (id, _) in &rects {
            if let Some(view) = self.views.get(id) {
                let _ = write!(out, "\n== pane {id}\n{}", view.read(cx).dump(window, cx));
            }
        }
        out
    }

    /// Tab `index`'s panes' progress as one (`progress::combine`).
    fn tab_progress(&self, index: usize, cx: &App) -> Option<Progress> {
        let panes = self.tabs.all()[index].panes();
        progress::combine(panes.iter().filter_map(|id| self.views.get(id)?.read(cx).progress()))
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
        // A program's progress, a thin line over `track`: the scheme's blue, its yellow while
        // held, its red on an error; all of it, faint, while there is no telling how far.
        let meter = |progress: Progress, track: Hsla| {
            let color = match progress.state {
                State::Error => scheme.color(1),
                State::Paused => scheme.color(3),
                State::Normal | State::Indeterminate => scheme.color(4),
            };
            let (fill, opacity) = match (progress.state, progress.percent) {
                (State::Indeterminate, _) => (1., 0.5),
                (_, percent) => (percent.map_or(1., |percent| f32::from(percent) / 100.), 1.),
            };
            let done = div().h_full().w(relative(fill)).rounded_full();
            let line = div().absolute().h(px(2.)).rounded_full().bg(track);
            line.child(done.bg(hsla(color).opacity(opacity)))
        };
        // Before the shown tab's title while Secure Keyboard Entry is on.
        let lock = |color: Rgb| {
            let held = crate::app::secure_input_held(cx);
            held.then(|| svg().path("lock.svg").flex_none().size(px(11.)).text_color(hsla(color)))
        };
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
                .children(lock(fg))
                .child(label(text));
            parts.push(centered.into_any_element());
            if let Some(progress) = self.tab_progress(0, cx) {
                let line = meter(progress, transparent_black()).left_0().right_0().bottom_0();
                parts.push(line.into_any_element());
            }
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
            // Along the pill's bottom, between its rounded ends.
            let progress = self.tab_progress(index, cx).map(|progress| {
                let inset = px(pill.h / 2.);
                meter(progress, hsla(fg).opacity(0.15)).left(inset).right(inset).bottom(px(3.))
            });
            let pill = at(pill)
                .rounded_full()
                .bg(hsla(bg))
                .text_color(hsla(fg))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(5.))
                .px(px(32.))
                .children(lock(fg).filter(|_| index == self.tabs.index()))
                .child(label(title))
                .children(close)
                .children(digit)
                .children(progress);
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
        let scale = window.scale_factor();
        for tab in self.tabs.all() {
            for (id, rect) in tab.grids(area) {
                if let Some(view) = self.views.get(&id) {
                    view.update(cx, |view, _| view.resize(rect.rows, rect.cols, cell, scale));
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
        let said = self.said.as_ref().filter(|(_, at)| at.elapsed() < SAID).map(|(said, _)| {
            div()
                .absolute()
                .left_0()
                .bottom_0()
                .max_w_full()
                .px(px(8.))
                .py(px(2.))
                .font_family(UI_FONT)
                .text_size(px(12.))
                .bg(hsla(self.scheme.color(FOREGROUND)))
                .text_color(hsla(self.scheme.color(BACKGROUND)))
                .child(said.clone())
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
            .on_action(cx.listener(|this, action: &ClosePane, window, cx| {
                let pane = this.tabs.active().focus();
                crate::note(&format!("pane {pane}: {}", action.keystroke));
                let running = this.running(&[pane], cx);
                this.confirm(Closing::Pane, running, window, cx, move |this, window, cx| {
                    this.close(pane, true, window, cx)
                });
            }))
            .on_action(cx.listener(|_, _: &UndoClose, window, cx| {
                // Once this window is done with the chord, so that any window can take the
                // close back, this one included.
                let from = window.window_handle().window_id();
                cx.defer(move |cx| undo_close(from, cx));
            }))
            .on_action(cx.listener(|this, action: &MoveFocus, window, cx| {
                this.focus_toward(this.tabs.active().focus(), action.direction, false, window, cx);
            }))
            .on_action(cx.listener(|this, action: &ResizePane, window, cx| {
                this.resize(this.tabs.active().focus(), action.direction, RESIZE_STEP, window, cx)
            }))
            .on_action(cx.listener(|this, _: &LastPane, window, cx| {
                this.tabs.active_mut().focus_previous();
                this.focus_shown(window, cx);
            }))
            .on_action(cx.listener(|this, action: &SwapPane, window, cx| {
                let (area, ..) = this.area(window, cx);
                let focus = this.tabs.active().focus();
                this.tabs.active_mut().swap(focus, action.0, area);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleZoom, _, cx| {
                this.tabs.active_mut().toggle_zoom();
                cx.notify();
            }))
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::new_window))
            .on_action(cx.listener(Self::open_config))
            .on_action(cx.listener(|this, action: &SelectTab, window, cx| {
                this.select_tab(action.0, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &PreviousTab, window, cx| this.cycle_tab(-1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle_tab(1, window, cx)))
            .on_action(
                cx.listener(|this, _: &CloseWindow, window, cx| this.close_button(window, cx)),
            )
            .on_action(cx.listener(|_, _: &Minimize, window, _| window.minimize_window()))
            .on_action(cx.listener(|_, _: &ZoomWindow, window, _| window.zoom_window()))
            .on_drag_move(cx.listener(Self::drag_divider))
            .children(titlebar)
            .children(panes)
            .children(dividers)
            .children(said)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(closing: Closing, running: &[&str]) -> Option<(String, String, &'static str)> {
        question(closing, &running.iter().map(|command| command.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_closed_pane_that_exits_leaves_the_rest_of_its_close_to_bring_back() {
        let mut tab = Layout::new(1);
        tab.split(1, Direction::Right, 2);
        let mut closed = Reopen::Tab(0, tab);
        assert!(closed.without(2));
        assert!(matches!(&closed, Reopen::Tab(0, tab) if tab.panes() == [1]));
        assert!(closed.without(9), "a pane it never held");
        assert!(!closed.without(1), "its last pane: nothing is left");
        let mut split = Layout::new(1);
        split.split(1, Direction::Down, 3);
        let mut pane = Reopen::Pane(3, split.place(3).unwrap());
        assert!(pane.without(1));
        assert!(!pane.without(3));
        assert!(Reopen::Window.without(5));
    }

    #[test]
    fn the_config_opens_in_the_editor_else_with_open() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("rtty-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Stands in for macOS's `open`, which would launch TextEdit.
        let open = dir.join("open");
        std::fs::write(&open, "#!/bin/sh\nprintf 'open %s' \"$*\"\n").unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        let file = "/tmp/a dir/it's.toml";
        let run = |shell: &str, editor: &str| {
            let output = std::process::Command::new(shell)
                .args(["-c", EDIT_CONFIG])
                .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
                .env("EDITOR", editor)
                .env("RTTY_CONFIG_FILE", file)
                .output()
                .unwrap();
            String::from_utf8(output.stdout).unwrap()
        };
        for shell in ["/bin/zsh", "/bin/bash"] {
            assert_eq!(run(shell, "printf %s."), format!("{file}."), "{shell}");
            assert_eq!(run(shell, ""), format!("open -t {file}"), "{shell}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn nothing_is_asked_while_only_shells_at_their_prompts_would_close() {
        for closing in [Closing::Pane, Closing::Tab, Closing::Window, Closing::Quit] {
            assert_eq!(ask(closing, &[]), None, "{closing:?}");
        }
    }

    #[test]
    fn a_pane_names_its_program_and_the_rest_count_their_panes() {
        let pane = ask(Closing::Pane, &["sleep 100"]).unwrap();
        assert_eq!(
            pane,
            ("Close this pane?".into(), "It is still running sleep 100.".into(), "Close")
        );
        let tab = ask(Closing::Tab, &["sleep 100"]).unwrap();
        assert_eq!(tab.1, "1 pane in it is still running a program: sleep 100.");
        let window = ask(Closing::Window, &["nvim notes.md", "cargo build"]).unwrap();
        assert_eq!(window.0, "Close this window?");
        assert_eq!(
            window.1,
            "2 panes in it are still running programs: nvim notes.md, cargo build."
        );
        let quit = ask(Closing::Quit, &["claude", "", "htop"]).unwrap();
        let detail = "3 panes are still running programs: claude, a program, htop.";
        assert_eq!(quit, ("Quit rtty?".into(), detail.into(), "Quit"));
        // A program whose arguments could not be read.
        assert_eq!(ask(Closing::Pane, &[""]).unwrap().1, "It is still running a program.");
    }
}
