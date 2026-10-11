//! The grid: writing, wrapping, cursor movement, editing, scroll regions,
//! character sets, tab stops, saved cursors, the screens, resets, damage.

use rtty_vt::{Attrs, Color, Terminal, Underline};

fn fed(rows: u16, cols: u16, bytes: &str) -> Terminal {
    let mut term = Terminal::new(rows, cols, 100);
    term.advance(bytes.as_bytes());
    term
}

fn screen(term: &Terminal) -> Vec<String> {
    (0..term.size().0).map(|r| term.screen_row(r).text()).collect()
}

#[test]
fn text_wraps_at_the_margin_and_marks_the_row() {
    let term = fed(3, 5, "abcdefgh");
    assert_eq!(screen(&term), ["abcde", "fgh", ""]);
    assert!(term.screen_row(0).wrapped() && !term.screen_row(1).wrapped());
    assert_eq!(term.cursor_position(), (1, 3));
    // A full row holds the cursor on its last column until the next
    // character.
    let term = fed(3, 5, "abcde");
    assert_eq!((term.cursor_position(), term.wrap_pending()), ((0, 4), true));
    assert!(!term.screen_row(0).wrapped());
}

#[test]
fn without_autowrap_the_last_column_is_overwritten() {
    let term = fed(2, 5, "\x1b[?7labcdefg");
    assert_eq!(screen(&term), ["abcdg", ""]);
}

#[test]
fn rewriting_or_erasing_a_wrapped_rows_end_unwraps_it() {
    for unwrap in ["\x1b[1;5Hz", "\x1b[1;3H\x1b[K", "\x1b[1;1H\x1b[P"] {
        let term = fed(3, 5, &format!("abcdefg{unwrap}"));
        assert!(!term.screen_row(0).wrapped(), "{unwrap:?}");
    }
    let term = fed(3, 5, "abcdefg\x1b[1;1H\x1b[1K");
    assert!(term.screen_row(0).wrapped(), "the end survives an erase to the left");
}

#[test]
fn sgr_renditions_colours_and_underlines() {
    let term = fed(
        1,
        20,
        "\x1b[1;2;3;5;7;8;9;53m1\x1b[0;4:3;58:2::1:2:3m2\x1b[21m3\x1b[24;31;102m4\x1b[38;5;200;48:2::9:8:7m5\x1b[39;49;4m6",
    );
    let row = term.screen_row(0);
    let cell = |col: usize| row.cells()[col];
    let all = [
        Attrs::BOLD,
        Attrs::DIM,
        Attrs::ITALIC,
        Attrs::BLINK,
        Attrs::INVERSE,
        Attrs::HIDDEN,
        Attrs::STRIKE,
        Attrs::OVERLINE,
    ];
    assert!(all.iter().all(|&attr| cell(0).attrs().contains(attr)));
    assert_eq!(cell(1).attrs().underline(), Underline::Curly);
    assert_eq!(row.underline_color(1), Color::Rgb(1, 2, 3));
    assert_eq!(cell(2).attrs().underline(), Underline::Double);
    assert_eq!((cell(3).fg(), cell(3).bg()), (Color::Named(1), Color::Named(10)));
    assert_eq!(cell(3).attrs().underline(), Underline::None);
    assert_eq!((cell(4).fg(), cell(4).bg()), (Color::Indexed(200), Color::Rgb(9, 8, 7)));
    assert_eq!((cell(5).fg(), cell(5).bg()), (Color::Default, Color::Default));
    assert_eq!(cell(5).attrs().underline(), Underline::Single);
    assert_eq!(row.underline_color(5), Color::Rgb(1, 2, 3));
}

#[test]
fn erasing_takes_the_current_background() {
    let term = fed(2, 6, "abcdef\x1b[44m\x1b[1;3H\x1b[K");
    let cells = term.screen_row(0).cells();
    assert_eq!(term.screen_row(0).text(), "ab");
    assert!(
        cells[2..].iter().all(|cell| cell.bg() == Color::Named(4) && cell.fg() == Color::Default)
    );
}

#[test]
fn cursor_movement_stays_in_bounds_and_inside_margins() {
    let mut term = fed(10, 10, "\x1b[5;5H\x1b[20A");
    assert_eq!(term.cursor_position(), (0, 4));
    term.advance(b"\x1b[3;7r\x1b[5;5H\x1b[20A");
    assert_eq!(term.cursor_position(), (2, 4), "CUU stops at the top margin");
    term.advance(b"\x1b[20B");
    assert_eq!(term.cursor_position(), (6, 4), "CUD at the bottom one");
    term.advance(b"\x1b[9;1H\x1b[20B\x1b[20C");
    assert_eq!(term.cursor_position(), (9, 9), "below the region, the screen's edge");
    term.advance(b"\x1b[2E\x1b[3F");
    assert_eq!(term.cursor_position(), (6, 0));
    term.advance(b"\x1b[4G\x1b[2d");
    assert_eq!(term.cursor_position(), (1, 3));
}

