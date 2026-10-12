# rtty

A GPU terminal emulator in Rust: [GPUI](https://gpui.rs) (Zed's UI framework) draws it,
[rtty-vt](vt/README.md), the terminal engine in `vt/` that tutti shares, holds the terminal
state, and [portable-pty](https://crates.io/crates/portable-pty) opens the pty. rtty is on its
way to being a native GUI front end for tutti, the tmux-for-AI-agents daemon; today it is one
window, one terminal, a local pty.

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
| `--config FILE` | read this config file instead of `~/.config/rtty/config.toml` (it must exist) |
| `--font NAME` | font family (default Menlo); an unknown family is an error |
| `--size PT` | font size in points (default 14) |
| `--ligatures on\|off` | the font's programming ligatures: on enables `calt` and `liga`, off disables `calt`, `liga` and `dlig`; default on |
| `-e PROGRAM [ARGS...]` | run the program instead of the login shell; the window closes when it exits |
| `--stats` | once a second while frames are drawn, print frames, layout and paint ms/frame |
| `--trace FILE` | write every read from the pty (`<`) and write to it (`>`), escaped, one line each with the milliseconds since start: what a program asks and what it was told |
| `--script FILE` | drive the window from a script instead of a person: wait for text, keys and typed text (through GPUI's keystroke dispatch), clicks, drags and the wheel, focus in and out, an appearance flip, a resize, and `dump`, which writes what the next frame draws (runs, colors, underlines, the cursor) and the modes it is drawn from; the step list is in `src/script.rs` |
| `--smoke` | run `/bin/sh -c 'printf hello-rtty; exit 7'` through the same pty and terminal with no window; pass when the grid shows `hello-rtty` and the exit code is 7 |
| `--bench [COMMAND]` | time COMMAND (default `seq -f %.0f 1 2000000`) into a pipe, the parser alone on its output, and the whole pty pipeline |

`--font`, `--size` and `--ligatures` override the config file. `TERM` is `xterm-256color`,
`COLORTERM` is `truecolor`, `TERM_PROGRAM` is `rtty` (with `TERM_PROGRAM_VERSION`), and the
variables that would tell a program it runs inside a multiplexer rtty was started from (`TMUX`,
`TMUX_PANE`, `STY`, `ZELLIJ`, `TUTTI_PANE`, `TUTTI_SESSION`) are removed: Neovim, for one, skips
its undercurl probe inside tmux.

## Configuration

`~/.config/rtty/config.toml` (`$XDG_CONFIG_HOME/rtty/config.toml` when that is set). Every key is
optional; an unknown key, an unknown scheme name or a malformed file is an error that names it,
and rtty does not start.

```toml
font = "FiraCode Nerd Font"
size = 12
ligatures = true
scheme_dark = "OneDark Pro"       # the default
scheme_light = "Solarized Light"  # the default
# scheme = "Solarized Dark"       # one scheme whatever the OS appearance; not with the two above
```

## Color schemes

Schemes are WezTerm's TOML format, so WezTerm scheme files work unchanged: `[colors]` with
`foreground`, `background`, `ansi` (8), `brights` (8), optionally `cursor_bg`, `cursor_fg`,
`cursor_border`, `selection_bg`, `selection_fg` and `[colors.indexed]` (index → color), and
`[metadata] name`. Keys rtty does not draw (`tab_bar`, `scrollbar_thumb`, …) are ignored. Colors
are `#rgb`, `#rrggbb` or `#rrggbbaa` (alpha ignored).

rtty looks for a scheme name in this order, first match wins:

1. Built in: **OneDark Pro** and **One Light** (onedarkpro.nvim's own WezTerm files, MIT, Oli
   Morris), **Solarized Dark** and **Solarized Light** (the canonical palette and 16-color mapping,
   MIT, Ethan Schoonover). Each file in `schemes/` carries its source and license.
2. `~/.config/rtty/colors/*.toml`.
3. WezTerm's `~/.config/wezterm/colors/*.toml`, so an existing WezTerm setup needs no copying.

A scheme is found by its `metadata.name`, else by its file stem (`onedarkpro` finds
`onedarkpro.toml` whatever it calls itself). Every file in those directories is read at startup,
so a malformed one stops rtty with its path and the offending key.

**Following the OS.** rtty uses `scheme_dark` while the window's appearance is dark and
`scheme_light` while it is light, and switches the moment macOS flips, with no restart. Cells keep
their color indices, so everything already on screen recolors on the next frame. Colors a program
set itself (OSC 4/10/11/12) are the program's, not the scheme's, and survive a flip; a program
that cares re-sets them when it hears about the flip (below). OSC 10/11/12/4 queries answer with
the current scheme.

**Telling programs (DEC mode 2031).** rtty implements contour's color palette update
notifications. `CSI ? 996 n` asks for the scheme and gets `CSI ? 997 ; 1 n` (dark) or
`CSI ? 997 ; 2 n` (light). A program that set `CSI ? 2031 h` gets that report, unsolicited, each
time the scheme changes, so Neovim and friends can switch `background` live. `CSI ? 2031 l` or RIS
turns it off, and DECRQM (`CSI ? 2031 $ p`) reports the mode's state. Dark or light is the
scheme's background, not the OS setting, so a pinned light scheme answers light on a dark Mac.
rtty-vt answers all of these itself: rtty hands it the scheme's palette and leaning at startup
and on every flip.

## Keys

Ctrl keys belong to the terminal; Cmd chords belong to the app.

| Key | Action |
| --- | --- |
| `Cmd+Q` | quit |
| `Cmd+C` | copy the selection |
| `Cmd+V` | paste (bracketed when the program asked for it) |
| `Cmd+=` / `Cmd+-` | font size up / down |
| mouse drag | select (double click: word, triple click: line); Shift+drag when a program takes the mouse |
| mouse buttons | reported to a program that asked (modes 9, 1000, 1002, 1003, in the default, UTF-8, SGR, urxvt or SGR-pixel encoding) |
| wheel | scrollback on the primary screen; wheel reports to a program that asked for mouse reports; arrow keys to any other full-screen program (alternate scroll, on by default) |

The terminal gets printable text (through the platform input handler, so dead keys and IME
composition go through macOS), Enter, Backspace, Tab, Shift+Tab, Esc, the arrows,
Home/End/PgUp/PgDn/Insert/Delete, F1–F12 (xterm encodings, with modifier parameters), Ctrl+letters
and Ctrl+`[ \ ] ^ _ @ space`, and Option as Alt: an ESC prefix (so Option+B is word-back in a shell,
and Option no longer types `∫`). Application cursor mode (DECCKM) switches the arrows to `ESC O`.
A held key repeats instead of opening macOS's accent picker.

Programs that ask for more get it:

- **xterm's modifyOtherKeys** (`CSI > 4 ; n m`, which Emacs turns on at level 1). Level 1 sends
  `CSI 27 ; mod ; code ~` for Ctrl with keys that have no control character (`C-;`, `C-.`,
  `C-1`) and for Ctrl or Shift with Enter and Tab; every control character stays as it was. Level
  2 sends it for every key with Ctrl or Alt.
- **The kitty keyboard protocol** (`CSI > flags u`; Neovim pushes flags 3). Disambiguate: Esc is
  `CSI 27 u`, keys with Ctrl or Alt are `CSI code ; mod u` (so `C-i` is not Tab and `S-Enter` is
  not Enter), and the other keys use the `CSI 1 ; mod X` and `CSI n ; mod ~` forms; plain text
  and unmodified Enter, Tab and Backspace stay as they were. Event types add repeats (`:2`) and
  releases (`:3`) of those keys. All keys as escape codes, alternate (shifted) keys and associated
  text are encoded too, though no program checked here uses them.

GPUI does not report keypad keys apart from the main keys, so application keypad mode (DECKPAM)
has nothing to change, and a shifted symbol arrives as itself (`!`, not Shift+`1`).

## How it works

- `pane.rs` — one terminal, self-contained: the child on a pty (portable-pty), the rtty-vt
  `Terminal` behind a mutex, and three threads. The reader feeds the pty's output to
  `Terminal::advance` and sends the engine's replies to the writer in stream order; the writer
  owns the pty's input side, so a program that stops reading stalls neither the reader nor the
  window; the waiter reports the child's exit. A channel carries notices to the window: `Output`
  (at most one in flight until the window draws), the engine's other events (title, OSC 52
  clipboard, cursor style, mode changes) and the exit. The pane also keeps the view state a
  window needs per terminal: where the view sits in the scrollback (an absolute row, so it stays
  on its text while output arrives) and the selection.
- `render.rs` — what a frame draws, independent of GPUI and unit tested: per row, background spans
  and text runs with their resolved colors and decorations, and the cursor. A program's colors
  (OSC 4/10/11/12) win over the scheme's; DECSCNM swaps the defaults.
- `selection.rs` — selections by absolute `(row, col)`, by cells, words or lines, copied with
  `Terminal::text`.
- `mouse.rs` — mouse reports in every encoding, unit tested.
- `view.rs` — `PaneView`, the GPUI view of one pane. A foreground task drains the pane's channel
  in batches: `Output` marks the view dirty, so however many arrive between two display
  refreshes, GPUI draws one frame (that is the coalescing). Each frame locks the terminal only to
  read the rows that changed (the engine's damage, plus the cursor's rows and rows that left the
  screen) into `render` lines, then shapes those outside the lock; unchanged rows reuse last
  frame's shaped runs (see [Ligatures](#ligatures)). A synchronized update (mode 2026) keeps the
  last frame up until it ends, or for 150 ms. Resizing recomputes rows/cols from the cell metrics
  and resizes the terminal and the pty. The view is also GPUI's text input handler, for committed
  text and IME marked text.
- `window.rs` — `TerminalWindow`, a window's panes. One shows; tabs and splits will lay out
  several, each pane view in its own part of the window.
- `scheme.rs` and `config.rs` — color schemes and the config file.
- `keys.rs` — the key encoder (GPUI keystroke → bytes: legacy xterm, modifyOtherKeys, the kitty
  protocol) and paste wrapping, unit tested.
- `script.rs` — `--script`, which drives the real window the way a person would and dumps what it
  draws; how rtty is checked against Neovim, Emacs and the rest without a screenshot.

## Measurements (Apple silicon Mac, release build)

`rtty --bench` (80×24 grid, 10k rows of scrollback; one run each, repeat runs vary by ~10%):

| Workload | into a pipe | parse only (memory) | pty + reader + rtty-vt |
| --- | --- | --- | --- |
| `seq -f %.0f 1 2000000` (16.9 MB) | 235 ms | 71 ms (238 MB/s) | 2020 ms (8.4 MB/s) |
| `seq -f %.0f 1 2000000 \| cat` | 192 ms | 67 ms (251 MB/s) | 201 ms (84 MB/s) |
| 20k lines, a color/bold/italic change every word (32.7 MB) | 618 ms | 97 ms (336 MB/s) | 956 ms (34 MB/s) |
| the same 20k lines from a file (`cat`) | 14 ms | 96 ms (339 MB/s) | 186 ms (176 MB/s) |

The pipeline keeps up with the producer. Plain `seq` on a tty is slow because stdio line-buffers
a tty: two million one-line `write(2)`s into the tty layer (`script -q /dev/null seq …` takes
2.8 s with no terminal at all). Through `cat`, the same bytes take ~200 ms end to end. The parser
alone runs at 240–340 MB/s, well above what a pty delivers. On alacritty_terminal (rtty before
rtty-vt) the same parse took 99–105 ms and `cat` of the file 207 ms.

In the window (900×600, Menlo 14, launch to exit with `/usr/bin/time`, three runs each): five
copies of the dense file (163 MB) drain in 1.24 s with 0.89 s of user CPU (alacritty_terminal:
1.38 s, 1.24 s), five of `seq`'s output (74 MB) in 0.80–0.86 s (1.10–1.13 s). An idle window
over a quiet program uses 0.13 s of CPU in 20 s once started, as before.

Drawing (`--stats`): a frame re-reads and reshapes only the rows the engine reports changed,
plus the cursor's. A steady one-line redraw costs ~0.06–0.09 ms layout + 0.2 ms paint per frame;
the dense flood at 6 pt (~250×75 cells, a style change every word) 1.0 ms layout + 8.3 ms paint
(alacritty_terminal: 1.6 ms + 8.3 ms). CPU time; GPU time is not measured. GPUI stops drawing
while the window is occluded, so `--stats` prints nothing for a hidden window.

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

Works: one window running a shell or `-e` program with colors, attributes, wide chars,
ligatures, every underline style (single, double, curly, dotted, dashed; SGR 4:0–4:5) in its own
color (SGR 58, so Neovim's diagnostics undercurl in theirs), overline, selection and copy/paste,
scrollback, resize with reflow, mouse reports, window title from OSC 0/2, OSC 52 copy,
synchronized updates, focus reports (mode 1004: `CSI I` and `CSI O` as the window is activated
and deactivated and as the pane gains and loses the keyboard), the keyboard protocols above, a
config file, and WezTerm-format color schemes that follow the macOS appearance live (with DEC
mode 2031 reports).

The cursor takes the shape a program sets with DECSCUSR (block, bar or underline; Neovim's
insert mode is a bar), and a hollow block while the window or pane is not focused. It blinks when
DECSCUSR asks for blinking, or, while DECSCUSR is at its default, when ATT610 (`CSI ? 12 h`) does;
it stops blinking and stays on after 10 s without a key, so an idle window draws nothing.

Not yet: IME is wired through GPUI's input handler (composition is drawn at the cursor and the
candidate window is placed there) but unverified with a real input method; emoji and CJK rely on
CoreText's font fallback and an emoji may overhang its two cells; no bell, URL detection, tabs or
splits; macOS only.

## The GPUI pin

`gpui = "=0.2.2"` from crates.io, with the `runtime_shaders` feature. 0.2.2 is the newest
release on crates.io and builds on this Mac with the stable toolchain, which a git rev of Zed
would not guarantee (Zed tracks its own `rust-toolchain`, and a rev drags in its whole workspace).
`runtime_shaders` compiles GPUI's Metal shaders when the app starts instead of at build time:
Xcode 26 ships the offline `metal` compiler as a separate download
(`xcodebuild -downloadComponent MetalToolchain`), so without the feature the build fails on a
machine that has not fetched it. The cost is a shader compile at startup.

Licensing: GPUI and rtty-vt are Apache-2.0, portable-pty is MIT. rtty takes nothing from Zed's
GPL `terminal` / `terminal_view` crates.

## Roadmap

1. **Attach to tutti.** Replace the local pty with a raw single-pane stream from tutti-server
   (being built now): the stream's bytes feed the same `Terminal` the pty reader feeds today, and
   key bytes go back over the socket instead of into the pty. `Pane` is the seam. tutti-server
   answers DA1/DA2/DSR/CPR itself and strips those queries from the raw stream, so rtty must not
   answer them on an attached pane (today the engine's replies go straight back into the pty).
2. **Sidebar and agent panels** as GPUI views beside the terminal: tutti's project/agent tree with
   state dots, the waiting queue, the changed-files panel.
3. **Layout on Cmd chords** (split, focus, zoom, tabs) so every Ctrl key stays with the panes.
