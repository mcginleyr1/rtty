# rtty

A GPU terminal emulator in Rust: [GPUI](https://gpui.rs) (Zed's UI framework) draws it,
[rtty-vt](vt/README.md), our own terminal engine in `vt/`, holds the terminal state, and
[portable-pty](https://crates.io/crates/portable-pty) opens the pty.

rtty is one half of a tmux replacement. The other half is tutti, the multiplexer for AI coding
agents: a TUI you run in any terminal, whose daemon keeps sessions alive across detaches.

- **rtty** is the terminal. Like WezTerm, it has its own tabs, windows and splits.
- **tutti** stays an app. Inside rtty it finds rtty and uses what rtty offers.
- **Both run the same engine.** rtty-vt is shared by rtty, tutti's daemon and tutti's TUI, so a
  program behaves the same in either.

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
rtty cli list                           # from inside a pane: the windows, tabs and panes
```

| Flag | Meaning |
| --- | --- |
| `--config FILE` | read this config file instead of `~/.config/rtty/config.toml` (it must exist) |
| `--font NAME` | font family (default Menlo); an unknown family is an error |
| `--size PT` | font size in points (default 14) |
| `--ligatures on\|off` | the font's programming ligatures: on enables `calt` and `liga`, off disables `calt`, `liga` and `dlig`; default on |
| `-e PROGRAM [ARGS...]` | run the program in the first pane instead of the login shell; its pane closes when it exits (new panes run the shell) |
| `--stats` | once a second while a pane draws frames, print its frames, layout and paint ms/frame (a line per pane that drew) |
| `--trace FILE` | write every read from a pty (`<`) and write to it (`>`), escaped, one line each with the milliseconds since start and the pane's number: what a program asks and what it was told |
| `--script FILE` | drive the window from a script instead of a person: wait for text, keys and typed text (through GPUI's keystroke dispatch, so Cmd chords split and switch panes, or with `nskey` as AppKit key events sent to the window itself, which take macOS's own path: key equivalent, text input context, key down), clicks, drags and the wheel at window cells (to the pane under them), clicks and hovers on the tab bar's pills, close buttons and `+`, focus in and out, an appearance flip, a resize, and `dump`, which writes the window's tabs, bar and layout and what the next frame draws in each shown pane (runs, colors, underlines, the cursor) and the modes it is drawn from; the step list is in `src/script.rs`. A scripted window does not take the keyboard from whatever app has it |
| `--activate` | with `--script`: take the keyboard as a normal launch does, for the checks that need a focused window (focus reports, the focused cursor) |
| `--smoke` | run `/bin/sh -c 'printf hello-rtty; exit 7'` through the same pty and terminal with no window; pass when the grid shows `hello-rtty` and the exit code is 7 |
| `--bench [COMMAND]` | time COMMAND (default `seq -f %.0f 1 2000000`) into a pipe, the parser alone on its output, and the whole pty pipeline |

`--font`, `--size` and `--ligatures` override the config file. `TERM` is `xterm-256color`,
`COLORTERM` is `truecolor`, `TERM_PROGRAM` is `rtty` (with `TERM_PROGRAM_VERSION`), and the
variables that would tell a program it runs inside a multiplexer rtty was started from (`TMUX`,
`TMUX_PANE`, `STY`, `ZELLIJ`, `TUTTI_PANE`, `TUTTI_SESSION`) are removed: Neovim, for one, skips
its undercurl probe inside tmux. `RTTY_PANE` and `RTTY_SOCKET` tell a pane's program which pane it
is and where [`rtty cli`](#rtty-cli-and-editors) reaches this rtty.

**The log.** rtty appends to `$TMPDIR/rtty-<uid>/rtty.log`, next to its socket (`echo
$TMPDIR` shows where `$TMPDIR` is; it is per user on macOS): when each rtty started and quit and
why, each pane's start, split, `Cmd+W` and its program's exit status, a tab's close button, and
every panic, on any thread, with its backtrace. Never what a pane shows or is sent. Past a megabyte it starts over,
keeping the last as `rtty.log.old`. When rtty vanishes, its last lines say whether it quit (and
why) or panicked (and where).

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
keys = "rtty"                     # the default; "tutti" adds Ctrl+h/j/k/l and Alt+h/j/k/l (Keys)
bell = "visual"                   # the default: BEL flashes the pane (and the dock, unfocused); or "none"
```

## Color schemes

Schemes are WezTerm's TOML format, so WezTerm scheme files work unchanged: `[colors]` with
`foreground`, `background`, `ansi` (8), `brights` (8), optionally `cursor_bg`, `cursor_fg`,
`cursor_border`, `selection_bg`, `selection_fg`, `split` (the line between panes),
`[colors.tab_bar]` (`background`, and `fg_color` and `bg_color` of `active_tab`, `inactive_tab`,
`inactive_tab_hover`, `new_tab` and `new_tab_hover`: the shown tab, the others, one under the
pointer, and the `+` button plain and under the pointer) and `[colors.indexed]` (index → color),
and `[metadata] name`. What a scheme leaves out of the tab bar and the split line is mixed from
its foreground and background: the bar is the background, a shade darker in a light scheme; the
shown tab is the pane's own colors, the other tabs dim their titles on the bar, and the `+` is
drawn as they are. A tab that should stand out (the shown one, one under the pointer, the `+`
under it) but has the bar's own background, as in WezTerm's flat tab bars, is lifted from the
bar, toward the foreground in a dark scheme and toward white in a light one, so it is lighter
either way. The folder before a lone tab's title is the scheme's blue. Keys rtty does not draw
(`scrollbar_thumb`, the tab bar's `inactive_tab_edge`, a tab's `intensity`, …) are ignored.
Colors are `#rgb`, `#rrggbb` or `#rrggbbaa` (alpha ignored).

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
| `Cmd+N` | new window |
| `Cmd+T` | new tab, after the current one |
| `Cmd+1`–`9` | show that tab |
| `Cmd+Shift+[` / `Cmd+Shift+]` | previous / next tab, round the ends |
| `Cmd+D` / `Cmd+Shift+D` | split the pane right / down |
| `Cmd+W` | close the pane; the last pane closes its tab, the last tab its window, the last window quits |
| `Cmd+Opt+arrows` | focus the nearest pane that way |
| `Cmd+Ctrl+arrows` | move the pane's nearest divider that way, 5% of its split a press (tutti's step) |
| `Ctrl+h/j/k/l`, `Alt+h/j/k/l` | with `keys = "tutti"` only: focus and resize, as in tutti |
| `Cmd+Shift+Enter` | zoom the pane to fill its tab, or back |
| `Cmd+C` | copy the selection |
| `Cmd+V` | paste (bracketed when the program asked for it) |
| `Cmd+F` | search the pane's scrollback and screen (below) |
| `Cmd+Up` / `Cmd+Down` | the view to the shell's previous / next prompt (OSC 133;A); past the last, back to the output |
| `Cmd` over a link, `Cmd+click` | underline the link under the pointer; open it (below) |
| `Cmd+=` / `Cmd+-` | font size up / down, in every pane of every window |
| mouse drag | select (double click: word, triple click: line), within the pane; Shift+drag when a program takes the mouse |
| mouse buttons | a click focuses the pane under it; reported to a program that asked (modes 9, 1000, 1002, 1003, in the default, UTF-8, SGR, urxvt or SGR-pixel encoding), at the pane's own cells |
| divider drag | resize the panes on either side |
| tab click | show that tab; its × (under the pointer) closes it and every pane in it; `+` opens a new tab, as `Cmd+T` does |
| double click on the bar | zoom the window, as on any titlebar (System Settings decides what a double click does) |
| wheel | scrollback on the primary screen; wheel reports to a program that asked for mouse reports; arrow keys to any other full-screen program (alternate scroll, on by default) |

New panes, tabs and windows run the login shell in the focused pane's working directory: as its
shell last reported it with OSC 7 (`ESC ] 7 ; file://host/path BEL`), else the directory of the
pane's foreground process (the pty's process group leader: the shell at a prompt, or the program
it runs), else your home directory. macOS's zsh and bash send OSC 7 only from
`/etc/zshrc_$TERM_PROGRAM`, which exists for Terminal.app alone, so the foreground process is what
usually answers. A pane closes when its program exits, with the same cascade as `Cmd+W`.

