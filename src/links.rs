//! Links in a terminal, independent of GPUI: an OSC 8 hyperlink on a cell, else a plain URL in the
//! text around it, else the path of a file that exists, with the line and column after it as
//! compilers, grep, test runners and stack traces print them; what a click on one does; and what
//! to show of one whose text is not its target. Nothing here opens anything: the caller passes
//! the opener and the revealer, and opens a file as `opening` says.
//!
//! A `file:` link is never opened: opening a file URL launches what it names (an app, a
//! `.command` that runs in Terminal, an executable), and an OSC 8 link's text can be anything, so
//! output that says "click here" could run a program. It is shown in Finder instead, as is a path
//! whose opening would run something.

use std::fs::{self, File};
use std::io::Read;
use std::ops::Range;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use rtty_vt::Terminal;

use crate::search;
use crate::selection::Point;

/// A plain URL: http, https or file, up to a space or a quote, not ending in punctuation (a
/// sentence's full stop, a closing bracket around the URL).
pub static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(?:https?|file)://[^\s<>"'`]*[^\s<>"'`.,;:!?)\]}]"#).expect("a regex")
});

/// A path, then `:line` or `:line:col`, or Python's `", line N`. The path has a slash (`/abs`,
/// `~/x`, `./x`, `src/x`) or is a name with an extension (`main.rs`), and holds no space, quote,
/// bracket, colon, `=` or backslash. It starts the text or follows a space, a quote, an opening
/// bracket, `=`, `,`, `;` or `|`, never a colon: a URL's `//host` is none, nor is any part of a
/// Windows `C:\x` or `C:/x`.
static PATH: LazyLock<Regex> = LazyLock::new(|| {
    let source = r#"(?:^|[\s"'`(\[{<=,;|])(?P<path>%*/%*|[\w.-]*\w\.[A-Za-z]\w*)"#.to_owned()
        + r#"(?::(?P<line>\d+)(?::(?P<col>\d+))?|", line (?P<python>\d+))?"#;
    Regex::new(&source.replace('%', r#"[^\s:"'`()\[\]{}<>=,;|\\]"#)).expect("a regex")
});

/// The schemes a click opens with the system's handler; `file` is revealed, any other ignored.
const OPENABLE: [&str; 3] = ["http", "https", "mailto"];

/// Extensions of files that run something, or open what they point at, when opened.
const RUNS: [&str; 7] = ["command", "tool", "terminal", "jar", "fileloc", "inetloc", "webloc"];

/// What opening a file does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opening {
    /// Text: an editor shows it at the line and column.
    Edit,
    /// The app macOS has for it (`open`): Finder for a directory, Preview for an image or a PDF.
    Open,
    /// Finder shows it, selected: opening it would run something.
    Reveal,
}

/// A file and a place in it: a line and a column, from 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: u32,
    pub col: u32,
}

/// Where a link leads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// An OSC 8 hyperlink's URI, or a URL in the text.
    Uri(String),
    /// A file that exists, by its absolute path.
    File(Location),
}

/// What a Cmd+click on a link does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Follow {
    /// http, https, mailto: the system's handler opens it.
    Open,
    /// file: Finder shows it, selected, and nothing runs.
    Reveal,
    /// Any other scheme.
    Ignore,
}

/// A link and the cells it covers, inclusive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub target: Target,
    pub start: Point,
    pub end: Point,
}

/// The link on cell `point`: its OSC 8 hyperlink, over the run of cells on that row carrying the
/// same one; else a plain URL in the cell's line, soft wraps included; else a path there to a
/// file that exists (`resolve`), from the directory the shell reported (OSC 7), else `cwd`'s.
pub fn link_at(
    term: &Terminal,
    (row, col): Point,
    cwd: impl FnOnce() -> Option<PathBuf>,
) -> Option<Link> {
    let line = term.row(row)?;
    let col = usize::from(col);
    if let Some(link) = line.hyperlink(col) {
        let same = |at: usize| line.hyperlink(at).is_some_and(|other| **other == **link);
        let first = (0..col).rev().take_while(|&at| same(at)).last().unwrap_or(col);
        let last = (col + 1..line.cells().len()).take_while(|&at| same(at)).last().unwrap_or(col);
        return Some(Link {
            target: Target::Uri(link.uri.clone()),
            start: (row, first as u16),
            end: (row, last as u16),
        });
    }
    let (from, to) = search::line_rows(term, row);
    let point = (row, col as u16);
    let on = |&(start, end): &(Point, Point)| (start..=end).contains(&point);
    if let Some((start, end)) = search::find_in(term, &URL, from, to).into_iter().find(on) {
        return Some(Link { target: Target::Uri(term.text(start, end)), start, end });
    }
    let mut found = None;
    search::each_line(term, from, to, |text, cells| {
        let mut here = paths(text).into_iter().map(|(span, at)| (cells(span), at));
        found = found.take().or_else(|| here.find(|(cells, _)| on(cells)));
    });
    let ((start, end), mut at) = found?;
    at.path = resolve(&at.path, || term.cwd().map(PathBuf::from).or_else(cwd))?;
    Some(Link { target: Target::File(at), start, end })
}

