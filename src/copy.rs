//! Copy mode, after alacritty's vi mode and WezTerm's and iTerm2's copy modes: a cursor by
//! absolute row that vi's keys move through a pane's screen and scrollback, selecting characters,
//! lines or a block to copy. Independent of GPUI but for its keystrokes: the view draws the
//! cursor, scrolls to keep it in view and does what a key asks (`Effect`).

use std::mem;

use gpui::Keystroke;
use rtty_vt::Terminal;

use crate::prompt;
use crate::search::Match;
use crate::selection::{self, Kind, Point, Selection, Side};

/// Copy mode in a pane: where its cursor is, and what has been typed toward a motion.
#[derive(Clone, Debug, PartialEq)]
pub struct Mode {
    /// The cell under the cursor, by absolute row.
    pub cursor: Point,
    /// A count being typed: the `5` of `5j`.
    count: Option<usize>,
    /// The first `g` of `gg`.
    g: bool,
    /// `?` started the search, so `n` goes up and `N` down.
    up: bool,
}

/// What a key asks of the view, besides the cursor, the selection and the view's top.
#[derive(Debug, PartialEq)]
pub enum Effect {
    Stay,
    /// `/` or `?`: a new search, its query typed into the search bar.
    Search,
    /// `y`: copy this and leave.
    Yank(String),
    Leave,
}

/// The most times a count repeats a motion.
const MOST: usize = 9999;

impl Mode {
    pub fn new(cursor: Point) -> Mode {
        Mode { cursor, count: None, g: false, up: false }
    }

    /// What has been typed toward a motion: a count, a `g`.
    pub fn pending(&self) -> String {
        let count = self.count.map_or(String::new(), |count| count.to_string());
        if self.g { count + "g" } else { count }
    }