The focus and resize keys are rtty's unless the pane's program claims them (below, and
[Editors](#rtty-cli-and-editors)). `keys = "tutti"` makes `Ctrl+h/j/k/l` and `Alt+h/j/k/l` rtty's
too, as tutti's direct keys are, at the cost of those keys in programs that do not claim them; by
default every Ctrl key stays with the programs.
Focus by direction picks, among the panes beyond the focused one's edge that overlap it across,
the nearest; then the one focused last; then the one overlapping it most, as tmux and WezTerm do.

**The tab bar.** rtty draws its own titlebar, a bar 36 points tall across the window's top with
the traffic lights at its left, the way Ghostty looks on macOS. With one tab it shows the tab's
title, centered, after a folder while the pane's directory is known, dimmed while the window is in
the background. With more, it holds a rounded pill per tab, equal widths filling the row after
the traffic lights: the shown tab's pill is lighter, the others recede with dimmed titles; each
title is centered, each of the first nine pills shows its `Cmd+1`–`9` at its right end, and the
pill under the pointer shows a close × at its left end. A round `+` ends the row. The bar keeps
its height whatever the number of tabs, so opening or closing a tab resizes no pane; in full
screen, where macOS hides the titlebar, a lone tab has no bar and a row of tabs starts at the
window's edge. Its colors are the scheme's (below), so it follows the light/dark flip.

**Titles.** A pane is called by the title its program set (OSC 0 or 2); else, while a program
runs in its foreground (the pty's process group, `tcgetpgrp`), by that program's command line
(`cargo run --release`); else, with the shell at its prompt, by the shell's directory (OSC 7 when
the shell reports it, else the shell's own working directory), home written `~`. A pane looks at
its foreground process and directory once a second, and at once when its program sets a title or
its shell reports a directory. A tab is titled by its focused pane, the window by its shown tab,
and `rtty cli list` reports both. The focused pane draws the focused cursor and the others a
hollow one; focus reports (mode 1004) go to the pane gaining and the pane losing the keyboard.

**Search.** `Cmd+F` opens a bar at the pane's top right, which takes the keyboard until `Esc`. The
query searches every row the pane holds, scrollback and screen, as lines: a match runs on across a
soft wrap. A query without a capital ignores case (smart-case); `Ctrl+R` switches between text
and a regex. Matches show on the scheme's yellow and the current one on its cursor color; `Enter`
and `Shift+Enter` go to the next and previous match, round the ends, scrolling the view to it.
Matches stay on their text while output scrolls it up, and output arriving with the bar open is
searched again.

**Links.** A link is an OSC 8 hyperlink, or a plain `http`, `https` or `file` URL in the text
(trailing punctuation left out). Holding `Cmd` underlines the one under the pointer and `Cmd+click`
follows it: `http`, `https` and `mailto` open with the system's handler; a `file` link is shown in
Finder, selected, and never opened, since opening a file URL launches what it names (an app, a
`.command` script, an executable) and an OSC 8 link's text can claim anything; any other scheme is
ignored. Nothing happens without the click. While `Cmd` is held over a link whose text is not its
target (an OSC 8 "click here", or one showing one address and going to another), the pane's bottom
edge shows where it really goes and what a click will do.

**Bell and notifications.** BEL flashes its pane (`bell = "none"` turns that off) and, while the
window is not the key window, bounces rtty's dock icon once. OSC 9, OSC 777;notify and kitty's OSC
99 notifications become macOS notifications, except from the pane that has the keyboard in the key
window; clicking one brings rtty forward at its pane. macOS posts notifications only for an app
with a bundle identifier: unbundled (`cargo run`), `UNUserNotificationCenter` aborts the process
and the old `NSUserNotificationCenter` drops what it is given, so rtty posts nothing and says so
once on stderr. The least that works is a minimal, ad-hoc signed `.app`:

```sh
cargo build --release
mkdir -p target/rtty.app/Contents/MacOS
cp target/release/rtty target/rtty.app/Contents/MacOS/
cp bundle/Info.plist target/rtty.app/Contents/
codesign --force --sign - target/rtty.app
open target/rtty.app    # or run target/rtty.app/Contents/MacOS/rtty with arguments
```

The bundle's first notification asks for permission (System Settings › Notifications › rtty).

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

**What a command leaves on.** A program killed with SIGKILL turns off none of the reports and key
encodings it turned on, and the shell after it then gets a click, a focus change or a flip to dark
as typing, and its keys in kitty's encoding. When the shell marks the command's end (OSC 133;D,
the first of a run of them), rtty turns off the mouse reports (every mode and encoding), focus
reports (1004), light/dark reports (2031), modifyOtherKeys and kitty's flags, right there in the
stream, so what the shell sets for its next prompt stays. Bracketed paste and the cursor and keypad
modes stay too: shells set those themselves. tutti does the same.

## rtty cli and editors

`rtty cli` is the same binary in another role: it asks the rtty a pane runs in, through a Unix
socket (`RTTY_SOCKET`, in `$TMPDIR/rtty-<uid>/`, a directory rtty creates `0700` and refuses to use
when it is anyone else's or open to anyone else), and never opens a window. The commands, their
output and their exit codes follow `tutti pane …`, so an editor plugin speaks to either the same
way:

| Command | Does |
| --- | --- |
| `rtty cli pane focus --direction <left\|right\|up\|down> [--from <pane>] [--wrap]` | focuses the pane that way and prints its id; at an edge it says `no pane <direction> of <pane>` and exits 1; `--wrap` goes round to the farthest pane on the other side |
| `rtty cli pane resize --direction <…> [--amount <ratio> \| --cells <n>] [--from <pane>]` | moves the nearest split border that way, by 0.05 of the split unless told otherwise |
| `rtty cli pane focus <pane>` | shows the pane's tab and gives it the keyboard |
| `rtty cli pane split <pane> <right\|down\|left\|up>` | opens a pane there in its working directory and prints its id |
| `rtty cli pane zoom [<pane>]` | zooms the pane, or back |
| `rtty cli list` | every window, tab and pane as one line of JSON: ids, focus, zoom, titles, working directories, sizes, and which panes claim the keys |

`--from` is `$RTTY_PANE` by default, else the focused pane of the active window. An error says
`error: …` and exits 1; a malformed command prints the usage and exits 2.

**Keys a pane can claim.** A program that sets the OSC 1337 user var `RTTY_KEYS` to `pane`
(`ESC ] 1337 ; SetUserVar=RTTY_KEYS=cGFuZQ== BEL`, the value base64) gets rtty's focus and resize
chords instead of rtty acting on them: `Cmd+Opt+arrows`, `Cmd+Ctrl+arrows`, and with
`keys = "tutti"`, `Ctrl+h/j/k/l` and `Alt+h/j/k/l`. The claim ends when the var is set empty, when
a shell in the pane draws its next prompt (OSC 133;A), and with the program. The chords are encoded
as the program asked for keys, Cmd as the kitty protocol's super modifier: Neovim (kitty flags 3)
reads `Cmd+Opt+Left` as `<D-M-Left>` and `Cmd+Ctrl+Left` as `<D-C-Left>`. No xterm encoding says
Cmd in a way Emacs decodes (it reads `CSI 1 ; 11 D` as an undefined `M-[ 1 ; 1`), so a Cmd chord
reaches only a program speaking the kitty protocol; for any other program rtty keeps acting on it,
claim or not, and Emacs moves with the tutti-style keys instead.

**Editors.** [`editors/`](editors/README.md) has a Neovim plugin (with a smart-splits.nvim backend)
and an Emacs package. Each claims the keys while the editor runs, moves between the editor's own
windows, and past its last window calls `rtty cli pane focus --from $RTTY_PANE`, so one set of
keys moves through editor windows and rtty panes alike. Both do nothing outside rtty, and inside
tutti (`TUTTI_PANE`), where tutti's own plugins own navigation.

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
- `title.rs` — what a pane is called: its program's title, else the command line of the process
  in its foreground (`Pane::foreground`), else its shell's directory; unit tested.
- `render.rs` — what a frame draws, independent of GPUI and unit tested: per row, background spans
  and text runs with their resolved colors and decorations, and the cursor. A program's colors
  (OSC 4/10/11/12) win over the scheme's; DECSCNM swaps the defaults.
- `selection.rs` — selections by absolute `(row, col)`, by cells, words or lines, copied with
  `Terminal::text`.
- `mouse.rs` — mouse reports in every encoding, unit tested.
- `search.rs` and `links.rs` — matches of a query over a pane's rows, as cells by absolute row
  (soft wraps joined, wide characters whole), and the link on a cell (OSC 8, else a URL in its
  line) with what a click does with it (open, reveal in Finder, ignore) and what to show of a
  disguised one; both unit tested, and neither opens anything.
- `notify.rs` — the dock's attention and macOS notifications, which need an app bundle.
- `view.rs` — `PaneView`, the GPUI view of one pane. A foreground task drains the pane's channel
  in batches: `Output` marks the view dirty, so however many arrive between two display
  refreshes, GPUI draws one frame (that is the coalescing). Each frame locks the terminal only to
  read the rows that changed (the engine's damage, plus the cursor's rows and rows that left the
  screen) into `render` lines, then shapes those outside the lock; unchanged rows reuse last
  frame's shaped runs (see [Ligatures](#ligatures)). A synchronized update (mode 2026) keeps the
  last frame up until it ends, or for 150 ms. The view is also GPUI's text input handler, for
  committed text and IME marked text.
- `layout.rs` — a window's tabs, each a tree of splits with ratios, in cells, with no GPUI in it
  and unit tested: where each pane and divider goes in an area (a divider is one cell; no pane
  gets fewer than two across a split), split and close with the cascade to tab and window, a
  divider dragged to a cell or moved by a resize, focus by direction, zoom.
- `tabbar.rs` — the bar in the titlebar as rectangles: the pills, their close buttons and the `+`
  for a window's width and tabs, and what a point is over; no GPUI in it, unit tested.
- `window.rs` — `TerminalWindow`: one window's tabs, the bar (drawn where `tabbar` puts it, in a
  transparent titlebar, the traffic lights moved to its middle) and the dividers, and the Cmd
  chords as GPUI actions. It shares the window's cells out with `layout`, and every pane's grid
  and pty, shown or not, follow its cells on every resize. Each pane draws as a cached GPUI view,
  so output in one pane redraws that pane alone. `focus_toward` is the one call for moving focus
  by direction: the keys, `rtty cli` and so the editors all go through it. `answer` handles
  `rtty cli`'s requests on the main thread.
- `control.rs` and `cli.rs` — the socket (one JSON line each way, served on a thread of its own
  and answered on GPUI's main thread) and `rtty cli`'s commands, both unit tested.
- `scheme.rs` and `config.rs` — color schemes and the config file.
- `keys.rs` — the key encoder (GPUI keystroke → bytes: legacy xterm, modifyOtherKeys, the kitty
  protocol, and a claimed chord's Cmd as kitty's super) and paste wrapping, unit tested.
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

Splits (40×121 cells, timed by `/usr/bin/time -p` in the pane, three runs each): the five dense
copies drain in 0.95–1.07 s in a lone pane and 0.94–1.00 s in one pane of a 2×2 split while the
other three sit at a prompt. Idle, the window costs the same with eight panes as with one: 0.14 s
of CPU per 30 s (comparing 10 s and 40 s idle runs, startup excluded), about 5 ms a second. With
one pane of four redrawing a line, `--stats` prints one line a second: the idle panes draw
nothing.

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

Works: windows, tabs and splits (above), each pane running a shell or `-e` program with colors,
attributes, wide chars, ligatures, every underline style (single, double, curly, dotted, dashed;
SGR 4:0–4:5) in its own color (SGR 58, so Neovim's diagnostics undercurl in theirs), overline,
selection and copy/paste, scrollback, resize with reflow, mouse reports, a Ghostty-style tab bar
in the titlebar, window and tab titles from OSC 0/2, the foreground process or the shell's
directory, OSC 52 copy, synchronized updates, focus reports (mode 1004: `CSI I` and `CSI O` as
the window is activated and deactivated and as the pane gains and loses the keyboard), the
keyboard protocols above, scrollback search, prompt jumps, links, the bell, notifications (from
an app bundle), a config file, and WezTerm-format color schemes that follow the macOS
appearance live in every pane (with DEC mode 2031 reports). Checked with `--script` against
Neovim in two side-by-side panes (each redraws at its own size through divider moves and window
resizes; clicks reach the pane under them at its own cells; a light/dark flip reaches both) and
Emacs `-nw` in a stacked split; and with the editor plugins, Neovim and Emacs side by side: one
key walks through Neovim's windows on to Emacs and back.

The cursor takes the shape a program sets with DECSCUSR (block, bar or underline; Neovim's
insert mode is a bar), and a hollow block while the window or pane is not focused. It blinks when
DECSCUSR asks for blinking, or, while DECSCUSR is at its default, when ATT610 (`CSI ? 12 h`) does;
it stops blinking and stays on after 10 s without a key, so an idle window draws nothing.

Not yet: IME is wired through GPUI's input handler (composition is drawn at the cursor and the
candidate window is placed there) but unverified with a real input method; emoji and CJK rely on
CoreText's font fallback and an emoji may overhang its two cells; notifications need the `.app`
above, and posting from it and clicking one are not yet checked on screen; a selection drag stops
at its pane's edge rather than scrolling; macOS only.

## The GPUI pin

`gpui = "=0.2.2"` from crates.io, with the `runtime_shaders` feature. 0.2.2 is the newest
release on crates.io and builds on this Mac with the stable toolchain, which a git rev of Zed
would not guarantee (Zed tracks its own `rust-toolchain`, and a rev drags in its whole workspace).
`runtime_shaders` compiles GPUI's Metal shaders when the app starts instead of at build time:
Xcode 26 ships the offline `metal` compiler as a separate download
(`xcodebuild -downloadComponent MetalToolchain`), so without the feature the build fails on a
machine that has not fetched it. The cost is a shader compile at startup.

Licensing: GPUI and rtty-vt are Apache-2.0, portable-pty is MIT. rtty takes nothing
from Zed's GPL `terminal` / `terminal_view` crates.

## Roadmap

1. **tutti knows rtty.** tutti detects rtty (XTVERSION) and uses what it offers: the kitty
   keyboard protocol, live light/dark (mode 2031), notifications, OSC 52 and hyperlinks, and
   undercurl and cursor shapes passed through to its panes; and a move past tutti's edge, from
   tutti or its editor plugins, goes on to rtty's panes.
