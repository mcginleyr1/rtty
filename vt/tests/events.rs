//! What the stream asks of the host: titles, bells, notifications, the
//! clipboard, shell integration, user vars, program status, links, modes.

use rtty_vt::{
    Attrs, CursorShape, CursorStyle, Event, Mark, MarkKind, ModeChange, MouseEncoding, MouseMode,
    OSC_MAX, Terminal,
};

fn events(bytes: &str) -> Vec<Event> {
    let mut term = Terminal::new(24, 80, 100);
    term.advance(bytes.as_bytes());
    term.take_events()
}

/// An OSC string is kept up to `OSC_MAX` bytes. One longer is dropped
/// whole, whatever ends it and however the reads cut it, and the stream
/// after it parses as ever: a BEL, CAN or SUB that ends it goes with it, an
/// `ESC` that ends it starts what follows.
#[test]
fn an_osc_string_past_the_limit_is_dropped() {
    let title = |len: usize| format!("\x1b]2;{}", "t".repeat(len - 2));
    let feed = |stream: &[u8], chunk: usize| {
        let mut term = Terminal::new(2, 20, 0);
        for read in stream.chunks(chunk) {
            term.advance(read);
        }
        term
    };
    let kept = [title(OSC_MAX).as_bytes(), b"\x07after"].concat();
    for chunk in [4093, 65_536, kept.len()] {
        let mut term = feed(&kept, chunk);
        assert_eq!(term.title().len(), OSC_MAX - 2, "{chunk}-byte reads");
        assert_eq!(term.take_events().len(), 1);
        assert_eq!(term.screen_row(0).text(), "after");
    }
    for end in ["\x07", "\x1b\\", "\x18", "\x1a", "\x1b[1m", "\x07\x1b]2;next\x07"] {
        let dropped = [title(OSC_MAX + 1).as_bytes(), end.as_bytes(), b"after"].concat();
        for chunk in [1, 4093, 65_536, dropped.len()] {
            // A byte at a time only near the limit: it is slow in a debug build.
            let mut term = Terminal::new(2, 20, 0);
            let (head, tail) = dropped.split_at(if chunk == 1 { OSC_MAX - 8 } else { 0 });
            term.advance(head);
            for read in tail.chunks(chunk) {
                term.advance(read);
            }
            let next = end.ends_with("next\x07").then(|| Event::Title("next".into()));
            assert_eq!(term.take_events(), next.into_iter().collect::<Vec<_>>(), "{end:?}");
            assert_eq!(term.title(), if end.ends_with("next\x07") { "next" } else { "" });
            assert_eq!(term.screen_row(0).text(), "after", "{end:?}, {chunk}-byte reads");
            let bold = term.screen_row(0).cells()[0].attrs().contains(Attrs::BOLD);
            assert_eq!(bold, end == "\x1b[1m", "{end:?}");
        }
    }
}

#[test]
fn titles_and_the_title_stack() {
    assert_eq!(
        events("\x1b]0;both\x07"),
        [Event::IconName("both".into()), Event::Title("both".into())]
    );
    assert_eq!(events("\x1b]1;icon\x1b\\"), [Event::IconName("icon".into())]);
    assert_eq!(events("\x1b]2;a;b\x07"), [Event::Title("a;b".into())]);
    let mut term = Terminal::new(24, 80, 0);
    term.advance(b"\x1b]2;first\x07\x1b[22t\x1b]2;second\x07\x1b[23t");
    assert_eq!(term.title(), "first");
    assert_eq!(term.take_events().last(), Some(&Event::Title("first".into())));
}

#[test]
fn a_run_of_bells_is_one_event() {
    assert_eq!(events("\x07\x07\x07"), [Event::Bell]);
    assert_eq!(
        events("\x07\x1b]2;t\x07\x07"),
        [Event::Bell, Event::Title("t".into()), Event::Bell]
    );
    // The BEL ending an OSC string is no bell.
    assert_eq!(events("\x1b]2;t\x07"), [Event::Title("t".into())]);
}

