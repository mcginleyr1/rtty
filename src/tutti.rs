//! A pane of tutti's daemon, drawn as an rtty pane: the daemon keeps the pty, rtty draws. This is
//! the daemon's wire as rtty speaks it, written from tutti's `docs/transport-decision.md` and its
//! protocol: the frames, and the few requests, responses and events a pane stream needs.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::PathBuf;

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

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
pub fn next(input: &mut impl Read, buf: &mut Vec<u8>) -> io::Result<Option<Frame>> {
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
pub enum Message {
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
pub enum HostFrame {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
