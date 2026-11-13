# Configuration

The config file: where it is, how to open it, every key with an example value and the file that
explains it, the syntax of key bindings, and which keys a reload changes at once.

## The file

`~/.config/rtty/config.toml` (`$XDG_CONFIG_HOME/rtty/config.toml` when that is set). Every key is
optional; an unknown key, an unknown scheme name or a malformed file is an error that names it,
and rtty does not start. Once rtty runs, a saved change takes effect by itself
([Reloading](#reloading)). `--config FILE` reads another file, and `--font`, `--size` and
`--ligatures` override the file's keys ([Flags](cli.md#flags)).

## Opening the file

`Cmd+,` (`open-config`) opens the file in a new tab. With no file there it first writes
[`config.template.toml`](../config.template.toml), every key commented out at its default. The tab
runs your login shell as an interactive login shell, so that its dotfiles set `EDITOR` (an app
started from the Dock or Finder has no `EDITOR` of its own), and the shell runs `$EDITOR` on the
file, or `open -t` (TextEdit, or your default text editor) when `EDITOR` is empty; the tab closes
with the editor. The words it runs are the same in zsh, bash and fish (fish untried).

## Every key

```toml
font = "FiraCode Nerd Font"
fallback = ["Symbols Nerd Font Mono"]  # for what `font` lacks, in order
size = 12
ligatures = true
font_features = ["ss01", "zero"]  # OpenType features, over `ligatures`'
line_height = 1.2                 # the cell's height as a share of the font's; 1 by default
scheme_dark = "OneDark Pro"       # the default
scheme_light = "Solarized Light"  # the default
# scheme = "Solarized Dark"       # one scheme whatever the OS appearance; not with the two above
keys = "rtty"                     # the default; "tutti" adds Ctrl+h/j/k/l and Alt+h/j/k/l
bell = "visual"                   # the default: BEL flashes the pane (and the dock, unfocused); or "none"
option_as_alt = "both"            # the default; "left", "right" or "none": the Option keys that are Alt
minimum_contrast = 1              # the default, off; 4.5 keeps text legible
confirm_close = true              # the default: ask before closing what runs a program
undo_timeout = 5                  # the default: seconds a close waits for Cmd+Z; 0: none
tabs = true                       # the default; false: no tabs and no bar, a new tab is a window
shell_integration = "auto"        # the default: zsh, bash and fish load rtty's; or "none"
notify_after = 10                 # the default: notify a command of 10 s or more that ends out of view; 0: never
click_to_move = true              # the default: a click on the command line moves the shell's cursor there
editor = ["nvim", "+{line}", "{path}"]  # edits the files Cmd+click opens; unset: $EDITOR
quick_select_patterns = ['[A-Z]+-\d+']  # regexes quick select labels too
restore_layout = true             # the default: the windows come back at the next launch
graphics = true                   # the default: programs show images; false drops their commands
secure_input = false              # the default: Secure Keyboard Entry starts off
auto_secure_input = true          # the default: Secure Keyboard Entry at a password prompt
global_hotkey = "ctrl-`"          # a chord that shows and hides rtty from any app; unset by default
clipboard_read = "ask"            # the default: a program's OSC 52 read asks first; or "allow", "deny"

[bind]                            # chords and their actions, over the defaults
"alt-\\" = "split-right"
"cmd-shift-d" = "none"            # unbinds a default
```

Where each key is explained:

| Keys | Explained in |
| --- | --- |
| `font`, `fallback`, `size`, `ligatures`, `font_features`, `line_height` | [Fonts and ligatures](fonts.md) |
| `scheme`, `scheme_dark`, `scheme_light`, `minimum_contrast` | [Color schemes](color-schemes.md) |
| `keys`, `[bind]` | [Key bindings](#key-bindings) below, [Keys](keys.md) |
| `option_as_alt` | [Option on one side](keys.md#option-on-one-side) |
| `secure_input`, `auto_secure_input` | [Secure Keyboard Entry](keys.md#secure-keyboard-entry) |
| `global_hotkey` | [The global hotkey](keys.md#the-global-hotkey) |
| `bell` | [Bell and notifications](windows.md#bell-and-notifications) |
| `confirm_close` | [Closing what runs](windows.md#closing-what-runs) |
| `undo_timeout` | [Undoing a close](windows.md#undoing-a-close) |
| `tabs` | [Without tabs](windows.md#without-tabs) |
| `restore_layout` | [Restoring windows](windows.md#restoring-windows) |
| `shell_integration`, `notify_after`, `click_to_move` | [Shell integration](shell-integration.md) |
| `editor` | [Paths](selection.md#paths) |
| `quick_select_patterns` | [Quick select and quick open](selection.md#quick-select-and-quick-open) |
| `clipboard_read` | [The clipboard](selection.md#the-clipboard) |
| `graphics` | [Images](images.md) |

## Key bindings

`[bind]` maps chords to the actions named in [Keys](keys.md#default-keys). A chord there is
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

### A Neovim-like layout

A Neovim-like layout: the window keys smart-splits.nvim suggests, with LazyVim's split and zoom
keys (`<leader>|`, `<leader>-`, `<leader>wm`) moved onto Option, since a terminal has no leader.
In a pane where Neovim [claims the keys](keys.md#keys-a-pane-can-claim), focus, resize, `Alt+x` and
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

## Reloading

rtty looks at the config file, and at the scheme files it names (not the built-in ones), once a
second, and reads the config again when one has changed since it was last read; `Cmd+Shift+,`
(`reload-config`, Ghostty's chord) reads it at once. A file that is gone is the defaults, as at
launch, unless `--config` named it: that is a mistake at a reload as at launch. Every window says
what came of it along its bottom for a few seconds, and [the log](scripting.md#the-log) says it
too. A mistake, which at launch would stop rtty, keeps the config in effect and says what and
where it is (`config not reloaded, the last one stays: …config.toml: line 6: "cmd-d": unknown
action "split-sideways"`). `--font`, `--size` and `--ligatures` stay over the file's keys. What
changes, and when:

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
| `tabs` | at once: the bar and the window buttons go or come back; tabs already open stay, reached by their keys |
| `notify_after` | for every command that ends from then on, running ones included |
| `shell_integration` | new panes only: a shell that is running has loaded rtty's scripts, or not, as it started |
| `restore_layout` | saving the layout stops or starts at once; restoring it is at launch |
| `graphics` | new panes only: a pane's engine takes graphics commands, or not, from its first byte |
| `secure_input` | when it changed: Secure Keyboard Entry's toggle (the App menu's) is set to it at once, and secure input is held or let go to match; a reload that leaves it as it was keeps the menu's choice |
| `auto_secure_input` | at once: secure input is held or let go for the pane with the keyboard as it is now |
| `global_hotkey` | at once: the old chord is given back to the other apps and the new one taken |
| `clipboard_read` | from the next read a program asks for |
