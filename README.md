# rtty

A GPU terminal emulator in Rust: [GPUI](https://gpui.rs) (Zed's UI framework) draws it,
[alacritty_terminal](https://crates.io/crates/alacritty_terminal) holds the terminal state and
owns the pty. v0 is the spike that proves this stack before rtty becomes a native GUI front end
for tutti, the tmux-for-AI-agents daemon: one window, one terminal, a local pty.

## Build and run

macOS only for now (GPUI's Metal renderer). Needs a Rust toolchain with edition 2024 and Xcode's
SDK; the offline Metal compiler is **not** needed (see [the GPUI pin](#the-gpui-pin)).

```sh
cargo run --release                     # your login shell
cargo run --release -- --font "SF Mono" --size 13
cargo run --release -- --font "FiraCode Nerd Font Mono" --ligatures off
cargo run --release -- -e htop          # a program instead of the shell
cargo run --release -- --smoke          # headless pipeline check, exits 0 or 1
cargo run --release -- --bench          # headless pipeline throughput
cargo run --release -- --stats          # print frame timings to stderr while drawing
```

| Flag | Meaning |
| --- | --- |
| `--font NAME` | font family (default Menlo); an unknown family is an error |
| `--size PT` | font size in points (default 14) |
| `--ligatures on\|off` | the font's programming ligatures: on enables `calt` and `liga`, off disables `calt`, `liga` and `dlig`; default on |
| `-e PROGRAM [ARGS...]` | run the program instead of the login shell; the window closes when it exits |
| `--stats` | once a second while frames are drawn, print frames, layout and paint ms/frame |
| `--smoke` | run `/bin/sh -c 'printf hello-rtty; exit 7'` through the same pty and terminal with no window; pass when the grid shows `hello-rtty` and the exit code is 7 |
| `--bench [COMMAND]` | time COMMAND (default `seq -f %.0f 1 2000000`) into a pipe, the parser alone on its output, and the whole pty pipeline |

`TERM` is `xterm-256color`, `COLORTERM` is `truecolor`.

## Keys

Ctrl keys belong to the terminal; Cmd chords belong to the app.

| Key | Action |
| --- | --- |
| `Cmd+Q` | quit |
| `Cmd+C` | copy the selection |
| `Cmd+V` | paste (bracketed when the program asked for it) |
| `Cmd+=` / `Cmd+-` | font size up / down |
| mouse drag | select (double click: word, triple click: line) |
| wheel | scrollback on the primary screen; wheel reports to a program that asked for mouse reports; arrow keys to any other full-screen program (alternate scroll) |

The terminal gets printable text (through the platform input handler, so dead keys and IME
composition go through macOS), Enter, Backspace, Tab, Shift+Tab, Esc, the arrows,
Home/End/PgUp/PgDn/Insert/Delete, F1–F12 (xterm encodings, with modifier parameters), Ctrl+letters
and Ctrl+`[ \ ] ^ _ @ space`, and Option as Alt: an ESC prefix (so Option+B is word-back in a shell,
and Option no longer types `∫`). Application cursor mode (DECCKM) switches the arrows to `ESC O`.
A held key repeats instead of opening macOS's accent picker.

## How it works

- `session.rs` — spawns the child on a pty (`alacritty_terminal::tty`) and runs alacritty's
  `EventLoop`: a reader thread parses pty output into the `Term` under a `FairMutex` and sends
  `Wakeup` events. A channel carries the terminal's events to the UI.
- `view.rs` — the GPUI view. A foreground task drains the event channel in batches: `Wakeup`
  marks the view dirty, so however many arrive between two display refreshes, GPUI draws one frame
  (that is the coalescing). Each frame locks the term only to read the visible cells into spans,
  then shapes outside the lock: per row, runs of same-background cells become one quad, and runs
  of same-style cells become one shaped text run (see [Ligatures](#ligatures)). Resizing the
  window recomputes rows/cols from the cell metrics and resizes the term and the pty. The view is
  also GPUI's text input handler, for committed text and IME marked text.
- `keys.rs` — the key encoder (GPUI keystroke → bytes) and paste wrapping, unit tested.
- `palette.rs` — the default 16/256 palette and named colors; programs can override any slot
  (OSC 4/10/11/12).

## Measurements (Apple silicon Mac, release build)

`rtty --bench` (80×24 grid, 10k lines of scrollback; one run each, repeat runs vary by ~20%):

| Workload | into a pipe | parse only (memory) | pty + reader + Term |
| --- | --- | --- | --- |
| `seq -f %.0f 1 2000000` (16.9 MB) | 196 ms | 103 ms (164 MB/s) | 2253 ms (7.5 MB/s) |
| `seq -f %.0f 1 2000000 \| cat` | 190 ms | 99 ms (171 MB/s) | 210 ms (80 MB/s) |
| 20k lines, a color/bold/italic change every word (32.7 MB) | 625 ms | 107 ms (307 MB/s) | 882 ms (37 MB/s) |

The pipeline keeps up with the producer. Plain `seq` on a tty is slow because stdio line-buffers
a tty: two million one-line `write(2)`s into the tty layer (`script -q /dev/null seq …` takes
2.8 s with no terminal at all). Through `cat`, the same bytes take ~200 ms end to end. The parser
alone runs at 160–300 MB/s, well above what a pty delivers.

Drawing (`--stats`, 900×600 window, 14 pt): during the `seq` flood 117 frames/s at 0.33 ms
layout + 0.06 ms paint per frame (CPU time; GPU time is not measured); a steady colorful redraw
~0.14 ms + 0.16 ms; the dense color flood at 6 pt (~250×75 cells, a style change every word)
88 frames/s at 1.5 ms + 8.3 ms. Rendering does not slow the pipeline: the dense flood
finishes as fast in the window (0.9 s) as headless. GPUI stops drawing while the window is
occluded, so `--stats` prints nothing for a hidden window.

## Ligatures

Programming ligatures (`->`, `=>`, `!=`, `===`, `<=`, `|>`, …) render with fonts that have them
(checked with Fira Code, JetBrains Mono, Iosevka and Victor Mono). `--ligatures off` turns off the
font's `calt`, `liga` and `dlig` features.

- Each run of single-width cells with the same foreground, background and font style goes
  through CoreText as one string, so the font's contextual alternates can form.
- The glyphs stay on the grid: every glyph is placed on the cell of the character it was shaped
  from (the shaper reports each glyph's first character). A ligature over N cells starts on its
  first cell, the font draws it across N cell widths (the cell width is the font's own advance),
  and the glyph after it starts N cells later, whatever advances the font or a fallback font
  reported. The run is shaped without GPUI's `force_width`, which gives every glyph one cell by
  glyph order and so shifts everything after a glyph that stands for several characters; instead
  the glyph positions in the shaped layout are rewritten to their cells, and GPUI paints each
  glyph (`Window::paint_glyph`) at the run's origin plus that position. Most coding fonts keep one
  glyph per cell (a spacer plus the ligature glyph), and their glyphs already sit on the grid;
  the snapping matters for fonts whose ligatures replace N glyphs with one, and for fallback
  glyphs with other advances.
- The cell under a visible cursor is shaped on its own, so the cursor shows the plain character it
  sits on (`-` or `>` of an `->`), as kitty, WezTerm and Ghostty do. A selection edge ends a run too, since
  the selection changes the background.
- No ligature forms across a change of color, background, bold/italic, underline or strikethrough;
  across a wide (CJK, emoji) or combining-mark cell, which are shaped alone; or across a line
  wrap.

## Status

v0 works: one window running a shell or `-e` program with colors, attributes, wide chars,
ligatures, cursor shapes, selection and copy/paste, scrollback, resize, window title from
OSC 0/2.

Not yet: IME is wired through GPUI's input handler (composition is drawn at the cursor and the
candidate window is placed there) but unverified with a real input method; mouse clicks are not
reported to programs (only the wheel is); emoji and CJK rely on CoreText's font fallback and an
emoji may overhang its two cells; no blinking cursor, bell, kitty keyboard protocol, URL
detection, config file, tabs or splits; macOS only.

## The GPUI pin

`gpui = "=0.2.2"` from crates.io, with the `runtime_shaders` feature. 0.2.2 is the newest
release on crates.io and builds on this Mac with the stable toolchain, which a git rev of Zed
would not guarantee (Zed tracks its own `rust-toolchain`, and a rev drags in its whole workspace).
`runtime_shaders` compiles GPUI's Metal shaders when the app starts instead of at build time:
Xcode 26 ships the offline `metal` compiler as a separate download
(`xcodebuild -downloadComponent MetalToolchain`), so without the feature the build fails on a
machine that has not fetched it. The cost is a shader compile at startup. `alacritty_terminal` is
pinned to `=0.26.0`.

Licensing: GPUI and alacritty_terminal are Apache-2.0. rtty takes nothing from Zed's GPL
`terminal` / `terminal_view` crates.

## Roadmap

1. **Attach to tutti.** Replace the local pty with a raw single-pane stream from tutti-server
   (being built now): the stream's bytes feed the same `Term` the pty reader feeds today, and key
   bytes go back over the socket instead of into the pty. `Session` is the seam. tutti-server
   answers DA1/DA2/DSR/CPR itself and strips those queries from the raw stream, so rtty must not
   answer them on an attached pane (today the `Term`'s replies go straight back into the pty).
2. **Sidebar and agent panels** as GPUI views beside the terminal: tutti's project/agent tree with
   state dots, the waiting queue, the changed-files panel.
3. **Layout on Cmd chords** (split, focus, zoom, tabs) so every Ctrl key stays with the panes.
