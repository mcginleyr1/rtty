//! The socket `rtty cli` talks to: one JSON line asks, one answers. A running rtty listens in a
//! directory only its user can enter, and each pane's program finds the socket in `RTTY_SOCKET`
//! and its own pane in `RTTY_PANE`. The window answers on the main thread (`window::answer`).

use std::fs::{self, DirBuilder};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde::{Deserialize, Serialize};

use crate::layout::{Amount, Direction, PaneId};

/// How long either side waits on the other.
const TIMEOUT: Duration = Duration::from_secs(5);

/// A pane-less request acts from the focused pane of the active window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    List,
    /// Shows the pane's tab and gives it the keyboard.
    Focus {
        pane: PaneId,
    },
    /// Gives the keyboard to the pane beside `from`; with `wrap`, round the tab's edge.
    FocusToward {
        from: Option<PaneId>,
        direction: Direction,
        wrap: bool,
    },
    /// Moves the divider nearest `from` that way.
    Resize {
        from: Option<PaneId>,
        direction: Direction,
        amount: Amount,
    },
    /// Puts a new pane beside the pane that way; it takes the keyboard.
    Split {
        pane: Option<PaneId>,
        direction: Direction,
    },
    Zoom {
        pane: Option<PaneId>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    Done,
    Pane(PaneId),
    /// No pane lies `direction` of `from`.
    Edge {
        from: PaneId,
        direction: Direction,
    },
    Windows(Vec<WindowInfo>),
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: u64,
    pub active: bool,
    pub tabs: Vec<TabInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    pub active: bool,
    /// Its focused pane's title, as the tab shows it.
    pub title: String,
    pub zoomed: bool,
    pub panes: Vec<PaneInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub id: PaneId,
    pub focused: bool,
    pub title: String,
    pub cwd: Option<PathBuf>,
    pub rows: u16,
    pub cols: u16,
    /// The program claimed rtty's navigation and resize keys (`RTTY_KEYS=pane`).
    pub claims_keys: bool,
}

/// A request and where its answer goes.
pub type Incoming = (Request, mpsc::Sender<Response>);

/// This process's socket, in `$TMPDIR/rtty-<uid>`: created 0700 when missing, refused when it
/// is a link, another user's, or open to anyone else.
pub fn socket_path() -> Result<PathBuf, String> {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let dir = std::env::temp_dir().join(format!("rtty-{uid}"));
    match DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(format!("{}: {err}", dir.display())),
    }
    let meta = fs::symlink_metadata(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    if !meta.is_dir() || meta.uid() != uid || meta.permissions().mode() & 0o077 != 0 {
        return Err(format!("{}: not a private directory of this user's", dir.display()));
    }
    Ok(dir.join(format!("{}.sock", std::process::id())))
}

/// Removes the sockets rtty processes left in `dir` when they died without quitting: sockets
/// named as rtty names them (`<pid>.sock`) that refuse a connection. A live rtty's socket
/// accepts and stays; nothing else in the directory is touched.
pub fn sweep(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for path in entries.flatten().map(|entry| entry.path()) {
        let stem = path.file_name().and_then(|name| name.to_str()?.strip_suffix(".sock"));
        let named =
            stem.is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()));
        let socket = fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_socket());
        let refused = |err: io::Error| err.kind() == io::ErrorKind::ConnectionRefused;
        if named && socket && UnixStream::connect(&path).is_err_and(refused) {
            let _ = fs::remove_file(&path);
        }
    }
}

/// Listens on `path`, a connection at a time, on a thread of its own. Each request arrives on
/// the receiver with the way back for its answer.
pub fn listen(path: &Path) -> io::Result<UnboundedReceiver<Incoming>> {
    // A socket a crashed rtty with this pid left behind.
    let _ = fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    let (requests, receiver) = unbounded();
    thread::Builder::new().name("rtty cli".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = serve(stream, &requests);
        }
    })?;
    Ok(receiver)
}

fn serve(stream: UnixStream, requests: &UnboundedSender<Incoming>) -> io::Result<()> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let response = match serde_json::from_str(&line) {
        Ok(request) => {
            let (answer, answered) = mpsc::channel();
            requests.unbounded_send((request, answer)).map_err(io::Error::other)?;
            answered.recv_timeout(TIMEOUT).unwrap_or(Response::Error("rtty did not answer".into()))
        }
        Err(err) => Response::Error(format!("not a request: {err}")),
    };
    writeln!(&stream, "{}", serde_json::to_string(&response)?)
}

/// Asks the rtty listening on `socket`.
pub fn ask(socket: &Path, request: &Request) -> io::Result<Response> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    writeln!(stream, "{}", serde_json::to_string(request)?)?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[test]
    fn the_socket_lives_in_a_private_directory() {
        let path = socket_path().unwrap();
        let dir = path.parent().unwrap();
        assert_eq!(fs::metadata(dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(path.ends_with(format!("{}.sock", std::process::id())));
    }

    #[test]
    fn a_request_and_its_answer_cross_the_socket() {
        let dir = std::env::temp_dir().join(format!("rtty-test-{}", std::process::id()));
        DirBuilder::new().mode(0o700).recursive(true).create(&dir).unwrap();
        let path = dir.join("a.sock");
        let mut requests = listen(&path).unwrap();
        let answerer = thread::spawn(move || {
            let (request, answer) = futures::executor::block_on(requests.next()).unwrap();
            answer.send(Response::Pane(4)).unwrap();
            request
        });
        let request =
            Request::FocusToward { from: Some(3), direction: Direction::Left, wrap: true };
        assert_eq!(ask(&path, &request).unwrap(), Response::Pane(4));
        assert_eq!(answerer.join().unwrap(), request);
        // On the wire, as another client would write it.
        let mut stream = UnixStream::connect(&path).unwrap();
        writeln!(stream, "{{\"request\":\"nonsense\"}}").unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        assert!(
            line.starts_with("{\"error\":\"not a request: unknown variant `nonsense`"),
            "{line}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_sweep_removes_dead_rttys_sockets_and_nothing_else() {
        let dir = std::env::temp_dir().join(format!("rtty-sweep-{}", std::process::id()));
        DirBuilder::new().mode(0o700).recursive(true).create(&dir).unwrap();
        let socket = |name: &str| UnixListener::bind(dir.join(name)).unwrap();
        drop(socket("11.sock"));
        let live = socket("22.sock");
        drop(socket("notes.sock"));
        fs::write(dir.join("33.sock"), "not a socket").unwrap();
        fs::write(dir.join("readme"), "").unwrap();
        sweep(&dir);
        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["22.sock", "33.sock", "notes.sock", "readme"]);
        // The live one still answers.
        assert!(UnixStream::connect(dir.join("22.sock")).is_ok());
        drop(live);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn requests_read_as_json_with_lowercase_directions() {
        let amount = Amount::Fraction(0.05);
        let request = Request::Resize { from: None, direction: Direction::Up, amount };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            json,
            r#"{"request":"resize","from":null,"direction":"up","amount":{"fraction":0.05}}"#
        );
        let edge = Response::Edge { from: 3, direction: Direction::Down };
        assert_eq!(
            serde_json::to_string(&edge).unwrap(),
            r#"{"edge":{"from":3,"direction":"down"}}"#
        );
    }
}