#[test]
fn notifications_and_progress() {
    let note = |title: Option<&str>, body: Option<&str>| Event::Notification {
        title: title.map(Into::into),
        body: body.map(Into::into),
    };
    assert_eq!(events("\x1b]9;build done\x07"), [note(None, Some("build done"))]);
    assert_eq!(events("\x1b]9;a;b\x1b\\"), [note(None, Some("a;b"))]);
    assert_eq!(
        events("\x1b]777;notify;Title;Body;more\x07"),
        [note(Some("Title"), Some("Body;more"))]
    );
    assert_eq!(events("\x1b]777;notify;;Body\x07"), [note(None, Some("Body"))]);
    assert!(events("\x1b]777;notify;;\x07\x1b]777;other;x\x07\x1b]9;\x07").is_empty());
    // kitty's OSC 99: a title by default, a body by `p=body`, base64 by
    // `e=1`, and chunks with one `i` and `d=0` adding up to the last.
    assert_eq!(events("\x1b]99;;Hello world\x1b\\"), [note(Some("Hello world"), None)]);
    assert_eq!(events("\x1b]99;p=body;just a body\x07"), [note(None, Some("just a body"))]);
    assert_eq!(
        events(
            "\x1b]99;i=1:d=0;Build\x1b\\\x1b]99;i=1:d=0; done\x1b\\\x1b]99;i=1:p=body;all green\x1b\\"
        ),
        [note(Some("Build done"), Some("all green"))]
    );
    assert_eq!(events("\x1b]99;e=1;SGk7dGhlcmU=\x07"), [note(Some("Hi;there"), None)]);
    // Another id starts over; closes, queries and empty payloads say nothing.
    assert_eq!(events("\x1b]99;i=1:d=0;lost\x07\x1b]99;i=2;kept\x07"), [note(Some("kept"), None)]);
    assert!(events("\x1b]99;i=1:p=close;\x07\x1b]99;i=1:p=?;\x07\x1b]99;;\x07").is_empty());
    // ConEmu's numbered subcommands are never notifications; 4 is progress.
    assert_eq!(events("\x1b]9;4;1;50\x07"), [Event::Progress { state: 1, percent: Some(50) }]);
    assert_eq!(events("\x1b]9;4;0\x07"), [Event::Progress { state: 0, percent: None }]);
    assert!(events("\x1b]9;9;/tmp\x07\x1b]9;4;7\x07\x1b]9;12\x07").is_empty());
}

#[test]
fn clipboard_sets_decode_and_reads_go_unanswered() {
    assert_eq!(
        events("\x1b]52;c;aGVsbG8=\x07"),
        [Event::Clipboard { selection: "c".into(), text: "hello".into() }]
    );
    assert!(events("\x1b]52;c;?\x07\x1b]52;c;!!\x07").is_empty());
}

#[test]
fn the_working_directory() {
    let mut term = Terminal::new(24, 80, 0);
    term.advance("\x1b]7;file://host/tmp/caf%C3%A9\x07".as_bytes());
    assert_eq!(term.cwd(), Some("/tmp/café"));
    assert_eq!(term.take_events(), [Event::Cwd("/tmp/café".into())]);
    term.advance(b"\x1b]7;http://elsewhere/x\x07");
    assert_eq!(term.cwd(), Some("/tmp/café"));
}

#[test]
fn user_vars_and_program_status() {
    assert_eq!(
        events("\x1b]1337;SetUserVar=user_name=YmFy\x07"),
        [Event::UserVar { name: "user_name".into(), value: "bar".into() }]
    );
    assert_eq!(
        events("\x1b]1337;SetUserVar=gone=\x07"),
        [Event::UserVar { name: "gone".into(), value: String::new() }]
    );
    assert!(events("\x1b]1337;SetUserVar=bad-name=YmFy\x07\x1b]1337;File=x\x07").is_empty());
    assert_eq!(
        events("\x1b]7501;state=working:app=claude\x07"),
        [Event::ProgramStatus("state=working:app=claude".into())]
    );
}

