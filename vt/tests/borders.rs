//! The cell-border protocol (draft, version 1): the query, the pen, the
//! rectangle commands, and how borders travel with their cells.

use rtty_vt::{Borders, Color, Event, Stroke, Terminal};

const TOP: u16 = 3;
const RIGHT: u16 = 12;
const LEFT: u16 = 192;
const ALL: u16 = 4095;

/// One command.
fn b(command: &str) -> String {
    format!("\x1bP1+bcell-border;{command}\x1b\\")
}

fn fed(rows: u16, cols: u16, bytes: &str) -> Terminal {
    let mut term = Terminal::new(rows, cols, 100);
    term.set_cell_borders(true);
    term.advance(bytes.as_bytes());
    term
}

fn stroke(width: u8, color: Color) -> Stroke {
    Stroke { width, color, centered: false, above: false, priority: 0 }
}

/// A border set with `stroke` on the masked segments.
fn set(mask: u16, stroke: Stroke) -> Borders {
    let segments = std::array::from_fn(|bit| (mask & (1 << bit) != 0).then_some(stroke));
    Borders { segments, ..Borders::default() }
}

/// The segments each cell of visible row `row` has, as masks.
fn masks(term: &Terminal, row: u16) -> Vec<u16> {
    masks_of(term.screen_row(row), usize::from(term.size().1))
}

fn replies(term: &mut Terminal) -> Vec<String> {
    let replies = term.take_events().into_iter().filter_map(|event| match event {
        Event::Reply(bytes) => Some(String::from_utf8(bytes).unwrap()),
        _ => None,
    });
    replies.collect()
}

const SUPPORT: &str =
    "\x1bP1+bcell-border;ok;1;pen,rect,half,center,palette,round,priority,outside,radius\x1b\\";

#[test]
fn the_query_is_answered_ahead_of_da1() {
    let mut term = fed(2, 10, &(b("q") + "\x1b[c"));
    assert_eq!(replies(&mut term), [SUPPORT, "\x1b[?62;22c"]);
    // Another version, no version, and fields after `q` are not this protocol's query.
    term.advance(
        b"\x1bP2+bcell-border;q\x1b\\\x1bP+bcell-border;q\x1b\\\x1bP1;1+bcell-border;q\x1b\\",
    );
    term.advance((b("q;1") + "\x1bP1+bother;q\x1b\\").as_bytes());
    assert_eq!(replies(&mut term), [""; 0]);
}

#[test]
fn a_terminal_whose_host_draws_no_borders_drops_the_protocol() {
    let mut term = Terminal::new(2, 10, 0);
    term.advance(
        (b("q") + "\x1b[c" + &b("pen;255;16;fg;0;0") + "a" + &b("set;255;16;fg;0;0;1;4"))
            .as_bytes(),
    );
    assert_eq!(replies(&mut term), ["\x1b[?62;22c"]);
    assert_eq!(term.border_pen(), None);
    assert_eq!(masks(&term, 0), [0; 10]);
    assert_eq!(term.screen_row(0).text(), "a");
}

#[test]
fn the_pen_goes_to_cells_written_and_erased_until_sgr_0() {
    let mut term = fed(3, 8, &(b("pen;3;16;p4;0;1") + "ab\x1b[31mc"));
    let top = Stroke { above: true, ..stroke(16, Color::Indexed(4)) };
    assert_eq!(term.border_pen(), Some(&set(TOP, top)), "another SGR leaves the pen");
    assert_eq!(term.screen_row(0).borders(1), Some(&set(TOP, top)));
    assert_eq!(masks(&term, 0), [TOP, TOP, TOP, 0, 0, 0, 0, 0]);
    // Moving the cursor writes nothing; erasing gives the erased cells the pen.
    term.advance(b"\x1b[2;3H\x1b[K\x1b[3;1H\x1b[2X");
    assert_eq!(masks(&term, 1), [0, 0, TOP, TOP, TOP, TOP, TOP, TOP]);
    assert_eq!(masks(&term, 2), [TOP, TOP, 0, 0, 0, 0, 0, 0]);
    // The pen changes a segment at a time, and SGR 0 empties it.
    term.advance((b("pen;1;8;ff8000;1;0;priority=3") + &b("pen-clear;2")).as_bytes());
    let half = Stroke { centered: true, priority: 3, ..stroke(8, Color::Rgb(255, 128, 0)) };
    assert_eq!(term.border_pen(), Some(&set(1, half)));
    term.advance(b"\x1b[m\x1b[1;1Hx\x1b[2;1H\x1b[2K");
    assert_eq!(term.border_pen(), None);
    assert_eq!(
        masks(&term, 0),
        [0, TOP, TOP, 0, 0, 0, 0, 0],
        "a cell written with no pen has none"
    );
    assert_eq!(masks(&term, 1), [0; 8]);
}

