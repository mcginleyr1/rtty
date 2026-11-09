//! One terminal: a child process on its own pty, the rtty-vt `Terminal` its output feeds, the
//! threads that move the bytes, and what a window keeps per terminal (where the view sits in the
//! scrollback, the selection). A window lays panes out and draws them; a pane knows no pixels.
//!
//! Three threads per pane. The reader feeds the pty's output to `Terminal::advance` under the
//! lock and hands the engine's replies to the writer, in stream order. The writer owns the pty's
//! input side, so a program that stops reading stalls neither the reader nor the window. The
//! waiter reports the child's exit.
//!
//! A pane can show a pane of tutti's daemon instead (`attach`, `tutti.rs`): the window's connection
//! to the session streams it into the pane's terminal and takes what is typed, and the daemon keeps
//! the program. Or it can only say why it shows nothing (`held`).

use std::ascii;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use std::{mem, ptr};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtyPair, PtySize, native_pty_system};
use rtty_vt::{ColorScheme, Event, Terminal};

use crate::scheme::{SLOTS, Scheme};
use crate::selection::Selection;
use crate::title::Foreground;
use crate::{prompt, shell, tutti};

pub const SCROLLBACK: usize = 10_000;

/// What a pane's threads tell the window, in the order it happened.
#[derive(Debug)]
pub enum Notice {
    /// The terminal changed: draw. Not sent again until the window took it (`Pane::drawing`).
    Output,
    /// Something the stream asked of the host other than a reply, which the reader already sent.
    Event(Event),
    /// The child exited, as its status says ("Exited with code 1", "Terminated by Hangup").
    Exit(String),
}

/// The terminal's size until the window has laid out its first frame.
pub const START_SIZE: (u16, u16) = (24, 80);

/// `--trace`: every read from the pty (`<`) and write to it (`>`), one line each, escaped, after
/// the milliseconds since the trace began and the pane's number.
#[derive(Clone)]
pub struct Trace {
    file: Arc<Mutex<(Instant, File)>>,
    pane: u64,
}

impl Trace {
    pub fn create(path: &Path) -> io::Result<Trace> {
        Ok(Trace { file: Arc::new(Mutex::new((Instant::now(), File::create(path)?))), pane: 0 })
    }

    /// The same trace, its lines marked as `pane`'s.
    pub fn for_pane(&self, pane: u64) -> Trace {
        Trace { pane, ..self.clone() }
    }

    pub fn log(&self, direction: char, bytes: &[u8]) {
        let mut trace = self.file.lock().expect("a pty thread panicked");
        let (started, file) = &mut *trace;
        let escaped: String =
            bytes.iter().flat_map(|&byte| ascii::escape_default(byte)).map(char::from).collect();
        let millis = started.elapsed().as_secs_f64() * 1000.;
        let _ = writeln!(file, "{millis:10.3} {} {direction} {escaped}", self.pane);
    }
}

pub struct Pane {
    pub term: Arc<Mutex<Terminal>>,
    source: Source,
    output_pending: Arc<AtomicBool>,
    pub reader: Option<JoinHandle<()>>,
    pub waiter: Option<JoinHandle<Option<u32>>>,
    /// The absolute row at the top of the view while it is scrolled back; `None` follows the
    /// output.
    scrolled: Option<u64>,
    pub selection: Option<Selection>,
}

/// Where a pane's bytes come from and go.
enum Source {
    /// A child on a pty of rtty's own: the pty, the writer's input, the child's killer, and the
    /// login shell's pid, when the pane runs one rather than a program of its own (`-e`).
    Pty {
        master: Box<dyn MasterPty + Send>,
        input: mpsc::Sender<Vec<u8>>,
        killer: Box<dyn ChildKiller + Send + Sync>,
        shell: Option<libc::pid_t>,
    },
    /// A pane of tutti's daemon: the pane's id there, the window's connection to its session,
    /// what the session's last snapshot said of the pane, and the trace of what is typed.
    Tutti {
        pane: u64,
        session: Arc<tutti::Session>,
        watched: Arc<Mutex<tutti::Watched>>,
        trace: Option<Trace>,
    },
    /// Nothing: the terminal says why.
    Held,
}

