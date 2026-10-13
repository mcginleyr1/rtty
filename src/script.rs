//! `--script FILE`: drives the real window as a person would, to check rtty against real programs
//! with nobody at the keyboard and no screenshots. Keys go through GPUI's own keystroke dispatch
//! (key bindings, the key encoder, the text input handler) to the focused pane, mouse events to
//! the pane under them as the window's hit test would send them, focus through GPUI's focus, and
//! `dump` writes the window's layout and what the next frame draws in each shown pane. Rows and
//! columns are the window's cells, counted from the top left of the panes. One step per line; `#`
//! starts a comment.
//!
//! ```text
//! wait TEXT              until the focused pane shows TEXT (10 s at most)
//! sleep MS
//! keys KEYSTROKE...      as gpui parses them: ctrl-a escape enter shift-tab cmd-d ...; each is
//!                        pressed and released
//! nskey KEYSTROKE...     the same, as AppKit events to the window, which go the way a key
//!                        pressed on the keyboard does (key equivalent, text input context, key
//!                        down); only keys of a US layout's keyboard
//! type TEXT              the rest of the line, one keystroke per character
//! click ROW COL [left|middle|right] [CLICKS] [cmd]
//! drag ROW COL ROW COL   with the left button
//! move ROW COL [cmd]     with no button; `cmd` holds Cmd, as over a link
//! wheel ROW COL LINES    positive scrolls up
//! focus in|out           the keyboard comes to the focused pane or leaves the window's elements
//! bar click tab N        in the titlebar's bar: the pointer onto tab N's pill (from 1), then a
//!                        press and release there
//! bar click close N      ... on tab N's close button
//! bar click new          ... on the +
//! bar hover tab N|new|none   the pointer onto tab N's pill or the +, or off the bar
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
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, TouchPhase,
    WeakEntity, Window, WindowAppearance, point,
};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType, NSView};
use objc2_foundation::{NSPoint, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::tabbar::Hit;
use crate::window::TerminalWindow;

#[derive(Debug)]
pub enum Step {
    Wait(String),
    Sleep(Duration),
    Keys(Vec<Keystroke>),
    NsKeys(Vec<Keystroke>),
    Type(String),
    Click { row: u16, col: u16, button: MouseButton, clicks: usize, cmd: bool },
    Drag { from: (u16, u16), to: (u16, u16) },
    Move { row: u16, col: u16, cmd: bool },
    Wheel { row: u16, col: u16, lines: f32 },
    Focus(bool),
    BarClick(Hit),
    BarHover(Option<Hit>),
    Appearance(WindowAppearance),
    Resize { rows: u16, cols: u16 },
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
                    .map(|key| Keystroke::parse(key).map_err(|_| error("unknown keystroke")))
                    .collect::<Result<Vec<_>, _>>()?;
                if command == "keys" {
                    Step::Keys(keystrokes)
                } else if keystrokes.iter().all(|keystroke| key_code(&keystroke.key).is_some()) {
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
                let cells = numbers(4)?;
                Step::Drag { from: (cells[0], cells[1]), to: (cells[2], cells[3]) }
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
    let center = |row: u16, col: u16, cx: &mut AsyncWindowContext| {
        this.update_in(cx, |this, window, cx| this.cell_center(row, col, window, cx))
            .map_err(|err| err.to_string())
    };
    // gpui 0.2.2 keeps `Window::dispatch_event`'s result type private, so mouse events go to the
    // panes' own handlers as the window's hit testing would send them: to the pane under the
    // pointer, and a release to the others too (`on_mouse_up_out`).
    let dispatch = |event: Mouse, cx: &mut AsyncWindowContext| {
        this.update_in(cx, |this, window, cx| {
            let under = this.pane_at(event.position(), window, cx);
            for pane in this.shown(window, cx) {
                let over = under.as_ref() == Some(&pane);
                pane.update(cx, |pane, cx| match &event {
                    Mouse::Down(event) if over => pane.mouse_down(event, window, cx),
                    Mouse::Up(event) => pane.mouse_up(event, window, cx),
                    Mouse::Move(event) if over => pane.mouse_move(event, window, cx),
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
            for keystroke in keystrokes {
                // Outside any update, as AppKit calls GPUI.
                appkit_stroke(view, keystroke)?;
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
            let position = center(*row, *col, cx)?;
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
        Step::Drag { from, to } => {
            let (start, end) = (center(from.0, from.1, cx)?, center(to.0, to.1, cx)?);
            let (button, modifiers, click_count) = (MouseButton::Left, none, 1);
            let position = start;
            let down =
                MouseDownEvent { button, position, modifiers, click_count, first_mouse: false };
            dispatch(Mouse::Down(down), cx)?;
            let drag = MouseMoveEvent { position: end, pressed_button: Some(button), modifiers };
            dispatch(Mouse::Move(drag), cx)?;
            let up = MouseUpEvent { button, position: end, modifiers, click_count };
            dispatch(Mouse::Up(up), cx)?;
        }
        Step::Move { row, col, cmd } => {
            let position = center(*row, *col, cx)?;
            let modifiers = Modifiers { platform: *cmd, ..none };
            let motion = MouseMoveEvent { position, pressed_button: None, modifiers };
            dispatch(Mouse::Move(motion), cx)?;
        }
        Step::Wheel { row, col, lines } => {
            let position = center(*row, *col, cx)?;
            let wheel = ScrollWheelEvent {
                position,
                delta: ScrollDelta::Lines(point(0., *lines)),
                modifiers: none,
                touch_phase: TouchPhase::Moved,
            };
            dispatch(Mouse::Wheel(wheel), cx)?;
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
        Step::Quit => {
            crate::note("quitting: the script's quit");
            cx.update(|_, cx| cx.quit()).map_err(|err| err.to_string())?
        }
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

/// A key pressed as AppKit delivers it from the keyboard: a chord with Cmd or Ctrl is offered to
/// the window as a key equivalent first, and only if that is not taken comes as a key down; then
/// the key comes up.
fn appkit_stroke(view: NonNull<c_void>, keystroke: &Keystroke) -> Result<(), String> {
    // SAFETY: GPUI's window handle is its live NSView, used on the main thread.
    let view: &NSView = unsafe { view.cast().as_ref() };
    let window = view.window().ok_or("the view is in no window")?;
    let code = key_code(&keystroke.key).expect("parsed keys have a code");
    let held = keystroke.modifiers;
    let flags = [
        (held.platform, NSEventModifierFlags::Command),
        (held.control, NSEventModifierFlags::Control),
        (held.alt, NSEventModifierFlags::Option),
        (held.shift, NSEventModifierFlags::Shift),
    ];
    let flags = flags
        .into_iter()
        .filter(|(held, _)| *held)
        .fold(NSEventModifierFlags::empty(), |flags, (_, flag)| flags | flag);
    let text = keystroke.key_char.clone().unwrap_or_else(|| match keystroke.key.as_str() {
        key if key.chars().count() == 1 && held.shift => key.to_uppercase(),
        key if key.chars().count() == 1 => key.to_owned(),
        _ => String::new(),
    });
    let text = NSString::from_str(&text);
    let event = |kind| {
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind, NSPoint::ZERO, flags, 0., window.windowNumber(), None, &text, &text, false, code,
        )
        .ok_or("AppKit made no key event")
    };
    let down = event(NSEventType::KeyDown)?;
    if !((held.platform || held.control) && window.performKeyEquivalent(&down)) {
        window.sendEvent(&down);
    }
    let up = event(NSEventType::KeyUp)?;
    window.sendEvent(&up);
    Ok(())
}

/// The key's virtual key code on a US keyboard.
fn key_code(key: &str) -> Option<u16> {
    // Codes 0 to 50, by the character the key types; return and tab stand in as \n and \t.
    const CHARACTERS: &str = "asdfhgzxcv§bqweryt123465=97-80]ou[ip\nlj'k;\\,/nm.\t `";
    let named = match key {
        "enter" => Some(36),
        "tab" => Some(48),
        "space" => Some(49),
        "backspace" => Some(51),
        "escape" => Some(53),
        "left" => Some(123),
        "right" => Some(124),
        "down" => Some(125),
        "up" => Some(126),
        _ => None,
    };
    let mut chars = key.chars();
    let single = chars.next().filter(|_| chars.next().is_none());
    named.or_else(|| CHARACTERS.chars().position(|c| Some(c) == single).map(|code| code as u16))
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
        assert!(parse("bar wave").unwrap_err().contains("bar click|hover"));
        assert!(parse("wiggle 1 2").unwrap_err().starts_with("script line 1: unknown step"));
        assert!(parse("quit\nclick 1").unwrap_err().starts_with("script line 2: numbers expected"));
        assert!(parse("keys a-b").unwrap_err().contains("unknown keystroke"));
        let appkit = parse("nskey cmd-shift-d cmd-f enter").unwrap();
        assert!(matches!(&appkit[0].1, Step::NsKeys(keys) if keys.len() == 3));
        assert!(parse("nskey f13").unwrap_err().contains("a key a US keyboard has"));
        assert_eq!([key_code("d"), key_code("f"), key_code("t")], [Some(2), Some(3), Some(17)]);
        assert_eq!(
            [key_code("`"), key_code("enter"), key_code("up")],
            [Some(50), Some(36), Some(126)]
        );
    }
}