#[test]
fn rectangles_change_the_cells_from_the_cursor_and_leave_the_pen() {
    let mut term = fed(4, 6, &("abcdef\x1b[2;3H".to_string() + &b("set;192;4;fg;1;0;9;2")));
    assert_eq!(term.border_pen(), None);
    assert_eq!(term.cursor_position(), (1, 2));
    assert_eq!(masks(&term, 0), [0; 6]);
    for row in 1..4 {
        assert_eq!(masks(&term, row), [0, 0, LEFT, LEFT, 0, 0], "clipped to the screen");
    }
    let left = Stroke { centered: true, ..stroke(4, Color::Default) };
    assert_eq!(term.screen_row(3).borders(3), Some(&set(LEFT, left)));
    // Only the masked segments change; the text stays.
    term.advance(
        (b("set;64;8;p1;0;0;1;1") + &b("set;3;8;p1;0;0;1;9") + "\x1b[3;4H" + &b("clear;4095;1;1"))
            .as_bytes(),
    );
    assert_eq!(masks(&term, 1), [0, 0, LEFT | TOP, LEFT | TOP, TOP, TOP]);
    let cell = term.screen_row(1).borders(2).unwrap();
    assert_eq!(cell.segments[6], Some(stroke(8, Color::Indexed(1))));
    assert_eq!(cell.segments[7], Some(left));
    assert_eq!(masks(&term, 2), [0, 0, LEFT, 0, 0, 0]);
    term.advance(b"\x1b[1;1H");
    term.advance(b("set;4095;1;fg;0;0;1;3").as_bytes());
    assert_eq!(term.screen_row(0).text(), "abcdef");
    assert_eq!(masks(&term, 0), [ALL, ALL, ALL, 0, 0, 0]);
    // Writing or erasing a cell with an empty pen takes its borders away, a rectangle's too.
    term.advance(b"\x1b[1;2HZ\x1b[X");
    assert_eq!(masks(&term, 0), [ALL, 0, 0, 0, 0, 0]);
}

#[test]
fn joints_round_and_square_and_carry_an_outside_colour_and_a_radius() {
    let mut term = fed(2, 6, &b("round;15;1;2;outside=default;radius=64"));
    let rounded = Borders {
        joints: 15,
        outside: Some(Color::Default),
        radius: Some(64),
        ..Borders::default()
    };
    assert_eq!(term.screen_row(0).borders(1), Some(&rounded));
    assert_eq!(term.screen_row(0).borders(2), None);
    // Squaring keeps the colour and the radius; a joint operation without them leaves them.
    term.advance((b("square;5;1;1") + &b("round;16;1;1;outside=p9")).as_bytes());
    let cell = term.screen_row(0).borders(0).unwrap();
    assert_eq!((cell.joints, cell.outside, cell.radius), (26, Some(Color::Indexed(9)), Some(64)));
    term.advance((b("pen-round;240;outside=0a0b0c") + &b("pen-square;16")).as_bytes());
    let pen = term.border_pen().unwrap();
    assert_eq!((pen.joints, pen.outside, pen.radius), (224, Some(Color::Rgb(10, 11, 12)), None));
    term.advance(b("pen-square;255").as_bytes());
    assert_eq!(term.border_pen().unwrap().outside, Some(Color::Rgb(10, 11, 12)), "kept, unused");
}

