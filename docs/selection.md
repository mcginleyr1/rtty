# Selection, search and links

Selecting a block, searching the scrollback, the links and paths a `Cmd+click` follows, quick
select and quick open, copy mode, and how a program copies to the clipboard and reads it.
Selecting with a plain drag, a double click or a triple click is in the
[mouse table](keys.md#the-mouse).

## Block selection

Option+drag selects a block, the same columns on every row it spans, as in Terminal.app (and
WezTerm and Ghostty; iTerm2 takes Cmd+Option). Copying it gives each row's columns, trailing blanks
trimmed, the rows joined by newlines. A wide character is in a block when its first cell is: one
the block's right edge cuts is drawn and copied whole, one its left edge cuts is left out. While a
program takes the mouse an Option+drag is the program's (Emacs's secondary selection is
`M-drag-mouse-1`), so Shift+Option+drag selects a block there, as Shift+drag selects in a program's
place. [Copy mode](#copy-mode) selects blocks from the keyboard.

## Search

`Cmd+F` opens a bar at the pane's top right, which takes the keyboard until `Esc`. The query
searches every row the pane holds, scrollback and screen, as lines: a match runs on across a soft
wrap. A query without a capital ignores case (smart-case); `Ctrl+R` switches between text and a
regex. Matches show on the scheme's yellow and the current one on its cursor color; `Enter` and
`Shift+Enter` go to the next and previous match, round the ends, scrolling the view to it. Matches
stay on their text while output scrolls it up, and output arriving with the bar open is searched
again.

## Links

A link is an OSC 8 hyperlink, a plain `http`, `https` or `file` URL in the text (trailing
punctuation left out), or the path of a file that exists ([Paths](#paths)). Holding `Cmd`
underlines the one under the pointer and `Cmd+click` follows it:

- `http`, `https` and `mailto` open with the system's handler;
- a `file` link is shown in Finder, selected, and never opened, since opening a file URL launches
  what it names (an app, a `.command` script, an executable) and an OSC 8 link's text can claim
  anything;
- any other scheme is ignored.

Nothing happens without the click. While `Cmd` is held over a link whose text is not its target
(an OSC 8 "click here", or one showing one address and going to another), the pane's bottom edge
shows where it really goes and what a click will do.

## Paths

A path is found as compilers, grep, test runners and stack traces print it, with a line and column
after it or not: `src/view.rs:123:45`, `./a/b.rs:7`, `/abs/x.py:3`, `~/notes.md`, `Foo.java:12`,
Python's `File "x.py", line 3`. It has a slash or is a name with an extension, holds no space,
quote, bracket, colon or backslash, and starts a line or follows a space, a quote, an opening
bracket or `=`, never a colon, so neither a URL's `//host` nor any part of a Windows `C:\x` is one;
a sentence's full stop after it is left out. It is a link only when it exists: a relative path
from the directory the shell last reported (OSC 7, which
[shell integration](shell-integration.md) sends at each prompt), else the foreground process's.
rtty looks only while `Cmd` is held over it and at the click, never per frame. A click on

- **text** (a file with no NUL byte in its first 8 KiB) edits it at the line and column:
  1. in a Neovim running rtty's plugin, in that pane or else in another pane of its tab, which
     then takes the keyboard (the plugin names its server to rtty: [editors](../editors/README.md)).
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

## Quick select and quick open

`Cmd+Shift+Space` (`quick-select`; WezTerm's QuickSelect is the idea, on Ctrl+Shift+Space there, a
Ctrl key and so a program's here) labels everything worth copying on the focused pane's screen:

- URLs;
- paths;
- git hashes (7 to 40 hex digits);
- IPv4 and IPv6 addresses;
- UUIDs;
- numbers of four digits or more;
- what the regexes in `quick_select_patterns` match (a pattern's first group, when it has one).

Of two that overlap, the one starting first is labeled. Each match shows on the search's yellow
under a bold label in the scheme's colors inverted, which reads in a light scheme and a dark one
alike. Labels are home-row letters (`asdfghjkl`), the shortest nearest the cursor, and none is the
start of another: past nine matches, letters stand for two-letter labels.

Typing a label copies its match and says so, as `copy-last-output` does; typing it with Shift
(`A`, or `aS`) pastes it into the pane too, bracketed when the program asked for that. Esc puts the
labels away, as does any other key, which goes nowhere else: no binding, no program.

`Cmd+Shift+O` (`quick-open`) labels only what a `Cmd+click` would follow: URLs and paths to files
that exist (not OSC 8 links); typing a label follows it as the click would have when the labels
appeared, whatever output has moved under them since.

## Copy mode

`Cmd+Shift+C` (`copy-mode`, iTerm2's chord; alacritty's vi mode and WezTerm's copy mode are the
other ideas) puts a cursor on the pane where the terminal's is, or on the bottom row of a view
scrolled back from it, drawn as its cell with the colors swapped in place of the terminal's
cursor; `copy mode` at the pane's bottom right says it is on, with what it selects and a count
being typed. A selection made before goes. Until copy mode ends, a key no binding takes is copy
mode's, never the program's, and a paste or a dropped file goes nowhere; output keeps arriving,
and the cursor and the selection stay on their text as it scrolls up.

| Key | Moves the cursor, or does |
| --- | --- |
| `h` `j` `k` `l`, arrows | a character left, a row down, up, a character right (a wide character's two cells are one) |
| `w` `b` `e` | to the next word's start, this or the last word's start, this or the next word's end: a word is a run of what a double click selects, or of the separators between (`,` `:` `\|` quotes, brackets); a soft wrap runs a word on, a line end is a blank |
| `W` `B` `E` | the same, with words told apart by blanks alone |
| `0` `^` `$` | the row's first column, first character, last character |
| `H` `M` `L` | the view's top, middle, bottom row, at its first character |
| `gg` `G` | the first row held (the oldest of the scrollback), the screen's last |
| `Ctrl+U` `Ctrl+D`, `Ctrl+B` `Ctrl+F` | half a view up, down; a whole view up, down: the view moves as far |
| `[` `]` | the shell's previous, next prompt (OSC 133;A, [Shell integration](shell-integration.md)) |
| `/` `?` | search, in the [search bar](#search); `Enter` puts the cursor on the current match and leaves the bar up, its matches for `n` and `N`; `Esc` closes the bar |
| `n` `N` | the next, previous match after `/` (after `?`, the previous, next), round the ends |
| a count first | `5j`, `3w`: the motion that many times |
| `v` `V` `Ctrl+V` | select characters, lines, a block, from the cursor's cell to wherever it goes; the same key again ends the selection, another changes its kind |
| `o` | the cursor to the selection's other end, which then moves |
| `y` | copy the selection, or with none the cursor's line, say so and leave |
| `Esc`, `q`, `Cmd+Shift+C` | `Esc` ends the selection, else leaves; `q` and `Cmd+Shift+C` leave |

The view stays where copy mode left it until something is typed. A resize ends copy mode, as
it ends a selection: reflow renumbers the rows; so does `Cmd+K`, which clears them. Bindings
still work in copy mode (`Cmd+C` copies its selection); `Ctrl+h/j/k/l` with `keys = "tutti"`
stay the panes'.

## The clipboard

A program copies with OSC 52 (`ESC ] 52 ; c ; base64 BEL`), as Neovim and tmux can, and rtty puts
the text on the clipboard. A program's read (`ESC ] 52 ; c ; ? BEL`) would hand it whatever was
copied last, a password included, so it is answered only as `clipboard_read` says:

- `"ask"` (the default) asks in a sheet on the window, "*program* wants to read the clipboard",
  and answers only on `Allow`, which takes a click: `Deny` is the default, on Return and on Esc,
  so a key pressed by habit hands nothing over;
- `"allow"` answers at once;
- `"deny"` never answers, as before.

The answer is the clipboard's text in base64, for the selection the program named, ended as its
query was (BEL or ST).