    /// A key typed in copy mode, on a view of `rows` rows from `top`: vi's motions move the
    /// cursor (`h j k l` and the arrows, `w b e W B E`, `0 ^ $`, `H M L`, `gg G`, Ctrl+U, D, B
    /// and F, `[` and `]` to the previous and next prompt, `n N` through `matches`, a count before
    /// any of them), the selection's moving end with it; `v`, `V` and Ctrl+V start, change or end
    /// a selection of characters, lines or a block, and `o` swaps its ends. `top` moves with the
    /// pages and to keep the cursor in view.
    pub fn key(
        &mut self,
        keystroke: &Keystroke,
        term: &Terminal,
        top: &mut u64,
        rows: u16,
        selection: &mut Option<Selection>,
        matches: &[Match],
    ) -> Effect {
        let mods = keystroke.modifiers;
        let key = match keystroke.key_char.as_deref() {
            _ if mods.control || mods.alt || mods.platform || mods.function => keystroke.unparse(),
            Some(typed) => typed.to_owned(),
            None => keystroke.key.clone(),
        };
        let (count, g) = (self.count.take(), mem::take(&mut self.g));
        let digit = key.parse::<usize>().ok().filter(|&n| n < 10 && (n > 0 || count.is_some()));
        if let Some(digit) = digit {
            self.count = Some((count.unwrap_or(0) * 10 + digit).min(MOST));
            return Effect::Stay;
        }
        let times = count.unwrap_or(1);
        let (oldest, bottom) =
            (term.oldest_row(), term.screen_top() + u64::from(term.size().0) - 1);
        let rows = u64::from(rows.max(1));
        let at = self.cursor;
        let down = |by: i64| (at.0.saturating_add_signed(by).clamp(oldest, bottom), at.1);
        let line = |row: u64| {
            let row = row.clamp(oldest, bottom);
            (row, nonblank(term, row, false))
        };
        let mut scroll = 0;
        let to = match key.as_str() {
            "h" | "left" => (at.0, at.1.saturating_sub(times as u16)),
            "l" | "right" => (at.0, at.1.saturating_add(times as u16)),
            "j" | "down" => down(times as i64),
            "k" | "up" => down(-(times as i64)),
            "w" | "b" | "e" | "W" | "B" | "E" => (0..times).fold(at, |at, _| word(term, at, &key)),
            "0" => (at.0, 0),
            "^" => (at.0, nonblank(term, at.0, false)),
            "$" => (at.0, nonblank(term, at.0, true)),
            "H" => line(*top),
            "M" => line(*top + (rows - 1) / 2),
            "L" => line(*top + rows - 1),
            "g" if g => line(oldest),
            "g" => {
                (self.count, self.g) = (count, true);
                at
            }
            "G" => line(bottom),
            "ctrl-u" | "ctrl-d" | "ctrl-b" | "ctrl-f" => {
                let page =
                    if matches!(key.as_str(), "ctrl-u" | "ctrl-d") { rows / 2 } else { rows };
                let page = page as i64 * times as i64;
                scroll = if matches!(key.as_str(), "ctrl-u" | "ctrl-b") { -page } else { page };
                down(scroll)
            }
            "[" | "]" => {
                let mut prompts = prompt::prompts(term);
                prompts.retain(|&row| if key == "[" { row < at.0 } else { row > at.0 });
                if key == "[" {
                    prompts.reverse();
                }
                prompts.get(times - 1).map_or(at, |&row| (row, 0))
            }
            "n" | "N" => {
                let up = (key == "n") == self.up;
                (0..times).fold(at, |at, _| next_match(matches, at, up))
            }
            "v" | "V" | "ctrl-v" => {
                let kind = match key.as_str() {
                    "v" => Kind::Cells,
                    "V" => Kind::Lines,
                    _ => Kind::Block,
                };
                *selection = match *selection {
                    Some(selected) if selected.kind() == kind => None,
                    Some(mut selected) => {
                        selected.set_kind(kind);
                        Some(selected)
                    }
                    None => Some(Selection::new(kind, at, Side::Left)),
                };
                at
            }
            "o" => selection.as_mut().map_or(at, Selection::swap),
            "y" => {
                let line = Selection::new(Kind::Lines, at, Side::Left);
                let text = selection.and_then(|selected| selected.text(term));
                return Effect::Yank(text.or_else(|| line.text(term)).unwrap_or_default());
            }
            "/" | "?" => {
                self.up = key == "?";
                return Effect::Search;
            }
            "escape" | "ctrl-c" | "ctrl-[" => match selection.take() {
                Some(_) => at,
                None => return Effect::Leave,
            },
            "q" => return Effect::Leave,
            _ => at,
        };
        self.go(to, term, selection);
        let shown = self.cursor.0.saturating_sub(rows - 1)..=self.cursor.0;
        *top = top.saturating_add_signed(scroll).clamp(*shown.start(), *shown.end());
        Effect::Stay
    }

    /// Puts the cursor on `to`, kept to the rows held and to a wide character's first cell, and
    /// the selection's moving end with it.
    pub fn go(&mut self, to: Point, term: &Terminal, selection: &mut Option<Selection>) {
        let bottom = term.screen_top() + u64::from(term.size().0) - 1;
        let (row, col) = (to.0.clamp(term.oldest_row(), bottom), to.1.min(term.size().1 - 1));
        let cell = term.row(row).and_then(|cells| cells.cells().get(usize::from(col)).copied());
        self.cursor = (row, col - u16::from(cell.is_some_and(|cell| cell.is_spacer())));
        if let Some(selection) = selection {
            selection.extend_over(self.cursor);
        }
    }
}

/// The first column of `row` that is not blank (with `last`, the last one), else the first.
fn nonblank(term: &Terminal, row: u64, last: bool) -> u16 {
    let shows = |col: &u16| !selection::char_at(term, (row, *col)).is_whitespace();
    let mut cols = 0..term.size().1;
    let found = if last { cols.rev().find(shows) } else { cols.find(shows) };
    found.unwrap_or(0)
}

