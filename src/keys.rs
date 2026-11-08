use std::path::PathBuf;

use gpui::{Keystroke, Modifiers};
use rtty_vt::Terminal;
use rtty_vt::input::{self, Code, Key, Kind};

/// The bytes a key event sends to the pty, as the program behind `term` asked for its keys
/// (`rtty_vt::input`), or `None` when it sends nothing or is not the terminal's: Cmd chords
/// belong to the app, and what types text goes through the platform input handler so IME
/// composition and dead keys work (unless the program asked for every key as an escape code).
pub fn encode(keystroke: &Keystroke, term: &Terminal, kind: Kind) -> Option<Vec<u8>> {
    (!keystroke.modifiers.platform).then(|| send(keystroke, term, kind)).flatten()
}

/// A chord bound to focus, resize, close or last pane (Cmd+Opt+arrows, tutti's Ctrl+h and
/// Alt+h, whatever `[bind]` adds), for a program that claimed them: as `encode` sends the key,
/// Cmd as the kitty protocol's super modifier. `None` when the program's encoding cannot say it:
/// Cmd has no xterm form that Emacs or others decode (Emacs reads `CSI 1 ; 11 D` as an undefined
/// `M-[ 1 ; 1`), so outside the kitty protocol a Cmd chord stays rtty's; and so does Cmd with a
/// key that types text (Cmd+W), unless the program asked for every key as an escape code.
pub fn encode_chord(keystroke: &Keystroke, term: &Terminal) -> Option<Vec<u8>> {
    let m = &keystroke.modifiers;
    let flags = term.kitty_keyboard();
    let types = matches!(code(&keystroke.key), Some(Code::Char(_))) && !m.control && !m.alt;
    let sayable =
        !m.platform || flags & input::ALL_KEYS != 0 || flags & input::DISAMBIGUATE != 0 && !types;
    sayable.then(|| send(keystroke, term, Kind::Press)).flatten()
}

/// `keystroke` as the program asked for it, but for text it would just type.
fn send(keystroke: &Keystroke, term: &Terminal, kind: Kind) -> Option<Vec<u8>> {
    let m = &keystroke.modifiers;
    let code = code(&keystroke.key)?;
    let text = match code {
        Code::Char(c) if !(m.control || m.alt || m.platform) => {
            if m.shift { c.to_ascii_uppercase() } else { c }.to_string()
        }
        _ => String::new(),
    };
    let key = Key { code, mods: mods(m), kind, text: &text, ..Key::default() };
    let bytes = input::encode_key(&key, term.modes(), term.kitty_keyboard());
    bytes.filter(|bytes| *bytes != text.as_bytes())
}

/// The key GPUI names: one character (or space), a named key, or F1 to F12.
fn code(key: &str) -> Option<Code> {
    Some(match key {
        "enter" => Code::Enter,
        "tab" => Code::Tab,
        "backspace" => Code::Backspace,
        "escape" => Code::Escape,
        "up" => Code::Up,
        "down" => Code::Down,
        "right" => Code::Right,
        "left" => Code::Left,
        "home" => Code::Home,
        "end" => Code::End,
        "insert" => Code::Insert,
        "delete" => Code::Delete,
        "pageup" => Code::PageUp,
        "pagedown" => Code::PageDown,
        "space" => Code::Char(' '),
        key => {
            let mut chars = key.chars();
            match (chars.next(), chars.next(), key.strip_prefix('f').map(str::parse)) {
                (_, _, Some(Ok(n))) => Code::F(n),
                (Some(c), None, _) => Code::Char(c),
                _ => return None,
            }
        }
    })
}

