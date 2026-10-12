//! `rtty cli …`: asks the rtty a pane runs in (`$RTTY_SOCKET`) and never opens a window. The
//! commands, their output and their exit codes follow `tutti pane …`, so an editor plugin speaks
//! to either the same way.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::control::{self, Request, Response};
use crate::layout::{Amount, Direction, PaneId};

pub const USAGE: &str = "usage: rtty cli list
       rtty cli pane focus <pane>
       rtty cli pane focus --direction <left|right|up|down> [--from <pane>] [--wrap]
       rtty cli pane resize --direction <left|right|up|down> [--amount <ratio> | --cells <n>]
                            [--from <pane>]
       rtty cli pane split <pane> <right|down|left|up>
       rtty cli pane zoom [<pane>]
--from defaults to $RTTY_PANE, else the focused pane; --amount to 0.05 of the split.";

/// tutti's step for Alt+h/j/k/l and `pane resize`.
pub const RESIZE_STEP: Amount = Amount::Fraction(0.05);

pub fn run(args: Vec<String>) -> ExitCode {
    let request = match parse(&args, std::env::var("RTTY_PANE").ok()) {
        Ok(request) => request,
        Err(message) => {
            eprintln!("rtty cli: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Some(socket) = std::env::var_os("RTTY_SOCKET").map(PathBuf::from) else {
        eprintln!("error: RTTY_SOCKET is not set: not in an rtty pane");
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

/// The request `args` (after `cli`) make; `here` is `$RTTY_PANE`, the default `--from`.
fn parse(args: &[String], here: Option<String>) -> Result<Request, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let (command, rest) = match words.as_slice() {
        ["list"] => return Ok(Request::List),
        ["pane", command, rest @ ..] => (*command, rest),
        _ => return Err("expected `list` or `pane <command>`".into()),
    };
    let mut positional = Vec::new();
    let (mut direction, mut from, mut amount, mut wrap) = (None, None, None, false);
    let mut rest = rest.iter();
    while let Some(&word) = rest.next() {
        let mut value = || rest.next().copied().ok_or(format!("{word} needs a value"));
        match word {
            "--direction" => direction = Some(direction_named(value()?)?),
            "--from" => from = Some(pane_numbered(value()?)?),
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
        parse(&args, here.map(String::from))
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
