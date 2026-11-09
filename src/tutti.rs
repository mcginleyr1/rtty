//! A tutti session's daemon as rtty shows it: the daemon keeps the ptys, the session and the
//! agents' states, and rtty draws the session's panes as rtty panes (`Pane::attach`). Here is the
//! daemon's wire as rtty speaks it, written from tutti's `docs/transport-decision.md` and its
//! protocol, and the window's connection to a session, whose pane streams' reader is ported from
//! tutti's `pane attach --inline`.
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
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::layout::Direction;
use crate::pane::{Notice, SCROLLBACK, Trace};
use crate::scheme::Scheme;

/// The revision of the daemon's requests, responses and events this rtty speaks, which the
/// daemon's answer to an attach names (tutti's `WIRE_REV`).
pub const WIRE_REV: u32 = 19;

/// The revision of the host frames a `tutti proxy` speaks, which its hello names (`HOST_REV`).
pub const HOST_REV: u32 = 2;

/// What rtty attaches as: its bundle's identifier, which tutti tells its watchers so that one (a
/// notch app) can bring rtty forward.
const TERMINAL: &str = "local.rtty";

/// The longest frame either side sends.
const MAX_FRAME: usize = 16 << 20;

/// One frame: `[u32 len][u8 kind][payload]`, little-endian, `len` counting the kind byte and the
/// payload. A pane frame's payload starts with the pane's id, a `u64`. Kind 3, what changed of a
/// pane's screen, goes to attached clients that are not quiet: never to rtty, which reads one as
/// an error.
#[derive(Debug, PartialEq)]
pub enum Frame {
    /// One JSON object: a request, a response or an event, tagged by its `type` (kind 1).
    Control(Vec<u8>),
    /// The pane's screen as it stands, at its size: rtty-vt's `snapshot()` (kind 2, after the
    /// rows, the columns and a `u32` sequence number).
    Snapshot { pane: u64, rows: u16, cols: u16, bytes: Vec<u8> },
    /// Typed into the pane (kind 4).
    Input { pane: u64, bytes: Vec<u8> },
    /// One pty read of the pane, but for the terminal queries the daemon answered (kind 5).
    Raw { pane: u64, bytes: Vec<u8> },
    /// Between a client and the `tutti proxy` it runs, never the daemon's: the proxy's hello
    /// first (kind 6).
    Host(Vec<u8>),
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let (kind, head, bytes): (u8, Vec<u8>, &[u8]) = match self {
            Frame::Control(json) => (1, Vec::new(), json),
            Frame::Snapshot { pane, rows, cols, bytes } => {
                let head = [&pane.to_le_bytes()[..], &rows.to_le_bytes(), &cols.to_le_bytes()];
                (2, [&head.concat()[..], &0u32.to_le_bytes()].concat(), bytes)
            }
            Frame::Input { pane, bytes } => (4, pane.to_le_bytes().to_vec(), bytes),
            Frame::Raw { pane, bytes } => (5, pane.to_le_bytes().to_vec(), bytes),
            Frame::Host(json) => (6, Vec::new(), json),
        };
        let len = u32::try_from(1 + head.len() + bytes.len()).expect("a frame under 4 GiB");
        [&len.to_le_bytes()[..], &[kind], &head, bytes].concat()
    }

    /// The frame at the front of `buf` and how many bytes it took; `None` until all of it is there.
    pub fn decode(buf: &[u8]) -> io::Result<Option<(Frame, usize)>> {
        let Some(&len) = buf.first_chunk::<4>() else { return Ok(None) };
        let len = u32::from_le_bytes(len) as usize;
        if len == 0 || len > MAX_FRAME {
            return Err(io::Error::other(format!("tutti sent a frame {len} bytes long")));
        }
        let Some(body) = buf.get(4..4 + len) else { return Ok(None) };
        let (kind, payload) = (body[0], &body[1..]);
        let short = || io::Error::other(format!("tutti sent a frame of kind {kind} cut short"));
        let pane = payload.first_chunk::<8>().map(|&id| u64::from_le_bytes(id));
        let frame = match kind {
            1 => Frame::Control(payload.to_vec()),
            2 => {
                let size = payload.get(8..12).ok_or_else(short)?;
                let (rows, cols) = (
                    u16::from_le_bytes([size[0], size[1]]),
                    u16::from_le_bytes([size[2], size[3]]),
                );
                let bytes = payload.get(16..).ok_or_else(short)?.to_vec();
                Frame::Snapshot { pane: pane.ok_or_else(short)?, rows, cols, bytes }
            }
            4 => Frame::Input { pane: pane.ok_or_else(short)?, bytes: payload[8..].to_vec() },
            5 => Frame::Raw { pane: pane.ok_or_else(short)?, bytes: payload[8..].to_vec() },
            6 => Frame::Host(payload.to_vec()),
            _ => return Err(io::Error::other(format!("tutti sent a frame of kind {kind}"))),
        };
        Ok(Some((frame, 4 + len)))
    }
}

