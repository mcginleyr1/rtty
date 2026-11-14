//! `rtty cli …`: asks the rtty a pane runs in (`$RTTY_SOCKET`) and never opens a window. The
//! commands, their output and their exit codes follow `tutti pane …`, so an editor plugin speaks
//! to either the same way.

use std::path::PathBuf;
use std::process::ExitCode;

use gpui::{Bounds, point, px, size};

use crate::control::{self, Request, Response};
use crate::layout::{Amount, Direction, PaneId};
use crate::tutti;

pub const USAGE: &str = "usage: rtty cli [--socket <path>] <command>
       rtty cli list
       rtty cli window new [--cwd <dir> | --tutti <pane> [-s <session>]]
                           [--frame <x>,<y>,<width>,<height> [--display <uuid>]]
       rtty cli pane focus <pane>
       rtty cli pane focus --direction <left|right|up|down> [--from <pane>] [--wrap]
       rtty cli pane resize --direction <left|right|up|down> [--amount <ratio> | --cells <n>]
                            [--from <pane>]
       rtty cli pane split <pane> <right|down|left|up>
       rtty cli pane zoom [<pane>]
       rtty cli tutti attach <pane> [-s <session>] [--split <right|down|left|up> | --tab]
                             [--from <pane>]
       rtty cli tutti open [-s <session>]
--from defaults to $RTTY_PANE, else the focused pane; --amount to 0.05 of the split; -s to
$TUTTI_SESSION, else tutti; tutti attach opens a new tab unless told to split; tutti open opens
a window that is the whole session. --socket names the rtty to ask from outside a pane
($TMPDIR/rtty-<uid>/<pid>.sock); without it, $RTTY_SOCKET, else the one rtty running. window new
prints the window as list does, and with --tutti shows that tutti pane in place of a shell;
--frame is in points from the display's top left. pane focus --direction at the edge of a window
goes on to ionmac's frame that way when ionmac runs, and exits 1 when nothing took the move.";

/// tutti's step for Alt+h/j/k/l and `pane resize`.
pub const RESIZE_STEP: Amount = Amount::Fraction(0.05);

