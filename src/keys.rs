use gpui::Keystroke;

/// How the program asked for keys: DECCKM, xterm's modifyOtherKeys and the kitty keyboard flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    /// DECCKM: unmodified arrows and Home/End send `ESC O`.
    pub cursor_keys: bool,
    /// `CSI > 4 ; n m`: 0 off; 1 modified keys that have no legacy form; 2 every modified key
    /// but Shift with text.
    pub modify_other_keys: u8,
    /// The active kitty keyboard flags; any of them replace modifyOtherKeys.
    pub kitty: u8,
}

/// A key going down, repeating while held, or coming up. Only the kitty protocol with its event
/// types flag tells repeats and releases apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Press,
    Repeat,
    Release,
}

/// Kitty's progressive enhancements.
const DISAMBIGUATE: u8 = 1;
const EVENT_TYPES: u8 = 2;
const ALTERNATE_KEYS: u8 = 4;
const ALL_KEYS: u8 = 8;
const ASSOCIATED_TEXT: u8 = 16;

/// The bytes a key event sends to the pty, or `None` when it sends nothing or is not the
/// terminal's: Cmd chords belong to the app, and plain text goes through the platform input
/// handler so IME composition and dead keys work (unless the program asked for every key as an
/// escape code).
pub fn encode(keystroke: &Keystroke, modes: Modes, event: Event) -> Option<Vec<u8>> {
    if keystroke.modifiers.platform {
        return None;
    }
    if modes.kitty & (DISAMBIGUATE | ALL_KEYS) != 0 {
        return kitty(keystroke, modes.kitty, event);
    }
    (event != Event::Release).then(|| legacy(keystroke, modes)).flatten()
}

/// xterm's encodings: control characters, an ESC prefix for Alt, `CSI 1 ; m X` and `CSI n ; m ~`
/// with modifier parameters, and `CSI 27 ; m ; code ~` for what modifyOtherKeys asks for.
fn legacy(keystroke: &Keystroke, modes: Modes) -> Option<Vec<u8>> {
    let mods = &keystroke.modifiers;
    let modifier = 1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.control);
    let esc_prefixed = |bytes: &[u8]| [if mods.alt { &b"\x1b"[..] } else { b"" }, bytes].concat();
    let other_key = |code: u32| format!("\x1b[27;{modifier};{code}~").into_bytes();
    let level = modes.modify_other_keys;
    let cursor = |letter: char| match (modifier, modes.cursor_keys) {
        (1, true) => format!("\x1bO{letter}"),
        (1, false) => format!("\x1b[{letter}"),
        _ => format!("\x1b[1;{modifier}{letter}"),
    };
    let function = |letter: char| match modifier {
        1 => format!("\x1bO{letter}"),
        _ => format!("\x1b[1;{modifier}{letter}"),
    };
    let tilde = |code: u8| match modifier {
        1 => format!("\x1b[{code}~"),
        _ => format!("\x1b[{code};{modifier}~"),
    };
    // Enter and Tab with Ctrl or Shift have no legacy form of their own; Backspace with Ctrl
    // keeps its ^H below level 2.
    let other_control = level >= 2 && modifier > 1 || level >= 1 && (mods.control || mods.shift);
    let sequence = match keystroke.key.as_str() {
        "enter" if other_control => return Some(other_key(13)),
        "enter" => return Some(esc_prefixed(b"\r")),
        "tab" if mods.shift && !mods.control => return Some(b"\x1b[Z".to_vec()),
        "tab" if other_control => return Some(other_key(9)),
        "tab" => return Some(esc_prefixed(b"\t")),
        "backspace" if level >= 2 && modifier > 1 => return Some(other_key(127)),
        "backspace" if mods.control => return Some(esc_prefixed(b"\x08")),
        "backspace" => return Some(esc_prefixed(b"\x7f")),
        "escape" => return Some(esc_prefixed(b"\x1b")),
        "up" => cursor('A'),
        "down" => cursor('B'),
        "right" => cursor('C'),
        "left" => cursor('D'),
        "home" => cursor('H'),
        "end" => cursor('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => function('P'),
        "f2" => function('Q'),
        "f3" => function('R'),
        "f4" => function('S'),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        _ => {
            let c = text_key(&keystroke.key)?;
            let shifted = if mods.shift { c.to_ascii_uppercase() } else { c };
            let control = mods.control.then(|| control_byte(c)).flatten();
            // Level 1 keeps every control character, the VT220's for digits (Ctrl+3 is ESC)
            // and Ctrl+Shift+letter's included, as Emacs expects from xterm.
            let other = match level {
                2 => mods.control || mods.alt,
                1 => mods.control && control.is_none(),
                _ => false,
            };
            return if other {
                Some(other_key(u32::from(shifted)))
            } else if mods.control {
                control.map(|byte| esc_prefixed(&[byte]))
            } else if mods.alt {
                Some(esc_prefixed(shifted.to_string().as_bytes()))
            } else {
                None
            };
        }
    };
    Some(sequence.into_bytes())
}

