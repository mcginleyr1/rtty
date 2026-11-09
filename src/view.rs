use std::collections::{BTreeMap, HashMap, HashSet};
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
use global_hotkey::hotkey::HotKey;
use gpui::{
    App, BorderStyle, Bounds, ClipboardItem, ContentMask, Context, Corners, DispatchPhase, Div,
    ElementInputHandler, EntityInputHandler, EventEmitter, ExternalPaths, FocusHandle, Font,
    FontId, FontWeight, Global, Hsla, KeyDownEvent, KeyUpEvent, Keystroke, LineLayout,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    PathBuilder, Pixels, Point, Render, RenderImage, Rgba, ScrollWheelEvent, ShapedGlyph,
    ShapedLine, ShapedRun, Size, Subscription, Task, TextRun, UTF16Selection, UnderlineStyle,
    Window, WindowAppearance, WindowTextSystem, actions, canvas, div, fill, outline, point,
    prelude::*, px, size,
};
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSArray, NSString, NSURL};
use regex::Regex;
use rtty_vt::input::{self, Action, Button, Mouse};
use rtty_vt::{
    BACKGROUND, Event, FOREGROUND, Mark, MarkKind, MouseEncoding, MouseMode, Rgb, Underline,
};

use crate::config::{Action as Bound, Bell, ClipboardRead, OptionAsAlt, Overrides, Theme};
use crate::copy::{self, Effect};
use crate::editor;
use crate::glyphs::{self, Seg, Shape};
use crate::keys;
use crate::links::{self, Link, Location, Opening, Target};
use crate::pane::{Notice, Pane, Trace};
use crate::progress::{self, Progress};
use crate::prompt;
use crate::quick;
use crate::render::{self, CursorShape, Drawn, Line, Palette, Run, Scene};
use crate::scheme::Scheme;
use crate::search::{self, Match, Query};
use crate::selection::{Kind, Point as GridPoint, Selection, Side};
use crate::title::{self, Title};
use crate::window::{ClosePane, LastPane, MoveFocus, ResizePane};

actions!(
    rtty,
    [
        Copy,
        CopyLastOutput,
        Paste,
        Find,
        PreviousPrompt,
        NextPrompt,
        ClearScrollback,
        QuickSelect,
        QuickOpen,
        CopyMode
    ]
);

/// How long a synchronized update (DEC mode 2026) may hold the last frame on screen.
const SYNC_TIMEOUT: Duration = Duration::from_millis(150);

/// While output arrives, an open search looks again at most this often.
const SEARCH_REFRESH: Duration = Duration::from_millis(100);

/// How long a bell flashes the pane.
const FLASH: Duration = Duration::from_millis(150);

/// How long a word on what a key did (copied the last output) stays over the pane.
const STATUS: Duration = Duration::from_millis(1500);

/// What a program reading the clipboard gets under `--script`, in place of the clipboard.
pub const STAND_IN: &str = "rtty --script's clipboard";

/// A blinking cursor is on for this long, then off for as long; it holds still once the
/// keyboard has been idle for `BLINK_FOR`, so an idle window draws nothing.
const BLINK_MS: u128 = 500;
const BLINK_FOR: Duration = Duration::from_secs(10);

/// A selecting drag held past the pane's top or bottom scrolls the view this many lines a second
/// at the edge, and as many more for each cell further out; a step a frame.
const AUTOSCROLL_SPEED: f32 = 10.;
const AUTOSCROLL_TICK: Duration = Duration::from_millis(16);

/// What every window and pane shares: the font and its size (Cmd+= and Cmd+- change it for all of
/// them), the color theme and the rest of the config, which a reload replaces, and what this
/// launch was told.
pub struct Settings {
    pub font: Font,
    pub font_size: Pixels,
    /// The configured size (`size`, or `--size`), which Cmd+0 goes back to.
    pub size: Pixels,
    /// The cell's height as a share of the font's (`line_height`).
    pub line_height: f32,
    pub theme: Theme,
    /// Every chord rtty acts on and its action (`Config::bindings`).
    pub bindings: BTreeMap<String, Bound>,
    pub option_as_alt: OptionAsAlt,
    pub bell: Bell,
    /// `minimum_contrast`: the WCAG contrast ratio text keeps with its background.
    pub minimum_contrast: f32,
    /// `confirm_close`: closing what runs a program asks first.
    pub confirm_close: bool,
    /// How long a close waits for `undo-close`; zero closes at once (`undo_timeout`).
    pub undo_timeout: Duration,
    /// Where the shell integration's scripts are, when login shells load it.
    pub shell_integration: Option<PathBuf>,
    /// A command that ran this long notifies its end out of view; `None` never.
    pub notify_after: Option<Duration>,
    pub click_to_move: bool,
    /// `editor`: what edits a file in a new pane, else the shell's `$EDITOR`.
    pub editor: Option<Vec<String>>,
    /// `quick_select_patterns`: what quick select labels besides what it knows.
    pub quick_select: Vec<Regex>,
    /// The layout is kept for the next launch (`restore_layout`), when this launch keeps it.
    pub restore_layout: bool,
    /// Secure Keyboard Entry: `secure_input` turns its toggle on, and `auto_secure_input` holds
    /// it at a password prompt (`app::refresh_secure_input`).
    pub secure_input: bool,
    pub auto_secure_input: bool,
    /// `global_hotkey`, as macOS registers it (`app::hotkey_pressed`).
    pub hotkey: Option<HotKey>,
    /// What a program's OSC 52 read of the clipboard gets (`clipboard_read`).
    pub clipboard_read: ClipboardRead,
    pub launch: Launch,
    /// `graphics`: programs show images with kitty's graphics protocol.
    pub graphics: bool,
}

impl Global for Settings {}

/// What this launch was told, which a reload keeps: the diagnostics asked for, where the config
/// is, and how a scripted run differs.
#[derive(Clone)]
pub struct Launch {
    pub stats: bool,
    pub trace: Option<Trace>,
    /// Where `rtty cli` reaches this rtty; each pane's `RTTY_SOCKET`.
    pub socket: PathBuf,
    /// The config file, `None` when there is no home to find it in.
    pub config: Option<PathBuf>,
    pub overrides: Overrides,
    /// Where the windows' layout is kept (`session::path`), when this launch restores it and
    /// saves it: not one with `-e`, under `--script` (short of `--restore`), or while another
    /// rtty runs, which keeps it.
    pub layout: Option<PathBuf>,
    /// Cmd+click opens links with the system's handler; under `--script` it records them in the
    /// pane's dump instead, so a scripted run never opens anything.
    pub open_links: bool,
    /// The last command's output goes to the clipboard, and a program's OSC 52 read gets it;
    /// under `--script` the output goes to the pane's dump only and a read gets `STAND_IN`, so a
    /// scripted run leaves the clipboard alone and never shows it.
    pub clipboard: bool,
    /// Asking is a native sheet; under `--script` it is recorded in the dump instead, and the
    /// script's `answer` step answers it, so a scripted run never leaves a dialog open.
    pub prompts: bool,
}