pub fn run(mut args: Vec<String>) -> ExitCode {
    let (here, session) = (std::env::var("RTTY_PANE").ok(), std::env::var("TUTTI_SESSION").ok());
    // Which rtty: the one `--socket` names, else the pane's own, else the only one running.
    let own = std::env::var_os("RTTY_SOCKET").map(PathBuf::from);
    let socket = match args.first().map(String::as_str) {
        Some("--socket") if args.len() > 1 => args.drain(..2).next_back().map(PathBuf::from),
        _ => own.clone(),
    };
    // `$RTTY_PANE` is a pane of the rtty this one runs in, and means nothing to another.
    let here = here.filter(|_| socket == own);
    let request = match parse(&args, here, session) {
        Ok(request) => request,
        Err(message) => {
            eprintln!("rtty cli: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Some(socket) = socket.or_else(control::lone_socket) else {
        eprintln!("error: not in an rtty pane, and not one rtty running: name one with --socket");
        return ExitCode::FAILURE;
    };
    match control::ask(&socket, &request) {
        Ok(response) => report(response),
        Err(err) => {
            eprintln!("error: {}: {err}", socket.display());
            ExitCode::FAILURE
        }
    }
}

/// What the answer prints, and the exit code: 1 at an edge and for an error, as tutti's.
fn report(response: Response) -> ExitCode {
    match response {
        Response::Done => ExitCode::SUCCESS,
        Response::Pane(pane) => {
            println!("{pane}");
            ExitCode::SUCCESS
        }
        Response::Windows(windows) => {
            println!("{}", serde_json::json!({ "windows": windows }));
            ExitCode::SUCCESS
        }
        Response::Edge { from, direction } => {
            eprintln!("no pane {} of {from}", direction.name());
            ExitCode::FAILURE
        }
        Response::Error(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The request `args` (after `cli`) make; `here` is `$RTTY_PANE`, the default `--from`, and
/// `tutti_session` `$TUTTI_SESSION`, the default `-s`.
fn parse(
    args: &[String],
    here: Option<String>,
    tutti_session: Option<String>,
) -> Result<Request, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let (command, rest) = match words.as_slice() {
        ["list"] => return Ok(Request::List),
        ["pane", command, rest @ ..] => (*command, rest),
        ["tutti", "attach", rest @ ..] => ("tutti attach", rest),
        ["tutti", "open", rest @ ..] => ("tutti open", rest),
        ["window", "new", rest @ ..] => ("window new", rest),
        _ => {
            let commands = "`list`, `window new`, `pane <command>`, `tutti attach` or `tutti open`";
            return Err(format!("expected {commands}"));
        }
    };
    let mut positional = Vec::new();
    let (mut direction, mut from, mut amount, mut wrap) = (None, None, None, false);
    let (mut session, mut split, mut tab) = (None, None, false);
    let (mut cwd, mut frame, mut display, mut shown) = (None, None, None, None);
    let mut rest = rest.iter();
    while let Some(&word) = rest.next() {
        let mut value = || rest.next().copied().ok_or(format!("{word} needs a value"));
        match word {
            // As the asker means it: the rtty asked has a working directory of its own.
            "--cwd" => {
                let dir = value()?;
                let whole = std::path::absolute(dir).map_err(|err| format!("--cwd {dir}: {err}"));
                cwd = Some(whole?);
            }
            "--display" => display = Some(value()?.to_owned()),
            "--frame" => {
                let text = value()?;
                let numbers: Result<Vec<f32>, _> = text.split(',').map(str::parse).collect();
                let Ok([x, y, width, height]) = numbers.as_deref() else {
                    return Err(format!("--frame {text}: not x,y,width,height"));
                };
                frame = Some(Bounds::new(point(px(*x), px(*y)), size(px(*width), px(*height))));
            }
            "--direction" => direction = Some(direction_named(value()?)?),
            "--from" => from = Some(pane_numbered(value()?)?),
            "-s" | "--session" => session = Some(value()?.to_owned()),
            "--split" => split = Some(direction_named(value()?)?),
            "--tutti" => {
                let pane = value()?;
                shown = Some(pane.parse().map_err(|_| format!("{pane}: not a pane number"))?);
            }
            "--tab" => tab = true,
            "--amount" | "--cells" if amount.is_some() => {
                return Err("--amount and --cells: one or the other".into());
            }
            "--amount" => {
                let ratio = value()?;
                let ratio: f32 =
                    ratio.parse().map_err(|_| format!("--amount {ratio}: not a number"))?;
                if !(ratio > 0. && ratio < 1.) {
                    return Err(format!("--amount {ratio}: not a fraction of the split"));
                }
                amount = Some(Amount::Fraction(ratio));
            }
            "--cells" => {
                let cells = value()?;
                let cells = cells.parse().ok().filter(|&cells: &u16| cells > 0);
                amount = Some(Amount::Cells(cells.ok_or("--cells: not a number of cells")?));
            }
            "--wrap" => wrap = true,
            _ if word.starts_with("--") => return Err(format!("unknown option {word}")),
            _ => positional.push(word),
        }
    }
    let here = here.as_deref().map(pane_numbered).transpose()?;
    let from = from.or(here);
    let session = session.or(tutti_session).unwrap_or_else(|| "tutti".into());
    let socket = || tutti::socket_path(&session);
    let pane = |index: usize| positional.get(index).map(|&word| pane_numbered(word)).transpose();
    let only = |count: usize, request: Request| {
        if positional.len() > count {
            Err(format!("unexpected {}", positional[count]))
        } else {
            Ok(request)
        }
    };
    match (command, direction) {
        ("focus", Some(direction)) => only(0, Request::FocusToward { from, direction, wrap }),
        ("focus", None) => {
            let pane = pane(0)?.ok_or("focus needs a pane or --direction")?;
            only(1, Request::Focus { pane })
        }
        ("resize", Some(direction)) => {
            let amount = amount.unwrap_or(RESIZE_STEP);
            only(0, Request::Resize { from, direction, amount })
        }
        ("resize", None) => Err("resize needs --direction".into()),
        ("split", _) => {
            let (Some(_), Some(&direction)) = (positional.first(), positional.get(1)) else {
                return Err("split needs a pane and a direction".into());
            };
            only(2, Request::Split { pane: pane(0)?, direction: direction_named(direction)? })
        }
        ("zoom", _) => only(1, Request::Zoom { pane: pane(0)?.or(here) }),
        ("tutti attach", _) if split.is_some() && tab => {
            Err("--split and --tab: one or the other".into())
        }
        ("tutti attach", _) => {
            let pane = positional.first().ok_or("tutti attach needs a pane")?;
            let pane = pane.parse().map_err(|_| format!("{pane}: not a pane number"))?;
            only(1, Request::TuttiAttach { socket: socket(), pane, from, split })
        }
        ("tutti open", _) => only(0, Request::TuttiOpen { socket: socket() }),
        ("window new", _) if cwd.is_some() && shown.is_some() => {
            Err("--cwd and --tutti: one or the other".into())
        }
        ("window new", _) => {
            let tutti = shown.map(|pane: u64| (socket(), pane));
            only(0, Request::NewWindow { cwd, frame, display, tutti })
        }
        _ => Err(format!("unknown command pane {command}")),
    }
}

fn direction_named(word: &str) -> Result<Direction, String> {
    Direction::ALL
        .into_iter()
        .find(|direction| direction.name() == word)
        .ok_or(format!("{word}: not left, right, up or down"))
}

fn pane_numbered(word: &str) -> Result<PaneId, String> {
    word.parse().map_err(|_| format!("{word}: not a pane number"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(line: &str, here: Option<&str>) -> Result<Request, String> {
        let args: Vec<String> = line.split_whitespace().map(String::from).collect();
        parse(&args, here.map(String::from), None)
    }

    #[test]
    fn commands_read_as_tuttis_do() {
        let left = Direction::Left;
        assert_eq!(parsed("list", None), Ok(Request::List));
        assert_eq!(parsed("pane focus 4", Some("7")), Ok(Request::Focus { pane: 4 }));
        assert_eq!(
            parsed("pane focus --direction left --from 3 --wrap", Some("7")),
            Ok(Request::FocusToward { from: Some(3), direction: left, wrap: true })
        );
        assert_eq!(
            parsed("pane focus --direction left", Some("7")),
            Ok(Request::FocusToward { from: Some(7), direction: left, wrap: false })
        );
        assert_eq!(
            parsed("pane focus --direction left", None),
            Ok(Request::FocusToward { from: None, direction: left, wrap: false })
        );
        let (up, down) = (Direction::Up, Direction::Down);
        assert_eq!(
            parsed("pane resize --direction up", Some("7")),
            Ok(Request::Resize { from: Some(7), direction: up, amount: Amount::Fraction(0.05) })
        );
        assert_eq!(
            parsed("pane resize --amount 0.2 --direction down --from 2", None),
            Ok(Request::Resize { from: Some(2), direction: down, amount: Amount::Fraction(0.2) })
        );
        assert_eq!(
            parsed("pane resize --direction up --cells 3", None),
            Ok(Request::Resize { from: None, direction: up, amount: Amount::Cells(3) })
        );
        assert_eq!(
            parsed("pane split 5 down", None),
            Ok(Request::Split { pane: Some(5), direction: down })
        );
        assert_eq!(
            parsed("pane split 5 up", None),
            Ok(Request::Split { pane: Some(5), direction: up })
        );
        assert_eq!(parsed("pane zoom", Some("7")), Ok(Request::Zoom { pane: Some(7) }));
        assert_eq!(parsed("pane zoom 2", Some("7")), Ok(Request::Zoom { pane: Some(2) }));
    }

    /// `tutti attach` names a pane as tutti does, in the session `-s` names, else the one tutti
    /// would take (`$TUTTI_SESSION`, else `tutti`), and finds that session's socket here.
    #[test]
    fn tutti_attach_names_a_pane_of_a_session() {
        let attach = |line: &str, session: Option<&str>| {
            let args: Vec<String> = line.split_whitespace().map(String::from).collect();
            parse(&args, Some("7".into()), session.map(String::from))
        };
        let socket = tutti::socket_path;
        assert_eq!(
            attach("tutti attach 3 -s work --split down", Some("outer")),
            Ok(Request::TuttiAttach {
                socket: socket("work"),
                pane: 3,
                from: Some(7),
                split: Some(Direction::Down)
            })
        );
        assert_eq!(
            attach("tutti attach 3", Some("outer")),
            Ok(Request::TuttiAttach {
                socket: socket("outer"),
                pane: 3,
                from: Some(7),
                split: None
            })
        );
        assert_eq!(
            attach("tutti attach --tab 3 --from 2", None),
            Ok(Request::TuttiAttach {
                socket: socket("tutti"),
                pane: 3,
                from: Some(2),
                split: None
            })
        );
        assert_eq!(
            attach("tutti open -s work", Some("outer")),
            Ok(Request::TuttiOpen { socket: socket("work") })
        );
        assert_eq!(attach("tutti open", None), Ok(Request::TuttiOpen { socket: socket("tutti") }));
        let error = |line| attach(line, None).unwrap_err();
        assert!(error("tutti open 3").contains("unexpected 3"));
        assert!(error("tutti attach").contains("needs a pane"));
        assert!(error("tutti attach x").contains("x: not a pane number"));
        assert!(error("tutti attach 3 4").contains("unexpected 4"));
        assert!(error("tutti attach 3 --tab --split left").contains("one or the other"));
        assert!(error("tutti attach 3 -s").contains("-s needs a value"));
    }

    /// A window manager asks for a window from outside any pane: in a directory, at a frame.
    #[test]
    fn window_new_takes_a_directory_and_a_frame() {
        let bare = Request::NewWindow { cwd: None, frame: None, display: None, tutti: None };
        assert_eq!(parsed("window new", Some("7")), Ok(bare));
        let frame = Some(Bounds::new(point(px(10.), px(38.5)), size(px(800.), px(600.))));
        let placed =
            Request::NewWindow { cwd: Some("/tmp".into()), frame, display: None, tutti: None };
        assert_eq!(parsed("window new --cwd /tmp --frame 10,38.5,800,600", None), Ok(placed));
        // A tutti pane in place of the shell, of the session `-s` names as `tutti attach` reads it.
        let tutti = Some((tutti::socket_path("work"), 3));
        let pane = Request::NewWindow { cwd: None, frame, display: None, tutti };
        assert_eq!(parsed("window new --tutti 3 -s work --frame 10,38.5,800,600", None), Ok(pane));
        let error = |line| parsed(line, None).unwrap_err();
        assert!(error("window new --tutti 3 --cwd /tmp").contains("one or the other"));
        assert!(error("window new --tutti x").contains("x: not a pane number"));
        assert!(error("window new --frame 10,20,800").contains("not x,y,width,height"));
        assert!(error("window new --frame a,b,c,d").contains("not x,y,width,height"));
        assert!(error("window new 3").contains("unexpected 3"));
        assert!(error("window").contains("expected `list`, `window new`"));
    }

    #[test]
    fn mistakes_say_what_is_wrong() {
        let error = |line| parsed(line, None).unwrap_err();
        assert!(error("pane focus --direction north").contains("north: not left"));
        assert!(error("pane focus --direction").contains("--direction needs a value"));
        assert!(error("pane focus").contains("needs a pane or --direction"));
        assert!(error("pane focus x").contains("x: not a pane number"));
        assert!(error("pane resize --direction up --amount 2").contains("not a fraction"));
        assert!(error("pane resize").contains("needs --direction"));
        assert!(error("pane resize --direction up --cells 0").contains("not a number of cells"));
        assert!(error("pane resize --cells 2 --amount 0.1").contains("one or the other"));
        assert!(error("pane split 3").contains("needs a pane and a direction"));
        assert!(error("pane split 3 sideways").contains("sideways: not left"));
        assert!(error("pane focus 3 4").contains("unexpected 4"));
        assert!(error("pane focus --bogus").contains("unknown option --bogus"));
        assert!(error("pane dance").contains("unknown command pane dance"));
        assert!(error("panes").contains("expected `list`"));
        assert!(parsed("list", Some("seven")).is_ok(), "list ignores $RTTY_PANE");
        assert!(parsed("pane zoom", Some("seven")).unwrap_err().contains("seven"));
    }
}
