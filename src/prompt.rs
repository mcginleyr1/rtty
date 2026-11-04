//! What the shell's marks (OSC 133) say about a pane's rows: where prompts start, the last
//! command's output, a command's command line, and how far a click on the command line being typed
//! is from the shell's cursor. A prompt that marks itself as well as rtty's integration does marks
//! each thing twice, one right after the other on one row; such a run counts as its first.

use std::mem;
use std::ops::Range;

use rtty_vt::{Cell, Mark, MarkKind, Terminal};

/// The marks on every row held, oldest first, each run of one kind on one row as its first.
pub fn marks(term: &Terminal) -> Vec<Mark> {
    let end = term.screen_top() + u64::from(term.size().0);
    let mut marks = term.marks(term.oldest_row(), end);
    marks.dedup_by(|later, earlier| {
        mem::discriminant(&later.kind) == mem::discriminant(&earlier.kind)
            && later.row == earlier.row
    });
    marks
}

/// The rows prompts start on (A).
pub fn prompts(term: &Terminal) -> Vec<u64> {
    let marks = marks(term).into_iter();
    marks.filter(|mark| mark.kind == MarkKind::PromptStart).map(|mark| mark.row).collect()
}

/// The text from `start` up to `end`, as a selection copies it, without the line ends before
/// `end`.
fn between(term: &Terminal, start: &Mark, end: &Mark) -> String {
    let last = match (end.row, end.col) {
        (row, 0) => row.checked_sub(1).map(|row| (row, term.size().1 - 1)),
        (row, col) => Some((row, col - 1)),
    };
    match last.filter(|&last| last >= (start.row, start.col)) {
        Some(last) => term.text((start.row, start.col), last).trim_end_matches('\n').to_owned(),
        None => String::new(),
    }
}

/// The output of the last command that marked where it began (C): up to the mark after that, its
/// end (D), or the next prompt from a shell that marks no end. Scrollback included.
pub fn last_output(term: &Terminal) -> Option<String> {
    let marks = marks(term);
    let output = marks.windows(2).rfind(|pair| pair[0].kind == MarkKind::OutputStart)?;
    Some(between(term, &output[0], &output[1]))
}

/// What was typed at the prompt before the output mark `output` (C): from the prompt's end (B).
pub fn command_line(term: &Terminal, output: &Mark) -> Option<String> {
    let marks = marks(term);
    let at = |mark: &&Mark| (mark.row, mark.col) < (output.row, output.col);
    let typed = marks.iter().rev().find(at).filter(|mark| mark.kind == MarkKind::CommandStart)?;
    Some(between(term, typed, output).trim().to_owned()).filter(|line| !line.is_empty())
}