#[test]
fn origin_mode_addresses_the_region() {
    let mut term = fed(10, 10, "\x1b[3;6r\x1b[?6h");
    assert_eq!(term.cursor_position(), (2, 0));
    term.advance(b"\x1b[2;3H");
    assert_eq!(term.cursor_position(), (3, 2));
    term.advance(b"\x1b[20;1H");
    assert_eq!(term.cursor_position(), (5, 0), "clamped to the region");
    term.advance(b"\x1b[?6l");
    assert_eq!(term.cursor_position(), (0, 0));
}

#[test]
fn scroll_regions_scroll_inside_only() {
    let mut term = fed(5, 4, "0\r\n1\r\n2\r\n3\r\n4\x1b[2;4r\x1b[4;1H\n");
    assert_eq!(screen(&term), ["0", "2", "3", "", "4"]);
    assert_eq!(term.screen_top(), 0, "an inner region pushes nothing back");
    term.advance(b"\x1b[2;1H\x1bM");
    assert_eq!(screen(&term), ["0", "", "2", "3", "4"]);
    term.advance(b"\x1b[S");
    assert_eq!(screen(&term), ["0", "2", "3", "", "4"]);
    term.advance(b"\x1b[2T");
    assert_eq!(screen(&term), ["0", "", "", "2", "4"]);
    // A region at the top pushes into the scrollback.
    term.advance(b"\x1b[1;2r\x1b[2;1H\n");
    assert_eq!(term.screen_top(), 1);
    assert_eq!(term.row(0).unwrap().text(), "0");
}

#[test]
fn insert_delete_and_erase() {
    let mut term = fed(3, 8, "abcdefgh\x1b[1;3H\x1b[2@");
    assert_eq!(screen(&term)[0], "ab  cdef");
    term.advance(b"\x1b[3P");
    assert_eq!(screen(&term)[0], "abdef");
    term.advance(b"\x1b[2X");
    assert_eq!(screen(&term)[0], "ab  f");
    term.advance(b"\x1b[1;4H\x1b[1K");
    assert_eq!(screen(&term)[0], "    f");
    term.advance(b"\x1b[2K");
    assert_eq!(screen(&term)[0], "");
    let mut term = fed(4, 3, "a\r\nb\r\nc\r\nd\x1b[2;1H\x1b[L");
    assert_eq!(screen(&term), ["a", "", "b", "c"]);
    term.advance(b"\x1b[2M");
    assert_eq!(screen(&term), ["a", "c", "", ""]);
    let mut term = fed(3, 3, "abc\r\ndef\r\nghi\x1b[2;2H\x1b[J");
    assert_eq!(screen(&term), ["abc", "d", ""]);
    term.advance(b"\x1b[1;2H\x1b[1J");
    assert_eq!(screen(&term), ["  c", "d", ""]);
}

#[test]
fn insert_mode_shifts_the_row() {
    let term = fed(1, 6, "abcd\x1b[1;2H\x1b[4hXY");
    assert_eq!(screen(&term), ["aXYbcd"]);
}

#[test]
fn newline_mode_returns_the_carriage() {
    let term = fed(3, 6, "\x1b[20hab\ncd");
    assert_eq!(screen(&term), ["ab", "cd", ""]);
}

#[test]
fn repeat_repeats_the_last_character() {
    let term = fed(1, 10, "ab\x1b[3b");
    assert_eq!(screen(&term), ["abbbb"]);
}

#[test]
fn character_sets() {
    let term = fed(3, 10, "\x1b(0lqk\x1b(B x\r\n\x1b)0a\x0eq\x0fq\r\n\x1b(A#");
    assert_eq!(screen(&term), ["┌─┐ x", "a─q", "£"]);
}

#[test]
fn tab_stops() {
    let mut term = fed(2, 30, "\ta\tb");
    assert_eq!(screen(&term)[0], format!("{}a{}b", " ".repeat(8), " ".repeat(7)));
    term.advance(b"\r\n\x1b[3g\x1b[5G\x1bH\x1b[12G\x1bH\r\tx\ty\tz");
    assert_eq!(
        screen(&term)[1],
        format!("{}x{}y{}z", " ".repeat(4), " ".repeat(6), " ".repeat(17))
    );
    term.advance(b"\x1b[2;1H\x1b[2I");
    assert_eq!(term.cursor_position(), (1, 11));
    term.advance(b"\x1b[Z");
    assert_eq!(term.cursor_position(), (1, 4));
}