/// The paths in a line of text (`PATH`), each with the bytes it covers, its line and column
/// included, and where it points as written. A path followed by no line loses the full stop,
/// `!` or `?` that ends a sentence after it.
pub fn paths(text: &str) -> Vec<(Range<usize>, Location)> {
    let found = PATH.captures_iter(text).filter_map(|groups| {
        let path = groups.name("path")?;
        let number = |name| groups.name(name).and_then(|n| n.as_str().parse().ok());
        let line = number("line").or_else(|| number("python"));
        let written = match line {
            Some(_) => path.as_str(),
            None => path.as_str().trim_end_matches(['.', '!', '?']),
        };
        let suffix = groups.name("col").or(groups.name("line"));
        let end = suffix.map_or(path.start() + written.len(), |suffix| suffix.end());
        let col = number("col").unwrap_or(1);
        let at = Location { path: written.into(), line: line.unwrap_or(1), col };
        written.chars().any(char::is_alphanumeric).then(|| (path.start()..end, at))
    });
    found.collect()
}

/// `written` as the absolute path of something that exists: `~/…` from the home directory, a
/// relative path from `cwd`.
pub fn resolve(written: &Path, cwd: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    let path = match written.strip_prefix("~") {
        Ok(rest) => PathBuf::from(std::env::var_os("HOME")?).join(rest),
        Err(_) if written.is_relative() => cwd()?.join(written),
        Err(_) => written.to_owned(),
    };
    path.exists().then_some(path)
}

/// What opening `path` does: text (no NUL in its first 8 KiB) is edited; a directory, an image,
/// a PDF and the like are opened; an app or other package (a directory with an extension), an
/// executable, and a file that runs or opens something else when opened are only shown.
pub fn opening(path: &Path) -> Opening {
    let Ok(meta) = fs::metadata(path) else { return Opening::Reveal };
    if meta.is_dir() {
        return if path.extension().is_some() { Opening::Reveal } else { Opening::Open };
    }
    let mut head = [0; 8192];
    let read = File::open(path).and_then(|mut file| file.read(&mut head)).unwrap_or(0);
    if !head[..read].contains(&0) {
        return Opening::Edit;
    }
    let ext = path.extension().unwrap_or_default();
    let runs = RUNS.iter().any(|runs| ext.eq_ignore_ascii_case(runs));
    if runs || meta.permissions().mode() & 0o111 != 0 { Opening::Reveal } else { Opening::Open }
}

/// What a click on `uri` does, by its scheme.
pub fn follows(uri: &str) -> Follow {
    let scheme = uri.split_once(':').map_or("", |(scheme, _)| scheme);
    if scheme.eq_ignore_ascii_case("file") {
        Follow::Reveal
    } else if OPENABLE.iter().any(|allowed| scheme.eq_ignore_ascii_case(allowed)) {
        Follow::Open
    } else {
        Follow::Ignore
    }
}

/// Follows a click on `uri`: `open` for what may open, `reveal` for a file; what it did.
pub fn follow(uri: &str, open: impl FnOnce(&str), reveal: impl FnOnce(&str)) -> Follow {
    let follow = follows(uri);
    match follow {
        Follow::Open => open(uri),
        Follow::Reveal => reveal(uri),
        Follow::Ignore => {}
    }
    follow
}

