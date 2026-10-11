//! Contour's color palette update notifications. `CSI ? 996 n` asks whether the colors are dark or
//! light; `CSI ? 2031 h` / `CSI ? 2031 l` turn unsolicited reports of palette changes on and off.
//! The answer and the report are both `CSI ? 997 ; 1 n` (dark) or `CSI ? 997 ; 2 n` (light).
//! alacritty_terminal ignores these sequences (it drops the private `n` and reports mode 2031 as
//! unrecognized), so a second parser watches the pty's bytes for them.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::vte::{Params, Parser, Perform};

use crate::session::Listener;

/// Shared by the pty reader thread, which sees the program's requests, and the window, which
/// knows the scheme.
pub struct ColorReports {
    dark: AtomicBool,
    enabled: AtomicBool,
}

impl ColorReports {
    pub fn new(dark: bool) -> Self {
        Self { dark: AtomicBool::new(dark), enabled: AtomicBool::new(false) }
    }

    /// The report to send for a new scheme, when the program asked for reports.
    pub fn scheme_changed(&self, dark: bool) -> Option<String> {
        self.dark.store(dark, Relaxed);
        self.enabled.load(Relaxed).then(|| report(dark))
    }

    /// alacritty answers DECRQM for mode 2031 with "not recognized"; rtty knows the mode.
    pub fn correct_reply(&self, reply: String) -> String {
        if reply == "\x1b[?2031;0$y" { mode_report(self.enabled.load(Relaxed)) } else { reply }
    }
}

pub fn report(dark: bool) -> String {
    format!("\x1b[?997;{}n", if dark { 1 } else { 2 })
}

/// DECRPM for mode 2031: 1 set, 2 reset.
fn mode_report(enabled: bool) -> String {
    format!("\x1b[?2031;{}$y", if enabled { 1 } else { 2 })
}

pub struct Watcher {
    parser: Parser,
    handler: Handler,
}

struct Handler {
    reports: Arc<ColorReports>,
    listener: Listener,
}

impl Watcher {
    /// Answers to `CSI ? 996 n` go out through `listener`, as alacritty's own replies do.
    pub fn new(reports: Arc<ColorReports>, listener: Listener) -> Self {
        Self { parser: Parser::new(), handler: Handler { reports, listener } }
    }

    /// Takes the pty's output in any chunks; a sequence split between reads still counts.
    pub fn watch(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.handler, bytes);
    }
}

impl Perform for Handler {
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _: bool, action: char) {
        if intermediates != b"?" {
            return;
        }
        let mut params = params.iter().map(|param| param[0]);
        match action {
            'h' | 'l' if params.any(|mode| mode == 2031) => {
                self.reports.enabled.store(action == 'h', Relaxed)
            }
            'n' if params.eq([996]) => {
                let answer = report(self.reports.dark.load(Relaxed));
                self.listener.send_event(Event::PtyWrite(answer));
            }
            _ => {}
        }
    }

    /// RIS resets every mode.
    fn esc_dispatch(&mut self, intermediates: &[u8], _: bool, byte: u8) {
        if intermediates.is_empty() && byte == b'c' {
            self.reports.enabled.store(false, Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::channel::mpsc::unbounded;

    fn watcher(dark: bool) -> (Watcher, Arc<ColorReports>, impl FnMut() -> Vec<String>) {
        let (sender, mut events) = unbounded();
        let reports = Arc::new(ColorReports::new(dark));
        let watcher = Watcher::new(reports.clone(), Listener(sender));
        let answers = move || {
            std::iter::from_fn(|| events.try_recv().ok())
                .map(|event| match event {
                    Event::PtyWrite(text) => text,
                    other => panic!("unexpected {other:?}"),
                })
                .collect()
        };
        (watcher, reports, answers)
    }

    #[test]
    fn reports_encode_dark_as_1_and_light_as_2() {
        assert_eq!(report(true).as_bytes(), b"\x1b[?997;1n");
        assert_eq!(report(false).as_bytes(), b"\x1b[?997;2n");
        assert_eq!(mode_report(true).as_bytes(), b"\x1b[?2031;1$y");
        assert_eq!(mode_report(false).as_bytes(), b"\x1b[?2031;2$y");
    }

    #[test]
    fn a_query_is_answered_with_the_current_scheme() {
        let (mut watcher, reports, mut answers) = watcher(true);
        watcher.watch(b"text \x1b[?996n more");
        assert_eq!(answers(), ["\x1b[?997;1n"]);
        reports.scheme_changed(false);
        watcher.watch(b"\x1b[?99");
        watcher.watch(b"6n");
        assert_eq!(answers(), ["\x1b[?997;2n"]);
        watcher.watch(b"\x1b[996n\x1b[?996;1n\x1b[6n");
        assert!(answers().is_empty());
    }

    #[test]
    fn mode_2031_turns_reports_on_and_off() {
        let (mut watcher, reports, _) = watcher(true);
        assert_eq!(reports.scheme_changed(false), None);
        watcher.watch(b"\x1b[?2031h");
        assert_eq!(reports.scheme_changed(true).as_deref(), Some("\x1b[?997;1n"));
        assert_eq!(reports.correct_reply("\x1b[?2031;0$y".into()), "\x1b[?2031;1$y");
        assert_eq!(reports.correct_reply("\x1b[?1049;2$y".into()), "\x1b[?1049;2$y");
        watcher.watch(b"\x1b[?1049;2031l");
        assert_eq!(reports.scheme_changed(false), None);
        assert_eq!(reports.correct_reply("\x1b[?2031;0$y".into()), "\x1b[?2031;2$y");
        watcher.watch(b"\x1b[?2031h\x1bc");
        assert_eq!(reports.scheme_changed(true), None);
    }
}
