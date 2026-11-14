//! A window: its tabs, each a tree of split panes (`layout.rs` does the arithmetic), the bar in
//! its titlebar (`tabbar.rs` places it), and the dividers between panes. Cmd chords arrive here as
//! actions; panes tell the window their titles, their focus, and that their programs exited.
//!
//! The layout is in cells, and every pane's grid and pty follow the cells it gets, shown or not.
//! Each pane draws as a cached view, so output in one pane redraws that pane alone.
//!
//! A window that shows tutti panes keeps one connection to each of their sessions, opened off the
//! main thread; a pane waits for it, blank, and shows its tutti pane once it is open, or says why
//! it cannot. A window can be a whole tutti session (`Mirror`): its tabs and splits are then the
//! session's, which the daemon keeps and tells of (`Told::Layout`), and what would change them asks
//! the daemon instead.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use block2::RcBlock;
use futures::StreamExt;
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
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAlert, NSAlertSecondButtonReturn, NSEvent, NSEventMask, NSModalResponse, NSView, NSWindow,
    NSWindowButton,
};
use objc2_foundation::NSString;
use portable_pty::CommandBuilder;
use raw_window_handle::RawWindowHandle;
use rtty_vt::{BACKGROUND, FOREGROUND, Rgb};

use crate::cli::RESIZE_STEP;
use crate::config::{self, ClipboardRead};
use crate::control::{PaneInfo, Request, Response, TabInfo, WindowInfo};
use crate::editor;
use crate::ionmac;
use crate::layout::{
    Amount, Closed, Direction, Layout, Named, Orientation, PaneId, Place, Rect, TabIds, Tabs,
};
use crate::links::Location;
use crate::notify;
use crate::pane::{Notice, Pane, START_SIZE};
use crate::progress::{self, Progress, State};
use crate::scheme::Scheme;
use crate::session;
use crate::tabbar::{self, Bar, Hit};
use crate::tutti::{self, Told};
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
        ZoomWindow,
        KillPane
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

/// What a window that is a tutti session says to a close of a pane or a tab: they are tutti's.
const DETACHES: &str =
    "tutti's panes run on: kill-pane ends one, and closing the window detaches from the session";

/// What it says to a pane of rtty's own.
const SESSION_ONLY: &str = "a tutti session's window shows the session's panes only";

/// What it says to an action while the session has not answered, or did not.
const UNANSWERED: &str = "the tutti session is not open";

/// A window's connection to a tutti session.
enum Link {
    /// Opening, and the panes waiting for it: rtty's id, and tutti's.
    Opening(Vec<(PaneId, u64)>),
    /// Open, while a pane showing one of its panes, or the window that is it, holds it.
    Open(Weak<tutti::Session>),
}

/// A window that is a tutti session: the session's tabs are its tabs.
struct Mirror {
    /// The daemon's socket, which names the session.
    socket: PathBuf,
    /// The window's connection to it, once the daemon answered.
    session: Option<Arc<tutti::Session>>,
    tabs: TabIds,
    panes: Named,
    /// The tab the session last heard the window show.
    selected: Option<u64>,
}

pub struct TerminalWindow {
    /// The window's own id, which the saved layout keeps, so it is the same after a restart
    /// (`rtty cli list` tells it).
    uid: String,
    /// `tabs = false`: no bar and no window buttons, and what would open a tab opens a window.
    bare: bool,
    tabs: Tabs,
    views: HashMap<PaneId, Entity<PaneView>>,
    /// The tutti sessions whose panes the window shows, by their daemons' sockets.
    sessions: HashMap<PathBuf, Link>,
    /// The tutti pane each pane was asked to show, by its session's socket and its id there,
    /// whether it shows, waits or says why it cannot: what the next launch asks for again.
    attached: HashMap<PaneId, (PathBuf, u64)>,
    /// The tutti session the window is, if it is one.
    mirror: Option<Mirror>,
    /// The theme's scheme for the window's appearance, for the tab bar and dividers.
    scheme: Arc<Scheme>,
    /// What in the bar the pointer is over, and what a press there began on: a close button or
    /// the + acts on a release over the same thing, as a button does.
    bar_hover: Option<Hit>,
    bar_press: Option<Hit>,
    /// Under `--script`, what bells and notifications asked of macOS and what came of it, and the
    /// questions asked, for the dump.
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

/// A sheet on `window` asking `question` whose Return and Esc both answer `stop`, so only a
/// click answers `go`. AppKit gives a button one key: `stop`, added first, has Return, and Esc,
/// which AppKit gives only a button titled Cancel, is caught while the sheet is up. The button
/// pressed, 0 for `go`, as `Window::prompt` counts them.
fn deliberate_sheet(
    question: &str,
    detail: &str,
    [go, stop]: [&str; 2],
    window: &Window,
) -> oneshot::Receiver<usize> {
    const ESCAPE: u16 = 53;
    let (answered, answer) = oneshot::channel();
    let Some(parent) = ns_window(window) else { return answer };
    let main = MainThreadMarker::new().expect("windows are on the main thread");
    let alert = NSAlert::new(main);
    alert.setMessageText(&NSString::from_str(question));
    alert.setInformativeText(&NSString::from_str(detail));
    let stop = alert.addButtonWithTitle(&NSString::from_str(stop));
    alert.addButtonWithTitle(&NSString::from_str(go));
    let sheet = alert.window();
    let escape = RcBlock::new(move |event: NonNull<NSEvent>| {
        // SAFETY: AppKit calls the monitor with a live event.
        let key = unsafe { event.as_ref() };
        let on_sheet = key.window(main).is_some_and(|window| ptr::eq(&*window, &*sheet));
        if !on_sheet || key.keyCode() != ESCAPE {
            return event.as_ptr();
        }
        // SAFETY: a click takes no sender.
        unsafe { stop.performClick(None) };
        ptr::null_mut()
    });
    // SAFETY: the monitor returns the event it is given, or null for an Esc it answered; it goes
    // with the sheet.
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &escape)
    };
    let answered = Cell::new(Some(answered));
    let done = RcBlock::new(move |response: NSModalResponse| {
        if let Some(monitor) = &monitor {
            // SAFETY: the monitor this sheet added, removed once.
            unsafe { NSEvent::removeMonitor(monitor) };
        }
        let button = if response == NSAlertSecondButtonReturn { 0 } else { 1 };
        if let Some(answered) = answered.take() {
            let _ = answered.send(button);
        }
    });
    alert.beginSheetModalForWindow_completionHandler(&parent, Some(&done));
    answer
}