#[test]
fn a_malformed_command_does_nothing_and_unknown_keys_are_passed_over() {
    let mut term = fed(2, 6, "");
    let long = format!("pen;255;16;fg;0;0{}", ";a=b".repeat(60));
    for bad in [
        "pen;0;16;fg;0;0",
        "pen;4096;16;fg;0;0",
        "pen;255;0;fg;0;0",
        "pen;255;65;fg;0;0",
        "pen;255;16;red;0;0",
        "pen;255;16;p256;0;0",
        "pen;255;16;fffff;0;0",
        "pen;255;16;fg;2;0",
        "pen;255;16;fg;0;2",
        "pen;+255;16;fg;0;0",
        "pen;255;16;fg;0",
        "pen;255;16;fg;0;0;style",
        "pen;255;16;fg;0;0;Style=solid",
        "pen;255;16;fg;0;0;style=",
        "pen-clear;255;1",
        "pen-round;256",
        "pen-round;15;outside=FFFFFF",
        "paint;255",
        "",
        &long,
    ] {
        term.advance(b(bad).as_bytes());
        assert_eq!(term.border_pen(), None, "{bad:?}");
    }
    for bad in [
        "set;255;16;fg;0;0;0;1",
        "set;255;16;fg;0;0;1;65536",
        "set;255;16;fg;0;0;1",
        "clear;255;1;1;x=y",
        "round;15;1",
    ] {
        term.advance(b(bad).as_bytes());
        assert_eq!(masks(&term, 0), [0; 6], "{bad:?}");
    }
    // A style or a key from a later version draws as version 1 does; of a key given twice the
    // last one known counts.
    term.advance(b("pen;0255;016;P7;0;0;style=double;glow=on;priority=2;priority=99").as_bytes());
    let known = Stroke { priority: 2, ..stroke(16, Color::Rgb(0, 0, 0)) };
    assert_eq!(term.border_pen(), None, "P7 is no colour");
    term.advance(
        b("pen;0255;016;AbCdEf;0;0;style=double;glow=on;priority=2;priority=99").as_bytes(),
    );
    assert_eq!(
        term.border_pen(),
        Some(&set(255, Stroke { color: Color::Rgb(0xab, 0xcd, 0xef), ..known }))
    );
    term.advance(b("pen-round;1;radius=0;radius=257;outside=fg").as_bytes());
    let pen = term.border_pen().unwrap();
    assert_eq!((pen.joints, pen.outside, pen.radius), (1, None, None));
}

