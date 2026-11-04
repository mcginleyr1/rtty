//! What the shell's marks (OSC 133) say about a pane's rows: where prompts start. A prompt that
//! marks itself as well as rtty's integration does marks each thing twice, one right after the
//! other on one row; such a run counts as its first.

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
        // A prompt that marks only its start: each one counts.
        let starts = self::term(10, 20, &format!("{A}$ ls\r\na\r\n{A}$ ls\r\na\r\n{A}$ "));
        assert_eq!(prompts(&starts), [0, 2, 4]);
    }
}
