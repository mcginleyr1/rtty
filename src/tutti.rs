//! A tutti session's daemon as rtty shows it: the daemon keeps the ptys, the session and the
//! agents' states, and rtty draws the session's panes as rtty panes (`Pane::attach`). The daemon's
//! wire is tutti's own crate, `tutti-wire`: where it listens, its frames, requests, responses and
//! events, and whether its revision and rtty's work together. Here is the window's connection to
//! a session, whose pane streams' reader is ported from tutti's `pane attach --inline`.
//!
//! A window that shows a session's panes keeps one connection to its daemon (`Session`), and all
//! of it goes over that one: an attached view, whose focus is its own and which hands moves past
//! tutti's edges on to rtty, quiet (sent no pane frames but its streams'), and a watcher. Its
//! answer says the daemon speaks this wire and gives the session's tabs with their split trees,
//! which a window that is the session lays out as its own; its snapshots name each pane and tell
//! its agent's state; its notes tell when an agent is blocked or done. And it streams each pane the
//! window shows, each frame naming its pane: the scrollback rows the pane holds, a snapshot of its
//! screen, and every pty read raw, but for the terminal queries the daemon answered itself. The
//! pane's own `Terminal` takes it all, the keyboard protocols its program sets too, so scrollback,
//! search, selection and links work on it as on any pane, and its keys are encoded as the program
//! asked. Closing the connection detaches: the panes and their programs run on in the daemon.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::mem;
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use rtty_vt::{BACKGROUND, CURSOR, Event, FOREGROUND, Rgb, Terminal};
use serde::Deserialize;
use tutti_wire::{
    AgentState, Event as Tells, HostColors, Note, NoteKind, PaneData, PaneDims, Response, Tagged,
    Toward, WIRE, attached_wire,
};
/// tutti's types that rtty has a name of its own for: a split's axis and a tab's split tree, where
/// rtty's `Direction` is a way to go and its `Layout` a tab, and the session's workspaces and
/// panes as its views and snapshots give them.
pub use tutti_wire::{
    Direction as Axis, Layout as Tree, WatchPane as Watched, WorkspaceView as Workspace,
};
pub use tutti_wire::{Frame, PaneId, Request, TabId, WorkspaceId, socket_path};

use crate::layout::Direction;
use crate::pane::{Notice, SCROLLBACK, Trace};
use crate::scheme::Scheme;

/// The revision of the host frames a `tutti proxy` speaks, which its hello names: tutti's
/// `HOST_REV`, which is its client's and no part of `tutti-wire`.
pub const HOST_REV: u32 = 2;

/// What rtty attaches as: its bundle's identifier, which tutti tells its watchers so that one (a
/// notch app) can bring rtty forward.
const TERMINAL: &str = "local.rtty";

impl From<Toward> for Direction {
    fn from(toward: Toward) -> Direction {
        match toward {
            Toward::Left => Direction::Left,
            Toward::Right => Direction::Right,
            Toward::Up => Direction::Up,
            Toward::Down => Direction::Down,
        }
    }
}

impl From<Direction> for Toward {
    fn from(direction: Direction) -> Toward {
        match direction {
            Direction::Left => Toward::Left,
            Direction::Right => Toward::Right,
            Direction::Up => Toward::Up,
            Direction::Down => Toward::Down,
        }
    }
}

/// A request as the control frame that carries it, under `req` when the daemon is to echo one on
/// its answer.
fn control(req: Option<u64>, request: &Request) -> Frame {
    let tagged = Tagged { req, body: request };
    Frame::Control(serde_json::to_vec(&tagged).expect("a request serializes"))
}

/// What the daemon says in a control frame: an event (what it `Tells`, an `Event` here being a
/// terminal's), or its answer to a request, with the request's `req` if it had one.
enum Said {
    Event(Tells),
    Answer(Option<u64>, Response),
}

/// What the control frame `json` says; nothing when it reads as neither, which is a newer
/// daemon's and passed over, as tutti's rule for its wire has an older client do.
fn said(json: &[u8]) -> Option<Said> {
    if let Ok(event) = serde_json::from_slice(json) {
        return Some(Said::Event(event));
    }
    let Tagged { req, body } = serde_json::from_slice(json).ok()?;
    Some(Said::Answer(req, body))
}

/// The next whole frame from `input`, read on into `buf` until it is there; `None` once the other
/// side closed the connection. Taking a frame off the front of `buf` moves none of the rest, and
/// nothing is read while a whole frame waits: a frame of a pane's few bytes costs no more than
/// those.
fn next(input: &mut impl Read, buf: &mut VecDeque<u8>) -> io::Result<Option<Frame>> {
    loop {
        let frame = Frame::decode(buf.make_contiguous());
        let unread = |err| io::Error::other(format!("tutti sent what is no frame: {err}"));
        if let Some((frame, used)) = frame.map_err(unread)? {
            buf.drain(..used);
            return Ok(Some(frame));
        }
        let mut chunk = [0; 1 << 16];
        match input.read(&mut chunk) {
            Ok(0) => return Ok(None),
            Ok(read) => buf.extend(&chunk[..read]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

/// A host frame rtty reads: a `tutti proxy`'s hello.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HostFrame {
    Hello {
        rev: u32,
    },
    #[serde(other)]
    Other,
}

/// The session whose daemon listens on `socket`, by name.
pub fn name(socket: &Path) -> String {
    socket
        .file_stem()
        .map_or_else(|| socket.display().to_string(), |stem| stem.display().to_string())
}

/// The connection to the daemon listening on `socket`, or why there is none. A Unix socket's
/// connect never waits: a listener whose queue is full refuses at once.
pub fn connect(socket: &Path) -> io::Result<UnixStream> {
    UnixStream::connect(socket).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
            let socket = socket.display();
            io::Error::new(err.kind(), format!("no tutti daemon listens on {socket}"))
        }
        _ => err,
    })
}

/// How long the daemon has to answer a session's opening, in all.
const OPENING: Duration = Duration::from_secs(3);

/// A connection read until a deadline: each read waits only for what is left of it, polled, not
/// a socket timeout, which macOS refuses to set (EINVAL) once the other side has hung up.
struct Until<'a>(&'a UnixStream, Instant);

impl Read for Until<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Whole milliseconds, rounded up: never short of the deadline.
        let left = self.1.saturating_duration_since(Instant::now()).as_micros().div_ceil(1000);
        let mut ready = libc::pollfd { fd: self.0.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one pollfd, which outlives the call.
        match unsafe { libc::poll(&mut ready, 1, i32::try_from(left).unwrap_or(i32::MAX)) } {
            0 => Err(io::ErrorKind::TimedOut.into()),
            -1 => Err(io::Error::last_os_error()),
            // Something to read, or the end: the read does not wait.
            _ => Read::read(&mut self.0, buf),
        }
    }
}

/// What a session's connection tells the window that shows its panes.
#[derive(Debug, PartialEq)]
pub enum Told {
    /// The session's tabs, as they are now.
    Layout(Vec<Workspace>),
    /// Someone other than rtty focused this pane: the window shows it.
    Focused(u64),
    /// A focus move from this pane, the one rtty shows focused, went past the edge of tutti's
    /// tab: rtty's layout takes it on.
    FocusEdge(u64, Direction),
    /// A resize from this pane found no split of tutti's that way: rtty's layout takes it.
    ResizeEdge(u64, Direction),
    /// The pane a split or a run rtty asked for made, in the layout by now.
    Created(u64),
    /// What tutti said to a request of rtty's it refused.
    Refused(String),
    /// The connection ended, as this says.
    Ended(String),
}

/// One connection to a tutti session's daemon, for a window that shows the session's panes, and
/// as long as one of them does (each holds it). It streams every pane the window shows. Dropping
/// it closes the connection, which detaches.
pub struct Session {
    /// The daemon's socket, which names the session.
    pub socket: PathBuf,
    stream: UnixStream,
    /// To the connection's writer.
    frames: mpsc::Sender<Frame>,
    /// The panes streamed, which the connection's reader feeds and tells what the session says of
    /// them.
    shown: Arc<Mutex<Shown>>,
}

/// The panes a session's connection streams, by tutti's id, the session's last snapshot, and the
/// stream requests the daemon may still answer, by their `req`, the last of which is `req`.
#[derive(Default)]
struct Shown {
    latest: Vec<Watched>,
    streams: BTreeMap<u64, Stream>,
    asked: HashMap<u64, Asked>,
    req: u64,
}

/// A stream request, by its pane.
#[derive(Clone, Copy, PartialEq)]
enum Asked {
    Attach(u64),
    Detach(u64),
}

/// What shows a pane the connection streams: the terminal its frames feed, what the session's
/// snapshots say of the pane, which it reads its name from, the window's notices of it (`Output`
/// at most one in flight while `pending`), and the trace of what it reads.
pub struct Shows {
    pub term: Arc<Mutex<Terminal>>,
    pub watched: Arc<Mutex<Option<Watched>>>,
    pub notices: UnboundedSender<Notice>,
    pub pending: Arc<AtomicBool>,
    pub trace: Option<Trace>,
}

/// A pane the connection streams to the pane that shows it.
struct Stream {
    shows: Shows,
    /// The pane's vote in its size, rows and columns.
    size: (u16, u16),
    /// The scrollback rows a `PaneRows` announced, gathered until its snapshot.
    rows: Option<Vec<u8>>,
    started: bool,
    /// Whether it ended: its program exited, or the connection did.
    ended: bool,
}