impl Pane {
    /// Runs `command` on a new `rows`×`cols` pty. With a `scheme`, color queries are answered
    /// from the program's first byte on (see `set_scheme`). A login shell loads the shell
    /// integration `integration` holds (`shell::install`). With `graphics`, the program can show
    /// images (kitty's graphics protocol) from its first byte on. The receiver yields the pane's
    /// notices.
    pub fn spawn(
        mut command: CommandBuilder,
        rows: u16,
        cols: u16,
        scheme: Option<&Scheme>,
        trace: Option<Trace>,
        integration: Option<&Path>,
        graphics: bool,
    ) -> io::Result<(Pane, UnboundedReceiver<Notice>)> {
        let pty = open_pty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        // The program runs in rtty, not in the terminal or multiplexer rtty was started from;
        // programs pick features by these (nvim does not probe for undercurl inside tmux).
        command.env("TERM_PROGRAM", "rtty");
        command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        for outer in ["TMUX", "TMUX_PANE", "STY", "ZELLIJ", "TUTTI_PANE", "TUTTI_SESSION"] {
            command.env_remove(outer);
        }
        let login_shell = command.is_default_prog();
        if let Some(dir) = integration.filter(|_| login_shell) {
            shell::inject(&mut command, dir);
        }
        // A child forked while another thread makes the process's first use of libnotify dies
        // before it runs its program: the fork handler libSystem runs in it waits for libnotify's
        // globals, which a thread the child does not have was making ("os_once_t is corrupt").
        // So that use is made here, before the fork.
        // SAFETY: only looks a token up.
        unsafe { notify_is_valid_token(0) };
        let mut child = pty.slave.spawn_command(command).map_err(io::Error::other)?;
        let shell = child.process_id().filter(|_| login_shell).and_then(|pid| pid.try_into().ok());
        let killer = child.clone_killer();
        let output = pty.master.try_clone_reader().map_err(io::Error::other)?;
        let mut writer = pty.master.take_writer().map_err(io::Error::other)?;

        let term = Arc::new(Mutex::new(terminal(rows, cols, scheme, graphics)));
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
        // The child holds the only other end, so reads end when it and its children are gone.
        // Closed only now that the reader runs: the last close of a pty's slave, which this is
        // when the program is already gone, waits some 0.6 s for the master to be read and then
        // discards what the program printed.
        drop(pty.slave);
        let waiter = thread::Builder::new().name("child waiter".into()).spawn(move || {
            let status = child.wait();
            let told = status.as_ref().map_or_else(ToString::to_string, ToString::to_string);
            let _ = notices.unbounded_send(Notice::Exit(told));
            status.ok().map(|status| status.exit_code())
        })?;
        let pane = Pane {
            term,
            source: Source::Pty { master: pty.master, input, killer, shell },
            output_pending,
            reader: Some(reader),
            waiter: Some(waiter),
            scrolled: None,
            selection: None,
        };
        Ok((pane, receiver))
    }

    /// Shows pane `pane` of `session` on the window's connection to the session's daemon, which
    /// streams it (`tutti::Session::stream`): the daemon keeps the program, and its scrollback,
    /// screen and output come over the connection, what is typed goes back, and dropping the pane
    /// ends nothing. `rows`×`cols` is this pane's vote in the pane's size, which the daemon settles
    /// among all that show it: the terminal takes the size each snapshot comes at. The session
    /// names the pane and tells its agent's state. Nothing here waits on the daemon. The rest as
    /// `spawn`; there are no threads of the pane's own, and the session tells of the program's
    /// exit. Refused while another pane of the window shows it.
    pub fn attach(
        session: Arc<tutti::Session>,
        pane: u64,
        rows: u16,
        cols: u16,
        scheme: Option<&Scheme>,
        trace: Option<Trace>,
        graphics: bool,
    ) -> io::Result<(Pane, UnboundedReceiver<Notice>)> {
        if let Some(scheme) = scheme {
            session.ask(&tutti::Request::HostColors(tutti::colors(scheme)));
        }
        let term = Arc::new(Mutex::new(terminal(rows, cols, scheme, graphics)));
        let (notices, receiver) = unbounded();
        let (output_pending, watched) = (Arc::new(AtomicBool::new(false)), Arc::default());
        let shows = tutti::Shows {
            term: term.clone(),
            watched: Arc::clone(&watched),
            notices,
            pending: output_pending.clone(),
            trace: trace.clone(),
        };
        session.stream(pane, rows, cols, shows)?;
        let pane = Pane {
            term,
            source: Source::Tutti { pane, session, watched, trace },
            output_pending,
            reader: None,
            waiter: None,
            scrolled: None,
            selection: None,
        };
        Ok((pane, receiver))
    }

    /// A pane that shows nothing but `why`, on a `rows`×`cols` terminal, and is titled so: a
    /// tutti pane that cannot be shown. It stays until it is closed.
    pub fn held(
        why: &str,
        rows: u16,
        cols: u16,
        scheme: Option<&Scheme>,
    ) -> (Pane, UnboundedReceiver<Notice>) {
        let mut term = terminal(rows, cols, scheme, false);
        term.advance(format!("\x1b]2;{why}\x07[{why}]\r\n").as_bytes());
        let pane = Pane {
            term: Arc::new(Mutex::new(term)),
            source: Source::Held,
            output_pending: Arc::default(),
            reader: None,
            waiter: None,
            scrolled: None,
            selection: None,
        };
        (pane, unbounded().1)
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
        match &self.source {
            Source::Pty { input, .. } => {
                let _ = input.send(bytes);
            }
            Source::Tutti { pane, session, trace, .. } => {
                if let Some(trace) = trace {
                    trace.log('>', &bytes);
                }
                session.send(tutti::Frame::Input { pane: *pane, bytes });
            }
            Source::Held => {}
        }
    }

    /// Asks a tutti pane's daemon what `request` makes of the pane's id, behind what was typed so
    /// far; nothing for a pane of rtty's own.
    fn ask(&self, request: impl FnOnce(u64) -> tutti::Request) {
        if let Source::Tutti { pane, session, .. } = &self.source {
            session.ask(&request(*pane));
        }
    }

