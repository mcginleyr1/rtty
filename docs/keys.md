# Keys

The default keys and the action each runs, what the mouse does, the menu bar, the keys that move
between panes and the ones a program can claim, Secure Keyboard Entry, the global hotkey, and how
keys are encoded for the program in a pane.

## Default keys

Ctrl keys belong to the terminal; Cmd chords belong to the app. These are the defaults; the
action names are `[bind]`'s ([Key bindings](configuration.md#key-bindings)). An action with no key
has no default chord and runs only once `[bind]` gives it one, or from the menu bar where it has
an item.

### The app and its windows

| Key | Action | Does |
| --- | --- | --- |
| `Cmd+Q` | `quit` | quit; asks first while a pane in any window runs a program ([Closing what runs](windows.md#closing-what-runs)) |
| `Cmd+N` | `new-window` | new window |
| `Cmd+Shift+W` | `close-window` | close the window, asking first as its close button does |
| `Cmd+M` | `minimize` | minimize the window into the dock |
| none | `zoom-window` | zoom the window, as a double click on its bar does |
| `Cmd+H` / `Cmd+Opt+H` | `hide` / `hide-others` | hide rtty / hide every other app |
| none | `show-all` / `about` | show every app again / rtty's About panel |
| none | `secure-input` | turn Secure Keyboard Entry on or off ([Secure Keyboard Entry](#secure-keyboard-entry)) |

### Tabs

| Key | Action | Does |
| --- | --- | --- |
| `Cmd+T` | `new-tab` | new tab, after the current one |
| `Cmd+1`–`9` | `tab-1`–`tab-9` | show that tab |
| `Cmd+Shift+[` / `Cmd+Shift+]` | `prev-tab` / `next-tab` | previous / next tab, round the ends |

### Panes

| Key | Action | Does |
| --- | --- | --- |
| `Cmd+D` / `Cmd+Shift+D` | `split-right` / `split-down` | split the pane right / down |
| `Cmd+W` | `close-pane` | close the pane, asking first while it runs a program ([Closing what runs](windows.md#closing-what-runs)); the last pane closes its tab, the last tab its window, the last window quits. A [tutti pane](tutti.md#tutti-panes) is only detached: it runs on in tutti. In a window that is a tutti session it closes nothing and says so: closing the window detaches |
| none | `kill-pane` | end the focused pane's program now, without asking: hang up on it, or have tutti's daemon kill a tutti pane; its exit closes the pane (in a window that is a tutti session, tutti takes it out of the layout) |
| `Cmd+Z` | `undo-close` | bring back the pane, tab or window closed last, within `undo_timeout` seconds ([Undoing a close](windows.md#undoing-a-close)) |
| `Cmd+Opt+arrows` | `focus-left`, `-right`, `-up`, `-down` | focus the nearest pane that way |
| `Cmd+Ctrl+arrows` | `resize-left`, `-right`, `-up`, `-down` | move the pane's nearest divider that way, 5% of its split a press (tutti's step) |
| `Ctrl+h/j/k/l`, `Alt+h/j/k/l` | `focus-…`, `resize-…` | with `keys = "tutti"` only: focus and resize, as in tutti |
| none | `last-pane` | focus the pane the focus came from in this tab, and back again, as Vim's `wincmd p` does; nothing once that pane has closed |
| none | `swap-left`, `-right`, `-up`, `-down` | trade places with the pane focus would go to that way; the splits stay, and the focus goes with the pane |
| `Cmd+Shift+Enter` | `zoom` | zoom the pane to fill its tab, or back |

### Copying, searching and the scrollback

| Key | Action | Does |
| --- | --- | --- |
| `Cmd+C` | `copy` | copy the selection |
| `Cmd+Shift+A` | `copy-last-output` | copy the last command's output, scrollback included ([Shell integration](shell-integration.md#what-the-marks-give)), as iTerm2's chord does |
| `Cmd+V` | `paste` | paste (bracketed when the program asked for it) |
| `Cmd+F` | `find` | search the pane's scrollback and screen ([Search](selection.md#search)) |
| `Cmd+Shift+Space` | `quick-select` | label what is worth copying on the pane's screen; typing a label copies it, in uppercase pastes it too ([Quick select](selection.md#quick-select-and-quick-open)) |
| `Cmd+Shift+O` | `quick-open` | label the links on the pane's screen; typing a label opens it as `Cmd+click` would ([Quick open](selection.md#quick-select-and-quick-open)) |
| `Cmd+Shift+C` | `copy-mode` | copy mode, or out of it: move a cursor through the screen and the scrollback with vi's keys, select characters, lines or a block, copy ([Copy mode](selection.md#copy-mode)) |
| `Cmd+Up` / `Cmd+Down` | `prompt-prev` / `prompt-next` | the view to the shell's previous / next prompt (OSC 133;A); past the last, back to the output |
| `Cmd+K` | `clear-scrollback` | clear the pane's scrollback and screen, then send the program Ctrl+L so a shell draws its prompt again at the top, as Terminal.app does; under a full-screen program (the alternate screen: Neovim, less) only the scrollback goes, and the program is sent nothing |

### Font size and the config file

| Key | Action | Does |
| --- | --- | --- |
| `Cmd+=` / `Cmd+-` | `font-bigger` / `font-smaller` | font size up / down, in every pane of every window |
| `Cmd+0` | `font-reset` | font size back to the configured one (`size`, or `--size`) |
| `Cmd+Shift+,` | `reload-config` | read the config file again now ([Reloading](configuration.md#reloading)); rtty also does when it changes |
| `Cmd+,` | `open-config` | the config file in your editor, in a new tab; written first from rtty's commented template when there is none ([Opening the file](configuration.md#opening-the-file)) |

### The mouse

| With the mouse | Does |
| --- | --- |
| `Cmd` over a link, `Cmd+click` | underline the link (a URL, or a path such as `src/main.rs:12:5`) under the pointer; open it ([Links](selection.md#links)) |
| mouse drag | select (double click: word, triple click: line; Option+drag: a block, [Block selection](selection.md#block-selection)), within the pane; held past its top or bottom, the scrollback scrolls that way, faster the further out; Shift+drag (Shift+Option+drag for a block) when a program takes the mouse |
| mouse buttons | a click focuses the pane under it; reported to a program that asked (modes 9, 1000, 1002, 1003, in the default, UTF-8, SGR, urxvt or SGR-pixel encoding), at the pane's own cells; in the pane with the keyboard, a click on the command line being typed moves the shell's cursor there ([Shell integration](shell-integration.md#what-the-marks-give)) |
| divider drag | resize the panes on either side |
| file drop | files dragged from Finder onto a pane type their paths there (below) |
| tab click | show that tab; its × (under the pointer) closes it and every pane in it; `+` opens a new tab, as `Cmd+T` does |
| double click on the bar | zoom the window, as on any titlebar (System Settings decides what a double click does) |
| wheel | scrollback on the primary screen; wheel reports to a program that asked for mouse reports; arrow keys to any other full-screen program (alternate scroll, on by default) |

**Dropped files.** Files dragged from Finder onto a pane type their paths there, separated by
spaces, with a backslash before each character a shell reads specially (`/tmp/Screen\ Shot.png`,
`it\'s`), and non-ASCII as it is. They arrive as a paste, bracketed when the program asked for
that, so an agent such as Claude Code gets a dropped screenshot's path as pasted text. The pane
takes the keyboard. A path holding a control character, which would arrive as a key (Enter,
Ctrl+U), types nothing.

## The menu bar

rtty's menus hold the actions above:

| Menu | Items |
| --- | --- |
| rtty (the App menu) | About, Settings… (`open-config`), Reload Configuration, Secure Keyboard Entry, Hide, Hide Others, Show All and Quit |
| Shell | New Window, New Tab, the splits, Close Pane, Close Window and Undo Close |
| Edit | Copy, Paste, Find, Quick Select, Quick Open, Copy Mode, Copy Last Output and Clear Scrollback |
| View | Bigger, Smaller, Actual Size and Zoom Pane |
| Window | Minimize, Zoom, Next Tab and Previous Tab |

An item runs its action as the action's chord does, and shows the chord `[bind]` gives that
action, as of the last reload: a rebound action shows its new chord, an unbound one none. In a pane
that claims the keys ([Keys a pane can claim](#keys-a-pane-can-claim)), Close Pane therefore goes
to the program, as its chord would, when that chord is one the program's encoding can say (never
the default `Cmd+W`).

## Keys between panes

The focus, resize, close and last-pane keys are rtty's unless the pane's program claims them
([Keys a pane can claim](#keys-a-pane-can-claim)). `keys = "tutti"` makes `Ctrl+h/j/k/l` and
`Alt+h/j/k/l` rtty's too, as tutti's direct keys are, at the cost of those keys in programs that do
not claim them; by default every Ctrl key stays with the programs.

Focus by direction picks, among the panes beyond the focused one's edge that overlap it across,
the nearest; then the one focused last; then the one overlapping it most, as tmux and WezTerm do.
A swap trades places with that same pane.

## Keys a pane can claim

A program that sets the OSC 1337 user var `RTTY_KEYS` to `pane`
(`ESC ] 1337 ; SetUserVar=RTTY_KEYS=cGFuZQ== BEL`, the value base64) gets every chord bound to
`focus-…`, `resize-…`, `close-pane` or `last-pane` instead of rtty acting on it: the things an
editor does with its own windows. By default those are `Cmd+Opt+arrows`, `Cmd+Ctrl+arrows` and
`Cmd+W`, and with `keys = "tutti"`, `Ctrl+h/j/k/l` and `Alt+h/j/k/l`; `[bind]` adds its own, so
with `"alt-x" = "close-pane"` and `"ctrl-\\" = "last-pane"` Neovim's `Alt+x` closes a Neovim
window and its `Ctrl+\` goes to Neovim's previous one. Every other chord stays rtty's in a claiming
pane: splits, zoom, swaps, tabs and the rest. The claim ends when the var is set empty, when a
shell in the pane draws its next prompt (OSC 133;A), and with the program. `rtty cli list` says
which panes claim the keys ([rtty cli](cli.md#rtty-cli)).

A claimed chord is encoded as the program asked for keys, Cmd as the kitty protocol's super
modifier: Neovim (kitty flags 3) reads `Cmd+Opt+Left` as `<D-M-Left>` and `Cmd+Ctrl+Left` as
`<D-C-Left>`. No xterm encoding says Cmd in a way Emacs decodes (it reads `CSI 1 ; 11 D` as an
undefined `M-[ 1 ; 1`), and the kitty protocol, short of every key as an escape code (flag 8, which
Neovim does not ask for), has no form for Cmd with a key that types text. So a Cmd chord reaches
only a program speaking the kitty protocol, and `Cmd+W` reaches none: rtty keeps acting on such a
chord, claim or not, so `Cmd+W` closes the pane even with Neovim in it, and Emacs moves with the
tutti-style keys instead.

## Editors

[`editors/`](../editors/README.md) has a Neovim plugin (with a smart-splits.nvim backend) and an
Emacs package. Each claims the keys while the editor runs, moves between the editor's own windows,
and past its last window calls `rtty cli pane focus --from $RTTY_PANE`
([rtty cli](cli.md#rtty-cli)), so one set of keys moves through editor windows and rtty panes
alike. Both do nothing outside rtty, and inside tutti (`TUTTI_PANE`), where tutti's own plugins own
navigation.

## Secure Keyboard Entry

While it is on, macOS sends keys to rtty alone, so no other program (a keylogger, an app's
keyboard event tap) sees what is typed. `secure-input`, the App menu's Secure Keyboard Entry
(checked while on), turns it on and off, as in Terminal.app and iTerm2; `secure_input = true`
starts with it on. It is held only while rtty is the active app: rtty calls
`EnableSecureEventInput` as it becomes active, and `DisableSecureEventInput` as it stops being
active and as it quits, once for each enable, since macOS counts them and holds secure input for
every app while any is held.

With `auto_secure_input` (the default), it is on as well while the focused pane of the window with
the keyboard reads a password, as Ghostty does: its pty's echo off and canonical mode on (`sudo`,
`ssh`, `read -s`), looked at with `tcgetattr` as the pane prints, as its focus moves, and once a
second. A lock before the shown tab's title in the bar says it is on.

## The global hotkey

`global_hotkey`, a chord in `[bind]`'s syntax ([Key bindings](configuration.md#key-bindings)),
works from any app: while rtty is the active app it hides rtty, else it brings rtty forward at the
window that had the keyboard last (else the first, else a new one). macOS registers it for rtty
through Carbon's `RegisterEventHotKey` (the `global-hotkey` crate), which needs no Accessibility
permission, and takes the chord from every other app while rtty runs. It needs a modifier, unless
its key is a function key, and a key a US keyboard has: a shifted symbol is Shift and its key
(`` ctrl-shift-` ``, not `ctrl-~`). A chord macOS will not register is reported on stderr and in
[the log](scripting.md#the-log), and rtty runs without it.

## What the terminal gets

The terminal gets printable text (through the platform input handler, so dead keys and IME
composition go through macOS), Enter, Backspace, Tab, Shift+Tab, Esc, the arrows,
Home/End/PgUp/PgDn/Insert/Delete, F1–F12 (xterm encodings, with modifier parameters), Ctrl+letters
and Ctrl+`[ \ ] ^ _ @ space`, and Option as Alt: an ESC prefix (so Option+B is word-back in a shell,
and Option no longer types `∫`). Application cursor mode (DECCKM) switches the arrows to `ESC O`.
A held key repeats instead of opening macOS's accent picker.

## Option on one side

`option_as_alt` names the Option keys that are Alt: `"both"` (the default), `"left"`, `"right"` or
`"none"`. The other Option types what macOS gives it, as in any Mac text field: `˙` for Option+H,
`€` for Option+Shift+2 on a US layout, and é for Option+E then E (a dead key); no `alt-` binding
fires for those keys and no ESC goes with them. With a key that types nothing (an arrow, Enter,
Backspace), or with Ctrl or Cmd held, it stays Alt (`CSI 1 ; 3 D`, ESC DEL). Which Option is down
comes from the key event's device-dependent modifier flags, which the keyboard sets; an event
without them is taken as Alt. Such a key goes straight from AppKit to the window's text input, the
way GPUI hands it composing keys, and never reaches GPUI's bindings or rtty's key encoder.

## Keyboard protocols

Programs that ask for more get it (rtty-vt's encoder, the same one tutti's TUI uses):

- **xterm's modifyOtherKeys** (`CSI > 4 ; n m`, which Emacs turns on at level 1). Level 1 sends
  `CSI 27 ; mod ; code ~` for Ctrl with keys that have no control character (`C-;`, `C-.`,
  `C-1`), for Ctrl or Shift with Enter and for Ctrl with Tab (Shift+Tab stays `CSI Z`); every
  control character stays as it was. Level 2 sends it for every key with Ctrl or Alt, and Enter,
  Tab (Shift+Tab is `CSI 27 ; 2 ; 9 ~`), Backspace and Esc with any modifier. Level 3, xterm's
  newest, sends it for every key, unmodified ones too (`CSI 27 ; 1 ; 32 ~` for space); any other
  level is off, as in xterm. xterm's formatOtherKeys (`CSI > 4 ; 1 f`) turns each of those into
  `CSI code ; mod u`. Without modifyOtherKeys, Ctrl leaves a key it has no control character for
  as it is (`C-1` types 1).
- **The kitty keyboard protocol** (`CSI > flags u`; Neovim pushes flags 3). Disambiguate: Esc is
  `CSI 27 u`, keys with Ctrl or Alt are `CSI code ; mod u` (so `C-i` is not Tab and `S-Enter` is
  not Enter), and the other keys use the `CSI 1 ; mod X` and `CSI n ; mod ~` forms, never `SS3`
  (F3 is `CSI 13 ~`); plain text and unmodified Enter, Tab and Backspace stay as they were. Event
  types add repeats (`:2`) and releases (`:3`) of those keys. All keys as escape codes, alternate
  (shifted) keys and associated text are encoded too, though no program checked here uses them.
  The wheel's arrow keys on the alternate screen are the arrow keys as the program asked for them.

GPUI does not report keypad keys apart from the main keys, so application keypad mode (DECKPAM)
has nothing to change, and a shifted symbol arrives as itself (`!`, not Shift+`1`).

## What a command leaves on

A program killed with SIGKILL turns off none of the reports and key encodings it turned on, and
the shell after it then gets a click, a focus change or a flip to dark as typing, and its keys in
kitty's encoding. When the shell marks the command's end (OSC 133;D, the first of a run of them;
[Shell integration](shell-integration.md)), rtty turns off the mouse reports (every mode and
encoding), focus reports (1004), light/dark reports (2031), modifyOtherKeys and its format, and
kitty's flags, right there in the stream, so what the shell sets for its next prompt stays.
Bracketed paste and the cursor and keypad modes stay too: shells set those themselves. tutti does
the same.
