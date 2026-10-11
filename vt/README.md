# rtty-vt

The terminal core shared by tutti's daemon, tutti's TUI and the rtty GPU
terminal: the bytes a program writes go in, a grid with absolute-numbered
scrollback, modes, replies and events come out. No IO. The byte-level parser
is `vte`'s DEC state machine; everything above it is this crate.

```rust
let mut term = rtty_vt::Terminal::new(24, 80, 10_000);
term.advance(output);                    // the pty's bytes, in any chunks
for event in term.take_events() { … }    // replies for the pty, titles, marks…
let rows = term.take_damage();           // visible rows changed since last time
let wire = term.redraw(&rows);           // escape bytes bringing a follower level
let fresh = term.snapshot();             // escape bytes for a fresh terminal
```

- Rows: every row the primary screen ever showed has an absolute index;
  `screen_top()` and `oldest_row()` never go backwards; `rows(from, to)` reads
  scrollback and screen; OSC 133 marks travel with their rows.
- Resize reflows the primary screen (scrollback included); the alternate
  screen is cut or padded.
- Unicode: `unicode-width` per character, grapheme clusters by
  `unicode-segmentation` for combining marks and emoji sequences; see the
  crate documentation for the width rules and VS16.

Tests: per-feature tests from the specs; differential tests against
alacritty_terminal over a randomized grammar and the recorded programs in
`fixtures/` (`DIVERGENCES.md` lists the known differences); checks that
`snapshot()` and `redraw()` reproduce the visible state, over random streams
and at every fixture checkpoint; property tests that resizes keep
lines and numbering, and that no input panics; and an answer to every query
the recorded programs sent. The long campaigns run with
`cargo test --release -- --ignored`; throughput against alacritty_terminal and
vt100 with `cargo bench`.

The fixture tests read `fixtures/`, or the directories `RTTY_VT_FIXTURES`
lists (a path list, as `PATH` is):
`cargo test --config 'env.RTTY_VT_FIXTURES="/one/fixtures:/other/fixtures"'`.
