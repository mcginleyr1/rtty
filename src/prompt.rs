//! What the shell's marks (OSC 133) say about a pane's rows: where prompts start, the last
//! command's output and a command's command line. A prompt that marks itself as well as rtty's
//! integration does marks each thing twice, one right after the other on one row; such a run
//! counts as its first.

use std::mem;

use rtty_vt::{Mark, MarkKind, Terminal};

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
}
