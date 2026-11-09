//! A pane of tutti's daemon, drawn as an rtty pane: the daemon keeps the pty, the session and the
//! agent's state, and rtty draws (`Pane::attach`). Here is the daemon's wire as rtty speaks it,
//! written from tutti's `docs/transport-decision.md` and its protocol (the frames, and the few
//! requests, responses and events a pane stream needs), and the stream's opening and reader,
//! ported from tutti's `pane attach --inline`.
//!
//! One connection a pane, to the session's daemon on its Unix socket. The connection watches the
//! session, whose first snapshot says the daemon speaks this wire and holds the pane, and whose
//! later ones name the pane and tell its agent's state. Then it streams the pane: the scrollback
//! rows the pane holds, a snapshot of its screen, and every pty read raw, but for the terminal
//! queries the daemon answered itself. The pane's own `Terminal` takes it all, so scrollback,
//! search, selection and links work on it as on any pane. Closing the connection detaches: the
//! pane and its program run on in the daemon.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::mem;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use futures::channel::mpsc::UnboundedSender;
use rtty_vt::{BACKGROUND, CURSOR, Event, FOREGROUND, Rgb, Terminal};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::pane::{Notice, SCROLLBACK, Trace};
use crate::scheme::Scheme;

/// The revision of the daemon's requests, responses and events this rtty speaks, which a watched
/// session's snapshot names (tutti's `WIRE_REV`).
pub const WIRE_REV: u32 = 18;

/// The revision of the host frames a `tutti proxy` speaks, which its hello names (`HOST_REV`).
pub const HOST_REV: u32 = 2;

/// The longest frame either side sends.
const MAX_FRAME: usize = 16 << 20;

