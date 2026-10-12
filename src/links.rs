//! Links in a terminal, independent of GPUI: an OSC 8 hyperlink on a cell, else a plain URL in the
//! text around it; and which of them may be opened. Nothing here opens anything: the caller
//! passes the opener.

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

/// The schemes a click may open; any other is ignored.
const OPENABLE: [&str; 4] = ["http", "https", "file", "mailto"];

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

/// Whether a click may open `uri`: http, https, file and mailto only.
pub fn openable(uri: &str) -> bool {
    let scheme = uri.split_once(':').map_or("", |(scheme, _)| scheme);
    OPENABLE.iter().any(|allowed| scheme.eq_ignore_ascii_case(allowed))
}

/// Opens `uri` with `open` when it may be opened; whether it was.
pub fn open(uri: &str, open: impl FnOnce(&str)) -> bool {
    let may = openable(uri);
    if may {
        open(uri);
    }
    may
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
    fn only_http_https_file_and_mailto_open() {
        let mut opened = Vec::new();
        for uri in ["https://example.com", "HTTP://x", "file:///tmp/a", "mailto:a@b.c"] {
            assert!(open(uri, |uri| opened.push(uri.to_string())), "{uri}");
        }
        for uri in ["javascript:alert(1)", "ftp://h/x", "ssh://h", "x-man-page://ls", "noscheme"] {
            assert!(!open(uri, |_| panic!("opened {uri}")), "{uri}");
        }
        assert_eq!(opened, ["https://example.com", "HTTP://x", "file:///tmp/a", "mailto:a@b.c"]);
    }
}
