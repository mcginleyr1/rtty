//! Editing a file at a line and column: in a Neovim that runs rtty's plugin, through its server
//! (msgpack-rpc on a Unix socket, `nvim`), else in a new pane running the `editor` template or
//! the shell's `$EDITOR` (`command`).

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use portable_pty::CommandBuilder;
use regex::bytes::{Captures, Regex};

use crate::links::Location;

/// How long a Neovim may take to answer before the file opens in a pane instead.
const NVIM_TIMEOUT: Duration = Duration::from_millis(300);

/// What the plugin's `open` is called with, through `nvim_exec_lua`.
const OPEN: &str = "require('rtty').open(...)";

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

/// Has the Neovim serving `server` open `at`, through rtty's plugin, and waits for its answer:
/// an error when it is not there, does not answer in time, or fails.
pub fn nvim(server: &Path, at: &Location) -> io::Result<()> {
    let mut stream = UnixStream::connect(server)?;
    stream.set_read_timeout(Some(NVIM_TIMEOUT))?;
    stream.write_all(&request(at))?;
    let mut answer = [0; 4];
    stream.read_exact(&mut answer)?;
    // [1, 0, nil, …]: the answer to request 0, with no error.
    match answer {
        [0x94, 1, 0, 0xc0] => Ok(()),
        _ => Err(io::Error::other(format!("Neovim answered {answer:02x?}"))),
    }
}

/// The msgpack-rpc request `[0, 0, "nvim_exec_lua", [OPEN, [path, line, col]]]`: strings as
/// str32, numbers as uint32.
fn request(at: &Location) -> Vec<u8> {
    let string = |out: &mut Vec<u8>, bytes: &[u8]| {
        out.push(0xdb);
        out.extend((bytes.len() as u32).to_be_bytes());
        out.extend(bytes);
    };
    let mut out = vec![0x94, 0, 0];
    string(&mut out, b"nvim_exec_lua");
    out.push(0x92);
    string(&mut out, OPEN.as_bytes());
    out.push(0x93);
    string(&mut out, at.path.as_os_str().as_bytes());
    for number in [at.line, at.col] {
        out.push(0xce);
        out.extend(number.to_be_bytes());
    }
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
    fn the_request_calls_the_plugins_open_with_the_place() {
        let mut expected = vec![0x94, 0, 0, 0xdb, 0, 0, 0, 13];
        expected.extend(b"nvim_exec_lua");
        expected.extend([0x92, 0xdb, 0, 0, 0, 25]);
        expected.extend(b"require('rtty').open(...)");
        expected.extend([0x93, 0xdb, 0, 0, 0, 7]);
        expected.extend(b"/x/a.rs");
        expected.extend([0xce, 0, 0, 1, 0x2c, 0xce, 0, 0, 0, 9]);
        assert_eq!(request(&at("/x/a.rs", 300, 9)), expected);
    }

    /// A Neovim stand-in on a socket in a scratch directory that reads one request and answers
    /// `answer`, or nothing.
    fn server(test: &str, answer: Option<&'static [u8]>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rtty-nvim-{test}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s");
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0; request(&at("/x/a.rs", 1, 1)).len()];
            stream.read_exact(&mut request).unwrap();
            if let Some(answer) = answer {
                stream.write_all(answer).unwrap();
            }
            thread::sleep(Duration::from_secs(1));
        });
        socket
    }

    #[test]
    fn neovim_opens_it_when_it_answers_without_an_error() {
        assert!(nvim(&server("ok", Some(&[0x94, 1, 0, 0xc0, 0xc0])), &at("/x/a.rs", 1, 1)).is_ok());
        let failed: &[u8] = &[0x94, 1, 0, 0x92, 0, 0xa1, b'x', 0xc0];
        assert!(nvim(&server("failed", Some(failed)), &at("/x/a.rs", 1, 1)).is_err());
        assert!(nvim(&server("silent", None), &at("/x/a.rs", 1, 1)).is_err());
        assert!(nvim(Path::new("/nonexistent/nvim.0"), &at("/x/a.rs", 1, 1)).is_err());
    }
}
