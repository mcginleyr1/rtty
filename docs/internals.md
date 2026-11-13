# Internals

What each source file in `src/` does and how the files fit together, why GPUI is pinned to one
release, the two patched dependencies, and the licences of what rtty builds on.

## How it works

[GPUI](https://gpui.rs) (Zed's UI framework) draws rtty, [rtty-vt](../vt/README.md), our own
terminal engine in `vt/`, holds the terminal state, and
[portable-pty](https://crates.io/crates/portable-pty) opens the pty. rtty's own code is in `src/`:

- `pane.rs` — one terminal, self-contained: the child on a pty (portable-pty), the rtty-vt
  `Terminal` behind a mutex, and three threads. The reader feeds the pty's output to
  `Terminal::advance` and sends the engine's replies to the writer in stream order; the writer
  owns the pty's input side, so a program that stops reading stalls neither the reader nor the
  window; the waiter reports the child's exit. A channel carries notices to the window: `Output`
  (at most one in flight until the window draws), the engine's other events (title, OSC 52
  clipboard, cursor style, mode changes) and the exit. The pane also keeps the view state a
  window needs per terminal: where the view sits in the scrollback (an absolute row, so it stays
  on its text while output arrives) and the selection. `Pane::attach` makes the same pane of a
  tutti pane instead, with no threads of its own: the window's connection to the session streams
  it into the terminal and takes what is typed, resizing votes, and dropping it ends nothing;
  nothing above the pane knows the difference but for its title and its size. `Pane::held` is a
  pane that only says why it shows no tutti pane.
- `tutti.rs` — a window's connection to a tutti session, on tutti's own wire crate (`tutti-wire`:
  the frames, the requests, responses and events, and the revision check); `Session`, that
  connection, opened off the main thread with its revision checks, and every pane stream on it. Its one reader hands each pane
  frame, and each event of a stream (its rows, its exit, a refusal by its `req`), to the stream of
  the pane it names, ported from tutti's `pane attach --inline`: scrollback rows before each
  snapshot, snapshots less what the daemon keeps, raw reads with the engine's replies sent back.
  The session's snapshots and notes go to the panes shown (their titles, pills, key claims and
  notifications), and its layout, focus, edges and other refusals to the window (`Told`). Unit
  tested against a stand-in daemon over socket pairs.
- `title.rs` — what a pane is called: its program's title, else the command line of the process
  in its foreground (`Pane::foreground`), else its shell's directory; unit tested.
- `render.rs` — what a frame draws, independent of GPUI and unit tested: per row, background spans
  and text runs with their resolved colors and decorations, the characters rtty draws itself, and
  the cursor. A program's colors (OSC 4/10/11/12) win over the scheme's; DECSCNM swaps the
  defaults. A Unicode placeholder's cell draws no glyph: its image does. A row also carries its
  cells' borders, their colors resolved.
- `glyphs.rs` — box drawing, block elements and the powerline separators as rectangles and paths
  on a cell's device pixels, from the Unicode charts; unit tested.
- `borders.rs` — [cell borders](borders.md) as rectangles and paths on the grid's device pixels,
  from the rows in view: which of two strokes on a shared edge is drawn, how lines meet at corners
  and the cell's centre, rounded joints and what they cut off; no GPUI in it, unit tested. The
  view paints them each frame, so a row's cached line never depends on its neighbours.
- `selection.rs` — selections by absolute `(row, col)`, by cells, words, lines or a block,
  copied with `Terminal::text` (a block row by row); unit tested.
- `search.rs` and `links.rs` — matches of a query over a pane's rows, as cells by absolute row
  (soft wraps joined, wide characters whole), and the link on a cell (OSC 8, else a URL in its
  line, else a path with its line and column to a file that exists) with what a click does with
  it (open, reveal in Finder, edit, ignore) and what to show of a disguised one; both unit tested,
  and neither opens anything.
- `editor.rs` — editing a file at a line: which Neovim to believe, the msgpack-rpc call (with its
  deadline) that has one running rtty's plugin open it, and what a new pane runs otherwise
  (`editor`, else `$EDITOR`, through the login shell); unit tested, against sh and a stand-in
  Neovim socket.
- `quick.rs` — what quick select finds on screen, as cells, and its labels, nearest the cursor
  first and none a prefix of another; unit tested. The view takes every key while they show
  (a keystroke interceptor, ahead of the bindings).
- `copy.rs` — copy mode: its cursor by absolute row, vi's motions over the rows held (words by
  the selection's rules, prompts by `prompt.rs`'s marks, matches by the search's), and what each
  key asks of the view (search, copy, leave); independent of GPUI but for keystrokes, and unit
  tested. The view takes the keys no binding takes, after the search bar's.
- `shell.rs` and `shell/` — the shell integration scripts, written into rtty's directory at
  startup, and how a login shell is started to load them (`Pane::spawn`); unit tested, and with
  real zsh and bash through a pty.
- `prompt.rs` — what the shell's OSC 133 marks say, as cells by absolute row: where prompts start,
  the last command's output, a command's command line, and how many arrows a click on the command
  line is from the shell's cursor; a run of doubled marks counts once. Unit tested.
- `notify.rs` — the dock's attention and progress badge, and macOS notifications, which need an
  app bundle.
- `progress.rs` — what a program's OSC 9;4 and OSC 7501 reports say of its progress, and many
  panes' progress as one; unit tested.