/// The next whole frame from `input`, read on into `buf` until it is there; `None` once the other
/// side closed the connection. Taking a frame off the front of `buf` moves none of the rest, and
/// nothing is read while a whole frame waits: a frame of a pane's few bytes costs no more than
/// those.
fn next(input: &mut impl Read, buf: &mut VecDeque<u8>) -> io::Result<Option<Frame>> {
    loop {
        if let Some((frame, used)) = Frame::decode(buf.make_contiguous())? {
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

/// What rtty asks of the daemon.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Join the session's attached clients: as a `view`, whose focus is its own (it moves no pane
    /// of the session's, and marks one seen only when it says someone looks) and on which no
    /// permission request waits, that hands moves past tutti's edges on (`handoff`) to the
    /// terminal `terminal` names, and, `quiet`, is sent no pane frames but its streams'.
    Attach { terminal: Option<String>, handoff: bool, view: bool, quiet: bool },
    /// Subscribe to the session's snapshots (`Message::Snapshot`): one at once, then one on every
    /// change. Read-only: it moves no size, focus or seen state.
    Watch,
    /// The session's tabs as they are now (`Message::View`).
    View,
    /// The colours of the terminal the panes are shown on, which the daemon answers their colour
    /// queries with, and whose light or dark it tells the programs that asked (mode 2031).
    HostColors(HostColors),
    /// Stream `pane` to this connection, beside any other it streams, each frame naming its pane:
    /// `history` rows of its scrollback, a snapshot, then every pty read raw, and its exit.
    /// `rows`×`cols` is this connection's vote in the pane's size, until the stream is detached;
    /// `inline` numbers each snapshot's rows (`Message::PaneRows`) and hands on, before a resync's
    /// snapshot, the rows that scrolled off while the stream was behind; `keys` keeps the keyboard
    /// protocols the program sets in the reads. The daemon streams a pane once a connection until
    /// then, and echoes `req`, the request's own number, on its refusal.
    PaneAttach { pane: u64, rows: u16, cols: u16, history: u32, inline: bool, keys: bool, req: u64 },
    /// Ends this connection's stream of `pane`, its vote with it: nothing of it follows the answer,
    /// which echoes `req`, and the pane may be streamed again.
    PaneDetach { pane: u64, req: u64 },
    /// Moves this connection's vote in each pane's size.
    ViewSizes { panes: Vec<Dims> },
    /// `pane` has the keyboard: from a view, only what the view shows, which tutti hands moves at
    /// its edges back by, and, `seen`, what someone looks at: a pane whose agent is done turns
    /// idle and its notes are read, though the session's active pane stays.
    PaneFocus { pane: u64, seen: bool },
    /// Ends `pane`'s program.
    PaneKill { pane: u64 },
    /// Splits `pane`'s cell, a login shell in the new half, right of it (`Horizontal`) or below.
    PaneSplit { pane: u64, direction: Axis },
    /// Moves the border of the nearest split around `from` on `toward`'s axis that way, by `delta`
    /// of the split; with no such split, `Message::ResizeEdge` hands it on.
    PaneResizeToward { from: Option<u64>, toward: Direction, delta: f32 },
    /// Sets the ratio of the split between `a`, in its first half, and `b`, in its second.
    PaneSplitRatio { a: u64, b: u64, ratio: f32 },
    /// A new tab in `workspace`, which becomes the session's current tab.
    TabNew { workspace: Option<u64> },
    /// Makes tab `id` the session's current tab.
    TabSelect { id: u64 },
    /// Runs `cmd` in a new pane of `tab`, else of the current tab.
    PaneRun { tab: Option<u64>, cmd: Vec<String> },
}

/// One pane's vote in its size (`Request::ViewSizes`).
#[derive(Debug, Serialize)]
pub struct Dims {
    pub pane: u64,
    pub rows: u16,
    pub cols: u16,
}

/// A terminal's colours, 16 bits a channel as xterm reports them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct HostColors {
    pub fg: Rgb16,
    pub bg: Rgb16,
    pub cursor: Option<Rgb16>,
    pub dark: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Rgb16 {
    pub r: u16,
    pub g: u16,
    pub b: u16,
}

/// A request as the control frame that carries it.
pub fn control(request: &Request) -> Frame {
    Frame::Control(serde_json::to_vec(request).expect("a request serializes"))
}

/// What the daemon says that rtty acts on: a response or an event, tagged as tutti's are; anything
/// else is `Other`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Message {
    /// An answer, with the `req` of the request answered, if it had one.
    Ok {
        #[serde(default)]
        req: Option<u64>,
    },
    /// A refusal, with the `req` of the request refused, if it had one.
    Error {
        message: String,
        #[serde(default)]
        req: Option<u64>,
    },
    /// Before each snapshot of a stream: the scrollback rows up to its screen come between this
    /// and it, as logical lines, after `skipped` rows the stream leaves out.
    PaneRows { pane: u64, skipped: u64 },
    /// The pane's program exited: the last of its stream, after its last output, or from the
    /// session, of a pane the connection does not stream.
    PaneExited { pane: u64, code: i32 },
    /// The whole session as a watcher sees it.
    Snapshot { panes: Vec<Watched> },
    /// The answer to an attach: the session's workspaces, and the wire the daemon speaks (0 from
    /// one older than the field).
    Attached {
        #[serde(default)]
        wire_rev: u32,
        workspaces: Vec<Workspace>,
    },
    /// The session's tabs or panes changed: all of them, as they are now.
    LayoutChanged { workspaces: Vec<Workspace> },
    /// The answer to `View`: the session's workspaces.
    View { workspaces: Vec<Workspace> },
    /// The answer to a split or a run: the pane it made.
    PaneCreated { id: u64 },
    /// Someone other than an attached client focused `pane`: a script, a notch app's click.
    PaneFocused { pane: u64 },
    /// A focus move from `pane`, which this view shows focused, found no pane `toward` in tutti's
    /// tab.
    FocusEdge { pane: u64, toward: Direction },
    /// A resize from `pane` found no split of tutti's on `toward`'s axis.
    ResizeEdge { pane: u64, toward: Direction },
    /// A note landed in the session's notification history.
    Noted { note: Note },
    #[serde(other)]
    Other,
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

/// One pane of a watched session's snapshot, as much of it as rtty reads.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Watched {
    pub id: u64,
    /// What tutti calls the pane: the command it was run with, or the name it was given.
    pub title: String,
    /// Its agent's state: `unknown`, `working`, `blocked`, `done` or `idle`.
    pub state: String,
    /// The directory the daemon last saw the pane's program in.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// A permission request its agent holds for an answer.
    #[serde(default)]
    pub approval: Option<IgnoredAny>,
    /// The program's OSC 1337 user vars.
    #[serde(default)]
    pub user_vars: BTreeMap<String, String>,
}

/// A workspace of the session, as much of it as rtty reads: its tabs.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Workspace {
    pub id: u64,
    pub tabs: Vec<Tab>,
}

