//! Mouse reports for programs that asked for them (DEC modes 9, 1000, 1002, 1003), in the
//! encoding they chose (default, 1005 UTF-8, 1006 SGR, 1015 urxvt, 1016 SGR pixels).

use rtty_vt::{MouseEncoding, MouseMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    /// Motion with no button held.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Press,
    Release,
    Motion,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

/// One mouse event over the cell at 0-based `(row, col)`, or the pixel `(x, y)` from the pane's
/// top left for SGR pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub button: Button,
    pub action: Action,
    pub modifiers: Modifiers,
    pub cell: (u16, u16),
    pub pixel: (u32, u32),
}

/// Whether a program in `mode` wants to hear about `report`.
pub fn wanted(mode: MouseMode, report: &Report) -> bool {
    let wheel = matches!(report.button, Button::WheelUp | Button::WheelDown);
    match (mode, report.action) {
        (MouseMode::None, _) => false,
        (MouseMode::X10, action) => action == Action::Press,
        (_, Action::Press) => true,
        (_, Action::Release) => !wheel,
        (MouseMode::Click, Action::Motion) => false,
        (MouseMode::Drag, Action::Motion) => report.button != Button::None,
        (MouseMode::Motion, Action::Motion) => true,
    }
}

/// The bytes that report `report`; `None` where the encoding cannot express the cell.
pub fn encode(report: &Report, mode: MouseMode, encoding: MouseEncoding) -> Option<Vec<u8>> {
    let sgr = matches!(encoding, MouseEncoding::Sgr | MouseEncoding::SgrPixels);
    let base = match report.button {
        Button::Left => 0,
        Button::Middle => 1,
        Button::Right => 2,
        Button::None => 3,
        Button::WheelUp => 64,
        Button::WheelDown => 65,
    };
    // Only SGR says which button was released; the others report "a button went up".
    let mut code = if report.action == Action::Release && !sgr { 3 } else { base };
    if mode != MouseMode::X10 {
        let Modifiers { shift, alt, control } = report.modifiers;
        code += 4 * u32::from(shift) + 8 * u32::from(alt) + 16 * u32::from(control);
    }
    if report.action == Action::Motion {
        code += 32;
    }
    let (row, col) = (u32::from(report.cell.0) + 1, u32::from(report.cell.1) + 1);
    let end = if report.action == Action::Release { 'm' } else { 'M' };
    Some(match encoding {
        MouseEncoding::Sgr => format!("\x1b[<{code};{col};{row}{end}").into_bytes(),
        MouseEncoding::SgrPixels => {
            let (x, y) = (report.pixel.0 + 1, report.pixel.1 + 1);
            format!("\x1b[<{code};{x};{y}{end}").into_bytes()
        }
        MouseEncoding::Urxvt => format!("\x1b[{};{col};{row}M", code + 32).into_bytes(),
        MouseEncoding::Default => {
            let byte = |n: u32| u8::try_from(n + 32).ok();
            vec![0x1b, b'[', b'M', byte(code)?, byte(col)?, byte(row)?]
        }
        MouseEncoding::Utf8 => {
            let mut bytes = b"\x1b[M".to_vec();
            for n in [code, col, row] {
                let c = char::from_u32(n + 32).filter(|_| n + 32 < 0x800)?;
                bytes.extend(c.encode_utf8(&mut [0; 4]).as_bytes());
            }
            bytes
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(button: Button, action: Action, row: u16, col: u16) -> Report {
        Report { button, action, modifiers: Modifiers::default(), cell: (row, col), pixel: (0, 0) }
    }

    #[test]
    fn modes_choose_which_events_are_reported() {
        let press = report(Button::Left, Action::Press, 0, 0);
        let release = report(Button::Left, Action::Release, 0, 0);
        let drag = report(Button::Left, Action::Motion, 0, 0);
        let hover = report(Button::None, Action::Motion, 0, 0);
        let wheel_up = report(Button::WheelUp, Action::Press, 0, 0);
        let all = [press, release, drag, hover, wheel_up];
        let reported = |mode| all.iter().map(|report| wanted(mode, report)).collect::<Vec<_>>();
        assert_eq!(reported(MouseMode::None), [false; 5]);
        assert_eq!(reported(MouseMode::X10), [true, false, false, false, true]);
        assert_eq!(reported(MouseMode::Click), [true, true, false, false, true]);
        assert_eq!(reported(MouseMode::Drag), [true, true, true, false, true]);
        assert_eq!(reported(MouseMode::Motion), [true; 5]);
        assert!(!wanted(MouseMode::Click, &report(Button::WheelUp, Action::Release, 0, 0)));
    }

    #[test]
    fn sgr_names_the_button_released_and_carries_modifiers() {
        let mode = MouseMode::Drag;
        let encode = |report| String::from_utf8(encode(&report, mode, MouseEncoding::Sgr).unwrap());
        assert_eq!(encode(report(Button::Left, Action::Press, 4, 9)).unwrap(), "\x1b[<0;10;5M");
        assert_eq!(encode(report(Button::Right, Action::Release, 4, 9)).unwrap(), "\x1b[<2;10;5m");
        assert_eq!(encode(report(Button::Left, Action::Motion, 0, 0)).unwrap(), "\x1b[<32;1;1M");
        assert_eq!(
            encode(report(Button::WheelDown, Action::Press, 0, 0)).unwrap(),
            "\x1b[<65;1;1M"
        );
        let mut chord = report(Button::Middle, Action::Press, 0, 0);
        chord.modifiers = Modifiers { shift: true, alt: true, control: true };
        assert_eq!(encode(chord).unwrap(), "\x1b[<29;1;1M");
    }

    #[test]
    fn the_default_encoding_offsets_by_32_and_gives_up_past_column_223() {
        let mode = MouseMode::Click;
        let press = report(Button::Left, Action::Press, 1, 2);
        assert_eq!(encode(&press, mode, MouseEncoding::Default).unwrap(), b"\x1b[M #\"");
        let release = report(Button::Left, Action::Release, 1, 2);
        assert_eq!(encode(&release, mode, MouseEncoding::Default).unwrap(), b"\x1b[M##\"");
        let far = report(Button::Left, Action::Press, 0, 300);
        assert_eq!(encode(&far, mode, MouseEncoding::Default), None);
        assert_eq!(encode(&far, mode, MouseEncoding::Utf8).unwrap(), "\x1b[M \u{14d}!".as_bytes());
    }

    #[test]
    fn x10_reports_no_modifiers_and_urxvt_and_pixels_have_their_own_forms() {
        let mut press = report(Button::Left, Action::Press, 1, 2);
        press.modifiers.control = true;
        press.pixel = (17, 40);
        assert_eq!(encode(&press, MouseMode::X10, MouseEncoding::Default).unwrap(), b"\x1b[M #\"");
        let urxvt = encode(&press, MouseMode::Click, MouseEncoding::Urxvt).unwrap();
        assert_eq!(urxvt, b"\x1b[48;3;2M");
        let pixels = encode(&press, MouseMode::Click, MouseEncoding::SgrPixels).unwrap();
        assert_eq!(pixels, b"\x1b[<16;18;41M");
    }
}