#[test]
fn a_wide_character_has_no_line_through_it() {
    let mut term = fed(2, 8, &(b("pen;4095;16;fg;0;0") + &b("pen-round;255") + "a日"));
    assert_eq!(masks(&term, 0), [ALL, ALL & !RIGHT, ALL & !LEFT, 0, 0, 0, 0, 0]);
    let joints = |col: usize| term.screen_row(0).borders(col).map(|borders| borders.joints);
    assert_eq!([joints(0), joints(1), joints(2)], [Some(255), Some(!6), Some(!9)]);
    // Overwriting one half blanks the other, which takes the pen.
    term.advance(
        (b("pen-clear;4095") + &b("pen-square;255") + &b("pen;3;8;p2;0;0") + "\x1b[1;3Hx")
            .as_bytes(),
    );
    assert_eq!(term.screen_row(0).text(), "a x");
    assert_eq!(masks(&term, 0), [ALL, TOP, TOP, 0, 0, 0, 0, 0]);
    // A rectangle is of cells: it may give the halves different borders.
    term.advance(b"\x1b[m\x1b[2;1H\xe6\x97\xa5\x1b[2;1H");
    term.advance(b("set;12;8;fg;0;0;1;1").as_bytes());
    assert_eq!(masks(&term, 1), [RIGHT, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn shifted_cells_and_moved_lines_keep_their_borders_and_new_blanks_take_the_pen() {
    let frame = b("set;3;16;fg;0;0;1;3");
    let mut term = fed(4, 6, &("abc\x1b[1;1H".to_string() + &frame + &b("pen;192;16;fg;0;0")));
    term.advance(b"\x1b[2@");
    assert_eq!(masks(&term, 0), [LEFT, LEFT, TOP, TOP, TOP, 0], "ICH");
    term.advance(b"\x1b[3P");
    assert_eq!(masks(&term, 0), [TOP, TOP, 0, LEFT, LEFT, LEFT], "DCH");
    term.advance(b"\x1b[L");
    assert_eq!(masks(&term, 0), [LEFT; 6], "IL");
    assert_eq!(masks(&term, 1), [TOP, TOP, 0, LEFT, LEFT, LEFT]);
    term.advance(b"\x1b[4;1H\n");
    assert_eq!(masks(&term, 0), [TOP, TOP, 0, LEFT, LEFT, LEFT], "a linefeed at the bottom");
    assert_eq!(masks(&term, 3), [LEFT; 6]);
    assert_eq!(masks_of(term.row(0).unwrap(), 6), [LEFT; 6], "in the scrollback");
    term.advance(b"\x1b[1;1H\x1bM");
    assert_eq!(masks(&term, 0), [LEFT; 6], "RI at the top");
    term.advance(b"\x1b[m\x1b[2J");
    assert!((0..4).all(|row| masks(&term, row) == [0; 6]));
    assert_eq!(masks_of(term.row(0).unwrap(), 6), [LEFT; 6], "ED 2 scrolls bordered rows back");
    term.advance(b"\x1b[3J");
    assert_eq!(term.oldest_row(), term.screen_top());
}

fn masks_of(row: &rtty_vt::Row, cols: usize) -> Vec<u16> {
    (0..cols)
        .map(|col| {
            let segments = row.borders(col).map(|borders| borders.segments).unwrap_or_default();
            (0..12).filter(|&bit| segments[bit].is_some()).map(|bit| 1 << bit).sum()
        })
        .collect()
}

#[test]
fn the_pen_is_saved_and_restored_with_the_cursor_and_resets_clear_it() {
    let pen = b("pen;3;16;fg;0;0");
    let mut term = fed(3, 6, &(pen.clone() + "\x1b7\x1b[m\x1b8"));
    assert_eq!(term.border_pen(), Some(&set(TOP, stroke(16, Color::Default))));
    // 1049 saves it going in and restores it coming back; the alternate screen's cells are its own.
    term.advance(b"a\x1b[?1049h");
    term.advance((b("pen-clear;4095") + "\x1b[1;1Hb").as_bytes());
    assert_eq!((term.border_pen(), masks(&term, 0)[0]), (None, 0));
    term.advance(b"\x1b[?1049l");
    assert_eq!(term.border_pen(), Some(&set(TOP, stroke(16, Color::Default))));
    assert_eq!(masks(&term, 0)[0], TOP);
    // DECRC with nothing saved, DECSTR and RIS clear the pen; RIS the cells' borders too.
    let mut term = fed(3, 6, &(pen.clone() + "\x1b8"));
    assert_eq!(term.border_pen(), None);
    term.advance((pen.clone() + "a\x1b[!p").as_bytes());
    assert_eq!((term.border_pen(), masks(&term, 0)[0]), (None, TOP));
    term.advance((pen + "\x1bc").as_bytes());
    assert_eq!((term.border_pen(), masks(&term, 0)[0]), (None, 0));
}

#[test]
fn a_blank_cell_with_borders_is_content_through_scrollback_and_reflow() {
    let mut term = fed(2, 8, &("ab".to_string() + &b("pen;3;16;fg;0;0") + "\x1b[4X\x1b[m\r\n\n\n"));
    assert_eq!(term.row(0).unwrap().cells().len(), 6, "not trimmed with the trailing blanks");
    term.resize(2, 4);
    assert_eq!(masks_of(term.row(0).unwrap(), 4), [0, 0, TOP, TOP]);
    assert_eq!(masks_of(term.row(1).unwrap(), 4), [TOP, TOP, 0, 0]);
    term.resize(2, 8);
    let rejoined = term.row(term.oldest_row()).unwrap();
    assert_eq!(masks_of(rejoined, 8), [0, 0, TOP, TOP, TOP, TOP, 0, 0]);
}

#[test]
fn a_snapshot_and_a_redraw_carry_borders_and_the_pen() {
    let scene = b("set;3;16;p4;1;1;1;6")
        + "\x1b[2;1H"
        + &b("pen;192;8;fg;0;0;priority=2")
        + &b("pen-round;9;outside=default;radius=32")
        + "x日"
        + &b("pen-clear;128")
        + "\x1b[4mé\u{301}\x1b[3;1H\x1b[2X";
    let mut term = fed(4, 8, &scene);
    let seen = |term: &Terminal| -> Vec<Vec<Option<Borders>>> {
        (0..4)
            .map(|row| (0..8).map(|col| term.screen_row(row).borders(col).cloned()).collect())
            .collect()
    };
    let mut copy = fed(4, 8, "");
    copy.advance(&term.snapshot());
    assert_eq!(seen(&copy), seen(&term));
    assert_eq!(copy.border_pen(), term.border_pen());
    assert_eq!(copy.contents(), term.contents());
    term.take_damage();
    term.advance((b("pen;3;4;102030;0;1") + "\x1b[1;2Hq\x1b[4;1H\x1b[K").as_bytes());
    let damage = term.take_damage();
    copy.advance(&term.redraw(&damage));
    assert_eq!(seen(&copy), seen(&term));
    assert_eq!(copy.border_pen(), term.border_pen());
    // A follower that draws no borders shows the same text.
    let mut plain = Terminal::new(4, 8, 0);
    plain.advance(&term.snapshot());
    assert_eq!(plain.contents(), term.contents());
}
