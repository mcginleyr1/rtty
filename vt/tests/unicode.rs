//! Widths and grapheme clusters, as the crate documentation states them.

use rtty_vt::Terminal;

fn fed(cols: u16, text: &str) -> Terminal {
    let mut term = Terminal::new(3, cols, 0);
    term.advance(text.as_bytes());
    term
}

/// The first row's cells: their text and width.
fn cells(term: &Terminal) -> Vec<(String, u8)> {
    let row = term.screen_row(0);
    (0..row.cells().len())
        .filter(|&col| !row.cells()[col].is_spacer())
        .map(|col| {
            let mut text = String::new();
            row.push_grapheme(col, &mut text);
            (text, row.cells()[col].width())
        })
        .filter(|(text, _)| text != " ")
        .collect()
}

fn cell(text: &str, width: u8) -> (String, u8) {
    (text.to_string(), width)
}

#[test]
fn combining_marks_join_the_cell_before() {
    let term = fed(10, "e\u{301}x\u{308}\u{323}");
    assert_eq!(cells(&term), [cell("e\u{301}", 1), cell("x\u{308}\u{323}", 1)]);
    assert_eq!(term.cursor_position(), (0, 2));
    // After a wide character, onto it rather than its spacer.
    let term = fed(10, "日\u{301}");
    assert_eq!(cells(&term), [cell("日\u{301}", 2)]);
}

#[test]
fn emoji_sequences_stay_one_cell_pair() {
    let family = "👨\u{200D}👩\u{200D}👧";
    let term = fed(10, &format!("{family}x"));
    assert_eq!(cells(&term), [cell(family, 2), cell("x", 1)]);
    let term = fed(10, "👍🏽🇺🇸x");
    assert_eq!(cells(&term), [cell("👍🏽", 2), cell("🇺🇸", 2), cell("x", 1)]);
    assert_eq!(term.cursor_position(), (0, 5));
}

#[test]
fn a_narrow_cluster_widens_as_wcwidth_sums_it() {
    // Regional indicators are one column each: a flag is two.
    let term = fed(10, "a🇺🇸b");
    assert_eq!(cells(&term), [cell("a", 1), cell("🇺🇸", 2), cell("b", 1)]);
    // At the last column there is no room: the flag moves to the next row,
    // as a wide character would.
    let mut term = Terminal::new(3, 4, 0);
    term.advance("abc🇺🇸".as_bytes());
    assert_eq!(term.screen_row(0).text(), "abc");
    assert!(term.screen_row(0).cells()[3].is_pad() && term.screen_row(0).wrapped());
    assert_eq!(term.screen_row(1).cluster(0), Some("🇺🇸"));
    assert_eq!(term.cursor_position(), (1, 2));
}

#[test]
fn variation_selector_16_joins_without_widening() {
    let term = fed(10, "\u{2764}\u{FE0F}x");
    assert_eq!(cells(&term), [cell("\u{2764}\u{FE0F}", 1), cell("x", 1)]);
}

#[test]
fn joining_needs_the_cluster_printed_just_before() {
    // A cursor movement in between: the modifier starts a cell of its own.
    let term = fed(10, "👍\x1b[C\x1b[D🏽");
    assert_eq!(cells(&term), [cell("👍", 2), cell("🏽", 2)]);
    // Combining marks join the cell before the cursor whatever came between.
    let term = fed(10, "e\x1b[31m\u{301}");
    assert_eq!(cells(&term), [cell("e\u{301}", 1)]);
}

#[test]
fn wide_characters_wrap_whole_and_leave_a_pad() {
    let mut term = Terminal::new(3, 5, 0);
    term.advance("abcd日".as_bytes());
    let row = term.screen_row(0);
    assert!(row.cells()[4].is_pad() && row.wrapped());
    assert_eq!(row.text(), "abcd");
    assert_eq!(term.screen_row(1).text(), "日");
    assert_eq!(term.contents(), "abcd日", "a pad is no text");
}

#[test]
fn overwriting_half_a_wide_character_blanks_the_other_half() {
    let mut term = Terminal::new(1, 6, 0);
    term.advance("日本語\x1b[2Gx\x1b[4G\x1b[X".as_bytes());
    assert_eq!(term.screen_row(0).text(), " x  語");
    assert!(term.screen_row(0).cells().iter().take(4).all(|cell| cell.width() == 1));
}

#[test]
fn a_cluster_stops_growing_at_32_characters() {
    let term = fed(10, &format!("a{}", "\u{301}".repeat(100)));
    assert_eq!(term.screen_row(0).cluster(0).unwrap().chars().count(), 32);
}

#[test]
fn a_utf8_character_split_across_reads_loses_nothing() {
    let text = "é日😀a";
    for cut in 0..=text.len() {
        let mut term = Terminal::new(1, 10, 0);
        term.advance(&text.as_bytes()[..cut]);
        term.advance(&text.as_bytes()[cut..]);
        assert_eq!(term.screen_row(0).text(), text, "cut at {cut}");
    }
    // Byte by byte, and through an escape sequence.
    let mut term = Terminal::new(1, 20, 0);
    for byte in "ü\x1b[1mñ\x1b]2;tí\x07ç".as_bytes() {
        term.advance(std::slice::from_ref(byte));
    }
    assert_eq!((term.screen_row(0).text().as_str(), term.title()), ("üñç", "tí"));
}

#[test]
fn invalid_utf8_shows_replacement_characters() {
    let mut term = Terminal::new(1, 10, 0);
    term.advance(b"a\xffb\xe2\x82c");
    assert_eq!(term.screen_row(0).text(), "a\u{FFFD}b\u{FFFD}c");
}