/// What to show while Cmd is held over `link`, when its text is not its target: the target, and
/// what a click does with it. Nothing for a link that says what it is, a plain URL or a path
/// among them.
pub fn disguise(term: &Terminal, link: &Link) -> Option<String> {
    let Target::Uri(uri) = &link.target else { return None };
    if term.text(link.start, link.end).trim() == uri {
        return None;
    }
    let verb = match follows(uri) {
        Follow::Open => "opens",
        Follow::Reveal => "shows in Finder",
        Follow::Ignore => "does not open",
    };
    Some(format!("{verb} {uri}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(rows: u16, cols: u16, bytes: &[u8]) -> Terminal {
        let mut term = Terminal::new(rows, cols, 100);
        term.advance(bytes);
        term
    }

    /// The link on `point`, with no directory but the shell's to find a relative path in.
    fn link(term: &Terminal, point: Point) -> Option<Link> {
        link_at(term, point, || None)
    }

    fn uri(term: &Terminal, point: Point) -> Option<String> {
        match link(term, point)?.target {
            Target::Uri(uri) => Some(uri),
            Target::File(at) => panic!("a file, {at:?}"),
        }
    }

    fn web(uri: &str, start: Point, end: Point) -> Link {
        Link { target: Target::Uri(uri.into()), start, end }
    }

    #[test]
    fn plain_urls_stop_before_spaces_quotes_and_closing_punctuation() {
        let term =
            term(4, 80, b"see https://example.com/a?b=1, or (file:///tmp/x.txt). \"http://h/p\"");
        let first = link(&term, (0, 10)).unwrap();
        assert_eq!(first, web("https://example.com/a?b=1", (0, 4), (0, 28)));
        assert_eq!(uri(&term, (0, 36)).as_deref(), Some("file:///tmp/x.txt"));
        assert_eq!(uri(&term, (0, 56)).as_deref(), Some("http://h/p"));
        // Off the URL, and on the punctuation after it, no link.
        assert_eq!(uri(&term, (0, 1)), None);
        assert_eq!(uri(&term, (0, 29)), None);
    }

    #[test]
    fn only_http_https_and_file_are_found_in_text() {
        let term = term(2, 60, b"ftp://h/x javascript:alert(1) mailto:a@b.c ssh://h");
        for col in [2, 15, 33, 46] {
            assert_eq!(uri(&term, (0, col)), None, "column {col}");
        }
    }

    #[test]
    fn a_url_runs_on_across_a_soft_wrap() {
        // 20 columns: the URL wraps after "https://example.com/" into the next row.
        let term = term(3, 20, b"go https://example.com/long/path now");
        let wrapped = link(&term, (1, 2)).unwrap();
        assert_eq!(wrapped, web("https://example.com/long/path", (0, 3), (1, 11)));
        assert_eq!(link(&term, (0, 5)), Some(wrapped));
    }

    #[test]
    fn an_osc_8_hyperlink_covers_its_run_of_cells() {
        let linked =
            term(2, 40, b"a \x1b]8;id=1;https://example.com/x\x1b\\click here\x1b]8;;\x1b\\ b");
        assert_eq!(link(&linked, (0, 5)), Some(web("https://example.com/x", (0, 2), (0, 11))));
        assert_eq!(link(&linked, (0, 13)), None);
        // Any scheme a program puts in OSC 8 is found; only opening is limited.
        let script = term(2, 40, b"\x1b]8;;javascript:alert(1)\x1b\\x\x1b]8;;\x1b\\");
        assert_eq!(uri(&script, (0, 0)).as_deref(), Some("javascript:alert(1)"));
    }

    #[test]
    fn http_https_and_mailto_open_files_are_revealed_and_the_rest_ignored() {
        let never = |uri: &str| panic!("{uri} went the wrong way");
        let mut opened = Vec::new();
        for uri in ["https://example.com", "HTTP://x", "mailto:a@b.c"] {
            let follow = follow(uri, |uri| opened.push(uri.to_string()), never);
            assert_eq!(follow, Follow::Open, "{uri}");
        }
        assert_eq!(opened, ["https://example.com", "HTTP://x", "mailto:a@b.c"]);
        for uri in ["javascript:alert(1)", "ftp://h/x", "ssh://h", "x-man-page://ls", "noscheme"] {
            assert_eq!(follow(uri, never, never), Follow::Ignore, "{uri}");
        }
    }

    /// Output that says "click here" over a file URL naming something that would run: a click
    /// shows it in Finder and opens nothing.
    #[test]
    fn a_hostile_file_link_is_revealed_never_opened() {
        let hostile = term(2, 40, b"\x1b]8;;file:///tmp/x.command\x1b\\click here\x1b]8;;\x1b\\");
        let target = uri(&hostile, (0, 3)).unwrap();
        let mut revealed = Vec::new();
        let did = follow(&target, |uri| panic!("opened {uri}"), |uri| revealed.push(uri.into()));
        assert_eq!((did, revealed), (Follow::Reveal, vec!["file:///tmp/x.command".to_string()]));
        // A plain file URL in the text goes the same way.
        let plain = term(2, 40, b"see file:///Applications/X.app now");
        let target = uri(&plain, (0, 10)).unwrap();
        assert_eq!(follow(&target, |uri| panic!("opened {uri}"), |_| {}), Follow::Reveal);
    }

    #[test]
    fn a_disguised_link_shows_its_target_and_a_plain_url_nothing_more() {
        let shown = |bytes: &[u8], col| {
            let term = term(2, 60, bytes);
            disguise(&term, &link(&term, (0, col)).unwrap())
        };
        assert_eq!(
            shown(b"\x1b]8;;file:///tmp/x.command\x1b\\click here\x1b]8;;\x1b\\", 2).as_deref(),
            Some("shows in Finder file:///tmp/x.command")
        );
        assert_eq!(
            shown(b"\x1b]8;;https://evil.example/\x1b\\https://bank.example/\x1b]8;;\x1b\\", 2)
                .as_deref(),
            Some("opens https://evil.example/")
        );
        assert_eq!(
            shown(b"\x1b]8;;javascript:alert(1)\x1b\\docs\x1b]8;;\x1b\\", 1).as_deref(),
            Some("does not open javascript:alert(1)")
        );
        // A link that says what it is, and a plain URL, show nothing more.
        assert_eq!(
            shown(b"\x1b]8;;https://a.example/\x1b\\https://a.example/\x1b]8;;\x1b\\", 3),
            None
        );
        assert_eq!(shown(b"go to https://a.example/x now", 8), None);
    }

    /// Each path in `text`: what it covers, as written, its line and its column.
    fn written(text: &str) -> Vec<(&str, &str, u32, u32)> {
        let found = paths(text).into_iter().map(|(span, at)| {
            let path = &text[span.start..span.start + at.path.as_os_str().len()];
            assert_eq!(Path::new(path), at.path);
            (&text[span], path, at.line, at.col)
        });
        found.collect()
    }

    #[test]
    fn paths_carry_the_line_and_column_compilers_grep_and_stack_traces_print() {
        assert_eq!(written("src/view.rs:123:45"), [("src/view.rs:123:45", "src/view.rs", 123, 45)]);
        assert_eq!(written("  --> src/main.rs:5:9"), [("src/main.rs:5:9", "src/main.rs", 5, 9)]);
        assert_eq!(written("./a/b.rs:7: warning"), [("./a/b.rs:7", "./a/b.rs", 7, 1)]);
        assert_eq!(written("/abs/x.py:3"), [("/abs/x.py:3", "/abs/x.py", 3, 1)]);
        assert_eq!(written("~/notes.md"), [("~/notes.md", "~/notes.md", 1, 1)]);
        // grep's match after its line; Java's frame; Go's offset after the line.
        assert_eq!(written("lib/a.ex:12:def f"), [("lib/a.ex:12", "lib/a.ex", 12, 1)]);
        // A Java frame's class reads as a name with an extension too; no file has that name.
        let java = written("at a.X.run(Foo.java:12)");
        assert_eq!(java, [("a.X.run", "a.X.run", 1, 1), ("Foo.java:12", "Foo.java", 12, 1)]);
        assert_eq!(written("\t/go/x.go:8 +0x1d"), [("/go/x.go:8", "/go/x.go", 8, 1)]);
        // Python names the line after the quoted path; the link is the path.
        let python = written(r#"  File "/abs/x.py", line 3, in <module>"#);
        assert_eq!(python, [("/abs/x.py", "/abs/x.py", 3, 1)]);
        assert_eq!(written("at f (/srv/x.js:3:5)"), [("/srv/x.js:3:5", "/srv/x.js", 3, 5)]);
        let two = written("a.rs b/c.rs");
        assert_eq!(two, [("a.rs", "a.rs", 1, 1), ("b/c.rs", "b/c.rs", 1, 1)]);
    }

    #[test]
    fn trailing_punctuation_and_brackets_are_not_the_path() {
        assert_eq!(written("see src/a.rs."), [("src/a.rs", "src/a.rs", 1, 1)]);
        assert_eq!(written("fix (src/a.rs) now!"), [("src/a.rs", "src/a.rs", 1, 1)]);
        assert_eq!(
            written("[docs/x.md], 'b/y.md'"),
            [("docs/x.md", "docs/x.md", 1, 1), ("b/y.md", "b/y.md", 1, 1)]
        );
        assert_eq!(written("src/a.rs:12."), [("src/a.rs:12", "src/a.rs", 12, 1)]);
        assert_eq!(written("--path=out/x.txt;"), [("out/x.txt", "out/x.txt", 1, 1)]);
    }

    #[test]
    fn urls_windows_paths_and_slashes_alone_are_no_paths() {
        for text in [
            "https://example.com:8080/a/b.rs:12",
            "file:///tmp/x.txt",
            r"C:\Temp\x\file.txt",
            "C:/Temp/x/file.txt",
            r".\a\b.txt",
            "// a comment",
            "1.5 v2.0.1 / ./ ../..",
            "word and.123",
        ] {
            assert_eq!(written(text), [], "{text}");
        }
    }

    /// A scratch directory holding `files`, each with its content.
    fn scratch(test: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rtty-links-{test}-{}", std::process::id()));
        for (name, content) in files {
            let path = dir.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        dir.canonicalize().unwrap()
    }

    #[test]
    fn a_path_is_a_link_where_it_exists_from_the_shells_directory_else_the_programs() {
        let dir = scratch("exists", &[("src/main.rs", b"fn main() {}\n")]);
        let osc7 = format!("\x1b]7;file://host{}\x07", dir.display());
        let text = format!("{osc7}error: src/main.rs:3:5 and src/gone.rs:1");
        let reported = term(2, 60, text.as_bytes());
        let at = Location { path: dir.join("src/main.rs"), line: 3, col: 5 };
        let found = Link { target: Target::File(at), start: (0, 7), end: (0, 21) };
        assert_eq!(link_at(&reported, (0, 10), || panic!("the shell said")), Some(found.clone()));
        assert_eq!(link(&reported, (0, 30)), None, "no such file");
        // No OSC 7: the program's directory, asked only now.
        let unreported = term(2, 60, b"error: src/main.rs:3:5");
        assert_eq!(link_at(&unreported, (0, 21), || Some(dir.clone())), Some(found));
        assert_eq!(link(&unreported, (0, 10)), None, "no directory to find it in");
        // An absolute path needs none, and wide characters before it keep it on its cells.
        let wide = format!("漢字 {}:2", dir.join("src/main.rs").display());
        let wide = term(2, 120, wide.as_bytes());
        let link = link(&wide, (0, 9)).unwrap();
        let end = 5 + dir.join("src/main.rs:2").as_os_str().len() as u16 - 1;
        assert_eq!((link.start, link.end), ((0, 5), (0, end)));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn text_is_edited_and_what_would_run_is_only_shown() {
        let binary: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        let dir = scratch(
            "opening",
            &[
                ("notes.md", b"# notes\n"),
                ("empty.txt", b""),
                ("run.sh", b"#!/bin/sh\necho hi\n"),
                ("image.png", binary),
                ("tool", binary),
                ("Site.webloc", binary),
                ("plain/keep", b""),
                ("Thing.app/Contents/Info", b""),
            ],
        );
        for executable in ["run.sh", "tool"] {
            fs::set_permissions(dir.join(executable), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let opening = |name: &str| opening(&dir.join(name));
        for name in ["notes.md", "empty.txt", "run.sh"] {
            assert_eq!(opening(name), Opening::Edit, "{name}");
        }
        for name in ["image.png", "plain"] {
            assert_eq!(opening(name), Opening::Open, "{name}");
        }
        for name in ["tool", "Site.webloc", "Thing.app", "gone"] {
            assert_eq!(opening(name), Opening::Reveal, "{name}");
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