/// A tab: whether it is the session's current one, its split tree (`None` until its first pane
/// exists), and the pane tutti last focused in it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Tab {
    pub id: u64,
    pub active: bool,
    pub layout: Option<Tree>,
    pub active_pane: Option<u64>,
}

/// A tab's split tree, as tutti writes its `Layout`: the first child left of the second or above
/// it, `ratio` the first's share.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub enum Tree {
    Leaf(u64),
    Split { direction: Axis, ratio: f32, first: Box<Tree>, second: Box<Tree> },
}

/// A split's axis: side by side (`Horizontal`), or one above the other.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// A note of the session's notification history, as much of it as rtty reads: the pane, what it
/// was called then, what kind of note (`bell`, `notify`, `blocked`, `done`), and what came with it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Note {
    pub pane: u64,
    pub pane_title: String,
    pub kind: String,
    pub title: Option<String>,
    pub body: Option<String>,
}

/// Where the daemon of tutti session `session` listens: `$XDG_RUNTIME_DIR/tutti/<session>.sock`,
/// else `/tmp/tutti-<uid>/<session>.sock`, as tutti finds it.
pub fn socket_path(session: &str) -> PathBuf {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("tutti"),
        // SAFETY: getuid has no preconditions and cannot fail.
        _ => PathBuf::from(format!("/tmp/tutti-{}", unsafe { libc::getuid() })),
    };
    dir.join(format!("{session}.sock"))
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
    pub watched: Arc<Mutex<Watched>>,
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
    /// hands moves on, and watching. The daemon's answer must speak `WIRE_REV`, and gives the
    /// session's tabs; its first snapshot names the panes. The daemon has `OPENING` to answer,
    /// however it trickles, so this runs off the main thread. The session, its tabs, and what it
    /// tells the window from then on.
    pub fn open(
        mut stream: UnixStream,
        socket: PathBuf,
    ) -> io::Result<(Session, Vec<Workspace>, UnboundedReceiver<Told>)> {
        let deadline = Instant::now() + OPENING;
        let terminal = Some(TERMINAL.into());
        let attach = Request::Attach { terminal, handoff: true, view: true, quiet: true };
        stream
            .write_all(&[control(&attach).encode(), control(&Request::Watch).encode()].concat())?;
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
            match serde_json::from_slice(&json) {
                Ok(Message::Attached { wire_rev, .. }) if wire_rev != WIRE_REV => {
                    let update = match wire_rev < WIRE_REV {
                        true => "update tutti and restart its daemon (`tutti server stop`)",
                        false => "update rtty",
                    };
                    return Err(io::Error::other(format!(
                        "tutti's daemon speaks wire revision {wire_rev}, rtty {WIRE_REV}: {update}"
                    )));
                }
                Ok(Message::Attached { workspaces, .. }) => {
                    tabs = Some(workspaces);
                    if early {
                        stream.write_all(&control(&Request::View).encode())?;
                        asked = true;
                    }
                }
                Ok(Message::View { workspaces }) if asked => {
                    (tabs, asked) = (Some(workspaces), false)
                }
                // One after the answer is newer than it.
                Ok(Message::LayoutChanged { workspaces }) => match tabs {
                    None => early = true,
                    Some(_) => tabs = Some(workspaces),
                },
                Ok(Message::Snapshot { panes }) => first = Some(panes),
                Ok(Message::Error { message, .. }) => return Err(io::Error::other(message)),
                _ => {}
            }
        }
        let shown =
            Arc::new(Mutex::new(Shown { latest: first.unwrap_or_default(), ..Shown::default() }));
        let (frames, outgoing) = mpsc::channel::<Frame>();
        let mut writer = stream.try_clone()?;
        thread::Builder::new().name("tutti session writer".into()).spawn(move || {
            for frame in outgoing {
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
        self.send(control(request));
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
        let now = shown.latest.iter().find(|watched| watched.id == pane).cloned();
        let now = now.unwrap_or_else(|| Watched { id: pane, ..Watched::default() });
        tell_of(&shows.notices, &Watched::default(), &now);
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
            self.ask(&Request::PaneDetach { pane, req });
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
            Ok(Some(Frame::Raw { pane, mut bytes })) => {
                while let Ok(Some((Frame::Raw { pane: same, bytes: more }, used))) =
                    Frame::decode(buf.make_contiguous())
                    && same == pane
                {
                    bytes.extend(more);
                    buf.drain(..used);
                }
                shown().raw(pane, &bytes, replies);
                continue;
            }
            Ok(Some(Frame::Snapshot { pane, rows, cols, bytes })) => {
                shown().screen(pane, rows, cols, &bytes);
                continue;
            }
            Ok(Some(Frame::Input { .. } | Frame::Host(_))) => continue,
            Ok(None) => break CLOSED.to_owned(),
            Err(err) => break format!("tutti's connection broke: {err}"),
        };
        let told = match serde_json::from_slice(&json) {
            Ok(Message::PaneRows { pane, skipped }) => {
                if let Some(stream) = shown().streamed(pane) {
                    stream.rows = Some(left_out(pane, skipped));
                }
                continue;
            }
            // One before the stream's first snapshot is the session's, of an exit before the
            // stream began, which tells it itself after that snapshot.
            Ok(Message::PaneExited { pane, code }) => {
                let mut shown = shown();
                if shown.streams.get(&pane).is_some_and(|stream| stream.started) {
                    shown.end(pane, &format!("Exited with code {code}"));
                }
                continue;
            }
            Ok(Message::Ok { req: Some(req) }) => {
                shown().answered(req, None, replies);
                continue;
            }
            Ok(Message::Error { message, req: Some(req) }) => {
                shown().answered(req, Some(&message), replies);
                continue;
            }
            Ok(Message::Snapshot { panes }) => {
                shown().snapshot(panes);
                continue;
            }
            Ok(Message::Noted { note }) => {
                shown().noted(note);
                continue;
            }
            Ok(Message::LayoutChanged { workspaces }) => Told::Layout(workspaces),
            Ok(Message::PaneFocused { pane }) => Told::Focused(pane),
            Ok(Message::FocusEdge { pane, toward }) => Told::FocusEdge(pane, toward),
            Ok(Message::ResizeEdge { pane, toward }) => Told::ResizeEdge(pane, toward),
            Ok(Message::PaneCreated { id }) => Told::Created(id),
            Ok(Message::Error { message, .. }) => Told::Refused(message),
            Ok(_) => continue,
            Err(err) => {
                crate::note(&format!("tutti said what rtty cannot read: {err}"));
                continue;
            }
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
        let attach =
            Request::PaneAttach { pane, rows, cols, history, inline: true, keys: true, req };
        let _ = out.send(control(&attach));
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
            Dims { pane, rows, cols }
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
            let Some(now) = panes.iter().find(|pane| pane.id == *id) else { continue };
            let mut watched = stream.shows.watched.lock().expect("a pane panicked");
            let was = mem::replace(&mut *watched, now.clone());
            tell_of(&stream.shows.notices, &was, now);
        }
        self.latest = panes;
    }

    /// A note: an agent blocked or done is a notification, from the pane that shows it. A bell's
    /// and a program's own notification came raw in its stream.
    fn noted(&self, note: Note) {
        let said = match note.kind.as_str() {
            "blocked" | "done" => note.kind.clone(),
            _ => return,
        };
        let Some(stream) = self.streams.get(&note.pane) else { return };
        let (title, body) = (note.title.unwrap_or(note.pane_title), note.body.unwrap_or(said));
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
                    let _ = replies.send(Frame::Input { pane, bytes });
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

/// The session's snapshot said `was` of a pane and now says `now`, which its notices hear. A new
/// title or directory renames it; its agent working shows on the tab's pill, going on, or paused
/// while blocked or holding a permission request; and the program's claim on the keys
/// (`TUTTI_KEYS`, which tutti's editor plugins set) is the window's to honour as `RTTY_KEYS` is.
/// The pane's own OSC 9, 777, 9;4 and 7501 come raw, and its agent's turn ending as a note: none of
/// them is told again here.
fn tell_of(notices: &UnboundedSender<Notice>, was: &Watched, now: &Watched) {
    let tell = |event| {
        let _ = notices.unbounded_send(Notice::Event(event));
    };
    if (&was.title, &was.cwd) != (&now.title, &now.cwd) {
        tell(Event::Title(now.title.clone()));
    }
    let pill = |watched: &Watched| match watched.state.as_str() {
        _ if watched.approval.is_some() => 4,
        "blocked" => 4,
        "working" => 3,
        _ => 0,
    };
    if pill(was) != pill(now) {
        tell(Event::Progress { state: pill(now), percent: None });
    }
    let keys = |watched: &Watched| watched.user_vars.get("TUTTI_KEYS").cloned();
    if keys(was) != keys(now) {
        let value = keys(now).unwrap_or_default();
        tell(Event::UserVar { name: "TUTTI_KEYS".into(), value });
    }
}

/// A scheme's colours as tutti takes a terminal's.
pub fn colors(scheme: &Scheme) -> HostColors {
    let wide = |Rgb(r, g, b): Rgb| Rgb16 {
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

    #[test]
    fn frames_round_trip_and_wait_for_their_last_byte() {
        let frames = [
            Frame::Control(br#"{"type":"watch"}"#.to_vec()),
            Frame::Snapshot { pane: 7, rows: 24, cols: 80, bytes: b"\x1b[H\x1b[2Jhi".to_vec() },
            Frame::Input { pane: 3, bytes: b"ls\r".to_vec() },
            Frame::Raw { pane: u64::MAX, bytes: Vec::new() },
            Frame::Host(br#"{"type":"hello","rev":2}"#.to_vec()),
        ];
        for frame in frames {
            let encoded = frame.encode();
            for cut in 0..encoded.len() {
                assert!(
                    Frame::decode(&encoded[..cut]).unwrap().is_none(),
                    "{frame:?} cut at {cut}"
                );
            }
            let twice = [encoded.clone(), encoded.clone()].concat();
            let (decoded, used) = Frame::decode(&twice).unwrap().unwrap();
            assert_eq!((decoded, used), (frame, encoded.len()));
        }
    }

    /// The bytes tutti's own encoder writes for a snapshot: the pane, the rows, the columns, a
    /// sequence number, then the screen. A delta, written the same way, never goes to a quiet
    /// attach, and reads as an error.
    #[test]
    fn a_snapshot_reads_as_tutti_writes_it() {
        let written = |kind: u8| {
            let mut wire = 1u32.to_le_bytes().to_vec();
            wire.push(kind);
            wire.extend(9u64.to_le_bytes());
            wire.extend(30u16.to_le_bytes());
            wire.extend(100u16.to_le_bytes());
            wire.extend(5u32.to_le_bytes());
            wire.extend(b"screen");
            let len = wire.len() as u32 - 4;
            wire[..4].copy_from_slice(&len.to_le_bytes());
            Frame::decode(&wire)
        };
        let snapshot = Frame::Snapshot { pane: 9, rows: 30, cols: 100, bytes: b"screen".to_vec() };
        assert_eq!(written(2).unwrap().unwrap(), (snapshot, 27));
        assert!(written(3).unwrap_err().to_string().contains("kind 3"));
    }

    #[test]
    fn a_frame_rtty_cannot_read_is_an_error() {
        let unknown = [&5u32.to_le_bytes()[..], &[7], &[0; 4]].concat();
        assert!(Frame::decode(&unknown).unwrap_err().to_string().contains("kind 7"));
        let short = [&5u32.to_le_bytes()[..], &[5], &[0; 4]].concat();
        assert!(Frame::decode(&short).unwrap_err().to_string().contains("cut short"));
        assert!(Frame::decode(&0u32.to_le_bytes()).is_err());
        assert!(Frame::decode(&u32::MAX.to_le_bytes()).is_err());
    }

    /// A session as tutti's daemon writes its answer to an attach, at wire revision `rev`: one
    /// workspace, 5, its tab 1 the session's current one, panes 2 and 3 side by side, 3 focused
    /// there, and its tab 4 with no pane yet.
    fn attached_json(rev: u32) -> String {
        format!(
            r#"{{"type":"attached","session":"s","wire_rev":{rev},"restorable":null,"notes":[],
              "workspaces":[{{"id":5,"name":"w","dir":"/w","branch":null,"changes":null,
                "changed_files":[],"stale":false,"parent":null,"tabs":[
                {{"id":1,"name":"one","active":true,"active_pane":3,"panes":[],
                  "layout":{{"Split":{{"direction":"horizontal","ratio":0.25,
                    "first":{{"Leaf":2}},"second":{{"Leaf":3}}}}}}}},
                {{"id":4,"name":"two","active":false,"active_pane":null,"panes":[],
                  "layout":null}}]}}]}}"#
        )
    }

    /// The tabs `attached_json` writes.
    fn attached_tabs() -> Vec<Workspace> {
        let split = Tree::Split {
            direction: Axis::Horizontal,
            ratio: 0.25,
            first: Box::new(Tree::Leaf(2)),
            second: Box::new(Tree::Leaf(3)),
        };
        let one = Tab { id: 1, active: true, layout: Some(split), active_pane: Some(3) };
        let two = Tab { id: 4, active: false, layout: None, active_pane: None };
        vec![Workspace { id: 5, tabs: vec![one, two] }]
    }

    /// Requests as tutti's daemon reads them (its `protocol.rs`), responses and events as it
    /// writes them, unknown fields and types passed over.
    #[test]
    fn requests_and_messages_read_as_tuttis() {
        let json = |request| match control(&request) {
            Frame::Control(json) => String::from_utf8(json).unwrap(),
            frame => panic!("{frame:?}"),
        };
        assert_eq!(json(Request::Watch), r#"{"type":"watch"}"#);
        assert_eq!(json(Request::View), r#"{"type":"view"}"#);
        assert_eq!(
            json(Request::Attach {
                terminal: Some("local.rtty".into()),
                handoff: true,
                view: true,
                quiet: true
            }),
            r#"{"type":"attach","terminal":"local.rtty","handoff":true,"view":true,"quiet":true}"#
        );
        assert_eq!(
            json(Request::PaneAttach {
                pane: 3,
                rows: 24,
                cols: 80,
                history: 10_000,
                inline: true,
                keys: true,
                req: 3
            }),
            r#"{"type":"pane_attach","pane":3,"rows":24,"cols":80,"history":10000,"inline":true,"keys":true,"req":3}"#
        );
        assert_eq!(
            json(Request::ViewSizes { panes: vec![Dims { pane: 3, rows: 30, cols: 100 }] }),
            r#"{"type":"view_sizes","panes":[{"pane":3,"rows":30,"cols":100}]}"#
        );
        let white = Rgb16 { r: 0xffff, g: 0xffff, b: 0xffff };
        let black = Rgb16 { r: 0, g: 0, b: 0 };
        let colors = HostColors { fg: black, bg: white, cursor: None, dark: false };
        assert_eq!(
            json(Request::HostColors(colors)),
            r#"{"type":"host_colors","fg":{"r":0,"g":0,"b":0},"bg":{"r":65535,"g":65535,"b":65535},"cursor":null,"dark":false}"#
        );
        assert_eq!(json(Request::PaneKill { pane: 4 }), r#"{"type":"pane_kill","pane":4}"#);
        assert_eq!(
            json(Request::PaneDetach { pane: 4, req: 4 }),
            r#"{"type":"pane_detach","pane":4,"req":4}"#
        );
        assert_eq!(
            json(Request::PaneFocus { pane: 4, seen: true }),
            r#"{"type":"pane_focus","pane":4,"seen":true}"#
        );
        assert_eq!(
            json(Request::PaneSplit { pane: 4, direction: Axis::Vertical }),
            r#"{"type":"pane_split","pane":4,"direction":"vertical"}"#
        );
        assert_eq!(
            json(Request::PaneResizeToward { from: Some(4), toward: Direction::Left, delta: 0.05 }),
            r#"{"type":"pane_resize_toward","from":4,"toward":"left","delta":0.05}"#
        );
        assert_eq!(
            json(Request::PaneSplitRatio { a: 2, b: 3, ratio: 0.5 }),
            r#"{"type":"pane_split_ratio","a":2,"b":3,"ratio":0.5}"#
        );
        assert_eq!(
            json(Request::TabNew { workspace: Some(5) }),
            r#"{"type":"tab_new","workspace":5}"#
        );
        assert_eq!(json(Request::TabSelect { id: 4 }), r#"{"type":"tab_select","id":4}"#);
        assert_eq!(
            json(Request::PaneRun { tab: None, cmd: vec!["/bin/zsh".into()] }),
            r#"{"type":"pane_run","tab":null,"cmd":["/bin/zsh"]}"#
        );

        let read = |json: &str| serde_json::from_str::<Message>(json).unwrap();
        assert!(matches!(read(r#"{"req":2,"type":"ok"}"#), Message::Ok { req: Some(2) }));
        assert!(matches!(read(r#"{"type":"ok"}"#), Message::Ok { req: None }));
        let rows = read(r#"{"type":"pane_rows","pane":3,"epoch":7,"top":40,"skipped":2}"#);
        assert!(matches!(rows, Message::PaneRows { pane: 3, skipped: 2 }));
        assert!(matches!(
            read(r#"{"type":"pane_exited","pane":3,"code":-1}"#),
            Message::PaneExited { pane: 3, code: -1 }
        ));
        let refused = read(r#"{"req":3,"type":"error","message":"no pane 3"}"#);
        assert!(matches!(refused, Message::Error { req: Some(3), .. }), "{refused:?}");
        let refused = read(r#"{"type":"error","message":"no tab 9"}"#);
        assert!(matches!(refused, Message::Error { req: None, .. }), "{refused:?}");
        assert!(matches!(
            read(r#"{"type":"state_changed","pane":3,"from":"idle","to":"working"}"#),
            Message::Other
        ));
        let Message::Attached { wire_rev: 19, workspaces } = read(&attached_json(19)) else {
            panic!()
        };
        assert_eq!(workspaces, attached_tabs());
        let old = read(r#"{"type":"attached","session":"s","workspaces":[]}"#);
        assert!(matches!(old, Message::Attached { wire_rev: 0, .. }), "{old:?}");
        assert!(matches!(
            read(r#"{"type":"layout_changed","workspaces":[]}"#),
            Message::LayoutChanged { .. }
        ));
        let view = read(r#"{"req":3,"type":"view","workspaces":[{"id":5,"name":"w","tabs":[]}]}"#);
        assert!(matches!(&view, Message::View { workspaces } if workspaces[0].id == 5), "{view:?}");
        assert!(matches!(
            read(r#"{"req":4,"type":"pane_created","id":9}"#),
            Message::PaneCreated { id: 9 }
        ));
        assert!(matches!(
            read(r#"{"type":"pane_focused","pane":9}"#),
            Message::PaneFocused { pane: 9 }
        ));
        assert!(matches!(
            read(r#"{"type":"focus_edge","pane":9,"toward":"up"}"#),
            Message::FocusEdge { pane: 9, toward: Direction::Up }
        ));
        assert!(matches!(
            read(r#"{"type":"resize_edge","pane":9,"toward":"right"}"#),
            Message::ResizeEdge { pane: 9, toward: Direction::Right }
        ));
        let noted = read(
            r#"{"type":"noted","note":{"id":4,"time":1,"pane":3,"pane_title":"claude",
                "workspace":"w","kind":"blocked","title":null,"body":"Bash · ls","read":false}}"#,
        );
        let Message::Noted { note } = noted else { panic!("{noted:?}") };
        assert_eq!((note.pane, note.kind.as_str(), note.title), (3, "blocked", None));
        let snapshot = read(
            r#"{"type":"snapshot","session":"s","wire_rev":19,"attached_terminal":null,"panes":[
                {"id":3,"workspace":"w","dir":"/w","tab":"t","title":"claude","agent":"claude",
                 "state":"blocked","source":"hook","exited":null,"focused":false,
                 "user_vars":{"TUTTI_KEYS":"pane"},"cwd":"/w/src",
                 "approval":{"id":1,"tool":"Bash","summary":"ls"},"issue":null}]}"#,
        );
        let Message::Snapshot { panes } = snapshot else { panic!("{snapshot:?}") };
        let pane = &panes[0];
        assert_eq!((pane.id, pane.title.as_str(), pane.state.as_str()), (3, "claude", "blocked"));
        assert_eq!(pane.cwd.as_deref(), Some(Path::new("/w/src")));
        assert!(pane.approval.is_some());
        assert_eq!(pane.user_vars["TUTTI_KEYS"], "pane");

        let hello = r#"{"type":"hello","rev":2,"startup":{"first_run":null,"notice":null}}"#;
        assert!(matches!(serde_json::from_str(hello).unwrap(), HostFrame::Hello { rev: 2 }));
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
            self.send(Frame::Raw { pane, bytes: bytes.to_vec() });
        }

        /// Stream `pane` at 4×40, its screen what a terminal makes of `screen`.
        fn screen(&mut self, pane: u64, screen: &[u8]) {
            let mut shell = Terminal::new(4, 40, 100);
            shell.advance(screen);
            self.send(Frame::Snapshot { pane, rows: 4, cols: 40, bytes: shell.snapshot() });
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
        daemon.say(&attached_json(WIRE_REV));
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
            "inline": true, "keys": true, "req": null
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
        daemon.say(&attached_json(WIRE_REV));
        thread::sleep(Duration::from_millis(50));
        assert!(!open.is_finished(), "the opening waits for the first snapshot");
        daemon.watched(&[(3, "working", r#""TUTTI_KEYS":"pane""#)]);
        let (session, tabs, _told) = open.join().unwrap().unwrap();
        assert_eq!(tabs, attached_tabs());
        assert_eq!(name(&session.socket), "s");
        session.ask(&Request::PaneKill { pane: 9 });
        assert_eq!(daemon.asked()["type"], "pane_kill", "no view asked: nothing came before");
        let (notices, mut heard) = unbounded();
        let shows = shows(notices);
        let watched = shows.watched.clone();
        session.stream(3, 4, 40, shows).unwrap();
        assert_eq!(daemon.asked()["type"], "pane_attach");
        assert_eq!(watched.lock().unwrap().state, "working");
        let seen =
            received(&mut heard, |notice| matches!(notice, Notice::Event(Event::UserVar { .. })));
        assert!(
            matches!(&seen[0], Notice::Event(Event::Title(title)) if title == "sh"),
            "{seen:?}"
        );
        assert!(matches!(seen[1], Notice::Event(Event::Progress { state: 3, .. })), "{seen:?}");

        let (open, mut daemon) = opening();
        daemon.say(&attached_json(WIRE_REV));
        daemon.say(r#"{"type":"layout_changed","workspaces":[{"id":5,"tabs":[]}]}"#);
        daemon.watched(&[]);
        let (_, tabs, _) = open.join().unwrap().unwrap();
        assert_eq!(tabs, [Workspace { id: 5, tabs: Vec::new() }]);
    }

    /// A layout change told before the answer may be newer than the answer's layout (the daemon
    /// tells changes to a client it registered before it took that layout): the opening asks the
    /// session's view, which the daemon takes after it, and opens on that.
    #[test]
    fn a_change_told_before_the_answer_is_settled_by_the_view() {
        let (open, mut daemon) = opening();
        let newer = r#"{"type":"layout_changed","workspaces":[{"id":5,"tabs":[]}]}"#;
        daemon.say(newer);
        daemon.say(&attached_json(WIRE_REV));
        daemon.watched(&[]);
        thread::sleep(Duration::from_millis(100));
        assert!(!open.is_finished(), "the opening waits for the view");
        assert_eq!(daemon.asked(), json!({"type": "view"}));
        daemon.say(&newer.replace("layout_changed", "view"));
        let (_, tabs, _) = open.join().unwrap().unwrap();
        assert_eq!(tabs, [Workspace { id: 5, tabs: Vec::new() }]);
    }

    /// What the session says after its opening: its layout, a focus from elsewhere, the edges it
    /// hands on, the pane a request made, and a refusal go to the window, in order; a frame of a
    /// pane not streamed, and the exit of one, go nowhere; and its end says so.
    #[test]
    fn a_sessions_events_go_to_the_window() {
        let (session, mut told, mut daemon) = session();
        session.ask(&Request::PaneSplit { pane: 3, direction: Axis::Horizontal });
        assert_eq!(daemon.asked()["type"], "pane_split");
        daemon.send(Frame::Snapshot { pane: 3, rows: 4, cols: 40, bytes: b"x".to_vec() });
        daemon.say(r#"{"type":"pane_exited","pane":3,"code":0}"#);
        daemon.say(r#"{"type":"layout_changed","workspaces":[]}"#);
        daemon.say(r#"{"req":7,"type":"pane_created","id":8}"#);
        daemon.say(r#"{"type":"state_changed","pane":3,"from":"idle","to":"working"}"#);
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
    /// what is typed; a snapshot's go nowhere.
    #[test]
    fn replies_to_raw_reads_go_back_and_a_snapshots_do_not() {
        let (pane, _notices, mut daemon) = attached();
        daemon.rows(3);
        let asking = b"\x1b[H\x1b[2J\x1b[6n$ ".to_vec();
        daemon.send(Frame::Snapshot { pane: 3, rows: 4, cols: 40, bytes: asking });
        daemon.raw(3, b"\x1b[c");
        let Some(Frame::Input { pane: 3, bytes }) = daemon.heard() else { panic!("no reply") };
        assert!(bytes.starts_with(b"\x1b[?") && bytes.ends_with(b"c"), "{bytes:?}");
        pane.write(b"ls\r".to_vec());
        assert_eq!(daemon.heard(), Some(Frame::Input { pane: 3, bytes: b"ls\r".to_vec() }));
    }

    /// A resize moves the pane's vote, and the terminal takes the size of the snapshot that
    /// follows, the screen redrawn without a copy of it in the scrollback.
    #[test]
    fn a_resize_votes_and_the_snapshot_at_the_size_settled_resizes() {
        let (mut pane, _notices, mut daemon) = attached();
        let mut shell = Terminal::new(4, 40, 100);
        shell.advance(b"$ ls\r\nfile\r\n$ ");
        daemon.rows(3);
        daemon.send(Frame::Snapshot { pane: 3, rows: 4, cols: 40, bytes: shell.snapshot() });
        wait_until(&pane.term, "showed the screen", |term| term.contents().contains("file"));
        pane.resize(6, 30, (8, 16));
        let vote = json!({"type": "view_sizes", "panes": [{"pane": 3, "rows": 6, "cols": 30}]});
        assert_eq!(daemon.asked(), vote);
        assert_eq!(pane.lock().size(), (4, 40), "the daemon settles the size");
        shell.resize(6, 30);
        daemon.say(r#"{"type":"ok"}"#);
        daemon.rows(3);
        daemon.send(Frame::Snapshot { pane: 3, rows: 6, cols: 30, bytes: shell.snapshot() });
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
            daemon.send(Frame::Snapshot { pane: 3, rows: 4, cols: 40, bytes: shell.snapshot() });
            daemon.raw(3, stale.as_bytes());
            // The pane goes on to print what the stream skips.
            shell.advance(lines(3..printed).as_bytes());
            let top = shell.screen_top();
            let rows = format!(
                r#"{{"type":"pane_rows","pane":3,"epoch":1,"top":{top},"skipped":{from}}}"#
            );
            daemon.say(&rows);
            daemon.send(Frame::Raw { pane: 3, bytes: shell.history(from as u64, top) });
            daemon.send(Frame::Snapshot { pane: 3, rows: 4, cols: 40, bytes: shell.snapshot() });
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
        daemon.send(Frame::Snapshot { pane: 3, rows: 4, cols: 40, bytes: shell.snapshot() });
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

    /// An opening refused: another wire revision, another proxy's host frames, the daemon's own
    /// refusal, a pane frame where a quiet attach is sent none, or the daemon gone. Each says why.
    #[test]
    fn an_opening_says_why_it_was_refused() {
        let refused = |play: &dyn Fn(&mut Daemon)| {
            let (open, mut daemon) = opening();
            play(&mut daemon);
            drop(daemon);
            open.join().unwrap().err().expect("refused").to_string()
        };
        let old = refused(&|daemon| daemon.say(&attached_json(18)));
        assert!(old.contains("wire revision 18, rtty 19: update tutti"), "{old}");
        let newer = refused(&|daemon| daemon.say(&attached_json(20)));
        assert!(newer.ends_with("update rtty"), "{newer}");
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
        daemon.say(&attached_json(WIRE_REV));
        daemon.watched(&[]);
        assert!(open.join().unwrap().is_ok());
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
        theirs.write_all(&control(&Request::Watch).encode()).unwrap();
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
