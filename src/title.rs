//! What a pane is called in its tab and the window's title: the title its program set (OSC 0/2),
//! else the program in its foreground, else, with the shell at a prompt, the directory it is in.
//! The process facts come from the pane (`Pane::foreground`); the choosing is here, unit tested.

use std::path::{Path, PathBuf};

/// The process in a pane's foreground: its pty's process group leader.
#[derive(Clone, Debug, PartialEq)]
pub struct Foreground {
    /// It is the pane's login shell, so the shell is at a prompt.
    pub shell: bool,
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
}

/// A pane's title and the directory it is in, when known.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Title {
    pub text: String,
    pub dir: Option<PathBuf>,
}

/// `program` is the title the program set ("" for none) and `reported` the directory the shell
/// last reported (OSC 7), which beats the foreground process's own.
pub fn title(
    program: &str,
    reported: Option<&Path>,
    foreground: Option<&Foreground>,
    home: Option<&Path>,
) -> Title {
    let dir = reported.or(foreground.and_then(|process| process.cwd.as_deref()));
    let command = foreground.filter(|process| !process.shell).map(|process| command(&process.argv));
    let text = if !program.is_empty() {
        program.to_owned()
    } else if let Some(command) = command.filter(|command| !command.is_empty()) {
        command
    } else {
        dir.map_or("rtty".into(), |dir| shorten(dir, home))
    };
    Title { text, dir: dir.map(Path::to_path_buf) }
}

/// The command line as typed: the program's name without its directory (or a login shell's
/// `-`), then its arguments.
fn command(argv: &[String]) -> String {
    let Some((program, args)) = argv.split_first() else { return String::new() };
    let name = program.rsplit('/').next().unwrap_or(program);
    let name = name.strip_prefix('-').unwrap_or(name);
    std::iter::once(name).chain(args.iter().map(String::as_str)).collect::<Vec<_>>().join(" ")
}

/// `dir` with the home directory written `~`.
pub fn shorten(dir: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| dir.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => dir.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> Option<&'static Path> {
        Some(Path::new("/Users/me"))
    }

    fn process(shell: bool, argv: &[&str], cwd: &str) -> Foreground {
        Foreground {
            shell,
            argv: argv.iter().map(|arg| arg.to_string()).collect(),
            cwd: Some(PathBuf::from(cwd)),
        }
    }

    fn text(program: &str, reported: Option<&str>, foreground: Option<&Foreground>) -> String {
        title(program, reported.map(Path::new), foreground, home()).text
    }

    #[test]
    fn the_programs_own_title_comes_first() {
        let cargo = process(false, &["cargo", "run"], "/Users/me/project");
        assert_eq!(text("vim: notes.md", None, Some(&cargo)), "vim: notes.md");
        let shell = process(true, &["-zsh"], "/Users/me/project");
        assert_eq!(text("set by the prompt", Some("/tmp"), Some(&shell)), "set by the prompt");
    }

    #[test]
    fn a_program_in_the_foreground_is_its_command_line() {
        let cargo = process(false, &["/usr/local/bin/cargo", "run", "--release"], "/Users/me/x");
        assert_eq!(text("", None, Some(&cargo)), "cargo run --release");
        // Another shell, started from the login shell, is a program like any other.
        let nested = process(false, &["-bash"], "/Users/me");
        assert_eq!(text("", None, Some(&nested)), "bash");
        // A process whose arguments could not be read leaves the directory.
        let unreadable = process(false, &[], "/Users/me/x");
        assert_eq!(text("", None, Some(&unreadable)), "~/x");
    }

    #[test]
    fn the_shell_at_a_prompt_is_its_directory_with_home_as_a_tilde() {
        let shell = process(true, &["-zsh"], "/Users/me/develop/project");
        assert_eq!(text("", None, Some(&shell)), "~/develop/project");
        // What the shell reported (OSC 7) beats what the process table says.
        assert_eq!(text("", Some("/Users/me"), Some(&shell)), "~");
        assert_eq!(text("", Some("/opt/data"), Some(&shell)), "/opt/data");
        assert_eq!(text("", Some("/Users/me/x"), None), "~/x");
        assert_eq!(text("", None, None), "rtty");
    }

    #[test]
    fn the_directory_is_known_whatever_the_title() {
        let cargo = process(false, &["cargo"], "/Users/me/project");
        let found = title("named", None, Some(&cargo), home());
        assert_eq!(found.dir, Some(PathBuf::from("/Users/me/project")));
        let reported = title("", Some(Path::new("/srv")), Some(&cargo), home());
        assert_eq!(reported.dir, Some(PathBuf::from("/srv")));
        assert_eq!(title("", None, None, home()).dir, None);
    }

    #[test]
    fn only_a_whole_home_directory_shortens() {
        assert_eq!(shorten(Path::new("/Users/me/a b"), home()), "~/a b");
        assert_eq!(shorten(Path::new("/Users/meow"), home()), "/Users/meow");
        assert_eq!(shorten(Path::new("/Users/me"), None), "/Users/me");
    }
}