/// Quitting, however asked for (Cmd+Q, the menu's Quit, and through `app::should_terminate` the
/// dock's Quit and logging out): quits at once while nothing but shells at their prompts runs
/// in any window, or closing does not ask; else asks, on the window that had the keyboard last
/// (else another), and `app::asking` takes the answer. A window a close hid is closed already.
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
    let asks = cx.global::<Settings>().confirm_close;
    let Some((question, detail, button)) = question(Closing::Quit, &running).filter(|_| asks)
    else {
        return crate::app::quit("nothing runs that quitting ends, or it asks nothing", cx);
    };
    let key = crate::app::key(cx).filter(|key| windows.contains(key));
    let asked = key.into_iter().chain(windows).find_map(|asker| {
        let answer = asker.update(cx, |this, window, cx| {
            this.ask(&question, &detail, [button, "Cancel"], false, window, cx)
        });
        Some((asker, answer.ok()?))
    });
    let Some((asker, answer)) = asked else {
        return crate::note(&format!("{question} could not be asked: no window took it"));
    };
    let on = crate::app::number(asker, cx);
    crate::note(&format!("{question} asked first, on window {on}: {} running", running.len()));
    crate::app::asking(asker.window_id(), answer, cx)
}

/// What a new pane shows: a program on a pty of its own, or pane `pane` of the tutti session whose
/// daemon listens on `socket` (`TerminalWindow::attach_tutti`).
pub enum Program {
    Command(CommandBuilder),
    Tutti { socket: PathBuf, pane: u64 },
}

/// Where a new window goes: the first one centered on the main display; a later one a titlebar
/// below the window that had the keyboard last, on that window's display. GPUI's own cascade
/// keeps the place and opens on the main display, off screen when that one is the smaller.
fn new_frame(cx: &mut App) -> (Option<Bounds<Pixels>>, Option<DisplayId>) {
    if cx.windows().is_empty() {
        return (Some(Bounds::centered(None, size(px(900.), px(600.)), cx)), None);
    }
    let hidden = hidden_window(cx);
    let below = |window: &mut Window, cx: &mut App| {
        let display = window.display(cx)?.id();
        Some((window.window_bounds().get_bounds() + point(px(0.), px(35.)), display))
    };
    let key = crate::app::key(cx).filter(|key| Some(key.window_id()) != hidden);
    key.and_then(|key| key.update(cx, |_, window, cx| below(window, cx)).ok()?).unzip()
}

/// Opens a window with `program` in its one pane.
pub fn open(program: Program, cx: &mut App) -> Result<WindowHandle<TerminalWindow>, String> {
    open_at(next_pane_id(), program, None, cx)
}

/// Opens a window with `program` in its one pane, `id`, at `frame` on its display, else where a
/// new window goes.
fn open_at(
    id: PaneId,
    program: Program,
    frame: Option<(Bounds<Pixels>, Option<DisplayId>)>,
    cx: &mut App,
) -> Result<WindowHandle<TerminalWindow>, String> {
    let (bounds, display) = frame.map_or_else(|| new_frame(cx), |(b, d)| (Some(b), d));
    open_tabs(new_uid(), Tabs::new(id), vec![(id, program)], None, bounds, display, cx)
}

/// Opens a window that is the tutti session whose daemon listens on `socket`: the session's tabs,
/// splits and panes are the window's once the daemon answers. Until then its one pane waits, and
/// says why should the daemon not answer. Where `open` puts one.
pub fn open_session(socket: PathBuf, cx: &mut App) -> Result<WindowHandle<TerminalWindow>, String> {
    let (bounds, display) = new_frame(cx);
    let waiting = Tabs::new(next_pane_id());
    open_tabs(new_uid(), waiting, Vec::new(), Some(socket), bounds, display, cx)
}

/// A new window's id of its own: the time it opened, and how many this rtty opened before it.
fn new_uid() -> String {
    static OPENED: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_micros());
    format!("{now:x}-{:x}", OPENED.fetch_add(1, Ordering::Relaxed))
}

/// Where a window with `frame` on the display with UUID `display` goes: there, when that display
/// is here and holds the middle of the frame's titlebar, where the window can be taken hold of;
/// else centered on the main display.
fn placed(
    frame: Bounds<Pixels>,
    display: Option<&String>,
    cx: &App,
) -> (Bounds<Pixels>, Option<DisplayId>) {
    let titlebar = point(frame.origin.x + frame.size.width / 2., frame.origin.y);
    let found = cx.displays().into_iter().find(|found| {
        let uuid = found.uuid().map(|uuid| uuid.to_string());
        let on_it = Bounds::new(point(px(0.), px(0.)), found.bounds().size).contains(&titlebar);
        on_it && uuid.is_ok_and(|uuid| display == Some(&uuid))
    });
    match found {
        Some(found) => (frame, Some(found.id())),
        None => (Bounds::centered(None, frame.size, cx), None),
    }
}

/// Opens a window as `saved` has it: its frame on its display (centered on the main one, that
/// display gone or the frame's titlebar off it), its tabs, splits, zoom and focus, and in each
/// pane a new login shell in the pane's directory (the home directory, that one gone), or the
/// tutti pane it showed. Its panes keep their ids. A window that was a tutti session is that
/// session again, as its daemon has it now.
pub fn restore(
    saved: session::Window,
    cx: &mut App,
) -> Result<WindowHandle<TerminalWindow>, String> {
    let programs = programs(&saved);
    let last = programs.iter().map(|(id, _)| *id).max();
    NEXT_PANE.fetch_max(last.map_or(1, |id| id + 1), Ordering::Relaxed);
    let (frame, display) = placed(saved.frame, saved.display.as_ref(), cx);
    // A layout saved before windows had ids of their own gives it one now.
    let uid = Some(saved.uid).filter(|uid| !uid.is_empty()).unwrap_or_else(new_uid);
    // A session's window waits in one pane for its daemon, whatever tabs it was saved with.
    let (tabs, mut programs) = match saved.session {
        Some(_) => (Tabs::new(next_pane_id()), Vec::new()),
        None => (saved.tabs, programs),
    };
    if cx.global::<Settings>().tabs || tabs.all().len() == 1 {
        return open_tabs(uid, tabs, programs, saved.session, Some(frame), display, cx);
    }
    // Without tabs each saved tab is a window, all at the saved frame; the one that showed keeps
    // the saved window's id and is the one given back.
    let mut windows = Vec::new();
    for (index, tab) in tabs.all().iter().enumerate() {
        let (own, rest) = programs.into_iter().partition(|(id, _)| tab.contains(*id));
        programs = rest;
        let uid = if index == tabs.index() { uid.clone() } else { new_uid() };
        let tab = Tabs::of(vec![tab.clone()], 0);
        windows.push(open_tabs(uid, tab, own, None, Some(frame), display, cx)?);
    }
    Ok(windows.swap_remove(tabs.index()))
}