/// One frame: `[u32 len][u8 kind][payload]`, little-endian, `len` counting the kind byte and the
/// payload. A pane frame's payload starts with the pane's id, a `u64`.
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
/// side closed the connection.
fn next(input: &mut impl Read, buf: &mut Vec<u8>) -> io::Result<Option<Frame>> {
    let mut chunk = [0; 1 << 16];
    loop {
        if let Some((frame, used)) = Frame::decode(buf)? {
            buf.drain(..used);
            return Ok(Some(frame));
        }
        match input.read(&mut chunk) {
            Ok(0) => return Ok(None),
            Ok(read) => buf.extend_from_slice(&chunk[..read]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

/// What rtty asks of the daemon.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Subscribe to the session's snapshots (`Message::Snapshot`): one at once, then one on every
    /// change. Read-only: it moves no size, focus or seen state.
    Watch,
    /// The colours of the terminal the panes are shown on, which the daemon answers their colour
    /// queries with, and whose light or dark it tells the programs that asked (mode 2031).
    HostColors(HostColors),
    /// Stream `pane` to this connection: `history` rows of its scrollback, a snapshot, then every
    /// pty read raw. `rows`×`cols` is this connection's vote in the pane's size; `inline` numbers
    /// each snapshot's rows (`Message::PaneRows`) and hands on, before a resync's snapshot, the rows
    /// that scrolled off while the stream was behind.
    PaneAttach { pane: u64, rows: u16, cols: u16, history: u32, inline: bool },
    /// Moves this connection's vote in its pane's size.
    ViewSizes { panes: Vec<Dims> },
    /// Marks `pane` the session's active one: a finished agent's pane is then seen.
    PaneFocus { pane: u64 },
    /// Ends `pane`'s program.
    PaneKill { pane: u64 },
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

/// What the daemon says on a pane stream's connection that rtty acts on: a response (`ok`,
/// `error`) or an event, tagged as tutti's are; anything else is `Other`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Message {
    Ok,
    Error {
        message: String,
    },
    /// Before each snapshot: the scrollback rows up to its screen come between this and it, as
    /// logical lines, after `skipped` rows the stream leaves out.
    PaneRows {
        skipped: u64,
    },
    /// The pane's program exited; the daemon then closes the connection.
    PaneExited {
        code: i32,
    },
    /// The whole session as a watcher sees it.
    Snapshot {
        wire_rev: u32,
        panes: Vec<Watched>,
    },
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

/// The connection to the daemon listening on `socket`, or why there is none.
pub fn connect(socket: &Path) -> io::Result<UnixStream> {
    UnixStream::connect(socket).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
            let socket = socket.display();
            io::Error::new(err.kind(), format!("no tutti daemon listens on {socket}"))
        }
        _ => err,
    })
}

/// How long the daemon has to answer an opening.
const OPENING: Duration = Duration::from_secs(3);

/// Opens pane `pane`'s stream on `stream`, a connection to tutti's daemon (or to a `tutti proxy`,
/// whose hello comes first and must speak `HOST_REV`): the session watched, whose first snapshot
/// must speak `WIRE_REV` and hold the pane, then `colors` and the attach, `rows`×`cols` this
/// connection's vote in the pane's size, with as much of the pane's scrollback as a pane holds.
/// What that snapshot said of the pane, and the bytes read past it.
pub fn open(
    stream: &mut UnixStream,
    pane: u64,
    rows: u16,
    cols: u16,
    colors: Option<HostColors>,
) -> io::Result<(Watched, Vec<u8>)> {
    stream.set_read_timeout(Some(OPENING))?;
    stream.write_all(&control(&Request::Watch).encode())?;
    let mut buf = Vec::new();
    let watched = loop {
        let frame = next(stream, &mut buf).map_err(|err| match err.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                let waited = OPENING.as_secs();
                io::Error::other(format!("tutti's daemon did not answer within {waited} s"))
            }
            _ => err,
        })?;
        match frame.ok_or_else(|| io::Error::other("tutti's daemon closed the connection"))? {
            Frame::Host(json) => {
                if let Ok(HostFrame::Hello { rev }) = serde_json::from_slice(&json)
                    && rev != HOST_REV
                {
                    return Err(io::Error::other(format!(
                        "tutti proxy speaks host frames revision {rev}, rtty {HOST_REV}: run \
                         the same tutti on both ends"
                    )));
                }
            }
            Frame::Control(json) => match serde_json::from_slice(&json) {
                Ok(Message::Snapshot { wire_rev, .. }) if wire_rev != WIRE_REV => {
                    let update = match wire_rev < WIRE_REV {
                        true => "update tutti and restart its daemon (`tutti server stop`)",
                        false => "update rtty",
                    };
                    return Err(io::Error::other(format!(
                        "tutti's daemon speaks wire revision {wire_rev}, rtty {WIRE_REV}: {update}"
                    )));
                }
                Ok(Message::Snapshot { panes, .. }) => {
                    let mut panes = panes.into_iter();
                    let found = panes.find(|watched| watched.id == pane);
                    break found.ok_or_else(|| {
                        io::Error::other(format!("no pane {pane} in that tutti session"))
                    })?;
                }
                Ok(Message::Error { message }) => return Err(io::Error::other(message)),
                _ => {}
            },
            _ => {}
        }
    };
    if let Some(colors) = colors {
        stream.write_all(&control(&Request::HostColors(colors)).encode())?;
    }
    let history = SCROLLBACK as u32;
    let attach = Request::PaneAttach { pane, rows, cols, history, inline: true };
    stream.write_all(&control(&attach).encode())?;
    stream.set_read_timeout(None)?;
    Ok((watched, buf))
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

/// A pane stream's reader: the pane's terminal it feeds, the frames it hands the writer (the
/// engine's replies), the window's notices (`Output` at most one in flight), and what the
/// session's last snapshot said of the pane, which the pane reads its title from.
pub struct Reader {
    pub pane: u64,
    pub term: Arc<Mutex<Terminal>>,
    pub frames: mpsc::Sender<Frame>,
    pub notices: UnboundedSender<Notice>,
    pub pending: Arc<AtomicBool>,
    pub watched: Arc<Mutex<Watched>>,
    pub trace: Option<Trace>,
}

