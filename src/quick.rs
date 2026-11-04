//! Quick select, after WezTerm's: what is worth copying on a pane's screen (URLs, paths, git
//! hashes, IP addresses, UUIDs, long numbers and the config's own patterns), each under a short
//! label of home-row letters, nearest the cursor first and none a prefix of another. Independent
//! of GPUI: the view shows the labels and takes the letters typed.

use std::cmp::Reverse;
use std::sync::LazyLock;

use regex::Regex;
use rtty_vt::Terminal;

use crate::links;
use crate::search::{self, Match};
use crate::selection::Point;

/// The letters labels are made of.
pub const LETTERS: &str = "asdfghjkl";

/// What quick select finds besides URLs and paths: git hashes, IPv4 addresses, IPv6 addresses
/// (eight groups, or fewer around a `::`, which no Rust path like `std::fmt` makes, since the
/// address must start a word), UUIDs, and numbers of four digits or more.
static KNOWN: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    let group = "[0-9a-fA-F]{1,4}";
    let ipv6 = format!(
        r"(?:^|[^\w:])((?:{group}:){{7}}{group}|(?:(?:{group}:){{1,6}}|:):(?:{group}:){{0,5}}{group})\b"
    );
    let uuid = r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b";
    [r"\b[0-9a-f]{7,40}\b", r"\b(?:\d{1,3}\.){3}\d{1,3}\b", &ipv6, uuid, r"\b\d{4,}\b"]
        .map(|source| Regex::new(source).expect("a regex"))
        .into()
});

/// A match on the screen, the label that picks it, and its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub cells: Match,
    pub text: String,
}

/// What quick select finds on the rows `top..top + rows`, top to bottom: what `patterns` match
/// (a pattern's first group, when it has one), URLs, paths and what it knows (`KNOWN`); for quick
/// open, URLs and paths alone. Of two that overlap, the one starting first stays, the longer of
/// two starting together.
pub fn found(
    term: &Terminal,
    top: u64,
    rows: u16,
    patterns: &[Regex],
    open: bool,
) -> Vec<(Match, String)> {
    let bottom = top + u64::from(rows);
    let (from, _) = search::line_rows(term, top);
    let (_, to) = search::line_rows(term, bottom - 1);
    let mut found = Vec::new();
    search::each_line(term, from, to, |text, cells| {
        let groups = |pattern: &Regex| {
            let groups =
                pattern.captures_iter(text).filter_map(|found| found.get(1).or(found.get(0)));
            groups.map(|group| group.range()).collect::<Vec<_>>()
        };
        let patterns = patterns.iter().filter(|_| !open).flat_map(&groups);
        let urls = links::URL.find_iter(text).map(|url| url.range());
        let paths = links::paths(text).into_iter().map(|(span, _)| span);
        let known = KNOWN.iter().filter(|_| !open).flat_map(&groups);
        let mut spans: Vec<_> = patterns.chain(urls).chain(paths).chain(known).collect();
        spans.sort_by_key(|span| (span.start, Reverse(span.end)));
        let mut end = 0;
        for span in spans.into_iter().filter(|span| !span.is_empty()) {
            if span.start >= end {
                end = span.end;
                found.push((cells(span.clone()), text[span].to_owned()));
            }
        }
    });
    found.retain(|&((start, _), _)| (top..bottom).contains(&start.0));
    found
}

/// `found` labeled, nearest `cursor` first: by rows away, then by columns.
pub fn labeled(mut found: Vec<(Match, String)>, (row, col): Point) -> Vec<Item> {
    found.sort_by_key(|&((start, _), _)| (start.0.abs_diff(row), start.1.abs_diff(col)));
    let labels = labels(found.len()).into_iter().zip(found);
    labels.map(|(label, (cells, text))| Item { label, cells, text }).collect()
}