/// What each pane of a saved window shows again: the tutti pane it was asked to show (open,
/// waiting or held when it was saved), else a new login shell in its directory.
fn programs(saved: &session::Window) -> Vec<(PaneId, Program)> {
    let ids = saved.tabs.all().iter().flat_map(Layout::panes);
    let program = |id| match saved.tutti.get(&id).cloned() {
        Some((socket, pane)) => Program::Tutti { socket, pane },
        None => Program::Command(shell(saved.cwds.get(&id).cloned())),
    };
    ids.map(|id| (id, program(id))).collect()
}

/// The tutti panes the window's `tabs` were asked to show, as `attached` has them, for
/// `session` to keep: whatever became of the asking, the next launch asks again.
fn kept(
    tabs: &Tabs,
    attached: &HashMap<PaneId, (PathBuf, u64)>,
) -> BTreeMap<PaneId, (PathBuf, u64)> {
    let panes = tabs.all().iter().flat_map(Layout::panes);
    panes.filter_map(|id| Some((id, attached.get(&id)?.clone()))).collect()
}

/// Opens a window holding `tabs`, each pane showing its program, at `bounds` on `display`; with
/// no bounds, cascading from the active window. With `session`, the window is that tutti session,
/// whose daemon listens there, and `tabs` its one waiting pane.
fn open_tabs(
    uid: String,
    tabs: Tabs,
    programs: Vec<(PaneId, Program)>,
    session: Option<PathBuf>,
    bounds: Option<Bounds<Pixels>>,
    display_id: Option<DisplayId>,
    cx: &mut App,
) -> Result<WindowHandle<TerminalWindow>, String> {
    let scheme = cx.global::<Settings>().theme.for_appearance(cx.window_appearance()).clone();
    let (rows, cols) = START_SIZE;
    let (mut panes, mut tutti) = (Vec::new(), Vec::new());
    for (id, program) in programs {
        match program {
            Program::Command(command) => {
                let (pane, notices) = spawn(command, id, rows, cols, &scheme, cx)?;
                panes.push((id, pane, notices));
            }
            Program::Tutti { socket, pane } => tutti.push((id, socket, pane)),
        }
    }
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
    cx.open_window(options, |window, cx| {
        cx.new(|cx| TerminalWindow::new(uid, tabs, panes, tutti, session, window, cx))
    })
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
    let trace = settings.launch.trace.as_ref().map(|trace| trace.for_pane(id));
    command.env("RTTY_PANE", id.to_string());
    command.env("RTTY_SOCKET", &settings.launch.socket);
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
    if let Request::TuttiOpen { socket } = request {
        return open_session(socket, cx).map_or_else(Response::Error, |_| Response::Done);
    }
    if let Request::NewWindow { cwd, frame, display, tutti } = request {
        let key = crate::app::key(cx).and_then(|key| key.read(cx).ok());
        let program = match tutti {
            Some((socket, pane)) => Program::Tutti { socket, pane },
            None => Program::Command(shell(cwd.or_else(|| key?.focused_cwd(cx)))),
        };
        // A frame with no display named is on the main one.
        let frame = frame.map(|frame| match &display {
            Some(_) => placed(frame, display.as_ref(), cx),
            None => (frame, cx.primary_display().map(|display| display.id())),
        });
        let opened = open_at(next_pane_id(), program, frame, cx);
        let info = opened.and_then(|handle| {
            let info = handle.update(cx, |this, window, cx| this.info(window, cx));
            info.map_err(|err| err.to_string())
        });
        return info.map_or_else(Response::Error, |info| Response::Windows(vec![info]));
    }
    let pane = match request {
        Request::Focus { pane } => Some(pane),
        Request::FocusToward { from, .. } | Request::Resize { from, .. } => from,
        Request::Split { pane, .. } | Request::Zoom { pane } => pane,
        Request::TuttiAttach { from, .. } => from,
        Request::List | Request::TuttiOpen { .. } | Request::NewWindow { .. } => None,
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
    /// A window of `tabs`, `panes` in it shown at once, and its `tutti` panes (rtty's id, the
    /// session's socket, tutti's id) once their sessions answer. With `session`, the window is that
    /// tutti session, and the one pane of `tabs` waits for it.
    fn new(
        uid: String,
        tabs: Tabs,
        panes: Vec<(PaneId, Pane, UnboundedReceiver<Notice>)>,
        tutti: Vec<(PaneId, PathBuf, u64)>,
        session: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe_window_appearance(window, |this, window, cx| {
            this.follow_appearance(window.appearance(), cx)
        })
        .detach();
        // The font size sets the cells the layout shares out, and `tabs` whether there is a bar.
        cx.observe_global_in::<Settings>(window, |this, window, cx| {
            this.bare = !cx.global::<Settings>().tabs;
            this.show_buttons(window);
            cx.notify()
        })
        .detach();
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
        // The title dims while the window is in the background, as macOS's do. Brought forward,
        // someone looks at its focused pane.
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                let handle = window.window_handle().downcast().expect("a terminal window");
                crate::app::key_window(handle, cx);
                if let Some(view) = this.focused() {
                    view.read(cx).shown_focused(true);
                }
            }
            cx.notify()
        })
        .detach();
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| this.close_button(window, cx)).is_err()
        });
        let mut this = TerminalWindow {
            uid,
            bare: !cx.global::<Settings>().tabs,
            tabs,
            views: HashMap::new(),
            sessions: HashMap::new(),
            attached: HashMap::new(),
            mirror: None,
            scheme,
            bar_hover: None,
            bar_press: None,
            attention: Vec::new(),
            asked: None,
            said: None,
        };
        this.show_buttons(window);
        for (id, pane, notices) in panes {
            this.add(id, pane, notices, window, cx);
        }
        if let Some(socket) = session {
            let (session, tabs, panes, selected) = (None, Vec::new(), HashMap::new(), None);
            this.mirror = Some(Mirror { socket: socket.clone(), session, tabs, panes, selected });
            this.sessions.insert(socket.clone(), Link::Opening(Vec::new()));
            this.connect(socket, window, cx);
        }
        for (id, socket, pane) in tutti {
            this.attach_tutti(id, socket, pane, window, cx);
        }
        this
    }

    /// The window as `session` keeps it: its frame (the one it goes back to from full screen),
    /// its display, its tabs, its panes' directories, and the tutti panes they were asked to
    /// show (`kept`), or the tutti session it is, whether or not its daemon answered.
    fn saved(&self, window: &Window, cx: &App) -> session::Window {
        let display = window.display(cx).and_then(|display| display.uuid().ok());
        let cwds = self.views.iter().filter_map(|(id, view)| Some((*id, view.read(cx).cwd()?)));
        session::Window {
            uid: self.uid.clone(),
            frame: window.window_bounds().get_bounds(),
            display: display.map(|uuid| uuid.to_string()),
            tabs: self.tabs.clone(),
            cwds: cwds.collect(),
            tutti: kept(&self.tabs, &self.attached),
            session: self.mirror.as_ref().map(|mirror| mirror.socket.clone()),
        }
    }

    /// Shows tutti pane `pane` of the session whose daemon listens on `socket` in pane `id`, which
    /// the layout has already: at once while the window's connection to the session is open, else
    /// once it is, opening it first, off the main thread (`connect`). The pane keeps what it was
    /// asked to show (`attached`), however that goes.
    fn attach_tutti(
        &mut self,
        id: PaneId,
        socket: PathBuf,
        pane: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.attached.insert(id, (socket.clone(), pane));
        let session = match self.sessions.get_mut(&socket) {
            Some(Link::Opening(waiting)) => return waiting.push((id, pane)),
            Some(Link::Open(session)) => session.upgrade(),
            None => None,
        };
        match session {
            Some(session) => self.show_tutti(id, &session, pane, window, cx),
            None => {
                self.sessions.insert(socket.clone(), Link::Opening(vec![(id, pane)]));
                self.connect(socket, window, cx);
                cx.notify();
            }
        }
    }

    /// Opens the window's connection to the session whose daemon listens on `socket`, off the
    /// main thread: the daemon has a few seconds to answer (`tutti::Session::open`), and the
    /// window goes on meanwhile. `opened` takes what came of it.
    fn connect(&mut self, socket: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        crate::note(&format!("tutti session {}: opening", tutti::name(&socket)));
        let opening = cx.background_spawn({
            let socket = socket.clone();
            async move {
                tutti::connect(&socket).and_then(|stream| tutti::Session::open(stream, socket))
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let opened = opening.await;
            this.update_in(cx, |this, window, cx| this.opened(socket, opened, window, cx)).ok();
        })
        .detach();
    }

    /// The window's connection to the session at `socket` opened, or why not. The panes waiting
    /// for it show their tutti panes, or say why they cannot; a window that is the session lays
    /// the session out, or says in its waiting pane why it cannot. What the session says from then
    /// on goes to `told`.
    fn opened(
        &mut self,
        socket: PathBuf,
        opened: io::Result<(tutti::Session, Vec<tutti::Workspace>, UnboundedReceiver<Told>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let waiting = match self.sessions.remove(&socket) {
            Some(Link::Opening(waiting)) => waiting,
            _ => Vec::new(),
        };
        let name = tutti::name(&socket);
        let mirrored = self.mirror.as_ref().is_some_and(|mirror| mirror.socket == socket);
        let (session, tabs, mut told) = match opened {
            Ok(opened) => opened,
            Err(err) => {
                let why = format!("cannot show tutti session {name}: {err}");
                crate::note(&why);
                // A window that is the session stays it, unopened, so the next launch asks again.
                if mirrored {
                    self.hold(self.tabs.active().focus(), &why, window, cx);
                }
                for (id, _) in waiting {
                    self.hold(id, &why, window, cx);
                }
                return;
            }
        };
        crate::note(&format!("tutti session {name}: open"));
        let session = Arc::new(session);
        self.sessions.insert(socket.clone(), Link::Open(Arc::downgrade(&session)));
        cx.spawn_in(window, async move |this, cx| {
            while let Some(said) = told.next().await {
                let heard =
                    this.update_in(cx, |this, window, cx| this.told(&socket, said, window, cx));
                if heard.is_err() {
                    break;
                }
            }
        })
        .detach();
        if let Some(mirror) = self.mirror.as_mut().filter(|_| mirrored) {
            mirror.session = Some(session.clone());
            self.mirror_layout(tabs, window, cx);
        }
        for (id, pane) in waiting {
            self.show_tutti(id, &session, pane, window, cx);
        }
    }

    /// Shows tutti pane `pane` of `session` in pane `id`, at the cells the layout gives it there,
    /// or says why it cannot; nothing once `id` is gone.
    fn show_tutti(
        &mut self,
        id: PaneId,
        session: &Arc<tutti::Session>,
        pane: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((rows, cols)) = self.cells(id, window, cx) else { return };
        let settings = cx.global::<Settings>();
        let trace = settings.launch.trace.as_ref().map(|trace| trace.for_pane(id));
        let scheme = Some(&*self.scheme);
        match Pane::attach(session.clone(), pane, rows, cols, scheme, trace, settings.graphics) {
            Ok((pane, notices)) => {
                crate::note(&format!("pane {id}: showing tutti pane at {rows}x{cols}"));
                self.add(id, pane, notices, window, cx)
            }
            Err(err) => self.hold(id, &format!("cannot show tutti pane {pane}: {err}"), window, cx),
        }
    }

    /// Pane `id` says `why` it shows nothing (`Pane::held`); nothing once `id` is gone.
    fn hold(&mut self, id: PaneId, why: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some((rows, cols)) = self.cells(id, window, cx) else { return };
        crate::note(&format!("pane {id}: {why}"));
        let (pane, notices) = Pane::held(why, rows, cols, Some(&self.scheme));
        self.add(id, pane, notices, window, cx);
    }

    /// The cells pane `id` gets in its tab, shown or not.
    fn cells(&self, id: PaneId, window: &Window, cx: &App) -> Option<(u16, u16)> {
        let (area, ..) = self.area(window, cx);
        let grids = self.tabs.all()[self.tabs.find(id)?].grids(area);
        grids.into_iter().find(|(pane, _)| *pane == id).map(|(_, rect)| (rect.rows, rect.cols))
    }

    /// The pane that shows tutti pane `pane` of the session at `socket`.
    fn tutti_pane(&self, socket: &Path, pane: u64, cx: &App) -> Option<PaneId> {
        let showing = |(_, view): &(&PaneId, &Entity<PaneView>)| {
            view.read(cx).tutti() == Some((socket, pane))
        };
        self.views.iter().find(showing).map(|(id, _)| *id)
    }

    /// What the session at `socket` said. A window that is the session lays out its tabs as they
    /// are now; any window shows a pane that something other than rtty focused, and one rtty
    /// asked tutti for, and takes on a move past the edge of tutti's tab; a window of rtty's own
    /// takes on a resize past it too (one that is the session has no split tutti does not). A
    /// refusal is said along the window's bottom. The session's end closes the window that is it:
    /// its panes went with their daemon.
    fn told(&mut self, socket: &Path, said: Told, window: &mut Window, cx: &mut Context<Self>) {
        let mirrored = self.mirror.as_ref().is_some_and(|mirror| mirror.socket == socket);
        let shown = |this: &Self, pane: u64, cx: &App| this.tutti_pane(socket, pane, cx);
        match said {
            Told::Layout(tabs) if mirrored => self.mirror_layout(tabs, window, cx),
            Told::Layout(_) => {}
            Told::Focused(pane) | Told::Created(pane) => {
                if let Some(id) = shown(self, pane, cx) {
                    self.show(id, window, cx)
                }
            }
            Told::FocusEdge(pane, direction) => {
                let moved = shown(self, pane, cx)
                    .map(|id| self.focus_toward(id, direction, false, window, cx));
                if moved == Some(None) {
                    ionmac::go(direction);
                }
            }
            Told::ResizeEdge(pane, direction) => {
                if let Some(id) = shown(self, pane, cx).filter(|_| !mirrored) {
                    self.resize(id, direction, RESIZE_STEP, window, cx)
                }
            }
            Told::Refused(why) => {
                crate::note(&format!("tutti session {}: {why}", tutti::name(socket)));
                self.say(&format!("tutti: {why}"), cx)
            }
            Told::Ended(why) => {
                crate::note(&format!("tutti session {}: {why}", tutti::name(socket)));
                if mirrored {
                    self.close_window(false, window, cx)
                }
            }
        }
    }

    /// The session's tabs, `workspaces`, as the window's own: tutti's split trees as its layouts,
    /// each tab keeping its focus and zoom while their panes stay (`Layout::mirror`), the tab that
    /// showed showing still, else tutti's current one. Panes new to the window are shown, and
    /// panes gone from the session taken out, which detaches from them. A session with no pane
    /// left closes the window, as a last pane's close does.
    fn mirror_layout(
        &mut self,
        workspaces: Vec<tutti::Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mirror) = self.mirror.as_mut() else { return };
        let Some(session) = mirror.session.clone() else { return };
        let mirrored = self.tabs.mirror(&mirror.tabs, &workspaces, &mirror.panes, next_pane_id);
        let Some((tabs, ids, named)) = mirrored else {
            crate::note(&format!("tutti session {}: no pane left", tutti::name(&mirror.socket)));
            return self.close_window(false, window, cx);
        };
        let (tabs, ids, named) = match self.bare {
            true => tabs.shown_only(ids, named),
            false => (tabs, ids, named),
        };
        let kept: Vec<PaneId> = named.values().copied().collect();
        let gone: Vec<PaneId> =
            mirror.panes.values().filter(|id| !kept.contains(id)).copied().collect();
        let new: Vec<(PaneId, u64)> = named
            .iter()
            .filter(|(_, id)| !self.views.contains_key(id))
            .map(|(pane, id)| (*id, *pane))
            .collect();
        (mirror.panes, mirror.tabs) = (named, ids);
        self.tabs = tabs;
        self.take_out(&gone, None, window, cx);
        for (id, pane) in new {
            self.show_tutti(id, &session, pane, window, cx);
        }
        self.focus_shown(window, cx);
    }

    /// The session of the window that is one, and tutti's id for its pane `id`; or what to say
    /// instead.
    fn mirrored(&self, id: PaneId) -> Result<(Arc<tutti::Session>, tutti::PaneId), String> {
        let mirror = self.mirror.as_ref().ok_or("not a tutti session's window")?;
        let session = mirror.session.clone().ok_or(UNANSWERED)?;
        let shown = mirror.panes.iter().find(|(_, shown)| **shown == id);
        let pane = shown.map(|(pane, _)| tutti::PaneId(*pane));
        Ok((session, pane.ok_or(format!("pane {id} shows no pane of the session"))?))
    }

    /// Asks the session the window is what `request` makes of tutti's id for pane `id`, else says
    /// why it cannot.
    fn ask_tutti(
        &mut self,
        id: PaneId,
        request: impl FnOnce(tutti::PaneId) -> tutti::Request,
        cx: &mut Context<Self>,
    ) {
        match self.mirrored(id) {
            Ok((session, pane)) => session.ask(&request(pane)),
            Err(why) => self.say(&why, cx),
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
        // Without tabs there is no bar, as for a lone tab in full screen.
        let tabs = if self.bare { 1 } else { self.tabs.all().len() };
        Bar::new(width, tabs, self.bare || window.is_fullscreen())
    }

    /// The close, minimize and zoom buttons show with the bar they sit in, and go with it: a
    /// window with no bar (`tabs = false`) is its window manager's to move and close. The window
    /// stays a titled one, as the Accessibility API knows a standard window.
    fn show_buttons(&self, window: &Window) {
        let Some(window) = ns_window(window) else { return };
        let buttons = [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ];
        for button in buttons.into_iter().filter_map(|button| window.standardWindowButton(button)) {
            button.setHidden(self.bare);
        }
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
    /// whose password prompt holds Secure Keyboard Entry; a tutti pane's session hears the window
    /// shows it focused, active or not, and seen while the window is the active one. A window that
    /// is a tutti session makes the tab it shows the session's current one, where tutti's runs
    /// land.
    fn focus_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.tabs.active().focus();
        if let Some(view) = self.views.get(&focus) {
            view.read(cx).focus(window);
            view.read(cx).shown_focused(window.is_window_active());
        }
        window.set_window_title(&self.title(focus, cx));
        cx.defer(crate::app::refresh_secure_input);
        cx.notify();
        if let Some(mirror) = self.mirror.as_mut()
            && let (Some(session), Some(&(_, tab))) =
                (&mirror.session, mirror.tabs.get(self.tabs.index()))
            && mirror.selected.replace(tab) != Some(tab)
        {
            session.ask(&tutti::Request::TabSelect { id: tutti::TabId(tab) });
        }
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
                    if let Some(view) = self.views.get(&id) {
                        view.read(cx).shown_focused(window.is_window_active());
                    }
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
                if cx.global::<Settings>().launch.scripted {
                    self.attention.push(format!("bell in pane {id}: the dock asked for attention"));
                }
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
                if cx.global::<Settings>().launch.scripted {
                    self.attention
                        .push(format!("notification from pane {id} {title:?} {body:?}: {fate}"));
                }
            }
            // tutti keeps an exited pane until it is killed: so does the window that is its
            // session, showing its last screen.
            PaneEvent::Exited(status) if self.mirror.is_some() => {
                crate::note(&format!("pane {id}: its tutti pane's stream ended: {status}"))
            }
            PaneEvent::Exited(status) => {
                crate::note(&format!("pane {id}: its program exited: {status}"));
                leave_undo(id, cx);
                self.close(id, false, window, cx)
            }
            PaneEvent::Edit(at) => self.edit(id, at, window, cx),
            PaneEvent::ClipboardRead { selection, terminator } => {
                self.clipboard_read(id, selection.clone(), terminator, window, cx)
            }
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
        let command =
            Program::Command(editor::command(cx.global::<Settings>().editor.as_deref(), at));
        if let Err(err) = self.split(from, Direction::Right, Some(command), window, cx) {
            self.say(&err, cx);
        }
    }

    /// Pane `id`'s program asks for the clipboard (OSC 52), and gets it as `clipboard_read` says:
    /// for "ask", once a question on the window is answered Allow.
    fn clipboard_read(
        &mut self,
        id: PaneId,
        selection: String,
        terminator: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.views.get(&id).cloned() else { return };
        let asked = match cx.global::<Settings>().clipboard_read {
            ClipboardRead::Deny => {
                return crate::note(&format!("pane {id}: clipboard read denied"));
            }
            ClipboardRead::Allow => None,
            ClipboardRead::Ask => {
                // Called by what reads, not by what ran a moment ago.
                view.update(cx, |view, _| view.refresh_title());
                let question = format!("{} wants to read the clipboard", self.title(id, cx));
                let detail = "Allow sends it the text you copied last.";
                // A stray Return must not hand the clipboard over.
                Some(self.ask(&question, detail, ["Allow", "Deny"], true, window, cx))
            }
        };
        cx.spawn_in(window, async move |_, cx| {
            let allowed = match asked {
                Some(answer) => answer.await == Ok(0),
                None => true,
            };
            let fate = if allowed { "allowed" } else { "denied" };
            crate::note(&format!("pane {id}: clipboard read {fate}"));
            if allowed {
                view.update(cx, |view, cx| view.answer_clipboard(&selection, terminator, cx)).ok();
            }
        })
        .detach();
    }

    /// Every pane in the window's tabs.
    fn panes(&self) -> Vec<PaneId> {
        self.tabs.all().iter().flat_map(|tab| tab.panes()).collect()
    }

    /// What runs in `panes` other than shells at their prompts (`PaneView::running`).
    fn running(&self, panes: &[PaneId], cx: &App) -> Vec<String> {
        panes.iter().filter_map(|id| self.views.get(id)?.read(cx).running()).collect()
    }

    /// Asks `question` in a sheet on the window, `go` its default button and `stop` its Esc; or,
    /// `deliberate`, with Return and Esc both `stop`, so only a click says `go`
    /// (`deliberate_sheet`).
    /// Under `--script` the question waits in the dump for the script's `answer`. The button
    /// pressed, 0 for `go`.
    fn ask(
        &mut self,
        question: &str,
        detail: &str,
        [go, stop]: [&'static str; 2],
        deliberate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> oneshot::Receiver<usize> {
        match (cx.global::<Settings>().launch.scripted, deliberate) {
            (false, true) => deliberate_sheet(question, detail, [go, stop], window),
            (false, false) => {
                let buttons = [PromptButton::ok(go), PromptButton::cancel(stop)];
                window.prompt(PromptLevel::Warning, question, Some(detail), &buttons, cx)
            }
            (true, _) => {
                let keys =
                    if deliberate { format!(" (Return and Esc: {stop})") } else { String::new() };
                self.attention.push(format!("asked {question:?} {detail:?} [{go}, {stop}]{keys}"));
                let (asked, answer) = oneshot::channel();
                self.asked = Some(asked);
                answer
            }
        }
    }

    /// Does `then` at once while nothing but shells runs in what `closing` ends, or closing does
    /// not ask (`confirm_close = false`); else asks first (`ask`), and does it on the go-ahead.
    fn confirm(
        &mut self,
        closing: Closing,
        running: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let asks = cx.global::<Settings>().confirm_close;
        let Some((question, detail, button)) = question(closing, &running).filter(|_| asks) else {
            return then(self, window, cx);
        };
        crate::note(&format!("{question} asked first: {} running", running.len()));
        let answer = self.ask(&question, &detail, [button, "Cancel"], false, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let go = answer.await == Ok(0);
            crate::note(&format!("{question} {}", if go { button } else { "Cancel" }));
            if go {
                this.update_in(cx, |this, window, cx| then(this, window, cx)).ok();
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
        if self.mirror.is_some() {
            return self.say(DETACHES, cx);
        }
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

    /// Splits `from`, showing its tab; the new pane shows `program`, else runs the login shell, a
    /// command in `from`'s working directory, and takes the keyboard. Its id; none from a window
    /// that is a tutti session, which asks tutti to split instead (right or down, as tutti does),
    /// and shows the new pane once tutti has it (`Told::Created`).
    fn split(
        &mut self,
        from: PaneId,
        direction: Direction,
        program: Option<Program>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Option<PaneId>, String> {
        if self.mirror.is_some() {
            let direction = match (direction, program) {
                (_, Some(_)) => return Err(SESSION_ONLY.into()),
                (Direction::Right, None) => tutti::Axis::Horizontal,
                (Direction::Down, None) => tutti::Axis::Vertical,
                _ => return Err("tutti splits a pane right or down".into()),
            };
            let (session, pane) = self.mirrored(from)?;
            session.ask(&tutti::Request::PaneSplit { pane, direction });
            return Ok(None);
        }
        self.tabs.select(self.tabs.find(from).ok_or(format!("no pane {from}"))?);
        let (area, ..) = self.area(window, cx);
        let id = next_pane_id();
        let mut layout = self.tabs.active().clone();
        layout.split(from, direction, id);
        let rects = layout.rects(area);
        let (_, rect) =
            rects.iter().find(|(pane, _)| *pane == id).expect("the new pane is laid out");
        crate::note(&format!("pane {from}: splitting {} into pane {id}", direction.name()));
        match program.unwrap_or_else(|| Program::Command(CommandBuilder::new_default_prog())) {
            Program::Command(mut command) => {
                if let Some(cwd) = self.views.get(&from).and_then(|view| view.read(cx).cwd()) {
                    command.cwd(cwd);
                }
                let (pane, notices) = spawn(command, id, rect.rows, rect.cols, &self.scheme, cx)?;
                *self.tabs.active_mut() = layout;
                self.add(id, pane, notices, window, cx);
            }
            Program::Tutti { socket, pane } => {
                *self.tabs.active_mut() = layout;
                self.attach_tutti(id, socket, pane, window, cx);
            }
        }
        Ok(Some(id))
    }

    fn split_focused(&mut self, direction: Direction, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.split(self.tabs.active().focus(), direction, None, window, cx) {
            self.say(&err, cx);
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
                    // Past the window's edge the move is the window manager's, when it takes it.
                    None if !wrap && ionmac::go(direction) => Response::Done,
                    None => Response::Edge { from, direction },
                }
            }
            Request::Resize { from, direction, amount } => {
                self.resize(from.unwrap_or(focused), direction, amount, window, cx);
                Response::Done
            }
            Request::Split { pane, direction } => {
                match self.split(pane.unwrap_or(focused), direction, None, window, cx) {
                    Ok(Some(pane)) => Response::Pane(pane),
                    Ok(None) => Response::Done,
                    Err(err) => Response::Error(err),
                }
            }
            Request::Zoom { pane } => {
                self.show(pane.unwrap_or(focused), window, cx);
                self.tabs.active_mut().toggle_zoom();
                cx.notify();
                Response::Done
            }
            Request::TuttiAttach { socket, pane, from, split } => {
                let program = Program::Tutti { socket, pane };
                let opened = match split {
                    Some(direction) => self
                        .split(from.unwrap_or(focused), direction, Some(program), window, cx)
                        .map(|id| id.expect("a window of rtty's own splits itself")),
                    None => self.open_tab(program, window, cx),
                };
                opened.map_or_else(Response::Error, Response::Pane)
            }
            Request::TuttiOpen { .. } | Request::NewWindow { .. } => {
                Response::Error("not a pane's request".into())
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
                    let tutti = view.tutti().map(|(socket, pane)| (tutti::name(socket), pane));
                    Some(PaneInfo {
                        id,
                        focused: id == tab.focus(),
                        title: view.title().text.clone(),
                        cwd: view.cwd(),
                        rows,
                        cols,
                        claims_keys: view.claims_keys(),
                        tutti,
                        progress: view.progress(),
                        vars: view.vars().clone(),
                    })
                })
                .collect(),
        });
        WindowInfo {
            id: window.window_handle().window_id().as_u64(),
            uid: self.uid.clone(),
            window_number: ns_window(window).map(|window| window.windowNumber()),
            active: window.is_window_active(),
            tabs: tabs.collect(),
        }
    }

    /// A new tab, a login shell in it. A window that is a tutti session asks tutti for one in the
    /// workspace of the tab it shows, and a shell in it, as tutti's own new tab and its run do,
    /// and shows it once tutti has it (`Told::Created`).
    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mirror) = &self.mirror {
            let Some(session) = mirror.session.clone() else { return self.say(UNANSWERED, cx) };
            let shown = mirror.tabs.get(self.tabs.index());
            let workspace = shown.map(|(workspace, _)| tutti::WorkspaceId(*workspace));
            session.ask(&tutti::Request::TabNew { workspace });
            let cmd = vec![CommandBuilder::new_default_prog().get_shell()];
            let run = tutti::Request::PaneRun { tab: None, cmd, ephemeral: false, sandbox: None };
            return session.ask(&run);
        }
        let shell = Program::Command(shell(self.focused_cwd(cx)));
        if let Err(err) = self.open_tab(shell, window, cx) {
            self.say(&err, cx);
        }
    }

    /// Opens a tab after the current one with `program` in its one pane, which takes the
    /// keyboard; not in a window that is a tutti session, whose tabs are tutti's.
    fn open_tab(
        &mut self,
        program: Program,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PaneId, String> {
        if self.mirror.is_some() {
            return Err(SESSION_ONLY.into());
        }
        let (area, ..) = self.area(window, cx);
        let id = next_pane_id();
        if self.bare {
            // Without tabs, a window of its own, opened once this one's update is done.
            cx.defer(move |cx| {
                if let Err(err) = open_at(id, program, None, cx) {
                    eprintln!("rtty: {err}");
                }
            });
            return Ok(id);
        }
        match program {
            Program::Command(command) => {
                let (pane, notices) = spawn(command, id, area.rows, area.cols, &self.scheme, cx)?;
                self.tabs.open(id);
                self.add(id, pane, notices, window, cx);
            }
            Program::Tutti { socket, pane } => {
                self.tabs.open(id);
                self.attach_tutti(id, socket, pane, window, cx);
            }
        }
        Ok(id)
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
        if let Err(err) = self.open_tab(Program::Command(command), window, cx) {
            self.say(&err, cx);
        }
    }

    fn new_window(&mut self, _: &NewWindow, _: &mut Window, cx: &mut Context<Self>) {
        let command = Program::Command(shell(self.focused_cwd(cx)));
        // Opened once this window's update is done, so it cascades from this one.
        cx.defer(move |cx| {
            if let Err(err) = open(command, cx) {
                eprintln!("rtty: {err}");
            }
        });
    }

    /// `kill-pane`: ends the focused pane's program now, without asking (`Pane::kill`); its exit
    /// closes the pane. Closing a tutti pane only detaches from it.
    fn kill_pane(&mut self, _: &KillPane, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.focused() {
            crate::note(&format!("pane {}: killed", self.tabs.active().focus()));
            view.update(cx, |view, _| view.kill());
        }
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

    /// Moves the divider nearest `from` across `direction` by `amount`. A window that is a tutti
    /// session asks tutti to, which moves that split's ratio by the share `amount` is of it
    /// (`Layout::share`); with no such split, tutti hands the resize back, and there is none here
    /// either.
    fn resize(
        &mut self,
        from: PaneId,
        direction: Direction,
        amount: Amount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mirror.is_some() {
            let (area, ..) = self.area(window, cx);
            let layout = self.tabs.find(from).map(|tab| &self.tabs.all()[tab]);
            let delta = layout.and_then(|layout| layout.share(from, direction, amount, area));
            let toward = |pane| tutti::Request::PaneResizeToward {
                from: Some(pane),
                toward: direction.into(),
                delta: delta.unwrap_or(0.),
            };
            return self.ask_tutti(from, toward, cx);
        }
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
        let before = self.tabs.active().divided(&path);
        self.tabs.active_mut().drag(&path, area, (col, row));
        cx.notify();
        // A window that is a tutti session asks tutti to move it too, as tutti's border drag
        // does, whenever it crosses a cell; the layout tutti then tells of settles it.
        let Some((a, b, ratio)) =
            self.tabs.active().divided(&path).filter(|_| self.mirror.is_some())
        else {
            return;
        };
        if before.is_some_and(|(.., was)| was != ratio)
            && let (Ok((session, a)), Ok((_, b))) = (self.mirrored(a), self.mirrored(b))
        {
            session.ask(&tutti::Request::PaneSplitRatio { a, b, ratio });
        }
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
        // The close button as AppKit has it: hidden with the bar, under `tabs = false`.
        let close = ns_window(window)
            .and_then(|window| window.standardWindowButton(NSWindowButton::CloseButton));
        let buttons = close.map(|button| if button.isHidden() { "hidden" } else { "shown" });
        let _ = writeln!(out, "window buttons {buttons:?} uid {}", self.uid);
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
        if let Some(mirror) = &self.mirror {
            let panes: BTreeMap<u64, PaneId> = mirror.panes.iter().map(|(t, r)| (*t, *r)).collect();
            let (name, tabs) = (tutti::name(&mirror.socket), &mirror.tabs);
            let open = mirror.session.is_some();
            let _ = writeln!(out, "tutti session {name} open {open} tabs {tabs:?} panes {panes:?}");
        }
        let rects = layout.rects(area);
        for (id, rect) in &rects {
            let tutti = self.views.get(id).and_then(|view| {
                let (socket, pane) = view.read(cx).tutti()?;
                Some(format!(" tutti {}:{pane}", tutti::name(socket)))
            });
            let (row, col, rows, cols) = (rect.row, rect.col, rect.rows, rect.cols);
            let tutti = tutti.unwrap_or_default();
            let _ = writeln!(out, "pane {id} at {row},{col} {rows}x{cols}{tutti}");
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
                if this.mirror.is_some() {
                    return this.say(DETACHES, cx);
                }
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
                let from = this.tabs.active().focus();
                if this.focus_toward(from, action.direction, false, window, cx).is_none() {
                    ionmac::go(action.direction);
                }
            }))
            .on_action(cx.listener(|this, action: &ResizePane, window, cx| {
                this.resize(this.tabs.active().focus(), action.direction, RESIZE_STEP, window, cx)
            }))
            .on_action(cx.listener(|this, _: &LastPane, window, cx| {
                this.tabs.active_mut().focus_previous();
                this.focus_shown(window, cx);
            }))
            .on_action(cx.listener(|this, action: &SwapPane, window, cx| {
                if this.mirror.is_some() {
                    return this.say("tutti swaps no panes", cx);
                }
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
            .on_action(cx.listener(Self::kill_pane))
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

    /// What a window keeps of its tutti panes is what each was asked to show, whatever came of
    /// it (held, its daemon never answering, or still waiting), for the panes it still holds;
    /// restored, those ask again and the rest are shells.
    #[test]
    fn tutti_panes_come_back_as_asked_whether_or_not_they_showed() {
        let mut tabs = Tabs::new(1);
        tabs.active_mut().split(1, Direction::Right, 2);
        tabs.open(3);
        let socket = PathBuf::from("/run/tutti/work.sock");
        let asked = |pane| (socket.clone(), pane);
        let attached = HashMap::from([(2, asked(7)), (3, asked(8)), (9, asked(9))]);
        let tutti = kept(&tabs, &attached);
        assert_eq!(tutti, BTreeMap::from([(2, asked(7)), (3, asked(8))]), "9 closed since");
        let frame = Bounds::new(point(px(0.), px(0.)), size(px(900.), px(600.)));
        let cwds = BTreeMap::new();
        let (uid, display, session) = (String::new(), None, None);
        let saved = session::Window { uid, frame, display, tabs, cwds, tutti, session };
        let back: Vec<_> = programs(&saved)
            .into_iter()
            .map(|(id, program)| match program {
                Program::Tutti { socket, pane } => (id, Some((socket, pane))),
                Program::Command(_) => (id, None),
            })
            .collect();
        assert_eq!(back, [(1, None), (2, Some(asked(7))), (3, Some(asked(8)))]);
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