impl Reader {
    /// Reads the stream from `input` until it ends, `watched` holding what the opening's snapshot
    /// said of the pane. Each snapshot comes after a `PaneRows` and the scrollback rows it
    /// announced, gathered until then (`show`); each raw read is fed as it is, and the engine's
    /// replies to it go back to the pane: each answers a query the daemon left unanswered. Then
    /// tells the window how it ended: the pane's exit, the daemon gone, or what went wrong.
    pub fn run(self, mut input: impl Read) {
        let first = self.watched.lock().expect("a reader panicked").clone();
        self.tell_of(&Watched::default(), &first);
        let (mut buf, mut rows) = (Vec::new(), None::<Vec<u8>>);
        let ended = loop {
            let frame = match next(&mut input, &mut buf) {
                Ok(Some(frame)) => frame,
                Ok(None) => break "tutti's daemon closed the connection".to_owned(),
                Err(err) => break format!("tutti's stream broke: {err}"),
            };
            match frame {
                Frame::Raw { bytes, .. } => match rows.as_mut() {
                    Some(rows) => rows.extend(bytes),
                    None => self.feed(|term| term.advance(&bytes), true, &bytes),
                },
                Frame::Snapshot { rows: height, cols, bytes, .. } => {
                    let scrolled = rows.take().unwrap_or_default();
                    self.feed(|term| show(term, height, cols, &scrolled, &bytes), false, &bytes)
                }
                Frame::Control(json) => match serde_json::from_slice(&json) {
                    Ok(Message::PaneRows { skipped }) => rows = Some(left_out(self.pane, skipped)),
                    Ok(Message::PaneExited { code }) => break format!("Exited with code {code}"),
                    Ok(Message::Error { message }) => break message,
                    Ok(Message::Snapshot { panes, .. }) => {
                        if let Some(now) = panes.into_iter().find(|pane| pane.id == self.pane) {
                            let mut watched = self.watched.lock().expect("a reader panicked");
                            let was = mem::replace(&mut *watched, now.clone());
                            drop(watched);
                            self.tell_of(&was, &now)
                        }
                    }
                    _ => {}
                },
                Frame::Input { .. } | Frame::Host(_) => {}
            }
        };
        let _ = self.notices.unbounded_send(Notice::Exit(ended));
    }

    /// Changes the terminal with `change`, under its lock, for `read`: then the engine's replies
    /// go to the pane when `answer` (a snapshot's are dropped: it asks nothing of anyone), its
    /// other events to the window, and the window hears there is output to draw.
    fn feed(&self, change: impl FnOnce(&mut Terminal), answer: bool, read: &[u8]) {
        if let Some(trace) = &self.trace {
            trace.log('<', read);
        }
        let mut term = self.term.lock().expect("the window panicked");
        change(&mut term);
        let events = term.take_events();
        drop(term);
        for event in events {
            match event {
                Event::Reply(bytes) if answer => {
                    let _ = self.frames.send(Frame::Input { pane: self.pane, bytes });
                }
                Event::Reply(_) => {}
                event => self.tell(event),
            }
        }
        if !self.pending.swap(true, Ordering::AcqRel) {
            let _ = self.notices.unbounded_send(Notice::Output);
        }
    }

    fn tell(&self, event: Event) {
        let _ = self.notices.unbounded_send(Notice::Event(event));
    }