#[test]
fn saved_cursor_restores_position_rendition_sets_and_origin() {
    let mut term = fed(10, 10, "\x1b[3;8r\x1b[?6h\x1b[2;3H\x1b[31m\x1b(0\x1b7");
    term.advance(b"\x1b[?6l\x1b[0m\x1b(B\x1b[9;9H\x1b8q");
    assert_eq!(term.cursor_position(), (3, 3));
    assert!(term.modes().origin);
    let cell = term.screen_row(3).cells()[2];
    assert_eq!((cell.ch(), cell.fg()), ('─', Color::Named(1)));
    // With nothing saved, a restore goes home in the default rendition.
    let term = fed(5, 5, "\x1b[3;3H\x1b[1m\x1b8x");
    assert_eq!(term.screen_row(0).cells()[0].attrs(), Attrs::default());
}

#[test]
fn the_alternate_screen() {
    // 1049 saves the cursor, clears the alternate screen and restores both.
    let mut term = fed(3, 10, "primary\x1b[?1049h");
    assert_eq!(screen(&term), ["", "", ""]);
    assert_eq!(term.cursor_position(), (0, 7));
    term.advance(b"\x1b[2;2Halt\x1b[?1049l");
    assert_eq!((screen(&term)[0].as_str(), term.cursor_position()), ("primary", (0, 7)));
    term.advance(b"\x1b[?1049h");
    assert_eq!(screen(&term), ["", "", ""], "cleared on the way in");
    // 47 keeps the alternate screen's contents and the cursor; 1047 clears
    // it on the way out.
    let mut term = fed(3, 10, "p\x1b[?47hone\x1b[?47l");
    assert_eq!((screen(&term)[0].as_str(), term.cursor_position()), ("p", (0, 4)));
    term.advance(b"\x1b[?47h");
    assert_eq!(screen(&term)[0], " one", "the cursor is shared");
    term.advance(b"\x1b[?1047l\x1b[?47h");
    assert_eq!(screen(&term)[0], "");
}

#[test]
fn full_and_soft_resets() {
    let mut term = fed(3, 10, "a\r\nb\r\nc\r\nd\x1b[2;3r\x1b[?1h\x1b[4h\x1b[31m\x1b(0\x1b[?1049h");
    let top = term.screen_top();
    term.advance(b"\x1bc");
    assert_eq!(screen(&term), ["", "", ""]);
    assert_eq!((term.cursor_position(), term.scroll_region()), ((0, 0), (0, 2)));
    assert!(!term.modes().alt_screen && !term.modes().cursor_keys && !term.modes().insert);
    assert_eq!((term.oldest_row(), term.screen_top()), (top, top), "numbering carries on");
    term.advance(b"q");
    assert_eq!(term.screen_row(0).cells()[0].fg(), Color::Default);
    assert_eq!(term.screen_row(0).text(), "q");
    let mut term = fed(3, 10, "keep\x1b[2;3r\x1b[?6h\x1b[4h\x1b[?25l\x1b[1m\x1b[!p");
    assert_eq!(screen(&term)[0], "keep");
    assert_eq!(term.scroll_region(), (0, 2));
    assert!(!term.modes().origin && !term.modes().insert && term.modes().cursor_visible);
    term.advance(b"\x1b[3;1Hx");
    assert_eq!(term.screen_row(2).cells()[0].attrs(), Attrs::default());
}

#[test]
fn screen_alignment_fills_with_e() {
    let term = fed(2, 3, "\x1b[2;2r\x1b[31m\x1b#8");
    assert_eq!(screen(&term), ["EEE", "EEE"]);
    assert_eq!((term.cursor_position(), term.scroll_region()), ((0, 0), (0, 1)));
}

#[test]
fn damage_names_the_rows_that_changed() {
    let mut term = Terminal::new(4, 10, 100);
    assert_eq!(term.take_damage(), [0, 1, 2, 3], "everything, at first");
    assert!(term.take_damage().is_empty());
    term.advance(b"\x1b[3;1Hhello");
    assert_eq!(term.take_damage(), [2]);
    term.advance(b"\x1b[4;1H\n");
    assert_eq!(term.take_damage(), [0, 1, 2, 3], "a scroll moves every row");
    term.advance(b"\x1b[?1049h");
    assert_eq!(term.take_damage(), [0, 1, 2, 3]);
    term.resize(5, 10);
    assert_eq!(term.take_damage(), [0, 1, 2, 3, 4]);
}

#[test]
fn contents_and_selections_join_soft_wraps() {
    let term = fed(5, 5, "abcdefg\r\nhi  \r\n\r\n");
    assert_eq!(term.contents(), "abcdefg\nhi");
    let top = term.screen_top();
    assert_eq!(term.text((top, 3), (top + 1, 1)), "defg");
    assert_eq!(term.text((top, 0), (top + 2, 4)), "abcdefg\nhi");
}

#[test]
fn nothing_panics_on_a_one_column_screen() {
    let mut term = fed(3, 1, "日本\u{301}a🇺🇸👍🏽\t\x1b[5@\x1b[5P\x1b[3X\x1b[2;1r\n\n\x1b[L\x1b[M");
    term.resize(2, 4);
    term.advance("日本".as_bytes());
    term.resize(1, 1);
    let _ = term.snapshot();
}
