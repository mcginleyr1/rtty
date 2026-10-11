use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::thread::JoinHandle;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, State};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty::{self, Options, Pty, Shell};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

/// Forwards terminal events (wakeups, title changes, replies the terminal wants written back)
/// from the pty reader thread to whoever drains `Session::events`.
#[derive(Clone)]
pub struct Listener(UnboundedSender<Event>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.0.unbounded_send(event);
    }
}

/// One child process on a pty, its output parsed into `term` by alacritty's reader thread.
pub struct Session {
    pub term: Arc<FairMutex<Term<Listener>>>,
    pub reader: JoinHandle<(EventLoop<Pty, Listener>, State)>,
    pty: EventLoopSender,
}

impl Session {
    /// `shell: None` runs the user's login shell. The receiver yields the terminal's events.
    pub fn spawn(
        shell: Option<Shell>,
        size: WindowSize,
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
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let pty = event_loop.channel();
        Ok((Self { term, reader: event_loop.spawn(), pty }, events))
    }

    /// Ignores the error a write gets once the child has exited.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.pty.send(Msg::Input(bytes.into()));
    }

    pub fn resize(&self, size: WindowSize) {
        self.term.lock().resize(TermSize::new(size.num_cols.into(), size.num_lines.into()));
        let _ = self.pty.send(Msg::Resize(size));
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
