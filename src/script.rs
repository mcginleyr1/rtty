//! `--script FILE`: drives the real window as a person would, to check rtty against real programs
//! with nobody at the keyboard and no screenshots. Keys go through GPUI's own keystroke dispatch
//! (key bindings, the key encoder, the text input handler) to the focused pane, mouse events to
//! the pane under them as the window's hit test would send them (a drag to the pane it began in,
//! wherever it goes), focus through GPUI's focus, and `dump` writes the window's layout and what
//! the next frame draws in each shown pane. Rows and columns are the window's cells, counted from
//! the top left of the panes. One step per line; `#` starts a comment.
//!
//! ```text
//! wait TEXT              until the focused pane shows TEXT (10 s at most)
//! sleep MS
//! keys KEYSTROKE...      as gpui parses them: ctrl-a escape enter shift-tab cmd-d ...; each is
//!                        pressed and released
//! nskey KEYSTROKE...     the same, as AppKit events handed to NSApplication, which go the way a
//!                        key pressed on the keyboard does (event monitors, key equivalent, text
//!                        input context, key down); only keys of a US layout's keyboard. `->`
//!                        gives what the key types, the event's characters: alt-h->˙ is Option+H.
//!                        `left:` or `right:` before an alt- key holds that Option key, in the
//!                        event's device-dependent flags as the keyboard sets them: right:alt-e
//! type TEXT              the rest of the line, one keystroke per character
//! click ROW COL [left|middle|right] [CLICKS] [cmd]
//! drag ROW COL ROW COL [MS]  with the left button, held at the end for MS before the release;
//!                        the end may be past the panes (negative rows are above them)
//! move ROW COL [cmd]     with no button; `cmd` holds Cmd, as over a link
//! wheel ROW COL LINES    positive scrolls up
//! drop ROW COL "PATH"... files dropped from Finder onto the pane there, each path a JSON string
//!                        ("/tmp/a b.png" "/tmp/café.png"), through the pane's drop handler
//! focus in|out           the keyboard comes to the focused pane or leaves the window's elements
//! bar click tab N        in the titlebar's bar: the pointer onto tab N's pill (from 1), then a
//!                        press and release there
//! bar click close N      ... on tab N's close button
//! bar click new          ... on the +
//! bar hover tab N|new|none   the pointer onto tab N's pill or the +, or off the bar
//! answer ok|cancel       press a button on the question closing asked first (Cmd+W, a tab's
//!                        close button, the window's, quitting), which a scripted window keeps in
//!                        its dump instead of showing a sheet
//! terminate              AppKit's `terminate:`, in process, as the dock's Quit and logging out
//!                        start it: it asks first, and AppKit waits for the answer
//! window close N         the close button of window N (from 1, in the order they opened), as a
//!                        click on it goes on; not the script's own window
//! menu MENU ITEM         the menu bar's item (`menu Shell Split Right`): the action it carries,
//!                        dispatched to the window as a click on it dispatches it to the key
//!                        window, which a scripted window never is
//! app active|inactive    what rtty does when macOS makes it the active app, the window the key
//!                        one, and when rtty stops being it; the window takes no keyboard
//! hotkey                 what the global hotkey does when pressed, which a scripted rtty keeps
//!                        in its dump instead of doing
//! appearance dark|light  what the window does when macOS flips
//! resize ROWS COLS       the window, to fit that many cells of panes
//! dump FILE
//! quit
//! ```

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use gpui::{
    AsyncWindowContext, Context, KeyUpEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, OwnedMenuItem, Pixels, Point, ScrollDelta, ScrollWheelEvent,
    TouchPhase, WeakEntity, Window, WindowAppearance, WindowHandle, point,
};
use objc2::{MainThreadMarker, sel};
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSView};
use objc2_foundation::{NSObjectNSDelayedPerforming, NSPoint, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::tabbar::Hit;
use crate::window::TerminalWindow;
use crate::{LEFT_OPTION, RIGHT_OPTION};

#[derive(Debug)]
pub enum Step {
    Wait(String),
    Sleep(Duration),
    Keys(Vec<Keystroke>),
    /// Each with the Option key it names, if it names one.
    NsKeys(Vec<(Keystroke, NSEventModifierFlags)>),
    Type(String),
    Click {
        row: u16,
        col: u16,
        button: MouseButton,
        clicks: usize,
        cmd: bool,
    },
    Drag {
        from: (i32, i32),
        to: (i32, i32),
        hold: Duration,
    },
    Move {
        row: u16,
        col: u16,
        cmd: bool,
    },
    Wheel {
        row: u16,
        col: u16,
        lines: f32,
    },
    Drop {
        row: u16,
        col: u16,
        paths: Vec<PathBuf>,
    },
    Focus(bool),
    BarClick(Hit),
    BarHover(Option<Hit>),
    Answer(usize),
    Terminate,
    WindowClose(usize),
    /// The menu's title, the item's.
    Menu(String, String),
    /// Whether rtty is the active app.
    App(bool),
    Hotkey,
    Appearance(WindowAppearance),
    Resize {
        rows: u16,
        cols: u16,
    },
    Dump(PathBuf),
    Quit,
}

enum Mouse {
    Down(MouseDownEvent),
    Up(MouseUpEvent),
    Move(MouseMoveEvent),
    Wheel(ScrollWheelEvent),
}

impl Mouse {
    fn position(&self) -> Point<Pixels> {
        match self {
            Mouse::Down(event) => event.position,
            Mouse::Up(event) => event.position,
            Mouse::Move(event) => event.position,
            Mouse::Wheel(event) => event.position,
        }
    }
}

const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// The steps of a script, each with its line number; an error names the line.
pub fn parse(source: &str) -> Result<Vec<(usize, Step)>, String> {
    let mut steps = Vec::new();
    for (number, line) in source.lines().enumerate().map(|(index, line)| (index + 1, line)) {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        let mut args: Vec<&str> = rest.split_whitespace().collect();
        let cmd = matches!(command, "click" | "move") && args.last() == Some(&"cmd");
        if cmd {
            args.pop();
        }
        let error = |what: &str| format!("script line {number}: {what}: {line:?}");
        let numbers = |count: usize| -> Result<Vec<u16>, String> {
            let numbers: Option<Vec<u16>> =
                args.iter().take(count).map(|arg| arg.parse().ok()).collect();
            numbers
                .filter(|numbers| numbers.len() == count)
                .ok_or_else(|| error("numbers expected"))
        };
        let step = match command {
            "wait" if !rest.is_empty() => Step::Wait(rest.to_owned()),
            "type" if !rest.is_empty() => Step::Type(rest.to_owned()),
            "sleep" => Step::Sleep(Duration::from_millis(numbers(1)?[0].into())),
            "keys" | "nskey" => {
                let keystrokes = args
                    .iter()
                    .map(|key| {
                        let (side, key) = match key.split_once(':') {
                            Some(("left", key)) if command == "nskey" => (LEFT_OPTION, key),
                            Some(("right", key)) if command == "nskey" => (RIGHT_OPTION, key),
                            _ => (NSEventModifierFlags::empty(), *key),
                        };
                        let keystroke =
                            Keystroke::parse(key).map_err(|_| error("unknown keystroke"));
                        match keystroke? {
                            keystroke if side.is_empty() || keystroke.modifiers.alt => {
                                Ok((keystroke, side))
                            }
                            _ => Err(error("an alt- key after left: or right: expected")),
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if command == "keys" {
                    Step::Keys(keystrokes.into_iter().map(|(keystroke, _)| keystroke).collect())
                } else if keystrokes.iter().all(|(keystroke, _)| key_code(&keystroke.key).is_some())
                {
                    Step::NsKeys(keystrokes)
                } else {
                    return Err(error("a key a US keyboard has expected"));
                }
            }
            "click" => {
                let cell = numbers(2)?;
                let button = match args.get(2).copied() {
                    None | Some("left") => MouseButton::Left,
                    Some("middle") => MouseButton::Middle,
                    Some("right") => MouseButton::Right,
                    Some(_) => return Err(error("left, middle or right expected")),
                };
                let clicks = args.get(3).map_or(Ok(1), |clicks| clicks.parse());
                let clicks = clicks.map_err(|_| error("a click count expected"))?;
                Step::Click { row: cell[0], col: cell[1], button, clicks, cmd }
            }
            "drag" => {
                let cells: Vec<i32> = args.iter().take(4).filter_map(|n| n.parse().ok()).collect();
                let hold: Option<u64> = args.get(4).map_or(Some(0), |ms| ms.parse().ok());
                match (cells.as_slice(), hold) {
                    (&[from_row, from_col, to_row, to_col], Some(hold)) => Step::Drag {
                        from: (from_row, from_col),
                        to: (to_row, to_col),
                        hold: Duration::from_millis(hold),
                    },
                    _ => return Err(error("ROW COL ROW COL [MS] expected")),
                }
            }
            "move" => {
                let cell = numbers(2)?;
                Step::Move { row: cell[0], col: cell[1], cmd }
            }
            "wheel" => {
                let cell = numbers(2)?;
                let lines = args.get(2).and_then(|lines| lines.parse().ok());
                let lines = lines.ok_or_else(|| error("a number of lines expected"))?;
                Step::Wheel { row: cell[0], col: cell[1], lines }
            }
            "drop" => {
                let cell = numbers(2)?;
                let quoted = rest.splitn(3, ' ').nth(2).unwrap_or_default();
                let paths: Result<Vec<PathBuf>, _> =
                    serde_json::Deserializer::from_str(quoted).into_iter().collect();
                let paths = paths.map_err(|_| error("paths as JSON strings expected"))?;
                if paths.is_empty() {
                    return Err(error("a path expected"));
                }
                Step::Drop { row: cell[0], col: cell[1], paths }
            }
            "focus" => match rest.trim() {
                "in" => Step::Focus(true),
                "out" => Step::Focus(false),
                _ => return Err(error("in or out expected")),
            },
            "bar" => {
                let tab = || match args.get(2).and_then(|n| n.parse::<usize>().ok()) {
                    Some(n) if n > 0 => Ok(n - 1),
                    _ => Err(error("a tab number from 1 expected")),
                };
                match (args.first().copied(), args.get(1).copied()) {
                    (Some("click"), Some("tab")) => Step::BarClick(Hit::Tab(tab()?)),
                    (Some("click"), Some("close")) => Step::BarClick(Hit::Close(tab()?)),
                    (Some("click"), Some("new")) => Step::BarClick(Hit::New),
                    (Some("hover"), Some("tab")) => Step::BarHover(Some(Hit::Tab(tab()?))),
                    (Some("hover"), Some("new")) => Step::BarHover(Some(Hit::New)),
                    (Some("hover"), Some("none")) => Step::BarHover(None),
                    _ => return Err(error("bar click|hover tab N, close N, new or none expected")),
                }
            }
            "answer" => match rest.trim() {
                "ok" => Step::Answer(0),
                "cancel" => Step::Answer(1),
                _ => return Err(error("ok or cancel expected")),
            },
            "window" => match (args.first().copied(), args.get(1).map(|n| n.parse::<usize>())) {
                (Some("close"), Some(Ok(n))) if n > 0 => Step::WindowClose(n - 1),
                _ => return Err(error("window close N, from 1, expected")),
            },
            "menu" => match rest.trim().split_once(' ') {
                Some((menu, item)) => Step::Menu(menu.into(), item.trim().into()),
                None => return Err(error("menu MENU ITEM expected")),
            },
            "app" => match rest.trim() {
                "active" => Step::App(true),
                "inactive" => Step::App(false),
                _ => return Err(error("active or inactive expected")),
            },
            "appearance" => match rest.trim() {
                "dark" => Step::Appearance(WindowAppearance::Dark),
                "light" => Step::Appearance(WindowAppearance::Light),
                _ => return Err(error("dark or light expected")),
            },
            "resize" => {
                let size = numbers(2)?;
                Step::Resize { rows: size[0], cols: size[1] }
            }
            "dump" if !rest.is_empty() => Step::Dump(rest.trim().into()),
            "terminate" => Step::Terminate,
            "hotkey" => Step::Hotkey,
            "quit" => Step::Quit,
            _ => return Err(error("unknown step")),
        };
        steps.push((number, step));
    }
    Ok(steps)
}

/// Runs the steps against the window, each after the focused pane's first frame. A step that
/// fails ends rtty with exit status 1.
pub fn run(steps: Vec<(usize, Step)>, window: &mut Window, cx: &mut Context<TerminalWindow>) {
    cx.spawn_in(window, async move |this, cx| {
        for (number, step) in steps {
            if let Err(err) = perform(&this, &step, cx).await {
                eprintln!("rtty: script line {number}: {err}");
                std::process::exit(1);
            }
        }
    })
    .detach();
}

async fn perform(
    this: &WeakEntity<TerminalWindow>,
    step: &Step,
    cx: &mut AsyncWindowContext,
) -> Result<(), String> {
    let pane = this.update(cx, |window, _| window.focused()).map_err(|err| err.to_string())?;
    let pane = pane.ok_or("no pane")?;
    // The pane's first frame lays out what the step works on.
    while pane
        .update(cx, |pane, _| pane.cell_center(0, 0))
        .map_err(|err| err.to_string())?
        .is_none()
    {
        cx.background_executor().timer(Duration::from_millis(10)).await;
    }
    let center = |row: i32, col: i32, cx: &mut AsyncWindowContext| {
        this.update_in(cx, |this, window, cx| this.cell_center(row, col, window, cx))
            .map_err(|err| err.to_string())
    };
    // gpui 0.2.2 keeps `Window::dispatch_event`'s result type private, so mouse events go to the
    // panes' own handlers as the window's hit testing would send them: to the pane under the
    // pointer, a release to the others too (`on_mouse_up_out`), and motion to every pane's drag
    // (`drag_move`, which listens to the whole window).
    let dispatch = |event: Mouse, cx: &mut AsyncWindowContext| {
        this.update_in(cx, |this, window, cx| {
            let under = this.pane_at(event.position(), window, cx);
            for pane in this.shown(window, cx) {
                let over = under.as_ref() == Some(&pane);
                pane.update(cx, |pane, cx| match &event {
                    Mouse::Down(event) if over => pane.mouse_down(event, window, cx),
                    Mouse::Up(event) => pane.mouse_up(event, window, cx),
                    Mouse::Move(event) => {
                        if over {
                            pane.mouse_move(event, window, cx);
                        }
                        pane.drag_move(event, window, cx)
                    }
                    Mouse::Wheel(event) if over => pane.scroll_wheel(event, window, cx),
                    _ => {}
                });
            }
        })
        .map_err(|err| err.to_string())
    };
    let none = Modifiers::default();
    match step {
        Step::Wait(text) => {
            let started = Instant::now();
            while !pane.update(cx, |pane, _| pane.screen_text()).is_ok_and(|s| s.contains(text)) {
                if started.elapsed() > WAIT_TIMEOUT {
                    let screen = pane.update(cx, |pane, _| pane.screen_text()).unwrap_or_default();
                    return Err(format!("never saw {text:?}; the screen:\n{screen}"));
                }
                cx.background_executor().timer(Duration::from_millis(20)).await;
            }
        }
        Step::Sleep(duration) => cx.background_executor().timer(*duration).await,
        Step::Keys(keystrokes) => {
            for keystroke in keystrokes {
                stroke(this, keystroke.clone(), cx)?;
            }
        }
        Step::NsKeys(keystrokes) => {
            let view = cx
                .update(|window, _| match window.window_handle().map(|handle| handle.as_raw()) {
                    Ok(RawWindowHandle::AppKit(handle)) => Some(handle.ns_view),
                    _ => None,
                })
                .map_err(|err| err.to_string())?
                .ok_or("the window has no AppKit view")?;
            for (keystroke, side) in keystrokes {
                // Outside any update, as AppKit calls GPUI.
                appkit_stroke(view, keystroke, *side)?;
                cx.background_executor().timer(Duration::from_millis(20)).await;
            }
        }
        Step::Type(text) => {
            for c in text.chars() {
                let shift = c.is_ascii_uppercase();
                let keystroke = Keystroke {
                    modifiers: Modifiers { shift, ..none },
                    key: c.to_ascii_lowercase().to_string(),
                    key_char: Some(c.to_string()),
                };
                stroke(this, keystroke, cx)?;
            }
        }
        Step::Click { row, col, button, clicks, cmd } => {
            let position = center((*row).into(), (*col).into(), cx)?;
            for click_count in 1..=*clicks {
                let button = *button;
                let modifiers = Modifiers { platform: *cmd, ..none };
                let first_mouse = false;
                let down = MouseDownEvent { button, position, modifiers, click_count, first_mouse };
                dispatch(Mouse::Down(down), cx)?;
                let up = MouseUpEvent { button, position, modifiers, click_count };
                dispatch(Mouse::Up(up), cx)?;
            }
        }
        Step::Drag { from, to, hold } => {
            let (start, end) = (center(from.0, from.1, cx)?, center(to.0, to.1, cx)?);
            let (button, modifiers, click_count) = (MouseButton::Left, none, 1);
            let position = start;
            let down =
                MouseDownEvent { button, position, modifiers, click_count, first_mouse: false };
            dispatch(Mouse::Down(down), cx)?;
            let drag = MouseMoveEvent { position: end, pressed_button: Some(button), modifiers };
            dispatch(Mouse::Move(drag), cx)?;
            cx.background_executor().timer(*hold).await;
            let up = MouseUpEvent { button, position: end, modifiers, click_count };
            dispatch(Mouse::Up(up), cx)?;
        }
        Step::Move { row, col, cmd } => {
            let position = center((*row).into(), (*col).into(), cx)?;
            let modifiers = Modifiers { platform: *cmd, ..none };
            let motion = MouseMoveEvent { position, pressed_button: None, modifiers };
            dispatch(Mouse::Move(motion), cx)?;
        }
        Step::Wheel { row, col, lines } => {
            let position = center((*row).into(), (*col).into(), cx)?;
            let wheel = ScrollWheelEvent {
                position,
                delta: ScrollDelta::Lines(point(0., *lines)),
                modifiers: none,
                touch_phase: TouchPhase::Moved,
            };
            dispatch(Mouse::Wheel(wheel), cx)?;
        }
        Step::Drop { row, col, paths } => {
            let position = center((*row).into(), (*col).into(), cx)?;
            this.update_in(cx, |this, window, cx| {
                let pane = this.pane_at(position, window, cx).ok_or("no pane there")?;
                pane.update(cx, |pane, cx| pane.drop_paths(paths, window, cx));
                Ok::<_, String>(())
            })
            .map_err(|err| err.to_string())??
        }
        Step::Focus(true) => pane
            .update_in(cx, |pane, window, _| pane.focus(window))
            .map_err(|err| err.to_string())?,
        Step::Focus(false) => {
            cx.update(|window, _| window.blur()).map_err(|err| err.to_string())?
        }
        // Through the bar's own handlers and hit testing, at the middle of what the step names.
        Step::BarClick(hit) => this
            .update_in(cx, |this, window, cx| {
                let at = this.bar_point(*hit, window).ok_or(format!("no {hit:?} in the bar"))?;
                this.bar_move(Some(at), window, cx);
                this.bar_down(at, 1, window, cx);
                this.bar_up(at, window, cx);
                Ok::<_, String>(())
            })
            .map_err(|err| err.to_string())??,
        Step::BarHover(hit) => this
            .update_in(cx, |this, window, cx| {
                let at = match hit {
                    Some(hit) => {
                        Some(this.bar_point(*hit, window).ok_or(format!("no {hit:?} in the bar"))?)
                    }
                    None => None,
                };
                this.bar_move(at, window, cx);
                Ok::<_, String>(())
            })
            .map_err(|err| err.to_string())??,
        Step::Answer(button) => this
            .update(cx, |this, _| this.answer_question(*button))
            .map_err(|err| err.to_string())??,
        Step::WindowClose(index) => cx
            .update(|_, cx| {
                let mut windows = cx.windows().into_iter().filter_map(|window| window.downcast());
                let number = index + 1;
                let handle: WindowHandle<TerminalWindow> =
                    windows.nth(*index).ok_or(format!("no window {number}"))?;
                handle
                    .update(cx, |this, window, cx| this.close_button(window, cx))
                    .map_err(|_| format!("window {number} is the script's own"))
            })
            .map_err(|err| err.to_string())??,
        Step::Menu(menu, item) => cx
            .update(|window, cx| {
                let menus = cx.get_menus().unwrap_or_default();
                let items = menus.iter().filter(|bar| bar.name.as_ref() == menu);
                let action = items.flat_map(|bar| &bar.items).find_map(|entry| match entry {
                    OwnedMenuItem::Action { name, action, .. } if name == item => {
                        Some(action.boxed_clone())
                    }
                    _ => None,
                });
                window.dispatch_action(action.ok_or(format!("no {item:?} in menu {menu}"))?, cx);
                Ok::<_, String>(())
            })
            .map_err(|err| err.to_string())??,
        Step::App(active) => cx
            .update(|window, cx| {
                if *active {
                    let handle = Window::window_handle(window).downcast();
                    let handle = handle.expect("a terminal window");
                    crate::app::key_window(handle, cx);
                }
                crate::app::set_active(*active, cx)
            })
            .map_err(|err| err.to_string())?,
        Step::Hotkey => {
            cx.update(|_, cx| crate::app::hotkey_pressed(cx)).map_err(|err| err.to_string())?
        }
        Step::Appearance(appearance) => {
            this.update(cx, |this, cx| this.follow_appearance(*appearance, cx))
                .map_err(|err| err.to_string())?;
        }
        Step::Resize { rows, cols } => {
            let size = this
                .update_in(cx, |this, window, cx| this.size_for(*rows, *cols, window, cx))
                .map_err(|err| err.to_string())?;
            cx.update(|window, _| window.resize(size)).map_err(|err| err.to_string())?;
        }
        Step::Dump(path) => {
            let dump = this
                .update_in(cx, |this, window, cx| this.dump(window, cx))
                .map_err(|err| err.to_string())?;
            std::fs::write(path, dump).map_err(|err| format!("{}: {err}", path.display()))?;
        }
        // From the run loop, as AppKit handles the quit Apple event: AppKit waits for the
        // question's answer inside `terminate:`, and the steps after it run meanwhile.
        Step::Terminate => {
            let app =
                NSApplication::sharedApplication(MainThreadMarker::new().expect("main thread"));
            // SAFETY: `terminate:` takes the sender, which may be nil.
            unsafe { app.performSelector_withObject_afterDelay(sel!(terminate:), None, 0.) }
        }
        Step::Quit => cx
            .update(|_, cx| crate::app::quit("the script's quit", cx))
            .map_err(|err| err.to_string())?,
    }
    Ok(())
}

/// A key pressed through GPUI's dispatch, then released to the pane that has the keyboard by
/// then, as GPUI would send it.
fn stroke(
    this: &WeakEntity<TerminalWindow>,
    keystroke: Keystroke,
    cx: &mut AsyncWindowContext,
) -> Result<(), String> {
    let up = KeyUpEvent { keystroke: keystroke.clone() };
    cx.update(|window, cx| window.dispatch_keystroke(keystroke, cx))
        .map_err(|err| err.to_string())?;
    let pane = this.update(cx, |this, _| this.focused()).map_err(|err| err.to_string())?;
    match pane {
        Some(pane) => pane
            .update_in(cx, |pane, window, cx| pane.key_up(&up, window, cx))
            .map_err(|err| err.to_string()),
        None => Ok(()),
    }
}

/// A key pressed and released, its events handed to NSApplication as the window server hands it
/// the keyboard's, so AppKit's own dispatch (event monitors, key equivalents, then the window's
/// key down) decides where they go. In process: they reach no other app and take no focus.
/// `side` adds the device-dependent flag of the Option key held.
fn appkit_stroke(
    view: NonNull<c_void>,
    keystroke: &Keystroke,
    side: NSEventModifierFlags,
) -> Result<(), String> {
    // SAFETY: GPUI's window handle is its live NSView, used on the main thread.
    let view: &NSView = unsafe { view.cast().as_ref() };
    let window = view.window().ok_or("the view is in no window")?;
    let (code, c) = key_code(&keystroke.key).expect("parsed keys have a code");
    let held = keystroke.modifiers;
    let flags = [
        (held.platform, NSEventModifierFlags::Command),
        (held.control, NSEventModifierFlags::Control),
        (held.alt, NSEventModifierFlags::Option),
        (held.shift, NSEventModifierFlags::Shift),
    ];
    let flags =
        flags.into_iter().filter(|(held, _)| *held).fold(side, |flags, (_, flag)| flags | flag);
    let plain = match held.shift {
        true => c.to_uppercase().to_string(),
        false => c.to_string(),
    };
    // What the key types with its modifiers, as `alt-h->˙` gives it; AppKit's `characters`.
    let typed = NSString::from_str(keystroke.key_char.as_deref().unwrap_or(&plain));
    let plain = NSString::from_str(&plain);
    let event = |kind| {
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind, NSPoint::ZERO, flags, 0., window.windowNumber(), None, &typed, &plain, false, code,
        )
        .ok_or("AppKit made no key event")
    };
    let app = NSApplication::sharedApplication(MainThreadMarker::new().expect("the main thread"));
    app.sendEvent(&*event(NSEventType::KeyDown)?);
    app.sendEvent(&*event(NSEventType::KeyUp)?);
    Ok(())
}

/// The key's virtual key code on a US keyboard, and the character AppKit's events give it (the
/// arrows' are its function-key characters).
fn key_code(key: &str) -> Option<(u16, char)> {
    // Codes 0 to 50, by the character the key types; return and tab stand in as \n and \t.
    const CHARACTERS: &str = "asdfhgzxcv§bqweryt123465=97-80]ou[ip\nlj'k;\\,/nm.\t `";
    let named = match key {
        "enter" => Some((36, '\r')),
        "tab" => Some((48, '\t')),
        "space" => Some((49, ' ')),
        "backspace" => Some((51, '\u{7f}')),
        "escape" => Some((53, '\u{1b}')),
        "left" => Some((123, '\u{f702}')),
        "right" => Some((124, '\u{f703}')),
        "down" => Some((125, '\u{f701}')),
        "up" => Some((126, '\u{f700}')),
        _ => None,
    };
    let mut chars = key.chars();
    let single = chars.next().filter(|_| chars.next().is_none());
    let typed = |c| CHARACTERS.chars().position(|key| key == c).map(|code| (code as u16, c));
    named.or_else(|| typed(single?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_parse_and_mistakes_name_their_line() {
        let script =
            "# a comment\n\nwait $ \nkeys ctrl-a escape\ntype :wq\nclick 1 2 right 2\nquit\n";
        let steps = parse(script).unwrap();
        let numbers: Vec<usize> = steps.iter().map(|(number, _)| *number).collect();
        assert_eq!(numbers, [3, 4, 5, 6, 7]);
        assert!(matches!(&steps[0].1, Step::Wait(text) if text == "$ "));
        assert!(
            matches!(&steps[1].1, Step::Keys(keys) if keys.len() == 2 && keys[0].modifiers.control)
        );
        assert!(matches!(
            &steps[3].1,
            Step::Click { row: 1, col: 2, button: MouseButton::Right, clicks: 2, cmd: false }
        ));
        let cmd = parse("click 3 4 left 1 cmd\nmove 5 6 cmd").unwrap();
        assert!(matches!(&cmd[0].1, Step::Click { row: 3, col: 4, clicks: 1, cmd: true, .. }));
        assert!(matches!(&cmd[1].1, Step::Move { row: 5, col: 6, cmd: true }));
        let bar = parse(
            "bar click tab 2\nbar click close 1\nbar click new\nbar hover tab 3\nbar hover none",
        );
        let bar: Vec<Step> = bar.unwrap().into_iter().map(|(_, step)| step).collect();
        assert!(matches!(bar[0], Step::BarClick(Hit::Tab(1))));
        assert!(matches!(bar[1], Step::BarClick(Hit::Close(0))));
        assert!(matches!(bar[2], Step::BarClick(Hit::New)));
        assert!(matches!(bar[3], Step::BarHover(Some(Hit::Tab(2)))));
        assert!(matches!(bar[4], Step::BarHover(None)));
        assert!(parse("bar click tab 0").unwrap_err().contains("a tab number from 1"));
        let answers = parse("answer ok\nanswer cancel").unwrap();
        assert!(matches!((&answers[0].1, &answers[1].1), (Step::Answer(0), Step::Answer(1))));
        assert!(parse("answer maybe").unwrap_err().contains("ok or cancel expected"));
        assert!(matches!(parse("terminate").unwrap()[0].1, Step::Terminate));
        assert!(matches!(parse("hotkey").unwrap()[0].1, Step::Hotkey));
        let app = parse("app active\napp inactive").unwrap();
        assert!(matches!((&app[0].1, &app[1].1), (Step::App(true), Step::App(false))));
        assert!(parse("app front").unwrap_err().contains("active or inactive expected"));
        assert!(matches!(parse("window close 2").unwrap()[0].1, Step::WindowClose(1)));
        for wrong in ["window close 0", "window close", "window open 2"] {
            assert!(parse(wrong).unwrap_err().contains("window close N, from 1"), "{wrong}");
        }
        let menu = parse("menu Shell Split Right").unwrap();
        assert!(
            matches!(&menu[0].1, Step::Menu(menu, item) if menu == "Shell" && item == "Split Right")
        );
        assert!(parse("menu Shell").unwrap_err().contains("menu MENU ITEM expected"));
        assert!(parse("bar wave").unwrap_err().contains("bar click|hover"));
        assert!(parse("wiggle 1 2").unwrap_err().starts_with("script line 1: unknown step"));
        assert!(parse("quit\nclick 1").unwrap_err().starts_with("script line 2: numbers expected"));
        assert!(parse("keys a-b").unwrap_err().contains("unknown keystroke"));
        let drags = parse("drag 1 2 -3 4 500\ndrag 5 6 7 8").unwrap();
        let held = Duration::from_millis(500);
        assert!(
            matches!(drags[0].1, Step::Drag { from: (1, 2), to: (-3, 4), hold } if hold == held)
        );
        assert!(matches!(drags[1].1, Step::Drag { to: (7, 8), hold: Duration::ZERO, .. }));
        assert!(parse("drag 1 2 3").unwrap_err().contains("ROW COL ROW COL [MS] expected"));
        let drop = parse(r#"drop 2 7 "/tmp/a b.png" "/tmp/it's \"x\"" "/tmp/café""#).unwrap();
        let paths = ["/tmp/a b.png", "/tmp/it's \"x\"", "/tmp/café"].map(PathBuf::from);
        assert!(
            matches!(&drop[0].1, Step::Drop { row: 2, col: 7, paths: dropped } if *dropped == paths)
        );
        assert!(parse("drop 2 7").unwrap_err().contains("a path expected"));
        assert!(parse("drop 2 7 /tmp/a").unwrap_err().contains("paths as JSON strings expected"));
        let appkit = parse("nskey cmd-shift-d cmd-f enter").unwrap();
        assert!(matches!(&appkit[0].1, Step::NsKeys(keys) if keys.len() == 3));
        let sides = parse("nskey left:alt-h right:alt-e e shift-;->:").unwrap();
        let Step::NsKeys(sides) = &sides[0].1 else { panic!("nskey") };
        let sides: Vec<_> = sides.iter().map(|(key, side)| (key.key.as_str(), *side)).collect();
        let none = NSEventModifierFlags::empty();
        assert_eq!(sides, [("h", LEFT_OPTION), ("e", RIGHT_OPTION), ("e", none), (";", none)]);
        assert!(parse("nskey right:e").unwrap_err().contains("an alt- key after left: or right:"));
        assert!(parse("nskey f13").unwrap_err().contains("a key a US keyboard has"));
        let option = parse("nskey alt-shift-h->Ó").unwrap();
        assert!(matches!(&option[0].1, Step::NsKeys(keys)
            if keys[0].0.key == "h" && keys[0].0.key_char.as_deref() == Some("Ó")));
        assert_eq!([key_code("d"), key_code("t")], [Some((2, 'd')), Some((17, 't'))]);
        assert_eq!([key_code("`"), key_code("enter")], [Some((50, '`')), Some((36, '\r'))]);
        assert_eq!(
            [key_code("escape"), key_code("up")],
            [Some((53, '\u{1b}')), Some((126, '\u{f700}'))]
        );
    }
}