    /// The session's snapshot said `was` of the pane and now says `now`. A new title or directory
    /// renames it; its agent working shows on the tab's pill, going on, or paused while blocked or
    /// holding a permission request; its turn ending is a notification; and the program's claim on
    /// the keys (`TUTTI_KEYS`, which tutti's editor plugins set) is the window's to honour as
    /// `RTTY_KEYS` is. The pane's own OSC 9, 777, 9;4 and 7501 come raw, and are not told again.
    fn tell_of(&self, was: &Watched, now: &Watched) {
        if (&was.title, &was.cwd) != (&now.title, &now.cwd) {
            self.tell(Event::Title(now.title.clone()));
        }
        let pill = |watched: &Watched| match watched.state.as_str() {
            _ if watched.approval.is_some() => 4,
            "blocked" => 4,
            "working" => 3,
            _ => 0,
        };
        if pill(was) != pill(now) {
            self.tell(Event::Progress { state: pill(now), percent: None });
        }
        let keys = |watched: &Watched| watched.user_vars.get("TUTTI_KEYS").cloned();
        if keys(was) != keys(now) {
            let value = keys(now).unwrap_or_default();
            self.tell(Event::UserVar { name: "TUTTI_KEYS".into(), value });
        }
        if was.state == "working" && now.state == "done" {
            let (title, body) = (Some(now.title.clone()), Some("done".into()));
            self.tell(Event::Notification { title, body });
        }
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
/// ED 2 would push the stale screen into the scrollback. And what the daemon keeps from a stream
/// goes: mode 2031, whose light/dark reports are the daemon's to send (rtty tells it of a flip with
/// `HostColors`), and the keyboard protocols, kitty's flags and modifyOtherKeys, whose changes never
/// reach a stream, so the keys stay legacy rather than follow a state that went stale.
fn strip(snapshot: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(snapshot.len());
    let (mut rest, mut cleared) = (snapshot, false);
    while let Some(at) = rest.windows(2).position(|pair| pair == b"\x1b[") {
        out.extend_from_slice(&rest[..at]);
        let end = rest[at + 2..].iter().position(|byte| (0x40..=0x7e).contains(byte));
        let end = end.map_or(rest.len(), |last| at + last + 3);
        let seq = &rest[at..end];
        let keys = (seq.starts_with(b"\x1b[=") || seq.starts_with(b"\x1b[>"))
            && seq.ends_with(b"u")
            || seq.starts_with(b"\x1b[>4;") && (seq.ends_with(b"m") || seq.ends_with(b"f"));
        if seq == b"\x1b[2J" && !mem::replace(&mut cleared, true) {
            out.extend_from_slice(b"\x1b[J");
        } else if !keys && !seq.starts_with(b"\x1b[?2031") {
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
    use futures::channel::mpsc::UnboundedReceiver;
    use std::thread;
    use std::time::Instant;

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
    /// sequence number, then the screen.
    #[test]
    fn a_snapshot_reads_as_tutti_writes_it() {
        let mut wire = 1u32.to_le_bytes().to_vec();
        wire.push(2);
        wire.extend(9u64.to_le_bytes());
        wire.extend(30u16.to_le_bytes());
        wire.extend(100u16.to_le_bytes());
        wire.extend(5u32.to_le_bytes());
        wire.extend(b"screen");
        let len = wire.len() as u32 - 4;
        wire[..4].copy_from_slice(&len.to_le_bytes());
        let (frame, _) = Frame::decode(&wire).unwrap().unwrap();
        assert_eq!(
            frame,
            Frame::Snapshot { pane: 9, rows: 30, cols: 100, bytes: b"screen".to_vec() }
        );
    }

    #[test]
    fn a_frame_rtty_cannot_read_is_an_error() {
        let delta = [&5u32.to_le_bytes()[..], &[3], &[0; 4]].concat();
        assert!(Frame::decode(&delta).unwrap_err().to_string().contains("kind 3"));
        let short = [&5u32.to_le_bytes()[..], &[5], &[0; 4]].concat();
        assert!(Frame::decode(&short).unwrap_err().to_string().contains("cut short"));
        assert!(Frame::decode(&0u32.to_le_bytes()).is_err());
        assert!(Frame::decode(&u32::MAX.to_le_bytes()).is_err());
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
        assert_eq!(
            json(Request::PaneAttach {
                pane: 3,
                rows: 24,
                cols: 80,
                history: 10_000,
                inline: true
            }),
            r#"{"type":"pane_attach","pane":3,"rows":24,"cols":80,"history":10000,"inline":true}"#
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

        let read = |json: &str| serde_json::from_str::<Message>(json).unwrap();
        assert!(matches!(read(r#"{"req":2,"type":"ok"}"#), Message::Ok));
        let rows = read(r#"{"type":"pane_rows","pane":3,"epoch":7,"top":40,"skipped":2}"#);
        assert!(matches!(rows, Message::PaneRows { skipped: 2 }));
        assert!(matches!(
            read(r#"{"type":"pane_exited","pane":3,"code":-1}"#),
            Message::PaneExited { code: -1 }
        ));
        assert!(matches!(read(r#"{"type":"layout_changed","workspaces":[]}"#), Message::Other));
        let snapshot = read(
            r#"{"type":"snapshot","session":"s","wire_rev":18,"attached_terminal":null,"panes":[
                {"id":3,"workspace":"w","dir":"/w","tab":"t","title":"claude","agent":"claude",
                 "state":"blocked","source":"hook","exited":null,"focused":false,
                 "user_vars":{"TUTTI_KEYS":"pane"},"cwd":"/w/src",
                 "approval":{"id":1,"tool":"Bash","summary":"ls"},"issue":null}]}"#,
        );
        let Message::Snapshot { wire_rev: 18, panes } = snapshot else { panic!("{snapshot:?}") };
        let pane = &panes[0];
        assert_eq!((pane.id, pane.title.as_str(), pane.state.as_str()), (3, "claude", "blocked"));
        assert_eq!(pane.cwd.as_deref(), Some(std::path::Path::new("/w/src")));
        assert!(pane.approval.is_some());
        assert_eq!(pane.user_vars["TUTTI_KEYS"], "pane");

        let hello = r#"{"type":"hello","rev":2,"startup":{"first_run":null,"notice":null}}"#;
        assert!(matches!(serde_json::from_str(hello).unwrap(), HostFrame::Hello { rev: 2 }));
    }

    /// The daemon's end of a pane's connection, played by a test.
    struct Daemon {
        stream: UnixStream,
        buf: Vec<u8>,
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

        /// The session as a watcher sees it, at wire revision `rev`, its panes as `panes` writes
        /// them: `(id, state, user vars)`, each titled `sh` and in `/w`.
        fn watched(&mut self, rev: u32, panes: &[(u64, &str, &str)]) {
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
                r#"{{"type":"snapshot","session":"s","wire_rev":{rev},"attached_terminal":null,"panes":[{panes}]}}"#
            ));
        }
    }

    type Attached = io::Result<(Pane, UnboundedReceiver<Notice>)>;

    /// `Pane::attach` of pane 3, 4×40 and with no colours, on a thread of its own over a socket
    /// pair, and the daemon's end, which has heard the watch.
    fn attaching() -> (thread::JoinHandle<Attached>, Daemon) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let attach = thread::spawn(move || Pane::attach(ours, 3, 4, 40, None, None, false));
        let mut daemon = Daemon { stream: theirs, buf: Vec::new() };
        assert_eq!(daemon.asked(), serde_json::json!({"type": "watch"}));
        (attach, daemon)
    }

    /// A pane 3 opened as the daemon opens it, its stream not yet begun.
    fn attached() -> (Pane, UnboundedReceiver<Notice>, Daemon) {
        let (attach, mut daemon) = attaching();
        daemon.watched(WIRE_REV, &[(2, "idle", ""), (3, "idle", "")]);
        let attach_request = serde_json::json!({
            "type": "pane_attach", "pane": 3, "rows": 4, "cols": 40, "history": 10000, "inline": true
        });
        assert_eq!(daemon.asked(), attach_request);
        let (pane, notices) = attach.join().unwrap().unwrap();
        (pane, notices, daemon)
    }

    fn wait_until(pane: &Pane, what: &str, done: impl Fn(&Terminal) -> bool) {
        let started = Instant::now();
        while !done(&pane.lock()) {
            let screen = pane.lock().contents();
            assert!(started.elapsed() < Duration::from_secs(5), "never {what}: {screen:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// The notices up to the first `until` takes, within 5 s.
    fn told(
        notices: &mut UnboundedReceiver<Notice>,
        until: impl Fn(&Notice) -> bool,
    ) -> Vec<Notice> {
        let (started, mut seen) = (Instant::now(), Vec::new());
        while !seen.last().is_some_and(&until) {
            match notices.try_recv() {
                Ok(notice) => seen.push(notice),
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

    /// The opening and a stream's first snapshot: its history goes into the scrollback, after a
    /// line on what was left out, and the snapshot draws the screen without pushing the blank one
    /// into it, less the modes the daemon keeps (2031 and the keyboard protocols); raw output then
    /// goes on from there. tutti's title and directory name the pane.
    #[test]
    fn a_stream_opens_on_its_history_and_its_snapshot() {
        let (pane, mut notices, mut daemon) = attached();
        assert_eq!(pane.tutti_name(), Some(("sh".into(), Some("/w".into()))));
        let mut shell = Terminal::new(4, 40, 100);
        shell.advance(b"\x1b[?2031h\x1b[>1u\x1b[>4;2m\x1b[?1004h$ ");
        daemon.say(r#"{"type":"pane_rows","pane":3,"epoch":1,"top":2,"skipped":5}"#);
        daemon.send(Frame::Raw { pane: 3, bytes: b"one\r\n".to_vec() });
        daemon.send(Frame::Raw { pane: 3, bytes: b"two\r\n".to_vec() });
        daemon.send(Frame::Snapshot { pane: 3, rows: 4, cols: 40, bytes: shell.snapshot() });
        daemon.send(Frame::Raw { pane: 3, bytes: b"echo hi".to_vec() });
        wait_until(&pane, "showed the typing", |term| term.contents().contains("$ echo hi"));
        let term = pane.lock();
        let left_out = "[tutti pane 3: 5 lines not shown]";
        assert_eq!(scrollback(&term), [left_out, "one", "two"]);
        let modes = term.modes();
        assert!(modes.focus_reports, "what the program set comes with the snapshot");
        assert!(!modes.color_scheme_reports && modes.modify_other_keys == 0);
        assert_eq!(term.kitty_keyboard(), 0);
        drop(term);
        let seen = told(&mut notices, |notice| matches!(notice, Notice::Output));
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
        daemon.say(r#"{"type":"pane_rows","pane":3,"epoch":1,"top":0,"skipped":0}"#);
        let asking = b"\x1b[H\x1b[2J\x1b[6n$ ".to_vec();
        daemon.send(Frame::Snapshot { pane: 3, rows: 4, cols: 40, bytes: asking });
        daemon.send(Frame::Raw { pane: 3, bytes: b"\x1b[c".to_vec() });
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
        daemon.say(r#"{"type":"pane_rows","pane":3,"epoch":1,"top":0,"skipped":0}"#);
        daemon.send(Frame::Snapshot { pane: 3, rows: 4, cols: 40, bytes: shell.snapshot() });
        wait_until(&pane, "showed the screen", |term| term.contents().contains("file"));
        pane.resize(6, 30, (8, 16));
        let vote = serde_json::json!({"type": "view_sizes", "panes": [{"pane": 3, "rows": 6, "cols": 30}]});
        assert_eq!(daemon.asked(), vote);
        assert_eq!(pane.lock().size(), (4, 40), "the daemon settles the size");
        shell.resize(6, 30);
        daemon.say(r#"{"type":"ok"}"#);
        daemon.say(r#"{"type":"pane_rows","pane":3,"epoch":1,"top":0,"skipped":0}"#);
        daemon.send(Frame::Snapshot { pane: 3, rows: 6, cols: 30, bytes: shell.snapshot() });
        wait_until(&pane, "resized", |term| term.size() == (6, 30));
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
            daemon.say(r#"{"type":"pane_rows","pane":3,"epoch":1,"top":0,"skipped":0}"#);
            daemon.send(Frame::Snapshot { pane: 3, rows: 4, cols: 40, bytes: shell.snapshot() });
            daemon.send(Frame::Raw { pane: 3, bytes: stale.into() });
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
            wait_until(&pane, "resynced", |term| term.contents().contains(&last));
            let term = pane.lock();
            assert_eq!(scrollback(&term), want, "{stale:?}, {printed} printed");
            assert_eq!(term.contents(), shell.contents(), "{stale:?}");
            assert!(!term.modes().alt_screen);
        }
    }

    /// The session's snapshots name the pane and show its agent on the pill: going on while it
    /// works, paused while it is blocked, and a notification as its turn ends; a claim on the
    /// keys tutti's plugins make is told as `RTTY_KEYS` is. Other panes' changes tell nothing.
    #[test]
    fn the_sessions_snapshots_tell_the_agents_state() {
        let (_pane, mut notices, mut daemon) = attached();
        told(&mut notices, |notice| matches!(notice, Notice::Event(Event::Title(_))));
        daemon.watched(WIRE_REV, &[(2, "working", ""), (3, "working", "")]);
        let progress = |state| move |notice: &Notice| matches!(notice, Notice::Event(Event::Progress { state: s, .. }) if *s == state);
        assert_eq!(told(&mut notices, progress(3)).len(), 1);
        daemon.watched(WIRE_REV, &[(2, "done", ""), (3, "blocked", r#""TUTTI_KEYS":"pane""#)]);
        let claimed = |notice: &Notice| {
            let claim = Event::UserVar { name: "TUTTI_KEYS".into(), value: "pane".into() };
            matches!(notice, Notice::Event(event) if *event == claim)
        };
        let seen = told(&mut notices, claimed);
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(progress(4)(&seen[0]), "{seen:?}");
        daemon.watched(WIRE_REV, &[(3, "working", "")]);
        daemon.watched(WIRE_REV, &[(3, "done", "")]);
        let ended = |notice: &Notice| matches!(notice, Notice::Event(Event::Notification { .. }));
        let seen = told(&mut notices, ended);
        let Notice::Event(Event::Notification { title, body }) = seen.last().unwrap() else {
            panic!()
        };
        assert_eq!((title.as_deref(), body.as_deref()), (Some("sh"), Some("done")));
        assert!(matches!(seen[seen.len() - 2], Notice::Event(Event::Progress { state: 0, .. })));
    }

    /// The pane's exit ends the stream; closing the pane closes the connection and nothing more.
    #[test]
    fn the_programs_exit_ends_the_stream_and_a_close_only_detaches() {
        let (_pane, mut notices, mut daemon) = attached();
        daemon.say(r#"{"type":"pane_exited","pane":3,"code":3}"#);
        let seen = told(&mut notices, |notice| matches!(notice, Notice::Exit(_)));
        assert!(
            matches!(seen.last(), Some(Notice::Exit(status)) if status == "Exited with code 3")
        );

        let (pane, _notices, mut daemon) = attached();
        pane.set_scheme(&find(&load(&[]).unwrap(), "Solarized Light").unwrap());
        assert_eq!(daemon.asked()["dark"], false, "the daemon hears of the scheme");
        drop(pane);
        assert_eq!(daemon.heard(), None, "closed, with no kill asked");
    }

    /// An opening refused: another wire revision, another proxy's host frames, no such pane, the
    /// daemon's own refusal, or the daemon gone. Each says why.
    #[test]
    fn an_opening_says_why_it_was_refused() {
        let refused = |play: &dyn Fn(&mut Daemon)| {
            let (attach, mut daemon) = attaching();
            play(&mut daemon);
            drop(daemon);
            attach.join().unwrap().err().expect("refused").to_string()
        };
        let old = refused(&|daemon| daemon.watched(17, &[(3, "idle", "")]));
        assert!(old.contains("wire revision 17, rtty 18: update tutti"), "{old}");
        let newer = refused(&|daemon| daemon.watched(19, &[(3, "idle", "")]));
        assert!(newer.ends_with("update rtty"), "{newer}");
        let proxy =
            refused(&|daemon| daemon.send(Frame::Host(br#"{"type":"hello","rev":1}"#.into())));
        assert!(proxy.contains("host frames revision 1, rtty 2"), "{proxy}");
        let missing = refused(&|daemon| daemon.watched(WIRE_REV, &[(2, "idle", "")]));
        assert_eq!(missing, "no pane 3 in that tutti session");
        let error =
            refused(&|daemon| daemon.say(r#"{"type":"error","message":"bad request: watch"}"#));
        assert_eq!(error, "bad request: watch");
        assert!(refused(&|_| {}).contains("closed the connection"));

        // A proxy that speaks this revision says hello first, and the opening goes on.
        let (attach, mut daemon) = attaching();
        daemon.send(Frame::Host(br#"{"type":"hello","rev":2,"startup":{}}"#.into()));
        daemon.watched(WIRE_REV, &[(3, "idle", "")]);
        assert_eq!(daemon.asked()["type"], "pane_attach");
        assert!(attach.join().unwrap().is_ok());
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