    /// What the user typed or pasted: it ends the selection and brings the view back to the
    /// screen.
    pub fn input(&mut self, bytes: Vec<u8>) {
        self.selection = None;
        self.scrolled = None;
        self.write(bytes);
    }

    /// Resizes the terminal (which reflows the primary screen) and the pty, which tells the
    /// program. `cell` is the cell size in pixels, for programs that ask. A tutti pane's size is
    /// its daemon's to settle: this moves the pane's vote, and the snapshot at the size settled
    /// resizes the terminal.
    pub fn resize(&mut self, rows: u16, cols: u16, cell: (u16, u16)) {
        let mut term = self.lock();
        if !matches!(self.source, Source::Tutti { .. }) {
            term.resize(rows, cols);
        }
        term.set_cell_pixels(cell.0, cell.1);
        drop(term);
        // Reflow renumbers rows, so a scrolled view and a selection would land elsewhere.
        self.scrolled = None;
        self.selection = None;
        let (pixel_width, pixel_height) =
            (cols.saturating_mul(cell.0), rows.saturating_mul(cell.1));
        match &self.source {
            Source::Pty { master, .. } => {
                let _ = master.resize(PtySize { rows, cols, pixel_width, pixel_height });
            }
            Source::Tutti { pane, session, .. } => session.vote(*pane, rows, cols),
            Source::Held => {}
        }
    }

    /// The scheme the window draws with: color queries answer with it, and a program that asked
    /// for mode 2031 reports hears when it flips between dark and light. A tutti pane's daemon
    /// answers and tells its programs, so it hears of the scheme instead.
    pub fn set_scheme(&self, scheme: &Scheme) {
        let mut term = self.lock();
        host_scheme(&mut term, scheme);
        let replies = term.take_events();
        drop(term);
        if matches!(self.source, Source::Tutti { .. }) {
            return self.ask(|_| tutti::Request::HostColors(tutti::colors(scheme)));
        }
        for event in replies {
            if let Event::Reply(bytes) = event {
                self.write(bytes);
            }
        }
    }

    /// Ends the program: hangs up on the child, or asks a tutti pane's daemon to kill the pane (a
    /// pane tutti keeps once its program exited goes too). The exit follows, as any exit does.
    pub fn kill(&mut self) {
        if let Source::Pty { killer, .. } = &mut self.source {
            let _ = killer.kill();
        }
        self.ask(|pane| tutti::Request::PaneKill { pane });
    }

    /// The window shows the pane focused, whether or not it is the active window, and someone
    /// looks at it (`seen`) when it is: a tutti pane's session hears so, as a view's own focus,
    /// which moves nothing of the session's (`tutti::Request::Attach`) but where tutti hands moves
    /// at its edges back, and which marks the pane seen when someone looks.
    pub fn focused(&self, seen: bool) {
        self.ask(|pane| tutti::Request::PaneFocus { pane, seen });
    }

    /// What tutti calls a tutti pane, and the directory its daemon last saw the pane's program in.
    pub fn tutti_name(&self) -> Option<(String, Option<PathBuf>)> {
        let Source::Tutti { watched, .. } = &self.source else { return None };
        let watched = watched.lock().expect("a reader panicked");
        Some((watched.title.clone(), watched.cwd.clone()))
    }

    /// A tutti pane's session, by its daemon's socket, and the pane's id there.
    pub fn tutti(&self) -> Option<(&Path, u64)> {
        let Source::Tutti { session, pane, .. } = &self.source else { return None };
        Some((&session.socket, *pane))
    }

