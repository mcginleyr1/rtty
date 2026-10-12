//! One terminal: a child process on its own pty, the rtty-vt `Terminal` its output feeds, the
//! threads that move the bytes, and what a window keeps per terminal (where the view sits in the
//! scrollback, the selection). A window lays panes out and draws them; a pane knows no pixels.
//!
//! Three threads per pane. The reader feeds the pty's output to `Terminal::advance` under the
//! lock and hands the engine's replies to the writer, in stream order. The writer owns the pty's
//! input side, so a program that stops reading stalls neither the reader nor the window. The
//! waiter reports the child's exit.

use std::ascii;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use rtty_vt::{ColorScheme, Event, Terminal};

use crate::scheme::{SLOTS, Scheme};
use crate::selection::Selection;

pub const SCROLLBACK: usize = 10_000;

/// What a pane's threads tell the window, in the order it happened.
#[derive(Debug)]
pub enum Notice {
    /// The terminal changed: draw. Not sent again until the window took it (`Pane::drawing`).
    Output,
    /// Something the stream asked of the host other than a reply, which the reader already sent.
    Event(Event),
    /// The child exited.
    Exit,
}

/// `--trace`: every read from the pty (`<`) and write to it (`>`), one line each, escaped, after
/// the milliseconds since the trace began.
#[derive(Clone)]
pub struct Trace(Arc<Mutex<(Instant, File)>>);

impl Trace {
    pub fn create(path: &Path) -> io::Result<Trace> {
        Ok(Trace(Arc::new(Mutex::new((Instant::now(), File::create(path)?)))))
    }

    fn log(&self, direction: char, bytes: &[u8]) {
        let mut trace = self.0.lock().expect("a pty thread panicked");
        let (started, file) = &mut *trace;
        let escaped: String =
            bytes.iter().flat_map(|&byte| ascii::escape_default(byte)).map(char::from).collect();
        let millis = started.elapsed().as_secs_f64() * 1000.;
        let _ = writeln!(file, "{millis:10.3} {direction} {escaped}");
    }
}

pub struct Pane {
    pub term: Arc<Mutex<Terminal>>,
    master: Box<dyn MasterPty + Send>,
    input: mpsc::Sender<Vec<u8>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    output_pending: Arc<AtomicBool>,
    pub reader: Option<JoinHandle<()>>,
    pub waiter: Option<JoinHandle<Option<u32>>>,
    /// The absolute row at the top of the view while it is scrolled back; `None` follows the
    /// output.
    scrolled: Option<u64>,
    pub selection: Option<Selection>,
}

impl Pane {
    /// Runs `command` on a new `rows`×`cols` pty. With a `scheme`, color queries are answered
    /// from the program's first byte on (see `set_scheme`). The receiver yields the pane's
    /// notices.
    pub fn spawn(
        mut command: CommandBuilder,
        rows: u16,
        cols: u16,
        scheme: Option<&Scheme>,
        trace: Option<Trace>,
    ) -> io::Result<(Pane, UnboundedReceiver<Notice>)> {
        let size = PtySize { rows, cols, pixel_width: 0, pixel_height: 0 };
        let pty = native_pty_system().openpty(size).map_err(io::Error::other)?;
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        // The program runs in rtty, not in the terminal or multiplexer rtty was started from;
        // programs pick features by these (nvim does not probe for undercurl inside tmux).
        command.env("TERM_PROGRAM", "rtty");
        command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        for outer in ["TMUX", "TMUX_PANE", "STY", "ZELLIJ", "TUTTI_PANE", "TUTTI_SESSION"] {
            command.env_remove(outer);
        }
        let mut child = pty.slave.spawn_command(command).map_err(io::Error::other)?;
        // The child holds the only other end, so reads end when it and its children are gone.
        drop(pty.slave);
        let killer = child.clone_killer();
        let output = pty.master.try_clone_reader().map_err(io::Error::other)?;
        let mut writer = pty.master.take_writer().map_err(io::Error::other)?;

        let mut term = Terminal::new(rows, cols, SCROLLBACK);
        // Alternate scroll on, as most terminals start: the wheel moves a full-screen program
        // that takes no mouse reports (a pager) by its arrow keys.
        term.advance(b"\x1b[?1007h");
        if let Some(scheme) = scheme {
            host_scheme(&mut term, scheme);
        }
        let term = Arc::new(Mutex::new(term));
        let (notices, receiver) = unbounded();
        let (input, inputs) = mpsc::channel::<Vec<u8>>();
        let output_pending = Arc::new(AtomicBool::new(false));

        let writer_trace = trace.clone();
        thread::Builder::new().name("pty writer".into()).spawn(move || {
            for bytes in inputs {
                if let Some(trace) = &writer_trace {
                    trace.log('>', &bytes);
                }
                if writer.write_all(&bytes).is_err() {
                    break;
                }
            }
        })?;
        let reader = thread::Builder::new().name("pty reader".into()).spawn({
            let (term, replies) = (term.clone(), input.clone());
            let (notices, pending) = (notices.clone(), output_pending.clone());
            move || read(output, &term, &replies, &notices, &pending, trace)
        })?;
        let waiter = thread::Builder::new().name("child waiter".into()).spawn(move || {
            let code = child.wait().ok().map(|status| status.exit_code());
            let _ = notices.unbounded_send(Notice::Exit);
            code
        })?;
        let pane = Pane {
            term,
            master: pty.master,
            input,
            killer,
            output_pending,
            reader: Some(reader),
            waiter: Some(waiter),
            scrolled: None,
            selection: None,
        };
        Ok((pane, receiver))
    }