/// A pane as the window shows it: the pane itself, its keyboard focus, and what its last frame
/// put where.
pub struct PaneView {
    pane: Pane,
    focus: FocusHandle,
    /// The theme's scheme for the window's current appearance.
    scheme: Arc<Scheme>,
    geometry: Option<Geometry>,
    cache: Cache,
    /// The engine's images as GPUI draws them, by generation, until the engine lets them go.
    images: HashMap<u64, Arc<RenderImage>>,
    marked_text: Option<String>,
    scroll_remainder: Pixels,
    /// The cell the last mouse report named.
    mouse_cell: Option<(u16, u16)>,
    /// A button went down in this pane and has not come up: the drag is this pane's.
    pressed: bool,
    /// Where the pointer is while a left-button drag selects, in the pane or anywhere past it.
    selecting: Option<Point<Pixels>>,
    /// While that pointer is past the pane's top or bottom: the steps scrolling the view to it.
    autoscroll: Option<Task<()>>,
    /// The program claimed the chords bound to focus, resize, close and last pane (user var
    /// `RTTY_KEYS=pane`), until it clears the var or a shell draws its next prompt (OSC 133;A).
    claims_keys: bool,
    /// The server of a Neovim running rtty's plugin here (user var `RTTY_NVIM`), which edits the
    /// files Cmd+clicks open, until it clears the var or a shell draws its next prompt.
    nvim: Option<PathBuf>,
    /// Keys whose press went to the program and whose release has not.
    keys_down: Vec<String>,
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
    /// Cmd+F's search, while its bar is open.
    search: Option<Search>,
    /// Quick select's labels, while they show.
    quick: Option<Quick>,
    /// Copy mode, while it is on: a key no binding takes is its, never the program's.
    copy: Option<copy::Mode>,
    /// Every key goes to `quick_key` first, ahead of the bindings and the program.
    _keys: Subscription,
    /// Where the pointer last was over the pane.
    pointer: Option<Point<Pixels>>,
    /// The link under the pointer while Cmd is held, and its target when its text is not that
    /// (`links::disguise`).
    link: Option<(Link, Option<String>)>,
    /// What Cmd+clicks on links did, or under `--script` would have.
    followed: Vec<String>,
    /// When the last bell flashed the pane.
    flashed: Option<Instant>,
    /// The command running since its output's mark (OSC 133;C), and its command line.
    command: Option<(Instant, Option<String>)>,
    /// A word on what a key did, and when, shown over the pane for `STATUS`.
    status: Option<(&'static str, Instant)>,
    /// What the last command's output key copied, or under `--script` would have.
    copied: Option<String>,
    /// Where a left click that may move the shell's cursor went down (`prompt::cursor_moves`).
    click: Option<GridPoint>,
    /// What the pane is called, as of the last look (`refresh_title`).
    title: Title,
    /// What the program last said of its progress, until it removes it or a shell draws its
    /// next prompt (OSC 133;A).
    progress: Option<Progress>,
    stats: Option<FrameStats>,
}

/// A search of the pane's scrollback and screen: the query, its matches top to bottom, and the
/// one shown.
#[derive(Default)]
struct Search {
    query: Query,
    matches: Vec<Match>,
    current: Option<usize>,
    /// Why the query is no regex.
    error: Option<String>,
    /// The look again that output asked for, a moment after it.
    refresh: Option<Task<()>>,
    /// Copy mode took the query (Enter): the bar stays, its matches for `n` and `N`, and keys go
    /// to copy mode.
    accepted: bool,
}

/// Quick select or quick open, labels on screen: what each picks, the letters typed so far, and
/// whether one of them was typed with Shift.
struct Quick {
    items: Vec<quick::Item<Pick>>,
    typed: String,
    shifted: bool,
}

/// What a label picks: text to copy, or the link a Cmd+click on it would have followed when the
/// labels were made, whatever the screen shows there since.
#[derive(Clone, Debug)]
enum Pick {
    Copy(String),
    Open(Target),
}

/// What a pane tells the window that holds it.
pub enum PaneEvent {
    /// The program named itself.
    Title,
    /// A click gave the pane the keyboard.
    Focused,
    /// BEL.
    Bell,
    /// OSC 9, OSC 777;notify or OSC 99.
    Notification { title: Option<String>, body: Option<String> },
    /// The program's progress changed (`PaneView::progress`).
    Progress,
    /// OSC 52: the program asks for the clipboard (`PaneView::answer_clipboard`).
    ClipboardRead { selection: String, terminator: &'static str },
    /// The program exited, as its status says.
    Exited(String),
    /// A Cmd+click on a text file: the window has it edited (`TerminalWindow::edit`).
    Edit(Location),
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
    shaped: Vec<Shaped>,
}

/// A run shaped, and where and how big to draw each glyph that would not keep to its cells
/// (`fit`).
struct Shaped {
    line: ShapedLine,
    fits: Option<Vec<Option<Fit>>>,
}

/// A glyph drawn at a share of the font size, moved from where the shaper put it.
#[derive(Clone, Copy, Debug)]
struct Fit {
    scale: f32,
    offset: Point<Pixels>,
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
    selection: (Option<(GridPoint, GridPoint)>, bool),
    matches: (Vec<Match>, Option<Match>),
    link: Option<(GridPoint, GridPoint)>,
    focused: bool,
    size: (u16, u16),
    font: Font,
    font_size: Pixels,
    cell: Size<Pixels>,
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
    /// Images over the cells, lowest first.
    pictures: Vec<Picture>,
    cursor: Option<PaintQuad>,
    marked_text: Option<(PaintQuad, Point<Pixels>, ShapedLine)>,
    /// A bell's flash over the pane: the text color, faint.
    flash: Option<Hsla>,
    layout_time: Duration,
}

/// An image drawn whole at `bounds`, showing only within `clip`: the part of it a placement
/// shows, over the cells it covers, in the pane.
#[derive(Clone)]
struct Picture {
    image: Arc<RenderImage>,
    bounds: Bounds<Pixels>,
    clip: Bounds<Pixels>,
    z: i32,
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
        // A closed pane's images leave the window's sprite atlas with it.
        cx.on_release_in(window, |view, window, _| {
            for (_, image) in view.images.drain() {
                window.drop_image(image).ok();
            }
        })
        .detach();
        let settings = cx.global::<Settings>();
        let scheme = settings.theme.for_appearance(window.appearance()).clone();
        let stats = settings.launch.stats.then(FrameStats::default);
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
        let this = cx.weak_entity();
        let keys = cx.intercept_keystrokes(move |event, window, cx| {
            this.update(cx, |view, cx| view.quick_key(&event.keystroke, window, cx)).ok();
        });
        let (rows, cols) = pane.lock().size();
        let mut view = Self {
            pane,
            focus,
            scheme,
            geometry: None,
            cache: Cache::default(),
            images: HashMap::new(),
            marked_text: None,
            scroll_remainder: px(0.),
            mouse_cell: None,
            pressed: false,
            selecting: None,
            autoscroll: None,
            claims_keys: false,
            nvim: None,
            keys_down: Vec::new(),
            grid: (rows, cols, (0, 0)),
            focused: window.is_window_active(),
            sync_since: None,
            blink_since: None,
            blink_wake: None,
            search: None,
            quick: None,
            copy: None,
            _keys: keys,
            pointer: None,
            link: None,
            followed: Vec::new(),
            flashed: None,
            command: None,
            status: None,
            copied: None,
            click: None,
            title: Title::default(),
            progress: None,
            stats,
        };
        view.refresh_title();
        view
    }