/// GPUI's modifiers in kitty's bits, Cmd as super.
pub fn mods(m: &Modifiers) -> u8 {
    input::SHIFT * u8::from(m.shift)
        + input::ALT * u8::from(m.alt)
        + input::CTRL * u8::from(m.control)
        + input::SUPER * u8::from(m.platform)
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

/// The answer to a program's OSC 52 clipboard read: `text` in padded base64, for `selection`, as
/// the query asked and ended (`terminator`).
pub fn clipboard(selection: &str, text: &str, terminator: &str) -> Vec<u8> {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let base64: String = text
        .as_bytes()
        .chunks(3)
        .flat_map(|chunk| {
            let bits =
                chunk.iter().zip([16, 8, 0]).fold(0, |bits, (&b, at)| bits | u32::from(b) << at);
            let digit = move |i: usize| char::from(DIGITS[(bits >> (18 - 6 * i) & 63) as usize]);
            (0..4).map(move |i| if i <= chunk.len() { digit(i) } else { '=' })
        })
        .collect();
    format!("\x1b]52;{selection};{base64}{terminator}").into_bytes()
}

/// What sh, bash, zsh or fish read as something other than itself somewhere in a word.
const SHELL_SPECIAL: &str = " !\"#$&'()*;<>?[\\]^`{|}~";

/// Dropped files' paths as a shell reads them back, separated by spaces: every character a shell
/// treats specially behind a backslash, the rest (non-ASCII included) as it is. `None` when a path
/// holds a control character, which reaches the shell's line editor as a key (Enter, Ctrl+U,
/// Ctrl+D) whatever quotes it.
pub fn dropped(paths: &[PathBuf]) -> Option<String> {
    let quoted = paths.iter().map(|path| {
        let path = path.to_str().filter(|path| !path.chars().any(char::is_control))?;
        Some(path.chars().fold(String::new(), |mut quoted, c| {
            if SHELL_SPECIAL.contains(c) {
                quoted.push('\\');
            }
            quoted.push(c);
            quoted
        }))
    });
    quoted.collect::<Option<Vec<String>>>().map(|paths| paths.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal whose program asked for its keys with `setup`.
    fn term(setup: &[u8]) -> Terminal {
        let mut term = Terminal::new(2, 10, 0);
        term.advance(setup);
        term
    }

    fn press(source: &str, term: &Terminal) -> Option<String> {
        let bytes = encode(&Keystroke::parse(source).unwrap(), term, Kind::Press);
        bytes.map(|bytes| String::from_utf8(bytes).unwrap())
    }

    fn key(source: &str) -> Option<String> {
        press(source, &term(b""))
    }

    #[test]
    fn printable_text_is_left_to_the_input_handler() {
        for term in [term(b""), term(b"\x1b[>4;2m"), term(b"\x1b[>3u")] {
            for source in ["a", "shift-a", "1", "space", "shift-space", "cmd-v"] {
                assert_eq!(press(source, &term), None, "{source}");
            }
        }
        // Unless every key is to be an escape code.
        assert_eq!(press("a", &term(b"\x1b[>8u")).unwrap(), "\x1b[97u");
        assert_eq!(press("cmd-a", &term(b"\x1b[>8u")), None);
    }

    #[test]
    fn gpui_keys_reach_the_encoder_with_their_modifiers() {
        assert_eq!(key("enter").unwrap(), "\r");
        assert_eq!(key("shift-tab").unwrap(), "\x1b[Z");
        assert_eq!(key("ctrl-backspace").unwrap(), "\x08");
        assert_eq!(key("ctrl-a").unwrap(), "\x01");
        assert_eq!(key("ctrl-space").unwrap(), "\x00");
        assert_eq!(key("ctrl-1").unwrap(), "1");
        assert_eq!(key("alt-shift-b").unwrap(), "\x1bB");
        assert_eq!(key("alt-backspace").unwrap(), "\x1b\x7f");
        assert_eq!(key("shift-left").unwrap(), "\x1b[1;2D");
        assert_eq!(key("pagedown").unwrap(), "\x1b[6~");
        assert_eq!(key("ctrl-f").unwrap(), "\x06");
        assert_eq!(key("f1").unwrap(), "\x1bOP");
        assert_eq!(key("ctrl-f12").unwrap(), "\x1b[24;5~");
        assert_eq!(press("up", &term(b"\x1b[?1h")).unwrap(), "\x1bOA");
        assert_eq!(press("ctrl-;", &term(b"\x1b[>4;1m")).unwrap(), "\x1b[27;5;59~");
        assert_eq!(press("ctrl-i", &term(b"\x1b[>1u")).unwrap(), "\x1b[105;5u");
        let rich = term(b"\x1b[>28u");
        assert_eq!(press("shift-a", &rich).unwrap(), "\x1b[97:65;2;65u");
        assert_eq!(press("ctrl-a", &rich).unwrap(), "\x1b[97;5u");
    }

    #[test]
    fn releases_send_nothing_without_kitty_event_types() {
        let ctrl_a = Keystroke::parse("ctrl-a").unwrap();
        assert_eq!(encode(&ctrl_a, &term(b""), Kind::Release), None);
        assert_eq!(encode(&ctrl_a, &term(b""), Kind::Repeat).unwrap(), [1]);
        assert_eq!(encode(&ctrl_a, &term(b"\x1b[>3u"), Kind::Release).unwrap(), b"\x1b[97;5:3u");
    }

    /// Probed with nvim 0.12 (kitty flags 3): `CSI 1;11D` reads as `<M-D-Left>`, `CSI 1;13A` as
    /// `<C-D-Up>`. Emacs -nw decodes no Cmd form, so its claim covers tutti's keys alone.
    #[test]
    fn claimed_chords_carry_cmd_only_in_the_kitty_protocol() {
        let (kitty, emacs) = (term(b"\x1b[>3u"), term(b"\x1b[>4;1m"));
        let chord = |source, term: &Terminal| {
            let bytes = encode_chord(&Keystroke::parse(source).unwrap(), term);
            bytes.map(|bytes| String::from_utf8(bytes).unwrap())
        };
        assert_eq!(chord("cmd-alt-left", &kitty).unwrap(), "\x1b[1;11D");
        assert_eq!(chord("cmd-ctrl-up", &kitty).unwrap(), "\x1b[1;13A");
        assert_eq!(chord("cmd-alt-left", &term(b"")), None);
        assert_eq!(chord("cmd-alt-left", &emacs), None);
        assert_eq!(chord("ctrl-h", &emacs).unwrap(), "\x08");
        assert_eq!(chord("alt-l", &emacs).unwrap(), "\x1bl");
        assert_eq!(chord("ctrl-h", &kitty).unwrap(), "\x1b[104;5u");
        assert_eq!(chord("alt-k", &kitty).unwrap(), "\x1b[107;3u");
        // Close and last pane: Neovim's `<M-x>` and `<C-\>`. Disambiguating, Cmd with a key that
        // types text stays rtty's, so Cmd+W does; with every key an escape code it is the
        // program's.
        assert_eq!(chord("alt-x", &kitty).unwrap(), "\x1b[120;3u");
        assert_eq!(chord("ctrl-\\", &emacs).unwrap(), "\x1c");
        assert_eq!(chord("cmd-w", &kitty), None);
        assert_eq!(chord("cmd-w", &emacs), None);
        assert_eq!(chord("cmd-w", &term(b"\x1b[>9u")).unwrap(), "\x1b[119;9u");
        // Unclaimed, Cmd stays the app's whatever the mode.
        assert_eq!(press("cmd-alt-left", &kitty), None);
    }

    #[test]
    fn a_clipboard_read_is_answered_in_padded_base64_as_it_asked() {
        let answer = |text| String::from_utf8(clipboard("c", text, "\x07")).unwrap();
        let rfc4648 =
            [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg==")];
        for (text, base64) in rfc4648.into_iter().chain([("foobar", "Zm9vYmFy"), ("é?", "w6k/")]) {
            assert_eq!(answer(text), format!("\x1b]52;c;{base64}\x07"), "{text:?}");
        }
        assert_eq!(clipboard("", "hello", "\x1b\\"), b"\x1b]52;;aGVsbG8=\x1b\\");
    }

    #[test]
    fn paste_brackets_or_maps_newlines_to_enter() {
        assert_eq!(paste("a\nb", true), b"\x1b[200~a\nb\x1b[201~");
        assert_eq!(paste("a\x1b[201~b", true), b"\x1b[200~a[201~b\x1b[201~");
        assert_eq!(paste("a\r\nb\nc", false), b"a\rb\rc");
    }

    const AWKWARD: [&str; 5] = [
        "/tmp/shot.png",
        "/tmp/Screen Shot 2026-11-04 at 9.41.07.png",
        "/tmp/it's \"done\".txt",
        "/tmp/café 日本/naïve.md",
        "/tmp/a$(b)&c;d|e*f?[g]{h}<i>!j#k~l^m`n\\o",
    ];

    #[test]
    fn dropped_paths_escape_what_a_shell_would_read_otherwise() {
        let drop = |paths: &[&str]| dropped(&paths.iter().map(PathBuf::from).collect::<Vec<_>>());
        assert_eq!(drop(&AWKWARD[..1]).unwrap(), "/tmp/shot.png");
        assert_eq!(
            drop(&AWKWARD[1..3]).unwrap(),
            "/tmp/Screen\\ Shot\\ 2026-11-04\\ at\\ 9.41.07.png /tmp/it\\'s\\ \\\"done\\\".txt"
        );
        assert_eq!(drop(&AWKWARD[3..4]).unwrap(), "/tmp/café\\ 日本/naïve.md");
        assert_eq!(
            drop(&AWKWARD[4..]).unwrap(),
            "/tmp/a\\$\\(b\\)\\&c\\;d\\|e\\*f\\?\\[g\\]\\{h\\}\\<i\\>\\!j\\#k\\~l\\^m\\`n\\\\o"
        );
        for control in ["/tmp/a\nb", "/tmp/a\rls", "/tmp/a\x1b[201~", "/tmp/a\u{85}b"] {
            assert_eq!(drop(&["/tmp/fine", control]), None, "{control:?}");
        }
    }

    /// The shells themselves read each path back whole, as `printf` shows.
    #[test]
    fn shells_read_dropped_paths_back_as_they_were() {
        let paths: Vec<PathBuf> = AWKWARD.iter().map(PathBuf::from).collect();
        let words = dropped(&paths).unwrap();
        for shell in ["/bin/sh", "/bin/zsh"] {
            let output = std::process::Command::new(shell)
                .args(["-c", &format!("printf '%s\\n' {words}")])
                .output()
                .unwrap();
            assert!(output.status.success(), "{shell}: {output:?}");
            let read = String::from_utf8(output.stdout).unwrap();
            assert_eq!(read.lines().collect::<Vec<_>>(), AWKWARD, "{shell}");
        }
    }
}