- `view.rs` — `PaneView`, the GPUI view of one pane. A foreground task drains the pane's channel
  in batches: `Output` marks the view dirty, so however many arrive between two display
  refreshes, GPUI draws one frame (that is the coalescing). Each frame locks the terminal only to
  read the rows that changed (the engine's damage, plus the cursor's rows and rows that left the
  screen) into `render` lines, then shapes those outside the lock; unchanged rows reuse last
  frame's shaped runs (see [Ligatures](fonts.md#ligatures)). A synchronized update (mode 2026) keeps the
  last frame up until it ends, or for 150 ms. Images (`Terminal::placements`) are read under the
  same lock and drawn by z beneath or over the text, each engine image one GPUI image kept by its
  generation until the engine lets it go ([Images](images.md)). The view is also GPUI's text input
  handler, for committed text and IME marked text.
- `layout.rs` — a window's tabs, each a tree of splits with ratios, in cells, with no GPUI in it
  and unit tested: where each pane and divider goes in an area (a divider is one cell; no pane
  gets fewer than two across a split), split and close with the cascade to tab and window, a
  closed pane or tab put back where it was, a divider dragged to a cell or moved by a resize, focus by direction and back to the last pane, a
  swap with the pane that way, zoom; and a tab mirroring tutti's split tree (`Layout::mirror`),
  keeping its own focus and zoom.
- `session.rs` — the windows' layout as the next launch restores it (the layout's own split trees,
  serialized, and the tutti panes and sessions shown), read back checked and written whole; unit
  tested.
- `tabbar.rs` — the bar in the titlebar as rectangles: the pills, their close buttons and the `+`
  for a window's width and tabs, and what a point is over; no GPUI in it, unit tested.
- `window.rs` — `TerminalWindow`: one window's tabs, the bar (drawn where `tabbar` puts it, in a
  transparent titlebar, the traffic lights moved to its middle) and the dividers, and the bound
  chords as GPUI actions. It shares the window's cells out with `layout`, and every pane's grid
  and pty, shown or not, follow its cells on every resize. Each pane draws as a cached GPUI view,
  so output in one pane redraws that pane alone. `focus_toward` is the one call for moving focus
  by direction: the keys, `rtty cli` and so the editors all go through it. `answer` handles
  `rtty cli`'s requests on the main thread. `confirm` is the one way a close asks first (its
  wording, `question`, unit tested), and the last close waits in an app-wide slot, its panes'
  views alive and a closed window ordered out, until `undo-close` or its timer. A window keeps its
  connections to tutti sessions (`attach_tutti`, `opened`, `told`); a window that is a session
  (`Mirror`) lays out its tabs from tutti's (`mirror_layout`) and asks tutti where it would change
  its own layout.
- `control.rs` and `cli.rs` — the socket (one JSON line each way, served on a thread of its own
  and answered on GPUI's main thread) and `rtty cli`'s commands, both unit tested.
- `scheme.rs` and `config.rs` — color schemes and the config file, whose key bindings come to
  one action per chord (the defaults, `keys`, then `[bind]`) that `main.rs` binds in GPUI.
  `main.rs` turns them into the `Settings` global every window and pane reads as it draws and
  acts, and at a reload replaces it and the keymap whole.
- `app.rs` — rtty as a macOS app: the menu bar, a table of titles and `[bind]` actions, each
  item the GPUI action its chord runs (`gpui_action` in `main.rs`), so macOS shows the chord; and
  `applicationShouldTerminate:`, added to GPUI's application delegate at startup, which asks
  `window::quit`'s question and answers AppKit later (`NSTerminateLater`); and Secure Keyboard
  Entry, held from one place (`refresh_secure_input`) as rtty's activation, the toggle and the
  focused pane's password prompt (`Pane::reads_password`) change; and the global hotkey.
  `app::configure`, which `main.rs`'s `configure` runs at launch and at every reload once the
  keymap is bound, puts their config in effect: the menu bar again, the toggle, the hotkey.
- `keys.rs` — GPUI keystrokes as rtty-vt's key events, which `rtty_vt::input` encodes (legacy
  xterm, modifyOtherKeys, the kitty protocol; mouse reports in every encoding too), and what stays
  rtty's: Cmd chords but a claimed one's (Cmd as kitty's super), and text, which goes through the
  input handler; paste wrapping and dropped paths escaped for a shell, unit tested (the escaping
  against sh and zsh themselves).
- `script.rs` — `--script`, which drives the real window the way a person would and dumps what it
  draws; how rtty is checked against Neovim, Emacs and the rest without a screenshot.

## The GPUI pin

`gpui = "=0.2.2"` from crates.io, with the `runtime_shaders` feature. 0.2.2 is the newest
release on crates.io and builds on this Mac with the stable toolchain, which a git rev of Zed
would not guarantee (Zed tracks its own `rust-toolchain`, and a rev drags in its whole workspace).
`runtime_shaders` compiles GPUI's Metal shaders when the app starts instead of at build time:
Xcode 26 ships the offline `metal` compiler as a separate download
(`xcodebuild -downloadComponent MetalToolchain`), so without the feature the build fails on a
machine that has not fetched it. The cost is a shader compile at startup.

## Patched dependencies

Two of GPUI's dependencies, `block` 0.1.6 and `proc-macro-error2` 2.0.1, each have a line Rust
warns will become an error (a static of an uninhabited type; a re-exported private extern crate),
and neither has a release that fixes it. `[patch.crates-io]` builds copies of them from
[`patches/`](../patches/README.md) with that line changed (and, in `block`, its `extern` blocks given
their `"C"`), so a build prints no future-incompatibility warning.

## Licensing

Licensing: GPUI and rtty-vt are Apache-2.0, portable-pty is MIT, global-hotkey is Apache-2.0 or
MIT, tutti-wire is MIT or Apache-2.0; the image crates (`png` and `miniz_oxide` in rtty-vt, `image` for GPUI's frames) are MIT or
Apache-2.0. rtty takes nothing from Zed's GPL `terminal` / `terminal_view` crates, and nothing
from kitty's GPL source: the graphics protocol is implemented from its documentation.