    /// The directory the pane's program is in, as the system tells (`foreground`) or a tutti
    /// pane's daemon does.
    pub fn cwd(&self) -> Option<PathBuf> {
        self.foreground().and_then(|process| process.cwd).or_else(|| self.tutti_name()?.1)
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

    /// Cmd+K: the scrollback goes and, on the primary screen, the screen too (`Terminal::clear`);
    /// the view follows the output again. Whether the alternate screen is up.
    pub fn clear(&mut self) -> bool {
        let mut term = self.lock();
        term.clear();
        let alternate = term.modes().alt_screen;
        drop(term);
        (self.scrolled, self.selection) = (None, None);
        alternate
    }

    /// Cmd+Up and Cmd+Down: puts the shell's previous (`up`) or next prompt (OSC 133;A) above or
    /// below the view's top at the top of the view; a prompt on the screen goes back to following
    /// the output. `false` when there is none that way.
    pub fn jump_to_prompt(&mut self, up: bool) -> bool {
        let term = self.lock();
        let (top, screen_top) = (self.top(&term), term.screen_top());
        let prompts = prompt::prompts(&term);
        drop(term);
        let target = if up {
            prompts.iter().rev().find(|&&row| row < top)
        } else {
            // Following the output, the view is as far down as it goes.
            prompts.iter().find(|&&row| row > top && top < screen_top)
        };
        let Some(&row) = target else { return false };
        self.scrolled = (row < screen_top).then_some(row);
        true
    }

    /// Scrolls the view, `rows` tall, so absolute row `row` shows: left alone when it does, else
    /// with the row a third of the way down.
    pub fn reveal(&mut self, row: u64, rows: u16) {
        let term = self.lock();
        let (top, screen_top) = (self.top(&term), term.screen_top());
        if (top..top + u64::from(rows)).contains(&row) {
            return;
        }
        let top = row.saturating_sub(u64::from(rows / 3)).clamp(term.oldest_row(), screen_top);
        drop(term);
        self.scrolled = (top < screen_top).then_some(top);
    }

    /// The process in the pty's foreground: its process group's leader (`tcgetpgrp`), the shell
    /// at a prompt, else the program the shell runs. Its arguments and working directory are
    /// asked of the kernel for that one process, never the whole process table. None for a tutti
    /// pane, whose program is its daemon's.
    pub fn foreground(&self) -> Option<Foreground> {
        let Source::Pty { master, shell, .. } = &self.source else { return None };
        let pid = master.process_group_leader()?;
        Some(Foreground {
            shell: *shell == Some(pid),
            argv: process_args(pid).unwrap_or_default(),
            cwd: process_cwd(pid),
        })
    }

    /// The program reads a password, as `sudo`, `ssh` and `read -s` do: the pty's echo off and
    /// its canonical mode on. Never known of a tutti pane.
    pub fn reads_password(&self) -> bool {
        let Source::Pty { master, .. } = &self.source else { return false };
        let Some(fd) = master.as_raw_fd() else { return false };
        // SAFETY: tcgetattr writes only the struct it is given.
        let mut termios: libc::termios = unsafe { mem::zeroed() };
        let read = unsafe { libc::tcgetattr(fd, &mut termios) } == 0;
        read && termios.c_lflag & libc::ICANON != 0 && termios.c_lflag & libc::ECHO == 0
    }

    /// The selected text, as it is copied.
    pub fn selected_text(&self) -> Option<String> {
        self.selection?.text(&self.lock())
    }
}

impl Drop for Pane {
    /// Hangs up on the child, as closing a terminal does. A tutti pane is shown no more, and the
    /// pane and its program run on in the daemon.
    fn drop(&mut self) {
        match &mut self.source {
            Source::Pty { killer, .. } => {
                let _ = killer.kill();
            }
            Source::Tutti { pane, session, .. } => session.forget(*pane),
            Source::Held => {}
        }
    }
}

/// A terminal as a pane starts: `rows`×`cols`, the scrollback a pane keeps, and rtty's setup.
fn terminal(rows: u16, cols: u16, scheme: Option<&Scheme>, graphics: bool) -> Terminal {
    let mut term = Terminal::new(rows, cols, SCROLLBACK);
    // Alternate scroll on, as most terminals start: the wheel moves a full-screen program that
    // takes no mouse reports (a pager) by its arrow keys.
    term.advance(b"\x1b[?1007h");
    // What a killed program left on (mouse and focus reports, 2031, key encodings) goes off when
    // the shell marks the command's end, before the shell hears of a click or a flip to dark as
    // typing.
    term.set_command_end_reset(true);
    term.set_graphics(graphics);
    if let Some(scheme) = scheme {
        host_scheme(&mut term, scheme);
    }
    term
}

unsafe extern "C" {
    /// `notify.h`: whether `token` is one libnotify gave out.
    fn notify_is_valid_token(token: libc::c_int) -> bool;
}

/// A new pty, opened by one thread at a time: macOS's openpty(3) now and then fails (errno -6)
/// when two threads of a process call it at once, as the tests' threads do.
fn open_pty(size: PtySize) -> io::Result<PtyPair> {
    static OPENING: Mutex<()> = Mutex::new(());
    let _alone = OPENING.lock().unwrap_or_else(PoisonError::into_inner);
    native_pty_system().openpty(size).map_err(io::Error::other)
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

/// A process's working directory: the path of its current directory's vnode
/// (`PROC_PIDVNODEPATHINFO`).
fn process_cwd(pid: libc::pid_t) -> Option<PathBuf> {
    // SAFETY: proc_vnodepathinfo is plain data, for which zeroes are a value.
    let mut info: libc::proc_vnodepathinfo = unsafe { mem::zeroed() };
    let size = mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: the buffer is a proc_vnodepathinfo of the size passed.
    let filled = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size)
    };
    if filled != size {
        return None;
    }
    let path = info.pvi_cdir.vip_path.as_flattened().iter().map(|&c| c as u8);
    let path: Vec<u8> = path.take_while(|&byte| byte != 0).collect();
    (!path.is_empty()).then(|| PathBuf::from(OsString::from_vec(path)))
}

