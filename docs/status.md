# Status and roadmap

What works, how each part was checked against real programs, what has been checked only through
`--script`'s dump and not yet by eye on screen, and what is left to do. `--script` and its dump
are in [scripting.md](scripting.md#--script).

## What works

- Windows, tabs and splits ([windows.md](windows.md)), each pane running a shell or `-e` program.
- Colors, attributes, wide chars and ligatures ([fonts.md](fonts.md#ligatures)).
- Every underline style (single, double, curly, dotted, dashed; SGR 4:0–4:5) in its own color (SGR
  58, so Neovim's diagnostics undercurl in theirs), and overline.
- Selection (scrolling the scrollback when dragged past the pane) and copy/paste.
- Scrollback, and resize with reflow.
- Mouse reports.
- A Ghostty-style tab bar in the titlebar.
- Window and tab titles from OSC 0/2, the foreground process or the shell's directory.
- OSC 52 copy and, with permission, read.
- Synchronized updates.
- Focus reports (mode 1004: `CSI I` and `CSI O` as the window is activated and deactivated and as
  the pane gains and loses the keyboard).
- The [keyboard protocols](keys.md#keyboard-protocols).
- Scrollback search, and prompt jumps.
- Links (URLs, and `path:line:col` opened in an editor pane or a neighbouring Neovim), quick select
  and quick open.
- The bell, and notifications (from an app bundle).
- A program's progress on its tab's pill and the dock's badge.
- A question before closing what runs a program, and `Cmd+Z` to undo a close.
- `Cmd+K` to clear.
- A config file with key bindings, and a menu bar showing them.
- WezTerm-format color schemes that follow the macOS appearance live in every pane (with DEC mode
  2031 reports).
- `rtty cli` and the editor plugins.
- `tabs = false` for a window manager with tabs of its own ([Without tabs](windows.md#without-tabs)),
  and what one reads and asks through `rtty cli`: windows' ids, panes' progress and user vars, a
  new window.
- The log.
- Shell integration that loads itself (prompt jumps, the last command's output, notifications of
  long commands, click to move the cursor).
- Images (kitty's graphics protocol, Unicode placeholders included).
- [Cell borders](borders.md): lines on cells' edges that take no cells, by the cell-border
  protocol (all of version 1 but the line styles).
- [tutti panes](tutti.md#tutti-panes): one pane of a tutti session, or a whole session in a
  window.

## How it was checked

All of these were checked with `--script`.

### Editors

- Neovim in two side-by-side panes: each redraws at its own size through divider moves and window
  resizes; clicks reach the pane under them at its own cells; a light/dark flip reaches both.
- Emacs `-nw` in a stacked split.
- The editor plugins, Neovim and Emacs side by side: one key walks through Neovim's windows on to
  Emacs and back.

### Shell integration

Checked against zsh 5.9, macOS's bash 3.2 and Homebrew's bash 5.3, each with a scratch home and no
integration of its own, and against zsh with a prompt that marks itself too:

- the marks arrive;
- prompt jumps go from prompt to prompt;
- `Cmd+Shift+A` copies the last output out of the scrollback;
- a long command ending in a pane without the keyboard is notified;
- a click moves the cursor across wide characters and a soft wrap.

fish is not installed here, so its script is unchecked.

### Paths, quick select and quick open

Checked in a scratch home:

- a printed `src/main.rs:3:5` is underlined under `Cmd` and a click runs the `editor` template with
  its line and column, or with no template the `$EDITOR` a `.zshrc` exports;
- a relative path is found from a hand-printed OSC 7 while the shell sits elsewhere;
- a Neovim with the plugin in the pane beside opens the file at its line and column and takes the
  keyboard;
- quick select labels, copies, pastes with Shift (through AppKit's key path too) and cancels on Esc
  or any other key;
- quick open follows a URL and a path.

### Block selection and copy mode

Checked in a scratch home:

- an Option+drag selects a block that leaves out a wide character its left edge cuts;
- where `cat` takes the mouse an Option+drag reaches it as an Alt drag and a Shift+Option+drag
  selects;
- copy mode moves by cells, words, the view's rows, half pages, `gg` into the scrollback and `[` to
  a prompt, selects a block and lines (through AppKit's key path too, with nothing reaching the
  shell), searches with `/`, and copies what it selected with `y`.

Its cursor is checked in the dump's drawn colors; its label, which the dump does not show, not yet
by eye.

### Images

Checked with a program sending RGB, RGBA, PNG, chunked, compressed and temporary-file images,
placements under and over text, a delete and placeholders, through a reflow and `clear`, by the
dump's `image` lines, not yet by eye.

### Cell borders

Checked with a shell printing the commands, by the dump's `border` lines: a frame's top in a
palette color under the text and its left side in the default foreground over it, a rounded corner
with its arc and its outside filled, the colors following a flip to light, a written cell losing
its borders and its neighbours keeping theirs, and the query answered ahead of DA1. The lines'
pixels (inside and centred strokes, the merge on a shared edge, corners, centre arms, arcs and
forks) are unit tested. Seen on screen under tutti's TUI: its pane frames, inside strokes on whole
edges with rounded corners. The rest has not been seen yet, and the protocol author's gallery has
not been run in rtty.

### Without tabs

Checked with `tabs = false` in a scratch config: no bar and the close button hidden as AppKit has
it, `Cmd+T` opening a window, a saved window of two tabs coming back as two windows with one
keeping its id, and the bar and buttons back when `tabs` is turned on again; and `rtty cli list`
and `window new` read as JSON from inside a pane and with no `RTTY_SOCKET`. Not checked: any of
it under a window manager, the look of a window with no bar, and a tutti session's window
without tabs against a real daemon (its one-tab cut is unit tested).

### tutti panes

[tutti panes](tutti.md#tutti-panes) are checked against a real tutti daemon in a scratch session.

One pane in rtty's layout:

- the 3000 lines `seq` printed before the attach scroll by the wheel in rtty's own scrollback and
  `Cmd+F` finds them;
- what is typed reaches the pane, and a resize its `stty size` (both read back with `tutti pane
  read`);
- a DA1 asked in raw mode gets one reply and a flip to light one `997;2n`;
- `rtty cli tutti attach` opens a pane in a split and in a tab;
- `Cmd+W` leaves the pane running in `tutti pane list`;
- `tutti pane kill` and `kill-pane` close the rtty pane;
- `tutti pane rename` and a program's `TUTTI_KEYS` reach `rtty cli list`.

A window that is a session:

- it opens on its two tabs and their splits;
- a split right and down, a new tab, a resize and `kill-pane` made in rtty show in `tutti pane
  list`, `tutti tab list` and `tutti watch`, and a split made with `tutti pane split` shows in
  rtty;
- tutti's TUI, attached in another rtty's pane, keeps its focused pane while rtty's moves (`tutti
  watch`);
- a program's OSC 7501 working then done makes one notification;
- a focus from `tutti pane focus` shows that pane;
- tutti's focus move and resize past its tab's edge move rtty's focus and divider;
- a daemon that accepts and never answers (`nc -lU`) leaves the window typing while it waits, and
  its window says why after 3 s;
- a restored window reattaches its tutti panes, a killed one saying so, and a restored session
  window is the session again.

The connection and its streams:

- a window holds one connection to the daemon however many panes it shows (`lsof -U` on both
  ends);
- a pane no window shows prints 20 000 lines and none of its frames reaches rtty;
- a pane closed while another of its session shows is detached alone, the connection staying, and
  a second viewer then gets the pane at its own size;
- shown again, it comes back with its history on a new stream, and a second ask for it is refused;
- a pane's exit leaves it showing and a kill after takes it out.

Keys, speed and focus:

- Neovim in a tutti pane pushes kitty's flags, rtty's terminal for the pane takes them, rtty writes
  Shift+Enter as `CSI 13;2u` and Neovim reads `<S-CR>`;
- after a program left the flags on, the daemon's reset follows a command's end in the `--trace`;
- a pane printing 30 MB beside a pane being typed in delays no echo there (under 1 ms, from the
  trace's times);
- a view's focus with `seen` turns a done pane idle and moves no tab's active pane (asked of the
  daemon directly), and a scripted window, never the active one, leaves it done.

tutti's TUI in an rtty pane, which claims rtty's keys and hands moves on to `rtty cli`, is in
[tutti.md](tutti.md#tuttis-tui-in-an-rtty-pane).

## Not yet

- IME is wired through GPUI's input handler (composition is drawn at the cursor and the candidate
  window is placed there) but unverified with a real input method.
- Emoji and CJK rely on CoreText's font fallback (an emoji wider than its two cells is drawn
  smaller to fit them).
- The drawn box, block and powerline characters and a selection dragged past a pane are checked by
  the `--script` dump, not yet by eye on screen.
- Notifications need the `.app` ([Bell and notifications](windows.md#bell-and-notifications)), and
  posting from it and clicking one are not yet checked on screen.
- Closing's question is checked through the `--script` dump, where a scripted window keeps it, and
  its `answer` step, and the window's close button through its `window close` step, which runs
  what a click runs; the native sheet itself, its buttons and Esc, the click on the close button as
  AppKit sends it, the dock's badge and the progress line's look are not yet checked by eye.
- Quitting as macOS asks is checked with `--script`'s `terminate` step, which sends AppKit's
  `terminate:` from the run loop, in process: the question, `Cancel` keeping rtty, `Quit` quitting
  it, nothing asked over an idle shell, and the asking window closing first, after which the
  question comes again on the window left and its `Quit` still ends AppKit's wait; the dock's Quit
  and a logout themselves are not yet checked.
- The menu bar's chords are checked in the `--script` dump, which lists the menus as AppKit has
  them, and its items by `--script`'s `menu` step, which dispatches an item's action to the
  scripted window (a click dispatches it to the key window, which a scripted window never is); the
  menus' look and a click on an item are not yet checked by eye.
- Secure Keyboard Entry is checked through the dump, with `--script`'s `app` step standing in for
  macOS making rtty active and not: the enable and disable calls rtty made, one each way, macOS's
  own word (`IsSecureEventInputEnabled`), a password prompt holding it, and none held after
  quitting; the lock and the menu's check mark are not yet checked by eye.
- The global hotkey is registered with macOS in a scripted run too, and `--script`'s `hotkey` step
  runs what a press runs, which a scripted rtty keeps in its dump instead of doing; a real press is
  not yet checked.
- A clipboard read is checked with `--script` and `--trace`: no answer goes to the pty until
  `answer ok`, none after `answer cancel`, and with `clipboard_read = "allow"` one at once; the
  sheet and its keys (Return and Esc denying) are not yet checked by eye.

## Roadmap

The one item this held, tutti knowing rtty, has shipped
([tutti's TUI in an rtty pane](tutti.md#tuttis-tui-in-an-rtty-pane)), and so has rtty showing tutti
panes natively, a pane or a whole session in a window. What is left:

1. **A look on screen** at what scripts check only in the dump:
   - IME with a real input method;
   - a dead key on the Option that is not Alt (é from Option+E, E: the text input inserts the
     characters a synthesized key event carries, and composes nothing from them);
   - a file dragged from Finder (`drop` calls the pane's drop handler, past GPUI's drag);
   - the drawn box, block and powerline characters;
   - a selection dragged past a pane;
   - notifications from the `.app`, posted and clicked;
   - closing's sheet and the window's close button;
   - the progress line and the dock's badge;
   - quick select's labels over their matches;
   - the menu bar and a click on its items;
   - `Cmd+A` and `Cmd+O`, which no binding takes: under `--script`'s `nskey` the menu bar ran an
     item on an unshifted chord whose own chord has Shift, so in a key window they may run Copy
     Last Output (`Shift+Cmd+A`) and Quick Open (`Shift+Cmd+O`);
   - the dock's Quit and a logout;
   - Secure Keyboard Entry's lock and check mark;
   - a real press of the global hotkey;
   - a clipboard read's sheet;
   - kitty images and their placeholders;
   - cell borders past what tutti's TUI draws (centred strokes, half edges, centre arms, forks),
     with the protocol author's gallery (`scripts/cell-borders.py` on his Ghostty branch);
   - the config reload's banner;
   - a restored window landing on its own display.
2. **tutti sessions on other machines** ([tutti panes](tutti.md#tutti-panes)), through `tutti
   proxy` over ssh: one channel a window, as everything goes over one connection.

Cmd+D and Cmd+F, which once quit rtty and did nothing in a launch where its window was key,
split and search in the installed app since a reinstall (seen on screen; the cause was never
pinned down). rtty.log still records each Cmd chord as AppKit hands it over and as GPUI
dispatches it, should it come back.
