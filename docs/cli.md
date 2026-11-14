# Command line

rtty's flags, the environment variables it sets for the programs in its panes, and `rtty cli`,
which asks a running rtty about its windows, tabs and panes and tells it to change them.

## Flags

| Flag | Meaning |
| --- | --- |
| `--config FILE` | read this config file instead of `~/.config/rtty/config.toml` (it must exist) |
| `--font NAME` | font family (default Menlo); an unknown family is an error |
| `--size PT` | font size in points (default 14) |
| `--ligatures on\|off` | the font's programming ligatures: on enables `calt` and `liga`, off disables `calt`, `liga` and `dlig`, over the config's `ligatures` and `font_features`; default on |
| `-e PROGRAM [ARGS...]` | run the program in the first pane instead of the login shell; its pane closes when it exits (new panes run the shell) |
| `--tutti SESSION[:PANE]` | make the first window tutti session SESSION, its tabs and splits the session's; with `:PANE`, show only that pane of it in the first pane instead of the login shell ([tutti panes](tutti.md#tutti-panes)); no saved layout is restored. Not with `-e` |
| `--stats` | print frame timings to stderr while a pane draws ([`--stats`](scripting.md#--stats)) |
| `--trace FILE` | write every read from a pty and every write to it to FILE ([`--trace`](scripting.md#--trace)) |
| `--script FILE` | drive the window from a script instead of a person ([`--script`](scripting.md#--script)) |
| `--activate` | with `--script`: take the keyboard as a normal launch does ([`--activate`](scripting.md#--activate-and---restore)) |
| `--restore` | with `--script`: restore the saved layout and save it, as a normal launch does ([`--restore`](scripting.md#--activate-and---restore)) |
| `--smoke` | check the pty and the terminal with no window; exits 0 or 1 ([`--smoke`](scripting.md#--smoke)) |
| `--bench [COMMAND]` | time COMMAND through the pty and the terminal with no window ([`--bench`](scripting.md#--bench)) |

`--font`, `--size` and `--ligatures` override the config file
([Configuration](configuration.md)).

## Environment

What rtty sets, and removes, in the environment of a pane's program:

| Variable | Value |
| --- | --- |
| `TERM` | `xterm-256color` |
| `COLORTERM` | `truecolor` |
| `TERM_PROGRAM` | `rtty` |
| `TERM_PROGRAM_VERSION` | rtty's version |
| `RTTY_PANE` | which pane the program is in |
| `RTTY_SOCKET` | where [`rtty cli`](#rtty-cli) reaches this rtty |
| `TMUX`, `TMUX_PANE`, `STY`, `ZELLIJ`, `TUTTI_PANE`, `TUTTI_SESSION` | removed |

The last row is the variables that would tell a program it runs inside a multiplexer rtty was
started from: Neovim, for one, skips its undercurl probe inside tmux.

## rtty cli

`rtty cli` is the same binary in another role: it asks the rtty a pane runs in, through a Unix
socket (`RTTY_SOCKET`, in `$TMPDIR/rtty-<uid>/`, a directory rtty creates `0700` and refuses to use
when it is anyone else's or open to anyone else). The commands, their output and their exit codes
follow `tutti pane …`, so an editor plugin speaks to either the same way:

| Command | Does |
| --- | --- |
| `rtty cli pane focus --direction <left\|right\|up\|down> [--from <pane>] [--wrap]` | focuses the pane that way and prints its id; at an edge it says `no pane <direction> of <pane>` and exits 1, unless ionmac takes the move on to the frame beside the window ([Past the window's edge](windows.md#past-the-windows-edge)), which prints nothing and exits 0; `--wrap` goes round to the farthest pane on the other side |
| `rtty cli pane resize --direction <…> [--amount <ratio> \| --cells <n>] [--from <pane>]` | moves the nearest split border that way, by 0.05 of the split unless told otherwise |
| `rtty cli pane focus <pane>` | shows the pane's tab and gives it the keyboard |
| `rtty cli pane split <pane> <right\|down\|left\|up>` | opens a pane there in its working directory and prints its id |
| `rtty cli pane zoom [<pane>]` | zooms the pane, or back |
| `rtty cli list` | every window, tab and pane as one line of JSON: ids, focus, zoom, titles, working directories, sizes, which panes claim the keys, and the tutti session and pane a tutti pane shows (`"tutti":["work",3]`). For a window manager, each window also has `uid`, an id of rtty's own that the saved layout keeps, so the window has it again after a restart, and `window_number`, macOS's number for it; each pane has `progress` when its program reports one (`{"state":"normal","percent":40}`; the states are `normal`, `error`, `indeterminate` and `paused`) and `vars`, the user vars its program set and has not emptied (tutti's TUI names its session there: `{"TUTTI_TUI":"work"}`) |
| `rtty cli window new [--cwd <dir> \| --tutti <pane> [-s <session>]] [--frame <x>,<y>,<width>,<height> [--display <uuid>]]` | opens a window with a login shell in `<dir>`, else the focused pane's directory, and prints it as `list` does. With `--tutti` the window shows that pane of a tutti session in place of a shell, the session found as `tutti attach` finds it; the window is printed at once, with no pane in it until the session answers, and the pane says why when it cannot. `--frame` is in points from the top left of the display named by `--display` (its UUID, as the saved layout has it), else of the main one; a frame on a display not here is centered on the main one |
| `rtty cli tutti attach <pane> [-s <session>] [--split <right\|down\|left\|up> \| --tab] [--from <pane>]` | shows pane `<pane>` of a tutti session as an rtty pane ([tutti panes](tutti.md#tutti-panes)), in a new tab unless `--split` says which way from `--from`, and prints the new rtty pane's id at once: the pane shows once the session answers, or says why it cannot. The session is `-s`, else `$TUTTI_SESSION`, else `tutti`, as tutti's own commands take it, and its socket is found in the asking shell's environment (`$XDG_RUNTIME_DIR`), as tutti finds it. Not in a window that is a tutti session |
| `rtty cli tutti open [-s <session>]` | opens a window that is the whole tutti session ([tutti panes](tutti.md#tutti-panes)); the session as `tutti attach` finds it. It returns at once; the window fills in when the session answers |

`--from` is `$RTTY_PANE` by default, else the focused pane of the active window. An error says
`error: …` and exits 1; a malformed command prints the usage and exits 2.

Outside a pane there is no `RTTY_SOCKET`: `rtty cli --socket <path> …` names the rtty to ask
(each listens on `<pid>.sock` in that directory), and without it `rtty cli` asks the one rtty
running, or says so when there are several or none.

The editor plugins call these commands past the editor's last window; what a program in a pane
does to get rtty's pane keys is in [Keys a pane can claim](keys.md#keys-a-pane-can-claim), and the
plugins themselves in [`editors/`](../editors/README.md).