/// The kitty keyboard protocol: `CSI code[:shifted] ; modifiers[:event] [; text] u` for keys
/// with no text of their own (or every key, with ALL_KEYS), `CSI 1 ; m X` and `CSI n ; m ~` for
/// the functional keys. Text keys without Ctrl or Alt still type text, and Enter, Tab and
/// Backspace stay legacy without modifiers, as the protocol has it.
fn kitty(keystroke: &Keystroke, flags: u8, event: Event) -> Option<Vec<u8>> {
    let mods = &keystroke.modifiers;
    let bits = u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.control);
    let all = flags & ALL_KEYS != 0;
    let events = flags & EVENT_TYPES != 0;
    if event == Event::Release && !events {
        return None;
    }
    let text = text_key(&keystroke.key);
    let (number, last) = match keystroke.key.as_str() {
        "escape" => (27, 'u'),
        "enter" => (13, 'u'),
        "tab" => (9, 'u'),
        "backspace" => (127, 'u'),
        "insert" => (2, '~'),
        "delete" => (3, '~'),
        "pageup" => (5, '~'),
        "pagedown" => (6, '~'),
        "up" => (1, 'A'),
        "down" => (1, 'B'),
        "right" => (1, 'C'),
        "left" => (1, 'D'),
        "home" => (1, 'H'),
        "end" => (1, 'F'),
        "f1" => (1, 'P'),
        "f2" => (1, 'Q'),
        // Not CSI R, which a cursor position report also ends in.
        "f3" => (13, '~'),
        "f4" => (1, 'S'),
        "f5" => (15, '~'),
        "f6" => (17, '~'),
        "f7" => (18, '~'),
        "f8" => (19, '~'),
        "f9" => (20, '~'),
        "f10" => (21, '~'),
        "f11" => (23, '~'),
        "f12" => (24, '~'),
        _ => (u32::from(text?), 'u'),
    };
    let typed = text.map(|c| if mods.shift { c.to_ascii_uppercase() } else { c });
    if !all {
        let plain = !mods.control && !mods.alt;
        match keystroke.key.as_str() {
            _ if plain && typed.is_some() => return None,
            "enter" | "tab" | "backspace" if bits == 0 => {
                let legacy = [number as u8 & 0x7f];
                return (event != Event::Release).then(|| legacy.to_vec());
            }
            _ => {}
        }
    }
    let event_type = match event {
        _ if !events => 1,
        Event::Press => 1,
        Event::Repeat => 2,
        Event::Release => 3,
    };
    let mut modifiers = String::new();
    if bits != 0 || event_type != 1 {
        modifiers = (1 + bits).to_string();
        if event_type != 1 {
            modifiers += &format!(":{event_type}");
        }
    }
    let mut code = number.to_string();
    if flags & ALTERNATE_KEYS != 0
        && let Some(shifted) = typed.filter(|&shifted| u32::from(shifted) != number)
    {
        code += &format!(":{}", u32::from(shifted));
    }
    let types = all && flags & ASSOCIATED_TEXT != 0 && !mods.control && !mods.alt;
    let text = typed.filter(|_| types && event != Event::Release);
    let sequence = match (last, text) {
        ('u', Some(text)) => format!("\x1b[{code};{modifiers};{}u", u32::from(text)),
        ('u' | '~', None) if modifiers.is_empty() => format!("\x1b[{code}{last}"),
        ('u' | '~', _) => format!("\x1b[{code};{modifiers}{last}"),
        (_, _) if modifiers.is_empty() => format!("\x1b[{last}"),
        (_, _) => format!("\x1b[1;{modifiers}{last}"),
    };
    Some(sequence.into_bytes())
}

