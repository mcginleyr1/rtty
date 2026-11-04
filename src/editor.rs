//! Editing a file at a line and column: in a Neovim that runs rtty's plugin, through its server
//! (msgpack-rpc on a Unix socket, `nvim`), else in a new pane running the `editor` template or
//! the shell's `$EDITOR` (`command`).

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use portable_pty::CommandBuilder;
use regex::bytes::{Captures, Regex};

use crate::links::Location;

/// How long the Neovims of a tab have, together, to open a file before it opens in a pane
/// instead. The deadline goes with each request, and the plugin's `open` refuses a request it
/// reads past it, so a Neovim that was busy never opens the file as well as the pane.
pub const NVIM_WAIT: Duration = Duration::from_millis(500);

/// How long past the deadline an answer may still arrive: from a Neovim taking the request in
/// time to rtty reading its answer.
pub const NVIM_GRACE: Duration = Duration::from_millis(250);

/// What the plugin's `open` is called with, through `nvim_exec_lua`.
const OPEN: &str = "require('rtty').open(...)";

/// Whether a pane whose foreground program is `program` may have files opened through `server`,
/// the socket its user var names: the program is `nvim`, and the socket is this user's. Any
/// output, an ssh session's included, can set the var.
pub fn trusted(server: &Path, program: &str) -> bool {
    // SAFETY: getuid cannot fail.
    let me = unsafe { libc::getuid() };
    let mine =
        fs::metadata(server).is_ok_and(|meta| meta.file_type().is_socket() && meta.uid() == me);
    Path::new(program).file_name().is_some_and(|name| name == "nvim") && mine
}

/// Bytes, so a path that is not UTF-8 is filled in as it is.
static PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{(path|line|col)\}").expect("a regex"));

/// What a new pane runs to edit `at`: `template` with `{path}`, `{line}` and `{col}` filled in,
/// else `$EDITOR +line path` (`vi` with no EDITOR). The user's login shell runs it, interactive,
/// so it finds what the dotfiles set up (the PATH, EDITOR), which an app started from Finder
/// lacks; the arguments go to it apart from the script, so no path needs quoting.
pub fn command(template: Option<&[String]>, at: &Location) -> CommandBuilder {
    let shell = CommandBuilder::new_default_prog().get_shell();
    let fish = shell.rsplit('/').next() == Some("fish");
    let (path, line, col) = (at.path.as_os_str(), at.line.to_string(), at.col.to_string());
    let fill = |arg: &String| {
        let value = |found: &Captures| match &found[1] {
            b"path" => path.as_bytes().to_vec(),
            b"line" => line.clone().into_bytes(),
            _ => col.clone().into_bytes(),
        };
        OsString::from_vec(PLACEHOLDER.replace_all(arg.as_bytes(), value).into_owned())
    };
    let args = match template {
        Some(template) => template.iter().map(fill).collect(),
        None => vec![format!("+{line}").into(), path.to_owned()],
    };
    let script = match (template.is_some(), fish) {
        (true, false) => r#"exec "$@""#,
        (true, true) => "exec $argv",
        (false, false) => r#"eval exec "${EDITOR:-vi}" '"$@"'"#,
        (false, true) => {
            "set -q EDITOR; or set EDITOR vi; eval exec $EDITOR (string escape -- $argv)"
        }
    };
    // sh, bash and zsh take the first argument after the script as $0.
    let zero = (!fish).then(|| "rtty".into());
    let argv = [shell.into(), "-l".into(), "-i".into(), "-c".into(), script.into()];
    CommandBuilder::from_argv(argv.into_iter().chain(zero).chain(args).collect())
}

