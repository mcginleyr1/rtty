//! Replies to the queries a program sends, from xterm's ctlseqs, the VT510
//! manual, kitty's keyboard protocol and contour's colour-scheme reports.

use rtty_vt::{ColorScheme, Event, FOREGROUND, Rgb, Terminal};

fn replies(term: &mut Terminal, bytes: &str) -> Vec<String> {
    term.advance(bytes.as_bytes());
    term.take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Reply(bytes) => Some(String::from_utf8(bytes).unwrap()),
            _ => None,
        })
        .collect()
}

fn ask(bytes: &str) -> Vec<String> {
    replies(&mut Terminal::new(24, 80, 0), bytes)
}

#[test]
fn device_attributes() {
    assert_eq!(ask("\x1b[c"), ["\x1b[?62;22c"]);
    assert_eq!(ask("\x1b[0c"), ["\x1b[?62;22c"]);
    assert_eq!(ask("\x1bZ"), ["\x1b[?62;22c"]);
    let secondary = ask("\x1b[>c");
    assert!(secondary[0].starts_with("\x1b[>1;") && secondary[0].ends_with(";0c"), "{secondary:?}");
    assert_eq!(ask("\x1b[>q"), [format!("\x1bP>|rtty-vt {}\x1b\\", env!("CARGO_PKG_VERSION"))]);
}

#[test]
fn status_and_cursor_position_reports() {
    assert_eq!(ask("\x1b[5n"), ["\x1b[0n"]);
    assert_eq!(ask("\x1b[5;10H\x1b[6n"), ["\x1b[5;10R"]);
    assert_eq!(ask("\x1b[5;10H\x1b[?6n"), ["\x1b[?5;10;1R"]);
    // Origin mode reports relative to the scroll region.
    assert_eq!(ask("\x1b[3;8r\x1b[?6h\x1b[2;4H\x1b[6n"), ["\x1b[2;4R"]);
    // A full row leaves the cursor on its last column.
    assert_eq!(ask(&format!("{}\x1b[6n", "x".repeat(80))), ["\x1b[1;80R"]);
}

#[test]
fn mode_reports() {
    assert_eq!(ask("\x1b[?1h\x1b[?1$p"), ["\x1b[?1;1$y"]);
    assert_eq!(ask("\x1b[?2004$p"), ["\x1b[?2004;2$y"]);
    assert_eq!(ask("\x1b[?7$p"), ["\x1b[?7;1$y"]);
    assert_eq!(ask("\x1b[?2031h\x1b[?2031$p"), ["\x1b[?2031;1$y"]);
    assert_eq!(ask("\x1b[?1049h\x1b[?1049$p"), ["\x1b[?1049;1$y"]);
    assert_eq!(ask("\x1b[?9999$p"), ["\x1b[?9999;0$y"]);
    assert_eq!(ask("\x1b[4h\x1b[4$p"), ["\x1b[4;1$y"]);
    assert_eq!(ask("\x1b[20$p"), ["\x1b[20;2$y"]);
    assert_eq!(ask("\x1b[12$p"), ["\x1b[12;0$y"]);
}

#[test]
fn colour_queries_answer_from_the_program_then_the_host() {
    let mut term = Terminal::new(24, 80, 0);
    assert!(replies(&mut term, "\x1b]4;1;?\x07\x1b]11;?\x07").is_empty(), "nothing set: no answer");
    term.set_host_color(1, Some(Rgb(1, 2, 3)));
    term.set_host_color(FOREGROUND, Some(Rgb(0xaa, 0xbb, 0xcc)));
    term.set_host_color(FOREGROUND + 1, Some(Rgb(0, 0, 0)));
    assert_eq!(replies(&mut term, "\x1b]4;1;?\x07"), ["\x1b]4;1;rgb:0101/0202/0303\x07"]);
    assert_eq!(replies(&mut term, "\x1b]4;1;?\x1b\\"), ["\x1b]4;1;rgb:0101/0202/0303\x1b\\"]);
    assert_eq!(
        replies(&mut term, "\x1b]10;?;?\x07"),
        ["\x1b]10;rgb:aaaa/bbbb/cccc\x07", "\x1b]11;rgb:0000/0000/0000\x07"]
    );
    // What the program sets wins, until it resets it.
    assert!(replies(&mut term, "\x1b]4;1;#ff8000\x07\x1b]10;rgb:12/34/56\x07").is_empty());
    assert_eq!(term.color(1), Some(Rgb(0xff, 0x80, 0)));
    assert_eq!(replies(&mut term, "\x1b]4;1;?\x07"), ["\x1b]4;1;rgb:ffff/8080/0000\x07"]);
    assert_eq!(replies(&mut term, "\x1b]10;?\x07"), ["\x1b]10;rgb:1212/3434/5656\x07"]);
    replies(&mut term, "\x1b]104;1\x07\x1b]110\x07");
    assert_eq!(term.color_override(1), None);
    assert_eq!(replies(&mut term, "\x1b]4;1;?;2;?\x07"), ["\x1b]4;1;rgb:0101/0202/0303\x07"]);
}