    pub fn lock(&self) -> MutexGuard<'_, Terminal> {
        self.term.lock().expect("a pty thread panicked")
    }

    /// The window is about to draw what the last `Notice::Output` announced; output after this
    /// announces itself again.
    pub fn drawing(&self) {
        self.output_pending.store(false, Ordering::Release);
    }

    /// Bytes for the program, behind any replies already queued. Lost once the child is gone.
    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.input.send(bytes);
    }

    /// What the user typed or pasted: it ends the selection and brings the view back to the
    /// screen.
    pub fn input(&mut self, bytes: Vec<u8>) {
        self.selection = None;
        self.scrolled = None;
        self.write(bytes);
    }

    /// Resizes the terminal (which reflows the primary screen) and the pty, which tells the
    /// program. `cell` is the cell size in pixels, for programs that ask.
    pub fn resize(&mut self, rows: u16, cols: u16, cell: (u16, u16)) {
        let mut term = self.lock();
        term.resize(rows, cols);
        term.set_cell_pixels(cell.0, cell.1);
        drop(term);
        // Reflow renumbers rows, so a scrolled view and a selection would land elsewhere.
        self.scrolled = None;
        self.selection = None;
        let (pixel_width, pixel_height) =
            (cols.saturating_mul(cell.0), rows.saturating_mul(cell.1));
        let _ = self.master.resize(PtySize { rows, cols, pixel_width, pixel_height });
    }

    /// The scheme the window draws with: color queries answer with it, and a program that asked
    /// for mode 2031 reports hears when it flips between dark and light.
    pub fn set_scheme(&self, scheme: &Scheme) {
        let mut term = self.lock();
        host_scheme(&mut term, scheme);
        let replies = term.take_events();
        drop(term);
        for event in replies {
            if let Event::Reply(bytes) = event {
                self.write(bytes);
            }
        }
    }

    /// The absolute row at the top of the view.
    pub fn top(&self, term: &Terminal) -> u64 {
        let bottom = term.screen_top();
        self.scrolled.map_or(bottom, |top| top.clamp(term.oldest_row(), bottom))
    }

    /// Moves the view `lines` rows up into the scrollback, or down toward the screen when
    /// negative.
    pub fn scroll(&mut self, lines: i32) {
        let term = self.term.lock().expect("a pty thread panicked");
        let top = self.top(&term).saturating_add_signed(-i64::from(lines));
        let top = top.clamp(term.oldest_row(), term.screen_top());
        self.scrolled = (top < term.screen_top()).then_some(top);
    }

    /// The selected text, as it is copied.
    pub fn selected_text(&self) -> Option<String> {
        let term = self.lock();
        let (start, end) = self.selection?.range(&term)?;
        Some(term.text(start, end))
    }
}

impl Drop for Pane {
    /// Hangs up on the child, as closing a terminal does.
    fn drop(&mut self) {
        let _ = self.killer.kill();
    }
}

fn host_scheme(term: &mut Terminal, scheme: &Scheme) {
    for slot in 0..SLOTS {
        term.set_host_color(slot, Some(scheme.color(slot)));
    }
    let leaning = if scheme.is_dark() { ColorScheme::Dark } else { ColorScheme::Light };
    term.set_color_scheme(Some(leaning));
}

fn read(
    mut output: Box<dyn Read + Send>,
    term: &Mutex<Terminal>,
    replies: &mpsc::Sender<Vec<u8>>,
    notices: &UnboundedSender<Notice>,
    pending: &AtomicBool,
    trace: Option<Trace>,
) {
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = match output.read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            // EIO once the child's side is closed.
            Err(_) => return,
        };
        if let Some(trace) = &trace {
            trace.log('<', &buffer[..read]);
        }
        let mut term = term.lock().expect("the window panicked");
        term.advance(&buffer[..read]);
        let events = term.take_events();
        drop(term);
        for event in events {
            match event {
                Event::Reply(bytes) => {
                    let _ = replies.send(bytes);
                }
                event => {
                    let _ = notices.unbounded_send(Notice::Event(event));
                }
            }
        }
        if !pending.swap(true, Ordering::AcqRel) {
            let _ = notices.unbounded_send(Notice::Output);
        }
    }
}

