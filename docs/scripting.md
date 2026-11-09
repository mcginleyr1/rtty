# Scripting, tracing and the log

The flags that check and measure rtty with nobody at the keyboard (`--script` with its `dump`,
`--trace`, `--stats`, `--smoke` and `--bench`), and `rtty.log`, which says what a running rtty
did. How these are used to test rtty is in [testing.md](testing.md).

## `--script`

`--script FILE` drives the window from a script instead of a person. It is how rtty is checked
against Neovim, Emacs and the rest without a screenshot. A script can do the following; the step
list, with each step's arguments, is at the top of `src/script.rs`.

| A script can | Steps |
| --- | --- |
| wait for text, or for a time | `wait`, `sleep` |
| press keys and type text through GPUI's keystroke dispatch, so Cmd chords split and switch panes | `keys`, `type` |
| press keys as AppKit key events handed to NSApplication in process, which take macOS's own path: event monitors, key equivalent, text input context, key down. `alt-h->˙` gives the character Option types | `nskey` |
| click, move the pointer, drag, turn the wheel and drop files at window cells; each goes to the pane under it | `click`, `move`, `drag`, `wheel`, `drop` |
| click and hover on the tab bar's pills, close buttons and `+` | `bar click`, `bar hover` |
| answer a question that closing, quitting or a clipboard read asked | `answer` |
| send AppKit's `terminate:` as the dock's Quit sends it | `terminate` |
| press another window's close button | `window close` |
| run a menu bar item's action | `menu` |
| make rtty the active app (with any of its windows the key one) and not | `app` |
| press the global hotkey | `hotkey` |
| move the keyboard focus in and out | `focus` |
| flip the appearance between dark and light | `appearance` |
| resize the window | `resize` |
| write what the window shows to a file (below) | `dump` |
| quit rtty | `quit` |

A scripted window differs from a normal one in these ways:

- It does not take the keyboard from whatever app has it.
- It keeps a question in its dump instead of showing a sheet.
- A clipboard read a script answers gets a stand-in, never the clipboard.
- `copy-last-output` never copies to the clipboard; the dump shows what it copied.
- Short of `--restore`, it neither restores nor saves the layout
  ([Restoring windows](windows.md#restoring-windows)).

### `dump`

`dump` writes:

- the window's tabs, bar and layout;
- what the next frame draws in each shown pane: runs, their glyphs, colors, underlines, the cursor,
  the font with its features, the cell, and the rows each drawn character fills;
- the modes the frame is drawn from;
- the shell's marks, a running command, and what `copy-last-output` copied.

### `--activate` and `--restore`

Both go with `--script`.

- `--activate` takes the keyboard as a normal launch does, for the checks that need a focused
  window (focus reports, the focused cursor).
- `--restore` restores the saved layout and saves it, as a normal launch does
  ([Restoring windows](windows.md#restoring-windows)); the script drives the first window.

## `--trace`

`--trace FILE` writes every read from a pty (`<`) and write to it (`>`), escaped, one line each
with the milliseconds since start and the pane's number: what a program asks and what it was told.
For a [tutti pane](tutti.md#tutti-panes) it writes each snapshot and read its stream brings, and
what goes to the pane.

## `--stats`

Once a second while a pane draws frames, `--stats` prints its frames, layout and paint ms/frame to
stderr (a line per pane that drew). GPUI stops drawing while the window is occluded, so `--stats`
prints nothing for a hidden window. Numbers measured with it are in
[measurements.md](measurements.md#drawing).

## `--smoke`

`--smoke` runs `/bin/sh -c 'printf hello-rtty; exit 7'` through the same pty and terminal with no
window. It passes, and exits 0, when the grid shows `hello-rtty` and the exit code is 7; otherwise
it exits 1.

## `--bench`

`--bench [COMMAND]` times COMMAND (default `seq -f %.0f 1 2000000`) three ways with no window: into
a pipe, the parser alone on its output, and the whole pty pipeline. Results are in
[measurements.md](measurements.md#rtty---bench).

## The log

rtty appends to `$TMPDIR/rtty-<uid>/rtty.log`, next to its socket (`echo $TMPDIR` shows where
`$TMPDIR` is; it is per user on macOS). It records:

- when each rtty started and quit, and why;
- each pane's start, split, close (with the chord that closed it) and its program's exit status;
- a tab's close button;
- the question a close asked and its answer (never what runs);
- a close undone or made for good;
- the dock's progress badge as it changes;
- what came of each config reload;
- the layout restored at launch (or why none was);
- each Cmd chord as AppKit hands it to rtty and as GPUI dispatched it (the window's activation,
  the key contexts it met, the actions bound to it there and the one that ran);
- every panic, on any thread, with its backtrace.

It never records what a pane shows or is sent. Past a megabyte it starts over, keeping the last as
`rtty.log.old`. When rtty vanishes, its last lines say whether it quit (and why) or panicked (and
where).