impl Session {
    /// Opens the session whose daemon `stream` reaches, listening on `socket` (or a `tutti
    /// proxy`'s, whose hello comes first and must speak `HOST_REV`): attached as a quiet view that
    /// hands moves on, and watching. The daemon's answer names its wire's revision and floor, which
    /// must work with rtty's (`Wire::accepts`), and gives the session's tabs; its first snapshot
    /// names the panes. The daemon has `OPENING` to answer, however it trickles, so this runs off
    /// the main thread. The session, its tabs, and what it tells the window from then on.
    pub fn open(
        mut stream: UnixStream,
        socket: PathBuf,
    ) -> io::Result<(Session, Vec<Workspace>, UnboundedReceiver<Told>)> {
        let deadline = Instant::now() + OPENING;
        let terminal = Some(TERMINAL.into());
        let attach = Request::Attach { terminal, handoff: true, view: true, quiet: true };
        let watch = control(None, &Request::Watch);
        stream.write_all(&[control(None, &attach).encode(), watch.encode()].concat())?;
        let (mut buf, mut tabs, mut first) = (VecDeque::new(), None, None);
        // A change told before the answer may be older than the answer's layout or newer: the
        // daemon registers the client, takes the layout, then queues its answer, and tells every
        // client of a change meanwhile. The session's view, asked after the answer, settles it:
        // the daemon reads a connection's requests in order, so it takes that view after any
        // change it told before its answer.
        let (mut early, mut asked) = (false, false);
        while tabs.is_none() || first.is_none() || asked {
            let frame =
                next(&mut Until(&stream, deadline), &mut buf).map_err(|err| match err.kind() {
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                        let waited = OPENING.as_secs();
                        io::Error::other(format!("tutti's daemon did not answer within {waited} s"))
                    }
                    _ => err,
                })?;
            let json = match frame.ok_or_else(|| io::Error::other(CLOSED))? {
                Frame::Host(json) => {
                    if let Ok(HostFrame::Hello { rev }) = serde_json::from_slice(&json)
                        && rev != HOST_REV
                    {
                        return Err(io::Error::other(format!(
                            "tutti proxy speaks host frames revision {rev}, rtty {HOST_REV}: run \
                             the same tutti on both ends"
                        )));
                    }
                    continue;
                }
                Frame::Control(json) => json,
                _ => return Err(io::Error::other("tutti sent a pane frame to a quiet attach")),
            };
            // The answer's revision and floor, read on their own: a daemon too new for the rest
            // of its answer to read here is refused as one, not waited out.
            if let Some((rev, floor)) = attached_wire(&json) {
                WIRE.accepts("rtty", rev, floor).map_err(io::Error::other)?;
            }
            match said(&json) {
                Some(Said::Answer(_, Response::Attached { workspaces, .. })) => {
                    tabs = Some(workspaces);
                    if early {
                        stream.write_all(&control(None, &Request::View).encode())?;
                        asked = true;
                    }
                }
                Some(Said::Answer(_, Response::View { workspaces })) if asked => {
                    (tabs, asked) = (Some(workspaces), false)
                }
                // One after the answer is newer than it.
                Some(Said::Event(Tells::LayoutChanged { workspaces })) => match tabs {
                    None => early = true,
                    Some(_) => tabs = Some(workspaces),
                },
                Some(Said::Event(Tells::Snapshot { panes, .. })) => first = Some(panes),
                Some(Said::Answer(_, Response::Error { message })) => {
                    return Err(io::Error::other(message));
                }
                _ => {}
            }
        }
        let shown =
            Arc::new(Mutex::new(Shown { latest: first.unwrap_or_default(), ..Shown::default() }));
        let (frames, outgoing) = mpsc::channel::<Frame>();
        let mut writer = stream.try_clone()?;
        thread::Builder::new().name("tutti session writer".into()).spawn(move || {
            // Every frame of the connection is encoded here, and what is typed into a pane goes in
            // as many frames as its length takes: a longer frame, tutti's encoder panics on.
            let framed = |frame| match frame {
                Frame::Input { pane, bytes } => Frame::inputs(pane, bytes),
                frame => vec![frame],
            };
            for frame in outgoing.into_iter().flat_map(framed) {
                if writer.write_all(&frame.encode()).is_err() {
                    break;
                }
            }
        })?;
        let (tell, told) = unbounded();
        let (input, listening, replies) = (stream.try_clone()?, shown.clone(), frames.clone());
        thread::Builder::new()
            .name("tutti session reader".into())
            .spawn(move || listen(input, buf, &listening, &replies, &tell))?;
        let session = Session { socket, stream, frames, shown };
        Ok((session, tabs.unwrap_or_default(), told))
    }

    /// Sends `frame`, behind what was sent before.
    pub fn send(&self, frame: Frame) {
        let _ = self.frames.send(frame);
    }

    /// Asks the daemon, behind what was asked before; its answer, when it is one rtty acts on,
    /// comes as `Told`.
    pub fn ask(&self, request: &Request) {
        self.send(control(None, request));
    }

    fn shown(&self) -> std::sync::MutexGuard<'_, Shown> {
        self.shown.lock().expect("a tutti session's reader panicked")
    }

    /// Streams tutti pane `pane` to what `shows` it, which votes `rows`×`cols` for its size: the
    /// scrollback rows the pane holds, a snapshot, every pty read raw with the keyboard protocols
    /// its program sets, and its exit. The pane hears what the session's snapshots say of it, the
    /// last one's at once, and of its notes. While a detach of the pane waits for its answer, the
    /// stream is asked for once it comes (`Shown::answered`), so that all the daemon sends of the
    /// pane until then is the old stream's. Refused while another pane of the window shows it.
    pub fn stream(&self, pane: u64, rows: u16, cols: u16, shows: Shows) -> io::Result<()> {
        let mut shown = self.shown();
        if shown.streams.contains_key(&pane) {
            return Err(io::Error::other("it shows in this window already"));
        }
        let now = shown.latest.iter().find(|watched| watched.id.0 == pane).cloned();
        tell_of(&shows.notices, None, now.as_ref());
        *shows.watched.lock().expect("a pane panicked") = now;
        let size = (rows, cols);
        let stream = Stream { shows, size, rows: None, started: false, ended: false };
        shown.streams.insert(pane, stream);
        if !shown.detaching(pane) {
            shown.attach(pane, &self.frames);
        }
        Ok(())
    }

    /// The pane showing tutti pane `pane` votes `rows`×`cols` for its size: the view tells every
    /// pane it streams at its vote, as an attached client's set, which tutti takes whole in place
    /// of the last, and which moves each stream's own vote too.
    pub fn vote(&self, pane: u64, rows: u16, cols: u16) {
        let mut shown = self.shown();
        if let Some(stream) = shown.streams.get_mut(&pane) {
            stream.size = (rows, cols);
        }
        self.ask(&shown.sizes());
    }

    /// Tutti pane `pane` is shown no more: its stream is detached, its vote in the pane's size
    /// with it, and the view's set no longer names it; the pane may be streamed again. A stream
    /// still waiting behind a detach was never asked for, and needs none.
    pub fn forget(&self, pane: u64) {
        let mut shown = self.shown();
        if shown.streams.remove(&pane).is_none() {
            return;
        }
        shown.asked.retain(|_, asked| *asked != Asked::Attach(pane));
        if !shown.detaching(pane) {
            let req = shown.asking(Asked::Detach(pane));
            self.send(control(Some(req), &Request::PaneDetach { pane: PaneId(pane) }));
        }
        self.ask(&shown.sizes());
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

const CLOSED: &str = "tutti's daemon closed the connection";

/// Reads a session's connection, from what `buf` holds of it on, until it ends: each pane frame,
/// and each event of a stream, goes to the stream of the pane it names, its replies by `replies`;
/// the session's snapshots and notes go to the panes streamed; the layout, focus, edges and other
/// refusals to the window. One thread reads them all, and waits on nothing but a terminal's lock
/// while it feeds one. The daemon keeps a queue a stream and skips one that falls behind to a
/// fresh snapshot alone, but a stream's frames wait behind every frame sent before them: the reads
/// of one pane that came together are fed at once, so that a flood of short reads costs what its
/// bytes do, and not a lock and a feed each. Then every stream, and the window, hear how it ended.
fn listen(
    mut input: impl Read,
    mut buf: VecDeque<u8>,
    shown: &Mutex<Shown>,
    replies: &mpsc::Sender<Frame>,
    tell: &UnboundedSender<Told>,
) {
    let shown = || shown.lock().expect("the window panicked");
    let ended = loop {
        let json = match next(&mut input, &mut buf) {
            Ok(Some(Frame::Control(json))) => json,
            Ok(Some(Frame::PaneRaw { pane, mut bytes })) => {
                while let Ok(Some((Frame::PaneRaw { pane: same, bytes: more }, used))) =
                    Frame::decode(buf.make_contiguous())
                    && same == pane
                {
                    bytes.extend(more);
                    buf.drain(..used);
                }
                shown().raw(pane.0, &bytes, replies);
                continue;
            }
            Ok(Some(Frame::PaneSnapshot(PaneData { pane, rows, cols, bytes, .. }))) => {
                shown().screen(pane.0, rows, cols, &bytes);
                continue;
            }
            // What changed of a pane's screen goes to attached clients that are not quiet.
            Ok(Some(Frame::PaneDelta(_))) => break "tutti sent a quiet attach a delta".to_owned(),
            Ok(Some(Frame::Input { .. } | Frame::Host(_))) => continue,
            Ok(None) => break CLOSED.to_owned(),
            Err(err) => break format!("tutti's connection broke: {err}"),
        };
        let told = match said(&json) {
            Some(Said::Event(Tells::PaneRows { pane, skipped, .. })) => {
                if let Some(stream) = shown().streamed(pane.0) {
                    stream.rows = Some(left_out(pane.0, skipped));
                }
                continue;
            }
            // One before the stream's first snapshot is the session's, of an exit before the
            // stream began, which tells it itself after that snapshot.
            Some(Said::Event(Tells::PaneExited { pane, code })) => {
                let mut shown = shown();
                if shown.streams.get(&pane.0).is_some_and(|stream| stream.started) {
                    shown.end(pane.0, &format!("Exited with code {code}"));
                }
                continue;
            }
            Some(Said::Answer(Some(req), Response::Ok)) => {
                shown().answered(req, None, replies);
                continue;
            }
            Some(Said::Answer(Some(req), Response::Error { message })) => {
                shown().answered(req, Some(&message), replies);
                continue;
            }
            Some(Said::Event(Tells::Snapshot { panes, .. })) => {
                shown().snapshot(panes);
                continue;
            }
            Some(Said::Event(Tells::Noted { note })) => {
                shown().noted(note);
                continue;
            }
            Some(Said::Event(Tells::LayoutChanged { workspaces })) => Told::Layout(workspaces),
            Some(Said::Event(Tells::PaneFocused { pane })) => Told::Focused(pane.0),
            Some(Said::Event(Tells::FocusEdge { pane, toward })) => {
                Told::FocusEdge(pane.0, toward.into())
            }
            Some(Said::Event(Tells::ResizeEdge { pane, toward })) => {
                Told::ResizeEdge(pane.0, toward.into())
            }
            Some(Said::Answer(_, Response::PaneCreated { id })) => Told::Created(id.0),
            Some(Said::Answer(_, Response::Error { message })) => Told::Refused(message),
            _ => continue,
        };
        let _ = tell.unbounded_send(told);
    };
    let mut shown = shown();
    let streams: Vec<u64> = shown.streams.keys().copied().collect();
    for pane in streams {
        shown.end(pane, &ended);
    }
    drop(shown);
    let _ = tell.unbounded_send(Told::Ended(ended));
}

impl Shown {
    /// The stream of `pane`, but while a detach of it waits for its answer: what comes of the pane
    /// until then is the detached stream's, and goes nowhere. The daemon streams only what rtty
    /// asked for, so a frame of any other pane is noted.
    fn streamed(&mut self, pane: u64) -> Option<&mut Stream> {
        if self.detaching(pane) {
            return None;
        }
        let stream = self.streams.get_mut(&pane);
        if stream.is_none() {
            crate::note(&format!("tutti sent a frame of pane {pane}, which rtty does not stream"));
        }
        stream
    }

    /// `asked`, under a `req` of its own.
    fn asking(&mut self, asked: Asked) -> u64 {
        self.req += 1;
        self.asked.insert(self.req, asked);
        self.req
    }

    /// Whether a detach of `pane` waits for its answer.
    fn detaching(&self, pane: u64) -> bool {
        self.asked.values().any(|asked| *asked == Asked::Detach(pane))
    }

    /// Asks the daemon, by `out`, to stream `pane` to its stream, at its vote.
    fn attach(&mut self, pane: u64, out: &mpsc::Sender<Frame>) {
        let Some(&Stream { size: (rows, cols), .. }) = self.streams.get(&pane) else { return };
        let (req, history) = (self.asking(Asked::Attach(pane)), SCROLLBACK as u32);
        let attach = Request::PaneAttach {
            pane: PaneId(pane),
            rows,
            cols,
            history,
            inline: true,
            since: None,
            keys: true,
        };
        let _ = out.send(control(Some(req), &attach));
    }

    /// The daemon answered request `req`, refusing it as `why` says if it did. A detach's answer is
    /// the last of the stream it ended: a pane shown again meanwhile is streamed now, by `out`. An
    /// attach's refusal is said in its pane.
    fn answered(&mut self, req: u64, why: Option<&str>, out: &mpsc::Sender<Frame>) {
        match (self.asked.remove(&req), why) {
            (Some(Asked::Detach(pane)), _) => self.attach(pane, out),
            (Some(Asked::Attach(pane)), Some(why)) => self.refused(pane, why),
            _ => {}
        }
    }

    /// Every pane streamed at its vote: the view's whole set.
    fn sizes(&self) -> Request {
        let dims = |(&pane, stream): (&u64, &Stream)| {
            let (rows, cols) = stream.size;
            PaneDims { pane: PaneId(pane), rows, cols }
        };
        Request::ViewSizes { panes: self.streams.iter().map(dims).collect() }
    }

    /// A raw read of `pane`'s: gathered while a `PaneRows` announced rows before a snapshot, else
    /// fed as it is.
    fn raw(&mut self, pane: u64, bytes: &[u8], replies: &mpsc::Sender<Frame>) {
        let Some(stream) = self.streamed(pane) else { return };
        match stream.rows.as_mut() {
            Some(rows) => rows.extend_from_slice(bytes),
            None => stream.feed(|term| term.advance(bytes), bytes, Some((pane, replies))),
        }
    }

    /// A snapshot of `pane`'s screen at `rows`×`cols`, after the scrollback rows its `PaneRows`
    /// announced (`show`).
    fn screen(&mut self, pane: u64, rows: u16, cols: u16, bytes: &[u8]) {
        let Some(stream) = self.streamed(pane) else { return };
        stream.started = true;
        let scrolled = stream.rows.take().unwrap_or_default();
        stream.feed(|term| show(term, rows, cols, &scrolled, bytes), bytes, None);
    }

    /// The daemon refused to stream `pane`, which the session no longer has: the pane says why,
    /// and stays.
    fn refused(&mut self, pane: u64, why: &str) {
        let Some(stream) = self.streamed(pane) else { return };
        let said = format!("[tutti pane {pane}: {why}]\r\n");
        stream.feed(|term| term.advance(said.as_bytes()), &[], None);
    }

    /// `pane`'s stream ended as `why` says, which the pane showing it hears, once.
    fn end(&mut self, pane: u64, why: &str) {
        if let Some(stream) = self.streams.get_mut(&pane)
            && !mem::replace(&mut stream.ended, true)
        {
            let _ = stream.shows.notices.unbounded_send(Notice::Exit(why.to_owned()));
        }
    }

    /// The session's snapshot: each pane streamed hears what changed of it.
    fn snapshot(&mut self, panes: Vec<Watched>) {
        for (id, stream) in &self.streams {
            let Some(now) = panes.iter().find(|pane| pane.id.0 == *id) else { continue };
            let mut watched = stream.shows.watched.lock().expect("a pane panicked");
            let was = watched.replace(now.clone());
            tell_of(&stream.shows.notices, was.as_ref(), Some(now));
        }
        self.latest = panes;
    }

    /// A note: an agent blocked or done is a notification, from the pane that shows it. A bell's
    /// and a program's own notification came raw in its stream.
    fn noted(&self, note: Note) {
        let said = match note.kind {
            NoteKind::Blocked => "blocked",
            NoteKind::Done => "done",
            NoteKind::Bell | NoteKind::Notify => return,
        };
        let Some(stream) = self.streams.get(&note.pane.0) else { return };
        let title = note.title.unwrap_or(note.pane_title);
        let body = note.body.unwrap_or_else(|| said.to_owned());
        let event = Event::Notification { title: Some(title), body: Some(body) };
        let _ = stream.shows.notices.unbounded_send(Notice::Event(event));
    }
}

impl Stream {
    /// Changes the terminal with `change`, under its lock, for `read`: then the engine's replies go
    /// back by `answer` (each answers a query the daemon left unanswered; a snapshot asks nothing of
    /// anyone), its other events to the window, and the window hears there is output to draw.
    fn feed(
        &self,
        change: impl FnOnce(&mut Terminal),
        read: &[u8],
        answer: Option<(u64, &mpsc::Sender<Frame>)>,
    ) {
        let Shows { term, notices, pending, trace, .. } = &self.shows;
        if let Some(trace) = trace {
            trace.log('<', read);
        }
        let mut held = term.lock().expect("the window panicked");
        change(&mut held);
        let events = held.take_events();
        drop(held);
        for event in events {
            match (event, answer) {
                (Event::Reply(bytes), Some((pane, replies))) => {
                    if let Some(trace) = trace {
                        trace.log('>', &bytes);
                    }
                    let _ = replies.send(Frame::Input { pane: PaneId(pane), bytes });
                }
                (Event::Reply(_), None) => {}
                (event, _) => {
                    let _ = notices.unbounded_send(Notice::Event(event));
                }
            }
        }
        if !pending.swap(true, Ordering::AcqRel) {
            let _ = notices.unbounded_send(Notice::Output);
        }
    }
}

/// The session's snapshot said `was` of a pane and now says `now` (nothing, of a pane it has not
/// named), which its notices hear. A new title or directory renames it; its agent working shows on
/// the tab's pill, going on, or paused while blocked or holding a permission request; and the
/// program's claim on the keys (`TUTTI_KEYS`, which tutti's editor plugins set) is the window's to
/// honour as `RTTY_KEYS` is. The pane's own OSC 9, 777, 9;4 and 7501 come raw, and its agent's turn
/// ending as a note: none of them is told again here.
fn tell_of(notices: &UnboundedSender<Notice>, was: Option<&Watched>, now: Option<&Watched>) {
    fn named(watched: Option<&Watched>) -> (&str, Option<&str>) {
        watched.map(|pane| (pane.title.as_str(), pane.cwd.as_deref())).unwrap_or_default()
    }
    fn keys(watched: Option<&Watched>) -> Option<&String> {
        watched.and_then(|pane| pane.user_vars.get("TUTTI_KEYS"))
    }
    let tell = |event| {
        let _ = notices.unbounded_send(Notice::Event(event));
    };
    if named(was) != named(now) {
        tell(Event::Title(named(now).0.to_owned()));
    }
    let pill = |watched: Option<&Watched>| match watched {
        Some(pane) if pane.approval.is_some() || pane.state == AgentState::Blocked => 4,
        Some(pane) if pane.state == AgentState::Working => 3,
        _ => 0,
    };
    if pill(was) != pill(now) {
        tell(Event::Progress { state: pill(now), percent: None });
    }
    if keys(was) != keys(now) {
        let value = keys(now).cloned().unwrap_or_default();
        tell(Event::UserVar { name: "TUTTI_KEYS".into(), value });
    }
}

/// A scheme's colours as tutti takes a terminal's.
pub fn colors(scheme: &Scheme) -> HostColors {
    let wide = |Rgb(r, g, b): Rgb| tutti_wire::Rgb {
        r: u16::from(r) * 257,
        g: u16::from(g) * 257,
        b: u16::from(b) * 257,
    };
    HostColors {
        fg: wide(scheme.color(FOREGROUND)),
        bg: wide(scheme.color(BACKGROUND)),
        cursor: Some(wide(scheme.color(CURSOR))),
        dark: scheme.is_dark(),
    }
}

/// A line on the rows of `pane` a stream leaves out before those it hands on, if it leaves any.
fn left_out(pane: u64, rows: u64) -> Vec<u8> {
    match rows {
        0 => Vec::new(),
        rows => format!("[tutti pane {pane}: {rows} lines not shown]\r\n").into_bytes(),
    }
}

/// The screen the rows a `PaneRows` announced are printed on, as tutti's `pane attach --inline`
/// prints them: the cursor hidden until the snapshot shows it, the alternate screen left, the
/// scroll region whole, origin mode and insert off, autowrap on, no rendition, link or other
/// character set, and the primary screen cleared from the top with ED 0, which pushes nothing into
/// the scrollback.
const PLAIN: &[u8] =
    b"\x1b[?25l\x1b[?1049l\x1b[r\x1b[?6l\x1b[?7h\x1b[4l\x1b[0m\x1b]8;;\x1b\\\x1b(B\x0f\x1b[H\x1b[J";

/// Brings `term` level with a snapshot of the pane at `rows`×`cols`: resized to that first, as the
/// pane's own terminal was (both are rtty-vt, and reflow alike); then `scrolled`, the scrollback
/// rows its `PaneRows` announced, printed over the stale screen and lifted into the scrollback by
/// as many line feeds at the bottom as they fill of the screen, so nothing is missed and nothing is
/// there twice; then the snapshot, on the blank screen (`strip`).
fn show(term: &mut Terminal, rows: u16, cols: u16, scrolled: &[u8], snapshot: &[u8]) {
    if term.size() != (rows, cols) {
        term.resize(rows, cols);
    }
    if !scrolled.is_empty() {
        term.advance(PLAIN);
        term.advance(scrolled);
        let (row, col) = term.cursor_position();
        let lift = "\n".repeat(usize::from(row + u16::from(col > 0)));
        term.advance(format!("\x1b[{rows};1H{lift}").as_bytes());
    }
    term.advance(&strip(snapshot));
}

/// A snapshot as a stream's terminal takes it. Its first ED 2, the primary screen's, becomes ED 0:
/// ED 2 would push the stale screen into the scrollback. And mode 2031 goes, whose light/dark
/// reports are the daemon's to send (rtty tells it of a flip with `HostColors`). The keyboard
/// protocols stay: the stream hands on each change to them.
fn strip(snapshot: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(snapshot.len());
    let (mut rest, mut cleared) = (snapshot, false);
    while let Some(at) = rest.windows(2).position(|pair| pair == b"\x1b[") {
        out.extend_from_slice(&rest[..at]);
        let end = rest[at + 2..].iter().position(|byte| (0x40..=0x7e).contains(byte));
        let end = end.map_or(rest.len(), |last| at + last + 3);
        let seq = &rest[at..end];
        if seq == b"\x1b[2J" && !mem::replace(&mut cleared, true) {
            out.extend_from_slice(b"\x1b[J");
        } else if !seq.starts_with(b"\x1b[?2031") {
            out.extend_from_slice(seq);
        }
        rest = &rest[end..];
    }
    out.extend_from_slice(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::Pane;
    use crate::scheme::{find, load};
    use rtty_vt::input;
    use serde_json::json;
    use tutti_wire::{MAX_FRAME_LEN, WIRE_FLOOR, WIRE_REV};

    /// A session as tutti's daemon writes its answer to an attach, at wire revision `rev` and, from
    /// a daemon that names one, with floor `floor`: one workspace, 5, its tab 1 the session's
    /// current one, panes 2 and 3 side by side, 3 focused there, and its tab 4 with no pane yet.
    fn attached_json(rev: u32, floor: Option<u32>) -> String {
        let floor = floor.map(|floor| format!(r#""wire_floor":{floor},"#)).unwrap_or_default();
        format!(
            r#"{{"type":"attached","session":"s","wire_rev":{rev},{floor}"restorable":null,"notes":[],
              "workspaces":[{{"id":5,"name":"w","dir":"/w","branch":null,"changes":null,
                "changed_files":[],"stale":false,"parent":null,"tabs":[
                {{"id":1,"name":"one","active":true,"active_pane":3,"panes":[],
                  "layout":{{"Split":{{"direction":"horizontal","ratio":0.25,
                    "first":{{"Leaf":2}},"second":{{"Leaf":3}}}}}}}},
                {{"id":4,"name":"two","active":false,"active_pane":null,"panes":[],
                  "layout":null}}]}}]}}"#
        )
    }

    /// The answer of a daemon at rtty's own revision and floor.
    fn answer() -> String {
        attached_json(WIRE_REV, Some(WIRE_FLOOR))
    }

    /// A layout with one workspace, 5, and no tab, as the event or the answer `kind`.
    fn no_tabs(kind: &str) -> String {
        format!(r#"{{"type":"{kind}","workspaces":[{{"id":5,"name":"w","dir":"/w","tabs":[]}}]}}"#)
    }

    /// The session's tabs by id, workspace by workspace.
    fn ids(workspaces: &[Workspace]) -> Vec<(u64, Vec<u64>)> {
        let tabs = |workspace: &Workspace| workspace.tabs.iter().map(|tab| tab.id.0).collect();
        workspaces.iter().map(|workspace| (workspace.id.0, tabs(workspace))).collect()
    }

    /// The daemon's end of a connection, played by a test.
    struct Daemon {
        stream: UnixStream,
        buf: VecDeque<u8>,
    }

    impl Daemon {
        fn send(&mut self, frame: Frame) {
            self.stream.write_all(&frame.encode()).unwrap();
        }

        fn say(&mut self, json: &str) {
            self.send(Frame::Control(json.into()));
        }

        /// The next frame rtty sent, `None` once it closed the connection.
        fn heard(&mut self) -> Option<Frame> {
            next(&mut self.stream, &mut self.buf).unwrap()
        }

        fn asked(&mut self) -> serde_json::Value {
            match self.heard() {
                Some(Frame::Control(json)) => serde_json::from_slice(&json).unwrap(),
                frame => panic!("{frame:?}"),
            }
        }

        /// The session as a watcher sees it, its panes as `panes` writes them: `(id, state, user
        /// vars)`, each titled `sh` and in `/w`.
        fn watched(&mut self, panes: &[(u64, &str, &str)]) {
            let panes: Vec<String> = panes
                .iter()
                .map(|(id, state, vars)| {
                    format!(
                        r#"{{"id":{id},"workspace":"w","dir":"/w","tab":"t","title":"sh",
                        "agent":null,"state":"{state}","source":null,"exited":null,
                        "focused":false,"user_vars":{{{vars}}},"cwd":"/w","approval":null}}"#
                    )
                })
                .collect();
            let panes = panes.join(",");
            self.say(&format!(
                r#"{{"type":"snapshot","session":"s","wire_rev":19,"attached_terminal":null,"panes":[{panes}]}}"#
            ));
        }

        /// Before stream `pane`'s snapshot: no rows left out, none handed on.
        fn rows(&mut self, pane: u64) {
            let rows = r#"{"type":"pane_rows","pane":P,"epoch":1,"top":0,"skipped":0}"#;
            self.say(&rows.replace('P', &pane.to_string()));
        }

        fn raw(&mut self, pane: u64, bytes: &[u8]) {
            self.send(Frame::PaneRaw { pane: PaneId(pane), bytes: bytes.to_vec() });
        }

        /// A snapshot of `pane`'s screen at `rows`×`cols`.
        fn snapshot(&mut self, pane: u64, rows: u16, cols: u16, bytes: Vec<u8>) {
            let pane = PaneId(pane);
            self.send(Frame::PaneSnapshot(PaneData { pane, rows, cols, seq: 0, bytes }));
        }

        /// Stream `pane` at 4×40, its screen what a terminal makes of `screen`.
        fn screen(&mut self, pane: u64, screen: &[u8]) {
            let mut shell = Terminal::new(4, 40, 100);
            shell.advance(screen);
            self.snapshot(pane, 4, 40, shell.snapshot());
        }
    }

    type Opened = io::Result<(Session, Vec<Workspace>, UnboundedReceiver<Told>)>;

    /// `Session::open` on a thread of its own over a socket pair, and the daemon's end, which has
    /// heard the attach and the watch.
    fn opening() -> (thread::JoinHandle<Opened>, Daemon) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let open = thread::spawn(move || Session::open(ours, PathBuf::from("/run/tutti/s.sock")));
        let mut daemon = Daemon { stream: theirs, buf: VecDeque::new() };
        let attach = json!({
            "type": "attach", "terminal": "local.rtty", "handoff": true, "view": true, "quiet": true
        });
        assert_eq!(daemon.asked(), attach);
        assert_eq!(daemon.asked(), json!({"type": "watch"}));
        (open, daemon)
    }

    /// A session opened as the daemon answers it, panes 2 and 3 idle, and the daemon's end.
    fn session() -> (Arc<Session>, UnboundedReceiver<Told>, Daemon) {
        let (open, mut daemon) = opening();
        daemon.say(&answer());
        daemon.watched(&[(2, "idle", ""), (3, "idle", "")]);
        let (session, _, told) = open.join().unwrap().unwrap();
        (Arc::new(session), told, daemon)
    }

    /// Tutti pane `pane` of `session`, 4×40 and with no colours, as the daemon streams it, its
    /// stream not yet begun, and its notices; the daemon has heard the stream asked for.
    fn attach(session: &Arc<Session>, daemon: &mut Daemon, pane: u64) -> (Pane, Notices) {
        let (shown, notices) =
            Pane::attach(session.clone(), pane, 4, 40, None, None, false).unwrap();
        attached_to(daemon, pane);
        (shown, notices)
    }

    /// The daemon has heard pane `pane` streamed at 4×40: the request's `req`.
    fn attached_to(daemon: &mut Daemon, pane: u64) -> u64 {
        let mut asked = daemon.asked();
        let req = asked["req"].take().as_u64().expect("a req");
        let attach = json!({
            "type": "pane_attach", "pane": pane, "rows": 4, "cols": 40, "history": 10000,
            "inline": true, "since": null, "keys": true, "req": null
        });
        assert_eq!(asked, attach);
        req
    }

    type Notices = UnboundedReceiver<Notice>;

    /// Pane 3 of a session as `attach` has it, alone on its connection, and the daemon's end.
    fn attached() -> (Pane, Notices, Daemon) {
        let (session, _told, mut daemon) = session();
        let (pane, notices) = attach(&session, &mut daemon, 3);
        assert_eq!(pane.tutti(), Some((Path::new("/run/tutti/s.sock"), 3)));
        (pane, notices, daemon)
    }

    fn wait_until(term: &Mutex<Terminal>, what: &str, done: impl Fn(&Terminal) -> bool) {
        let started = Instant::now();
        while !done(&term.lock().unwrap()) {
            let screen = term.lock().unwrap().contents();
            assert!(started.elapsed() < Duration::from_secs(5), "never {what}: {screen:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// What a receiver gets up to the first `until` takes, within 5 s.
    fn received<T: std::fmt::Debug>(
        receiver: &mut UnboundedReceiver<T>,
        until: impl Fn(&T) -> bool,
    ) -> Vec<T> {
        let (started, mut seen) = (Instant::now(), Vec::new());
        while !seen.last().is_some_and(&until) {
            match receiver.try_recv() {
                Ok(item) => seen.push(item),
                Err(_) => thread::sleep(Duration::from_millis(5)),
            }
            assert!(started.elapsed() < Duration::from_secs(5), "only told {seen:?}");
        }
        seen
    }

    /// The rows `term` has scrolled into its scrollback, as text.
    fn scrollback(term: &Terminal) -> Vec<String> {
        let rows = term.rows(term.oldest_row(), term.screen_top());
        rows.map(|(_, row)| row.text().trim_end().to_owned()).collect()
    }

    fn lines(range: std::ops::Range<usize>) -> String {
        range.map(|line| format!("L{line}\r\n")).collect()
    }

    fn names(range: std::ops::Range<usize>) -> Vec<String> {
        range.map(|line| format!("L{line}")).collect()
    }

    /// What shows a stream in a test: a 4×40 terminal, and `notices`.
    fn shows(notices: UnboundedSender<Notice>) -> Shows {
        let term = Arc::new(Mutex::new(Terminal::new(4, 40, 100)));
        Shows { term, watched: Arc::default(), notices, pending: Arc::default(), trace: None }
    }

    /// The opening: the answer gives the session's tabs, and a layout change after it replaces
    /// them; the first snapshot names the panes, which a pane shown later hears at once.
    #[test]
    fn a_session_opens_on_its_answer_and_its_first_snapshot() {
        let (open, mut daemon) = opening();
        daemon.say(&answer());
        thread::sleep(Duration::from_millis(50));
        assert!(!open.is_finished(), "the opening waits for the first snapshot");
        daemon.watched(&[(3, "working", r#""TUTTI_KEYS":"pane""#)]);
        let (session, tabs, _told) = open.join().unwrap().unwrap();
        assert_eq!(ids(&tabs), [(5, vec![1, 4])]);
        let one = &tabs[0].tabs[0];
        let split = one.layout.as_ref().map(Tree::panes);
        assert_eq!((split, one.active_pane), (Some(vec![PaneId(2), PaneId(3)]), Some(PaneId(3))));
        assert_eq!(name(&session.socket), "s");
        session.ask(&Request::PaneKill { pane: PaneId(9) });
        assert_eq!(daemon.asked()["type"], "pane_kill", "no view asked: nothing came before");
        let (notices, mut heard) = unbounded();
        let shows = shows(notices);
        let watched = shows.watched.clone();
        session.stream(3, 4, 40, shows).unwrap();
        assert_eq!(daemon.asked()["type"], "pane_attach");
        let state = watched.lock().unwrap().as_ref().map(|pane| pane.state);
        assert_eq!(state, Some(AgentState::Working));
        let seen =
            received(&mut heard, |notice| matches!(notice, Notice::Event(Event::UserVar { .. })));
        assert!(
            matches!(&seen[0], Notice::Event(Event::Title(title)) if title == "sh"),
            "{seen:?}"
        );
        assert!(matches!(seen[1], Notice::Event(Event::Progress { state: 3, .. })), "{seen:?}");

        let (open, mut daemon) = opening();
        daemon.say(&answer());
        daemon.say(&no_tabs("layout_changed"));
        daemon.watched(&[]);
        let (_, tabs, _) = open.join().unwrap().unwrap();
        assert_eq!(ids(&tabs), [(5, vec![])]);
    }

    /// A layout change told before the answer may be newer than the answer's layout (the daemon
    /// tells changes to a client it registered before it took that layout): the opening asks the
    /// session's view, which the daemon takes after it, and opens on that.
    #[test]
    fn a_change_told_before_the_answer_is_settled_by_the_view() {
        let (open, mut daemon) = opening();
        daemon.say(&no_tabs("layout_changed"));
        daemon.say(&answer());
        daemon.watched(&[]);
        thread::sleep(Duration::from_millis(100));
        assert!(!open.is_finished(), "the opening waits for the view");
        assert_eq!(daemon.asked(), json!({"type": "view"}));
        daemon.say(&no_tabs("view"));
        let (_, tabs, _) = open.join().unwrap().unwrap();
        assert_eq!(ids(&tabs), [(5, vec![])]);
    }

    /// What the session says after its opening: its layout, a focus from elsewhere, the edges it
    /// hands on, the pane a request made, and a refusal go to the window, in order; a frame of a
    /// pane not streamed, the exit of one, an event rtty has no use for and one of a newer daemon's
    /// that it does not know go nowhere; and its end says so.
    #[test]
    fn a_sessions_events_go_to_the_window() {
        let (session, mut told, mut daemon) = session();
        session.ask(&Request::PaneSplit { pane: PaneId(3), direction: Axis::Horizontal });
        assert_eq!(daemon.asked()["type"], "pane_split");
        daemon.snapshot(3, 4, 40, b"x".to_vec());
        daemon.say(r#"{"type":"pane_exited","pane":3,"code":0}"#);
        daemon.say(r#"{"type":"layout_changed","workspaces":[]}"#);
        daemon.say(r#"{"req":7,"type":"pane_created","id":8}"#);
        daemon.say(r#"{"type":"state_changed","pane":3,"from":"idle","to":"working"}"#);
        daemon.say(r#"{"type":"pane_teleported","pane":3,"to":9}"#);
        daemon.say(r#"{"type":"pane_focused","pane":2}"#);
        daemon.say(r#"{"type":"focus_edge","pane":3,"toward":"down"}"#);
        daemon.say(r#"{"type":"resize_edge","pane":3,"toward":"left"}"#);
        daemon.say(r#"{"type":"error","message":"no tab 9"}"#);
        drop(daemon);
        let seen = received(&mut told, |said| matches!(said, Told::Ended(_)));
        assert_eq!(
            seen,
            [
                Told::Layout(Vec::new()),
                Told::Created(8),
                Told::Focused(2),
                Told::FocusEdge(3, Direction::Down),
                Told::ResizeEdge(3, Direction::Left),
                Told::Refused("no tab 9".into()),
                Told::Ended(CLOSED.into()),
            ]
        );
    }

    /// The session's snapshots name a pane shown and show its agent on the pill: going on while
    /// it works, paused while it is blocked; a claim on the keys tutti's plugins make is told as
    /// `RTTY_KEYS` is. Other panes' changes tell nothing, and nor does a turn ending: its note
    /// does.
    #[test]
    fn the_sessions_snapshots_tell_the_agents_state() {
        let (session, _told, mut daemon) = session();
        let (_pane, mut heard) = attach(&session, &mut daemon, 3);
        received(&mut heard, |notice| matches!(notice, Notice::Event(Event::Title(_))));
        daemon.watched(&[(2, "working", ""), (3, "working", "")]);
        let progress = |state| move |notice: &Notice| matches!(notice, Notice::Event(Event::Progress { state: s, .. }) if *s == state);
        assert_eq!(received(&mut heard, progress(3)).len(), 1);
        daemon.watched(&[(2, "done", ""), (3, "blocked", r#""TUTTI_KEYS":"pane""#)]);
        let claimed = |notice: &Notice| {
            let claim = Event::UserVar { name: "TUTTI_KEYS".into(), value: "pane".into() };
            matches!(notice, Notice::Event(event) if *event == claim)
        };
        let seen = received(&mut heard, claimed);
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(progress(4)(&seen[0]), "{seen:?}");
        daemon.watched(&[(3, "working", "")]);
        daemon.watched(&[(3, "done", "")]);
        let seen = received(&mut heard, progress(0));
        let notified =
            seen.iter().any(|notice| matches!(notice, Notice::Event(Event::Notification { .. })));
        assert!(!notified, "{seen:?}");
    }

    /// An agent blocked or done is a notification from the pane showing it, titled by what came
    /// with the note, else by the pane; a bell's or a program's note came raw, and a pane no
    /// longer shown hears nothing.
    #[test]
    fn blocked_and_done_notes_are_notifications() {
        let (session, _told, mut daemon) = session();
        let (notices, mut heard) = unbounded();
        session.stream(3, 4, 40, shows(notices)).unwrap();
        assert_eq!(daemon.asked()["type"], "pane_attach");
        received(&mut heard, |notice| matches!(notice, Notice::Event(Event::Title(_))));
        let note = |pane: u64, kind: &str, title: &str, body: &str| {
            format!(
                r#"{{"type":"noted","note":{{"id":1,"time":1,"pane":{pane},"pane_title":"claude",
                    "workspace":"w","kind":"{kind}","title":{title},"body":{body},"read":false}}}}"#
            )
        };
        daemon.say(&note(3, "bell", "null", "null"));
        daemon.say(&note(3, "notify", r#""build""#, r#""ok""#));
        daemon.say(&note(2, "done", "null", "null"));
        daemon.say(&note(3, "blocked", "null", r#""Bash · ls""#));
        daemon.say(&note(3, "done", r#""app""#, "null"));
        let notified = |notice: &Notice| match notice {
            Notice::Event(Event::Notification { title, body }) => {
                Some((title.clone()?, body.clone()?))
            }
            _ => None,
        };
        let seen = received(&mut heard, |notice| {
            notified(notice).is_some_and(|(title, _)| title == "app")
        });
        let said: Vec<_> = seen.iter().filter_map(notified).collect();
        assert_eq!(said, [("claude".into(), "Bash · ls".into()), ("app".into(), "done".into())]);
        session.forget(3);
        daemon.say(&note(3, "done", "null", "null"));
        daemon.watched(&[(3, "working", "")]);
        thread::sleep(Duration::from_millis(100));
        assert!(heard.try_recv().is_err(), "a pane forgotten hears nothing");
    }

    /// A stream's opening and its first snapshot: its history goes into the scrollback, after a
    /// line on what was left out, and the snapshot draws the screen without pushing the blank one
    /// into it, with what the program set but 2031, which the daemon keeps; raw output then goes
    /// on from there. tutti's title and directory name the pane.
    #[test]
    fn a_stream_opens_on_its_history_and_its_snapshot() {
        let (pane, mut notices, mut daemon) = attached();
        assert_eq!(pane.tutti_name(), Some(("sh".into(), Some("/w".into()))));
        daemon.say(r#"{"type":"pane_rows","pane":3,"epoch":1,"top":2,"skipped":5}"#);
        daemon.raw(3, b"one\r\n");
        daemon.raw(3, b"two\r\n");
        daemon.screen(3, b"\x1b[?2031h\x1b[>1u\x1b[>4;2m\x1b[?1004h$ ");
        daemon.raw(3, b"echo hi");
        wait_until(&pane.term, "showed the typing", |term| term.contents().contains("$ echo hi"));
        let term = pane.lock();
        let left_out = "[tutti pane 3: 5 lines not shown]";
        assert_eq!(scrollback(&term), [left_out, "one", "two"]);
        let modes = term.modes();
        assert!(modes.focus_reports && modes.modify_other_keys == 2 && term.kitty_keyboard() == 1);
        assert!(!modes.color_scheme_reports);
        drop(term);
        let seen = received(&mut notices, |notice| matches!(notice, Notice::Output));
        assert!(
            matches!(&seen[0], Notice::Event(Event::Title(title)) if title == "sh"),
            "{seen:?}"
        );
    }

    /// The engine's replies to the stream's raw reads go back to the pane as input, in order with
    /// what is typed; a snapshot's go nowhere. A cell-border query is left unanswered, as the
    /// daemon's own engine leaves it: the daemon's snapshots would carry no borders.
    #[test]
    fn replies_to_raw_reads_go_back_and_a_snapshots_do_not() {
        let (pane, _notices, mut daemon) = attached();
        daemon.rows(3);
        let asking = b"\x1b[H\x1b[2J\x1b[6n$ ".to_vec();
        daemon.snapshot(3, 4, 40, asking);
        daemon.raw(3, b"\x1bP1+bcell-border;q\x1b\\\x1b[c");
        let Some(Frame::Input { pane: PaneId(3), bytes }) = daemon.heard() else {
            panic!("no reply")
        };
        assert!(bytes.starts_with(b"\x1b[?") && bytes.ends_with(b"c"), "{bytes:?}");
        pane.write(b"ls\r".to_vec());
        let typed = Frame::Input { pane: PaneId(3), bytes: b"ls\r".to_vec() };
        assert_eq!(daemon.heard(), Some(typed));
    }

    /// A paste longer than the wire's longest frame, which tutti's encoder panics on, goes in
    /// several frames (`Frame::inputs`), whole and in order, and what is typed next after it. A
    /// frame ends wherever its length does, mid-character or mid-paste: the daemon writes them to
    /// the pane's pty one after another, so the program reads what one write would have brought.
    #[test]
    fn a_paste_longer_than_a_frame_goes_in_several_whole_and_in_order() {
        let (pane, _notices, mut daemon) = attached();
        let pasted: Vec<u8> = (0..MAX_FRAME_LEN + 3).map(|at| (at % 251) as u8).collect();
        pane.write(pasted.clone());
        pane.write(b"\r".to_vec());
        let typed = |daemon: &mut Daemon| match daemon.heard() {
            Some(Frame::Input { pane: PaneId(3), bytes }) => bytes,
            frame => panic!("{frame:?}"),
        };
        let (mut heard, mut frames) = (Vec::new(), 0);
        while heard.len() < pasted.len() {
            heard.extend(typed(&mut daemon));
            frames += 1;
        }
        assert!(frames > 1 && heard == pasted, "the paste in {frames} frames, whole and in order");
        assert_eq!(typed(&mut daemon), b"\r");
    }

    /// A resize moves the pane's vote, and the terminal takes the size of the snapshot that
    /// follows, the screen redrawn without a copy of it in the scrollback.
    #[test]
    fn a_resize_votes_and_the_snapshot_at_the_size_settled_resizes() {
        let (mut pane, _notices, mut daemon) = attached();
        let mut shell = Terminal::new(4, 40, 100);
        shell.advance(b"$ ls\r\nfile\r\n$ ");
        daemon.rows(3);
        daemon.snapshot(3, 4, 40, shell.snapshot());
        wait_until(&pane.term, "showed the screen", |term| term.contents().contains("file"));
        pane.resize(6, 30, (8, 16));
        let vote = json!({"type": "view_sizes", "panes": [{"pane": 3, "rows": 6, "cols": 30}]});
        assert_eq!(daemon.asked(), vote);
        assert_eq!(pane.lock().size(), (4, 40), "the daemon settles the size");
        shell.resize(6, 30);
        daemon.say(r#"{"type":"ok"}"#);
        daemon.rows(3);
        daemon.snapshot(3, 6, 30, shell.snapshot());
        wait_until(&pane.term, "resized", |term| term.size() == (6, 30));
        let term = pane.lock();
        assert_eq!(term.contents(), shell.contents());
        assert!(scrollback(&term).is_empty(), "{:?}", scrollback(&term));
    }

    /// A stream that fell behind skips to a fresh snapshot after the rows that scrolled off the
    /// screen meanwhile: they go into the scrollback over the stale screen, nothing missed and
    /// nothing twice, after a line on any it left out.
    #[test]
    fn a_resyncs_rows_go_into_the_scrollback_before_its_snapshot() {
        for (stale, printed, from, want) in [
            ("", 21, 0, names(0..18)),
            ("\x1b[?1049h\x1b[2;3rvim", 21, 0, names(0..18)),
            ("", 6, 0, names(0..3)),
            ("", 21, 7, {
                let mut want = vec!["[tutti pane 3: 7 lines not shown]".to_owned()];
                want.extend(names(7..18));
                want
            }),
        ] {
            let (pane, _notices, mut daemon) = attached();
            let mut shell = Terminal::new(4, 40, 100);
            shell.advance(lines(0..3).as_bytes());
            daemon.rows(3);
            daemon.snapshot(3, 4, 40, shell.snapshot());
            daemon.raw(3, stale.as_bytes());
            // The pane goes on to print what the stream skips.
            shell.advance(lines(3..printed).as_bytes());
            let top = shell.screen_top();
            let rows = format!(
                r#"{{"type":"pane_rows","pane":3,"epoch":1,"top":{top},"skipped":{from}}}"#
            );
            daemon.say(&rows);
            daemon.raw(3, &shell.history(from as u64, top));
            daemon.snapshot(3, 4, 40, shell.snapshot());
            let last = format!("L{}", printed - 1);
            wait_until(&pane.term, "resynced", |term| term.contents().contains(&last));
            let term = pane.lock();
            assert_eq!(scrollback(&term), want, "{stale:?}, {printed} printed");
            assert_eq!(term.contents(), shell.contents(), "{stale:?}");
            assert!(!term.modes().alt_screen);
        }
    }

    /// What a receiver holds now.
    fn drained<T>(receiver: &mut UnboundedReceiver<T>) -> Vec<T> {
        std::iter::from_fn(|| receiver.try_recv().ok()).collect()
    }

    fn exited(notice: &Notice) -> bool {
        matches!(notice, Notice::Exit(_))
    }

    /// The pane's exit ends its stream, told once, and a kill still reaches the daemon on the
    /// connection, which outlives it; the pane shown focused is the connection's to tell, as a
    /// view's own focus, seen while someone looks; and closing the pane detaches its stream, with
    /// no kill asked, and the session's last pane closes the connection.
    #[test]
    fn the_programs_exit_ends_the_stream_and_a_close_only_detaches() {
        let (mut pane, mut notices, mut daemon) = attached();
        daemon.rows(3);
        daemon.screen(3, b"$ exit");
        daemon.say(r#"{"type":"pane_exited","pane":3,"code":3}"#);
        daemon.say(r#"{"type":"pane_exited","pane":3,"code":3}"#);
        daemon.raw(3, b" trailing");
        wait_until(&pane.term, "took what trailed", |term| term.contents() == "$ exit trailing");
        let exits: Vec<Notice> = drained(&mut notices).into_iter().filter(exited).collect();
        assert!(matches!(&exits[..], [Notice::Exit(why)] if why == "Exited with code 3"));
        pane.kill();
        assert_eq!(daemon.asked(), json!({"type": "pane_kill", "pane": 3}));

        let (session, _told, mut daemon) = session();
        let (pane, _notices) = attach(&session, &mut daemon, 3);
        pane.set_scheme(&find(&load(&[]).unwrap(), "Solarized Light").unwrap());
        assert_eq!(daemon.asked()["dark"], false, "the daemon hears of the scheme");
        pane.focused(false);
        assert_eq!(daemon.asked(), json!({"type": "pane_focus", "pane": 3, "seen": false}));
        pane.focused(true);
        assert_eq!(daemon.asked(), json!({"type": "pane_focus", "pane": 3, "seen": true}));
        drop(pane);
        let detach = json!({"type": "pane_detach", "pane": 3, "req": 2});
        assert_eq!(daemon.asked(), detach, "detached, with no kill asked");
        assert_eq!(daemon.asked(), json!({"type": "view_sizes", "panes": []}));
        drop(session);
        assert_eq!(daemon.heard(), None, "closed");
    }

    /// A pane the session no longer has: the daemon refuses its stream, naming the request by its
    /// `req`, and the pane says so and stays, with no exit to close it; another refusal is the
    /// window's.
    #[test]
    fn a_pane_the_session_lost_says_so_and_stays() {
        let (session, mut told, mut daemon) = session();
        let (_two, _) = attach(&session, &mut daemon, 2);
        let (pane, mut notices) = attach(&session, &mut daemon, 3);
        daemon.say(r#"{"type":"ok"}"#);
        daemon.say(r#"{"type":"error","message":"no tab 9"}"#);
        daemon.say(r#"{"req":2,"type":"error","message":"no pane 3"}"#);
        let said = |term: &Terminal| term.contents() == "[tutti pane 3: no pane 3]";
        wait_until(&pane.term, "said why", said);
        assert_eq!(received(&mut told, |_| true), [Told::Refused("no tab 9".into())]);
        thread::sleep(Duration::from_millis(100));
        assert!(!drained(&mut notices).iter().any(exited));
    }

    /// One connection streams several panes, each frame to the pane it names: interleaved, a
    /// resync of one leaves the other be, one's exit ends it alone while the other goes on, and
    /// the connection's end ends every stream.
    #[test]
    fn several_streams_share_the_connection_each_frame_to_its_pane() {
        let (session, mut told, mut daemon) = session();
        let (two, mut two_heard) = attach(&session, &mut daemon, 2);
        let (three, mut three_heard) = attach(&session, &mut daemon, 3);
        daemon.rows(2);
        daemon.rows(3);
        daemon.raw(2, b"before two\r\n");
        daemon.screen(3, b"three$ ");
        daemon.screen(2, b"two$ ");
        daemon.raw(3, b"ls");
        daemon.raw(2, b"pwd");
        wait_until(&two.term, "typed", |term| term.contents() == "two$ pwd");
        wait_until(&three.term, "typed", |term| term.contents() == "three$ ls");
        assert_eq!(scrollback(&two.lock()), ["before two"]);
        assert!(scrollback(&three.lock()).is_empty());

        let mut shell = Terminal::new(4, 40, 100);
        shell.advance(lines(0..7).as_bytes());
        let top = shell.screen_top();
        daemon
            .say(&format!(r#"{{"type":"pane_rows","pane":3,"epoch":1,"top":{top},"skipped":0}}"#));
        daemon.raw(3, &shell.history(0, top));
        daemon.raw(2, b" cd");
        daemon.snapshot(3, 4, 40, shell.snapshot());
        wait_until(&three.term, "resynced", |term| term.contents() == shell.contents());
        assert_eq!(scrollback(&three.lock()), names(0..4));
        wait_until(&two.term, "went on", |term| term.contents() == "two$ pwd cd");
        assert_eq!(scrollback(&two.lock()), ["before two"]);
        // A flood of short reads, fed together up to another pane's frame, all in order.
        (0..3000).for_each(|_| daemon.raw(2, b"."));
        daemon.raw(3, b"!");
        daemon.raw(2, b"#");
        wait_until(&two.term, "flooded", |term| term.contents().ends_with(".#"));
        wait_until(&three.term, "went on", |term| term.contents().ends_with('!'));
        let dots = |term: &Terminal| {
            let rows = term.rows(term.oldest_row(), term.screen_top() + 4);
            rows.map(|(_, row)| row.text().matches('.').count()).sum::<usize>()
        };
        assert_eq!(dots(&two.lock()), 3000);

        daemon.say(r#"{"type":"pane_exited","pane":2,"code":0}"#);
        received(&mut two_heard, exited);
        daemon.raw(3, b"still here");
        wait_until(&three.term, "went on", |term| term.contents().ends_with("still here"));
        assert!(!drained(&mut three_heard).iter().any(exited));
        drop(daemon);
        let closed = |notice: &Notice| matches!(notice, Notice::Exit(why) if why == CLOSED);
        received(&mut three_heard, closed);
        received(&mut told, |said| *said == Told::Ended(CLOSED.into()));
    }

    /// A resize votes for every pane the view streams, as the view's whole set. A pane closed
    /// detaches its stream and leaves the set. Shown again before the detach is answered, its new
    /// stream is asked for only after the answer: what the daemon sends of the pane until then is
    /// the old stream's, and goes nowhere and answers nothing; all after is the new stream's, its
    /// history, snapshot and exit. A pane shown already is refused, and the session's word of an
    /// exit before the new stream began is no one's.
    #[test]
    fn a_closed_pane_shows_again_on_a_new_stream_asked_for_after_the_detach() {
        let (session, _told, mut daemon) = session();
        let (_two, _) = attach(&session, &mut daemon, 2);
        let (mut three, _heard) = attach(&session, &mut daemon, 3);
        three.resize(6, 30, (8, 16));
        let both = json!({"type": "view_sizes", "panes": [
            {"pane": 2, "rows": 4, "cols": 40}, {"pane": 3, "rows": 6, "cols": 30}
        ]});
        assert_eq!(daemon.asked(), both);
        daemon.rows(3);
        daemon.screen(3, b"$ old");
        wait_until(&three.term, "showed", |term| term.contents() == "$ old");
        drop(three);
        assert_eq!(daemon.asked(), json!({"type": "pane_detach", "pane": 3, "req": 3}));
        let two = json!({"type": "view_sizes", "panes": [{"pane": 2, "rows": 4, "cols": 40}]});
        assert_eq!(daemon.asked(), two, "the set without the pane closed");
        let show = || Pane::attach(session.clone(), 3, 4, 40, None, None, false);
        let (again, mut heard) = show().unwrap();
        let refused = show().err().expect("refused");
        assert_eq!(refused.to_string(), "it shows in this window already");
        session.ask(&Request::View);
        assert_eq!(daemon.asked(), json!({"type": "view"}), "no stream asked before the answer");
        daemon.raw(3, b"\x1b[c stale");
        daemon.say(r#"{"type":"pane_exited","pane":3,"code":9}"#);
        daemon.say(r#"{"req":3,"type":"ok"}"#);
        assert_eq!(attached_to(&mut daemon, 3), 4, "asked after the answer, no reply before it");
        daemon.say(r#"{"type":"pane_rows","pane":3,"epoch":1,"top":1,"skipped":0}"#);
        daemon.raw(3, b"before\r\n");
        daemon.screen(3, b"$ new");
        daemon.raw(3, b" one");
        wait_until(&again.term, "streamed anew", |term| term.contents() == "$ new one");
        assert_eq!(scrollback(&again.lock()), ["before"]);
        assert!(!drained(&mut heard).iter().any(exited), "the old stream's exit is no one's");
        daemon.say(r#"{"type":"pane_exited","pane":3,"code":0}"#);
        let exit = |notice: &Notice| matches!(notice, Notice::Exit(why) if why.ends_with("code 0"));
        received(&mut heard, exit);
    }

    /// A refusal answers only the request it names: an earlier stream's refusal, come late, is no
    /// detach's answer, and the new stream waits for the detach's own, a refusal too here (the
    /// connection streamed no such pane), then hears its own refusal.
    #[test]
    fn an_attachs_refusal_is_no_detachs_answer() {
        let (session, _told, mut daemon) = session();
        let (three, _) = attach(&session, &mut daemon, 3);
        drop(three);
        assert_eq!(daemon.asked(), json!({"type": "pane_detach", "pane": 3, "req": 2}));
        assert_eq!(daemon.asked()["type"], "view_sizes");
        let (again, _) = Pane::attach(session.clone(), 3, 4, 40, None, None, false).unwrap();
        daemon.say(r#"{"req":1,"type":"error","message":"no pane 3"}"#);
        daemon.raw(3, b"stale");
        session.ask(&Request::View);
        assert_eq!(daemon.asked(), json!({"type": "view"}), "the detach still waits");
        daemon.say(r#"{"req":2,"type":"error","message":"this connection streams no pane 3"}"#);
        let req = attached_to(&mut daemon, 3);
        daemon.say(&format!(r#"{{"req":{req},"type":"error","message":"no pane 3"}}"#));
        let said = |term: &Terminal| term.contents() == "[tutti pane 3: no pane 3]";
        wait_until(&again.term, "said why", said);
    }

    /// A stream hands on the keyboard protocols the program sets, so the keys rtty encodes for the
    /// pane follow them; a command's end turns them off, as the daemon tells a keys stream to.
    #[test]
    fn the_keyboard_protocols_come_through_and_a_commands_end_ends_them() {
        let (pane, _notices, mut daemon) = attached();
        daemon.rows(3);
        daemon.screen(3, b"$ nvim");
        daemon.raw(3, b"\x1b[>4;2m\x1b[>4;1f\x1b[>1u\x1b[=5;1u");
        wait_until(&pane.term, "pushed", |term| term.kitty_keyboard() == 5);
        let escape = |term: &Terminal| {
            let key = input::Key { code: input::Code::Escape, ..input::Key::default() };
            input::encode_key(&key, term.modes(), term.kitty_keyboard()).unwrap()
        };
        let term = pane.lock();
        assert_eq!((term.modes().modify_other_keys, term.modes().format_other_keys), (2, 1));
        assert_eq!(escape(&term), b"\x1b[27u");
        drop(term);
        daemon.raw(3, b"\x1b]133;D;137\x07\x1b[>4;0m\x1b[>4;0f\x1b[<65535u$ ");
        wait_until(&pane.term, "reset", |term| term.contents().ends_with('$'));
        let term = pane.lock();
        assert_eq!((term.kitty_keyboard(), term.modes().modify_other_keys), (0, 0));
        assert_eq!(escape(&term), b"\x1b");
    }

    /// An opening refused: a daemon older than rtty's floor, or one whose floor rtty is older than
    /// (though the rest of its answer no longer reads here), another proxy's host frames, the
    /// daemon's own refusal, a pane frame where a quiet attach is sent none, or the daemon gone.
    /// Each says why.
    #[test]
    fn an_opening_says_why_it_was_refused() {
        let refused = |play: &dyn Fn(&mut Daemon)| {
            let (open, mut daemon) = opening();
            play(&mut daemon);
            drop(daemon);
            open.join().unwrap().err().expect("refused").to_string()
        };
        let (below, rtty) = (WIRE_FLOOR - 1, format!("rtty {WIRE_REV} (floor {WIRE_FLOOR})"));
        let old = refused(&|daemon| daemon.say(&attached_json(below, None)));
        let said = format!("wire revision {below} (floor {below}), {rtty}: update tutti");
        assert!(old.contains(&said), "{old}");
        let above = WIRE_REV + 1;
        let newer = refused(&|daemon| daemon.say(&attached_json(above + 2, Some(above))));
        let said = format!("wire revision {} (floor {above}), {rtty}: update rtty", above + 2);
        assert!(newer.ends_with(&said), "{newer}");
        let unread = format!(
            r#"{{"type":"attached","wire_rev":{above},"wire_floor":{above},"workspaces":7}}"#
        );
        assert!(refused(&|daemon| daemon.say(&unread)).ends_with("update rtty"));
        let loud = refused(&|daemon| daemon.screen(2, b"$ "));
        assert_eq!(loud, "tutti sent a pane frame to a quiet attach");
        let none =
            refused(&|daemon| daemon.say(r#"{"type":"attached","session":"s","workspaces":[]}"#));
        assert!(none.contains("wire revision 0"), "{none}");
        let proxy =
            refused(&|daemon| daemon.send(Frame::Host(br#"{"type":"hello","rev":1}"#.into())));
        assert!(proxy.contains("host frames revision 1, rtty 2"), "{proxy}");
        let error =
            refused(&|daemon| daemon.say(r#"{"type":"error","message":"bad request: attach"}"#));
        assert_eq!(error, "bad request: attach");
        assert!(refused(&|_| {}).contains("closed the connection"));

        // A proxy that speaks this revision says hello first, and the opening goes on.
        let (open, mut daemon) = opening();
        daemon.send(Frame::Host(br#"{"type":"hello","rev":2,"startup":{}}"#.into()));
        daemon.say(&answer());
        daemon.watched(&[]);
        assert!(open.join().unwrap().is_ok());
    }

    /// A daemon at another revision opens while each end is at or above the other's floor: one
    /// from before floors at rtty's floor, whose floor is its revision, and a newer one whose
    /// floor rtty's revision meets.
    #[test]
    fn a_session_opens_on_a_daemon_older_or_newer_within_the_floors() {
        for (rev, floor) in [(WIRE_FLOOR, None), (WIRE_REV + 3, Some(WIRE_REV))] {
            let (open, mut daemon) = opening();
            daemon.say(&attached_json(rev, floor));
            daemon.watched(&[]);
            let (_, tabs, _) = open.join().unwrap().unwrap();
            assert_eq!(ids(&tabs), [(5, vec![1, 4])], "{rev} with floor {floor:?}");
        }
    }

    /// A frame that is none, and a delta, which goes to clients that are not quiet, end the
    /// connection, which says so.
    #[test]
    fn what_is_no_frame_of_a_quiet_attachs_ends_the_connection() {
        let ended = |sent: &[u8]| {
            let (_session, mut told, mut daemon) = session();
            daemon.stream.write_all(sent).unwrap();
            match received(&mut told, |said| matches!(said, Told::Ended(_))).pop() {
                Some(Told::Ended(why)) => why,
                said => panic!("{said:?}"),
            }
        };
        let unknown = [&5u32.to_le_bytes()[..], &[7], &[0; 4]].concat();
        let why = ended(&unknown);
        assert_eq!(
            why,
            "tutti's connection broke: tutti sent what is no frame: unknown frame kind 0x7"
        );
        let (pane, rows, cols) = (PaneId(3), 4, 40);
        let delta = Frame::PaneDelta(PaneData { pane, rows, cols, seq: 1, bytes: Vec::new() });
        assert_eq!(ended(&delta.encode()), "tutti sent a quiet attach a delta");
    }

    /// A daemon that answers a byte at a time and never a whole frame has 3 s in all, not 3 s a
    /// byte.
    #[test]
    fn an_opening_waits_no_longer_than_its_deadline_in_all() {
        let started = Instant::now();
        let (open, mut daemon) = opening();
        let trickle = thread::spawn(move || {
            daemon.stream.write_all(&1000u32.to_le_bytes())?;
            for _ in 0..100 {
                thread::sleep(Duration::from_millis(100));
                daemon.stream.write_all(&[1])?;
            }
            Ok::<_, io::Error>(())
        });
        let refused = open.join().unwrap().err().expect("refused").to_string();
        let waited = started.elapsed();
        assert_eq!(refused, "tutti's daemon did not answer within 3 s");
        assert!(waited >= OPENING && waited < OPENING * 2, "waited {waited:?}");
        assert!(trickle.join().unwrap().is_err(), "rtty hung up while the daemon trickled");
    }

    /// What a daemon said before it hung up is read whole, within the deadline, then the end: a
    /// refusal and its reason, which a socket timeout set after the hang-up (macOS refuses it)
    /// would lose.
    #[test]
    fn what_was_said_before_a_hang_up_is_read() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        theirs.write_all(&control(None, &Request::Watch).encode()).unwrap();
        drop(theirs);
        let (mut buf, deadline) = (VecDeque::new(), Instant::now() + OPENING);
        let said = next(&mut Until(&ours, deadline), &mut buf).unwrap();
        assert!(matches!(said, Some(Frame::Control(_))), "{said:?}");
        assert!(next(&mut Until(&ours, deadline), &mut buf).unwrap().is_none());
    }

    /// No daemon at the socket says so.
    #[test]
    fn no_daemon_says_where_it_looked() {
        let socket =
            std::env::temp_dir().join(format!("rtty-no-tutti-{}.sock", std::process::id()));
        let err = connect(&socket).unwrap_err().to_string();
        assert_eq!(err, format!("no tutti daemon listens on {}", socket.display()));
    }
}