/// `count` labels of `LETTERS`, shortest first, none a prefix of another: a letter each while
/// they last, then the first letters stand for two-letter labels instead, and so on.
pub fn labels(count: usize) -> Vec<String> {
    let mut labels: Vec<String> = LETTERS.chars().map(String::from).collect();
    let mut expanded = 0;
    while labels.len() - expanded < count {
        let prefix = labels[expanded].clone();
        labels.extend(LETTERS.chars().map(|letter| format!("{prefix}{letter}")));
        expanded += 1;
    }
    labels.drain(expanded..).take(count).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(rows: u16, cols: u16, bytes: &[u8]) -> Terminal {
        let mut term = Terminal::new(rows, cols, 100);
        term.advance(bytes);
        term
    }

    fn texts(term: &Terminal, patterns: &[Regex], open: bool) -> Vec<String> {
        let found = found(term, 0, term.size().0, patterns, open);
        found.into_iter().map(|(_, text)| text).collect()
    }

    #[test]
    fn labels_are_short_home_row_letters_and_never_prefixes_of_one_another() {
        assert!(labels(0).is_empty());
        assert_eq!(labels(3), ["a", "s", "d"]);
        assert_eq!(labels(9).concat(), LETTERS);
        assert_eq!(labels(10), ["s", "d", "f", "g", "h", "j", "k", "l", "aa", "as"]);
        for count in [10, 17, 81, 82, 200] {
            let labels = labels(count);
            assert_eq!(labels.len(), count);
            for (index, label) in labels.iter().enumerate() {
                assert!(label.chars().all(|letter| LETTERS.contains(letter)), "{label}");
                for other in &labels[index + 1..] {
                    assert!(!other.starts_with(label.as_str()), "{label} starts {other}");
                    assert!(other.len() >= label.len(), "{label} is shorter than {other}");
                }
            }
        }
        assert_eq!(labels(81).iter().map(String::len).max(), Some(2));
        assert_eq!(labels(82).iter().map(String::len).max(), Some(3));
    }

    #[test]
    fn it_finds_urls_paths_hashes_addresses_uuids_and_long_numbers() {
        let screen = "see https://example.com/a?b=1 at src/view.rs:12:5\r\n\
                      commit 3f2a9c1d0e of 2026, pid 42\r\n\
                      host 192.168.1.20 or fe80::1 or 2001:db8::8a2e:370:7334\r\n\
                      id 550e8400-e29b-41d4-a716-446655440000 not std::fmt\r\n";
        let term = term(6, 80, screen.as_bytes());
        assert_eq!(
            texts(&term, &[], false),
            [
                "https://example.com/a?b=1",
                "src/view.rs:12:5",
                "3f2a9c1d0e",
                "2026",
                "192.168.1.20",
                "fe80::1",
                "2001:db8::8a2e:370:7334",
                "550e8400-e29b-41d4-a716-446655440000",
            ]
        );
        // Quick open labels what opens: URLs and paths.
        assert_eq!(texts(&term, &[], true), ["https://example.com/a?b=1", "src/view.rs:12:5"]);
    }

    #[test]
    fn the_configs_patterns_come_first_and_take_their_first_group() {
        let term = term(3, 60, b"ticket ABC-1234 and sha256:0123456789abcdef done");
        let patterns = [Regex::new(r"[A-Z]+-\d+").unwrap(), Regex::new(r"sha256:(\w+)").unwrap()];
        assert_eq!(texts(&term, &patterns, false), ["ABC-1234", "0123456789abcdef"]);
        assert_eq!(texts(&term, &[], false), ["1234", "0123456789abcdef"]);
        assert!(texts(&term, &patterns, true).is_empty(), "quick open takes no patterns");
    }

    #[test]
    fn the_cursors_nearest_match_gets_the_first_label_and_cells_span_wide_characters() {
        let term = term(5, 40, "a 1111\r\n漢字 2222\r\nb 3333".as_bytes());
        let found = found(&term, 0, 5, &[], false);
        assert_eq!(found[1], (((1, 5), (1, 8)), "2222".to_string()));
        let items = labeled(found, (2, 0));
        let labeled: Vec<(&str, &str)> =
            items.iter().map(|item| (item.label.as_str(), item.text.as_str())).collect();
        assert_eq!(labeled, [("a", "3333"), ("s", "2222"), ("d", "1111")]);
    }

    #[test]
    fn only_the_rows_in_view_are_labeled() {
        let term = term(3, 20, b"1111\r\n2222\r\n3333\r\n4444\r\n5555");
        // The screen holds 3333, 4444 and 5555; the view is scrolled up a row.
        let top = term.screen_top() - 1;
        let found = found(&term, top, 3, &[], false);
        let texts: Vec<&str> = found.iter().map(|(_, text)| text.as_str()).collect();
        assert_eq!(texts, ["2222", "3333", "4444"]);
    }
}
