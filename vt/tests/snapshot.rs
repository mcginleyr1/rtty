//! `snapshot()` reproduces the visible state: a fresh engine fed it matches
//! the original, and keeps matching as both are fed the same probes — a
//! switch back from the alternate screen, a restored cursor, tab stops.

mod common;

use common::{Profile, Token, build, stream};
use proptest::prelude::*;
use rtty_vt::{Cell, Terminal};

/// One visible cell, everything a renderer or a copy reads.
#[derive(Debug, PartialEq)]
struct Seen {
    text: String,
    fg: rtty_vt::Color,
    bg: rtty_vt::Color,
    attrs: rtty_vt::Attrs,
    /// Width, then whether it is a pad.
    layout: (u8, bool),
    underline: rtty_vt::Color,
    link: Option<rtty_vt::Hyperlink>,
}

fn seen(cell: &Cell, text: String) -> Seen {
    Seen {
        text,
        fg: cell.fg(),
        bg: cell.bg(),
        attrs: cell.attrs(),
        layout: (cell.width(), cell.is_pad()),
        underline: rtty_vt::Color::Default,
        link: None,
    }
}

#[derive(Debug, PartialEq)]
struct Visible {
    rows: Vec<(Vec<Seen>, bool)>,
    cursor: (u16, u16),
    pending: bool,
    style: Option<rtty_vt::CursorStyle>,
    modes: rtty_vt::Modes,
    kitty: u8,
    region: (u16, u16),
    title: String,
    icon: String,
    colors: Vec<Option<rtty_vt::Rgb>>,
}

/// Below three rows or columns a soft wrap of the bottom row, whose
/// continuation has scrolled away, cannot be reproduced without scrolling.
fn tiny(term: &Terminal) -> bool {
    let (rows, cols) = term.size();
    rows < 3 || cols < 3
}

/// In origin mode, cursor addressing cannot reach a cursor a restore left
/// outside the scroll region.
fn unreachable_cursor(term: &Terminal) -> bool {
    let (top, bottom) = term.scroll_region();
    let row = term.cursor_position().0;
    term.modes().origin && !(top..=bottom).contains(&row)
}

fn visible(term: &Terminal) -> Visible {
    let (rows, _) = term.size();
    Visible {
        rows: (0..rows)
            .map(|r| {
                let row = term.screen_row(r);
                let bottom = r + 1 == rows && tiny(term);
                // A pending wrap is made by reprinting the cursor's cell,
                // which ends its row's soft wrap and pad.
                let pending_here = term.wrap_pending() && term.cursor_position().0 == r;
                let last = row.cells().len() - 1;
                let cells = (0..row.cells().len())
                    .map(|col| {
                        let mut text = String::new();
                        row.push_grapheme(col, &mut text);
                        let mut seen = Seen {
                            underline: row.underline_color(col),
                            link: row.hyperlink(col).map(|link| (**link).clone()),
                            ..seen(&row.cells()[col], text)
                        };
                        seen.layout.1 &= !bottom && !(pending_here && col == last);
                        seen
                    })
                    .collect();
                (cells, row.wrapped() && !bottom && !pending_here)
            })
            .collect(),
        cursor: term.cursor_position(),
        pending: term.wrap_pending(),
        style: term.cursor_style(),
        // A snapshot is a whole frame: a synchronized update in progress is
        // not carried.
        modes: rtty_vt::Modes { sync_update: false, ..term.modes().clone() },
        kitty: term.kitty_keyboard(),
        region: term.scroll_region(),
        title: term.title().to_string(),
        icon: term.icon_name().to_string(),
        colors: (0..259).map(|slot| term.color_override(slot)).collect(),
    }
}

/// Probes fed to both engines after the snapshot, each from scratch.
const PROBES: &[&str] = &[
    "",
    "q",
    "\x1b8q",
    "\x1b[?1049lq",
    "\x1b[?1047l\x1b8q",
    "\r\tA\tB\tC\tD\tE",
    "\x1b[2J\x1b[Hq\x1b[3;1Hq\x1b[99;1Hq",
];

fn check(rows: u16, cols: u16, tokens: &[Token]) -> Result<(), TestCaseError> {
    for probe in PROBES {
        let (_, mut original) = build(rows, cols, tokens, false);
        // A pending wrap is made by reprinting the cursor's cell, which ends
        // a soft wrap of its row.
        let pending_on_wrap =
            original.wrap_pending() && original.screen_row(original.cursor_position().0).wrapped();
        if (tiny(&original) || pending_on_wrap) && !probe.is_empty()
            || unreachable_cursor(&original)
        {
            // A probe would bring what is not reproduced into view.
            continue;
        }
        let (rows, cols) = original.size();
        let snapshot = original.snapshot();
        let mut copy = Terminal::new(rows, cols, 0);
        copy.advance(&snapshot);
        original.advance(probe.as_bytes());
        copy.advance(probe.as_bytes());
        if let Some(detail) = difference(&visible(&original), &visible(&copy)) {
            return Err(TestCaseError::fail(format!(
                "probe {probe:?}: {detail}\nsnapshot {:?}",
                String::from_utf8_lossy(&snapshot)
            )));
        }
    }
    Ok(())
}