/// How many characters the shell's cursor goes, right when positive, to reach a click on cell
/// `at` of the command line being typed: the line, soft wraps included, from the end of the last
/// prompt (B) with no command run since. `None` for a click off it, or while the cursor is. A wide
/// character is one character, clicked on either half; a click past the end of what is typed goes
/// to the end.
pub fn cursor_moves(term: &Terminal, at: (u64, u16)) -> Option<isize> {
    let input = *marks(term).last().filter(|mark| mark.kind == MarkKind::CommandStart)?;
    if term.modes().alt_screen {
        return None;
    }
    let mut last_row = input.row;
    while term.row(last_row).is_some_and(|row| row.wrapped()) {
        last_row += 1;
    }
    let cells: Vec<Cell> =
        term.rows(input.row, last_row + 1).flat_map(|(_, row)| row.cells().to_vec()).collect();
    let cols = usize::from(term.size().1);
    let index = |(row, col): (u64, u16)| {
        let row = usize::try_from(row.checked_sub(input.row)?).ok()?;
        Some(row * cols + usize::from(col)).filter(|&index| index <= cells.len())
    };
    let (row, col) = term.cursor_position();
    let cursor =
        index((term.screen_top() + u64::from(row), col))? + usize::from(term.wrap_pending());
    let start = index((input.row, input.col))?;
    let typed = cells.iter().rposition(|cell| cell.ch() != ' ').map_or(0, |last| last + 1);
    let mut target = index(at).filter(|&target| target >= start && cursor >= start)?;
    target = target.min(typed.max(cursor));
    if cells.get(target).is_some_and(Cell::is_spacer) {
        target -= 1;
    }
    let characters = |range: Range<usize>| {
        let cells = cells[range].iter();
        cells.filter(|cell| !cell.is_spacer() && !cell.is_pad()).count() as isize
    };
    Some(if target >= cursor { characters(cursor..target) } else { -characters(target..cursor) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "\x1b]133;A\x07";
    const B: &str = "\x1b]133;B\x07";
    const C: &str = "\x1b]133;C\x07";

    fn term(rows: u16, cols: u16, text: &str) -> Terminal {
        let mut term = Terminal::new(rows, cols, 100);
        term.advance(text.as_bytes());
        term
    }

    fn end(status: i32) -> String {
        format!("\x1b]133;D;{status}\x07")
    }

    #[test]
    fn a_doubled_mark_counts_once() {
        // Another integration's marks inside rtty's, as a prompt that marks itself has them.
        let text = format!("{A}{A}$ {B}{B}ls\r\n{C}{C}a b\r\n{}{}{A}{A}$ {B}{B}", end(0), end(0));
        let term = term(10, 20, &text);
        let kinds: Vec<MarkKind> = marks(&term).iter().map(|mark| mark.kind).collect();
        use MarkKind::*;
        let ran = CommandEnd { exit: Some(0) };
        assert_eq!(kinds, [PromptStart, CommandStart, OutputStart, ran, PromptStart, CommandStart]);
        assert_eq!(prompts(&term), [0, 2]);
        assert_eq!(last_output(&term).as_deref(), Some("a b"));
        // A prompt that marks only its start: each one counts.
        let starts = self::term(10, 20, &format!("{A}$ ls\r\na\r\n{A}$ ls\r\na\r\n{A}$ "));
        assert_eq!(prompts(&starts), [0, 2, 4]);
    }

    #[test]
    fn the_last_output_runs_from_its_mark_to_the_commands_end_through_the_scrollback() {
        let lines: String = (1..=30).map(|n| format!("line {n}\r\n")).collect();
        let text = format!(
            "{A}$ {B}echo first\r\n{C}first\r\n{}{A}$ {B}seq\r\n{C}{lines}{}{A}$ {B}",
            end(0),
            end(1)
        );
        let term = term(5, 20, &text);
        let output = last_output(&term).unwrap();
        assert!(output.starts_with("line 1\nline 2\n") && output.ends_with("line 29\nline 30"));
        assert_eq!(output.lines().count(), 30);
    }

    #[test]
    fn output_without_a_line_end_stops_where_the_end_was_marked() {
        // zsh's PROMPT_SP mark after the output, past the end rtty's integration marks.
        let text = format!("{A}% {B}printf foo\r\n{C}foo{}%   \r\n{}{A}% {B}", end(0), end(0));
        assert_eq!(last_output(&term(10, 20, &text)).as_deref(), Some("foo"));
        let none = format!("{A}% {B}true\r\n{C}{}{A}% {B}", end(0));
        assert_eq!(last_output(&term(10, 20, &none)).as_deref(), Some(""));
        // A shell that marks no end: up to the next prompt.
        let endless = format!("{A}% {B}ls\r\n{C}a\r\n{A}% {B}");
        assert_eq!(last_output(&term(10, 20, &endless)).as_deref(), Some("a"));
    }

    #[test]
    fn a_running_command_or_none_has_no_output_yet() {
        assert_eq!(last_output(&term(10, 20, &format!("{A}$ {B}"))), None);
        let running = format!("{A}$ {B}ls\r\n{C}a\r\n{}{A}$ {B}sleep 9\r\n{C}", end(0));
        assert_eq!(last_output(&term(10, 20, &running)).as_deref(), Some("a"), "the last one done");
        // An empty line runs no command: the last output stays the last.
        let empty = format!("{A}$ {B}ls\r\n{C}a\r\n{}{A}$ {B}\r\n{}{A}$ {B}", end(0), end(0));
        assert_eq!(last_output(&term(10, 20, &empty)).as_deref(), Some("a"));
    }

    #[test]
    fn a_commands_line_is_what_was_typed_after_its_prompt() {
        let text = format!("{A}~ $ {B}make   test\r\n{C}");
        let term = term(10, 20, &text);
        let output = *marks(&term).last().unwrap();
        assert_eq!(command_line(&term, &output).as_deref(), Some("make   test"));
        // Soft-wrapped onto a second row.
        let long = format!("{A}$ {B}echo 0123456789abcdefghij\r\n{C}");
        let term = self::term(10, 12, &long);
        let output = *marks(&term).last().unwrap();
        assert_eq!(command_line(&term, &output).as_deref(), Some("echo 0123456789abcdefghij"));
        let unmarked = format!("{A}$ ls\r\n{C}");
        let term = self::term(10, 12, &unmarked);
        assert_eq!(command_line(&term, marks(&term).last().unwrap()), None);
    }

    #[test]
    fn a_click_on_the_command_line_moves_the_cursor_by_characters() {
        // "$ echo hello" with the cursor at its end, column 12.
        let term = term(10, 40, &format!("{A}$ {B}echo hello"));
        assert_eq!(cursor_moves(&term, (0, 7)), Some(-5), "onto the h");
        assert_eq!(cursor_moves(&term, (0, 2)), Some(-10), "the first character");
        assert_eq!(cursor_moves(&term, (0, 30)), Some(0), "past the end: the end");
        assert_eq!(cursor_moves(&term, (0, 0)), None, "on the prompt");
        assert_eq!(cursor_moves(&term, (1, 3)), None, "below the line");
        // Back at the start, a click goes right.
        let mut term = term;
        term.advance(b"\x1b[3G");
        assert_eq!(cursor_moves(&term, (0, 11)), Some(9));
    }

    #[test]
    fn wide_characters_are_one_character_on_either_half() {
        let term = term(10, 40, &format!("{A}$ {B}echo 日本語 ok"));
        // "$ echo " then 日, 本 and 語 at columns 7, 9 and 11, " ok" and the cursor at 16.
        assert_eq!(cursor_moves(&term, (0, 7)), Some(-6));
        assert_eq!(cursor_moves(&term, (0, 8)), Some(-6));
        assert_eq!(cursor_moves(&term, (0, 13)), Some(-3), "the space after 語");
    }

    #[test]
    fn a_soft_wrapped_command_line_is_one_line() {
        // 10 columns: "$ " and 15 characters wrap onto a second row.
        let term = term(10, 10, &format!("{A}$ {B}0123456789abcde"));
        assert_eq!(term.cursor_position(), (1, 7));
        assert_eq!(cursor_moves(&term, (0, 4)), Some(-13), "onto the 2");
        assert_eq!(cursor_moves(&term, (1, 0)), Some(-7), "onto the 8");
        // "$ 0123456" fills 9 of 10 columns: 日 goes to the next row, leaving a pad.
        let wide = self::term(10, 10, &format!("{A}$ {B}0123456日"));
        assert_eq!(cursor_moves(&wide, (1, 0)), Some(-1), "onto 日");
        assert_eq!(cursor_moves(&wide, (0, 8)), Some(-2), "onto the 6, past the pad");
    }

    #[test]
    fn no_command_line_while_a_command_runs_or_on_another_screen() {
        let running = term(10, 40, &format!("{A}$ {B}sleep 5\r\n{C}"));
        assert_eq!(cursor_moves(&running, (0, 4)), None);
        let elsewhere = term(10, 40, &format!("{A}$ {B}ls\r\nmore output"));
        assert_eq!(cursor_moves(&elsewhere, (0, 3)), None, "the cursor left the line");
        let mut alternate = term(10, 40, &format!("{A}$ {B}vim"));
        alternate.advance(b"\x1b[?1049h");
        assert_eq!(cursor_moves(&alternate, (0, 3)), None);
    }
}
