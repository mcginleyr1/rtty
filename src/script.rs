//! `--script FILE`: drives the real window as a person would, to check rtty against real programs
//! with nobody at the keyboard and no screenshots. Keys go through GPUI's own keystroke dispatch
//! (key bindings, the key encoder, the text input handler), mouse events to the pane's handlers,
//! focus through GPUI's focus, and `dump` writes what the next frame draws. One step per line;
//! `#` starts a comment.
//!
//! ```text
//! wait TEXT              until the screen shows TEXT (10 s at most)
//! sleep MS
//! keys KEYSTROKE...      as gpui parses them: ctrl-a escape enter shift-tab cmd-c ...
//! type TEXT              the rest of the line, one keystroke per character
//! click ROW COL [left|middle|right] [CLICKS]
//! drag ROW COL ROW COL   with the left button
//! move ROW COL           with no button
//! wheel ROW COL LINES    positive scrolls up
//! focus in|out           the keyboard comes to the pane or leaves the window's elements
//! appearance dark|light  what the window does when macOS flips
//! resize ROWS COLS       the window, to fit that many cells
//! dump FILE
//! quit
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    AsyncWindowContext, Context, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ScrollDelta, ScrollWheelEvent, TouchPhase, WeakEntity, Window, WindowAppearance,
    point,
};

use crate::window::TerminalWindow;

#[derive(Debug)]
pub enum Step {
    Wait(String),
    Sleep(Duration),
    Keys(Vec<Keystroke>),
    Type(String),
    Click { row: u16, col: u16, button: MouseButton, clicks: usize },
    Drag { from: (u16, u16), to: (u16, u16) },
    Move { row: u16, col: u16 },
    Wheel { row: u16, col: u16, lines: f32 },
    Focus(bool),
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
        let args: Vec<&str> = rest.split_whitespace().collect();
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
            "keys" => Step::Keys(
                args.iter()
                    .map(|key| Keystroke::parse(key).map_err(|_| error("unknown keystroke")))
                    .collect::<Result<_, _>>()?,
            ),
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
                Step::Click { row: cell[0], col: cell[1], button, clicks }
            }
            "drag" => {
                let cells = numbers(4)?;
                Step::Drag { from: (cells[0], cells[1]), to: (cells[2], cells[3]) }
            }
            "move" => {
                let cell = numbers(2)?;
                Step::Move { row: cell[0], col: cell[1] }
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

/// Runs the steps against the window's active pane, after its first frame. A step that fails
/// ends rtty with exit status 1.
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
    let pane = this.update(cx, |window, _| window.active()).map_err(|err| err.to_string())?;
    let pane = pane.ok_or("no pane")?;
    // The pane's first frame measures the cells that positions are made from.
    while pane
        .update(cx, |pane, _| pane.cell_center(0, 0))
        .map_err(|err| err.to_string())?
        .is_none()
    {
        cx.background_executor().timer(Duration::from_millis(10)).await;
    }
    let center = |row: u16, col: u16, cx: &mut AsyncWindowContext| {
        pane.update(cx, |pane, _| pane.cell_center(row, col))
            .ok()
            .flatten()
            .ok_or(format!("no cell {row},{col}"))
    };
    // gpui 0.2.2 keeps `Window::dispatch_event`'s result type private, so mouse events go to the
    // pane's own handlers, which is where the window's hit testing would send them.
    let dispatch = |event: Mouse, cx: &mut AsyncWindowContext| {
        pane.update_in(cx, |pane, window, cx| match event {
            Mouse::Down(event) => pane.mouse_down(&event, window, cx),
            Mouse::Up(event) => pane.mouse_up(&event, window, cx),
            Mouse::Move(event) => pane.mouse_move(&event, window, cx),
            Mouse::Wheel(event) => pane.scroll_wheel(&event, window, cx),
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
                cx.update(|window, cx| window.dispatch_keystroke(keystroke.clone(), cx))
                    .map_err(|err| err.to_string())?;
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
                cx.update(|window, cx| window.dispatch_keystroke(keystroke, cx))
                    .map_err(|err| err.to_string())?;
            }
        }
        Step::Click { row, col, button, clicks } => {
            let position = center(*row, *col, cx)?;
            for click_count in 1..=*clicks {
                let button = *button;
                let modifiers = none;
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
        Step::Move { row, col } => {
            let position = center(*row, *col, cx)?;
            let motion = MouseMoveEvent { position, pressed_button: None, modifiers: none };
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
        Step::Appearance(appearance) => {
            pane.update(cx, |pane, cx| pane.follow_appearance(*appearance, cx))
                .map_err(|err| err.to_string())?;
        }
        Step::Resize { rows, cols } => {
            let size = pane.update(cx, |pane, _| pane.size_for(*rows, *cols)).ok().flatten();
            let size = size.ok_or("no frame yet")?;
            cx.update(|window, _| window.resize(size)).map_err(|err| err.to_string())?;
        }
        Step::Dump(path) => {
            let dump = cx
                .update(|window, cx| pane.read(cx).dump(window))
                .map_err(|err| err.to_string())?;
            std::fs::write(path, dump).map_err(|err| format!("{}: {err}", path.display()))?;
        }
        Step::Quit => cx.update(|_, cx| cx.quit()).map_err(|err| err.to_string())?,
    }
    Ok(())
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
            Step::Click { row: 1, col: 2, button: MouseButton::Right, clicks: 2 }
        ));
        assert!(parse("wiggle 1 2").unwrap_err().starts_with("script line 1: unknown step"));
        assert!(parse("quit\nclick 1").unwrap_err().starts_with("script line 2: numbers expected"));
        assert!(parse("keys a-b").unwrap_err().contains("unknown keystroke"));
    }
}