    /// The keyboard came to or left this pane, or the window was activated or deactivated: draw
    /// the cursor accordingly and tell a program that asked (mode 1004) with `CSI I` / `CSI O`.
    /// Quick select's labels go with the keyboard.
    fn focus_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            self.quick = None;
        }
        let focused = self.focus.is_focused(window) && window.is_window_active();
        let changed = mem::replace(&mut self.focused, focused) != focused;
        if changed && self.pane.lock().modes().focus_reports {
            self.pane.write(if focused { b"\x1b[I" } else { b"\x1b[O" }.to_vec());
        }
        if changed && focused {
            self.pane.focused();
        }
        cx.notify();
    }

    /// Gives this pane the keyboard.
    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    /// Gives the terminal, and through the pty its program, `rows`×`cols` cells of `cell` on a
    /// display of `scale` device pixels a point: the window decides from its layout, for shown and
    /// hidden panes alike.
    pub fn resize(&mut self, rows: u16, cols: u16, cell: Size<Pixels>, scale: f32) {
        let grid = (rows, cols, device_pixels(cell, scale));
        if mem::replace(&mut self.grid, grid) != grid {
            self.pane.resize(rows, cols, grid.2);
            // Reflow renumbers the rows matches were found on, and copy mode's cursor's.
            self.leave_copy();
            self.research(false);
        }
    }

    /// Cmd+F: opens the search bar, which takes the keyboard until Esc.
    fn find(&mut self, _: &Find, _: &mut Window, cx: &mut Context<Self>) {
        self.search.get_or_insert_with(Search::default).accepted = false;
        cx.notify();
    }

    /// Looks for the query again. After an edit the current match is the last one starting above
    /// the view's bottom, and shows; otherwise it stays on the same text wherever output moved it,
    /// and the view stays put.
    fn research(&mut self, edited: bool) {
        let Some(search) = self.search.as_mut() else { return };
        let term = self.pane.lock();
        let (matches, error) = match search.query.pattern() {
            None => (Vec::new(), None),
            Some(Ok(pattern)) => (search::find(&term, &pattern), None),
            Some(Err(err)) => (Vec::new(), Some(err)),
        };
        let bottom = self.pane.top(&term) + u64::from(self.grid.0);
        drop(term);
        let previous = search.current.and_then(|index| search.matches.get(index).copied());
        let last = matches.len().checked_sub(1);
        search.current = match previous {
            _ if edited => matches.iter().rposition(|&((row, _), _)| row < bottom).or(last),
            Some(old) => last.map(|last| matches.partition_point(|found| *found < old).min(last)),
            None => last,
        };
        (search.matches, search.error) = (matches, error);
        if edited {
            self.reveal_match();
        }
    }

    /// Scrolls the view to the current match.
    fn reveal_match(&mut self) {
        let current = self.search.as_ref().and_then(|search| search.matches.get(search.current?));
        if let Some(&((row, _), _)) = current {
            self.pane.reveal(row, self.grid.0);
        }
    }

    /// A key while the search bar is open is the bar's, never the program's: Esc closes it, Enter
    /// and Shift+Enter go to the next match down or up (round the ends), Backspace takes a
    /// character back, Ctrl+R switches between text and regex. Text goes on to the input
    /// handler, which types it into the query (`replace_text_in_range`). In copy mode Enter
    /// hands the query to it, its cursor to the current match.
    fn search_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        let mods = keystroke.modifiers;
        let Some(search) = self.search.as_mut() else { return };
        match keystroke.key.as_str() {
            "escape" => self.search = None,
            "enter" if self.copy.is_some() => {
                search.accepted = true;
                let current = search.current.and_then(|index| search.matches.get(index));
                if let (Some(copy), Some(&(start, _))) = (self.copy.as_mut(), current) {
                    let mut selection = self.pane.selection;
                    copy.go(start, &self.pane.lock(), &mut selection);
                    self.pane.selection = selection;
                }
            }
            "enter" => {
                let count = search.matches.len();
                if count > 0 {
                    let step = if mods.shift { count - 1 } else { 1 };
                    search.current = Some(search.current.map_or(0, |index| (index + step) % count));
                    self.reveal_match();
                }
            }
            "backspace" => {
                search.query.text.pop();
                self.research(true);
            }
            "r" if mods.control => {
                search.query.regex = !search.query.regex;
                self.research(true);
            }
            _ if keystroke.key_char.is_some() && !mods.control && !mods.alt && !mods.platform => {
                return;
            }
            _ => {}
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Output arrived while the search bar is open: look again a moment later, once however
    /// much arrives meanwhile.
    fn refresh_search(&mut self, cx: &mut Context<Self>) {
        let Some(search) = self.search.as_mut().filter(|search| search.refresh.is_none()) else {
            return;
        };
        search.refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_REFRESH).await;
            let _ = this.update(cx, |view, cx| {
                if let Some(search) = view.search.as_mut() {
                    search.refresh = None;
                }
                view.research(false);
                cx.notify();
            });
        }));
    }

    /// Puts whether the selection is a block, copy mode's cursor in place of the terminal's, the
    /// link under the pointer, quick select's matches, and the search's matches in view and the
    /// current one, into `scene`.
    fn highlight(&self, scene: &mut Scene, top: u64, rows: u16) {
        scene.block = self.pane.selection.is_some_and(|selection| selection.kind() == Kind::Block);
        if let Some(copy) = &self.copy {
            (scene.cursor, scene.copy_cursor) = (None, Some(copy.cursor));
        }
        scene.link = self.link.as_ref().map(|(link, _)| (link.start, link.end));
        if let Some(quick) = &self.quick {
            scene.matches.extend(quick.items.iter().map(|item| item.cells));
        }
        let Some(search) = &self.search else { return };
        let bottom = top + u64::from(rows);
        let in_view = |found: &&Match| found.1.0 >= top && found.0.0 < bottom;
        scene.matches.extend(search.matches.iter().filter(in_view));
        scene.current_match = search.current.and_then(|index| search.matches.get(index).copied());
    }

    /// What the tab and the window call this pane (`title::title`).
    pub fn title(&self) -> &Title {
        &self.title
    }

    /// Looks again at what the pane should be called: the program's title, else its foreground
    /// process, else its directory. Whether that changed.
    pub fn refresh_title(&mut self) -> bool {
        let term = self.pane.lock();
        let (program, reported) = (term.title().to_owned(), term.cwd().map(PathBuf::from));
        drop(term);
        // A tutti pane has no process here: what tutti calls it comes after its program's own
        // title, and the directory its daemon saw after its shell's report.
        let (named, dir) = self.pane.tutti_name().unwrap_or_default();
        let program = if program.is_empty() { named } else { program };
        let reported = reported.or(dir);
        let foreground = self.pane.foreground();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let title =
            title::title(&program, reported.as_deref(), foreground.as_ref(), home.as_deref());
        mem::replace(&mut self.title, title) != self.title
    }

    /// The command line of what runs in the pane's foreground, unless that is its shell at a
    /// prompt: what closing the pane would end. Empty when its arguments cannot be read.
    pub fn running(&self) -> Option<String> {
        let foreground = self.pane.foreground().filter(|process| !process.shell)?;
        Some(title::command(&foreground.argv))
    }

    /// The grid, as rows and columns.
    pub fn size(&self) -> (u16, u16) {
        (self.grid.0, self.grid.1)
    }

    pub fn claims_keys(&self) -> bool {
        self.claims_keys
    }

    /// The server the pane's Neovim named, while `nvim` is the pane's foreground program and the
    /// server a socket of this user's (`editor::trusted`).
    pub fn nvim(&self) -> Option<PathBuf> {
        let server = self.nvim.clone()?;
        let program = self.pane.foreground()?.argv.into_iter().next()?;
        editor::trusted(&server, &program).then_some(server)
    }

    pub fn reads_password(&self) -> bool {
        self.pane.reads_password()
    }

    pub fn progress(&self) -> Option<Progress> {
        self.progress
    }

    fn set_progress(&mut self, progress: Option<Progress>, cx: &mut Context<Self>) {
        if mem::replace(&mut self.progress, progress) != progress {
            cx.emit(PaneEvent::Progress);
        }
    }

    /// The directory the shell last reported (OSC 7), else the foreground process's: most
    /// shells on macOS send OSC 7 only in Terminal.app.
    pub fn cwd(&self) -> Option<PathBuf> {
        let reported = self.pane.lock().cwd().map(PathBuf::from);
        reported.or_else(|| self.pane.cwd())
    }

    /// A chord bound to focus, resize, close or last pane: the program's when it claimed them and
    /// its encoding can say the chord (`keys::encode_chord`), else, as in copy mode, the window's
    /// to act on. A menu item whose action has no chord carries none, and is the window's.
    fn claimed_chord(&mut self, keystroke: &str, cx: &mut Context<Self>) {
        if keystroke.is_empty() {
            return cx.propagate();
        }
        let keystroke = Keystroke::parse(keystroke).expect("bound chords parse");
        let claimed = self.claims_keys && self.copy.is_none();
        let bytes = claimed.then(|| keys::encode_chord(&keystroke, &self.pane.lock()));
        match bytes.flatten() {
            Some(bytes) => {
                self.keys_down.push(keystroke.key);
                self.input(bytes, cx)
            }
            None => cx.propagate(),
        }
    }

    /// The OS switched between light and dark, or a reload changed the schemes: repaint with the
    /// other scheme (cells keep color indices, so what is on screen recolors) and tell the
    /// terminal, which answers color queries with it and tells programs that asked (mode 2031).
    /// Colors a program set itself with OSC 4/10/11/12 are the program's, not the scheme's, and
    /// stay.
    pub fn follow_appearance(&mut self, appearance: WindowAppearance, cx: &mut Context<Self>) {
        let scheme = cx.global::<Settings>().theme.for_appearance(appearance);
        if **scheme == *self.scheme {
            return;
        }
        self.scheme = scheme.clone();
        self.pane.set_scheme(&self.scheme);
        cx.notify();
    }

    fn notice(&mut self, notice: Notice, cx: &mut Context<Self>) {
        match notice {
            Notice::Output => {
                self.refresh_search(cx);
                // A password prompt prints as it turns echo off.
                if self.focused {
                    cx.defer(crate::app::refresh_secure_input);
                }
                cx.notify()
            }
            Notice::Event(Event::CursorStyle(_) | Event::Mode(_)) => cx.notify(),
            Notice::Event(Event::Title(_) | Event::Cwd(_)) => {
                if self.refresh_title() {
                    cx.emit(PaneEvent::Title)
                }
            }
            Notice::Event(Event::UserVar { name, value })
                if name == "RTTY_KEYS" || name == "TUTTI_KEYS" =>
            {
                self.claims_keys = value == "pane"
            }
            Notice::Event(Event::UserVar { name, value }) if name == "RTTY_NVIM" => {
                self.nvim = (!value.is_empty()).then(|| value.into())
            }
            // A mark a program sent while the alternate screen was up is not the shell's: no grid
            // row holds it, and it neither starts nor ends a command.
            Notice::Event(Event::Mark(mark)) if !prompt::held(&self.pane.lock(), &mark) => {}
            Notice::Event(Event::Mark(Mark { kind: MarkKind::PromptStart, .. })) => {
                (self.claims_keys, self.nvim) = (false, None);
                self.command = None;
                self.set_progress(None, cx)
            }
            Notice::Event(Event::Mark(mark @ Mark { kind: MarkKind::OutputStart, .. })) => {
                if self.command.is_none() {
                    let line = prompt::command_line(&self.pane.lock(), &mark);
                    self.command = Some((Instant::now(), line));
                }
            }
            // A command that ran `notify_after` or longer tells the window, which posts it as a
            // notification unless the pane has the keyboard in the active window.
            Notice::Event(Event::Mark(Mark { kind: MarkKind::CommandEnd { exit }, .. })) => {
                let Some((since, line)) = self.command.take() else { return };
                let took = since.elapsed();
                if cx.global::<Settings>().notify_after.is_none_or(|after| took < after) {
                    return;
                }
                // Called by its directory now that the shell is back, not by the command.
                if self.refresh_title() {
                    cx.emit(PaneEvent::Title)
                }
                let status =
                    exit.map_or("no exit status".into(), |code| format!("exit status {code}"));
                let took = took.as_secs_f64();
                let body = format!("{status} after {took:.0} s, in {}", self.title.text);
                let title = line.unwrap_or_else(|| "A command ended".into());
                cx.emit(PaneEvent::Notification { title: Some(title), body: Some(body) })
            }
            Notice::Event(Event::Progress { state, percent }) => {
                self.set_progress(progress::reported(state, percent), cx)
            }
            Notice::Event(Event::ProgramStatus(body)) => {
                if let Some(progress) = progress::status(&body) {
                    self.set_progress(progress, cx)
                }
            }
            Notice::Event(Event::Clipboard { text, .. }) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text))
            }
            Notice::Event(Event::ClipboardRead { selection, terminator }) => {
                cx.emit(PaneEvent::ClipboardRead { selection, terminator })
            }
            Notice::Event(Event::Bell) => {
                if cx.global::<Settings>().bell == Bell::Visual {
                    self.flashed = Some(Instant::now());
                    cx.notify();
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(FLASH).await;
                        this.update(cx, |_, cx| cx.notify()).ok();
                    })
                    .detach();
                    cx.emit(PaneEvent::Bell);
                }
            }
            Notice::Event(Event::Notification { title, body }) => {
                cx.emit(PaneEvent::Notification { title, body })
            }
            Notice::Event(_) => {}
            Notice::Exit(status) => {
                if let Some(stats) = self.stats.as_mut() {
                    stats.report();
                }
                cx.emit(PaneEvent::Exited(status))
            }
        }
    }

    fn input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.pane.input(bytes);
        // Typing shows the cursor and starts its blinking over.
        self.blink_since = None;
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if self.search.as_ref().is_some_and(|search| !search.accepted) {
            return self.search_key(keystroke, cx);
        }
        if self.copy.is_some() {
            return self.copy_key(keystroke, cx);
        }
        let kind = if event.is_held { input::Kind::Repeat } else { input::Kind::Press };
        // A held key repeats here: the input handler would open the accent picker instead.
        let bytes = keys::encode(keystroke, &self.pane.lock(), kind).or_else(|| {
            event.is_held.then(|| keystroke.key_char.clone()).flatten().map(String::into_bytes)
        });
        if bytes.is_some() || (keystroke.modifiers.control && !keystroke.modifiers.platform) {
            cx.stop_propagation();
        }
        if let Some(bytes) = bytes {
            self.keys_down.push(keystroke.key.clone());
            self.input(bytes, cx);
        }
    }

    /// Releases matter only to a program that asked for kitty's event types, and only for keys
    /// whose press it got: a chord that moved the focus here is released here too.
    pub fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if !self.keys_down.contains(&event.keystroke.key) {
            return;
        }
        self.keys_down.retain(|key| *key != event.keystroke.key);
        let bytes = keys::encode(&event.keystroke, &self.pane.lock(), input::Kind::Release);
        if let Some(bytes) = bytes {
            self.pane.write(bytes);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.pane.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Cmd+K, as Terminal.app's: the scrollback and the screen go, and the shell hears Ctrl+L to
    /// draw its prompt again at the top. A full-screen program keeps its screen and hears nothing.
    /// Copy mode ends: the rows its cursor was on are gone.
    fn clear_scrollback(&mut self, _: &ClearScrollback, _: &mut Window, cx: &mut Context<Self>) {
        self.leave_copy();
        if !self.pane.clear() {
            self.input(b"\x0c".to_vec(), cx);
        }
        self.research(false);
        cx.notify();
    }

    /// Copies the last command's output (`prompt::last_output`), and says so over the pane for a
    /// moment.
    fn copy_last_output(&mut self, _: &CopyLastOutput, _: &mut Window, cx: &mut Context<Self>) {
        let output = prompt::last_output(&self.pane.lock());
        let status = match output {
            None => "no command output to copy",
            Some(text) if text.is_empty() => "the last command printed nothing",
            Some(text) => {
                self.copy_text(text, cx);
                "copied the last command's output"
            }
        };
        self.say(status, cx);
    }

    /// Copies `text` (under `--script`, only into the dump).
    fn copy_text(&mut self, text: String, cx: &mut Context<Self>) {
        if cx.global::<Settings>().launch.clipboard {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        }
        self.copied = Some(text);
    }

    /// Says `status` at the pane's bottom right for a moment.
    fn say(&mut self, status: &'static str, cx: &mut Context<Self>) {
        self.status = Some((status, Instant::now()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(STATUS).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }

    /// Quick select, or quick open: labels what is worth copying on screen (`quick::found`), or
    /// the links there that a Cmd+click would follow, resolved now, until a label or any other
    /// key is typed (`quick_key`).
    fn quick(&mut self, open: bool, cx: &mut Context<Self>) {
        let term = self.pane.lock();
        let (top, (row, col)) = (self.pane.top(&term), term.cursor_position());
        let patterns = &cx.global::<Settings>().quick_select;
        let found = quick::found(&term, top, self.grid.0, patterns, open).into_iter();
        let link = |cells: Match| links::link_at(&term, cells.0, || self.pane.cwd());
        let picks = found.filter_map(|(cells, text)| match open {
            true => Some((cells, Pick::Open(link(cells)?.target))),
            false => Some((cells, Pick::Copy(text))),
        });
        let items = quick::labeled(picks.collect(), (term.screen_top() + u64::from(row), col));
        drop(term);
        if items.is_empty() {
            return self.say(if open { "nothing to open" } else { "nothing to select" }, cx);
        }
        self.quick = Some(Quick { items, typed: String::new(), shifted: false });
        cx.notify();
    }

    /// Every key while quick select's labels show in this pane is theirs, never a binding's or
    /// the program's: the letters of a label pick it (`pick`), and Esc or any other key puts the
    /// labels away.
    fn quick_key(&mut self, keystroke: &Keystroke, window: &Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            return;
        }
        let Some(mut quick) = self.quick.take() else { return };
        cx.stop_propagation();
        cx.notify();
        let mods = keystroke.modifiers;
        let letter = keystroke.key.chars().count() == 1;
        if !letter || mods.control || mods.alt || mods.platform || mods.function {
            return;
        }
        quick.typed.push_str(&keystroke.key);
        quick.shifted |= mods.shift;
        if let Some(item) = quick.items.iter().find(|item| item.label == quick.typed) {
            return self.pick(item.picks.clone(), quick.shifted, cx);
        }
        if quick.items.iter().any(|item| item.label.starts_with(&quick.typed)) {
            self.quick = Some(quick);
        }
    }

    /// A label typed: quick open follows its link as a Cmd+click would; quick select copies its
    /// text, and with Shift pastes it too (bracketed when the program asked for that).
    fn pick(&mut self, picks: Pick, shifted: bool, cx: &mut Context<Self>) {
        match picks {
            Pick::Open(target) => self.follow(target, cx),
            Pick::Copy(text) => {
                if shifted {
                    self.paste_text(&text, cx);
                }
                self.copy_text(text, cx);
                self.say(if shifted { "copied and pasted" } else { "copied" }, cx);
            }
        }
    }

    /// `copy-mode`, on or off: a cursor at the terminal's, or on the bottom row of a view scrolled
    /// back from it, that keys move through the screen and the scrollback, selecting
    /// (`copy_key`). A selection made before goes.
    fn copy_mode(&mut self, cx: &mut Context<Self>) {
        if self.copy.is_some() {
            self.leave_copy();
            return cx.notify();
        }
        let term = self.pane.lock();
        let (row, col) = term.cursor_position();
        let cursor = term.screen_top() + u64::from(row);
        let bottom = (self.pane.top(&term) + u64::from(self.grid.0)).saturating_sub(1);
        drop(term);
        self.pane.selection = None;
        let at = if cursor <= bottom { (cursor, col) } else { (bottom, 0) };
        self.copy = Some(copy::Mode::new(at));
        cx.notify();
    }

    /// A key in copy mode is copy mode's, never the program's (`copy::Mode::key`): the view
    /// scrolls as it says, the search's current match is the one the cursor lands on, and `y`
    /// copies (under `--script`, only into the dump).
    fn copy_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        cx.stop_propagation();
        cx.notify();
        let Some(copy) = self.copy.as_mut() else { return };
        let mut selection = self.pane.selection;
        let term = self.pane.lock();
        let before = self.pane.top(&term);
        let mut top = before;
        let matches = self.search.as_ref().map_or(&[][..], |search| &search.matches);
        let effect = copy.key(keystroke, &term, &mut top, self.grid.0, &mut selection, matches);
        let cursor = copy.cursor;
        drop(term);
        self.pane.selection = selection;
        self.pane.scroll((before as i64 - top as i64) as i32);
        if let Some(search) = self.search.as_mut()
            && let Some(index) = search.matches.iter().position(|&(start, _)| start == cursor)
        {
            search.current = Some(index);
        }
        match effect {
            Effect::Stay => {}
            Effect::Search => self.search = Some(Search::default()),
            Effect::Yank(text) => {
                self.copy_text(text, cx);
                self.say("copied", cx);
                self.leave_copy();
            }
            Effect::Leave => self.leave_copy(),
        }
    }

    /// Leaves copy mode: its selection goes, and a search it took the query of.
    fn leave_copy(&mut self) {
        (self.copy, self.pane.selection) = (None, None);
        if self.search.as_ref().is_some_and(|search| search.accepted) {
            self.search = None;
        }
    }

    /// Answers the program's OSC 52 read with the clipboard's text (`Launch::clipboard`).
    pub fn answer_clipboard(&self, selection: &str, terminator: &str, cx: &App) {
        let text = match cx.global::<Settings>().launch.clipboard {
            true => cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default(),
            false => STAND_IN.into(),
        };
        self.pane.write(keys::clipboard(selection, &text, terminator));
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.paste_text(&text, cx);
        }
    }

    /// Text pasted or dropped: its first line into the query while the search bar takes the
    /// keyboard, nowhere in copy mode, else to the program, bracketed when it asked for
    /// bracketed paste.
    fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(search) = self.search.as_mut().filter(|search| !search.accepted) {
            search.query.text.push_str(text.lines().next().unwrap_or_default());
            self.research(true);
            return cx.notify();
        }
        if self.copy.is_some() {
            return;
        }
        let bracketed = self.pane.lock().modes().bracketed_paste;
        self.input(keys::paste(text, bracketed), cx);
    }

    /// Files dropped on the pane, from Finder or `--script`'s `drop`: the pane takes the keyboard
    /// and their paths are pasted, escaped for a shell (`keys::dropped`).
    pub fn drop_paths(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        cx.emit(PaneEvent::Focused);
        match keys::dropped(paths) {
            Some(text) => self.paste_text(&text, cx),
            None => crate::note("a drop typed nothing: a path held a control character"),
        }
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
    pub fn dump(&self, window: &Window, cx: &App) -> String {
        let settings = cx.global::<Settings>();
        let (font, minimum_contrast) = (settings.font.clone(), settings.minimum_contrast);
        let text_system = window.text_system();
        let mut out = String::new();
        let fallbacks = font.fallbacks.iter().flat_map(|fallbacks| fallbacks.fallback_list());
        let fallbacks: Vec<String> = fallbacks
            .map(|family| {
                format!("{family:?} {:?}", text_system.resolve_font(&gpui::font(family.clone())))
            })
            .collect();
        let primary = text_system.resolve_font(&font);
        let _ = writeln!(
            out,
            "font {:?} {primary:?} size {} {:?} fallbacks {fallbacks:?}",
            font.family,
            f32::from(settings.font_size),
            font.features
        );
        // The cell, and where text sits in it: centered, whatever `line_height` adds.
        let Metrics { cell, ascent, descent } = metrics(text_system, settings);
        let text = (cell.height - ascent - descent) / 2.;
        let _ = writeln!(
            out,
            "cell {:.2}x{:.2} line_height {} text {:.2}..{:.2}",
            f32::from(cell.width),
            f32::from(cell.height),
            settings.line_height,
            f32::from(text),
            f32::from(text + ascent + descent)
        );
        // A drawn character's shapes in the device pixels `paint_drawn` gives them.
        let scale = window.scale_factor();
        let (width, height) = (cell.width * scale, cell.height * scale);
        let (width, height) = (f32::from(width).round(), f32::from(height).round());
        let thickness = self.cache.last.as_ref().map_or(px(1.), |frame| frame.thickness);
        let light = (f32::from(thickness) * scale).round().max(1.);
        let drawn_rows = |c: char| {
            let ys =
                glyphs::shapes(c, width, height, light).into_iter().flat_map(|shape| match shape {
                    Shape::Rect { y, h, .. } => vec![y, y + h],
                    Shape::Fill(segs) | Shape::Stroke(_, segs) => segs
                        .into_iter()
                        .map(|seg| match seg {
                            Seg::Move(_, y) | Seg::Line(_, y) | Seg::Curve { to: (_, y), .. } => y,
                        })
                        .collect(),
                });
            ys.fold((height, 0_f32), |(top, bottom), y| (top.min(y), bottom.max(y)))
        };
        let term = self.pane.lock();
        let palette = Palette::new(&term, &self.scheme, minimum_contrast);
        let (rows, cols) = term.size();
        let top = self.pane.top(&term);
        let selection = self.pane.selection.and_then(|selection| selection.range(&term));
        let focused = self.focus.is_focused(window) && window.is_window_active();
        let mut scene = Scene::new(&term, &palette, top, rows, selection, focused);
        self.highlight(&mut scene, top, rows);
        let hex = |Rgb(r, g, b): Rgb| format!("#{r:02x}{g:02x}{b:02x}");
        let (screen_top, oldest) = (term.screen_top(), term.oldest_row());
        let _ =
            writeln!(out, "size {rows}x{cols} top {top} screen_top {screen_top} oldest {oldest}");
        if let Some(at) = self.flashed {
            let _ = writeln!(out, "bell flashed {} ms ago", at.elapsed().as_millis());
        }
        if let Some(progress) = self.progress {
            let _ = writeln!(out, "progress {progress:?}");
        }
        let marks = prompt::marks(&term).into_iter();
        let marks: Vec<String> =
            marks.map(|mark| format!("{:?} {},{}", mark.kind, mark.row, mark.col)).collect();
        if !marks.is_empty() {
            let _ = writeln!(out, "marks {}", marks.join(", "));
        }
        if let Some((since, line)) = &self.command {
            let _ = writeln!(out, "command {line:?} running {} ms", since.elapsed().as_millis());
        }
        if let Some((text, at)) = self.status {
            let _ = writeln!(out, "status {text:?} {} ms ago", at.elapsed().as_millis());
        }
        if let Some(copied) = &self.copied {
            let _ = writeln!(out, "copied {copied:?}");
        }
        if self.link.is_some() || !self.followed.is_empty() {
            let _ = writeln!(out, "link {:?} followed {:?}", self.link, self.followed);
        }
        if let Some(quick) = &self.quick {
            let items = quick.items.iter().map(|item| {
                let ((row, col), _) = item.cells;
                format!("{} at {},{col} {:?}", item.label, row as i64 - top as i64, item.picks)
            });
            let items: Vec<String> = items.collect();
            let _ = writeln!(out, "quick typed {:?}: {}", quick.typed, items.join(", "));
        }
        if let Some(copy) = &self.copy {
            let (row, col) = copy.cursor;
            let row = row as i64 - top as i64;
            let _ = writeln!(out, "copy mode at {row},{col} pending {:?}", copy.pending());
        }
        if let Some(search) = &self.search {
            let current = search.current.and_then(|index| search.matches.get(index));
            let _ = writeln!(
                out,
                "search {:?} regex {} matches {} current {:?} {current:?} error {:?}",
                search.query.text,
                search.query.regex,
                search.matches.len(),
                search.current,
                search.error,
            );
        }
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
        if let Some(text) = &self.marked_text {
            let _ = writeln!(out, "composing {text:?}");
        }
        for (index, row) in term.rows(top, top + u64::from(rows)) {
            let line = scene.line(row, index);
            if line.backgrounds.is_empty() && line.runs.is_empty() && line.drawn.is_empty() {
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
            // As the last frame shaped the row: each glyph, the cell it sits on, the font it came
            // from when that is not the run's own, and the size it is drawn at when smaller.
            if let (Some(shaped), Some(grid)) = (self.cache.rows.get(&index), self.geometry) {
                for (run, shaped) in shaped.line.runs.iter().zip(&shaped.shaped) {
                    let own =
                        text_system.resolve_font(&styled(&font, run.style.bold, run.style.italic));
                    let glyphs = shaped.line.runs.iter().flat_map(|glyphs| {
                        let from = match glyphs.font_id {
                            id if id == own => String::new(),
                            id => format!(" from {id:?}"),
                        };
                        glyphs.glyphs.iter().map(move |glyph| (glyph, from.clone()))
                    });
                    let glyphs: Vec<String> = glyphs
                        .enumerate()
                        .map(|(index, (glyph, from))| {
                            let at = glyph.position.x / grid.cell.width;
                            let fit = shaped.fits.as_ref().and_then(|fits| fits[index]);
                            let fit = fit.map_or(String::new(), |fit| {
                                let (x, y) = (fit.offset.x / grid.cell.width, fit.offset.y);
                                format!(" fitted {:.2} moved {x:.2},{:.1}", fit.scale, f32::from(y))
                            });
                            format!("{:?}@{at:.2}{from}{fit}", glyph.id)
                        })
                        .collect();
                    let _ = writeln!(out, "  glyphs {} {}", run.col, glyphs.join(" "));
                }
            }
            for drawn in &line.drawn {
                let (top, bottom) = drawn_rows(drawn.c);
                let _ = writeln!(
                    out,
                    "  drawn {} {:?} {} device rows {top}..{bottom} of {height}",
                    drawn.col,
                    drawn.c,
                    hex(drawn.fg)
                );
            }
        }
        // The images the next frame draws, lowest first, from the cell each counts from; and how
        // many the last frame drew from how many GPUI images.
        if !term.images().is_empty() {
            let held: Vec<u32> = term.images().iter().map(|image| image.id).collect();
            let drawn = self.cache.last.as_ref().map_or(0, |frame| frame.pictures.len());
            let _ = writeln!(
                out,
                "images held {held:?}, the last frame drew {drawn} from {} GPUI images",
                self.images.len()
            );
        }
        for placement in term.placements(top, top + u64::from(rows)) {
            let image = &placement.image;
            let _ = writeln!(
                out,
                "image {} {}x{} at {},{} dest {:?} source {:?} z {}",
                image.id,
                image.width,
                image.height,
                placement.row as i64 - top as i64,
                placement.col,
                placement.dest,
                placement.source,
                placement.z
            );
        }
        if let (Some((start, end)), Some(selected)) = (selection, self.pane.selection) {
            let (kind, text) = (selected.kind(), selected.text(&term).unwrap_or_default());
            let _ = writeln!(out, "selected {kind:?} {start:?}..={end:?} {text:?}");
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
        let pixel = pixel_offset(position - grid.origin, grid.cell, self.grid.2);
        let mouse = Mouse { button, action, mods: keys::mods(&modifiers), cell, pixel };
        if let Some(bytes) = input::encode_mouse(&mouse, &modes) {
            self.pane.write(bytes);
        }
        true
    }

    /// A press over this pane: it takes the keyboard, and the press follows a link (Cmd), goes to
    /// a program that asked for mouse reports, or starts a selection, a block with Option (as in
    /// Terminal.app). A plain left click in a pane that already had the keyboard, out of copy
    /// mode, may move the shell's cursor too, when it comes up where it went down (`mouse_up`).
    pub fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let had_keyboard = self.focus.is_focused(window) && !event.first_mouse;
        window.focus(&self.focus);
        cx.emit(PaneEvent::Focused);
        let cmd_click = event.modifiers.platform && event.button == MouseButton::Left;
        if cmd_click && self.open_link(event.position, cx) {
            return;
        }
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
            _ if event.modifiers.alt => Kind::Block,
            _ => Kind::Cells,
        };
        self.pane.selection = Some(Selection::new(kind, point, side));
        self.selecting = Some(event.position);
        let plain = event.click_count == 1 && !event.modifiers.modified() && self.copy.is_none();
        let moves = had_keyboard && plain && cx.global::<Settings>().click_to_move;
        self.click = moves.then_some(point);
        cx.notify();
    }

    /// A release over this pane, or anywhere after a press in it (the program hears it at the
    /// nearest cell); a release that ends another pane's press is not this pane's. A plain click
    /// on the command line being typed moves the shell's cursor to it with arrow keys, as many as
    /// `prompt::cursor_moves` counts.
    pub fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.button == MouseButton::Left {
            (self.selecting, self.autoscroll) = (None, None);
            let up = self.grid_point(event.position).map(|(point, _)| point);
            let moves = self.click.take().filter(|&down| up == Some(down)).and_then(|down| {
                prompt::cursor_moves(&self.pane.lock(), down).filter(|&moves| moves != 0)
            });
            if let Some(moves) = moves {
                let arrow = Keystroke::parse(if moves < 0 { "left" } else { "right" });
                let arrow =
                    keys::encode(&arrow.expect("an arrow"), &self.pane.lock(), input::Kind::Press);
                self.input(arrow.expect("arrows encode").repeat(moves.unsigned_abs()), cx);
            }
        }
        if mem::take(&mut self.pressed) {
            let button = mouse_button(event.button);
            self.report_mouse(button, Action::Release, event.position, event.modifiers);
        }
    }

    /// Cmd held over a link underlines it, ready for Cmd+click, and shows its target when its
    /// text is not that.
    fn hover(&mut self, cmd: bool, cx: &mut Context<Self>) {
        let point = self.pointer.filter(|_| cmd).and_then(|position| self.grid_point(position));
        let link = point.and_then(|(point, _)| self.link_at(point));
        let term = self.pane.lock();
        let link = link.map(|link| {
            let disguise = links::disguise(&term, &link);
            (link, disguise)
        });
        drop(term);
        if link != self.link {
            self.link = link;
            cx.notify();
        }
    }

    /// The link on a cell (`links::link_at`), a relative path found from the shell's directory,
    /// else the foreground process's.
    fn link_at(&self, point: GridPoint) -> Option<Link> {
        links::link_at(&self.pane.lock(), point, || self.pane.cwd())
    }

    /// Cmd+click on a link. Whether there was a link.
    fn open_link(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(link) = self.grid_point(position).and_then(|(point, _)| self.link_at(point))
        else {
            return false;
        };
        self.follow(link.target, cx);
        true
    }

    /// Follows a link: http, https and mailto open with the system's handler, a `file:` URL
    /// shows in Finder (never opened: that would run an app or a script), any other scheme is
    /// ignored; a file is edited, opened or shown in Finder as `links::opening` says, the window
    /// finding the editor (`PaneEvent::Edit`). Under `--script` only the edit goes ahead; the
    /// rest is recorded.
    fn follow(&mut self, target: Target, cx: &mut Context<Self>) {
        let act = cx.global::<Settings>().launch.open_links;
        let followed = match target {
            Target::Uri(uri) => {
                let follow = links::follow(
                    &uri,
                    |uri| {
                        if act {
                            cx.open_url(uri)
                        }
                    },
                    |uri| {
                        if act {
                            reveal_in_finder(uri)
                        }
                    },
                );
                format!("{follow:?} {uri}")
            }
            Target::File(at) => {
                let opening = links::opening(&at.path);
                let place = format!("{opening:?} {}:{}:{}", at.path.display(), at.line, at.col);
                match opening {
                    Opening::Edit => cx.emit(PaneEvent::Edit(at)),
                    Opening::Open if act => cx.open_with_system(&at.path),
                    Opening::Reveal if act => cx.reveal_path(&at.path),
                    Opening::Open | Opening::Reveal => {}
                }
                place
            }
        };
        self.followed.push(followed);
    }

    /// Motion over this pane, with the button held only when it went down here.
    pub fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.pointer = Some(event.position);
        self.hover(event.modifiers.platform, cx);
        let pressed = event.pressed_button.filter(|_| self.pressed);
        let button = pressed.map_or(Button::None, mouse_button);
        self.report_mouse(button, Action::Motion, event.position, event.modifiers);
    }

    /// Motion anywhere in the window, over this pane or not: while a drag selects, the selection
    /// follows the pointer to the pane's nearest cell, and past the pane's top or bottom the view
    /// scrolls that way until the pointer comes back or the button comes up.
    pub fn drag_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting.is_none() {
            return;
        }
        self.selecting = Some(event.position);
        self.extend_selection(cx);
        let past = self.geometry.is_some_and(|grid| beyond(event.position.y, grid) != 0.);
        if !past {
            self.autoscroll = None;
        } else if self.autoscroll.is_none() {
            self.autoscroll = Some(cx.spawn(async move |this, cx| {
                let mut last = Instant::now();
                loop {
                    cx.background_executor().timer(AUTOSCROLL_TICK).await;
                    let elapsed = mem::replace(&mut last, Instant::now()).elapsed();
                    if this.update(cx, |view, cx| view.autoscroll(elapsed, cx)).is_err() {
                        break;
                    }
                }
            }));
        }
    }

    /// A step of a selecting drag held past the pane's top or bottom: the view scrolls toward the
    /// pointer for the time since the last step, and the selection follows to the edge row.
    fn autoscroll(&mut self, elapsed: Duration, cx: &mut Context<Self>) {
        let (Some(pointer), Some(grid)) = (self.selecting, self.geometry) else { return };
        self.scroll_remainder +=
            grid.cell.height * autoscroll_lines(beyond(pointer.y, grid), elapsed);
        let lines = (self.scroll_remainder / grid.cell.height) as i32;
        self.scroll_remainder -= grid.cell.height * lines as f32;
        self.pane.scroll(lines);
        self.extend_selection(cx);
    }

    /// The selection, to the pane's cell nearest the dragging pointer.
    fn extend_selection(&mut self, cx: &mut Context<Self>) {
        let Some((point, side)) = self.selecting.and_then(|pointer| self.grid_point(pointer))
        else {
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
            let arrow = Keystroke::parse(if lines > 0 { "up" } else { "down" });
            let arrow =
                keys::encode(&arrow.expect("an arrow"), &self.pane.lock(), input::Kind::Press);
            self.pane.write(arrow.expect("arrows encode").repeat(count));
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
        let minimum_contrast = settings.minimum_contrast;
        let text_system = window.text_system().clone();
        let Metrics { cell, ascent, descent } = metrics(&text_system, settings);
        let focused = self.focus.is_focused(window) && window.is_window_active();

        let mut term = self.pane.lock();
        // A tutti pane's terminal has the size its daemon settled on, which another client showing
        // the pane may hold smaller than the cells it has here, or a resize not yet reached.
        let (rows, cols) = (self.grid.0.min(term.size().0), self.grid.1.min(term.size().1));
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
        let palette = Palette::new(&term, &self.scheme, minimum_contrast);
        let top = self.pane.top(&term);
        let selection = self.pane.selection.and_then(|selection| selection.range(&term));
        let mut scene = Scene::new(&term, &palette, top, rows, selection, focused);
        self.highlight(&mut scene, top, rows);
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
            selection: (selection, scene.block),
            matches: (scene.matches.clone(), scene.current_match),
            link: scene.link,
            focused,
            size: (rows, cols),
            font: font.clone(),
            font_size,
            cell,
        };
        if cache.key.as_ref() != Some(&key) {
            cache.rows.clear();
            cache.key = Some(key);
        }
        let cursor = scene.cursor.map(|cursor| cursor.row).or(scene.copy_cursor.map(|at| at.0));
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
        let placements = term.placements(top, in_view.end);
        let held: HashSet<u64> = term.images().iter().map(|image| image.generation).collect();
        drop(term);
        for (_, gone) in self.images.extract_if(|generation, _| !held.contains(generation)) {
            window.drop_image(gone).ok();
        }

        for (index, line) in missing {
            let shaped = line
                .runs
                .iter()
                .map(|run| shape(&text_system, &font, font_size, cell, run))
                .collect();
            cache.rows.insert(index, Rc::new(ShapedRow { line, shaped }));
        }
        let at = |row: u16, col: u16| {
            point(
                bounds.origin.x + cell.width * f32::from(col),
                bounds.origin.y + cell.height * f32::from(row),
            )
        };
        // Each drawn whole, scaled and moved so that its source lands on its cells, and clipped
        // to them; a placement's row may be above the view.
        let pictures = placements
            .iter()
            .filter_map(|placement| {
                let image = &placement.image;
                let ([x, y, w, h], [sx, sy, sw, sh]) = (placement.dest, placement.source);
                let row = (placement.row as i64 - top as i64) as f32;
                let corner = bounds.origin
                    + point(cell.width * (f32::from(placement.col) + x), cell.height * (row + y));
                let clip = Bounds::new(corner, size(cell.width * w, cell.height * h));
                let clip = clip.intersect(&bounds);
                let (across, down) = (cell.width * (w / sw), cell.height * (h / sh));
                let whole = Bounds::new(
                    corner - point(across * sx, down * sy),
                    size(across * image.width as f32, down * image.height as f32),
                );
                let rendered =
                    self.images.entry(image.generation).or_insert_with(|| render_image(image));
                let image = rendered.clone();
                (!clip.is_empty()).then_some(Picture { image, bounds: whole, clip, z: placement.z })
            })
            .collect();
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
        let thickness = px((f32::from(font_size) / 14.).round().max(1.));
        let (underline, strike) = decorations(cell.height, ascent, descent, thickness);
        let frame = Frame {
            bounds,
            cell,
            underline,
            strike,
            thickness,
            background: hsla(scene.bg),
            rows: (0..rows)
                .filter_map(|row| Some((row, cache.rows.get(&(top + u64::from(row)))?.clone())))
                .collect(),
            pictures,
            cursor,
            marked_text,
            flash: self
                .flashed
                .filter(|at| at.elapsed() < FLASH)
                .map(|_| hsla(scene.fg).opacity(0.2)),
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
        // A drag goes on past the pane's edges, where the pane's own listeners hear nothing.
        let view = cx.weak_entity();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let _ = view.update(cx, |view, cx| view.drag_move(event, window, cx));
            }
        });
        let cell = frame.cell;
        let at = |row: u16, col: u16| {
            point(
                frame.bounds.origin.x + cell.width * f32::from(col),
                frame.bounds.origin.y + cell.height * f32::from(row),
            )
        };
        // Images by z: under the cells' own backgrounds, under the text, over it.
        let pictures = |window: &mut Window, layer: fn(i32) -> bool| {
            for picture in frame.pictures.iter().filter(|picture| layer(picture.z)) {
                let mask = ContentMask { bounds: picture.clip };
                let painted = window.with_content_mask(Some(mask), |window| {
                    let image = picture.image.clone();
                    window.paint_image(picture.bounds, Corners::default(), image, 0, false)
                });
                if let Err(err) = painted {
                    eprintln!("rtty: painting an image failed: {err}");
                }
            }
        };
        window.paint_quad(fill(bounds, frame.background));
        pictures(window, |z| z < i32::MIN / 2);
        for (row, shaped) in &frame.rows {
            for span in &shaped.line.backgrounds {
                let width = cell.width * f32::from(span.cells);
                window.paint_quad(fill(
                    Bounds::new(at(*row, span.col), size(width, cell.height)),
                    hsla(span.color),
                ));
            }
        }
        pictures(window, |z| (i32::MIN / 2..0).contains(&z));
        let scale = window.scale_factor();
        let light = (f32::from(frame.thickness) * scale).round().max(1.);
        for (row, shaped) in &frame.rows {
            for (run, shaped) in shaped.line.runs.iter().zip(&shaped.shaped) {
                let origin = at(*row, run.col);
                let line = &shaped.line;
                let painted = match &shaped.fits {
                    None => line.paint(origin, cell.height, window, cx),
                    Some(fits) => {
                        let color = hsla(run.style.fg);
                        paint_fitted(line, fits, origin, cell.height, color, window)
                    }
                };
                if let Err(err) = painted {
                    eprintln!("rtty: painting {:?} failed: {err}", line.text);
                }
                decorate(run, origin, &frame, window);
            }
            for drawn in &shaped.line.drawn {
                paint_drawn(drawn, at(*row, drawn.col), cell, scale, light, window);
            }
        }
        pictures(window, |z| z >= 0);
        if let Some(cursor) = frame.cursor {
            window.paint_quad(cursor);
        }
        if let Some((backdrop, origin, line)) = frame.marked_text {
            window.paint_quad(backdrop);
            line.paint(origin, cell.height, window, cx).ok();
        }
        if let Some(flash) = frame.flash {
            window.paint_quad(fill(bounds, flash));
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
        // Two lines and the wave reach a thickness further down, still inside the cell.
        Underline::Double => {
            let y = y.min(cell.height - thick * 2.);
            line(window, y - thick, underline);
            line(window, y + thick, underline);
        }
        Underline::Curly => {
            let y = y.min(cell.height - thick * 2.);
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

/// A character rtty draws itself (`glyphs`), over its cell with the cell's edges on device
/// pixels, so that its lines meet the next cell's.
fn paint_drawn(
    drawn: &Drawn,
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    scale: f32,
    light: f32,
    window: &mut Window,
) {
    let device = |at: Pixels| (f32::from(at) * scale).round();
    let (left, top) = (device(origin.x), device(origin.y));
    let (width, height) =
        (device(origin.x + cell.width) - left, device(origin.y + cell.height) - top);
    let at = |(x, y): (f32, f32)| point(px((left + x) / scale), px((top + y) / scale));
    let color = hsla(drawn.fg);
    for shape in glyphs::shapes(drawn.c, width, height, light) {
        let (mut path, segs, closed) = match shape {
            Shape::Rect { x, y, w, h, alpha } => {
                let bounds = Bounds::new(at((x, y)), size(px(w / scale), px(h / scale)));
                window.paint_quad(fill(bounds, color.opacity(alpha)));
                continue;
            }
            Shape::Fill(segs) => (PathBuilder::fill(), segs, true),
            Shape::Stroke(thick, segs) => (PathBuilder::stroke(px(thick / scale)), segs, false),
        };
        for seg in segs {
            match seg {
                Seg::Move(x, y) => path.move_to(at((x, y))),
                Seg::Line(x, y) => path.line_to(at((x, y))),
                Seg::Curve { to, c1, c2 } => path.cubic_bezier_to(at(to), at(c1), at(c2)),
            }
        }
        if closed {
            path.close();
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }
}

/// How many cells `y` is past the pane's top (negative) or bottom (positive); 0 within it.
fn beyond(y: Pixels, grid: Geometry) -> f32 {
    let top = grid.origin.y;
    let bottom = top + grid.cell.height * f32::from(grid.rows);
    match y {
        y if y < top => (y - top) / grid.cell.height,
        y if y > bottom => (y - bottom) / grid.cell.height,
        _ => 0.,
    }
}

/// How many lines up (negative: down) a selecting drag held `beyond` cells past the pane's edge
/// scrolls the view in `elapsed`: toward the pointer, faster the further out it is.
fn autoscroll_lines(beyond: f32, elapsed: Duration) -> f32 {
    if beyond == 0. {
        return 0.;
    }
    -beyond.signum() * AUTOSCROLL_SPEED * (1. + beyond.abs()) * elapsed.as_secs_f32()
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

/// The cell is the font's advance of `m` wide and `line_height` times its ascent plus descent
/// tall, whole pixels; text is centered in it, and drawn characters fill it.
pub fn metrics(text_system: &WindowTextSystem, settings: &Settings) -> Metrics {
    let (font_size, line_height) = (settings.font_size, settings.line_height);
    let font_id = text_system.resolve_font(&settings.font);
    let width =
        text_system.advance(font_id, font_size, 'm').expect("the terminal font has an 'm'").width;
    let ascent = text_system.ascent(font_id, font_size);
    let descent = text_system.descent(font_id, font_size).abs();
    Metrics { cell: size(width, ((ascent + descent) * line_height).ceil()), ascent, descent }
}

/// The run shaped as one string, every glyph on its cell and kept to it.
fn shape(
    text_system: &WindowTextSystem,
    font: &Font,
    font_size: Pixels,
    cell: Size<Pixels>,
    run: &Run,
) -> Shaped {
    let style = &run.style;
    let text_run = text_run(font, run.text.len(), style.fg, style.bold, style.italic);
    let own = text_system.resolve_font(&text_run.font);
    let mut line = text_system.shape_line(run.text.clone().into(), font_size, &[text_run], None);
    let fits = fit(text_system, own, &line, run, cell);
    if run.single_width
        && let Some(snapped) = snap_to_cells(&line, &line.text, cell.width)
    {
        *line = Arc::new(snapped);
    }
    Shaped { line, fits }
}

/// Where and how big to draw each glyph whose ink would not keep to the cells its characters
/// take (a wide character's two) and the line: one from another font, or an icon from the
/// private use areas, which a Nerd Font that is not its Mono variant draws wider than its cell.
/// Followed by a blank cell, as prompts put one after each icon, it takes that cell too, as
/// WezTerm and Ghostty let it; else it is scaled down to its own cells. Either way it is
/// centered there. The font's own glyphs stay as drawn, so a ligature still reaches over the
/// cells after it. `None` when nothing moves.
fn fit(
    text_system: &WindowTextSystem,
    own: FontId,
    layout: &LineLayout,
    run: &Run,
    cell: Size<Pixels>,
) -> Option<Vec<Option<Fit>>> {
    let text = &run.text;
    let glyphs: Vec<(FontId, &ShapedGlyph)> = layout
        .runs
        .iter()
        .flat_map(|shaped| shaped.glyphs.iter().map(move |glyph| (shaped.font_id, glyph)))
        .collect();
    let next = glyphs.iter().skip(1).map(|(_, glyph)| (glyph.index, glyph.position.x));
    let next = next.chain(iter::once((text.len(), layout.width)));
    let baseline = (cell.height - layout.ascent - layout.descent) / 2. + layout.ascent;
    let fits: Vec<Option<Fit>> = glyphs
        .iter()
        .zip(next)
        .map(|(&(font_id, glyph), (end, next_x))| {
            let c = text.get(glyph.index..)?.chars().next()?;
            let icon = matches!(c, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{10FFFF}');
            // A mark over the character before it has no advance, and stays.
            if !(font_id != own || icon) || next_x <= glyph.position.x {
                return None;
            }
            let (cells, blank_after) = match run.single_width {
                true => {
                    let cells = text.get(glyph.index..end).map_or(1, |chars| chars.chars().count());
                    let after = text.get(end..).and_then(|rest| rest.chars().next());
                    (cells, after.map_or(run.blank_after, |c| c == ' '))
                }
                false => (usize::from(run.cells), false),
            };
            let ink = text_system.typographic_bounds(font_id, layout.font_size, c).ok()?;
            let room = cell.width * cells.max(1) as f32;
            let spare = if blank_after { cell.width } else { px(0.) };
            fit_ink(ink, room, spare, cell.height, baseline)
        })
        .collect();
    fits.iter().any(Option::is_some).then_some(fits)
}

/// Where a glyph whose `ink` (from its origin on the baseline, y up) is wider than `room` or
/// taller than the line goes: into `room` and the `spare` blank after it, scaled down to fit
/// them and the line and centered there. One that fits but sticks out sideways moves over.
/// `None` when it keeps to `room` and the line already. `baseline` is from the line's top.
fn fit_ink(
    ink: Bounds<Pixels>,
    room: Pixels,
    spare: Pixels,
    height: Pixels,
    baseline: Pixels,
) -> Option<Fit> {
    let (left, bottom, width, tall) = (ink.origin.x, ink.origin.y, ink.size.width, ink.size.height);
    let inside = left >= px(0.) && left + width <= room;
    // Past a hundredth is past rounding.
    if (room / width).min(height / tall) > 0.99 && inside {
        return None;
    }
    let room = room + spare;
    let scale = (room / width).min(height / tall).min(1.);
    let x = (room - width * scale) / 2. - left * scale;
    let y = match scale < 0.99 {
        true => height / 2. + (bottom + tall / 2.) * scale - baseline,
        false => px(0.),
    };
    Some(Fit { scale, offset: point(x, y) })
}

/// Where, from a cell's top, its underline and its strikethrough go: a bit under the baseline of
/// text centered in the cell, and through the middle of its lowercase letters. Each line,
/// `thickness` tall, stays inside the cell however short `line_height` makes it.
fn decorations(
    cell: Pixels,
    ascent: Pixels,
    descent: Pixels,
    thickness: Pixels,
) -> (Pixels, Pixels) {
    let baseline = (cell - ascent - descent) / 2. + ascent;
    let last = cell - thickness;
    ((baseline + descent * 0.618).min(last), ((ascent * 0.5 + baseline) * 0.5).min(last))
}

/// The run's glyphs, those `fit` moved where it put them.
fn paint_fitted(
    layout: &LineLayout,
    fits: &[Option<Fit>],
    origin: Point<Pixels>,
    line_height: Pixels,
    color: Hsla,
    window: &mut Window,
) -> gpui::Result<()> {
    let baseline = origin.y + (line_height - layout.ascent - layout.descent) / 2. + layout.ascent;
    let glyphs = layout.runs.iter().flat_map(|run| run.glyphs.iter().map(|g| (run.font_id, g)));
    for ((font_id, glyph), fit) in glyphs.zip(fits) {
        let fit = fit.unwrap_or(Fit { scale: 1., offset: point(px(0.), px(0.)) });
        let at = point(origin.x + glyph.position.x, baseline) + fit.offset;
        let font_size = layout.font_size * fit.scale;
        if glyph.is_emoji {
            window.paint_emoji(at, font_id, glyph.id, font_size)?;
        } else {
            window.paint_glyph(at, font_id, glyph.id, font_size, color)?;
        }
    }
    Ok(())
}

/// The font in the weight and slant of a run's style.
fn styled(font: &Font, bold: bool, italic: bool) -> Font {
    let mut font = font.clone();
    if bold {
        font = font.bold();
    }
    if italic {
        font = font.italic();
    }
    font
}

fn text_run(font: &Font, len: usize, fg: Rgb, bold: bool, italic: bool) -> TextRun {
    TextRun {
        len,
        font: styled(font, bold, italic),
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

/// A cell of `cell` points in the device pixels of a display of `scale`: what rtty reports as
/// the cell size (the pty's `ws_xpixel` and `ws_ypixel`, `CSI 14 t` and `16 t`), as Ghostty and
/// WezTerm do, so that an image at its own size covers as many pixels on the display.
fn device_pixels(cell: Size<Pixels>, scale: f32) -> (u16, u16) {
    let pixels = |side: Pixels| (f32::from(side) * scale).round() as u16;
    (pixels(cell.width), pixels(cell.height))
}

/// A point `offset` from a pane's top left as SGR pixel mouse reports (1016) give it: in cells of
/// `cell` points, times the cell size in pixels rtty reports, `cell_pixels`.
fn pixel_offset(offset: Point<Pixels>, cell: Size<Pixels>, cell_pixels: (u16, u16)) -> (u32, u32) {
    let pixels = |at: Pixels, side: Pixels, size: u16| (at / side * f32::from(size)).max(0.) as u32;
    (pixels(offset.x, cell.width, cell_pixels.0), pixels(offset.y, cell.height, cell_pixels.1))
}

/// An image of the engine's (RGBA) as GPUI draws it (BGRA).
fn render_image(image: &rtty_vt::Image) -> Arc<RenderImage> {
    let mut pixels = image.rgba.clone();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(image.width, image.height, pixels)
        .expect("the engine's images hold width × height pixels");
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

impl Render for PaneView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout_view = cx.entity();
        let paint_view = cx.entity();
        div()
            .relative()
            .size_full()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::copy_last_output))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::find))
            .on_action(cx.listener(Self::clear_scrollback))
            .on_action(cx.listener(|view, _: &QuickSelect, _, cx| view.quick(false, cx)))
            .on_action(cx.listener(|view, _: &QuickOpen, _, cx| view.quick(true, cx)))
            .on_action(cx.listener(|view, _: &CopyMode, _, cx| view.copy_mode(cx)))
            .on_action(cx.listener(|view, _: &PreviousPrompt, _, cx| {
                view.pane.jump_to_prompt(true);
                cx.notify();
            }))
            .on_action(cx.listener(|view, _: &NextPrompt, _, cx| {
                view.pane.jump_to_prompt(false);
                cx.notify();
            }))
            .on_action(cx.listener(|view, action: &MoveFocus, _, cx| {
                view.claimed_chord(&action.keystroke, cx)
            }))
            .on_action(cx.listener(|view, action: &ResizePane, _, cx| {
                view.claimed_chord(&action.keystroke, cx)
            }))
            .on_action(cx.listener(|view, action: &ClosePane, _, cx| {
                view.claimed_chord(&action.keystroke, cx)
            }))
            .on_action(cx.listener(|view, action: &LastPane, _, cx| {
                view.claimed_chord(&action.keystroke, cx)
            }))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_modifiers_changed(cx.listener(|view, event: &ModifiersChangedEvent, _, cx| {
                view.hover(event.modifiers.platform, cx)
            }))
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
            .on_drop(cx.listener(|view, dropped: &ExternalPaths, window, cx| {
                view.drop_paths(dropped.paths(), window, cx)
            }))
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
            .children(self.search_bar(cx))
            .children(self.link_target(cx))
            .children(self.status_label(cx))
            .children(self.copy_label(cx))
            .children(self.quick_labels(cx))
    }
}

impl PaneView {
    /// The open search's bar, top right: the query, where the current match is among them, and
    /// whether the query is a regex.
    fn search_bar(&self, cx: &App) -> Option<Div> {
        let search = self.search.as_ref()?;
        let status = match (&search.error, search.current) {
            (Some(_), _) => "not a regex".to_string(),
            (None, Some(index)) => format!("{} of {}", index + 1, search.matches.len()),
            (None, None) if search.query.text.is_empty() => String::new(),
            (None, None) => "no matches".to_string(),
        };
        let mode = if search.query.regex { "regex" } else { "find" };
        let text = format!("{mode}: {}\u{258f}  {status}", search.query.text);
        Some(self.label(text, cx).top(px(0.)).right(px(0.)))
    }

    /// Where the link under the pointer really goes, bottom left, while Cmd is held over one
    /// whose text says otherwise.
    fn link_target(&self, cx: &App) -> Option<Div> {
        let (_, target) = self.link.as_ref()?;
        Some(self.label(target.clone()?, cx).bottom(px(0.)).left(px(0.)))
    }

    /// Quick select's labels over the first cells of their matches, bold, in the scheme's colors
    /// inverted: what is left to type of each label the letters typed so far begin.
    fn quick_labels(&self, cx: &App) -> Vec<Div> {
        let (Some(quick), Some(grid)) = (&self.quick, self.geometry) else { return Vec::new() };
        let settings = cx.global::<Settings>();
        let top = self.pane.top(&self.pane.lock());
        let label = |item: &quick::Item<Pick>| {
            let rest = item.label.strip_prefix(&quick.typed)?;
            let ((row, col), _) = item.cells;
            let row = row.checked_sub(top).filter(|&row| row < u64::from(grid.rows))?;
            let label = div()
                .absolute()
                .left(grid.cell.width * f32::from(col))
                .top(grid.cell.height * row as f32)
                .h(grid.cell.height)
                .line_height(grid.cell.height)
                .font(settings.font.clone())
                .text_size(settings.font_size)
                .font_weight(FontWeight::BOLD)
                .bg(hsla(self.scheme.color(FOREGROUND)))
                .text_color(hsla(self.scheme.color(BACKGROUND)))
                .child(rest.to_owned());
            Some(label)
        };
        quick.items.iter().filter_map(label).collect()
    }

    /// That copy mode is on, bottom right: what it selects, and what has been typed toward a
    /// motion.
    fn copy_label(&self, cx: &App) -> Option<Div> {
        let copy = self.copy.as_ref()?;
        let kind = self.pane.selection.map(|selection| format!(": {:?}", selection.kind()));
        let text =
            format!("copy mode{}  {}", kind.unwrap_or_default().to_lowercase(), copy.pending());
        Some(self.label(text.trim_end().into(), cx).bottom(px(0.)).right(px(0.)))
    }

    /// What a key just did, bottom right, for a moment.
    fn status_label(&self, cx: &App) -> Option<Div> {
        let (text, _) = self.status.filter(|(_, at)| at.elapsed() < STATUS)?;
        Some(self.label(text.into(), cx).bottom(px(0.)).right(px(0.)))
    }

    /// A line over the pane, in the scheme's colors inverted.
    fn label(&self, text: String, cx: &App) -> Div {
        let settings = cx.global::<Settings>();
        div()
            .absolute()
            .px(px(8.))
            .py(px(2.))
            .font(settings.font.clone())
            .text_size(settings.font_size)
            .bg(hsla(self.scheme.color(FOREGROUND)))
            .text_color(hsla(self.scheme.color(BACKGROUND)))
            .child(text)
    }
}

/// Shows the file a `file:` URL names in Finder, selected. Never opens it.
fn reveal_in_finder(uri: &str) {
    if let Some(url) = NSURL::URLWithString(&NSString::from_str(uri)) {
        NSWorkspace::sharedWorkspace()
            .activateFileViewerSelectingURLs(&NSArray::from_slice(&[&*url]));
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
        if let Some(search) = self.search.as_mut().filter(|search| !search.accepted) {
            search.query.text.push_str(text);
            self.research(true);
            return cx.notify();
        }
        if self.copy.is_none() {
            self.input(text.as_bytes().to_vec(), cx);
        }
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

    fn ink(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    /// In Menlo 14's cell (its ascent and descent, as `metrics` gets them) at each line height.
    #[test]
    fn the_underline_and_strikethrough_keep_inside_the_cell_at_any_line_height() {
        let (ascent, descent, thickness) = (px(13.), px(3.3), px(1.));
        let cell = |line_height: f32| ((ascent + descent) * line_height).ceil();
        // At 1, as before line_height: under the baseline (13.35 down), through the letters.
        let (underline, strike) = decorations(cell(1.), ascent, descent, thickness);
        assert!((f32::from(underline) - 15.39).abs() < 0.01, "{underline:?}");
        assert!((f32::from(strike) - 9.93).abs() < 0.01, "{strike:?}");
        for line_height in [0.5, 0.6, 0.75, 0.85, 1., 1.5, 3.] {
            let cell = cell(line_height);
            let (underline, strike) = decorations(cell, ascent, descent, thickness);
            assert!(underline + thickness <= cell && strike + thickness <= cell, "{line_height}");
            assert!(underline > strike, "{line_height}");
        }
    }

    /// An icon twice a 10×20 cell's width, its ink from 0 to 20 across and from the baseline up
    /// 10, in a line whose baseline is 15 down, with no blank cell after it: it draws at half
    /// size, centered in its cell.
    #[test]
    fn an_icon_too_wide_for_its_cell_is_scaled_into_the_middle_of_it() {
        let fit = fit_ink(ink(0., 0., 20., 10.), px(10.), px(0.), px(20.), px(15.)).unwrap();
        assert_eq!(fit.scale, 0.5);
        // Its ink, 10×5 now, from 0 to 10 across and 7.5 to 12.5 down the line.
        assert_eq!(fit.offset, point(px(0.), px(-2.5)));
        // A wide character's two cells hold it whole; one that fits but starts left of its cell
        // moves over.
        assert!(fit_ink(ink(0., 0., 20., 10.), px(20.), px(0.), px(20.), px(15.)).is_none());
        let moved = fit_ink(ink(-2., 0., 8., 10.), px(10.), px(0.), px(20.), px(15.)).unwrap();
        assert_eq!((moved.scale, moved.offset), (1., point(px(3.), px(0.))));
        // Too tall for the line, a narrow one shrinks too.
        let tall = fit_ink(ink(0., -10., 5., 40.), px(10.), px(0.), px(20.), px(15.));
        assert_eq!(tall.unwrap().scale, 0.5);
    }

    /// The same icon, 17 across, with a blank cell after it (a prompt's space): it keeps its
    /// size and centers across both cells. Wider than two cells, it shrinks to fit them.
    #[test]
    fn an_icon_followed_by_a_blank_draws_across_both_cells() {
        let fit = fit_ink(ink(0., 0., 17., 10.), px(10.), px(10.), px(20.), px(15.)).unwrap();
        assert_eq!((fit.scale, fit.offset), (1., point(px(1.5), px(0.))));
        let wider = fit_ink(ink(0., 0., 40., 10.), px(10.), px(10.), px(20.), px(15.)).unwrap();
        assert_eq!((wider.scale, wider.offset.x), (0.5, px(0.)));
        // One that keeps to its own cell stays as drawn, blank after it or not.
        assert!(fit_ink(ink(1., 0., 8., 10.), px(10.), px(10.), px(20.), px(15.)).is_none());
    }

    /// On a Retina display (2 pixels a point) a cell of 8×17 points is reported as 16×34 pixels,
    /// and a mouse pixel report agrees with it: the top left of cell (3, 5) is 5 × 16, 3 × 34, and
    /// half a point in is one pixel more. At 1× points and pixels are one.
    #[test]
    fn cell_sizes_and_mouse_pixels_are_device_pixels() {
        let cell = size(px(8.), px(17.));
        assert_eq!(device_pixels(cell, 2.), (16, 34));
        assert_eq!(device_pixels(cell, 1.), (8, 17));
        let corner = point(px(5. * 8.), px(3. * 17.));
        assert_eq!(pixel_offset(corner, cell, (16, 34)), (80, 102));
        assert_eq!(pixel_offset(corner + point(px(0.5), px(0.5)), cell, (16, 34)), (81, 103));
        assert_eq!(pixel_offset(corner, cell, (8, 17)), (40, 51));
        assert_eq!(pixel_offset(point(px(-3.), px(-1.)), cell, (16, 34)), (0, 0));
    }

    /// A pane of 10 rows of 20-pixel cells from y = 100: a pointer 50 pixels above it is 2.5
    /// cells out and scrolls up at 35 lines a second; 2 cells below, down at 30; within, never.
    #[test]
    fn a_drag_past_the_pane_scrolls_toward_the_pointer_faster_further_out() {
        let cell = size(px(10.), px(20.));
        let cursor = Bounds::new(point(px(0.), px(100.)), cell);
        let grid = Geometry { origin: point(px(0.), px(100.)), cell, rows: 10, cols: 8, cursor };
        assert_eq!([beyond(px(50.), grid), beyond(px(340.), grid)], [-2.5, 2.]);
        assert_eq!(
            [beyond(px(100.), grid), beyond(px(250.), grid), beyond(px(300.), grid)],
            [0.; 3]
        );
        let second = Duration::from_secs(1);
        assert_eq!(autoscroll_lines(-2.5, second), 35.);
        assert_eq!(autoscroll_lines(2., second / 10), -3.);
        assert_eq!(autoscroll_lines(0., second), 0.);
    }

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
