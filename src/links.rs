//! Links in a terminal, independent of GPUI: an OSC 8 hyperlink on a cell, else a plain URL in the
//! text around it; what a click on one does; and what to show of one whose text is not its target.
//! Nothing here opens anything: the caller passes the opener and the revealer.
//!
//! A `file:` link is never opened: opening a file URL launches what it names (an app, a
//! `.command` that runs in Terminal, an executable), and an OSC 8 link's text can be anything, so
//! output that says "click here" could run a program. It is shown in Finder instead.

use std::sync::LazyLock;

use regex::Regex;
use rtty_vt::Terminal;

use crate::search;
use crate::selection::Point;

/// A plain URL: http, https or file, up to a space or a quote, not ending in punctuation (a
/// sentence's full stop, a closing bracket around the URL).
static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(?:https?|file)://[^\s<>"'`]*[^\s<>"'`.,;:!?)\]}]"#).expect("a regex")
});

/// The schemes a click opens with the system's handler; `file` is revealed, any other ignored.
const OPENABLE: [&str; 3] = ["http", "https", "mailto"];

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
    pub uri: String,
    pub start: Point,
    pub end: Point,
}

/// The link on cell `point`: its OSC 8 hyperlink, over the run of cells on that row carrying the
/// same one; else a plain URL in the cell's line, soft wraps included.
pub fn link_at(term: &Terminal, (row, col): Point) -> Option<Link> {
    let line = term.row(row)?;
    let col = usize::from(col);
    if let Some(link) = line.hyperlink(col) {
        let same = |at: usize| line.hyperlink(at).is_some_and(|other| **other == **link);
        let first = (0..col).rev().take_while(|&at| same(at)).last().unwrap_or(col);
        let last = (col + 1..line.cells().len()).take_while(|&at| same(at)).last().unwrap_or(col);
        return Some(Link {
            uri: link.uri.clone(),
            start: (row, first as u16),
            end: (row, last as u16),
        });
    }
    let (from, to) = search::line_rows(term, row);
    let found = search::find_in(term, &URL, from, to);
    let point = (row, col as u16);
    let (start, end) = found.into_iter().find(|&(start, end)| (start..=end).contains(&point))?;
    Some(Link { uri: term.text(start, end), start, end })
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
/// what a click does with it. Nothing for a link that says what it is, a plain URL among them.
pub fn disguise(term: &Terminal, link: &Link) -> Option<String> {
    if term.text(link.start, link.end).trim() == link.uri {
        return None;
    }
    let verb = match follows(&link.uri) {
        Follow::Open => "opens",
        Follow::Reveal => "shows in Finder",
        Follow::Ignore => "does not open",
    };
    Some(format!("{verb} {}", link.uri))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(rows: u16, cols: u16, bytes: &[u8]) -> Terminal {
        let mut term = Terminal::new(rows, cols, 100);
        term.advance(bytes);
        term
    }

    fn uri(term: &Terminal, point: Point) -> Option<String> {
        link_at(term, point).map(|link| link.uri)
    }

    #[test]
    fn plain_urls_stop_before_spaces_quotes_and_closing_punctuation() {
        let term =
            term(4, 80, b"see https://example.com/a?b=1, or (file:///tmp/x.txt). \"http://h/p\"");
        let first = link_at(&term, (0, 10)).unwrap();
        assert_eq!(
            first,
            Link { uri: "https://example.com/a?b=1".into(), start: (0, 4), end: (0, 28) }
        );
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
        let link = link_at(&term, (1, 2)).unwrap();
        assert_eq!(link.uri, "https://example.com/long/path");
        assert_eq!((link.start, link.end), ((0, 3), (1, 11)));
        assert_eq!(link_at(&term, (0, 5)), Some(link));
    }

    #[test]
    fn an_osc_8_hyperlink_covers_its_run_of_cells() {
        let linked =
            term(2, 40, b"a \x1b]8;id=1;https://example.com/x\x1b\\click here\x1b]8;;\x1b\\ b");
        let link = link_at(&linked, (0, 5)).unwrap();
        assert_eq!(link, Link { uri: "https://example.com/x".into(), start: (0, 2), end: (0, 11) });
        assert_eq!(link_at(&linked, (0, 13)), None);
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
        let link = link_at(&hostile, (0, 3)).unwrap();
        let mut revealed = Vec::new();
        let did =
            follow(&link.uri, |uri| panic!("opened {uri}"), |uri| revealed.push(uri.to_string()));
        assert_eq!((did, revealed), (Follow::Reveal, vec!["file:///tmp/x.command".to_string()]));
        // A plain file URL in the text goes the same way.
        let plain = term(2, 40, b"see file:///Applications/X.app now");
        let link = link_at(&plain, (0, 10)).unwrap();
        assert_eq!(follow(&link.uri, |uri| panic!("opened {uri}"), |_| {}), Follow::Reveal);
    }

    #[test]
    fn a_disguised_link_shows_its_target_and_a_plain_url_nothing_more() {
        let shown = |bytes: &[u8], col| {
            let term = term(2, 60, bytes);
            disguise(&term, &link_at(&term, (0, col)).unwrap())
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
}