/// Where `w`, `e` or `b` goes from `at`: the next word's start, the end of this word or the
/// next, the start of this word or the last. A word is a run of word characters or one of other
/// characters but blanks (`selection::in_word`); for `W`, `E` and `B`, a run of anything but
/// blanks. A soft wrap runs a word on; a hard line end is a blank.
fn word(term: &Terminal, at: Point, key: &str) -> Point {
    let big = key.chars().all(char::is_uppercase);
    let class = |at: Point| match selection::char_at(term, at) {
        c if c.is_whitespace() => 0,
        c if big || selection::in_word(c) => 1,
        _ => 2,
    };
    let step = |at: Point, step| selection::neighbour(term, at, step).map_or(at, |(next, _)| next);
    // On over blanks and hard line ends, to the first cell that is neither.
    let skip = |mut at: Point, step| {
        while class(at) == 0
            && let Some((next, _)) = selection::neighbour(term, at, step)
        {
            at = next;
        }
        at
    };
    // To the far end of the run `at` is in, within its line.
    let run = |mut at: Point, step| {
        let run = class(at);
        let same = |&(next, hard): &(Point, bool)| !hard && class(next) == run;
        while let Some((next, _)) = selection::neighbour(term, at, step).filter(same) {
            at = next;
        }
        at
    };
    match key.to_ascii_lowercase().as_str() {
        "w" => skip(step(run(at, 1), 1), 1),
        "e" => run(skip(step(at, 1), 1), 1),
        _ => run(skip(step(at, -1), -1), -1),
    }
}

