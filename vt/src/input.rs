//! Keys and the mouse as the bytes a terminal sends its program, encoded as the program asked:
//! legacy xterm keys, xterm's modifyOtherKeys and kitty's keyboard protocol; mouse reports in
//! the mode and encoding it chose. A host turns its own events into `Key` and `Mouse` and reads
//! the program's asking off its `Terminal` (`modes()`, `kitty_keyboard()`).

use crate::modes::{Modes, MouseEncoding, MouseMode};

/// Kitty's modifier bits, which `Key::mods` and `Mouse::mods` hold; xterm's modifier parameter is
/// one more than the first three.
pub const SHIFT: u8 = 1;
pub const ALT: u8 = 2;
pub const CTRL: u8 = 4;
pub const SUPER: u8 = 8;
pub const HYPER: u8 = 16;
pub const META: u8 = 32;
pub const CAPS_LOCK: u8 = 64;
pub const NUM_LOCK: u8 = 128;

/// Kitty's keyboard flags: escape codes for what legacy keys leave ambiguous, repeats and
/// releases, a key's alternates, every key an escape code, and the text a key typed.
pub const DISAMBIGUATE: u8 = 1;
pub const EVENT_TYPES: u8 = 2;
pub const ALTERNATE_KEYS: u8 = 4;
pub const ALL_KEYS: u8 = 8;
pub const ASSOCIATED_TEXT: u8 = 16;

/// A key going down, repeating while held, or coming up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Press,
    Repeat,
    Release,
}

/// A key as the legacy encodings know it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Code {
    /// A key that types a character, by that character unshifted (`a` for Shift+A).
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    /// F1 to F12; the others only by `Key::number`.
    F(u8),
    /// A modifier key on its own, by `Key::number`.
    Modifier,
    /// Any other key (a media key, Caps Lock), by `Key::number`.
    #[default]
    Other,
}

/// A key event. A host fills in `code`, `mods` and `kind`; the rest is what its own terminal
/// said of the key in kitty's protocol, for the program to hear as far as it asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Key<'a> {
    pub code: Code,
    /// The modifiers held and the locks on, in kitty's bits.
    pub mods: u8,
    pub kind: Kind,
    /// The text the key typed (an input method's, Caps Lock's capital); empty for the character
    /// of a key with no modifier but Shift, shifted.
    pub text: &'a str,
    /// Kitty's number for the key when it is not `code`'s: a keypad key's, a layout's own letter,
    /// Left Shift's, 0 for text with no key.
    pub number: Option<u32>,
    /// Kitty's alternates: the shifted key (0 for `code`'s character, shifted) and the key in
    /// the base layout (0 for none).
    pub shifted: u32,
    pub base: u32,
}

/// The bytes `key` sends to a program that asked for its keys as `modes` (DECCKM, DECKPAM,
/// modifyOtherKeys) and kitty's `flags` say, or `None` when it sends none: a release or a lone
/// modifier the program did not ask to hear, a key with no encoding.
pub fn encode_key(key: &Key, modes: &Modes, flags: u8) -> Option<Vec<u8>> {
    if flags & (DISAMBIGUATE | ALL_KEYS) != 0 {
        return kitty(key, flags);
    }
    (key.kind != Kind::Release).then(|| legacy(key, modes)).flatten()
}

/// xterm's encodings: control characters, an ESC before Alt's keys, `CSI 1 ; m X` and
/// `CSI n ; m ~` with modifier parameters, and `CSI 27 ; m ; code ~` for what modifyOtherKeys
/// asks for. Level 2 spells out every modified key but Shift with text; level 1 only Ctrl with
/// a key that has no control character, and Enter and Tab with Ctrl or Shift: Emacs, which
/// turns level 1 on, reads Alt's ESC and the control characters, not their spelled-out forms.
/// Under DECKPAM, unless Num Lock overrides it, the keypad's digits, operators and Enter send
/// `SS3 [m] X` as xterm's PC keyboard does; otherwise they are their main keys, modifyOtherKeys
/// leaving them alone. Its Begin key is a cursor key, `E`.
fn legacy(key: &Key, modes: &Modes) -> Option<Vec<u8>> {
    let [shift, alt, ctrl] = [SHIFT, ALT, CTRL].map(|bit| key.mods & bit != 0);
    let modifier = 1 + (key.mods & (SHIFT | ALT | CTRL));
    let params = if modifier > 1 { modifier.to_string() } else { String::new() };
    // Kitty's KP_0 to KP_9, KP_DECIMAL, _DIVIDE, _MULTIPLY, _SUBTRACT, _ADD, _ENTER, _EQUAL and
    // _SEPARATOR, by their final bytes in application keypad mode.
    let keypad = key.number.and_then(|n| b"pqrstuvwxynojmkMXl".get(n.checked_sub(57399)? as usize));
    if let Some(&last) = keypad.filter(|_| modes.keypad && key.mods & NUM_LOCK == 0) {
        return Some(format!("\x1bO{params}{}", char::from(last)).into_bytes());
    }
    let level = if keypad.is_some() { 0 } else { modes.modify_other_keys };
    let esc = |bytes: &[u8]| [if alt { &b"\x1b"[..] } else { b"" }, bytes].concat();
    let other = |code: u32| format!("\x1b[27;{modifier};{code}~").into_bytes();
    let spelled = level >= 2 && modifier > 1;
    let enter_tab = spelled || level == 1 && (ctrl || shift);
    Some(match key.code {
        Code::Enter if enter_tab => other(13),
        Code::Enter => esc(b"\r"),
        Code::Tab if shift && !(ctrl && enter_tab) => b"\x1b[Z".to_vec(),
        Code::Tab if enter_tab => other(9),
        Code::Tab => esc(b"\t"),
        Code::Backspace if spelled => other(127),
        Code::Backspace if ctrl => esc(b"\x08"),
        Code::Backspace => esc(b"\x7f"),
        Code::Escape if spelled => other(27),
        Code::Escape => esc(b"\x1b"),
        Code::Char(c) => {
            let shifted = if shift { c.to_ascii_uppercase() } else { c };
            let control = ctrl.then(|| control_byte(c)).flatten();
            let spelled = match level {
                2.. => ctrl || alt,
                1 => ctrl && control.is_none(),
                _ => false,
            };
            match control {
                _ if spelled => other(u32::from(shifted)),
                Some(byte) => esc(&[byte]),
                None if ctrl || alt || key.text.is_empty() => {
                    esc(shifted.encode_utf8(&mut [0; 4]).as_bytes())
                }
                None => key.text.as_bytes().to_vec(),
            }
        }
        code => {
            let (number, last) = match key.number {
                Some(57427) => (1, 'E'),
                _ => csi_form(code, false)?,
            };
            let ss3 =
                matches!(last, 'P'..='S') || modes.cursor_keys && matches!(last, 'A'..='F' | 'H');
            csi(&number.to_string(), last, &params, ss3)
        }
    })
}