#[test]
fn shell_marks_land_on_absolute_rows_and_travel_with_them() {
    let mut term = Terminal::new(5, 20, 100);
    term.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07a\r\nb\r\n\x1b]133;D;2\x07");
    let mark = |kind, row, col| Mark { kind, row, col };
    assert_eq!(
        term.marks(0, 10),
        [
            mark(MarkKind::PromptStart, 0, 0),
            mark(MarkKind::CommandStart, 0, 2),
            mark(MarkKind::OutputStart, 1, 0),
            mark(MarkKind::CommandEnd { exit: Some(2) }, 3, 0),
        ]
    );
    let marks: Vec<_> = term
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Mark(mark) => Some(mark),
            _ => None,
        })
        .collect();
    assert_eq!(marks, term.marks(0, 10));
    // Scrolled into the scrollback, the rows keep their numbers and marks.
    term.advance("x\r\n".repeat(10).as_bytes());
    assert!(term.screen_top() > 3);
    assert_eq!(term.marks(0, 4).len(), 4);
    assert_eq!(term.row(1).unwrap().text(), "a");
    // A D with no exit code, and the alternate screen keeps none.
    term.advance(b"\x1b]133;D\x07\x1b[?1049h\x1b]133;A\x07");
    let all = term.marks(0, 100);
    assert_eq!(all.last().unwrap().kind, MarkKind::CommandEnd { exit: None });
    assert_eq!(all.len(), 5);
}

/// nvim killed with SIGKILL, as its shell's integration marks it: with the
/// host's say-so, the reports and key encodings it left on go off at the
/// command's end, however reads cut the stream, and what the shell sets after
/// the mark stays. Bracketed paste, DECCKM, DECKPAM and the alternate screen
/// stay too.
#[test]
fn a_commands_end_turns_off_what_it_left_on() {
    let left = "\x1b]133;C\x07\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[?1004h\x1b[?2031h\x1b[?2004h\
                \x1b[?1h\x1b=\x1b[>4;2m\x1b[>1u";
    let stream = format!("{left}\x1b]133;D;137\x1b\\\x1b]133;A\x07\x1b[?2031h$ ");
    for cut in 0..=stream.len() {
        let mut term = Terminal::new(3, 20, 0);
        term.set_command_end_reset(true);
        term.advance(&stream.as_bytes()[..cut]);
        term.advance(&stream.as_bytes()[cut..]);
        let modes = term.modes();
        assert_eq!((modes.mouse, modes.mouse_encoding), (MouseMode::None, MouseEncoding::Default));
        assert!(!modes.focus_reports, "cut at {cut}");
        assert_eq!((modes.modify_other_keys, term.kitty_keyboard()), (0, 0), "cut at {cut}");
        assert!(modes.color_scheme_reports, "the prompt's own, after the mark");
        assert!(modes.bracketed_paste && modes.cursor_keys && modes.keypad && modes.alt_screen);
    }
    // The mark's event comes first, then each mode that went off.
    let mut term = Terminal::new(3, 20, 0);
    term.set_command_end_reset(true);
    term.advance(b"\x1b[?1000h\x1b[>1u");
    term.take_events();
    term.advance(b"\x1b]133;D\x07");
    let events = term.take_events();
    assert!(matches!(events[0], Event::Mark(Mark { kind: MarkKind::CommandEnd { .. }, .. })));
    assert_eq!(
        events[1..],
        [
            Event::Mode(ModeChange::Mouse(MouseMode::None)),
            Event::Mode(ModeChange::KittyKeyboard(0))
        ]
    );
}