/// The character a single-character key (or space) types, unshifted.
fn text_key(key: &str) -> Option<char> {
    let key = if key == "space" { " " } else { key };
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    Some(c)
}

fn control_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        '@' | ' ' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '-' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// Bracketed paste wraps the text so the program knows it was not typed; the text loses any
/// ESC so it cannot end the bracket early. Unbracketed, newlines become the Enter key.
pub fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")).into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(source: &str, modes: Modes) -> Option<Vec<u8>> {
        encode(&Keystroke::parse(source).unwrap(), modes, Event::Press)
    }

    fn key(source: &str) -> Option<Vec<u8>> {
        press(source, Modes::default())
    }

    fn text(bytes: Option<Vec<u8>>) -> Option<String> {
        bytes.map(|bytes| String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn printable_text_is_left_to_the_input_handler() {
        assert_eq!(key("a"), None);
        assert_eq!(key("shift-a"), None);
        assert_eq!(key("space"), None);
        assert_eq!(key("cmd-v"), None);
    }

    #[test]
    fn editing_keys() {
        assert_eq!(key("enter").unwrap(), b"\r");
        assert_eq!(key("backspace").unwrap(), b"\x7f");
        assert_eq!(key("ctrl-backspace").unwrap(), b"\x08");
        assert_eq!(key("tab").unwrap(), b"\t");
        assert_eq!(key("shift-tab").unwrap(), b"\x1b[Z");
        assert_eq!(key("escape").unwrap(), b"\x1b");
    }

    #[test]
    fn control_chords() {
        assert_eq!(key("ctrl-a").unwrap(), [1]);
        assert_eq!(key("ctrl-c").unwrap(), [3]);
        assert_eq!(key("ctrl-shift-c").unwrap(), [3]);
        assert_eq!(key("ctrl-[").unwrap(), [0x1b]);
        assert_eq!(key("ctrl-\\").unwrap(), [0x1c]);
        assert_eq!(key("ctrl-]").unwrap(), [0x1d]);
        assert_eq!(key("ctrl-^").unwrap(), [0x1e]);
        assert_eq!(key("ctrl-_").unwrap(), [0x1f]);
        assert_eq!(key("ctrl-space").unwrap(), [0]);
    }

    #[test]
    fn alt_is_an_escape_prefix() {
        assert_eq!(key("alt-b").unwrap(), b"\x1bb");
        assert_eq!(key("alt-shift-b").unwrap(), b"\x1bB");
        assert_eq!(key("alt-backspace").unwrap(), b"\x1b\x7f");
        assert_eq!(key("alt-ctrl-a").unwrap(), b"\x1b\x01");
    }

    #[test]
    fn cursor_keys_follow_decckm_and_modifiers() {
        assert_eq!(key("up").unwrap(), b"\x1b[A");
        let decckm = Modes { cursor_keys: true, ..Modes::default() };
        assert_eq!(press("up", decckm).unwrap(), b"\x1bOA");
        assert_eq!(key("shift-left").unwrap(), b"\x1b[1;2D");
        assert_eq!(key("ctrl-right").unwrap(), b"\x1b[1;5C");
        assert_eq!(key("alt-left").unwrap(), b"\x1b[1;3D");
        assert_eq!(key("home").unwrap(), b"\x1b[H");
        assert_eq!(key("end").unwrap(), b"\x1b[F");
    }

    #[test]
    fn navigation_and_function_keys() {
        assert_eq!(key("pageup").unwrap(), b"\x1b[5~");
        assert_eq!(key("pagedown").unwrap(), b"\x1b[6~");
        assert_eq!(key("insert").unwrap(), b"\x1b[2~");
        assert_eq!(key("delete").unwrap(), b"\x1b[3~");
        assert_eq!(key("shift-delete").unwrap(), b"\x1b[3;2~");
        assert_eq!(key("f1").unwrap(), b"\x1bOP");
        assert_eq!(key("f4").unwrap(), b"\x1bOS");
        assert_eq!(key("ctrl-f1").unwrap(), b"\x1b[1;5P");
        assert_eq!(key("f5").unwrap(), b"\x1b[15~");
        assert_eq!(key("f12").unwrap(), b"\x1b[24~");
    }

    #[test]
    fn releases_send_nothing_without_kitty_event_types() {
        let up = Keystroke::parse("ctrl-a").unwrap();
        assert_eq!(encode(&up, Modes::default(), Event::Release), None);
        assert_eq!(encode(&up, Modes::default(), Event::Repeat).unwrap(), [1]);
    }

    /// Emacs turns on level 1 when DA2 says xterm 242 or later, and decodes exactly these
    /// (probed with C-h k in emacs -nw): C-1, C-;, C-., C-<return>, C-<tab> as themselves.
    #[test]
    fn modify_other_keys_level_1_covers_what_legacy_cannot_say() {
        let modes = Modes { modify_other_keys: 1, ..Modes::default() };
        let press = |source| text(press(source, modes));
        assert_eq!(press("ctrl-a").unwrap(), "\x01");
        assert_eq!(press("ctrl-[").unwrap(), "\x1b");
        assert_eq!(press("ctrl-;").unwrap(), "\x1b[27;5;59~");
        assert_eq!(press("ctrl-.").unwrap(), "\x1b[27;5;46~");
        assert_eq!(press("ctrl-1").unwrap(), "\x1b[27;5;49~");
        assert_eq!(press("ctrl-3").unwrap(), "\x1b");
        assert_eq!(press("ctrl-space").unwrap(), "\x00");
        assert_eq!(press("ctrl-shift-a").unwrap(), "\x01");
        assert_eq!(press("ctrl-enter").unwrap(), "\x1b[27;5;13~");
        assert_eq!(press("shift-enter").unwrap(), "\x1b[27;2;13~");
        assert_eq!(press("ctrl-tab").unwrap(), "\x1b[27;5;9~");
        assert_eq!(press("shift-tab").unwrap(), "\x1b[Z");
        assert_eq!(press("ctrl-backspace").unwrap(), "\x08");
        assert_eq!(press("alt-b").unwrap(), "\x1bb");
        assert_eq!(press("alt-enter").unwrap(), "\x1b\r");
        assert_eq!(press("a"), None);
    }

    #[test]
    fn modify_other_keys_level_2_covers_every_modified_key_but_shifted_text() {
        let modes = Modes { modify_other_keys: 2, ..Modes::default() };
        let press = |source| text(press(source, modes));
        assert_eq!(press("ctrl-a").unwrap(), "\x1b[27;5;97~");
        assert_eq!(press("alt-b").unwrap(), "\x1b[27;3;98~");
        assert_eq!(press("ctrl-alt-a").unwrap(), "\x1b[27;7;97~");
        assert_eq!(press("alt-enter").unwrap(), "\x1b[27;3;13~");
        assert_eq!(press("ctrl-shift-a").unwrap(), "\x1b[27;6;65~");
        assert_eq!(press("ctrl-backspace").unwrap(), "\x1b[27;5;127~");
        assert_eq!(press("shift-a"), None);
        assert_eq!(press("up").unwrap(), "\x1b[A");
    }

    /// nvim pushes flags 3 (disambiguate, event types) once `CSI ? u` is answered.
    #[test]
    fn kitty_disambiguate_encodes_what_legacy_would_confuse() {
        let modes = Modes { kitty: DISAMBIGUATE, ..Modes::default() };
        let press = |source| text(press(source, modes));
        assert_eq!(press("escape").unwrap(), "\x1b[27u");
        assert_eq!(press("ctrl-a").unwrap(), "\x1b[97;5u");
        assert_eq!(press("ctrl-i").unwrap(), "\x1b[105;5u");
        assert_eq!(press("ctrl-shift-a").unwrap(), "\x1b[97;6u");
        assert_eq!(press("alt-b").unwrap(), "\x1b[98;3u");
        assert_eq!(press("ctrl-space").unwrap(), "\x1b[32;5u");
        assert_eq!(press("a"), None);
        assert_eq!(press("shift-a"), None);
        assert_eq!(press("enter").unwrap(), "\r");
        assert_eq!(press("tab").unwrap(), "\t");
        assert_eq!(press("backspace").unwrap(), "\x7f");
        assert_eq!(press("shift-enter").unwrap(), "\x1b[13;2u");
        assert_eq!(press("ctrl-enter").unwrap(), "\x1b[13;5u");
        assert_eq!(press("shift-tab").unwrap(), "\x1b[9;2u");
        assert_eq!(press("ctrl-backspace").unwrap(), "\x1b[127;5u");
        assert_eq!(press("up").unwrap(), "\x1b[A");
        assert_eq!(press("shift-up").unwrap(), "\x1b[1;2A");
        assert_eq!(press("home").unwrap(), "\x1b[H");
        assert_eq!(press("f1").unwrap(), "\x1b[P");
        assert_eq!(press("f3").unwrap(), "\x1b[13~");
        assert_eq!(press("ctrl-f5").unwrap(), "\x1b[15;5~");
        assert_eq!(press("delete").unwrap(), "\x1b[3~");
        assert_eq!(press("cmd-c"), None);
    }

    #[test]
    fn kitty_event_types_mark_repeats_and_releases() {
        let modes = Modes { kitty: DISAMBIGUATE | EVENT_TYPES, ..Modes::default() };
        let event = |source, event| text(encode(&Keystroke::parse(source).unwrap(), modes, event));
        assert_eq!(event("ctrl-a", Event::Press).unwrap(), "\x1b[97;5u");
        assert_eq!(event("ctrl-a", Event::Repeat).unwrap(), "\x1b[97;5:2u");
        assert_eq!(event("ctrl-a", Event::Release).unwrap(), "\x1b[97;5:3u");
        assert_eq!(event("up", Event::Release).unwrap(), "\x1b[1;1:3A");
        assert_eq!(event("escape", Event::Release).unwrap(), "\x1b[27;1:3u");
        assert_eq!(event("delete", Event::Repeat).unwrap(), "\x1b[3;1:2~");
        // Keys that type text, and unmodified Enter, Tab and Backspace, release silently.
        assert_eq!(event("a", Event::Release), None);
        assert_eq!(event("enter", Event::Release), None);
        assert_eq!(event("enter", Event::Repeat).unwrap(), "\r");
    }

    #[test]
    fn kitty_all_keys_alternates_and_text() {
        let all = Modes { kitty: ALL_KEYS | EVENT_TYPES, ..Modes::default() };
        let event =
            |source, modes, event| text(encode(&Keystroke::parse(source).unwrap(), modes, event));
        assert_eq!(event("a", all, Event::Press).unwrap(), "\x1b[97u");
        assert_eq!(event("a", all, Event::Release).unwrap(), "\x1b[97;1:3u");
        assert_eq!(event("shift-a", all, Event::Press).unwrap(), "\x1b[97;2u");
        assert_eq!(event("enter", all, Event::Press).unwrap(), "\x1b[13u");
        assert_eq!(event("space", all, Event::Press).unwrap(), "\x1b[32u");
        let rich = Modes { kitty: ALL_KEYS | ALTERNATE_KEYS | ASSOCIATED_TEXT, ..Modes::default() };
        assert_eq!(event("shift-a", rich, Event::Press).unwrap(), "\x1b[97:65;2;65u");
        assert_eq!(event("a", rich, Event::Press).unwrap(), "\x1b[97;;97u");
        assert_eq!(event("ctrl-a", rich, Event::Press).unwrap(), "\x1b[97;5u");
        assert_eq!(event("escape", rich, Event::Press).unwrap(), "\x1b[27u");
    }

    #[test]
    fn paste_brackets_or_maps_newlines_to_enter() {
        assert_eq!(paste("a\nb", true), b"\x1b[200~a\nb\x1b[201~");
        assert_eq!(paste("a\x1b[201~b", true), b"\x1b[200~a[201~b\x1b[201~");
        assert_eq!(paste("a\r\nb\nc", false), b"a\rb\rc");
    }
}
