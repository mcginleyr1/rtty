# The UI suite

`tests/ui.rs` checks rtty's GUI layer through real windows. Each test hands `--script` a script
(the steps are listed at the top of `src/script.rs`) and asserts on what its `dump` steps wrote:
the window's tabs, bar and layout, and what the next frame draws in each shown pane.

```sh
just ui    # cargo test --test ui -- --ignored --test-threads=1
```

It needs a Mac with its display awake and takes about 35 seconds. Every test is `#[ignore]`d, so
a plain `cargo test` stays headless. The windows open on your screen but never take the keyboard
(no test passes `--activate`). While the display sleeps GPUI draws no frame and a script stalls:
a run is killed after 30 seconds and says so (`pmset -g assertions` shows `UserIsActive 1`).

## What a run can touch

`launch` starts the built rtty with a cleared environment in a directory of the test's own,
`/private/tmp/rtty-ui-<pid>-<test>` (short, since rtty's socket goes under it), removed afterwards:

- `HOME`, `ZDOTDIR`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `TMPDIR` are all that directory, so
  rtty's files are its own: the config is `rtty/config.toml` there, the saved layout
  `rtty/layout.json`, the socket and `rtty.log` under `rtty-<uid>/`, and the shell's history.
- `SHELL` is `/bin/zsh`, with a `.zshrc` that sets only `PS1='ready> '`.
- Under `--script` rtty records a followed URL instead of opening it. Whatever is copied (`Cmd+C`,
  copy mode, quick select, `copy-last-output`, a program's OSC 52) goes to the dump's `copied`
  line and never to your clipboard, and a paste or a program's clipboard read gets a stand-in.

## What it covers

Tabs (chords, the bar's pills, `+` and close, undoing a close), the question before closing or
quitting what runs a program, splits (layout, focus, resize, zoom, swap, the menu, a click), keys
through GPUI and through AppKit (`nskey`), Option as Alt or not, mouse selection (cells, words,
lines, blocks) and `Cmd+C`, copy mode, the wheel, prompt jumps, `Cmd+K`, search, window resize,
font size, the appearance flip, focus in and out, a second window and where it opens, the config
with `[bind]` and its live reload, shell integration (marks, exit status, `copy-last-output`,
click to move the cursor), `--restore`, links, quick select, a program's title and progress, mouse
reports, dropped files, a program's clipboard write and read, and a paste.

Not covered: tutti panes (they need a tutti daemon), what needs a key window (focus reports, the
focused cursor, notifications, Secure Keyboard Entry), `Cmd+click` on a path and `open-config`
(they start an editor), the global hotkey and the bell, images, and fonts other than Menlo.

`nskey` cannot press a window's menu chords: a scripted window is never the key window, so
AppKit hands a Cmd chord to the menu bar, whose item acts on the app's main window, and a scripted
rtty has none. `nskey cmd-d`, `cmd-w`, `cmd-t` and `cmd-f` do nothing (`keys` presses them); an
app-wide item (`cmd-=`), a key no item uses (`cmd-1`) and Shift with a letter (`cmd-shift-d`) work.

## Adding a test

- `run(name, config, script)` runs an inline script: `name` names the scratch directory, `config`
  is the config.toml. A line that is only `dump` gets a file of its own, and the dumps come back
  in order with the exit status. `launch` runs one in a directory you keep (the restore test).
- Wait for text only the output has, never the command line as typed: `echo do''ne`, then `wait
  done`. A program runs once it answers: `cat -n`, `go`, then `wait 1  go`. Sleep only where
  nothing shows: 300 ms after `resize` or a click, 2500 ms for a config change.
- Start with `resize 24 80` when cells matter, and name no color unless the script sets the
  appearance or the config pins a scheme: the window follows the Mac's.
- Take each expected value from a run (a failing test prints its dumps), checked with the README.