/// Only the first of a run of ends turns anything off: zsh marks one from
/// PROMPT_SP and another from precmd, and what a hook between them turns on
/// is the shell's. A host that never asked keeps everything.
#[test]
fn only_a_commands_first_end_turns_anything_off() {
    let mut term = Terminal::new(3, 20, 0);
    term.set_command_end_reset(true);
    term.advance(b"\x1b]133;D;0\x07\x1b[?1004h\x1b]133;D;0\x07$ ");
    assert!(term.modes().focus_reports, "a hook's, between two ends");
    term.advance(b"\x1b]133;A\x07$ \x1b]133;C\x07\x1b]133;D;0\x07");
    assert!(!term.modes().focus_reports, "a command ran since");

    let mut term = Terminal::new(3, 20, 0);
    term.advance(b"\x1b]133;C\x07\x1b[?1000h\x1b]133;D;0\x07");
    assert_eq!(term.modes().mouse, MouseMode::Click);
}

#[test]
fn hyperlinks_are_shared_by_their_cells() {
    let mut term = Terminal::new(2, 20, 0);
    term.advance(b"a\x1b]8;id=x;https://example.com/a;b\x1b\\link\x1b]8;;\x1b\\z");
    let row = term.screen_row(0);
    assert!(row.hyperlink(0).is_none() && row.hyperlink(5).is_none());
    let link = row.hyperlink(1).unwrap();
    assert_eq!((link.id.as_deref(), link.uri.as_str()), (Some("x"), "https://example.com/a;b"));
    assert!(std::sync::Arc::ptr_eq(link, row.hyperlink(4).unwrap()));
    // SGR 0 leaves the link open.
    term.advance(b"\x1b[2;1H\x1b]8;;u\x07a\x1b[0mb");
    assert_eq!(term.screen_row(1).hyperlink(1).map(|link| link.uri.as_str()), Some("u"));
}

#[test]
fn modes_a_host_acts_on_are_announced_when_they_change() {
    assert_eq!(
        events(
            "\x1b[?1049h\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[?1004h\x1b[?2004h\x1b[?2026h\x1b[?2031h"
        ),
        [
            Event::Mode(ModeChange::AltScreen(true)),
            Event::Mode(ModeChange::Mouse(MouseMode::Click)),
            Event::Mode(ModeChange::MouseEncoding(MouseEncoding::Sgr)),
            Event::Mode(ModeChange::FocusReports(true)),
            Event::Mode(ModeChange::BracketedPaste(true)),
            Event::Mode(ModeChange::SyncUpdate(true)),
            Event::Mode(ModeChange::ColorSchemeReports(true)),
        ]
    );
    // Resetting a mouse mode that is not the active one changes nothing.
    assert_eq!(
        events("\x1b[?1002h\x1b[?1000l\x1b[?1002l"),
        [
            Event::Mode(ModeChange::Mouse(MouseMode::Drag)),
            Event::Mode(ModeChange::Mouse(MouseMode::None))
        ]
    );
    assert_eq!(events("\x1b[>3u"), [Event::Mode(ModeChange::KittyKeyboard(3))]);
}

#[test]
fn cursor_styles() {
    let mut term = Terminal::new(24, 80, 0);
    term.advance(b"\x1b[5 q");
    let bar = Some(CursorStyle { shape: CursorShape::Bar, blinking: true });
    assert_eq!(term.cursor_style(), bar);
    assert_eq!(term.take_events(), [Event::CursorStyle(bar)]);
    term.advance(b"\x1b[4 q");
    assert_eq!(
        term.cursor_style(),
        Some(CursorStyle { shape: CursorShape::Underline, blinking: false })
    );
    term.advance(b"\x1b[0 q\x1b[9 q");
    assert_eq!(term.cursor_style(), None);
}

#[test]
fn a_host_that_never_drains_holds_a_bounded_queue() {
    let mut term = Terminal::new(24, 80, 0);
    for _ in 0..10_000 {
        term.advance(b"\x1b[5n");
    }
    assert_eq!(term.take_events().len(), 4096);
}
