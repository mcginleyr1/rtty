//! Replies to the queries a program sends, from xterm's ctlseqs, the VT510
//! manual, kitty's keyboard protocol and contour's colour-scheme reports,
//! and the probes recorded programs make.

mod common;

use rtty_vt::{
    BACKGROUND, CURSOR, ColorScheme, Event, FOREGROUND, ModeChange, Rgb, Terminal, XTERM_VERSION,
};

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
    // An xterm patch level emacs (242) and vim (277) turn their xterm
    // features on for.
    assert_eq!(ask("\x1b[>c"), ["\x1b[>1;277;0c"]);
    assert_eq!(ask("\x1b[>0c"), [format!("\x1b[>1;{XTERM_VERSION};0c")]);
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
    // nvim's startup probes: synchronized output is supported, left and
    // right margins, mode 2027 and in-band resize reports are not.
    assert_eq!(
        ask("\x1b[?69$p\x1b[?2026$p\x1b[?2027$p\x1b[?2031$p\x1b[?2048$p"),
        ["\x1b[?69;0$y", "\x1b[?2026;2$y", "\x1b[?2027;0$y", "\x1b[?2031;2$y", "\x1b[?2048;0$y"]
    );
    assert_eq!(ask("\x1b[?2026h\x1b[?2026$p"), ["\x1b[?2026;1$y"]);
    assert_eq!(ask("\x1b[?1048$p"), ["\x1b[?1048;2$y"]);
}

#[test]
fn modify_other_keys_is_tracked_announced_and_reported() {
    let mut term = Terminal::new(24, 80, 0);
    assert_eq!(replies(&mut term, "\x1b[?4m"), ["\x1b[>4;0m"]);
    term.advance(b"\x1b[>4;2m");
    assert_eq!(term.modes().modify_other_keys, 2);
    assert_eq!(term.take_events(), [Event::Mode(ModeChange::ModifyOtherKeys(2))]);
    assert_eq!(replies(&mut term, "\x1b[?4m"), ["\x1b[>4;2m"]);
    term.advance(b"\x1b[>4;0m\x1b[>4;1m\x1b[>4n");
    assert_eq!(
        term.take_events(),
        [0, 1, 0].map(|level| Event::Mode(ModeChange::ModifyOtherKeys(level)))
    );
    term.advance(b"\x1b[>4;2m\x1bc");
    assert_eq!(term.modes().modify_other_keys, 0, "a reset turns it off");
}

/// vim's probe: a terminal must swallow a DCS and a CSI it does not know.
#[test]
fn unknown_sequences_neither_print_nor_move_the_cursor() {
    let mut term = Terminal::new(24, 80, 0);
    assert_eq!(replies(&mut term, "\x1bPzz\x1b\\\x1b[0%m\x1b[6n"), ["\x1b[1;1R"]);
    assert_eq!(term.contents(), "");
}

/// Queries are answered as they arrive, a synchronized update open or not.
#[test]
fn queries_inside_a_synchronized_update_are_answered() {
    assert_eq!(
        ask("\x1b[?2026h\x1b[5n\x1b[c\x1bP+q4d73\x1b\\\x1b[?2026l"),
        ["\x1b[0n", "\x1b[?62;22c", "\x1bP1+r4d73=1B5D35323B25703125733B257032257307\x1b\\"]
    );
}

/// Every query a recorded program sent (`fixtures/*.json`) is answered by
/// an engine whose host set its colours, colour scheme and cell size.
#[test]
fn every_query_the_recorded_programs_sent_is_answered() {
    let mut unanswered = Vec::new();
    for fixture in common::fixtures() {
        let mut term = Terminal::new(fixture.rows, fixture.cols, 0);
        for slot in [FOREGROUND, CURSOR] {
            term.set_host_color(slot, Some(Rgb(0xdd, 0xdd, 0xdd)));
        }
        term.set_host_color(BACKGROUND, Some(Rgb(0x1c, 0x1c, 0x1c)));
        term.set_color_scheme(Some(ColorScheme::Dark));
        term.set_cell_pixels(8, 16);
        let mut fed = 0;
        for (offset, query) in &fixture.queries {
            term.advance(&fixture.output[fed..*offset]);
            fed = *offset;
            let answers = replies(&mut term, "");
            println!("{}\t{query}\t{answers:?}", fixture.name);
            if answers.is_empty() {
                unanswered.push(format!("{} at byte {offset}: {query}", fixture.name));
            }
        }
    }
    assert!(unanswered.is_empty(), "{unanswered:#?}");
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
    // nvim draws curly, dotted, dashed and double underlines only when this
    // probe's answer holds `4:3`.
    assert_eq!(ask("\x1b[0m\x1b[4:3m\x1bP$qm\x1b\\"), ["\x1bP1$r0;4:3m\x1b\\"]);
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
