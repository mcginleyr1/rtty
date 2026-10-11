use gpui::Keystroke;

/// The bytes a key sends to the pty, or `None` when the key is not the terminal's:
/// Cmd chords belong to the app, and plain printable text goes through the platform input
/// handler so IME composition and dead keys work.
pub fn encode(keystroke: &Keystroke, app_cursor: bool) -> Option<Vec<u8>> {
    let mods = &keystroke.modifiers;
    if mods.platform {
        return None;
    }
    let modifier = 1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.control);
    let esc_prefixed = |bytes: &[u8]| [if mods.alt { &b"\x1b"[..] } else { b"" }, bytes].concat();
    let cursor = |letter: char| match (modifier, app_cursor) {
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
    let sequence = match keystroke.key.as_str() {
        "enter" => return Some(esc_prefixed(b"\r")),
        "tab" if mods.shift => return Some(b"\x1b[Z".to_vec()),
        "tab" => return Some(esc_prefixed(b"\t")),
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
        key => {
            let key = if key == "space" { " " } else { key };
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            return if mods.control {
                control_byte(c).map(|byte| esc_prefixed(&[byte]))
            } else if mods.alt {
                let c = if mods.shift { c.to_ascii_uppercase() } else { c };
                Some(esc_prefixed(c.to_string().as_bytes()))
            } else {
                None
            };
        }
    };
    Some(sequence.into_bytes())
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

    fn key(source: &str) -> Option<Vec<u8>> {
        encode(&Keystroke::parse(source).unwrap(), false)
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
        assert_eq!(encode(&Keystroke::parse("up").unwrap(), true).unwrap(), b"\x1bOA");
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
    fn paste_brackets_or_maps_newlines_to_enter() {
        assert_eq!(paste("a\nb", true), b"\x1b[200~a\nb\x1b[201~");
        assert_eq!(paste("a\x1b[201~b", true), b"\x1b[200~a[201~b\x1b[201~");
        assert_eq!(paste("a\r\nb\nc", false), b"a\rb\rc");
    }
}