/// The start of the first match after `at` (before it, `up`), round the ends; `at` with none.
fn next_match(matches: &[Match], at: Point, up: bool) -> Point {
    let mut starts = matches.iter().map(|&(start, _)| start);
    let found = match up {
        true => starts.clone().rev().find(|&start| start < at).or_else(|| starts.next_back()),
        false => starts.clone().find(|&start| start > at).or_else(|| starts.next()),
    };
    found.unwrap_or(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(rows: u16, cols: u16, bytes: &[u8]) -> Terminal {
        let mut term = Terminal::new(rows, cols, 100);
        term.advance(bytes);
        term
    }

    /// Copy mode from `at`, the view on the screen, after `keys` as `--script`'s `keys` step
    /// sends them: the mode, the view's top, the selection, and what the last key asked.
    fn press(
        term: &Terminal,
        at: Point,
        keys: &str,
        matches: &[Match],
    ) -> (Mode, u64, Option<Selection>, Effect) {
        let mut mode = Mode::new(at);
        let (mut top, mut selection, mut effect) = (term.screen_top(), None, Effect::Stay);
        for key in keys.split(' ') {
            let keystroke = Keystroke::parse(key).unwrap().with_simulated_ime();
            effect = mode.key(&keystroke, term, &mut top, term.size().0, &mut selection, matches);
        }
        (mode, top, selection, effect)
    }

    fn cursor(term: &Terminal, at: Point, keys: &str) -> Point {
        press(term, at, keys, &[]).0.cursor
    }

    fn selected(term: &Terminal, at: Point, keys: &str) -> Option<String> {
        press(term, at, keys, &[]).2.and_then(|selection| selection.text(term))
    }

    #[test]
    fn hjkl_and_the_arrows_move_a_cell_or_a_count_of_them_and_stop_at_the_edges() {
        let term = term(5, 10, b"abc\r\ndef\r\nghi");
        assert_eq!(cursor(&term, (0, 0), "l l j"), (1, 2));
        assert_eq!(cursor(&term, (0, 0), "right down down left"), (2, 0));
        assert_eq!(cursor(&term, (0, 0), "5 l 2 j"), (2, 5));
        assert_eq!(cursor(&term, (0, 0), "h k 2 0 l 9 j"), (4, 9));
        // A key that is no motion changes nothing, and drops a count typed before it.
        assert_eq!(cursor(&term, (1, 1), "3 x l"), (1, 2));
    }

    #[test]
    fn word_motions_run_on_across_soft_wraps_and_stop_at_line_ends() {
        // 10 columns: "longword1234" wraps after "longword12".
        let term = term(4, 10, b"foo (bar) longword1234\r\nnext line");
        assert_eq!(cursor(&term, (0, 0), "w"), (0, 4));
        assert_eq!(cursor(&term, (0, 0), "w w w"), (0, 8));
        assert_eq!(cursor(&term, (0, 0), "4 w"), (1, 0));
        assert_eq!(cursor(&term, (1, 0), "w"), (3, 0), "over the hard line end");
        assert_eq!(cursor(&term, (1, 0), "e"), (2, 1), "to the end, across the wrap");
        assert_eq!(cursor(&term, (2, 1), "e"), (3, 3));
        assert_eq!(cursor(&term, (3, 0), "b"), (1, 0), "back to the start, across it");
        assert_eq!(cursor(&term, (0, 6), "b"), (0, 5));
        assert_eq!(cursor(&term, (0, 6), "b b"), (0, 4));
        // By blanks alone, "(bar)" is one word.
        assert_eq!(cursor(&term, (0, 0), "W"), (0, 4));
        assert_eq!(cursor(&term, (0, 0), "W W"), (1, 0));
        assert_eq!(cursor(&term, (0, 4), "E"), (0, 8));
        assert_eq!(cursor(&term, (1, 0), "B"), (0, 4));
    }

    #[test]
    fn zero_caret_and_dollar_take_the_rows_first_column_and_first_and_last_characters() {
        let term = term(3, 12, "  indented \r\nab日".as_bytes());
        assert_eq!(cursor(&term, (0, 5), "0"), (0, 0));
        assert_eq!(cursor(&term, (0, 5), "^"), (0, 2));
        assert_eq!(cursor(&term, (0, 0), "$"), (0, 9));
        // A wide character's first cell, not its second.
        assert_eq!(cursor(&term, (1, 0), "$"), (1, 2));
        assert_eq!(cursor(&term, (2, 4), "$ ^"), (2, 0), "a blank row's are its first column");
    }

    #[test]
    fn the_cursor_walks_into_the_scrollback_and_the_view_keeps_it_in_sight() {
        // 4 rows: "0" to "11" leave rows 0–7 in the scrollback.
        let lines: Vec<String> = (0..12).map(|n| n.to_string()).collect();
        let term = term(4, 10, lines.join("\r\n").as_bytes());
        assert_eq!(term.screen_top(), 8);
        let at = |keys| {
            let (mode, top, ..) = press(&term, (11, 0), keys, &[]);
            (mode.cursor.0, top)
        };
        assert_eq!(at("k k k"), (8, 8));
        assert_eq!(at("k k k k"), (7, 7), "a row up into the scrollback");
        assert_eq!(at("g g"), (0, 0));
        assert_eq!(at("g g G"), (11, 8));
        assert_eq!(at("g g j j j j"), (4, 1));
        // Half pages and pages move the view as far as the cursor.
        assert_eq!(at("ctrl-u"), (9, 6));
        assert_eq!(at("ctrl-b"), (7, 4));
        assert_eq!(at("ctrl-b ctrl-b ctrl-b"), (0, 0));
        assert_eq!(at("g g ctrl-f ctrl-d"), (6, 6));
        // H, M and L: the view's top, middle and bottom rows.
        assert_eq!(at("ctrl-b H"), (4, 4));
        assert_eq!(at("ctrl-b M"), (5, 4));
        assert_eq!(at("ctrl-b L"), (7, 4));
    }

    #[test]
    fn a_count_and_a_g_wait_for_their_motion() {
        let term = term(3, 20, b"some text");
        let pending = |keys| press(&term, (0, 0), keys, &[]).0.pending();
        assert_eq!(pending("1 2"), "12");
        assert_eq!(pending("1 0"), "10");
        assert_eq!(pending("g"), "g");
        assert_eq!(pending("3 g"), "3g");
        assert_eq!(pending("3 g g"), "");
        assert_eq!(cursor(&term, (0, 0), "1 2 l"), (0, 12));
        assert_eq!(cursor(&term, (0, 5), "0"), (0, 0), "0 with no count is a motion");
    }

    #[test]
    fn v_shift_v_and_ctrl_v_select_from_the_cursor_and_o_swaps_the_ends() {
        let term = term(3, 10, b"abcdef\r\nghijkl\r\nmnopqr");
        assert_eq!(selected(&term, (0, 1), "v j l").as_deref(), Some("bcdef\nghi"));
        assert_eq!(selected(&term, (0, 1), "v j l V").as_deref(), Some("abcdef\nghijkl"));
        assert_eq!(selected(&term, (0, 1), "v j l ctrl-v").as_deref(), Some("bc\nhi"));
        assert_eq!(selected(&term, (0, 1), "ctrl-v j l o h").as_deref(), Some("abc\nghi"));
        // Backwards takes both ends' cells in too.
        assert_eq!(selected(&term, (1, 3), "v k h").as_deref(), Some("cdef\nghij"));
        assert_eq!(selected(&term, (1, 3), "ctrl-v k h h").as_deref(), Some("bcd\nhij"));
        // The same key again ends the selection.
        assert_eq!(selected(&term, (0, 1), "v l v"), None);
        let (mode, ..) = press(&term, (0, 1), "v j l o", &[]);
        assert_eq!(mode.cursor, (0, 1));
    }

    #[test]
    fn y_copies_the_selection_else_the_line_and_esc_clears_the_selection_before_leaving() {
        let term = term(3, 10, b"abcdef\r\nghijkl");
        let effect = |keys| press(&term, (0, 1), keys, &[]).3;
        assert_eq!(effect("v l y"), Effect::Yank("bc".into()));
        assert_eq!(effect("y"), Effect::Yank("abcdef".into()));
        assert_eq!(effect("V j y"), Effect::Yank("abcdef\nghijkl".into()));
        let (_, _, selection, effect) = press(&term, (0, 1), "v l escape", &[]);
        assert_eq!((selection, effect), (None, Effect::Stay));
        assert_eq!(press(&term, (0, 1), "v escape escape", &[]).3, Effect::Leave);
        assert_eq!(press(&term, (0, 1), "v q", &[]).3, Effect::Leave);
        assert_eq!(press(&term, (0, 1), "ctrl-c", &[]).3, Effect::Leave);
    }

    #[test]
    fn slash_and_question_ask_for_a_search_whose_matches_n_and_shift_n_go_through() {
        let term = term(3, 10, b"ab x\r\nx cd\r\nef   x");
        let matches = [((0, 3), (0, 3)), ((1, 0), (1, 0)), ((2, 5), (2, 5))];
        let after = |keys| {
            let (mode, _, _, effect) = press(&term, (1, 2), keys, &matches);
            (mode.cursor, effect)
        };
        assert_eq!(after("/"), ((1, 2), Effect::Search));
        assert_eq!(after("/ n").0, (2, 5));
        assert_eq!(after("/ n n").0, (0, 3), "round the bottom");
        assert_eq!(after("/ N").0, (1, 0));
        assert_eq!(after("/ 2 N").0, (0, 3));
        // After ?, n goes up.
        assert_eq!(after("? n").0, (1, 0));
        assert_eq!(after("? N").0, (2, 5));
        assert_eq!(press(&term, (1, 2), "n", &[]).0.cursor, (1, 2), "no matches, no move");
    }

    #[test]
    fn brackets_go_to_the_previous_and_next_prompt() {
        let a = "\x1b]133;A\x07";
        let text = format!("{a}$ one\r\nout\r\n{a}$ two\r\nout\r\n{a}$ three");
        let term = term(6, 20, text.as_bytes());
        assert_eq!(cursor(&term, (4, 3), "["), (2, 0));
        assert_eq!(cursor(&term, (4, 3), "[ ["), (0, 0));
        assert_eq!(cursor(&term, (4, 3), "[ [ ["), (0, 0), "none above the first");
        assert_eq!(cursor(&term, (0, 2), "]"), (2, 0));
        assert_eq!(cursor(&term, (0, 2), "2 ]"), (4, 0));
    }

    #[test]
    fn the_cursor_stays_on_its_text_while_output_scrolls_it_into_the_scrollback() {
        let mut term = term(3, 10, b"keep\r\nme");
        let mut mode = Mode::new((1, 0));
        term.advance(b"\r\n1\r\n2\r\n3\r\n4");
        assert!(term.screen_top() > 1, "the row went into the scrollback");
        let (mut top, mut selection) = (term.screen_top(), None);
        let l = Keystroke::parse("l").unwrap().with_simulated_ime();
        assert_eq!(mode.key(&l, &term, &mut top, 3, &mut selection, &[]), Effect::Stay);
        assert_eq!((mode.cursor, selection::char_at(&term, mode.cursor)), ((1, 1), 'e'));
        assert_eq!(top, 1, "the view went back to it");
    }
}
