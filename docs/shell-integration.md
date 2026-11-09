# Shell integration

What rtty's shell integration marks, how it loads into zsh, bash and fish, what happens when a
prompt marks itself too, and what rtty does with the marks.

zsh, bash and fish in rtty mark where each prompt starts, where its command line starts, where a
command's output starts and where the command ends, with its status (OSC 133 A, B, C and D), and
report their directory at each prompt (OSC 7), with no edit to your dotfiles.
`shell_integration = "none"` leaves the shell as it is; a prompt that sends the marks itself
(oh-my-posh can) still gets everything in [What the marks give](#what-the-marks-give).

## How it loads

The scripts (`shell/`) are built into rtty, which writes them at startup into its private
directory, `$TMPDIR/rtty-<uid>/shell/`, so `cargo run` and the `.app` behave alike. A new pane's
login shell loads them; a `-e` program and any other shell are left alone.

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

## Marked twice

A prompt that marks itself as well (oh-my-posh, another terminal's integration copied into a
dotfile) puts a second mark of a kind right after rtty's, on the same row. rtty's scripts add A and
B to `PS1` only when it has none, and rtty counts a run of one kind of mark on one row as its
first, so a prompt jump stops once per prompt, the last output ends at its first end, a long
command notifies once, and what a command left on goes off once
([What a command leaves on](keys.md#what-a-command-leaves-on)). Skipping rtty's integration when
the user has one was the other way, but a prompt's marks show only once it draws, after the shell
has started.

## What the marks give

### Prompt jumps

`Cmd+Up` and `Cmd+Down` jump between prompts ([Keys](keys.md#default-keys)).

### The last command's output

`Cmd+Shift+A` (`copy-last-output`) copies the output of the last command that finished, from its
output mark to its end (or the next prompt, from a shell that marks no end), scrollback included.
A label at the pane's bottom right says for a moment that it copied, that the command printed
nothing, or that there is no output to copy.

### A long command's end

A command that ran `notify_after` seconds or more (10 by default; 0 for never) and ends in a pane
that is not the focused pane of the key window posts a notification: its command line, as typed
after the prompt, then its exit status, how long it ran and the pane's title. Like a program's own
notification it needs the app bundle
([Bell and notifications](windows.md#bell-and-notifications)), and clicking it brings rtty forward
at that pane.

A mark a program sends while the alternate screen is up is not the shell's, and neither starts nor
ends a command. rtty tells one apart by looking for the event's row, column and kind among the
marks the primary screen holds, so a program on the alternate screen that sends a mark exactly
where the shell left one of that kind passes for the shell. Telling for sure needs rtty-vt's mark
event to say whether it was stored, which waits because tutti reads that event too.

### Click to move the cursor

A plain click on the command line being typed (after the last prompt, before a command runs), in
the pane that already has the keyboard and whose program takes no mouse reports, moves the shell's
cursor there with arrow keys, as Ghostty's `cursor-click-to-move` does (`click_to_move = false`
turns it off).

- An arrow moves a character, so a wide character is one, clicked on either half.
- The line runs on across soft wraps.
- A click past what is typed goes to its end, and one on the prompt, off the line, or ending a
  drag moves nothing.
- A right prompt (zsh's `RPROMPT`) on the line counts as typed, so a click between the two sends
  more arrows than the shell can take.
- Spaces typed at the end and moved back over look like blank cells, so a click into them stops at
  the last character that is not a space.
- It takes a shell that marks a command's output, and once it has marked one, so not before a new
  pane's first command: without that mark a prompt's end is the last mark while a command runs too
  (bash 3.2 with a DEBUG trap of your own), and a click would type arrows into the command.