/// `/bin/sh -c command`.
pub fn shell_command(command: &str) -> CommandBuilder {
    let mut builder = CommandBuilder::new("/bin/sh");
    builder.args(["-c", command]);
    builder
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheme::{find, load};
    use std::time::{Duration, Instant};

    fn wait_for(pane: &Pane, text: &str) {
        let started = Instant::now();
        while !pane.lock().contents().contains(text) {
            assert!(started.elapsed() < Duration::from_secs(5), "never saw {text:?}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn finish(mut pane: Pane) -> (String, Option<u32>) {
        pane.reader.take().unwrap().join().unwrap();
        let code = pane.waiter.take().unwrap().join().unwrap();
        let screen = pane.lock().contents();
        (screen, code)
    }

    #[test]
    fn output_reaches_the_terminal_and_the_exit_code_the_waiter() {
        let (pane, _) =
            Pane::spawn(shell_command("printf 'a\\033[1mb'; exit 3"), 24, 80, None, None).unwrap();
        assert_eq!(finish(pane), ("ab".into(), Some(3)));
    }

    #[test]
    fn the_program_hears_it_runs_in_rtty_and_in_no_multiplexer() {
        let mut command = shell_command("printf '%s:%s' \"$TERM_PROGRAM\" \"${TMUX-none}\"");
        command.env("TMUX", "/tmp/outer,1,0");
        let (pane, _) = Pane::spawn(command, 24, 80, None, None).unwrap();
        assert_eq!(finish(pane).0, "rtty:none");
    }

    /// The engine answers a query and the reply reaches the program, which shows it with `od`.
    #[test]
    fn replies_go_back_to_the_program() {
        let script = "stty -icanon -echo min 0 time 20; printf '\\033[6n'; \
                      dd bs=1 count=6 2>/dev/null | od -c";
        let (pane, _) = Pane::spawn(shell_command(script), 24, 80, None, None).unwrap();
        let (screen, _) = finish(pane);
        assert!(screen.contains("033   [   1   ;   1   R"), "{screen}");
    }

    /// What the window does on an appearance flip, through a real pty: a program that turned on
    /// mode 2031 reads the report; it shows the bytes it got with `od -c`.
    #[test]
    fn a_scheme_change_reaches_a_program_that_asked() {
        let script = "stty -icanon -echo min 0 time 20; printf '\\033[?2031hready\\n'; \
                      dd bs=1 count=9 2>/dev/null | od -c";
        let schemes = load(&[]).unwrap();
        let dark = find(&schemes, "OneDark Pro").unwrap();
        let (pane, _) = Pane::spawn(shell_command(script), 24, 80, Some(&dark), None).unwrap();
        wait_for(&pane, "ready");
        pane.set_scheme(&find(&schemes, "Solarized Light").unwrap());
        let (screen, _) = finish(pane);
        assert!(screen.contains("033   [   ?   9   9   7   ;   2   n"), "{screen}");
    }

    /// A program that asks for the background in its first write (nvim does) is answered.
    #[test]
    fn color_queries_are_answered_from_the_first_byte() {
        let script = "stty -icanon -echo min 0 time 20; printf '\\033]11;?\\007'; \
                      dd bs=1 count=24 2>/dev/null | tr '\\033\\007' EB";
        let dark = find(&load(&[]).unwrap(), "OneDark Pro").unwrap();
        let (pane, _) = Pane::spawn(shell_command(script), 24, 80, Some(&dark), None).unwrap();
        let (screen, _) = finish(pane);
        assert!(screen.contains("E]11;rgb:2828/2c2c/3434B"), "{screen}");
    }

    #[test]
    fn the_view_scrolls_within_the_scrollback_and_stays_put_under_output() {
        let (mut pane, _) = Pane::spawn(
            shell_command("seq 1 40; printf done; sleep 1; seq 41 45"),
            10,
            20,
            None,
            None,
        )
        .unwrap();
        wait_for(&pane, "done");
        let bottom = pane.lock().screen_top();
        pane.scroll(5);
        assert_eq!(pane.top(&pane.lock()), bottom - 5);
        pane.scroll(1000);
        assert_eq!(pane.top(&pane.lock()), 0);
        wait_for(&pane, "45");
        assert_eq!(pane.top(&pane.lock()), 0);
        pane.scroll(-1000);
        let term = pane.lock();
        assert_eq!(pane.top(&term), term.screen_top());
    }
}