/// Has the Neovim serving `server` open `at` through rtty's plugin, if it takes the request by
/// `deadline`, and waits for its answer until `NVIM_GRACE` past that: an error when it is not
/// there, refuses (it read the request late), or does not answer in time. Blocks: for a
/// background thread. std has no connect timeout for a Unix socket, which connects at once
/// unless its listener's backlog is full; the caller stops waiting at the deadline, and the
/// request it carries keeps a late one from opening anything.
pub fn nvim(server: &Path, at: &Location, deadline: SystemTime) -> io::Result<()> {
    let left = deadline.duration_since(SystemTime::now()).ok().filter(|left| !left.is_zero());
    let left = left.ok_or(io::ErrorKind::TimedOut)?;
    let mut stream = UnixStream::connect(server)?;
    stream.set_write_timeout(Some(left))?;
    stream.set_read_timeout(Some(left + NVIM_GRACE))?;
    stream.write_all(&request(at, deadline))?;
    let mut answer = [0; 4];
    stream.read_exact(&mut answer)?;
    // [1, 0, nil, …]: the answer to request 0, with no error.
    match answer {
        [0x94, 1, 0, 0xc0] => Ok(()),
        _ => Err(io::Error::other(format!("Neovim answered {answer:02x?}"))),
    }
}

