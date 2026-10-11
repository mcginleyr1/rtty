use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::sync::Arc;
use std::thread::JoinHandle;

use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, State};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty::{
    self, ChildEvent, EventedPty, EventedReadWrite, Options, Pty, Shell,
};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use polling::{Event as Interest, PollMode, Poller};

use crate::color_reports::{ColorReports, Watcher};

/// Forwards terminal events (wakeups, title changes, replies the terminal wants written back)
/// from the pty reader thread to whoever drains `Session::events`.
#[derive(Clone)]
pub struct Listener(pub UnboundedSender<Event>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.0.unbounded_send(event);
    }
}

/// One child process on a pty, its output parsed into `term` by alacritty's reader thread.
pub struct Session {
    pub term: Arc<FairMutex<Term<Listener>>>,
    pub reader: JoinHandle<(EventLoop<TappedPty, Listener>, State)>,
    pty: EventLoopSender,
    color_reports: Arc<ColorReports>,
}

impl Session {
    /// `shell: None` runs the user's login shell; `dark` is the scheme's for color queries. The
    /// receiver yields the terminal's events.
    pub fn spawn(
        shell: Option<Shell>,
        size: WindowSize,
        dark: bool,
    ) -> io::Result<(Self, UnboundedReceiver<Event>)> {
        let (sender, events) = unbounded();
        let listener = Listener(sender);
        let dimensions = TermSize::new(size.num_cols.into(), size.num_lines.into());
        let term =
            Arc::new(FairMutex::new(Term::new(Config::default(), &dimensions, listener.clone())));
        let env = HashMap::from([
            ("TERM".to_owned(), "xterm-256color".to_owned()),
            ("COLORTERM".to_owned(), "truecolor".to_owned()),
        ]);
        let options = Options { shell, env, drain_on_exit: true, ..Options::default() };
        let pty = tty::new(&options, size, 0)?;
        let color_reports = Arc::new(ColorReports::new(dark));
        let output = pty.file().try_clone()?;
        let watcher = Watcher::new(color_reports.clone(), listener.clone());
        let pty = TappedPty { pty, output: TappedOutput { output, watcher } };
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let pty = event_loop.channel();
        Ok((Self { term, reader: event_loop.spawn(), pty, color_reports }, events))
    }

    /// Ignores the error a write gets once the child has exited.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.pty.send(Msg::Input(bytes.into()));
    }

    /// Writes back a reply the terminal produced for a query.
    pub fn reply(&self, reply: String) {
        self.write(self.color_reports.correct_reply(reply).into_bytes());
    }

    /// Tells a program that asked for palette reports (mode 2031) that the scheme changed.
    pub fn scheme_changed(&self, dark: bool) {
        if let Some(report) = self.color_reports.scheme_changed(dark) {
            self.write(report.into_bytes());
        }
    }

    pub fn resize(&self, size: WindowSize) {
        self.term.lock().resize(TermSize::new(size.num_cols.into(), size.num_lines.into()));
        let _ = self.pty.send(Msg::Resize(size));
    }
}

/// The pty as alacritty's event loop drives it, with its output also shown to the color-report
/// watcher. Everything else is the pty's own.
pub struct TappedPty {
    pty: Pty,
    output: TappedOutput,
}

pub struct TappedOutput {
    /// A second descriptor for the pty master, so reads can go through the watcher.
    output: File,
    watcher: Watcher,
}

impl Read for TappedOutput {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.output.read(buf)?;
        self.watcher.watch(&buf[..read]);
        Ok(read)
    }
}

impl EventedReadWrite for TappedPty {
    type Reader = TappedOutput;
    type Writer = File;

    unsafe fn register(
        &mut self,
        poller: &Arc<Poller>,
        interest: Interest,
        mode: PollMode,
    ) -> io::Result<()> {
        // SAFETY: the caller's contract, passed on; the pty lives as long as this wrapper.
        unsafe { self.pty.register(poller, interest, mode) }
    }

    fn reregister(
        &mut self,
        poller: &Arc<Poller>,
        interest: Interest,
        mode: PollMode,
    ) -> io::Result<()> {
        self.pty.reregister(poller, interest, mode)
    }

    fn deregister(&mut self, poller: &Arc<Poller>) -> io::Result<()> {
        self.pty.deregister(poller)
    }

    fn reader(&mut self) -> &mut TappedOutput {
        &mut self.output
    }

    fn writer(&mut self) -> &mut File {
        self.pty.writer()
    }
}

impl EventedPty for TappedPty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.pty.next_child_event()
    }
}

impl OnResize for TappedPty {
    fn on_resize(&mut self, size: WindowSize) {
        self.pty.on_resize(size);
    }
}

pub fn command(command: &str) -> Shell {
    Shell::new("/bin/sh".to_owned(), vec!["-c".to_owned(), command.to_owned()])
}

/// The visible screen as text, one line per row.
pub fn screen<T>(term: &Term<T>) -> String {
    let bottom = Line(term.screen_lines() as i32 - 1);
    term.bounds_to_string(Point::new(Line(0), Column(0)), Point::new(bottom, term.last_column()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const SIZE: WindowSize =
        WindowSize { num_lines: 24, num_cols: 80, cell_width: 8, cell_height: 16 };

    /// What the window does on an appearance flip, through a real pty: a program that turned on
    /// mode 2031 reads the report; it shows the bytes it got with `od -c`.
    #[test]
    fn a_scheme_change_reaches_a_program_that_asked() {
        let script = "stty -icanon -echo min 0 time 20; printf '\\033[?2031hready\\n'; \
                      dd bs=1 count=9 2>/dev/null | od -c";
        let (session, _events) = Session::spawn(Some(command(script)), SIZE, true).unwrap();
        let started = Instant::now();
        while !screen(&session.term.lock()).contains("ready") {
            assert!(started.elapsed() < Duration::from_secs(5), "the script never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        session.scheme_changed(false);
        let _ = session.reader.join();
        let screen = screen(&session.term.lock());
        assert!(screen.contains("033   [   ?   9   9   7   ;   2   n"), "{screen}");
    }
}
