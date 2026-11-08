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

macOS only (GPUI's Metal renderer, and AppKit for notifications and Finder); no Linux port is
planned. Needs a Rust toolchain with edition 2024 and Xcode's SDK; the offline Metal compiler is
**not** needed (see [the GPUI pin](#the-gpui-pin)).

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
| `--ligatures on\|off` | the font's programming ligatures: on enables `calt` and `liga`, off disables `calt`, `liga` and `dlig`, over the config's `ligatures` and `font_features`; default on |
| `-e PROGRAM [ARGS...]` | run the program in the first pane instead of the login shell; its pane closes when it exits (new panes run the shell) |
| `--stats` | once a second while a pane draws frames, print its frames, layout and paint ms/frame (a line per pane that drew) |
| `--trace FILE` | write every read from a pty (`<`) and write to it (`>`), escaped, one line each with the milliseconds since start and the pane's number: what a program asks and what it was told |
| `--script FILE` | drive the window from a script instead of a person: wait for text, keys and typed text (through GPUI's keystroke dispatch, so Cmd chords split and switch panes, or with `nskey` as AppKit key events handed to NSApplication in process, `alt-h->˙` giving the character Option types, which take macOS's own path: event monitors, key equivalent, text input context, key down), clicks, drags, the wheel and dropped files at window cells (to the pane under them), clicks and hovers on the tab bar's pills, close buttons and `+`, the answer to a question closing, quitting or a clipboard read asked (which a scripted window keeps in its dump instead of showing a sheet; a scripted read is answered with a stand-in, never the clipboard), AppKit's `terminate:` as the dock's Quit sends it, another window's close button, a menu bar item's action, rtty becoming the active app (with any of its windows the key one) and not, a press of the global hotkey, focus in and out, an appearance flip, a resize, and `dump`, which writes the window's tabs, bar and layout and what the next frame draws in each shown pane (runs, their glyphs, colors, underlines, the cursor, the font with its features, the cell and the rows each drawn character fills) and the modes it is drawn from, with the shell's marks, a running command and what `copy-last-output` copied (never to the clipboard under `--script`); the step list is in `src/script.rs`. A scripted window does not take the keyboard from whatever app has it |
| `--activate` | with `--script`: take the keyboard as a normal launch does, for the checks that need a focused window (focus reports, the focused cursor) |
| `--restore` | with `--script`: restore the saved layout and save it, as a normal launch does ([Restoring windows](#configuration)); the script drives the first window |
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
why, each pane's start, split, close (with the chord that closed it) and its program's exit
status, a tab's close button, the question a close asked and its answer (never what runs), a
close undone or made for good, the dock's progress badge as it changes, what came of each config
reload, the layout restored at launch (or why none was), each Cmd chord as AppKit
hands it to rtty and as GPUI dispatched it
(the window's activation, the key contexts it met, the actions bound to it there and the one that
ran), and every panic, on any thread, with its backtrace. Never what a pane shows or is sent. Past a megabyte it starts over,
keeping the last as `rtty.log.old`. When rtty vanishes, its last lines say whether it quit (and
why) or panicked (and where).

**Install.** With [just](https://github.com/casey/just):

```sh
just install                          # /Applications/rtty.app, and ~/.local/bin/rtty
just install ~/Applications ~/bin     # somewhere else: the app's folder, then the link's
just uninstall                        # the same arguments as install
just bundle                           # only build target/rtty.app
```

`just install` builds the release binary, assembles `rtty.app` (the binary, `bundle/Info.plist`
with Cargo.toml's version filled in, the icon), signs it, moves it into the folder in
place of the one there (moved whole, so the folder never holds a copy under another name), and
links `~/.local/bin/rtty` to the binary inside
it, so `rtty cli` works from any shell with `~/.local/bin` on its `PATH`. A `~/.local/bin/rtty`
that is a file rather than a link stops the install untouched. `just uninstall` removes the
app, and the link only when it is the one install made. The signature is ad hoc (`codesign --sign -`, no developer identity), which is
all an app built on the Mac that runs it needs: Gatekeeper's notarization check applies to
downloaded apps, and a local build is not one.

**The icon.** `bundle/icon.py` draws it from shapes, no fonts: a ❯_ prompt in One Dark Pro's
colors on macOS's rounded app-icon square, with Pillow at 4× and downsampled. `just icon` (needs
[uv](https://docs.astral.sh/uv/)) reruns it and rebuilds `bundle/rtty.icns` with `sips` and
`iconutil`, leaving `target/icon/icon-sizes.png`, the icon at 16 to 256 px, to check it still
reads small.

## Configuration

`~/.config/rtty/config.toml` (`$XDG_CONFIG_HOME/rtty/config.toml` when that is set). Every key is
optional; an unknown key, an unknown scheme name or a malformed file is an error that names it,
and rtty does not start. Once rtty runs, a saved change takes effect by itself (below).

`Cmd+,` (`open-config`) opens the file in a new tab. With no file there it first writes
[`config.template.toml`](config.template.toml), every key commented out at its default. The tab
runs your login shell as an interactive login shell, so that its dotfiles set `EDITOR` (an app
started from the Dock or Finder has no `EDITOR` of its own), and the shell runs `$EDITOR` on the
file, or `open -t` (TextEdit, or your default text editor) when `EDITOR` is empty; the tab closes
with the editor. The words it runs are the same in zsh, bash and fish (fish untried).

```toml
font = "FiraCode Nerd Font"
fallback = ["Symbols Nerd Font Mono"]  # for what `font` lacks, in order (Fonts, below)
size = 12
ligatures = true
font_features = ["ss01", "zero"]  # OpenType features, over `ligatures`' (Fonts, below)
line_height = 1.2                 # the cell's height as a share of the font's; 1 by default
scheme_dark = "OneDark Pro"       # the default
scheme_light = "Solarized Light"  # the default
# scheme = "Solarized Dark"       # one scheme whatever the OS appearance; not with the two above
keys = "rtty"                     # the default; "tutti" adds Ctrl+h/j/k/l and Alt+h/j/k/l (Keys)
bell = "visual"                   # the default: BEL flashes the pane (and the dock, unfocused); or "none"
option_as_alt = "both"            # the default; "left", "right" or "none": the Option keys that are Alt (Keys)
minimum_contrast = 1              # the default, off; 4.5 keeps text legible (Color schemes)
confirm_close = true              # the default: ask before closing what runs a program (Keys)
undo_timeout = 5                  # the default: seconds a close waits for Cmd+Z (Keys); 0: none
shell_integration = "auto"        # the default: zsh, bash and fish load rtty's (Shell integration); or "none"
notify_after = 10                 # the default: notify a command of 10 s or more that ends out of view; 0: never
click_to_move = true              # the default: a click on the command line moves the shell's cursor there
editor = ["nvim", "+{line}", "{path}"]  # edits the files Cmd+click opens (Links); unset: $EDITOR
quick_select_patterns = ['[A-Z]+-\d+']  # regexes quick select labels too (Quick select)
restore_layout = true             # the default: the windows come back at the next launch (below)
graphics = true                   # the default: programs show images (Images); false drops their commands
secure_input = false              # the default: Secure Keyboard Entry starts off (Keys)
auto_secure_input = true          # the default: Secure Keyboard Entry at a password prompt (Keys)
global_hotkey = "ctrl-`"          # a chord that shows and hides rtty from any app; unset by default (Keys)
clipboard_read = "ask"            # the default: a program's OSC 52 read asks first; or "allow", "deny" (Keys)

[bind]                            # chords and their actions, over the defaults (Keys)
"alt-\\" = "split-right"
"cmd-shift-d" = "none"            # unbinds a default
```

**Key bindings.** `[bind]` maps chords to the actions named in [Keys](#keys). A chord there is
added, or rebound if it has a default, and `"none"` unbinds a default (`keys = "tutti"`'s
included). A chord is written in GPUI's keystroke syntax: modifiers (`ctrl`, `alt` for Option,
`shift`, `cmd`, `fn`), then one key, joined by `-`: `"ctrl-h"`, `"alt-shift-h"`, `"cmd-d"`,
`"alt--"`. The key is a character, or `space`, `tab`, `enter`, `backspace`, `escape`, `up`,
`down`, `left`, `right`, `pageup`, `pagedown`, `home`, `end`, `insert`, `delete` or `f1`–`f35`.

- An Option chord is `alt-` and the key, never the character Option types: `alt-h`, not `˙`.
- A capital letter is Shift and the letter (`"alt-H"` is `"alt-shift-h"`), and a shifted symbol
  is the symbol itself: Option+Shift+\ is `"alt-|"`, never `"alt-shift-\\"`.
- In a double-quoted TOML key a backslash is doubled: `"ctrl-\\"` is Ctrl+\.

An unknown action, a chord that is not one, two spellings of one chord, or `none` on a chord with
nothing bound stops rtty with an error naming the chord and its line.

**Reloading.** rtty looks at the config file, and at the scheme files it names (not the built-in
ones), once a second, and reads the config again when one has changed since it was last read;
`Cmd+Shift+,` (`reload-config`, Ghostty's chord) reads it at once. A file that is gone is the
defaults, as at launch, unless `--config` named it: that is a mistake at a reload as at launch.
Every window says what came of it along its bottom for a few seconds, and
the log says it too. A mistake, which at launch would stop rtty, keeps the config in effect and
says what and where it is (`config not reloaded, the last one stays: …config.toml: line 6:
"cmd-d": unknown action "split-sideways"`). `--font`, `--size` and `--ligatures` stay over the
file's keys. What changes, and when:

| Keys | Take effect |
| --- | --- |
| `font`, `fallback`, `size`, `ligatures`, `font_features`, `line_height` | at once in every pane of every window, which reshapes its text and refits its cells; a size set with `Cmd+=`/`Cmd+-` stays unless `size` itself changed, and `Cmd+0` goes back to the new `size` |
| `scheme`, `scheme_dark`, `scheme_light`, and an edit to a scheme file they name | at once: every pane, the tab bar and the dividers repaint, and a program that asked for DEC mode 2031 hears of a flip between dark and light |
| `keys`, `[bind]` | at once, for the next chord, and the menu bar shows the new chords |
| `option_as_alt`, `minimum_contrast` | at once |
| `bell`, `confirm_close`, `click_to_move` | from the next bell, close or click |
| `editor` | for the next file a `Cmd+click` or quick open edits; an editor already open keeps going |
| `quick_select_patterns` | from the next quick select |
| `undo_timeout` | from the next close; one already waiting keeps its time |
| `notify_after` | for every command that ends from then on, running ones included |
| `shell_integration` | new panes only: a shell that is running has loaded rtty's scripts, or not, as it started |
| `restore_layout` | saving the layout stops or starts at once; restoring it is at launch |
| `graphics` | new panes only: a pane's engine takes graphics commands, or not, from its first byte |
| `secure_input` | when it changed: Secure Keyboard Entry's toggle (the App menu's) is set to it at once, and secure input is held or let go to match; a reload that leaves it as it was keeps the menu's choice |
| `auto_secure_input` | at once: secure input is held or let go for the pane with the keyboard as it is now |
| `global_hotkey` | at once: the old chord is given back to the other apps and the new one taken |
| `clipboard_read` | from the next read a program asks for |

**Restoring windows.** With `restore_layout = true`, the default (Terminal.app and iTerm2 restore
their windows by default too), rtty keeps each window's layout for the next launch: its frame and
display, its tabs, each tab's splits with their ratios, zoom and focus, and each pane's working
directory (as its shell last reported it with OSC 7, else its foreground process's). It saves as it
quits, and once a second when any of that changed since it last wrote it; a write that fails is
tried again each second, its error logged once. At launch it opens those windows, each pane a new
login shell in its directory (the home directory, when that is gone). It never starts the programs
that ran there and never keeps what a pane showed. A missing or corrupt file starts fresh, and the
log says why: corrupt is also a pane in two tabs, a focus on no pane, a ratio past 1, or a frame
under 100 points across or down. Closing the last window quits with no window to restore, so the
next launch opens one fresh window.

One rtty at a time keeps the layout: the first to lock `layout.lock` beside the file, which it holds
until it exits. Another started meanwhile (`cargo run` beside the installed app) neither restores
nor saves, so the first one's layout stays its own. A launch with `-e` or `--script` (short of
`--restore`) never tries for it.

The file is `$XDG_STATE_HOME/rtty/layout.json` (an empty or relative `XDG_STATE_HOME` is ignored),
else `~/.local/state/rtty/layout.json`, beside the config's `~/.config` as XDG has it, since rtty's
directory in `$TMPDIR` holds what may go with a restart. It is readable by its owner alone (0600)
and written whole, through a rename: JSON, a list of windows, each with its `frame` (points from its
display's top left), `display` (the display's UUID; on another display, or none, or with the
middle of its titlebar off that display, the window is centered on the main one), `tabs` (each tab's
split tree, by pane id, and which shows) and `cwds` (each pane's directory).

A Neovim-like layout: the window keys smart-splits.nvim suggests, with LazyVim's split and zoom
keys (`<leader>|`, `<leader>-`, `<leader>wm`) moved onto Option, since a terminal has no leader.
In a pane where Neovim [claims the keys](#rtty-cli-and-editors), focus, resize, `Alt+x` and
`Ctrl+\` go to Neovim, which hands focus and resize back to rtty past its own last window.

```toml
keys = "tutti"                  # Ctrl+h/j/k/l focus, Alt+h/j/k/l resize

[bind]
"alt-x" = "close-pane"
"ctrl-\\" = "last-pane"
"alt-\\" = "split-right"        # Option+\, for |
"alt--" = "split-down"
"alt-m" = "zoom"
"alt-shift-h" = "swap-left"
"alt-shift-j" = "swap-down"
"alt-shift-k" = "swap-up"
"alt-shift-l" = "swap-right"
```

**Fonts.** A character `font` lacks comes from the `fallback` families, in order, then from
macOS's own fallback (emoji, CJK). macOS's knows no font with Nerd Font icons, so with no
`fallback` key rtty adds an installed Nerd Font: Symbols Nerd Font Mono, else Symbols Nerd Font,
else the first `… Nerd Font Mono` family by name, else the first `… Nerd Font`. rtty bundles no
font; with no Nerd Font installed the icons stay missing. An unknown family, in `font` or in
`fallback`, is an error.

**Font features.** `font_features` turns OpenType features of the font on and off, as CoreText
shapes the text: `"ss01"` or `"+ss01"` turns one on, `"-calt"` off, and `"cv01=2"` picks the
font's second alternate where a feature has several. A tag is four letters or digits; anything else
is an error. Which features there are is the font's: Fira Code's `zero` slashes its zero and `ss01`
gives another `r`, and a font without a feature ignores it. `ligatures` is two of these features
too ([Ligatures](#ligatures)): `true` turns on `calt` and `liga`, `false` turns off those and
`dlig`. The words apply in order, the last on a tag winning: `ligatures`, then `font_features`, then
`--ligatures`. So `font_features = ["-calt"]` turns off the contextual alternates that make Fira
Code's, JetBrains Mono's and Cascadia's `->` and `!=`, and `"-liga"` the standard ligatures some
other fonts use; `--ligatures on` turns them back on for that launch.

**Line height.** `line_height` makes the cell taller (or shorter) than the font's ascent plus
descent, from 0.5 to 3 times it, rounded up to whole points; the text is centered in the taller
cell. Box drawing, block and powerline characters, which rtty draws itself, fill the whole cell, so
they still join from row to row. An underline keeps its place under the text, and a strikethrough
through it; in a cell shorter than the font's text (below about 0.95 for Menlo) they rise to stay
inside the cell, never in the next row's: the underline to the cell's bottom, a double or curly one
to a thickness above it.

rtty draws box drawing (U+2500–257F), block elements (U+2580–259F) and the powerline separators
(U+E0B0–E0BF) itself, filling each cell exactly: lines meet the next cell's, blocks tile, and the
separators fit the cell's height whatever the font, Menlo included. An icon (a private use area
character) or any glyph from another font whose ink is wider than its cell (a Nerd Font that is
not its Mono variant draws its icons about two cells wide) takes the next cell too when that one
is blank, as prompts leave one after each icon, and is centered across both, shrunk only if it is
wider still, as WezTerm and Ghostty draw them; with no blank after it, it is drawn smaller and
centered in its own cell instead of covering its neighbour. A wide character keeps to its two
cells, and anything taller than the line shrinks to it. This is drawing only: the cells stay as
the terminal has them. The font's own glyphs are left alone, so a ligature still reaches over the
cells after it.

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
`onedarkpro.toml` whatever it calls itself). Every file in those directories is read at startup
and at each reload, so a malformed one stops rtty with its path and the offending key (and keeps
the config in effect at a reload).

**Following the OS.** rtty uses `scheme_dark` while the window's appearance is dark and
`scheme_light` while it is light, and switches the moment macOS flips, with no restart. Cells keep
their color indices, so everything already on screen recolors on the next frame. Colors a program
set itself (OSC 4/10/11/12) are the program's, not the scheme's, and survive a flip; a program
that cares re-sets them when it hears about the flip (below). OSC 10/11/12/4 queries answer with
the current scheme.

**Minimum contrast.** `minimum_contrast` is a WCAG contrast ratio from 1 (the default, which
changes nothing) to 21. Text whose color falls short of it on its cell's background moves toward
white on a dark background, toward black on a light one, just far enough to reach it (or as far
as it goes); only the text's color moves, never the background. At 4.5, WCAG's ratio for text,
OneDark Pro's comment grey `#5c6370` (2.3:1) draws as `#8e939c`, and Solarized Light's comments,
base1 `#93a1a1` (2.5:1), as `#697373`; Solarized Light's own text, base00 `#657b83`, is 4.1:1
and darkens a shade, to `#60747c`. Text the program made inverse or hidden, the selection and
search matches keep their colors. iTerm2 and Ghostty have a setting of the same name.

**Telling programs (DEC mode 2031).** rtty implements contour's color palette update
notifications. `CSI ? 996 n` asks for the scheme and gets `CSI ? 997 ; 1 n` (dark) or
`CSI ? 997 ; 2 n` (light). A program that set `CSI ? 2031 h` gets that report, unsolicited, each
time the scheme changes, so Neovim and friends can switch `background` live. `CSI ? 2031 l` or RIS
turns it off, and DECRQM (`CSI ? 2031 $ p`) reports the mode's state. Dark or light is the
scheme's background, not the OS setting, so a pinned light scheme answers light on a dark Mac.
rtty-vt answers all of these itself: rtty hands it the scheme's palette and leaning at startup,
on every flip and at a reload that changes the scheme.

## Keys

Ctrl keys belong to the terminal; Cmd chords belong to the app. These are the defaults; the
action names are `[bind]`'s ([Configuration](#configuration)).

| Key | Action | Does |
| --- | --- | --- |
| `Cmd+Q` | `quit` | quit; asks first while a pane in any window runs a program (below) |
| `Cmd+N` | `new-window` | new window |
| `Cmd+T` | `new-tab` | new tab, after the current one |
| `Cmd+1`–`9` | `tab-1`–`tab-9` | show that tab |
| `Cmd+Shift+[` / `Cmd+Shift+]` | `prev-tab` / `next-tab` | previous / next tab, round the ends |
| `Cmd+D` / `Cmd+Shift+D` | `split-right` / `split-down` | split the pane right / down |
| `Cmd+W` | `close-pane` | close the pane, asking first while it runs a program (below); the last pane closes its tab, the last tab its window, the last window quits |
| `Cmd+Z` | `undo-close` | bring back the pane, tab or window closed last, within `undo_timeout` seconds (below) |
| `Cmd+Opt+arrows` | `focus-left`, `-right`, `-up`, `-down` | focus the nearest pane that way |
| `Cmd+Ctrl+arrows` | `resize-left`, `-right`, `-up`, `-down` | move the pane's nearest divider that way, 5% of its split a press (tutti's step) |
| `Ctrl+h/j/k/l`, `Alt+h/j/k/l` | `focus-…`, `resize-…` | with `keys = "tutti"` only: focus and resize, as in tutti |
| none | `last-pane` | focus the pane the focus came from in this tab, and back again, as Vim's `wincmd p` does; nothing once that pane has closed |
| none | `swap-left`, `-right`, `-up`, `-down` | trade places with the pane focus would go to that way; the splits stay, and the focus goes with the pane |
| `Cmd+Shift+Enter` | `zoom` | zoom the pane to fill its tab, or back |
| `Cmd+C` | `copy` | copy the selection |
| `Cmd+Shift+A` | `copy-last-output` | copy the last command's output, scrollback included ([Shell integration](#shell-integration)), as iTerm2's chord does |
| `Cmd+V` | `paste` | paste (bracketed when the program asked for it) |
| `Cmd+F` | `find` | search the pane's scrollback and screen (below) |
| `Cmd+Shift+Space` | `quick-select` | label what is worth copying on the pane's screen; typing a label copies it, in uppercase pastes it too (below) |
| `Cmd+Shift+O` | `quick-open` | label the links on the pane's screen; typing a label opens it as `Cmd+click` would (below) |
| `Cmd+Up` / `Cmd+Down` | `prompt-prev` / `prompt-next` | the view to the shell's previous / next prompt (OSC 133;A); past the last, back to the output |
| `Cmd+K` | `clear-scrollback` | clear the pane's scrollback and screen, then send the program Ctrl+L so a shell draws its prompt again at the top, as Terminal.app does; under a full-screen program (the alternate screen: Neovim, less) only the scrollback goes, and the program is sent nothing |
| `Cmd+=` / `Cmd+-` | `font-bigger` / `font-smaller` | font size up / down, in every pane of every window |
| `Cmd+0` | `font-reset` | font size back to the configured one (`size`, or `--size`) |
| `Cmd+Shift+,` | `reload-config` | read the config file again now ([Configuration](#configuration)); rtty also does when it changes |
| `Cmd+,` | `open-config` | the config file in your editor, in a new tab; written first from rtty's commented template when there is none ([Configuration](#configuration)) |
| `Cmd+Shift+W` | `close-window` | close the window, asking first as its close button does |
| `Cmd+M` | `minimize` | minimize the window into the dock |
| none | `zoom-window` | zoom the window, as a double click on its bar does |
| `Cmd+H` / `Cmd+Opt+H` | `hide` / `hide-others` | hide rtty / hide every other app |
| none | `show-all` / `about` | show every app again / rtty's About panel |
| none | `secure-input` | turn Secure Keyboard Entry on or off (below) |
| `Cmd` over a link, `Cmd+click` | | underline the link (a URL, or a path such as `src/main.rs:12:5`) under the pointer; open it (below) |
| mouse drag | | select (double click: word, triple click: line; Option+drag: a block, below), within the pane; held past its top or bottom, the scrollback scrolls that way, faster the further out; Shift+drag (Shift+Option+drag for a block) when a program takes the mouse |
| mouse buttons | | a click focuses the pane under it; reported to a program that asked (modes 9, 1000, 1002, 1003, in the default, UTF-8, SGR, urxvt or SGR-pixel encoding), at the pane's own cells; in the pane with the keyboard, a click on the command line being typed moves the shell's cursor there ([Shell integration](#shell-integration)) |
| divider drag | | resize the panes on either side |
| file drop | | files dragged from Finder onto a pane type their paths there, separated by spaces, with a backslash before each character a shell reads specially (`/tmp/Screen\ Shot.png`, `it\'s`), and non-ASCII as it is; a paste, bracketed when the program asked for that, so an agent such as Claude Code gets a dropped screenshot's path as pasted text. The pane takes the keyboard. A path holding a control character, which would arrive as a key (Enter, Ctrl+U), types nothing |
| tab click | | show that tab; its × (under the pointer) closes it and every pane in it; `+` opens a new tab, as `Cmd+T` does |
| double click on the bar | | zoom the window, as on any titlebar (System Settings decides what a double click does) |
| wheel | | scrollback on the primary screen; wheel reports to a program that asked for mouse reports; arrow keys to any other full-screen program (alternate scroll, on by default) |

**The menu bar.** rtty's menus hold the actions above: About, Settings… (`open-config`), Reload
Configuration, Secure Keyboard Entry, Hide, Hide Others, Show All and Quit; in Shell, New Window,
New Tab, the splits, Close Pane, Close Window and Undo Close; in Edit, Copy, Paste, Find, Quick
Select, Quick Open, Copy Last Output and Clear Scrollback; in View, Bigger, Smaller, Actual Size
and Zoom Pane; in Window, Minimize, Zoom, Next Tab and Previous Tab. An item runs its action as
the action's chord does, and shows the chord `[bind]` gives that action, as of the last reload: a
rebound action shows its new chord, an unbound one none. In a pane that claims the keys
([Editors](#rtty-cli-and-editors)), Close Pane therefore goes to the program, as its chord would,
when that chord is one the program's encoding can say (never the default `Cmd+W`).

**Secure Keyboard Entry.** While it is on, macOS sends keys to rtty alone, so no other program
(a keylogger, an app's keyboard event tap) sees what is typed. `secure-input`, the App menu's
Secure Keyboard Entry (checked while on), turns it on and off, as in Terminal.app and iTerm2;
`secure_input = true` starts with it on. It is held only while rtty is the active app: rtty
calls `EnableSecureEventInput` as it becomes active, and `DisableSecureEventInput` as it stops
being active and as it quits, once for each enable, since macOS counts them and holds secure
input for every app while any is held. With `auto_secure_input` (the default), it is on as well
while the focused pane of the window with the keyboard reads a password, as Ghostty does: its
pty's echo off and canonical mode on (`sudo`, `ssh`, `read -s`), looked at with `tcgetattr` as
the pane prints, as its focus moves, and once a second. A lock before the shown tab's title in the
bar says it is on.

**The global hotkey.** `global_hotkey`, a chord in `[bind]`'s syntax, works from any app: while
rtty is the active app it hides rtty, else it brings rtty forward at the window that had the
keyboard last (else the first, else a new one). macOS registers it for rtty through Carbon's
`RegisterEventHotKey` (the `global-hotkey` crate), which needs no Accessibility permission, and
takes the chord from every other app while rtty runs. It needs a modifier, unless its key is a
function key, and a key a US keyboard has: a shifted symbol is Shift and its key
(`` ctrl-shift-` ``, not `ctrl-~`). A chord macOS will not register is reported on stderr and in
the log, and rtty runs without it.

New panes, tabs and windows run the login shell in the focused pane's working directory: as its
shell last reported it with OSC 7 (`ESC ] 7 ; file://host/path BEL`), else the directory of the
pane's foreground process (the pty's process group leader: the shell at a prompt, or the program
it runs), else your home directory. rtty's [shell integration](#shell-integration) sends OSC 7 at
each prompt; without it, macOS's zsh and bash send it only from `/etc/zshrc_$TERM_PROGRAM`, which
exists for Terminal.app alone, and the foreground process is what answers. A pane closes when its
program exits, with the same cascade as `Cmd+W`.

**Closing what runs.** Closing a pane (`close-pane`, whatever chord it is bound to), a tab (its
×), a window (its close button) or rtty (`Cmd+Q`, the menu's Quit, the dock's Quit, logging out)
asks first, in a sheet on the window, while anything other than a pane's shell at its prompt
holds that pane's foreground (the pty's process group, as titles find it): an agent, Neovim, a
build. A pane's question names its program; a tab's, a window's and the quit's count the panes
running something and name what each runs. `Close` (or `Quit`, the default button) goes on and
`Cancel` (Esc) keeps everything. A pane whose shell sits at its prompt closes at once, as does a
pane whose program exits. `confirm_close = false` never asks. Quitting asks on the window that
had the keyboard last; should that window close before the answer (its last program exited),
rtty asks again on another, about what still runs, or quits when nothing does. When macOS quits
rtty (the dock's Quit, logging out), it waits for the answer, through
`applicationShouldTerminate:`: `Quit` lets the logout go on, `Cancel` stops it. Known limits: only the login shell rtty starts itself counts
as a shell, so a pane started with `rtty -e /bin/zsh` asks even at an idle prompt; and a command
forked a moment before the close may not hold the foreground yet, and then is not seen as
running.

**Undoing a close.** A closed pane, tab or window does not go at once: it waits, hidden, its
programs running on, for `undo_timeout` seconds (5 by default), and `Cmd+Z` (`undo-close`) brings
it back as it was: a pane into its split, at its side and share of the cells (beside the pane it
sat by, when the splits around it changed; in a tab of its own, when those went too), a tab at its
place in the row, a window on screen again with its tabs. Only the last close waits: a close ends
the one before it, as does its time running out, and then its programs are hung up on as a close
always did. A closed pane whose program exits meanwhile leaves the close, and the rest of it
still comes back (the other panes of a closed tab or window); a close with nothing left ends.
Closing the last window shown is for good, and so is quitting; `undo_timeout = 0` makes every
close final.

The focus, resize, close and last-pane keys are rtty's unless the pane's program claims them
([Editors](#rtty-cli-and-editors)). `keys = "tutti"` makes `Ctrl+h/j/k/l` and `Alt+h/j/k/l` rtty's
too, as tutti's direct keys are, at the cost of those keys in programs that do not claim them; by
default every Ctrl key stays with the programs.
Focus by direction picks, among the panes beyond the focused one's edge that overlap it across,
the nearest; then the one focused last; then the one overlapping it most, as tmux and WezTerm do.
A swap trades places with that same pane.

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

**Progress.** A program that reports its progress, with ConEmu's OSC 9;4 (`ESC ] 9 ; 4 ; state
; percent ST`, as Claude Code does with its progress setting on, and Ghostty and Windows Terminal
read it) or tutti's OSC 7501 program status (`state=working:progress=40`), shows it as a thin line
along the bottom of its tab's pill, between its rounded ends (with one tab, along the bar's
bottom edge): the scheme's blue, as far along as the percent; its yellow while paused (or, for
OSC 7501, `blocked`); its red on an error; the whole line in a faint blue while the program
cannot say how far. A tab with several reporting panes shows an error first, else the least
done. The line goes when the program removes it (`9;4;0`, or `done`, `idle` or `clear`) or a
shell draws its next prompt (OSC 133;A), so a build killed mid-report leaves nothing behind.

rtty's dock icon carries a badge that sums up every pane's progress in every window: `!` while
one reports an error, else the percent of the least done, else `…` while a program cannot say
how far; it goes once every program is done. It shows whether rtty is in front or not: a pill
shows only its own tab, and the badge is the one place an error in a tab or window out of sight
shows. (Checked through `--script`'s dump and the log's `dock badge` lines, not yet by eye.)

**Titles.** A pane is called by the title its program set (OSC 0 or 2); else, while a program
runs in its foreground (the pty's process group, `tcgetpgrp`), by that program's command line
(`cargo run --release`); else, with the shell at its prompt, by the shell's directory (OSC 7 when
the shell reports it, else the shell's own working directory), home written `~`. A pane looks at
its foreground process and directory once a second, and at once when its program sets a title or
its shell reports a directory. A tab is titled by its focused pane, the window by its shown tab,
and `rtty cli list` reports both. The focused pane draws the focused cursor and the others a
hollow one; focus reports (mode 1004) go to the pane gaining and the pane losing the keyboard.

**Block selection.** Option+drag selects a block, the same columns on every row it spans, as in
Terminal.app (and WezTerm and Ghostty; iTerm2 takes Cmd+Option). Copying it gives each row's
columns, trailing blanks trimmed, the rows joined by newlines. A wide character is in a block when
its first cell is: one the block's right edge cuts is drawn and copied whole, one its left edge
cuts is left out. While a program takes the mouse an Option+drag is the program's (Emacs's
secondary selection is `M-drag-mouse-1`), so Shift+Option+drag selects a block there, as Shift+drag
selects in a program's place.

**Search.** `Cmd+F` opens a bar at the pane's top right, which takes the keyboard until `Esc`. The
query searches every row the pane holds, scrollback and screen, as lines: a match runs on across a
soft wrap. A query without a capital ignores case (smart-case); `Ctrl+R` switches between text
and a regex. Matches show on the scheme's yellow and the current one on its cursor color; `Enter`
and `Shift+Enter` go to the next and previous match, round the ends, scrolling the view to it.
Matches stay on their text while output scrolls it up, and output arriving with the bar open is
searched again.

**Links.** A link is an OSC 8 hyperlink, a plain `http`, `https` or `file` URL in the text
(trailing punctuation left out), or the path of a file that exists (below). Holding `Cmd`
underlines the one under the pointer and `Cmd+click` follows it: `http`, `https` and `mailto` open
with the system's handler; a `file` link is shown in Finder, selected, and never opened, since
opening a file URL launches what it names (an app, a `.command` script, an executable) and an OSC 8
link's text can claim anything; any other scheme is ignored. Nothing happens without the click.
While `Cmd` is held over a link whose text is not its target (an OSC 8 "click here", or one showing
one address and going to another), the pane's bottom edge shows where it really goes and what a
click will do.

**Paths.** A path is found as compilers, grep, test runners and stack traces print it, with a line
and column after it or not: `src/view.rs:123:45`, `./a/b.rs:7`, `/abs/x.py:3`, `~/notes.md`,
`Foo.java:12`, Python's `File "x.py", line 3`. It has a slash or is a name with an extension, holds
no space, quote, bracket, colon or backslash, and starts a line or follows a space, a quote, an
opening bracket or `=`, never a colon, so neither a URL's `//host` nor any part of a Windows
`C:\x` is one; a sentence's full stop after it is left out. It is a link only when it exists: a
relative path from the directory the shell last reported (OSC 7, which [shell
integration](#shell-integration) sends at each prompt), else the foreground process's. rtty looks
only while `Cmd` is held over it and at the click, never per frame. A click on

- **text** (a file with no NUL byte in its first 8 KiB) edits it at the line and column:
  1. in a Neovim running rtty's plugin, in that pane or else in another pane of its tab, which
     then takes the keyboard (the plugin names its server to rtty: [editors](editors/README.md)).
     rtty believes that name only while `nvim` is the pane's foreground program and the server is
     a socket of yours, since any output can claim it. It asks off the window's thread, and the
     Neovims have half a second together; one that reads the request later refuses it, so the
     file never opens both there and in a pane;
  2. else in a new pane to the right, which runs `editor` with `{path}`, `{line}` and `{col}`
     filled in, or with no `editor`, `$EDITOR +line path` (`vi` without an EDITOR). Your login
     shell runs it, interactive, so it finds the PATH and EDITOR your dotfiles set, which an app
     started from Finder lacks. The pane closes when the editor exits; a GUI editor's command
     (`["code", "-g", "{path}:{line}:{col}"]`) returns at once, and its pane closes with it.
- **anything else** (a directory, an image, a PDF) opens with macOS's `open`, except what opening
  would run: a directory with an extension (an app, a bundle), an executable, and a `.command`,
  `.tool`, `.terminal`, `.jar`, `.fileloc`, `.inetloc` or `.webloc` that is not text are shown in
  Finder instead.

A path is taken for what it really is, its symlinks resolved, so a link named `tool` to an app
is the app. What is neither a file nor a directory (a FIFO, a socket, a device) is only shown in
Finder, never read.

**Quick select.** `Cmd+Shift+Space` (`quick-select`; WezTerm's QuickSelect is the idea, on
Ctrl+Shift+Space there, a Ctrl key and so a program's here) labels everything worth copying on the
focused pane's screen: URLs, paths, git hashes (7 to 40 hex digits), IPv4 and IPv6 addresses,
UUIDs, numbers of four digits or more, and what the regexes in `quick_select_patterns` match (a
pattern's first group, when it has one). Of two that overlap, the one starting first is labeled.
Each match shows on the search's yellow under a bold label in the scheme's colors inverted, which
reads in a light scheme and a dark one alike. Labels are home-row letters (`asdfghjkl`), the
shortest nearest the cursor, and none is the start of another: past nine matches, letters stand
for two-letter labels. Typing a label copies its match and says so, as `copy-last-output` does;
typing it with Shift (`A`, or `aS`) pastes it into the pane too, bracketed when the program asked
for that. Esc puts the labels away, as does any other key, which goes nowhere else: no binding,
no program. `Cmd+Shift+O` (`quick-open`) labels only what a `Cmd+click` would follow: URLs and
paths to files that exist (not OSC 8 links); typing a label follows it as the click would have
when the labels appeared, whatever output has moved under them since.

**The clipboard.** A program copies with OSC 52 (`ESC ] 52 ; c ; base64 BEL`), as Neovim and
tmux can, and rtty puts the text on the clipboard. A program's read (`ESC ] 52 ; c ; ?
BEL`) would hand it whatever was copied last, a password included, so it is answered only as
`clipboard_read` says: `"ask"` (the default) asks in a sheet on the window, "*program* wants to
read the clipboard", and answers only on `Allow`, which takes a click: `Deny` is the default, on
Return and on Esc, so a key pressed by habit hands nothing over; `"allow"` answers at once;
`"deny"` never answers, as before. The answer is the clipboard's text in base64, for the selection the program
named, ended as its query was (BEL or ST).

**Bell and notifications.** BEL flashes its pane (`bell = "none"` turns that off) and, while the
window is not the key window, bounces rtty's dock icon once. OSC 9, OSC 777;notify and kitty's OSC
99 notifications become macOS notifications, except from the pane that has the keyboard in the key
window, and so does the end of a long command out of view ([Shell
integration](#shell-integration)); clicking one brings rtty forward at its pane. macOS posts
notifications only for an app with a bundle identifier: unbundled (`cargo run`),
`UNUserNotificationCenter` aborts the process and the old `NSUserNotificationCenter` drops what it
is given, so rtty posts nothing and says so once on stderr. An ad-hoc signed `rtty.app` is
enough: `just install` puts one in `/Applications`, and `just bundle` builds one in `target/` (see
[Install](#build-and-run)); run `target/rtty.app/Contents/MacOS/rtty` with arguments to try a
build there. The bundle's first notification asks for permission (System Settings › Notifications
› rtty), which macOS ties to its identifier, `local.rtty`.

The terminal gets printable text (through the platform input handler, so dead keys and IME
composition go through macOS), Enter, Backspace, Tab, Shift+Tab, Esc, the arrows,
Home/End/PgUp/PgDn/Insert/Delete, F1–F12 (xterm encodings, with modifier parameters), Ctrl+letters
and Ctrl+`[ \ ] ^ _ @ space`, and Option as Alt: an ESC prefix (so Option+B is word-back in a shell,
and Option no longer types `∫`). Application cursor mode (DECCKM) switches the arrows to `ESC O`.
A held key repeats instead of opening macOS's accent picker.

**Option on one side.** `option_as_alt` names the Option keys that are Alt: `"both"` (the
default), `"left"`, `"right"` or `"none"`. The other Option types what macOS gives it, as in any
Mac text field: `˙` for Option+H, `€` for Option+Shift+2 on a US layout, and é for Option+E then E
(a dead key); no `alt-` binding fires for those keys and no ESC goes with them. With a key that
types nothing (an arrow, Enter, Backspace), or with Ctrl or Cmd held, it stays Alt (`CSI 1 ; 3 D`,
ESC DEL). Which Option is down comes from the key event's device-dependent modifier flags, which
the keyboard sets; an event without them is taken as Alt. Such a key goes straight from AppKit to
the window's text input, the way GPUI hands it composing keys, and never reaches GPUI's bindings
or rtty's key encoder.

Programs that ask for more get it (rtty-vt's encoder, the same one tutti's TUI uses):

- **xterm's modifyOtherKeys** (`CSI > 4 ; n m`, which Emacs turns on at level 1). Level 1 sends
  `CSI 27 ; mod ; code ~` for Ctrl with keys that have no control character (`C-;`, `C-.`,
  `C-1`) and for Ctrl or Shift with Enter and Tab; every control character stays as it was. Level
  2 sends it for every key with Ctrl or Alt, and Enter, Tab, Backspace and Esc with any modifier.
  Without it, Ctrl leaves a key it has no control character for as it is (`C-1` types 1).
- **The kitty keyboard protocol** (`CSI > flags u`; Neovim pushes flags 3). Disambiguate: Esc is
  `CSI 27 u`, keys with Ctrl or Alt are `CSI code ; mod u` (so `C-i` is not Tab and `S-Enter` is
  not Enter), and the other keys use the `CSI 1 ; mod X` and `CSI n ; mod ~` forms, never `SS3`
  (F3 is `CSI 13 ~`); plain text and unmodified Enter, Tab and Backspace stay as they were. Event
  types add repeats (`:2`) and releases (`:3`) of those keys. All keys as escape codes, alternate
  (shifted) keys and associated text are encoded too, though no program checked here uses them.
  The wheel's arrow keys on the alternate screen are the arrow keys as the program asked for them.

GPUI does not report keypad keys apart from the main keys, so application keypad mode (DECKPAM)
has nothing to change, and a shifted symbol arrives as itself (`!`, not Shift+`1`).

**What a command leaves on.** A program killed with SIGKILL turns off none of the reports and key
encodings it turned on, and the shell after it then gets a click, a focus change or a flip to dark
as typing, and its keys in kitty's encoding. When the shell marks the command's end (OSC 133;D,
the first of a run of them), rtty turns off the mouse reports (every mode and encoding), focus
reports (1004), light/dark reports (2031), modifyOtherKeys and kitty's flags, right there in the
stream, so what the shell sets for its next prompt stays. Bracketed paste and the cursor and keypad
modes stay too: shells set those themselves. tutti does the same.

## Shell integration

zsh, bash and fish in rtty mark where each prompt starts, where its command line starts, where a
command's output starts and where the command ends, with its status (OSC 133 A, B, C and D), and
report their directory at each prompt (OSC 7), with no edit to your dotfiles.
`shell_integration = "none"` leaves the shell as it is; a prompt that sends the marks itself
(oh-my-posh can) still gets everything below.

**How it loads.** The scripts (`shell/`) are built into rtty, which writes them at startup into its
private directory, `$TMPDIR/rtty-<uid>/shell/`, so `cargo run` and the `.app` behave alike. A new
pane's login shell loads them; a `-e` program and any other shell are left alone.

- **zsh** starts with `ZDOTDIR` at rtty's directory, whose `.zshenv` puts your `ZDOTDIR` back (or
  unsets it) and reads your `.zshenv`; zsh goes on to read your `.zprofile`, `.zshrc` and `.zlogin`
  as always. rtty's prompt hook moves itself last among `precmd_functions`, after a theme's that
  rebuilds `PS1`, so from the second prompt on its marks survive the theme. The end goes into
  `PROMPT_EOL_MARK`, which zsh draws right where the output ended (from the hook only with
  `PROMPT_SP` off).
- **bash** starts as `bash --rcfile <rtty's script>`, since a login shell reads no rc file it is
  given: an interactive shell that is not a login one (`shopt login_shell` is off, `logout` refuses,
  `~/.bash_logout` is not read), so the script first reads what a login shell would: `/etc/profile`,
  then the first of `~/.bash_profile`, `~/.bash_login` and `~/.profile`. Bash 4.4 marks the output
  from `PS0`. macOS's `/bin/bash` 3.2 has no `PS0` and marks it from a `DEBUG` trap, unless you have
  one of your own, and not for a command line that is only a subshell (`(make)`), which the trap
  does not see.
- **fish** finds rtty's `vendor_conf.d` through a directory put first in `XDG_DATA_DIRS`, which the
  script puts back. It wraps `fish_prompt` at the first prompt for A and B and marks C and D from
  `fish_preexec` and `fish_postexec`. It needs fish 3.1 or newer (`string collect`). Not yet tried
  with a real fish. A directory whose name holds a newline is split at it by fish's command
  substitution, so the OSC 7 sent for it names the wrong directory.

Each script undoes its injection as it starts, so nothing of it reaches what the shell runs:
another zsh or bash, or the shells tutti starts for its own panes, which are tutti's business.

**Marked twice.** A prompt that marks itself as well (oh-my-posh, another terminal's integration
copied into a dotfile) puts a second mark of a kind right after rtty's, on the same row. rtty's
scripts add A and B to `PS1` only when it has none, and rtty counts a run of one kind of mark on one
row as its first, so a prompt jump stops once per prompt, the last output ends at its first end, a
long command notifies once, and what a command left on goes off once (above). Skipping rtty's
integration when the user has one was the other way, but a prompt's marks show only once it draws,
after the shell has started.

**What the marks give.**

- `Cmd+Up` and `Cmd+Down` jump between prompts ([Keys](#keys)).
- `Cmd+Shift+A` (`copy-last-output`) copies the output of the last command that finished, from its
  output mark to its end (or the next prompt, from a shell that marks no end), scrollback included.
  A label at the pane's bottom right says for a moment that it copied, that the command printed
  nothing, or that there is no output to copy.
- A command that ran `notify_after` seconds or more (10 by default; 0 for never) and ends in a pane
  that is not the focused pane of the key window posts a notification: its command line, as typed
  after the prompt, then its exit status, how long it ran and the pane's title. Like a program's own
  notification it needs the app bundle, and clicking it brings rtty forward at that pane. A mark a
  program sends while the alternate screen is up is not the shell's, and neither starts nor ends a
  command. rtty tells one apart by looking for the event's row, column and kind among the marks the
  primary screen holds, so a program on the alternate screen that sends a mark exactly where the
  shell left one of that kind passes for the shell. Telling for sure needs rtty-vt's mark event to
  say whether it was stored, which waits because tutti reads that event too.
- A plain click on the command line being typed (after the last prompt, before a command runs), in
  the pane that already has the keyboard and whose program takes no mouse reports, moves the shell's
  cursor there with arrow keys, as Ghostty's `cursor-click-to-move` does (`click_to_move = false`
  turns it off). An arrow moves a character, so a wide character is one, clicked on either half; the
  line runs on across soft wraps; a click past what is typed goes to its end, and one on the prompt,
  off the line, or ending a drag moves nothing. A right prompt (zsh's `RPROMPT`) on the line counts
  as typed, so a click between the two sends more arrows than the shell can take. Spaces typed at
  the end and moved back over look like blank cells, so a click into them stops at the last
  character that is not a space. It takes a shell
  that marks a command's output, and once it has marked one, so not before a new pane's first
  command: without that mark a prompt's end is the last mark while a command runs too (bash 3.2
  with a DEBUG trap of your own), and a click would type arrows into the command.

## Images

Programs show images with kitty's graphics protocol: rtty-vt reads the commands (the subset is in
its [README](vt/README.md)) and keeps the images and where they are placed, and each frame draws
them with GPUI's images, clipped to the pane: those with a z under −2³⁰ beneath the cells'
backgrounds, other negative ones beneath the text, the rest over it. An image is anchored to the
cell its top left corner is on, so it scrolls with the text into the scrollback and back, and
moves with that cell when a resize reflows the screen. It keeps its size in cells, as the cell
size was when it was placed (`line_height` included): a later font change scales it, and a later
`line_height` change stretches it with its rows. `clear` (ED 2), the alternate screen's clearing
and a reset take the images on the screen; erasing text leaves them. Unicode placeholders work
too: cells of U+10EEEE draw no glyph but their part of the image, so images survive tmux and
tutti, which pass the placeholder text along. Each image is decoded once, by rtty-vt, held as one
GPUI image until rtty-vt evicts it (256 MiB of images per pane, the oldest first), and its pixels
go to the GPU once.

Clients size images by the cell size in pixels, which rtty reports in the display's device
pixels, as Ghostty and WezTerm do: the pty's `ws_xpixel` and `ws_ypixel`, `CSI 14 t` and `16 t`
(on a Retina display a cell of 8×17 points is 16×34), and SGR-pixel mouse reports (mode 1016),
which count a pointer's offset in those same pixels. So an image shown at its own size covers its
pixels one to one. The scale is the window's, as each layout finds it. `graphics = false` drops
every graphics command, as a terminal without the protocol does.

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
(`ESC ] 1337 ; SetUserVar=RTTY_KEYS=cGFuZQ== BEL`, the value base64) gets every chord bound to
`focus-…`, `resize-…`, `close-pane` or `last-pane` instead of rtty acting on it: the things an
editor does with its own windows. By default those are `Cmd+Opt+arrows`, `Cmd+Ctrl+arrows` and
`Cmd+W`, and with `keys = "tutti"`, `Ctrl+h/j/k/l` and `Alt+h/j/k/l`; `[bind]` adds its own, so
with `"alt-x" = "close-pane"` and `"ctrl-\\" = "last-pane"` Neovim's `Alt+x` closes a Neovim
window and its `Ctrl+\` goes to Neovim's previous one. Every other chord stays rtty's in a claiming
pane: splits, zoom, swaps, tabs and the rest. The claim ends when the var is set empty, when a
shell in the pane draws its next prompt (OSC 133;A), and with the program.

A claimed chord is encoded as the program asked for keys, Cmd as the kitty protocol's super
modifier: Neovim (kitty flags 3) reads `Cmd+Opt+Left` as `<D-M-Left>` and `Cmd+Ctrl+Left` as
`<D-C-Left>`. No xterm encoding says Cmd in a way Emacs decodes (it reads `CSI 1 ; 11 D` as an
undefined `M-[ 1 ; 1`), and the kitty protocol, short of every key as an escape code (flag 8, which
Neovim does not ask for), has no form for Cmd with a key that types text. So a Cmd chord reaches
only a program speaking the kitty protocol, and `Cmd+W` reaches none: rtty keeps acting on such a
chord, claim or not, so `Cmd+W` closes the pane even with Neovim in it, and Emacs moves with the
tutti-style keys instead.

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
  and text runs with their resolved colors and decorations, the characters rtty draws itself, and
  the cursor. A program's colors (OSC 4/10/11/12) win over the scheme's; DECSCNM swaps the
  defaults. A Unicode placeholder's cell draws no glyph: its image does.
- `glyphs.rs` — box drawing, block elements and the powerline separators as rectangles and paths
  on a cell's device pixels, from the Unicode charts; unit tested.
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
  frame's shaped runs (see [Ligatures](#ligatures)). A synchronized update (mode 2026) keeps the
  last frame up until it ends, or for 150 ms. Images (`Terminal::placements`) are read under the
  same lock and drawn by z beneath or over the text, each engine image one GPUI image kept by its
  generation until the engine lets it go ([Images](#images)). The view is also GPUI's text input
  handler, for committed text and IME marked text.
- `layout.rs` — a window's tabs, each a tree of splits with ratios, in cells, with no GPUI in it
  and unit tested: where each pane and divider goes in an area (a divider is one cell; no pane
  gets fewer than two across a split), split and close with the cascade to tab and window, a
  closed pane or tab put back where it was, a divider dragged to a cell or moved by a resize, focus by direction and back to the last pane, a
  swap with the pane that way, zoom.
- `session.rs` — the windows' layout as the next launch restores it (the layout's own split trees,
  serialized), read back checked and written whole; unit tested.
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
  views alive and a closed window ordered out, until `undo-close` or its timer.
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

## Measurements (Apple silicon Mac, release build)

`rtty --bench` (80×24 grid, 10k rows of scrollback; one run each, repeat runs vary by ~10%):

| Workload | into a pipe | parse only (memory) | pty + reader + rtty-vt |
| --- | --- | --- | --- |
| `seq -f %.0f 1 2000000` (16.9 MB) | 182 ms | 81 ms (208 MB/s) | 1998 ms (8.5 MB/s) |
| `seq -f %.0f 1 2000000 \| cat` | 194 ms | 87 ms (195 MB/s) | 198 ms (85 MB/s) |
| 20k lines, a color/bold/italic change every word (32.7 MB) | 662 ms | 100 ms (327 MB/s) | 932 ms (35 MB/s) |
| the same 20k lines from a file (`cat`) | 14 ms | 97 ms (337 MB/s) | 187 ms (175 MB/s) |

The pipeline keeps up with the producer. Plain `seq` on a tty is slow because stdio line-buffers
a tty: two million one-line `write(2)`s into the tty layer (`script -q /dev/null seq …` takes
2.8 s with no terminal at all). Through `cat`, the same bytes take ~200 ms end to end. The parser
alone runs at 200–340 MB/s, well above what a pty delivers. On alacritty_terminal (rtty before
rtty-vt) the same parse took 99–105 ms and `cat` of the file 207 ms.

Memory (`cargo test --release --test memory -- --ignored --nocapture` in `vt/`, heap bytes asked
for, 24 rows and 10k rows of scrollback): `seq`'s lines filling the scrollback hold 2.1 MB at 80
or 200 columns; full-width rows held 14 and 34 MB, until rows began to shrink to their text as
they scroll into the scrollback. Empty lines hold 1.4 MB (the rows themselves, 140 bytes each);
dense full-width colour, every cell written, still 14 and 34 MB. A terminal at a prompt with no
scrollback holds 36 KB at 80 columns and 82 KB at 200 until a program first switches to the
alternate screen, whose rows come then (69 and 162 KB with both screens). Shrinking
costs one small allocation per row scrolled off, ~6 ns: `seq`'s parse went from ~70 ms to ~81 ms
(`cargo bench` in `vt/`: 245 to 207 MB/s, alacritty_terminal 200–210); the dense and Unicode
parses are 2–5% slower, inside the noise of repeat runs, and the pipelines unchanged.

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
font's `calt`, `liga` and `dlig` features; `font_features` turns off (or on) any one of them, over
`ligatures` ([Fonts](#configuration)). rtty's own part below (runs shaped whole, glyphs snapped to
their cells, the cursor's cell alone) is the same whatever the features: it only lets the font's
ligatures form, and with `calt` and `liga` off the font forms none.

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
selection (scrolling the scrollback when dragged past the pane) and copy/paste, scrollback,
resize with reflow, mouse reports, a Ghostty-style tab bar in the titlebar, window and tab titles
from OSC 0/2, the foreground process or the shell's directory, OSC 52 copy and, with permission,
read, synchronized updates, focus reports (mode 1004: `CSI I` and `CSI O` as the window is activated and deactivated and as
the pane gains and loses the keyboard), the keyboard protocols above, scrollback search, prompt
jumps, links (URLs, and `path:line:col` opened in an editor pane or a neighbouring Neovim), quick
select and quick open, the bell, notifications (from an app bundle), a program's progress on its
tab's pill
and the dock's badge, a question before closing what runs a program and `Cmd+Z` to undo a close,
`Cmd+K` to clear, a config file with key bindings, a menu bar showing them,
WezTerm-format color schemes that follow the macOS appearance live in every pane (with DEC mode
2031 reports), `rtty cli` and the editor plugins, the log, shell integration that loads
itself (prompt jumps, the last command's output, notifications of long commands, click to move the
cursor), and images (kitty's graphics protocol, Unicode placeholders included; checked with
`--script` and a program sending RGB, RGBA, PNG, chunked, compressed and temporary-file images,
placements under and over text, a delete and placeholders, through a reflow and `clear`, by the
dump's `image` lines, not yet by eye). Checked with `--script` against Neovim in two
side-by-side panes (each redraws at its own size through divider moves and window resizes;
clicks reach the pane under them at its own cells; a light/dark flip reaches both) and Emacs
`-nw` in a stacked split; and with the editor plugins, Neovim and Emacs side by side: one key
walks through Neovim's windows on to Emacs and back. tutti's TUI in an rtty pane claims rtty's
keys and hands a move or resize past its own edge on to `rtty cli`, so the same keys walk
through Neovim's windows, tutti's panes and rtty's (tutti's README, "tutti in rtty"). The shell
integration is checked with `--script` against zsh 5.9, macOS's bash 3.2 and Homebrew's bash 5.3,
each with a scratch home and no integration of its own, and against zsh with a prompt that marks
itself too: the marks arrive, prompt jumps go from prompt to prompt, `Cmd+Shift+A` copies the
last output out of the scrollback, a long command ending in a pane without the keyboard is
notified, and a click moves the cursor across wide characters and a soft wrap. fish is not
installed here, so its script is unchecked. Paths and quick select are checked with `--script` in
a scratch home: a printed `src/main.rs:3:5` is underlined under `Cmd` and a click runs the `editor`
template with its line and column, or with no template the `$EDITOR` a `.zshrc` exports; a relative
path is found from a hand-printed OSC 7 while the shell sits elsewhere; a Neovim with the plugin in
the pane beside opens the file at its line and column and takes the keyboard; quick select labels,
copies, pastes with Shift (through AppKit's key path too) and cancels on Esc or any other key, and
quick open follows a URL and a path.

The cursor takes the shape a program sets with DECSCUSR (block, bar or underline; Neovim's
insert mode is a bar), and a hollow block while the window or pane is not focused. It blinks when
DECSCUSR asks for blinking, or, while DECSCUSR is at its default, when ATT610 (`CSI ? 12 h`) does;
it stops blinking and stays on after 10 s without a key, so an idle window draws nothing.

Not yet: IME is wired through GPUI's input handler
(composition is drawn at the cursor and the candidate window is placed there) but unverified
with a real input method; emoji and CJK rely on CoreText's font fallback (an emoji wider than its
two cells is drawn smaller to fit them); the drawn box, block and powerline characters and a
selection dragged past a pane are checked by the `--script` dump, not yet by eye on screen;
notifications need the `.app` above, and posting from it and clicking one are not yet checked on
screen. Closing's question is checked through the `--script` dump, where a scripted window keeps
it, and its `answer` step, and the window's close button through its `window close` step, which
runs what a click runs; the native sheet itself, its buttons and Esc, the click on the close
button as AppKit sends it, the dock's badge and the progress line's look are not yet checked by
eye. Quitting as macOS asks is checked with `--script`'s `terminate` step, which sends AppKit's
`terminate:` from the run loop, in process: the question, `Cancel` keeping rtty, `Quit` quitting
it, nothing asked over an idle shell, and the asking window closing first, after which the
question comes again on the window left and its `Quit` still ends AppKit's wait; the dock's Quit
and a logout themselves are not yet checked. The menu bar's chords are checked in the `--script` dump, which lists the menus as
AppKit has them, and its items by `--script`'s `menu` step, which dispatches an item's action to
the scripted window (a click dispatches it to the key window, which a scripted window never is);
the menus' look and a click on an item are not yet checked by eye. Secure Keyboard Entry is checked through the dump,
with `--script`'s `app` step standing in for macOS making rtty active and not: the enable and
disable calls rtty made, one each way, macOS's own word (`IsSecureEventInputEnabled`), a
password prompt holding it, and none held after quitting; the lock and the menu's check mark
are not yet checked by eye. The global hotkey is registered with macOS in a scripted run too, and
`--script`'s `hotkey` step runs what a press runs, which a scripted rtty keeps in its dump
instead of doing; a real press is not yet checked. A clipboard read is checked with `--script`
and `--trace`: no answer goes to the pty until `answer ok`, none after `answer cancel`, and with
`clipboard_read = "allow"` one at once; the sheet and its keys (Return and Esc denying) are not
yet checked by eye.

## The GPUI pin

`gpui = "=0.2.2"` from crates.io, with the `runtime_shaders` feature. 0.2.2 is the newest
release on crates.io and builds on this Mac with the stable toolchain, which a git rev of Zed
would not guarantee (Zed tracks its own `rust-toolchain`, and a rev drags in its whole workspace).
`runtime_shaders` compiles GPUI's Metal shaders when the app starts instead of at build time:
Xcode 26 ships the offline `metal` compiler as a separate download
(`xcodebuild -downloadComponent MetalToolchain`), so without the feature the build fails on a
machine that has not fetched it. The cost is a shader compile at startup.

Licensing: GPUI and rtty-vt are Apache-2.0, portable-pty is MIT, global-hotkey is Apache-2.0 or
MIT; the image crates (`png` and `miniz_oxide` in rtty-vt, `image` for GPUI's frames) are MIT or
Apache-2.0. rtty takes nothing from Zed's GPL `terminal` / `terminal_view` crates, and nothing
from kitty's GPL source: the graphics protocol is implemented from its documentation.

## Roadmap

The one item this held, tutti knowing rtty, has shipped (Status, above). What is left:

1. **A look on screen** at what scripts check only in the dump: IME with a real input method,
   a dead key on the Option that is not Alt (é from Option+E, E: the text input inserts the
   characters a synthesized key event carries, and composes nothing from them), a file dragged
   from Finder (`drop` calls the pane's drop handler, past GPUI's drag), the drawn
   box, block and powerline characters, a selection dragged past a pane, notifications from
   the `.app`, posted and clicked, closing's sheet and the window's close button, the
   progress line and the dock's badge, quick select's labels over their matches, the menu bar
   and a click on its items, the dock's Quit and a logout, Secure Keyboard Entry's lock and
   check mark, a real press of the global hotkey, a clipboard read's sheet, kitty images and
   their placeholders, the config reload's banner, and a restored window landing on its own
   display.

Cmd+D and Cmd+F, which once quit rtty and did nothing in a launch where its window was key,
split and search in the installed app since a reinstall (seen on screen; the cause was never
pinned down). rtty.log still records each Cmd chord as AppKit hands it over and as GPUI
dispatches it, should it come back.