/// The msgpack-rpc request `[0, 0, "nvim_exec_lua", [OPEN, [path, line, col, deadline]]]`, the
/// deadline in milliseconds since the epoch: strings as str32, numbers as uint32 and uint64.
fn request(at: &Location, deadline: SystemTime) -> Vec<u8> {
    let string = |out: &mut Vec<u8>, bytes: &[u8]| {
        out.push(0xdb);
        out.extend((bytes.len() as u32).to_be_bytes());
        out.extend(bytes);
    };
    let mut out = vec![0x94, 0, 0];
    string(&mut out, b"nvim_exec_lua");
    out.push(0x92);
    string(&mut out, OPEN.as_bytes());
    out.push(0x94);
    string(&mut out, at.path.as_os_str().as_bytes());
    for number in [at.line, at.col] {
        out.push(0xce);
        out.extend(number.to_be_bytes());
    }
    let millis = deadline.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    out.push(0xcf);
    out.extend(millis.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::thread;

    use super::*;

    fn at(path: &str, line: u32, col: u32) -> Location {
        Location { path: path.into(), line, col }
    }

    fn argv(command: &CommandBuilder) -> Vec<String> {
        command.get_argv().iter().map(|arg| arg.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_template_is_filled_in_and_run_by_the_login_shell() {
        let template = ["nvim", "+call cursor({line}, {col})", "{path}"].map(String::from);
        let command = command(Some(&template), &at("/src/{line} x.rs", 12, 5));
        let argv = argv(&command);
        let shell = CommandBuilder::new_default_prog().get_shell();
        assert_eq!(argv[..4], [shell, "-l".into(), "-i".into(), "-c".into()]);
        if !shell_is_fish() {
            assert_eq!(
                argv[4..],
                [r#"exec "$@""#, "rtty", "nvim", "+call cursor(12, 5)", "/src/{line} x.rs"]
            );
        }
    }

    #[test]
    fn with_no_template_the_shell_runs_its_editor_at_the_line() {
        let argv = argv(&command(None, &at("/src/a b.rs", 7, 3)));
        if !shell_is_fish() {
            let script = r#"eval exec "${EDITOR:-vi}" '"$@"'"#;
            assert_eq!(argv[4..], [script, "rtty", "+7", "/src/a b.rs"]);
        }
    }

    fn shell_is_fish() -> bool {
        CommandBuilder::new_default_prog().get_shell().ends_with("/fish")
    }

    #[test]
    fn a_path_that_is_not_utf_8_reaches_the_editor_byte_for_byte() {
        let path = PathBuf::from(OsString::from_vec(b"/src/caf\xe9 {col}.rs".to_vec()));
        let at = Location { path: path.clone(), line: 2, col: 9 };
        let template = ["vi", "{path}:{line}:{col}"].map(String::from);
        let filled = command(Some(&template), &at).get_argv().last().unwrap().clone();
        assert_eq!(filled.as_bytes(), b"/src/caf\xe9 {col}.rs:2:9");
        assert_eq!(command(None, &at).get_argv().last().unwrap(), path.as_os_str());
    }

    /// What sh makes of the default command: the editor's argv, a path with a space and a quote
    /// in it whole, and an EDITOR with arguments split into them.
    #[test]
    fn the_shell_hands_the_editor_its_arguments_whole() {
        let command = command(None, &at("/src/it's a.rs", 7, 3));
        let argv = argv(&command);
        let run = |editor: Option<&str>| {
            let mut sh = std::process::Command::new("/bin/sh");
            sh.args(["-c", &argv[4].replace("exec ", r#"printf "'[%s]'" "#)]).args(&argv[5..]);
            sh.env_remove("EDITOR");
            if let Some(editor) = editor {
                sh.env("EDITOR", editor);
            }
            String::from_utf8(sh.output().unwrap().stdout).unwrap()
        };
        if !shell_is_fish() {
            assert_eq!(run(Some("code -w")), "[code][-w][+7][/src/it's a.rs]");
            assert_eq!(run(None), "[vi][+7][/src/it's a.rs]");
        }
    }

    #[test]
    fn the_request_calls_the_plugins_open_with_the_place_and_the_deadline() {
        let mut expected = vec![0x94, 0, 0, 0xdb, 0, 0, 0, 13];
        expected.extend(b"nvim_exec_lua");
        expected.extend([0x92, 0xdb, 0, 0, 0, 25]);
        expected.extend(b"require('rtty').open(...)");
        expected.extend([0x94, 0xdb, 0, 0, 0, 7]);
        expected.extend(b"/x/a.rs");
        expected.extend([0xce, 0, 0, 1, 0x2c, 0xce, 0, 0, 0, 9, 0xcf]);
        expected.extend(1_700_000_000_123_u64.to_be_bytes());
        let deadline = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        assert_eq!(request(&at("/x/a.rs", 300, 9), deadline), expected);
    }

    /// A socket in a scratch directory.
    fn socket(test: &str) -> (PathBuf, UnixListener) {
        let dir = std::env::temp_dir().join(format!("rtty-nvim-{test}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s");
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        (socket, listener)
    }

    /// A Neovim stand-in that reads one request and answers `answer`, or nothing.
    fn server(test: &str, answer: Option<&'static [u8]>) -> PathBuf {
        let (socket, listener) = socket(test);
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0; request(&at("/x/a.rs", 1, 1), UNIX_EPOCH).len()];
            stream.read_exact(&mut request).unwrap();
            if let Some(answer) = answer {
                stream.write_all(answer).unwrap();
            }
            thread::sleep(Duration::from_secs(1));
        });
        socket
    }

    #[test]
    fn neovim_opens_it_when_it_answers_without_an_error_by_the_deadline() {
        let open = |server: &Path, wait: u64| {
            let deadline = SystemTime::now() + Duration::from_millis(wait);
            nvim(server, &at("/x/a.rs", 1, 1), deadline)
        };
        assert!(open(&server("ok", Some(&[0x94, 1, 0, 0xc0, 0xc0])), 500).is_ok());
        let failed: &[u8] = &[0x94, 1, 0, 0x92, 0, 0xa1, b'x', 0xc0];
        assert!(open(&server("failed", Some(failed)), 500).is_err());
        let started = std::time::Instant::now();
        assert!(open(&server("silent", None), 100).is_err());
        assert!(started.elapsed() < Duration::from_millis(100) + NVIM_GRACE * 2, "waited on");
        assert!(open(Path::new("/nonexistent/nvim.0"), 500).is_err());
        let (late, _listener) = socket("late");
        let past = open(&late, 0).unwrap_err();
        assert_eq!(past.kind(), io::ErrorKind::TimedOut, "past the deadline it asks nothing");
    }

    #[test]
    fn only_nvim_in_the_foreground_and_a_socket_of_this_users_are_trusted() {
        let (socket, _listener) = socket("trusted");
        assert!(trusted(&socket, "nvim") && trusted(&socket, "/opt/bin/nvim"));
        for program in ["vim", "nvim-qt", "bash", ""] {
            assert!(!trusted(&socket, program), "{program}");
        }
        let file = socket.with_file_name("file");
        std::fs::write(&file, b"").unwrap();
        assert!(!trusted(&file, "nvim"), "a file is no server");
        assert!(!trusted(Path::new("/nonexistent/nvim.0"), "nvim"));
    }
}
