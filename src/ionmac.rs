//! ionmac, the window manager whose frames rtty's windows may sit in. A focus move that finds no
//! pane at the edge of a window is handed on to it, so the same key goes on to the frame beside
//! the window. ionmac listens on `$TMPDIR/ionmac-<uid>/ionmac.sock`: one JSON line asks, one
//! answers. With no ionmac running the move stops at the window's edge, as it does without one.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::layout::Direction;

/// How long ionmac has to answer. tutti's TUI gives `rtty cli` half a second for the whole move
/// and steps a tab of its own after that, so a late answer has to count as none.
const TIMEOUT: Duration = Duration::from_millis(100);

/// Asks ionmac to move the focus `direction` of the focused frame: whether it did. Not when no
/// ionmac listens, when it answers late, or when nothing lies that way.
pub fn go(direction: Direction) -> bool {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    ask(&std::env::temp_dir().join(format!("ionmac-{uid}/ionmac.sock")), direction)
}

/// The asking is on a thread of its own, so that the wait ends on time whatever holds the socket
/// up: a connection that is not taken, or an answer that comes a byte at a time.
fn ask(socket: &Path, direction: Direction) -> bool {
    let socket = socket.to_owned();
    let (answered, answer) = mpsc::channel();
    thread::spawn(move || {
        let asked = || -> io::Result<String> {
            let mut stream = UnixStream::connect(socket)?;
            stream.set_read_timeout(Some(TIMEOUT))?;
            stream.set_write_timeout(Some(TIMEOUT))?;
            writeln!(stream, r#"{{"run":"go {}"}}"#, direction.name())?;
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line)?;
            Ok(line)
        };
        let _ = answered.send(asked());
    });
    let line = answer.recv_timeout(TIMEOUT).ok().and_then(Result::ok);
    let answer = line.and_then(|line| serde_json::from_str::<serde_json::Value>(&line).ok());
    answer.is_some_and(|answer| answer["ok"] == true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::net::UnixListener;
    use std::time::Instant;

    /// A stand-in for ionmac: moves left, says the screen ends to the right, and says nothing
    /// to a move down.
    #[test]
    fn a_move_is_handed_on_and_only_an_ok_in_time_counts() {
        let dir = std::env::temp_dir().join(format!("rtty-ionmac-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("ionmac.sock");
        assert!(!ask(&socket, Direction::Left), "nothing listens");
        let listener = UnixListener::bind(&socket).unwrap();
        thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                match line.trim() {
                    r#"{"run":"go left"}"# => writeln!(&stream, r#"{{"ok":true}}"#).unwrap(),
                    r#"{"run":"go right"}"# => {
                        writeln!(&stream, r#"{{"error":"the edge of the screen"}}"#).unwrap()
                    }
                    _ => held.push(stream),
                }
            }
        });
        assert!(ask(&socket, Direction::Left));
        assert!(!ask(&socket, Direction::Right));
        let asked = Instant::now();
        assert!(!ask(&socket, Direction::Down), "no answer");
        assert!(asked.elapsed() < TIMEOUT * 2, "{:?}", asked.elapsed());
        fs::remove_dir_all(dir).unwrap();
    }
}