/// A process's arguments, from the kernel's copy of its argument area (`KERN_PROCARGS2`).
fn process_args(pid: libc::pid_t) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let query = |buffer: *mut u8, size: &mut libc::size_t, mib: &mut [libc::c_int; 3]| {
        // SAFETY: `buffer` is null (a size query) or holds `size` bytes, as many as sysctl may
        // write; it says how many it did.
        unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buffer.cast(), size, ptr::null_mut(), 0) }
    };
    let mut size = 0;
    if query(ptr::null_mut(), &mut size, &mut mib) != 0 {
        return None;
    }
    let mut area = vec![0; size];
    if query(area.as_mut_ptr(), &mut size, &mut mib) != 0 {
        return None;
    }
    area.truncate(size);
    procargs(&area)
}

/// The arguments in a `KERN_PROCARGS2` area: argc, the executable's path, NULs up to the
/// arguments, then argc NUL-terminated arguments (and the environment after them).
fn procargs(area: &[u8]) -> Option<Vec<String>> {
    let (argc, rest) = area.split_first_chunk::<4>()?;
    let argc = usize::try_from(i32::from_ne_bytes(*argc)).ok()?;
    let after_path = rest.iter().position(|&byte| byte == 0)?;
    let rest = &rest[after_path..];
    let args = &rest[rest.iter().position(|&byte| byte != 0)?..];
    let args = args.split(|&byte| byte == 0).take(argc);
    Some(args.map(|arg| String::from_utf8_lossy(arg).into_owned()).collect())
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
    use rtty_vt::{Mark, MarkKind};
    use std::time::{Duration, Instant};

    fn wait_for(pane: &Pane, text: &str) {
        let started = Instant::now();
        while !pane.lock().contents().contains(text) {
            let screen = pane.lock().contents();
            assert!(started.elapsed() < Duration::from_secs(5), "never saw {text:?} in {screen:?}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// The pane's foreground process once `meant` says it is the one meant, within 5 s: a child
    /// takes the foreground (or prints) before it execs its program, and until then its
    /// arguments are its parent's.
    fn foreground_when(pane: &Pane, meant: impl Fn(&Foreground) -> bool) -> Foreground {
        let started = Instant::now();
        loop {
            let seen = pane.foreground();
            match seen {
                Some(foreground) if meant(&foreground) => return foreground,
                _ => assert!(started.elapsed() < Duration::from_secs(5), "last saw {seen:?}"),
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn finish(mut pane: Pane) -> (String, Option<u32>) {
        pane.reader.take().unwrap().join().unwrap();
        let code = pane.waiter.take().unwrap().join().unwrap();
        let screen = pane.lock().contents();
        (screen, code)
    }

    /// Sixteen threads open ptys together, twenty times over; unserialized, macOS fails a few of
    /// the 320.
    #[test]
    fn ptys_open_from_many_threads_at_once() {
        let size = PtySize { rows: 2, cols: 2, pixel_width: 0, pixel_height: 0 };
        for _ in 0..20 {
            let barrier = Arc::new(std::sync::Barrier::new(16));
            let opens: Vec<_> = (0..16)
                .map(|_| {
                    let barrier = barrier.clone();
                    thread::spawn(move || {
                        barrier.wait();
                        open_pty(size).map(drop)
                    })
                })
                .collect();
            for open in opens {
                open.join().unwrap().unwrap();
            }
        }
    }

    #[test]
    fn output_reaches_the_terminal_and_the_exit_code_the_waiter() {
        let (pane, _) = Pane::spawn(
            shell_command("printf 'a\\033[1mb'; exit 3"),
            24,
            80,
            None,
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(finish(pane), ("ab".into(), Some(3)));
    }

    /// A child forked while another thread made the process's first use of libnotify, as the
    /// notify tests' first call into Foundation does, died before it ran its program, of
    /// libSystem's "os_once_t is corrupt": in one fresh process in six of these. That use comes
    /// once in a process, so forty fresh ones each spawn panes on four threads while a fifth
    /// makes it.
    #[test]
    fn programs_start_while_another_thread_first_uses_libnotify() {
        const FRESH: &str = "RTTY_TEST_FRESH_PROCESS";
        if std::env::var_os(FRESH).is_none() {
            let test = "pane::tests::programs_start_while_another_thread_first_uses_libnotify";
            for _ in 0..40 {
                let run = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", test])
                    .env(FRESH, "1")
                    .output()
                    .unwrap();
                let told = String::from_utf8_lossy(&run.stdout);
                assert!(told.contains("1 passed"), "{told}");
            }
            return;
        }
        let first_use = thread::spawn(|| {
            thread::sleep(Duration::from_millis(1));
            crate::notify::bundled()
        });
        let spawners: Vec<_> = (0..4)
            .map(|_| {
                thread::spawn(|| {
                    for _ in 0..3 {
                        let command = shell_command("printf done");
                        let (pane, _) =
                            Pane::spawn(command, 24, 80, None, None, None, false).unwrap();
                        assert_eq!(finish(pane), ("done".into(), Some(0)));
                    }
                })
            })
            .collect();
        for spawner in spawners {
            spawner.join().unwrap();
        }
        first_use.join().unwrap();
    }

    #[test]
    fn the_program_hears_it_runs_in_rtty_and_in_no_multiplexer() {
        let mut command = shell_command("printf '%s:%s' \"$TERM_PROGRAM\" \"${TMUX-none}\"");
        command.env("TMUX", "/tmp/outer,1,0");
        let (pane, _) = Pane::spawn(command, 24, 80, None, None, None, false).unwrap();
        assert_eq!(finish(pane).0, "rtty:none");
    }

    /// Echo off with canonical mode on is a password prompt; echo off alone (a full-screen
    /// program, a shell's line editor) and a plain read are not.
    #[test]
    fn a_password_prompt_is_echo_off_in_canonical_mode() {
        let reads = |modes: &str| {
            let script = format!("stty {modes}; printf ready; sleep 5");
            let (pane, _) =
                Pane::spawn(shell_command(&script), 24, 80, None, None, None, false).unwrap();
            wait_for(&pane, "ready");
            pane.reads_password()
        };
        assert!(reads("-echo icanon"));
        assert!(!reads("-echo -icanon"));
        assert!(!reads("echo icanon"));
    }

    /// The engine answers a query and the reply reaches the program, which shows it with `od`.
    #[test]
    fn replies_go_back_to_the_program() {
        let script = "stty -icanon -echo min 0 time 20; printf '\\033[6n'; \
                      dd bs=1 count=6 2>/dev/null | od -c";
        let (pane, _) =
            Pane::spawn(shell_command(script), 24, 80, None, None, None, false).unwrap();
        let (screen, _) = finish(pane);
        assert!(screen.contains("033   [   1   ;   1   R"), "{screen}");
    }

    /// With graphics, an image in the program's first write is held; without, it is dropped.
    #[test]
    fn graphics_are_on_from_the_first_byte_or_not_at_all() {
        let image = "printf '\\033_Ga=t,i=1,q=2,f=24,s=1,v=1;AAAA\\033\\\\done'";
        for graphics in [true, false] {
            let (pane, _) =
                Pane::spawn(shell_command(image), 24, 80, None, None, None, graphics).unwrap();
            wait_for(&pane, "done");
            assert_eq!(pane.lock().images().len(), usize::from(graphics), "{graphics}");
            assert_eq!(finish(pane).0, "done");
        }
    }

    /// What the window does on an appearance flip, through a real pty: a program that turned on
    /// mode 2031 reads the report; it shows the bytes it got with `od -c`.
    #[test]
    fn a_scheme_change_reaches_a_program_that_asked() {
        let script = "stty -icanon -echo min 0 time 20; printf '\\033[?2031hready\\n'; \
                      dd bs=1 count=9 2>/dev/null | od -c";
        let schemes = load(&[]).unwrap();
        let dark = find(&schemes, "OneDark Pro").unwrap();
        let (pane, _) =
            Pane::spawn(shell_command(script), 24, 80, Some(&dark), None, None, false).unwrap();
        wait_for(&pane, "ready");
        pane.set_scheme(&find(&schemes, "Solarized Light").unwrap());
        let (screen, _) = finish(pane);
        assert!(screen.contains("033   [   ?   9   9   7   ;   2   n"), "{screen}");
    }

    /// A program killed before it turned 2031 and mouse reports off: at the shell's mark of the
    /// command's end both go off, and a flip to light sends the shell nothing it would take for
    /// typing.
    #[test]
    fn a_commands_end_turns_off_what_the_command_left_on() {
        let script = "stty -icanon -echo min 0 time 5; \
                      printf '\\033]133;C\\007\\033[?2031h\\033[?1000h\\033]133;D;137\\007ready\\n'; \
                      dd bs=1 count=9 2>/dev/null | od -c";
        let schemes = load(&[]).unwrap();
        let dark = find(&schemes, "OneDark Pro").unwrap();
        let (pane, _) =
            Pane::spawn(shell_command(script), 24, 80, Some(&dark), None, None, false).unwrap();
        wait_for(&pane, "ready");
        pane.set_scheme(&find(&schemes, "Solarized Light").unwrap());
        let modes = pane.lock().modes().clone();
        assert!(!modes.color_scheme_reports && modes.mouse == rtty_vt::MouseMode::None);
        let (screen, _) = finish(pane);
        assert!(!screen.contains("033"), "{screen}");
    }

    /// A program that asks for the background in its first write (nvim does) is answered.
    #[test]
    fn color_queries_are_answered_from_the_first_byte() {
        let script = "stty -icanon -echo min 0 time 20; printf '\\033]11;?\\007'; \
                      dd bs=1 count=24 2>/dev/null | tr '\\033\\007' EB";
        let dark = find(&load(&[]).unwrap(), "OneDark Pro").unwrap();
        let (pane, _) =
            Pane::spawn(shell_command(script), 24, 80, Some(&dark), None, None, false).unwrap();
        let (screen, _) = finish(pane);
        assert!(screen.contains("E]11;rgb:2828/2c2c/3434B"), "{screen}");
    }

    /// Three prompts, the last on the screen of a 10-row pane: up goes from prompt to prompt, down
    /// comes back and, at the prompt on the screen, follows the output again.
    #[test]
    fn prompt_jumps_go_from_mark_to_mark() {
        let script = "for p in one two three; do printf '\\033]133;A\\007$ %s\\n' $p; seq 1 30; done; \
                      printf '\\033]133;A\\007$ last'; sleep 5";
        let (mut pane, _) =
            Pane::spawn(shell_command(script), 10, 20, None, None, None, false).unwrap();
        wait_for(&pane, "$ last");
        let (prompts, screen_top): (Vec<u64>, u64) = {
            let term = pane.lock();
            let marks = term.marks(0, term.screen_top() + 10);
            (marks.iter().map(|mark| mark.row).collect(), term.screen_top())
        };
        assert_eq!(prompts.len(), 4);
        assert!(prompts[3] >= screen_top && prompts[2] < screen_top);
        let top = |pane: &Pane| pane.top(&pane.lock());
        assert!(pane.jump_to_prompt(true));
        assert_eq!(top(&pane), prompts[2]);
        assert!(pane.jump_to_prompt(true) && pane.jump_to_prompt(true));
        assert_eq!(top(&pane), prompts[0]);
        assert!(!pane.jump_to_prompt(true), "no prompt above the first");
        assert_eq!(top(&pane), prompts[0]);
        assert!(pane.jump_to_prompt(false));
        assert_eq!(top(&pane), prompts[1]);
        assert!(pane.jump_to_prompt(false) && pane.jump_to_prompt(false));
        assert_eq!(top(&pane), screen_top, "the last prompt is on the screen: follow the output");
        assert!(!pane.jump_to_prompt(false));
    }

    /// No OSC 7: `sh -c` (not a login shell) `cd`s and execs a program, which then holds the
    /// foreground and tells its arguments and working directory.
    #[test]
    fn the_foreground_process_tells_its_command_and_working_directory() {
        let dir = std::env::temp_dir().join(format!("rtty-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = format!("cd '{}' && exec sleep 5", dir.display());
        let (pane, _) =
            Pane::spawn(shell_command(&script), 24, 80, None, None, None, false).unwrap();
        let foreground = foreground_when(&pane, |process| process.argv == ["sleep", "5"]);
        assert!(!foreground.shell);
        assert_eq!(foreground.cwd.unwrap().canonicalize().unwrap(), dir.canonicalize().unwrap());
        drop(pane);
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn an_argument_area_reads_as_its_arguments_without_the_environment() {
        let mut area = 3i32.to_ne_bytes().to_vec();
        area.extend_from_slice(
            b"/usr/local/bin/cargo\0\0\0\0cargo\0run\0--release\0HOME=/Users/me\0",
        );
        assert_eq!(procargs(&area).unwrap(), ["cargo", "run", "--release"]);
        assert_eq!(procargs(&area[..2]), None);
    }

    /// A login shell (dash, with a home of its own so no one's profile runs) holds the
    /// foreground at its prompt; a command it runs takes it, in a process group of its own.
    #[test]
    fn the_login_shell_holds_the_foreground_until_it_runs_a_command() {
        let home = std::env::temp_dir().join(format!("rtty-home-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let mut shell = CommandBuilder::new_default_prog();
        shell.env("SHELL", "/bin/dash");
        shell.env("HOME", &home);
        shell.cwd(&home);
        let (pane, _) = Pane::spawn(shell, 24, 80, None, None, None, false).unwrap();
        wait_for(&pane, "$");
        let at_prompt = foreground_when(&pane, |process| process.shell);
        assert_eq!(at_prompt.cwd.unwrap().canonicalize().unwrap(), home.canonicalize().unwrap());
        pane.write(b"sleep 5\n".to_vec());
        let running = foreground_when(&pane, |process| process.argv == ["sleep", "5"]);
        assert!(!running.shell, "{running:?}");
        drop(pane);
        std::fs::remove_dir_all(home).unwrap();
    }

    fn wait_for_output(pane: &Pane, output: &str) {
        let started = Instant::now();
        while prompt::last_output(&pane.lock()).as_deref() != Some(output) {
            let term = pane.lock();
            let (last, screen) = (prompt::last_output(&term), term.contents());
            let waited = started.elapsed() < Duration::from_secs(5);
            assert!(waited, "never output {output:?}, last {last:?} on {screen:?}");
            drop(term);
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Real zsh and bash (macOS's 3.2, and Homebrew's where it is installed) as login shells, each
    /// with a home of its own whose startup files know nothing of rtty: they load the integration,
    /// still count as the pane's shell, mark prompts, command lines, output and ends, and leave
    /// nothing of the integration in what their commands inherit.
    #[test]
    fn login_shells_load_the_integration_and_keep_it_to_themselves() {
        let id = std::process::id();
        let integration = shell::install(&std::env::temp_dir().join(format!("rtty-int-{id}")));
        let integration = integration.unwrap();
        let homes = std::env::temp_dir().join(format!("rtty-homes-{id}"));
        let brew = Path::new("/opt/homebrew/bin/bash").exists().then_some("/opt/homebrew/bin/bash");
        for path in ["/bin/zsh", "/bin/bash"].into_iter().chain(brew) {
            let home = homes.join(path.replace('/', "-"));
            std::fs::create_dir_all(home.join("zdotdir")).unwrap();
            std::fs::write(home.join("zdotdir/.zshrc"), "PS1='ready> '").unwrap();
            std::fs::write(home.join(".bash_profile"), "PS1='ready> '").unwrap();
            let mut shell = CommandBuilder::new_default_prog();
            shell.env("SHELL", path);
            shell.env("HOME", &home);
            shell.env("ZDOTDIR", home.join("zdotdir"));
            shell.env("BASH_SILENCE_DEPRECATION_WARNING", "1");
            shell.cwd(&home);
            let (pane, _) =
                Pane::spawn(shell, 24, 80, None, None, Some(&integration), false).unwrap();
            wait_for(&pane, "ready>");
            assert!(foreground_when(&pane, |process| process.shell).shell, "{path}");
            pane.write(b"echo one two\n".to_vec());
            wait_for_output(&pane, "one two");
            pane.write(b"env | grep -c rtty-int; echo $ZDOTDIR\n".to_vec());
            wait_for_output(&pane, &format!("0\n{}", home.join("zdotdir").display()));
            let marks = prompt::marks(&pane.lock());
            let kinds = [MarkKind::PromptStart, MarkKind::CommandStart, MarkKind::OutputStart];
            for kind in kinds {
                assert!(marks.iter().any(|mark| mark.kind == kind), "{path}: no {kind:?}");
            }
            let ends = |mark: &Mark| matches!(mark.kind, MarkKind::CommandEnd { exit: Some(0) });
            assert!(marks.iter().any(ends), "{path}: no end");
            drop(pane);
        }
        std::fs::remove_dir_all(integration.parent().unwrap()).unwrap();
        std::fs::remove_dir_all(homes).unwrap();
    }

    /// A user's PROMPT_COMMAND array whose last element rebuilds PS1: bash 5.1 runs every element,
    /// so rtty's marking must be one of them and last, and bash 3.2 runs only the first, which
    /// must still hold all of rtty's. Either way the prompt after a command is marked.
    #[test]
    fn bash_marks_a_prompt_that_an_array_prompt_command_rebuilds() {
        let id = std::process::id();
        let integration = shell::install(&std::env::temp_dir().join(format!("rtty-arr-{id}")));
        let integration = integration.unwrap();
        let homes = std::env::temp_dir().join(format!("rtty-arr-homes-{id}"));
        let brew = Path::new("/opt/homebrew/bin/bash").exists().then_some("/opt/homebrew/bin/bash");
        for path in ["/bin/bash"].into_iter().chain(brew) {
            let home = homes.join(path.replace('/', "-"));
            std::fs::create_dir_all(&home).unwrap();
            let profile = "PS1='ready> '\nPROMPT_COMMAND=(: 'PS1=\"ready> \"')\n";
            std::fs::write(home.join(".bash_profile"), profile).unwrap();
            let mut shell = CommandBuilder::new_default_prog();
            shell.env("SHELL", path);
            shell.env("HOME", &home);
            shell.env("BASH_SILENCE_DEPRECATION_WARNING", "1");
            shell.cwd(&home);
            let (pane, _) =
                Pane::spawn(shell, 24, 80, None, None, Some(&integration), false).unwrap();
            wait_for(&pane, "ready>");
            pane.write(b"echo one two\n".to_vec());
            wait_for_output(&pane, "one two");
            let started = Instant::now();
            let last =
                || prompt::marks(&pane.lock()).iter().rev().take(2).map(|m| m.kind).collect();
            let mut kinds: Vec<MarkKind> = last();
            while kinds != [MarkKind::CommandStart, MarkKind::PromptStart] {
                assert!(started.elapsed() < Duration::from_secs(5), "{path}: last marks {kinds:?}");
                thread::sleep(Duration::from_millis(10));
                kinds = last();
            }
            drop(pane);
        }
        std::fs::remove_dir_all(integration.parent().unwrap()).unwrap();
        std::fs::remove_dir_all(homes).unwrap();
    }

    /// A pane that cannot show its tutti pane says why, on its screen and in its title, and
    /// takes nothing: what is typed goes nowhere, a kill ends nothing, and a resize reflows it.
    #[test]
    fn a_held_pane_says_why_and_takes_nothing() {
        let (mut pane, mut notices) = Pane::held("no pane 9 in tutti session s", 4, 40, None);
        assert_eq!(pane.lock().contents(), "[no pane 9 in tutti session s]");
        assert_eq!(pane.lock().title(), "no pane 9 in tutti session s");
        pane.input(b"ls\r".to_vec());
        pane.kill();
        pane.resize(4, 20, (8, 16));
        assert_eq!(pane.lock().size(), (4, 20));
        assert_eq!((pane.tutti(), pane.foreground(), pane.reads_password()), (None, None, false));
        assert!(notices.try_recv().is_err(), "nothing to tell");
    }

    #[test]
    fn the_view_scrolls_within_the_scrollback_and_stays_put_under_output() {
        let (mut pane, _) = Pane::spawn(
            shell_command("seq 1 40; printf done; sleep 1; seq 41 45"),
            10,
            20,
            None,
            None,
            None,
            false,
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