#[test]
fn termcap_queries() {
    // TN, then a name nobody knows.
    assert_eq!(
        ask("\x1bP+q544e;5858\x1b\\"),
        ["\x1bP1+r544e=787465726D2D323536636F6C6F72\x1b\\", "\x1bP0+r5858\x1b\\"]
    );
}

#[test]
fn setting_requests() {
    assert_eq!(
        ask("\x1b[1;4:3;31;48;5;100m\x1bP$qm\x1b\\"),
        ["\x1bP1$r0;1;4:3;31;48;5;100m\x1b\\"]
    );
    assert_eq!(ask("\x1b[2;10r\x1bP$qr\x1b\\"), ["\x1bP1$r2;10r\x1b\\"]);
    assert_eq!(ask("\x1b[6 q\x1bP$q q\x1b\\"), ["\x1bP1$r6 q\x1b\\"]);
    assert_eq!(ask("\x1bP$qx\x1b\\"), ["\x1bP0$r\x1b\\"]);
}

#[test]
fn colour_scheme_reports() {
    let mut term = Terminal::new(24, 80, 0);
    assert!(replies(&mut term, "\x1b[?996n").is_empty(), "no scheme known");
    term.set_color_scheme(Some(ColorScheme::Dark));
    assert_eq!(replies(&mut term, "\x1b[?996n"), ["\x1b[?997;1n"]);
    // Unsolicited reports only once the program asked for them.
    term.set_color_scheme(Some(ColorScheme::Light));
    assert!(term.take_events().is_empty());
    replies(&mut term, "\x1b[?2031h");
    term.set_color_scheme(Some(ColorScheme::Dark));
    term.set_color_scheme(Some(ColorScheme::Dark));
    assert_eq!(replies(&mut term, ""), ["\x1b[?997;1n"]);
    replies(&mut term, "\x1b[?2031l");
    term.set_color_scheme(Some(ColorScheme::Light));
    assert!(replies(&mut term, "").is_empty());
}

#[test]
fn kitty_keyboard_flags_stack_per_screen() {
    let mut term = Terminal::new(24, 80, 0);
    assert_eq!(replies(&mut term, "\x1b[?u"), ["\x1b[?0u"]);
    assert_eq!(replies(&mut term, "\x1b[>5u\x1b[?u"), ["\x1b[?5u"]);
    assert_eq!(replies(&mut term, "\x1b[=2;2u\x1b[?u"), ["\x1b[?7u"]);
    assert_eq!(replies(&mut term, "\x1b[=1;3u\x1b[?u"), ["\x1b[?6u"]);
    assert_eq!(replies(&mut term, "\x1b[=3u\x1b[?u"), ["\x1b[?3u"]);
    // The alternate screen keeps a stack of its own.
    assert_eq!(replies(&mut term, "\x1b[?1049h\x1b[?u\x1b[>1u\x1b[?u"), ["\x1b[?0u", "\x1b[?1u"]);
    assert_eq!(replies(&mut term, "\x1b[?1049l\x1b[?u"), ["\x1b[?3u"]);
    assert_eq!(replies(&mut term, "\x1b[<5u\x1b[?u"), ["\x1b[?0u"]);
    assert_eq!(term.kitty_keyboard(), 0);
}

#[test]
fn window_size_reports() {
    let mut term = Terminal::new(24, 80, 0);
    assert_eq!(replies(&mut term, "\x1b[18t"), ["\x1b[8;24;80t"]);
    assert!(replies(&mut term, "\x1b[14t\x1b[16t").is_empty(), "pixel size unknown");
    term.set_cell_pixels(10, 20);
    assert_eq!(replies(&mut term, "\x1b[14t\x1b[16t"), ["\x1b[4;480;800t", "\x1b[6;20;10t"]);
}

#[test]
fn clipboard_reads_are_never_answered() {
    let mut term = Terminal::new(24, 80, 0);
    term.advance(b"\x1b]52;c;?\x07");
    assert!(term.take_events().is_empty());
}