/// Kitty's protocol: `CSI code[:shifted[:base]] ; modifiers[:event] [; text] u`, and
/// `CSI 1 ; m X` and `CSI n ; m ~` for the functional keys (the keypad's Begin, KP_BEGIN, among
/// them as `CSI 1 ; m E`), never `SS3`; DECKPAM changes nothing. Unless every key is to
/// be an escape code (8), a text key with no modifier but Shift types its text, an unmodified
/// Enter, Tab or Backspace sends its legacy byte (their releases then nothing), a lone modifier
/// nothing, and the locks are left out. The event type goes only under 2 (a repeat otherwise a
/// press, a release nothing), the alternates only under 4, the text only under 16 with 8.
fn kitty(key: &Key, flags: u8) -> Option<Vec<u8>> {
    let all = flags & ALL_KEYS != 0;
    let event = match (key.kind, flags & EVENT_TYPES != 0) {
        (Kind::Release, false) => return None,
        (Kind::Repeat, true) => ":2",
        (Kind::Release, true) => ":3",
        _ => "",
    };
    let (number, last) = match key.number {
        Some(57427) => (1, 'E'),
        Some(number) => (number, 'u'),
        None => csi_form(key.code, true)?,
    };
    let mods = key.mods & if all { u8::MAX } else { !(CAPS_LOCK | NUM_LOCK) };
    let shift = key.mods & SHIFT != 0;
    let text_key =
        matches!(key.code, Code::Char(_)) && key.mods & (ALT | CTRL | SUPER | HYPER | META) == 0;
    let typed = match key.code {
        _ if !key.text.is_empty() => key.text.to_string(),
        Code::Char(c) if text_key && shift => c.to_ascii_uppercase().to_string(),
        Code::Char(c) if text_key => c.to_string(),
        _ => String::new(),
    };
    if !all {
        let sends = key.kind != Kind::Release;
        match key.code {
            Code::Modifier => return None,
            _ if text_key => return sends.then(|| typed.into_bytes()),
            _ if last == 'u' && matches!(number, 9 | 13 | 127) && mods == 0 => {
                return sends.then(|| vec![number as u8]);
            }
            _ => {}
        }
    }
    let shifted = match (key.shifted, key.code) {
        (0, Code::Char(c)) => u32::from(c.to_ascii_uppercase()),
        (shifted, _) => shifted,
    };
    let shifted = match shifted {
        0 => String::new(),
        shifted if !shift || shifted == number => String::new(),
        shifted => shifted.to_string(),
    };
    let alternates = match (flags & ALTERNATE_KEYS != 0, key.base) {
        (false, _) => String::new(),
        (true, 0) if shifted.is_empty() => String::new(),
        (true, 0) => format!(":{shifted}"),
        (true, base) => format!(":{shifted}:{base}"),
    };
    let text: Vec<String> =
        typed.chars().filter(|c| !c.is_control()).map(|c| u32::from(c).to_string()).collect();
    let text = match all && flags & ASSOCIATED_TEXT != 0 && key.kind != Kind::Release {
        true if !text.is_empty() => format!(";{}", text.join(":")),
        _ => String::new(),
    };
    let modifiers = match (mods, event) {
        (0, "") => String::new(),
        (mods, event) => format!("{}{event}", 1 + u32::from(mods)),
    };
    Some(csi(&format!("{number}{alternates}"), last, &format!("{modifiers}{text}"), false))
}

/// A key's number and final byte in its CSI form: `CSI number u` for the keys kitty numbers by
/// their character, `CSI 1 X` and `CSI n ~` for the rest. F3 is `CSI 13 ~` in kitty's protocol,
/// where `CSI R` would read as a cursor position report.
fn csi_form(code: Code, kitty: bool) -> Option<(u32, char)> {
    Some(match code {
        Code::Char(c) => (u32::from(c), 'u'),
        Code::Escape => (27, 'u'),
        Code::Enter => (13, 'u'),
        Code::Tab => (9, 'u'),
        Code::Backspace => (127, 'u'),
        Code::Insert => (2, '~'),
        Code::Delete => (3, '~'),
        Code::PageUp => (5, '~'),
        Code::PageDown => (6, '~'),
        Code::Up => (1, 'A'),
        Code::Down => (1, 'B'),
        Code::Right => (1, 'C'),
        Code::Left => (1, 'D'),
        Code::Home => (1, 'H'),
        Code::End => (1, 'F'),
        Code::F(3) if kitty => (13, '~'),
        Code::F(n @ 1..=4) => (1, char::from(b'P' + n - 1)),
        Code::F(n @ 5..=12) => ([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)], '~'),
        _ => return None,
    })
}

/// `CSI number ; params final`, as short as the forms allow: a letter's `1` goes when nothing
/// follows it (`SS3 final` instead when `ss3`), and so does any key's `;`.
fn csi(number: &str, last: char, params: &str, ss3: bool) -> Vec<u8> {
    match (last, params) {
        ('A'..='Z', "") if ss3 => format!("\x1bO{last}"),
        ('A'..='Z', "") => format!("\x1b[{last}"),
        (_, "") => format!("\x1b[{number}{last}"),
        (_, params) => format!("\x1b[{number};{params}{last}"),
    }
    .into_bytes()
}

