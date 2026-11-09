# Windows, tabs and panes

Where a new pane starts, what closing asks and how a close is undone, the tab bar and what it
shows of a program's progress, titles, the cursor, the bell and notifications, and how windows
come back at the next launch. The keys that open, close, split and move between them are in
[keys.md](keys.md#default-keys).

## New panes, tabs and windows

New panes, tabs and windows run the login shell in the focused pane's working directory: as its
shell last reported it with OSC 7 (`ESC ] 7 ; file://host/path BEL`), else the directory of the
pane's foreground process (the pty's process group leader: the shell at a prompt, or the program
it runs), else your home directory. rtty's [shell integration](shell-integration.md) sends OSC 7 at
each prompt; without it, macOS's zsh and bash send it only from `/etc/zshrc_$TERM_PROGRAM`, which
exists for Terminal.app alone, and the foreground process is what answers. A pane closes when its
program exits, with the same cascade as `Cmd+W`.

## Closing what runs

Closing a pane (`close-pane`, whatever chord it is bound to), a tab (its ×), a window (its close
button) or rtty (`Cmd+Q`, the menu's Quit, the dock's Quit, logging out) asks first, in a sheet on
the window, while anything other than a pane's shell at its prompt holds that pane's foreground
(the pty's process group, as titles find it): an agent, Neovim, a build.

- A pane's question names its program; a tab's, a window's and the quit's count the panes running
  something and name what each runs.
- `Close` (or `Quit`, the default button) goes on and `Cancel` (Esc) keeps everything.
- A pane whose shell sits at its prompt closes at once, as does a pane whose program exits.
- `confirm_close = false` never asks.

Quitting asks on the window that had the keyboard last; should that window close before the
answer (its last program exited), rtty asks again on another, about what still runs, or quits when
nothing does. When macOS quits rtty (the dock's Quit, logging out), it waits for the answer,
through `applicationShouldTerminate:`: `Quit` lets the logout go on, `Cancel` stops it.

Known limits: only the login shell rtty starts itself counts as a shell, so a pane started with
`rtty -e /bin/zsh` asks even at an idle prompt; and a command forked a moment before the close may
not hold the foreground yet, and then is not seen as running.

## Undoing a close

A closed pane, tab or window does not go at once: it waits, hidden, its programs running on, for
`undo_timeout` seconds (5 by default), and `Cmd+Z` (`undo-close`) brings it back as it was:

- a pane into its split, at its side and share of the cells (beside the pane it sat by, when the
  splits around it changed; in a tab of its own, when those went too);
- a tab at its place in the row;
- a window on screen again with its tabs.

Only the last close waits: a close ends the one before it, as does its time running out, and then
its programs are hung up on as a close always did. A closed pane whose program exits meanwhile
leaves the close, and the rest of it still comes back (the other panes of a closed tab or window);
a close with nothing left ends. Closing the last window shown is for good, and so is quitting;
`undo_timeout = 0` makes every close final.

## The tab bar

rtty draws its own titlebar, a bar 36 points tall across the window's top with the traffic lights
at its left, the way Ghostty looks on macOS.

- With one tab it shows the tab's title, centered, after a folder while the pane's directory is
  known, dimmed while the window is in the background.
- With more, it holds a rounded pill per tab, equal widths filling the row after the traffic
  lights: the shown tab's pill is lighter, the others recede with dimmed titles; each title is
  centered, each of the first nine pills shows its `Cmd+1`–`9` at its right end, and the pill under
  the pointer shows a close × at its left end. A round `+` ends the row.

The bar keeps its height whatever the number of tabs, so opening or closing a tab resizes no pane;
in full screen, where macOS hides the titlebar, a lone tab has no bar and a row of tabs starts at
the window's edge. Its colors are the scheme's ([Color schemes](color-schemes.md#the-format)), so
it follows the light/dark flip.

## Progress

A program that reports its progress, with ConEmu's OSC 9;4 (`ESC ] 9 ; 4 ; state ; percent ST`, as
Claude Code does with its progress setting on, and Ghostty and Windows Terminal read it) or tutti's
OSC 7501 program status (`state=working:progress=40`), shows it as a thin line along the bottom of
its tab's pill, between its rounded ends (with one tab, along the bar's bottom edge):

- the scheme's blue, as far along as the percent;
- its yellow while paused (or, for OSC 7501, `blocked`);
- its red on an error;
- the whole line in a faint blue while the program cannot say how far.

A tab with several reporting panes shows an error first, else the least done. The line goes when
the program removes it (`9;4;0`, or `done`, `idle` or `clear`) or a shell draws its next prompt
(OSC 133;A), so a build killed mid-report leaves nothing behind.

rtty's dock icon carries a badge that sums up every pane's progress in every window: `!` while
one reports an error, else the percent of the least done, else `…` while a program cannot say
how far; it goes once every program is done. It shows whether rtty is in front or not: a pill
shows only its own tab, and the badge is the one place an error in a tab or window out of sight
shows. (Checked through `--script`'s dump and the log's `dock badge` lines, not yet by eye.)

## Titles

A pane is called by the title its program set (OSC 0 or 2); else, while a program runs in its
foreground (the pty's process group, `tcgetpgrp`), by that program's command line (`cargo run
--release`); else, with the shell at its prompt, by the shell's directory (OSC 7 when the shell
reports it, else the shell's own working directory), home written `~`. A pane looks at its
foreground process and directory once a second, and at once when its program sets a title or its
shell reports a directory. A tab is titled by its focused pane, the window by its shown tab, and
`rtty cli list` reports both. The focused pane draws the focused cursor and the others a hollow
one; focus reports (mode 1004) go to the pane gaining and the pane losing the keyboard.

## The cursor

The cursor takes the shape a program sets with DECSCUSR (block, bar or underline; Neovim's
insert mode is a bar), and a hollow block while the window or pane is not focused. It blinks when
DECSCUSR asks for blinking, or, while DECSCUSR is at its default, when ATT610 (`CSI ? 12 h`) does;
it stops blinking and stays on after 10 s without a key, so an idle window draws nothing.

## Bell and notifications

BEL flashes its pane (`bell = "none"` turns that off) and, while the window is not the key window,
bounces rtty's dock icon once. OSC 9, OSC 777;notify and kitty's OSC 99 notifications become macOS
notifications, except from the pane that has the keyboard in the key window, and so does the end
of a long command out of view ([Shell integration](shell-integration.md#what-the-marks-give));
clicking one brings rtty forward at its pane.

macOS posts notifications only for an app with a bundle identifier: unbundled (`cargo run`),
`UNUserNotificationCenter` aborts the process and the old `NSUserNotificationCenter` drops what it
is given, so rtty posts nothing and says so once on stderr. An ad-hoc signed `rtty.app` is enough:
`just install` puts one in `/Applications`, and `just bundle` builds one in `target/` (see
[Installing the app](install.md#installing-the-app)); run `target/rtty.app/Contents/MacOS/rtty`
with arguments to try a build there. The bundle's first notification asks for permission (System
Settings › Notifications › rtty), which macOS ties to its identifier, `local.rtty`.

## Restoring windows

With `restore_layout = true`, the default (Terminal.app and iTerm2 restore their windows by
default too), rtty keeps each window's layout for the next launch: its frame and display, its
tabs, each tab's splits with their ratios, zoom and focus, and each pane's working directory (as
its shell last reported it with OSC 7, else its foreground process's). It saves as it quits, and
once a second when any of that changed since it last wrote it; a write that fails is tried again
each second, its error logged once.

At launch it opens those windows, each pane a new login shell in its directory (the home
directory, when that is gone). It never starts the programs that ran there and never keeps what a
pane showed.

[tutti panes](tutti.md#tutti-panes) are the exception, since tutti's daemon kept them running: a
pane asked to show one asks again, by the session's socket and the pane's id (whether it showed it
or its daemon was down), and one tutti no longer has says so (`[tutti pane 2: no pane 2]`) rather
than becoming a shell; a window that was a tutti session, answered or not, is that session again,
as its daemon has it now.

A missing or corrupt file starts fresh, and [the log](scripting.md#the-log) says why: corrupt is
also a pane in two tabs, a focus on no pane, a ratio past 1, or a frame under 100 points across or
down. Closing the last window quits with no window to restore, so the next launch opens one fresh
window.

One rtty at a time keeps the layout: the first to lock `layout.lock` beside the file, which it
holds until it exits. Another started meanwhile (`cargo run` beside the installed app) neither
restores nor saves, so the first one's layout stays its own. A launch with `-e` or `--script`
(short of `--restore`) never tries for it.

The file is `$XDG_STATE_HOME/rtty/layout.json` (an empty or relative `XDG_STATE_HOME` is ignored),
else `~/.local/state/rtty/layout.json`, beside the config's `~/.config` as XDG has it, since rtty's
directory in `$TMPDIR` holds what may go with a restart. It is readable by its owner alone (0600)
and written whole, through a rename: JSON, a list of windows, each with:

- its `frame` (points from its display's top left);
- `display` (the display's UUID; on another display, or none, or with the middle of its titlebar
  off that display, the window is centered on the main one);
- `tabs` (each tab's split tree, by pane id, and which shows);
- `cwds` (each pane's directory).
