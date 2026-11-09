# tutti

How rtty pairs with tutti, the multiplexer for AI coding agents: which of the two front ends for
a tutti session is the primary one, tutti's TUI running in an rtty pane, and rtty showing the panes
of a tutti session as panes of its own, one pane at a time or a whole session in a window.

## rtty and tutti

rtty is one half of a tmux replacement. The other half is tutti, the multiplexer for AI coding
agents: a TUI you run in any terminal, whose daemon keeps sessions alive across detaches.

- **rtty** is the terminal. Like WezTerm, it has its own tabs, windows and splits.
- **tutti** stays an app. Inside rtty it finds rtty and uses what rtty offers.
- **rtty is also a tutti client.** It shows a pane of a tutti session as a pane of its own:
  tutti's daemon keeps the program, the session and the agent's state, and rtty draws, so rtty's
  scrollback, search, selection and links work on it ([tutti panes](#tutti-panes)). A window can be
  a whole session, tutti's tabs and splits its own, or show one tutti pane in rtty's layout;
  tutti's TUI in an rtty pane works as before, beside either.
- **Both run the same engine.** rtty-vt is shared by rtty, tutti's daemon and tutti's TUI, so a
  program behaves the same in either.

## The two front ends

tutti's TUI is the primary front end for a tutti session. It is the complete one: the agent
sidebar, the approvals popup and diffs are there, and new agent features land there first.

rtty's native tutti panes ([tutti panes](#tutti-panes)) are a plain view of a session: rtty draws
the panes, shows on the tab's pill when an agent is working, paused while it is blocked or holds a
permission request, and notifies when an agent is blocked or done. The pill shows no other state
of an agent: done and idle draw no line. No further agent UI is planned in rtty
([Not planned](#not-planned)).

## tutti's TUI in an rtty pane

tutti's TUI in an rtty pane claims rtty's keys
([Keys a pane can claim](keys.md#keys-a-pane-can-claim)) and hands a move or resize past its own
edge on to [`rtty cli`](cli.md#rtty-cli), so the same keys walk through Neovim's windows, tutti's
panes and rtty's (tutti's documentation, "tutti in rtty").

## tutti panes

rtty can show the panes of a tutti session as panes of its own, two ways:

- **A window that is the session**: `rtty --tutti <session>`, or `rtty cli tutti open [-s
  <session>]` from a pane ([rtty cli](cli.md#rtty-cli)). tutti's tabs, of every workspace in
  turn, are the window's tabs, and each tab's split tree, with its ratios, is the tab's layout.
- **One tutti pane in rtty's layout**: `rtty --tutti <session>:<pane>` in the first window, or
  `rtty cli tutti attach <pane>` from a pane, in a split or a tab of its own.

tutti's daemon keeps the programs, the session and the agents' states; rtty draws. Unlike tutti's
TUI in an rtty pane, which redraws a grid, a pane's output reaches rtty's own terminal engine, so
rtty's scrollback, search, selection, copy mode, links, quick select and prompt jumps work on it as
on any pane. tutti's TUI can stay attached beside rtty: rtty never moves its focus.

tutti's daemon must be running; rtty never starts one.

## How a tutti pane works

- **One connection.** A window keeps one connection to each session whose panes it shows, to the
  daemon on its Unix socket (`$XDG_RUNTIME_DIR/tutti/<session>.sock`, else
  `/tmp/tutti-<uid>/<session>.sock`), and everything goes over it, however many panes it shows. It
  attaches as a quiet view that hands moves past tutti's edges on (`Attach` with `view`, `handoff`
  and `quiet`: no pane frames but its own streams'), and watches. The daemon's wire and rtty's must work together, each at or above the
  other's floor, the oldest revision it still reads faithfully: rtty speaks tutti's wire revision
  20 and works with a daemon at 19 or newer, and tutti's own `tutti-wire` crate decides. Otherwise
  rtty says which side to update (``tutti's daemon speaks wire revision 18 (floor 18), rtty 20
  (floor 19): update tutti and restart its daemon (`tutti server stop`)``). The connection opens off the main thread,
  and the daemon has 3 s to answer: meanwhile the window goes on, the panes waiting for it blank;
  should it not answer, they say why (`[cannot show tutti session work: tutti's daemon did not
  answer within 3 s]`, the pane's title too). Closing the window's last pane of the session, or
  the window, closes it.
- **The streams.** Each pane shown is streamed on that connection, every frame naming its pane,
  after the window's colours: up to 10 000 rows of the pane's scrollback go into rtty's, a snapshot
  draws the screen, and every read of the pane's pty follows as it was written. One thread reads
  them all; tutti queues each stream apart, so a stream that falls behind skips alone to a fresh
  snapshot after the rows that scrolled off meanwhile, which go into the scrollback, nothing missed
  and nothing twice. A pane the session no longer has says so and stays.
- **Closing one.** Closing a tutti pane detaches its stream alone (`PaneDetach`), its vote in the
  pane's size with it, while the window's other panes of the session stream on. Shown again
  (undo-close, `rtty cli tutti attach`), the pane is streamed anew, asked for once the daemon has
  answered the detach: what it sent of the old stream until then goes nowhere. A window shows a
  tutti pane once: a second says so.
- **Queries are answered once.** The daemon answers the terminal queries it can (DA1, cursor
  reports, colours, mode 2031's light/dark) and keeps them from the stream; what rtty's engine
  answers goes back to the pane, and answers only what the daemon left. A flip between light and
  dark goes to the daemon as the window's colours, and the daemon tells the programs that asked.
- **Keys follow the program.** The stream hands on the keyboard protocols a program sets (kitty's
  flags, modifyOtherKeys; `PaneAttach` with `keys`), and its snapshots carry them, so rtty encodes
  the pane's keys as the program asked: Neovim's kitty keys reach a tutti pane drawn here. A
  shell's mark of a command's end turns them off, in the daemon and in rtty alike.
- **Size.** The pane's cells are rtty's vote in the pane's size, and tutti gives each pane the
  smallest any viewer votes; rtty's terminal takes the size the daemon settles. While another
  client shows the pane smaller, rtty draws it smaller than its cells.
- **Title and agent.** tutti's name for the pane titles it, after its program's own title; a
  `pane rename` shows at once. The agent working shows on the tab's pill, paused while it is
  blocked or holds a permission request. An agent blocked or done is a notification, from tutti's
  notes (one each; a bell's or a program's own notification comes raw in its stream). A program's
  `TUTTI_KEYS=pane` claims the keys as `RTTY_KEYS=pane` does.
- **Focus.** The pane a window shows focused, active or not, is what rtty's view of the session
  shows focused: it moves no pane of the session's, so an attached TUI stays where it is. While
  the window is the active one someone is looking, so the pane is seen (`PaneFocus` with `seen`),
  as the window comes forward too: a finished agent's pane turns idle and its notes are read. A
  focus from outside any attached client (`tutti pane focus 3`, a notch app's click) shows that
  pane.
- **Editors across tutti and rtty.** A move or resize an editor in a tutti pane asks tutti for
  (`tutti pane focus --direction`, `pane resize`), past the edge of tutti's tab, comes back to rtty,
  which moves on in its own layout: from one tutti pane in rtty's layout to the rtty pane beside
  it, top and bottom edges too.
- **Closing detaches.** `Cmd+W` closes an rtty pane showing a tutti pane, and the tutti pane and
  its program run on; `kill-pane` ends the program, and its exit, however it comes (`tutti pane
  kill` too), closes the rtty pane.

## A window that is a session

A window that is a session keeps the daemon the source of truth: what would change its layout
asks tutti, and the window follows what tutti then tells of the session's tabs.

| rtty | asks tutti |
| --- | --- |
| split right, split down (`Cmd+D`, `Cmd+Shift+D`, `rtty cli pane split`) | `PaneSplit`, a login shell in the new half; the new pane takes the keyboard once tutti has it. tutti splits right or down only: left and up say so |
| new tab (`Cmd+T`, the `+`) | `TabNew` in the shown tab's workspace, then a run of your shell in it |
| resize (`Cmd+Ctrl+arrows`, `rtty cli pane resize`), a divider dragged | `PaneResizeToward`, `PaneSplitRatio` |
| a tab shown | `TabSelect`: where tutti's own runs land follows the window; an attached TUI's tab does not |
| `kill-pane` | `PaneKill`; a pane whose program exited stays, its last screen showing, as tutti keeps it, until killed |

Focus moves, zoom and the last pane are the window's own, as tutti's are its TUI's. `Cmd+W` and a
tab's close button close nothing and say so (tutti's panes run on; `kill-pane` ends one); closing
the window detaches from the session. A pane of rtty's own has no place there: an editor a
Cmd+click would open beside a pane, `open-config`, and `rtty cli tutti attach` say so; swap is not
tutti's. The session's daemon going closes the window, as a last pane closing does.

## Not yet

Not yet: sessions on other machines (`tutti --host`), which are on the
[roadmap](status.md#roadmap), and prompt marks in the scrollback a pane had before rtty attached.

## Not planned

tutti's chrome (the sidebar, approvals, notes, the changes panel) is not planned in rtty: the TUI
stays the place for those. It is agent UI, rtty's tutti panes stay a plain view of a session, and
no further agent UI is built in rtty ([The two front ends](#the-two-front-ends)).