/// The control character Ctrl makes of a key, as xterm and kitty's table have it, the VT220's
/// for the digits (Ctrl+3 is ESC) included.
fn control_byte(c: char) -> Option<u8> {
    match c {
        '@'..='_' | 'a'..='z' => Some(c as u8 & 0x1f),
        ' ' | '2' => Some(0),
        '3'..='7' => Some(c as u8 - b'3' + 0x1b),
        '-' | '/' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

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

/// One mouse event over the cell at 0-based `(row, col)`, or the pixel `(x, y)` from the
/// grid's top left for SGR pixels; a host that knows no pixels gives the cell as `(col, row)`,
/// and SGR pixels then reports cells as SGR does. `mods` are Shift, Alt and Ctrl's bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mouse {
    pub button: Button,
    pub action: Action,
    pub mods: u8,
    pub cell: (u16, u16),
    pub pixel: (u32, u32),
}

/// The bytes that report `mouse` to a program that asked for it with `modes`' mouse mode (9,
/// 1000, 1002, 1003) and in its encoding (default, 1005 UTF-8, 1006 SGR, 1015 urxvt, 1016 SGR
/// pixels), or `None` when the mode leaves it out. Only SGR says which button came up; X10
/// reports no modifiers; a cell past what the one-byte encodings can say goes as the farthest
/// they can (223, or 2015 in UTF-8), as xterm clamps it.
pub fn encode_mouse(mouse: &Mouse, modes: &Modes) -> Option<Vec<u8>> {
    let wheel = matches!(mouse.button, Button::WheelUp | Button::WheelDown);
    let wanted = match (modes.mouse, mouse.action) {
        (MouseMode::None, _) => false,
        (MouseMode::X10, action) => action == Action::Press,
        (_, Action::Press) => true,
        (_, Action::Release) => !wheel,
        (MouseMode::Click, Action::Motion) => false,
        (MouseMode::Drag, Action::Motion) => mouse.button != Button::None,
        (MouseMode::Motion, Action::Motion) => true,
    };
    if !wanted {
        return None;
    }
    let encoding = modes.mouse_encoding;
    let sgr = matches!(encoding, MouseEncoding::Sgr | MouseEncoding::SgrPixels);
    let release = mouse.action == Action::Release;
    let mut code = match mouse.button {
        _ if release && !sgr => 3,
        Button::Left => 0,
        Button::Middle => 1,
        Button::Right => 2,
        Button::None => 3,
        Button::WheelUp => 64,
        Button::WheelDown => 65,
    };
    if modes.mouse != MouseMode::X10 {
        code += 4 * u32::from(mouse.mods & (SHIFT | ALT | CTRL));
    }
    if mouse.action == Action::Motion {
        code += 32;
    }
    let (row, col) = (u32::from(mouse.cell.0) + 1, u32::from(mouse.cell.1) + 1);
    let end = if release { 'm' } else { 'M' };
    Some(match encoding {
        MouseEncoding::Sgr => format!("\x1b[<{code};{col};{row}{end}").into_bytes(),
        MouseEncoding::SgrPixels => {
            let (x, y) = (mouse.pixel.0 + 1, mouse.pixel.1 + 1);
            format!("\x1b[<{code};{x};{y}{end}").into_bytes()
        }
        MouseEncoding::Urxvt => format!("\x1b[{};{col};{row}M", code + 32).into_bytes(),
        MouseEncoding::Default => {
            let byte = |n: u32| 32 + n.min(223) as u8;
            vec![0x1b, b'[', b'M', byte(code), byte(col), byte(row)]
        }
        MouseEncoding::Utf8 => {
            let mut bytes = b"\x1b[M".to_vec();
            for n in [code, col, row] {
                let c = char::from_u32(32 + n.min(2015))?;
                bytes.extend(c.encode_utf8(&mut [0; 4]).as_bytes());
            }
            bytes
        }
    })
}

#[cfg(test)]
mod tests {
    use super::Code::Char;
    use super::*;

    fn key(code: Code, mods: u8) -> Key<'static> {
        Key { code, mods, ..Key::default() }
    }

    fn send(key: Key, modes: &Modes, flags: u8) -> Option<String> {
        encode_key(&key, modes, flags).map(|bytes| String::from_utf8(bytes).unwrap())
    }

    fn legacy(code: Code, mods: u8) -> Option<String> {
        send(key(code, mods), &Modes::default(), 0)
    }

    fn level(modify_other_keys: u8) -> Modes {
        Modes { modify_other_keys, ..Modes::default() }
    }

    fn kitty(flags: u8) -> impl Fn(Key) -> Option<String> {
        move |key| send(key, &Modes::default(), flags)
    }

    const NONE: u8 = 0;

    #[test]
    fn text_types_itself_shifted_or_as_the_host_says() {
        assert_eq!(legacy(Char('a'), NONE).unwrap(), "a");
        assert_eq!(legacy(Char('a'), SHIFT).unwrap(), "A");
        assert_eq!(legacy(Char('é'), NONE).unwrap(), "é");
        assert_eq!(legacy(Char(' '), SHIFT).unwrap(), " ");
        // Super has no legacy form: the key types as it would without.
        assert_eq!(legacy(Char('a'), SUPER).unwrap(), "a");
        let composed = Key { text: "e\u{301}", ..key(Char('e'), NONE) };
        assert_eq!(send(composed, &Modes::default(), 0).unwrap(), "e\u{301}");
    }

    #[test]
    fn editing_keys() {
        assert_eq!(legacy(Code::Enter, NONE).unwrap(), "\r");
        assert_eq!(legacy(Code::Backspace, NONE).unwrap(), "\x7f");
        assert_eq!(legacy(Code::Backspace, CTRL).unwrap(), "\x08");
        assert_eq!(legacy(Code::Tab, NONE).unwrap(), "\t");
        assert_eq!(legacy(Code::Tab, SHIFT).unwrap(), "\x1b[Z");
        assert_eq!(legacy(Code::Escape, NONE).unwrap(), "\x1b");
        // Kitty's table of legacy keys: Ctrl with Enter and Tab changes nothing, Ctrl+Shift+Tab
        // is Shift+Tab, and Alt is an ESC before any of them.
        assert_eq!(legacy(Code::Enter, CTRL).unwrap(), "\r");
        assert_eq!(legacy(Code::Enter, SHIFT).unwrap(), "\r");
        assert_eq!(legacy(Code::Tab, CTRL).unwrap(), "\t");
        assert_eq!(legacy(Code::Tab, CTRL | SHIFT).unwrap(), "\x1b[Z");
        assert_eq!(legacy(Code::Enter, ALT).unwrap(), "\x1b\r");
        assert_eq!(legacy(Code::Tab, ALT).unwrap(), "\x1b\t");
        assert_eq!(legacy(Code::Escape, ALT).unwrap(), "\x1b\x1b");
        assert_eq!(legacy(Code::Backspace, ALT).unwrap(), "\x1b\x7f");
        assert_eq!(legacy(Code::Backspace, CTRL | ALT).unwrap(), "\x1b\x08");
        assert_eq!(legacy(Code::Tab, ALT | SHIFT).unwrap(), "\x1b[Z");
    }

    #[test]
    fn control_chords() {
        for (c, byte) in [('a', 1), ('c', 3), ('[', 0x1b), ('\\', 0x1c), (']', 0x1d)] {
            assert_eq!(legacy(Char(c), CTRL).unwrap().as_bytes(), [byte], "{c}");
        }
        for (c, byte) in [('^', 0x1e), ('_', 0x1f), (' ', 0), ('@', 0), ('?', 0x7f)] {
            assert_eq!(legacy(Char(c), CTRL).unwrap().as_bytes(), [byte], "{c}");
        }
        // The VT220's digits, as kitty's table has them, and Ctrl+- and Ctrl+/ as Ctrl+_.
        for (c, byte) in [('2', 0), ('3', 0x1b), ('4', 0x1c), ('7', 0x1f), ('8', 0x7f)] {
            assert_eq!(legacy(Char(c), CTRL).unwrap().as_bytes(), [byte], "{c}");
        }
        assert_eq!(legacy(Char('-'), CTRL).unwrap(), "\x1f");
        assert_eq!(legacy(Char('/'), CTRL).unwrap(), "\x1f");
        assert_eq!(legacy(Char('c'), CTRL | SHIFT).unwrap(), "\x03");
        // Ctrl leaves a key it has no control character for untouched, as kitty's table says.
        assert_eq!(legacy(Char('1'), CTRL).unwrap(), "1");
        assert_eq!(legacy(Char(';'), CTRL).unwrap(), ";");
    }

    #[test]
    fn alt_is_an_escape_prefix() {
        assert_eq!(legacy(Char('b'), ALT).unwrap(), "\x1bb");
        assert_eq!(legacy(Char('b'), ALT | SHIFT).unwrap(), "\x1bB");
        assert_eq!(legacy(Char('a'), ALT | CTRL).unwrap(), "\x1b\x01");
        assert_eq!(legacy(Char('1'), ALT | CTRL).unwrap(), "\x1b1");
    }

    #[test]
    fn cursor_keys_follow_decckm_and_modifiers() {
        assert_eq!(legacy(Code::Up, NONE).unwrap(), "\x1b[A");
        assert_eq!(legacy(Code::Left, NONE).unwrap(), "\x1b[D");
        let decckm = Modes { cursor_keys: true, ..Modes::default() };
        let app = |code, mods| send(key(code, mods), &decckm, 0).unwrap();
        assert_eq!(app(Code::Up, NONE), "\x1bOA");
        assert_eq!(app(Code::End, NONE), "\x1bOF");
        assert_eq!(app(Code::Up, CTRL), "\x1b[1;5A");
        assert_eq!(app(Code::PageUp, NONE), "\x1b[5~");
        assert_eq!(legacy(Code::Left, SHIFT).unwrap(), "\x1b[1;2D");
        assert_eq!(legacy(Code::Right, CTRL).unwrap(), "\x1b[1;5C");
        assert_eq!(legacy(Code::Left, ALT).unwrap(), "\x1b[1;3D");
        assert_eq!(legacy(Code::Up, SUPER).unwrap(), "\x1b[A");
        assert_eq!(legacy(Code::Home, NONE).unwrap(), "\x1b[H");
        assert_eq!(legacy(Code::End, NONE).unwrap(), "\x1b[F");
    }

    #[test]
    fn navigation_and_function_keys() {
        assert_eq!(legacy(Code::PageUp, NONE).unwrap(), "\x1b[5~");
        assert_eq!(legacy(Code::PageDown, NONE).unwrap(), "\x1b[6~");
        assert_eq!(legacy(Code::Insert, NONE).unwrap(), "\x1b[2~");
        assert_eq!(legacy(Code::Delete, NONE).unwrap(), "\x1b[3~");
        assert_eq!(legacy(Code::Delete, SHIFT).unwrap(), "\x1b[3;2~");
        assert_eq!(legacy(Code::Delete, CTRL).unwrap(), "\x1b[3;5~");
        assert_eq!(legacy(Code::F(1), NONE).unwrap(), "\x1bOP");
        assert_eq!(legacy(Code::F(3), NONE).unwrap(), "\x1bOR");
        assert_eq!(legacy(Code::F(4), NONE).unwrap(), "\x1bOS");
        assert_eq!(legacy(Code::F(1), CTRL).unwrap(), "\x1b[1;5P");
        assert_eq!(legacy(Code::F(5), NONE).unwrap(), "\x1b[15~");
        assert_eq!(legacy(Code::F(12), NONE).unwrap(), "\x1b[24~");
        assert_eq!(legacy(Code::F(20), NONE), None);
        assert_eq!(legacy(Code::Other, NONE), None);
        assert_eq!(legacy(Code::Modifier, SHIFT), None);
    }

    #[test]
    fn releases_send_nothing_without_kitty_event_types() {
        let ctrl_a = |kind| Key { kind, ..key(Char('a'), CTRL) };
        assert_eq!(send(ctrl_a(Kind::Release), &Modes::default(), 0), None);
        assert_eq!(send(ctrl_a(Kind::Repeat), &Modes::default(), 0).unwrap(), "\x01");
    }

    /// Emacs turns on level 1 when DA2 says xterm 242 or later. Probed with C-h c in
    /// `emacs -nw -Q` (32.0.50): it reads `CSI 27 ; 5 ; 49 ~` as C-1, `CSI 27 ; 6 ; 9 ~` as
    /// C-S-<tab> and `CSI 27 ; 6 ; 33 ~` as C-!, but none of the spelled-out C-S-a, C-3, M-RET,
    /// M-TAB, C-<backspace>, M-DEL or modified Esc, which it reads as M-[ and stray keys.
    #[test]
    fn modify_other_keys_level_1_covers_what_legacy_cannot_say() {
        let one = |code, mods| send(key(code, mods), &level(1), 0).unwrap();
        assert_eq!(one(Char('a'), CTRL), "\x01");
        assert_eq!(one(Char('i'), CTRL), "\x09");
        assert_eq!(one(Char('['), CTRL), "\x1b");
        assert_eq!(one(Char(';'), CTRL), "\x1b[27;5;59~");
        assert_eq!(one(Char('.'), CTRL), "\x1b[27;5;46~");
        assert_eq!(one(Char('1'), CTRL), "\x1b[27;5;49~");
        assert_eq!(one(Char('1'), CTRL | SHIFT), "\x1b[27;6;49~");
        assert_eq!(one(Char('3'), CTRL), "\x1b");
        assert_eq!(one(Char(' '), CTRL), "\x00");
        assert_eq!(one(Char('a'), CTRL | SHIFT), "\x01");
        assert_eq!(one(Code::Enter, CTRL), "\x1b[27;5;13~");
        assert_eq!(one(Code::Enter, SHIFT), "\x1b[27;2;13~");
        assert_eq!(one(Code::Tab, CTRL), "\x1b[27;5;9~");
        assert_eq!(one(Code::Tab, CTRL | SHIFT), "\x1b[27;6;9~");
        assert_eq!(one(Code::Tab, SHIFT), "\x1b[Z");
        assert_eq!(one(Code::Backspace, CTRL), "\x08");
        assert_eq!(one(Code::Backspace, SHIFT), "\x7f");
        assert_eq!(one(Code::Escape, CTRL), "\x1b");
        assert_eq!(one(Char('b'), ALT), "\x1bb");
        assert_eq!(one(Code::Enter, ALT), "\x1b\r");
        assert_eq!(one(Code::Tab, ALT), "\x1b\t");
        assert_eq!(one(Code::Escape, ALT), "\x1b\x1b");
        assert_eq!(one(Code::Up, CTRL), "\x1b[1;5A");
        assert_eq!(one(Char('a'), NONE), "a");
    }

    #[test]
    fn modify_other_keys_level_2_covers_every_modified_key_but_shifted_text() {
        let two = |code, mods| send(key(code, mods), &level(2), 0).unwrap();
        assert_eq!(two(Char('a'), CTRL), "\x1b[27;5;97~");
        assert_eq!(two(Char('i'), CTRL), "\x1b[27;5;105~");
        assert_eq!(two(Char('b'), ALT), "\x1b[27;3;98~");
        assert_eq!(two(Char('a'), CTRL | ALT), "\x1b[27;7;97~");
        assert_eq!(two(Char('a'), CTRL | SHIFT), "\x1b[27;6;65~");
        assert_eq!(two(Char('3'), CTRL), "\x1b[27;5;51~");
        assert_eq!(two(Char(' '), CTRL), "\x1b[27;5;32~");
        assert_eq!(two(Code::Enter, ALT), "\x1b[27;3;13~");
        assert_eq!(two(Code::Enter, SHIFT), "\x1b[27;2;13~");
        assert_eq!(two(Code::Backspace, CTRL), "\x1b[27;5;127~");
        assert_eq!(two(Code::Escape, CTRL), "\x1b[27;5;27~");
        assert_eq!(two(Code::Tab, CTRL | SHIFT), "\x1b[27;6;9~");
        assert_eq!(two(Code::Tab, SHIFT), "\x1b[Z");
        assert_eq!(two(Code::Tab, ALT | SHIFT), "\x1b[Z");
        assert_eq!(two(Char('a'), SHIFT), "A");
        assert_eq!(two(Char('a'), NONE), "a");
        assert_eq!(two(Char('a'), SUPER), "a");
        assert_eq!(two(Code::Up, NONE), "\x1b[A");
    }

    /// Kitty's flags win over modifyOtherKeys when a program set both.
    #[test]
    fn kitty_comes_before_modify_other_keys() {
        let ctrl_i = send(key(Char('i'), CTRL), &level(2), DISAMBIGUATE);
        assert_eq!(ctrl_i.unwrap(), "\x1b[105;5u");
    }

    /// nvim pushes flags 3 (disambiguate, event types) once `CSI ? u` is answered. The
    /// functional keys take their CSI forms whatever DECCKM says, F1 to F4 included: with
    /// disambiguate, "all key events that do not generate text" are `CSI number ; modifier u`
    /// or `CSI 1 ; modifier [~ABCDEFHPQS]`, the parameters left out when there are none.
    #[test]
    fn kitty_disambiguate_encodes_what_legacy_would_confuse() {
        let decckm = Modes { cursor_keys: true, ..Modes::default() };
        for modes in [Modes::default(), decckm] {
            let press = |code, mods| send(key(code, mods), &modes, DISAMBIGUATE);
            assert_eq!(press(Code::Escape, NONE).unwrap(), "\x1b[27u");
            assert_eq!(press(Char('a'), CTRL).unwrap(), "\x1b[97;5u");
            assert_eq!(press(Char('i'), CTRL).unwrap(), "\x1b[105;5u");
            assert_eq!(press(Char('a'), CTRL | SHIFT).unwrap(), "\x1b[97;6u");
            assert_eq!(press(Char('b'), ALT).unwrap(), "\x1b[98;3u");
            assert_eq!(press(Char('b'), ALT | SHIFT).unwrap(), "\x1b[98;4u");
            assert_eq!(press(Char(' '), CTRL).unwrap(), "\x1b[32;5u");
            assert_eq!(press(Char('s'), SUPER | CTRL).unwrap(), "\x1b[115;13u");
            assert_eq!(press(Char('w'), SUPER).unwrap(), "\x1b[119;9u");
            assert_eq!(press(Char('a'), NONE).unwrap(), "a");
            assert_eq!(press(Char('a'), SHIFT).unwrap(), "A");
            assert_eq!(press(Code::Enter, NONE).unwrap(), "\r");
            assert_eq!(press(Code::Tab, NONE).unwrap(), "\t");
            assert_eq!(press(Code::Backspace, NONE).unwrap(), "\x7f");
            assert_eq!(press(Code::Enter, SHIFT).unwrap(), "\x1b[13;2u");
            assert_eq!(press(Code::Enter, CTRL).unwrap(), "\x1b[13;5u");
            assert_eq!(press(Code::Tab, SHIFT).unwrap(), "\x1b[9;2u");
            assert_eq!(press(Code::Backspace, CTRL).unwrap(), "\x1b[127;5u");
            assert_eq!(press(Code::Up, NONE).unwrap(), "\x1b[A");
            assert_eq!(press(Code::End, NONE).unwrap(), "\x1b[F");
            assert_eq!(press(Code::Up, SHIFT).unwrap(), "\x1b[1;2A");
            assert_eq!(press(Code::Up, CTRL).unwrap(), "\x1b[1;5A");
            assert_eq!(press(Code::Home, NONE).unwrap(), "\x1b[H");
            assert_eq!(press(Code::F(1), NONE).unwrap(), "\x1b[P");
            assert_eq!(press(Code::F(3), NONE).unwrap(), "\x1b[13~");
            assert_eq!(press(Code::F(3), CTRL).unwrap(), "\x1b[13;5~");
            assert_eq!(press(Code::F(5), NONE).unwrap(), "\x1b[15~");
            assert_eq!(press(Code::F(5), CTRL).unwrap(), "\x1b[15;5~");
            assert_eq!(press(Code::Delete, NONE).unwrap(), "\x1b[3~");
        }
    }

    #[test]
    fn kitty_event_types_mark_repeats_and_releases() {
        let event = |code, mods, kind, flags| {
            send(Key { kind, ..key(code, mods) }, &Modes::default(), flags)
        };
        let (press, repeat, release) = (Kind::Press, Kind::Repeat, Kind::Release);
        let types = DISAMBIGUATE | EVENT_TYPES;
        assert_eq!(event(Char('a'), CTRL, press, types).unwrap(), "\x1b[97;5u");
        assert_eq!(event(Char('a'), CTRL, repeat, types).unwrap(), "\x1b[97;5:2u");
        assert_eq!(event(Char('a'), CTRL, release, types).unwrap(), "\x1b[97;5:3u");
        assert_eq!(event(Code::Up, NONE, release, types).unwrap(), "\x1b[1;1:3A");
        assert_eq!(event(Code::F(3), NONE, release, types).unwrap(), "\x1b[13;1:3~");
        assert_eq!(event(Code::Escape, NONE, release, types).unwrap(), "\x1b[27;1:3u");
        assert_eq!(event(Code::Delete, NONE, repeat, types).unwrap(), "\x1b[3;1:2~");
        assert_eq!(event(Code::Delete, SHIFT, repeat, types).unwrap(), "\x1b[3;2:2~");
        // Keys that type text, and unmodified Enter, Tab and Backspace, release silently and
        // repeat as pressed, unless every key is an escape code.
        assert_eq!(event(Char('a'), NONE, release, types), None);
        assert_eq!(event(Char('a'), NONE, repeat, types).unwrap(), "a");
        assert_eq!(event(Code::Enter, NONE, release, types), None);
        assert_eq!(event(Code::Enter, NONE, repeat, types).unwrap(), "\r");
        let all = EVENT_TYPES | ALL_KEYS;
        assert_eq!(event(Char('a'), NONE, release, all).unwrap(), "\x1b[97;1:3u");
        assert_eq!(event(Code::Enter, NONE, release, all).unwrap(), "\x1b[13;1:3u");
        // Without event types a release is nothing and a repeat a press.
        assert_eq!(event(Code::Escape, NONE, release, DISAMBIGUATE), None);
        assert_eq!(event(Code::Escape, NONE, repeat, DISAMBIGUATE).unwrap(), "\x1b[27u");
    }

    #[test]
    fn kitty_all_keys_alternates_and_text() {
        let all = kitty(ALL_KEYS | EVENT_TYPES);
        assert_eq!(all(key(Char('a'), NONE)).unwrap(), "\x1b[97u");
        assert_eq!(all(key(Char('a'), SHIFT)).unwrap(), "\x1b[97;2u");
        assert_eq!(all(key(Char('é'), NONE)).unwrap(), "\x1b[233u");
        assert_eq!(all(key(Code::Enter, NONE)).unwrap(), "\x1b[13u");
        assert_eq!(all(key(Char(' '), NONE)).unwrap(), "\x1b[32u");
        let alternates = kitty(DISAMBIGUATE | ALTERNATE_KEYS | ALL_KEYS);
        assert_eq!(alternates(key(Char('a'), SHIFT)).unwrap(), "\x1b[97:65;2u");
        // A shifted key that is the key itself is no alternate.
        assert_eq!(alternates(key(Char('1'), SHIFT)).unwrap(), "\x1b[49;2u");
        assert_eq!(alternates(key(Char('!'), SHIFT)).unwrap(), "\x1b[33;2u");
        let rich = kitty(ALL_KEYS | ALTERNATE_KEYS | ASSOCIATED_TEXT);
        assert_eq!(rich(key(Char('a'), SHIFT)).unwrap(), "\x1b[97:65;2;65u");
        assert_eq!(rich(key(Char('a'), NONE)).unwrap(), "\x1b[97;;97u");
        assert_eq!(rich(key(Char('a'), CTRL)).unwrap(), "\x1b[97;5u");
        assert_eq!(rich(key(Char('w'), SUPER)).unwrap(), "\x1b[119;9u");
        assert_eq!(rich(key(Code::Escape, NONE)).unwrap(), "\x1b[27u");
        let release = Key { kind: Kind::Release, ..key(Char('a'), NONE) };
        assert_eq!(
            kitty(ALL_KEYS | EVENT_TYPES | ASSOCIATED_TEXT)(release).unwrap(),
            "\x1b[97;1:3u"
        );
    }

    /// What a host's own kitty terminal said of a key reaches the program as far as it asked:
    /// the key's own number, its alternates, the locks, a lone modifier, composed text.
    #[test]
    fn kitty_passes_on_what_the_host_terminal_said() {
        let all = kitty(DISAMBIGUATE | EVENT_TYPES | ALTERNATE_KEYS | ALL_KEYS | ASSOCIATED_TEXT);
        let some = kitty(DISAMBIGUATE);
        // Shift+1 on a US layout, and its text.
        let bang = Key { number: Some(49), shifted: 33, text: "!", ..key(Char('!'), SHIFT) };
        assert_eq!(all(bang).unwrap(), "\x1b[49:33;2;33u");
        assert_eq!(kitty(ALL_KEYS)(bang).unwrap(), "\x1b[49;2u");
        assert_eq!(some(bang).unwrap(), "!");
        // Ctrl+С on a Cyrillic layout: the base-layout key.
        let es = Key { number: Some(1089), base: 99, ..key(Char('с'), CTRL) };
        assert_eq!(all(es).unwrap(), "\x1b[1089::99;5u");
        assert_eq!(some(es).unwrap(), "\x1b[1089;5u");
        // Caps Lock: its bit only with every key an escape code, and the text it made.
        let capital = Key { text: "A", ..key(Char('a'), CAPS_LOCK) };
        assert_eq!(all(capital).unwrap(), "\x1b[97;65;65u");
        assert_eq!(some(capital).unwrap(), "A");
        assert_eq!(some(key(Code::Escape, CAPS_LOCK)).unwrap(), "\x1b[27u");
        assert_eq!(some(key(Code::Up, NUM_LOCK)).unwrap(), "\x1b[A");
        assert_eq!(all(key(Code::Up, NUM_LOCK)).unwrap(), "\x1b[1;129A");
        // A lone modifier, pressed and released, only with every key an escape code.
        let shift = Key { number: Some(57441), ..key(Code::Modifier, SHIFT) };
        assert_eq!(all(shift).unwrap(), "\x1b[57441;2u");
        let up = Key { kind: Kind::Release, ..Key { mods: NONE, ..shift } };
        assert_eq!(all(up).unwrap(), "\x1b[57441;1:3u");
        assert_eq!(kitty(DISAMBIGUATE | EVENT_TYPES)(shift), None);
        assert_eq!(some(key(Code::Modifier, SHIFT)), None);
        // Text with no key, as an input method makes it.
        let composed = Key { number: Some(0), text: "e\u{301}", ..key(Char('e'), NONE) };
        assert_eq!(all(composed).unwrap(), "\x1b[0;;101:769u");
        assert_eq!(some(composed).unwrap(), "e\u{301}");
        // The keypad: Enter is a key of its own, a digit is text.
        let enter = Key { number: Some(57414), ..key(Code::Enter, NONE) };
        assert_eq!(some(enter).unwrap(), "\x1b[57414u");
        assert_eq!(send(enter, &Modes::default(), 0).unwrap(), "\r");
        let zero = Key { number: Some(57399), text: "0", ..key(Char('0'), NONE) };
        assert_eq!(some(zero).unwrap(), "0");
        assert_eq!(all(zero).unwrap(), "\x1b[57399;;48u");
        // A media key has a number and no legacy form.
        let play = Key { number: Some(57428), ..key(Code::Other, NONE) };
        assert_eq!(some(play).unwrap(), "\x1b[57428u");
        assert_eq!(send(play, &Modes::default(), 0), None);
        assert_eq!(some(key(Code::Other, NONE)), None);
    }

    /// DECKPAM, as xterm's ctlseqs tabulates it ("VT220-Style Function Keys") and its PC
    /// keyboard sends it: a modifier is the `SS3` form's one parameter, and "Use the NumLock key
    /// to override the application mode". Otherwise every keypad key is its main key, as in
    /// kitty's legacy encoding, but modifyOtherKeys "does not apply to special keys, i.e.,
    /// cursor-, keypad-, function- or control-keys".
    #[test]
    fn application_keypad_sends_ss3_forms_unless_num_lock_overrides_it() {
        let deckpam = Modes { keypad: true, ..Modes::default() };
        let digits = ('0'..='9').zip(57399..).zip('p'..='y').map(|((c, n), x)| (n, Char(c), x));
        let others = [
            (57409, Char('.'), 'n'),
            (57410, Char('/'), 'o'),
            (57411, Char('*'), 'j'),
            (57412, Char('-'), 'm'),
            (57413, Char('+'), 'k'),
            (57414, Code::Enter, 'M'),
            (57415, Char('='), 'X'),
            (57416, Char(','), 'l'),
        ];
        for (number, code, last) in digits.chain(others) {
            let pad = Key { number: Some(number), ..key(code, NONE) };
            let main = send(key(code, NONE), &Modes::default(), 0);
            assert_eq!(send(pad, &deckpam, 0).unwrap(), format!("\x1bO{last}"), "{number}");
            assert_eq!(send(pad, &Modes::default(), 0), main, "{number}");
            assert_eq!(send(Key { mods: NUM_LOCK, ..pad }, &deckpam, 0), main, "{number}");
        }
        let pad = |number, code, mods| Key { number: Some(number), ..key(code, mods) };
        let app = |key, modes: &Modes| send(key, &Modes { keypad: true, ..modes.clone() }, 0);
        assert_eq!(app(pad(57414, Code::Enter, CTRL), &level(2)).unwrap(), "\x1bO5M");
        assert_eq!(app(pad(57413, Char('+'), SHIFT), &deckpam).unwrap(), "\x1bO2k");
        assert_eq!(app(pad(57399, Char('0'), ALT), &deckpam).unwrap(), "\x1bO3p");
        assert_eq!(app(pad(57410, Char('/'), SHIFT | ALT | CTRL), &deckpam).unwrap(), "\x1bO8o");
        assert_eq!(app(pad(57414, Code::Enter, SUPER), &deckpam).unwrap(), "\x1bOM");
        let decckm = Modes { cursor_keys: true, ..Modes::default() };
        assert_eq!(app(pad(57414, Code::Enter, NONE), &decckm).unwrap(), "\x1bOM");
        // Numeric: the main key, modifyOtherKeys or not.
        assert_eq!(send(pad(57400, Char('1'), ALT), &level(2), 0).unwrap(), "\x1b1");
        assert_eq!(send(pad(57400, Char('1'), CTRL), &level(2), 0).unwrap(), "1");
        assert_eq!(send(pad(57414, Code::Enter, CTRL), &level(1), 0).unwrap(), "\r");
        assert_eq!(send(pad(57414, Code::Enter, NONE), &decckm, 0).unwrap(), "\r");
        // The navigation keys are the cursor and editing keys, DECKPAM or not, as xterm makes
        // them; Begin is `E` among the cursor keys.
        assert_eq!(app(pad(57424, Code::End, NONE), &deckpam).unwrap(), "\x1b[F");
        assert_eq!(app(pad(57424, Code::End, NONE), &decckm).unwrap(), "\x1bOF");
        assert_eq!(app(pad(57425, Code::Insert, CTRL), &deckpam).unwrap(), "\x1b[2;5~");
        let begin = |mods| pad(57427, Code::Other, mods);
        assert_eq!(app(begin(NONE), &deckpam).unwrap(), "\x1b[E");
        assert_eq!(send(begin(NONE), &decckm, 0).unwrap(), "\x1bOE");
        assert_eq!(send(begin(CTRL), &decckm, 0).unwrap(), "\x1b[1;5E");
    }

    /// Kitty's flags leave DECKPAM out: a keypad key with text types it, the others are their
    /// numbers ("all non text keypad keys will be reported as separate keys with CSI u
    /// encoding"), but KP_BEGIN, which kitty's table gives as `1 E`.
    #[test]
    fn kitty_keypad_keys_ignore_deckpam() {
        let deckpam = Modes { keypad: true, ..Modes::default() };
        let press = |number, code, mods, text, flags| {
            send(Key { number: Some(number), text, ..key(code, mods) }, &deckpam, flags)
        };
        assert_eq!(press(57414, Code::Enter, NONE, "", DISAMBIGUATE).unwrap(), "\x1b[57414u");
        assert_eq!(press(57414, Code::Enter, CTRL, "", DISAMBIGUATE).unwrap(), "\x1b[57414;5u");
        assert_eq!(press(57399, Char('0'), NONE, "0", DISAMBIGUATE).unwrap(), "0");
        assert_eq!(press(57413, Char('+'), CTRL, "", DISAMBIGUATE).unwrap(), "\x1b[57413;5u");
        assert_eq!(press(57399, Char('0'), NUM_LOCK, "0", ALL_KEYS).unwrap(), "\x1b[57399;129u");
        assert_eq!(press(57424, Code::End, NONE, "", DISAMBIGUATE).unwrap(), "\x1b[57424u");
        assert_eq!(press(57427, Code::Other, NONE, "", DISAMBIGUATE).unwrap(), "\x1b[E");
        assert_eq!(press(57427, Code::Other, CTRL, "", DISAMBIGUATE).unwrap(), "\x1b[1;5E");
        let release = Key { kind: Kind::Release, number: Some(57427), ..key(Code::Other, NONE) };
        assert_eq!(send(release, &deckpam, DISAMBIGUATE | EVENT_TYPES).unwrap(), "\x1b[1;1:3E");
    }

    fn mouse(button: Button, action: Action, row: u16, col: u16) -> Mouse {
        Mouse { button, action, mods: NONE, cell: (row, col), pixel: (0, 0) }
    }

    fn modes(mouse: MouseMode, mouse_encoding: MouseEncoding) -> Modes {
        Modes { mouse, mouse_encoding, ..Modes::default() }
    }

    #[test]
    fn modes_choose_which_events_are_reported() {
        let press = mouse(Button::Left, Action::Press, 0, 0);
        let release = mouse(Button::Left, Action::Release, 0, 0);
        let drag = mouse(Button::Left, Action::Motion, 0, 0);
        let hover = mouse(Button::None, Action::Motion, 0, 0);
        let wheel_up = mouse(Button::WheelUp, Action::Press, 0, 0);
        let all = [press, release, drag, hover, wheel_up];
        let reported = |mode| {
            let modes = modes(mode, MouseEncoding::Sgr);
            all.iter().map(|mouse| encode_mouse(mouse, &modes).is_some()).collect::<Vec<_>>()
        };
        assert_eq!(reported(MouseMode::None), [false; 5]);
        assert_eq!(reported(MouseMode::X10), [true, false, false, false, true]);
        assert_eq!(reported(MouseMode::Click), [true, true, false, false, true]);
        assert_eq!(reported(MouseMode::Drag), [true, true, true, false, true]);
        assert_eq!(reported(MouseMode::Motion), [true; 5]);
        let wheel_release = mouse(Button::WheelUp, Action::Release, 0, 0);
        assert_eq!(
            encode_mouse(&wheel_release, &modes(MouseMode::Click, MouseEncoding::Sgr)),
            None
        );
    }

    #[test]
    fn sgr_names_the_button_released_and_carries_modifiers() {
        let sgr = modes(MouseMode::Motion, MouseEncoding::Sgr);
        let encode = |mouse| String::from_utf8(encode_mouse(&mouse, &sgr).unwrap()).unwrap();
        assert_eq!(encode(mouse(Button::Left, Action::Press, 4, 9)), "\x1b[<0;10;5M");
        assert_eq!(encode(mouse(Button::Left, Action::Release, 2, 4)), "\x1b[<0;5;3m");
        assert_eq!(encode(mouse(Button::Right, Action::Release, 4, 9)), "\x1b[<2;10;5m");
        assert_eq!(encode(mouse(Button::Left, Action::Motion, 2, 5)), "\x1b[<32;6;3M");
        assert_eq!(encode(mouse(Button::None, Action::Motion, 3, 6)), "\x1b[<35;7;4M");
        assert_eq!(encode(mouse(Button::WheelUp, Action::Press, 2, 4)), "\x1b[<64;5;3M");
        assert_eq!(encode(mouse(Button::WheelDown, Action::Press, 0, 0)), "\x1b[<65;1;1M");
        let ctrl_right = Mouse { mods: CTRL, ..mouse(Button::Right, Action::Press, 0, 0) };
        assert_eq!(encode(ctrl_right), "\x1b[<18;1;1M");
        let chord =
            Mouse { mods: SHIFT | ALT | CTRL, ..mouse(Button::Middle, Action::Press, 0, 0) };
        assert_eq!(encode(chord), "\x1b[<29;1;1M");
        let shift_wheel = Mouse { mods: SHIFT, ..mouse(Button::WheelUp, Action::Press, 0, 0) };
        assert_eq!(encode(shift_wheel), "\x1b[<68;1;1M");
    }

    /// The one-byte encodings offset each value by 32; past what a byte (or, in UTF-8 mode, a
    /// two-byte character) can carry, a cell goes as the farthest one, as xterm clamps it.
    #[test]
    fn legacy_encodings_offset_by_32_and_clamp_what_they_cannot_say() {
        let click = |encoding| modes(MouseMode::Click, encoding);
        let encode = |mouse, encoding| encode_mouse(&mouse, &click(encoding)).unwrap();
        let press = mouse(Button::Left, Action::Press, 1, 2);
        assert_eq!(encode(press, MouseEncoding::Default), b"\x1b[M #\"");
        let release = mouse(Button::Left, Action::Release, 1, 2);
        assert_eq!(encode(release, MouseEncoding::Default), b"\x1b[M##\"");
        let alt_right_up = Mouse { mods: ALT, ..mouse(Button::Right, Action::Release, 2, 4) };
        assert_eq!(
            encode(alt_right_up, MouseEncoding::Default),
            [0x1b, b'[', b'M', 32 + 3 + 8, 32 + 5, 32 + 3]
        );
        let wheel = mouse(Button::WheelUp, Action::Press, 2, 4);
        assert_eq!(
            encode(wheel, MouseEncoding::Default),
            [0x1b, b'[', b'M', 32 + 64, 32 + 5, 32 + 3]
        );
        let far = mouse(Button::WheelUp, Action::Press, 999, 999);
        assert_eq!(encode(far, MouseEncoding::Default), [0x1b, b'[', b'M', 32 + 64, 255, 255]);
        let wide = mouse(Button::Left, Action::Press, 0, 300);
        assert_eq!(encode(wide, MouseEncoding::Utf8), "\x1b[M \u{14d}!".as_bytes());
        let drag = mouse(Button::Left, Action::Motion, 0, 199);
        assert_eq!(
            encode_mouse(&drag, &modes(MouseMode::Drag, MouseEncoding::Utf8)).unwrap(),
            "\x1b[M@\u{e8}!".as_bytes()
        );
        let farther = mouse(Button::Left, Action::Press, 0, 9999);
        assert_eq!(encode(farther, MouseEncoding::Utf8), "\x1b[M \u{7ff}!".as_bytes());
    }

    #[test]
    fn x10_reports_no_modifiers_and_urxvt_and_pixels_have_their_own_forms() {
        let ctrl_press =
            Mouse { mods: CTRL, pixel: (17, 40), ..mouse(Button::Left, Action::Press, 1, 2) };
        let x10 = encode_mouse(&ctrl_press, &modes(MouseMode::X10, MouseEncoding::Default));
        assert_eq!(x10.unwrap(), b"\x1b[M #\"");
        let click = |encoding| modes(MouseMode::Click, encoding);
        let urxvt = encode_mouse(&ctrl_press, &click(MouseEncoding::Urxvt));
        assert_eq!(urxvt.unwrap(), b"\x1b[48;3;2M");
        let far = mouse(Button::Left, Action::Press, 299, 4);
        assert_eq!(encode_mouse(&far, &click(MouseEncoding::Urxvt)).unwrap(), b"\x1b[32;5;300M");
        let right_up = mouse(Button::Right, Action::Release, 2, 4);
        assert_eq!(encode_mouse(&right_up, &click(MouseEncoding::Urxvt)).unwrap(), b"\x1b[35;5;3M");
        let pixels = encode_mouse(&ctrl_press, &click(MouseEncoding::SgrPixels));
        assert_eq!(pixels.unwrap(), b"\x1b[<16;18;41M");
        let up = Mouse { pixel: (4, 2), ..mouse(Button::Left, Action::Release, 2, 4) };
        assert_eq!(encode_mouse(&up, &click(MouseEncoding::SgrPixels)).unwrap(), b"\x1b[<0;5;3m");
    }
}