fn difference(want: &Visible, got: &Visible) -> Option<String> {
    if want == got {
        return None;
    }
    let cell = want.rows.iter().zip(&got.rows).enumerate().find_map(|(r, (a, b))| {
        if a.1 != b.1 {
            return Some(format!("row {r} wrapped: want {} got {}", a.1, b.1));
        }
        let c = a.0.iter().zip(&b.0).position(|(x, y)| x != y)?;
        Some(format!("cell {r},{c}:\n want {:?}\n got  {:?}", a.0[c], b.0[c]))
    });
    Some(cell.unwrap_or_else(|| {
        let short = |v: &Visible| {
            format!(
                "{:?} {} {:?} {:?} {:?} {}",
                v.cursor, v.pending, v.style, v.modes, v.region, v.kitty
            )
        };
        format!(
            "want {}\n got  {}\n colours equal: {}",
            short(want),
            short(got),
            want.colors == got.colors
        )
    }))
}

/// A terminal seeded with a snapshot and then fed only `redraw` of the
/// damage matches, but for what `redraw` leaves out: the title and the
/// colours a program set.
fn check_redraw(
    rows: u16,
    cols: u16,
    before: &[Token],
    after: &[Token],
) -> Result<(), TestCaseError> {
    let (_, mut original) = build(rows, cols, before, false);
    let pending_on_wrap =
        original.wrap_pending() && original.screen_row(original.cursor_position().0).wrapped();
    if tiny(&original) || pending_on_wrap {
        // The snapshot the follower starts from leaves a soft wrap out.
        return Ok(());
    }
    let (rows, cols) = original.size();
    let mut copy = Terminal::new(rows, cols, 0);
    copy.advance(&original.snapshot());
    original.take_damage();
    let screen = original.modes().alt_screen;
    let mut stream = Vec::new();
    for token in after.iter().filter(|token| !matches!(token, Token::Resize(..))) {
        let bytes = token.bytes();
        original.advance(&bytes);
        stream.extend(bytes);
    }
    if original.modes().alt_screen != screen || unreachable_cursor(&original) {
        // A switch of screens calls for a snapshot.
        return Ok(());
    }
    let damage = original.take_damage();
    let redraw = original.redraw(&damage);
    copy.advance(&redraw);
    let plain = |term: &Terminal| Visible {
        title: String::new(),
        icon: String::new(),
        colors: Vec::new(),
        ..visible(term)
    };
    if let Some(detail) = difference(&plain(&original), &plain(&copy)) {
        return Err(TestCaseError::fail(format!(
            "{detail}\nafter {:?}\nredraw {:?}",
            String::from_utf8_lossy(&stream),
            String::from_utf8_lossy(&redraw)
        )));
    }
    Ok(())
}

/// Every recorded program in `fixtures/`, at each checkpoint: a snapshot
/// reproduces the visible state, and a follower fed `redraw` of the damage
/// since the last checkpoint — a snapshot after a switch of screens — stays
/// level.
#[test]
fn fixtures_round_trip() {
    let mut failures = Vec::new();
    for common::Fixture { name, output, rows, cols, stops, .. } in common::fixtures() {
        let mut term = Terminal::new(rows, cols, 1_000);
        let mut follower = Terminal::new(rows, cols, 0);
        let (mut screen, mut fed) = (false, 0);
        for stop in stops {
            term.advance(&output[fed..stop]);
            fed = stop;
            let damage = term.take_damage();
            if term.modes().alt_screen == screen {
                follower.advance(&term.redraw(&damage));
            } else {
                screen = term.modes().alt_screen;
                follower = Terminal::new(rows, cols, 0);
                follower.advance(&term.snapshot());
            }
            if unreachable_cursor(&term) {
                continue;
            }
            let mut copy = Terminal::new(rows, cols, 0);
            copy.advance(&term.snapshot());
            let plain = |term: &Terminal| Visible {
                title: String::new(),
                icon: String::new(),
                colors: Vec::new(),
                ..visible(term)
            };
            let snapshot = difference(&visible(&term), &visible(&copy));
            let redraw = difference(&plain(&term), &plain(&follower));
            if let Some(detail) =
                snapshot.map(|d| ("snapshot", d)).or(redraw.map(|d| ("redraw", d)))
            {
                failures.push(format!("{name} at byte {stop}, {}: {}", detail.0, detail.1));
                break;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 500, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn a_snapshot_reproduces_the_visible_state((rows, cols, tokens) in stream(Profile::Full)) {
        check(rows, cols, &tokens)?;
    }

    #[test]
    fn redraws_of_the_damage_keep_a_follower_level(
        (rows, cols, before) in stream(Profile::Full),
        after in prop::collection::vec(common::token(Profile::Full, 6, 20), 0..60),
    ) {
        check_redraw(rows, cols, &before, &after)?;
    }
}

#[test]
#[ignore]
fn many_redraws_keep_a_follower_level() {
    let config =
        ProptestConfig { cases: 20_000, failure_persistence: None, ..ProptestConfig::default() };
    proptest!(config, |(
        (rows, cols, before) in stream(Profile::Full),
        after in prop::collection::vec(common::token(Profile::Full, 6, 20), 0..60),
    )| {
        check_redraw(rows, cols, &before, &after)?;
    });
}

#[test]
#[ignore]
fn many_snapshots_reproduce_the_visible_state() {
    let config =
        ProptestConfig { cases: 50_000, failure_persistence: None, ..ProptestConfig::default() };
    proptest!(config, |((rows, cols, tokens) in stream(Profile::Full))| {
        check(rows, cols, &tokens)?;
    });
}
