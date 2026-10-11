# rtty-vt fixtures

Real terminal byte streams: what Neovim, Emacs, vim, less, man, bat, git, fzf, top and tmux
write to a terminal, plus synthetic streams that isolate one feature each. They are the
regression and differential-testing input for `rtty-vt`.

Each fixture is two files:

- `<name>.out`: the program's output bytes, verbatim, exactly as a terminal would read them
  from the pty master. Nothing else is in it: no input echo the program did not cause and no
  query answers.
- `<name>.json`: how it was recorded and what it contains.

| field | meaning |
|---|---|
| `rows`, `cols` | the pty size for the whole recording; replay at this size |
| `program`, `version`, `argv` | what ran (home-relative paths are reduced to the program name) |
| `env` | the complete environment (`<scratch>` is the throwaway HOME/TMPDIR root) |
| `inputs` | the files the program worked on: CPython stdlib modules or generated data |
| `input` | the scripted input: `keys` (sent at once), `type` (one character per `delay` s), `settle` (wait until output is quiet that long), `wait_for` (regex on output), `sleep`, `exit` |
| `answers` | the reply policy for each query kind (see below) |
| `queries` | every query in the stream, in order: byte `offset` just past it in `.out`, the sequence, and the reply sent (`null`: none) |
| `features` | what the stream exercises, detected by parsing it (not hand-tagged) |
| `checkpoints` | byte offsets where the output had gone quiet after an input step: stable screens to compare at |
| `bytes`, `sha256`, `exit_status`, `duration_s`, `recorded` | the recording itself |

## Using them

Feed `.out` into the engine at `rows`×`cols`, in one write or in arbitrary splits (the
stream does not depend on chunking), and compare screens at each checkpoint offset
against a reference (another emulator, or a stored snapshot). The query offsets mark where
a real terminal would have had to reply; the reply bytes went to the program's input and
are not in `.out`.

## Query replies

The harness answers what programs block on, the same way for every program:

| query | reply |
|---|---|
| DA1 `CSI c` | `CSI ? 62 ; 22 c` |
| DA2 `CSI > c` | `CSI > 1 ; 10 ; 0 c` (what tutti answers) |
| DSR 5 `CSI 5 n` | `CSI 0 n` |
| CPR `CSI 6 n` | `CSI row ; col R` from a cursor the harness tracks only roughly (no scroll regions, origin mode or grapheme clustering): approximate |
| OSC 10 / OSC 11 `?` | `rgb:d0d0/d0d0/d0d0` / `rgb:1c1c/1c1c/1c1c`, with the query's own terminator |
| DECRQM `CSI ? Ps $ p` | the mode's tracked state (1 set, 2 reset) for the modes an xterm-class terminal supports (1 6 7 12 25 47 1000 1002–1007 1015 1047–1049 2004 2026, ANSI 4 20); 0 (not recognized) for anything else, e.g. 69, 2027, 2031, 2048 |
| DECRQSS SGR `DCS $ q m ST` | `DCS 1 $ r 0;<SGR parameters since the last reset> m ST`, roughly tracked. nvim probes undercurl support this way, after `CSI 4:3 m`, and only draws curly/dashed/dotted/double underlines when the reply contains `4:3` |
| kitty `CSI ? u`, XTVERSION, XTGETTCAP, other DECRQSS, XTWINOPS, DSR ?996 | no reply; DA1 resolves the kitty query |

`emacs-80x24-alacritty-da2` overrides DA2 with alacritty's form, `CSI > 0 ; 2500 ; 1 c`.
With a version ≥ 242, Emacs' `xterm.el` also queries OSC 10/11 and enables
modifyOtherKeys.

## Re-recording

macOS, Python ≥ 3.11, standard library only:

```sh
python3 -I vt/fixtures/capture.py --scratch /path/to/empty/scratch            # everything
python3 -I vt/fixtures/capture.py --scratch /path/to/empty/scratch nvim-80x24 sgr
python3 -I vt/fixtures/capture.py --list
python3 -I vt/fixtures/capture.py --scratch DIR --bin nvim=/opt/homebrew/bin/nvim nvim-80x24
```

The recording rewrites `<name>.out`/`<name>.json` next to `capture.py` (or in `--out`) and
regenerates the table below.

- **Environment:** `TERM=xterm-256color`, `COLORTERM=truecolor`, `LANG=en_US.UTF-8`, `SHELL=/bin/sh`,
  a fixed `PATH`, and `HOME`, `TMPDIR` and `TMUX_TMPDIR` inside the scratch directory. That
  directory is wiped before each scenario, so no user configuration loads.
- **Inputs:**
  - the editors, bat and less open CPython standard-library modules (`fnmatch.py`, `argparse.py`);
  - git runs in a generated repository with fixed authors and dates, so commit hashes are stable;
  - fzf filters a generated path list.
- **tmux:** runs with `-f /dev/null` on its own socket (`-L rtty-vt-fixtures`) under the
  scratch `TMUX_TMPDIR`. Its panes run a non-login `/bin/sh`, so no `/etc/profile` prompt with
  host or user names appears. The harness only ever kills that socket's server and the
  processes it started.
- **top:** shows only the lowest pids, which are system daemons.
- **Synthetic streams:** come from `synth.py`, which turns off output post-processing on its
  tty, so the `.out` is byte-identical to what the generator wrote, bare LFs included.
- **Determinism:** two consecutive full recordings were compared byte for byte.
  - **Identical:** all synthetic streams, `seq-flood`, `bat`, the three `emacs` fixtures,
    `vim`, `less`, `man-ls`, `git-log` and `git-diff`.
  - **nvim:** identical up to the `:terminal` step. From there, the job pid in the buffer
    name and the batching of the job's output vary.
  - **fzf, tmux, top:** depend on timing: fzf's loading spinner and initial redraws, tmux's
    redraw batching, top's clock and statistics.
  - **Comparison points:** checkpoints are stable even where the exact bytes are not.
- **Feature detection:** `features` come from parsing the stream. `soft-wrap` and
  `linefeed-scroll` come from the harness's rough cursor, so they are approximate. They
  ignore scroll regions: a status-line region that scrolls above the last row is not
  counted.

## Fixtures

<!-- fixtures:begin -->
| fixture | size | rows×cols | program | features | description |
|---|---:|---|---|---|---|
| `alt-screen` | 1.1 KiB | 24×80 | synth.py (Python 3.14.4) | alt-screen, alt-screen-1047, alt-screen-1049, alt-screen-47, ed, reverse, save-cursor-1048 | alternate screen via 47, 1047 and 1049 with content on both screens and cursor save/restore |
| `bat-80x24` | 53.4 KiB | 24×80 | bat 0.26.1 (979ba22) | bold, box-drawing, linefeed-scroll, osc-dynamic-colors, soft-wrap, truecolor | bat --style=full: line numbers, grid, header, truecolor syntax highlighting |
| `charset-dec` | 0.3 KiB | 24×80 | synth.py (Python 3.14.4) | charset-dec, charset-other, charset-shift, ed, reverse | DEC special graphics via G0 and G1 with SO/SI, UK charset, back to ASCII |
| `decscusr` | 0.3 KiB | 24×80 | synth.py (Python 3.14.4) | cursor-blink, decscusr, dectcem, ed, reverse | DECSCUSR 0-6 and the default form, DECTCEM and cursor-blink mode |
| `decstbm` | 1.2 KiB | 24×80 | synth.py (Python 3.14.4) | decom, decstbm, ed, il-dl, linefeed-scroll, reverse-index, scroll-su-sd | scroll regions: LF and RI at margins, SU/SD, IL/DL inside and outside, origin mode, reset |
| `edit-ops` | 0.8 KiB | 24×80 | synth.py (Python 3.14.4) | 16-color, decaln, ech, ed, el, ich-dch, il-dl, irm, rep | ICH/DCH/ECH/EL/ED/REP/IL/DL, insert mode, DECALN |
| `emacs-120x40` | 31.3 KiB | 40×120 | GNU Emacs 32.0.50 | alt-screen, alt-screen-1049, ambiguous-width, backspace, bold, bracketed-paste, combining, cursor-blink, decckm, decstbm, dectcem, ed, el, emoji, focus-1004, il-dl, keypad-mode, mouse-1000, mouse-1003, mouse-1006, reverse, tabs, truecolor, wide | emacs -nw -Q: font-locked Python, C-x 2/3, M-x list-colors-display, xterm-mouse-mode, wide text in *scratch*, C-x C-c |
| `emacs-80x24-alacritty-da2` | 22.2 KiB | 24×80 | GNU Emacs 32.0.50 | alt-screen, alt-screen-1049, ambiguous-width, backspace, bold, bracketed-paste, combining, cursor-blink, decckm, decstbm, dectcem, ed, el, emoji, focus-1004, ich-dch, il-dl, keypad-mode, modify-other-keys, mouse-1000, mouse-1003, mouse-1006, osc-dynamic-colors, reverse, tabs, truecolor, wide | emacs as above, with an alacritty-style DA2 reply (version 2500): xterm.el queries the background and enables modifyOtherKeys |
| `emacs-80x24` | 19.0 KiB | 24×80 | GNU Emacs 32.0.50 | alt-screen, alt-screen-1049, ambiguous-width, backspace, bold, bracketed-paste, combining, cursor-blink, decckm, decstbm, dectcem, ed, el, emoji, focus-1004, il-dl, keypad-mode, mouse-1000, mouse-1003, mouse-1006, reverse, tabs, truecolor, wide | emacs -nw -Q: font-locked Python, C-x 2/3, M-x list-colors-display, xterm-mouse-mode, wide text in *scratch*, C-x C-c |
| `fzf-80x24` | 23.1 KiB | 24×80 | 0.74.5 (Homebrew) | 256-color, alt-screen, alt-screen-1049, bold, box-drawing, bracketed-paste, decawm, decawm-off, dectcem, el, mouse-1000, mouse-1002, mouse-1006, soft-wrap, sync-2026 | fzf full screen over generated paths: type a query, move, select |
| `fzf-height-80x24` | 9.5 KiB | 24×80 | 0.74.5 (Homebrew) | 256-color, bold, box-drawing, bracketed-paste, decawm, decawm-off, dectcem, ed, el, mouse-1000, mouse-1002, mouse-1006, sync-2026 | fzf --height=40% --border below five lines of output: inline mode, uses CPR |
| `git-diff-80x24` | 1.7 KiB | 24×80 | git version 2.56.0 | 16-color, bold, decckm, el, keypad-mode, linefeed-scroll, wide | git diff --color of unstaged edits in a synthetic repo, paged by less -FRX |
| `git-log-80x24` | 1.6 KiB | 24×80 | git version 2.56.0 | 16-color, backspace, bel, bold, decckm, el, keypad-mode, linefeed-scroll, reverse, soft-wrap, wide | git log --graph --oneline --color --all of a synthetic repo, paged by less -FRX |
| `less-80x24` | 11.7 KiB | 24×80 | less 668 (POSIX regular expressions) | alt-screen, alt-screen-1049, backspace, bold, decckm, ed, el, keypad-mode, linefeed-scroll, reverse, reverse-index, soft-wrap | less on CPython's argparse.py (~2,800 lines): page, search, jump to end and back, toggle line numbers |
| `man-ls-80x24` | 6.9 KiB | 24×80 | macOS 27.0.1 /usr/bin/man | alt-screen, alt-screen-1049, ambiguous-width, backspace, bold, decckm, el, keypad-mode, linefeed-scroll, reverse, underline | man ls through its pager: overstrike bold/underline rendered by less, paging, search |
| `modes` | 0.5 KiB | 24×80 | synth.py (Python 3.14.4) | alternate-scroll-1007, bracketed-paste, decckm, ed, focus-1004, keypad-mode, kitty-kbd, modify-other-keys, mouse-1000, mouse-1002, mouse-1003, mouse-1005, mouse-1006, mouse-1015, reverse, title, title-stack | input-affecting modes: mouse 1000-1006/1015, focus, bracketed paste, DECCKM, keypad, kitty keyboard push/pop, modifyOtherKeys, title stack |
| `nvim-120x40` | 79.6 KiB | 40×120 | NVIM v0.12.4 | alt-screen, alt-screen-1049, ambiguous-width, backspace, bold, box-drawing, bracketed-paste, bright-color, combining, dashed-underline, decckm, decscusr, decstbm, dectcem, dotted-underline, double-underline, ech, ed, el, emoji, emoji-zwj, focus-1004, il-dl, keypad-mode, modify-other-keys, mouse-1002, mouse-1006, osc-dynamic-colors, reverse, sgr-colon, sync-2026, truecolor, undercurl, underline-color, vs16, wide | nvim --clean: scroll, 256-color/truecolor, habamax, splits, mouse, insert/replace/visual, paste, search, diagnostics with curly/dashed/double/dotted coloured underlines, floats, :terminal, :qa! |
| `nvim-80x24` | 56.2 KiB | 24×80 | NVIM v0.12.4 | alt-screen, alt-screen-1049, ambiguous-width, backspace, bold, box-drawing, bracketed-paste, bright-color, combining, dashed-underline, decckm, decscusr, decstbm, dectcem, dotted-underline, double-underline, ech, ed, el, emoji, emoji-zwj, focus-1004, il-dl, keypad-mode, modify-other-keys, mouse-1002, mouse-1006, osc-dynamic-colors, reverse, sgr-colon, sync-2026, truecolor, undercurl, underline-color, vs16, wide | nvim --clean: scroll, 256-color/truecolor, habamax, splits, mouse, insert/replace/visual, paste, search, diagnostics with curly/dashed/double/dotted coloured underlines, floats, :terminal, :qa! |
| `osc-shell` | 0.6 KiB | 24×80 | synth.py (Python 3.14.4) | 16-color, bold, ed, osc133, osc1337, osc52, osc7, osc9-progress, title | a shell session with OSC 133 prompt marks, OSC 7 cwd, OSC 1337 SetUserVar, titles, OSC 52 |
| `osc8` | 0.7 KiB | 24×80 | synth.py (Python 3.14.4) | 16-color, bold, ed, osc8, reverse, soft-wrap, underline, wide | OSC 8 hyperlinks: plain, with id, split across lines, BEL and ST terminators |
| `seq-flood-80x24` | 1,454.0 KiB | 24×80 | macOS 27.0.1 /usr/bin/seq | linefeed-scroll | seq 1 200000: a 1.3 MB flood of short lines, scrolling the whole screen |
| `sgr` | 14.8 KiB | 24×80 | synth.py (Python 3.14.4) | 16-color, 256-color, blink, bold, box-drawing, bright-color, conceal, dashed-underline, dim, dotted-underline, double-underline, ed, encircled, framed, italic, linefeed-scroll, overline, reverse, sgr-colon, soft-wrap, strikethrough, subscript, superscript, truecolor, undercurl, underline, underline-color | every SGR attribute: 16/256/truecolor in ; and : forms, all underline styles, 58/59 underline colour, malformed params |
| `sync-2026` | 90.3 KiB | 24×80 | synth.py (Python 3.14.4) | 256-color, ed, el, linefeed-scroll, sync-2026 | 60 full-screen frames, each inside a mode 2026 synchronized update |
| `tabs` | 0.4 KiB | 24×80 | synth.py (Python 3.14.4) | ed, reverse, tabs | default tab stops, HTS/TBC, CHT/CBT, tabs at the right margin, tabs not erasing |
| `tmux-80x24` | 7.6 KiB | 24×80 | tmux 3.6a | 16-color, alt-screen, alt-screen-1049, bold, box-drawing, bracketed-paste, color-scheme-2031, dec-mode-7727, decckm, decstbm, dectcem, ech, ed, el, keypad-mode, osc-dynamic-colors, wide, xtwinops | tmux -f /dev/null on an isolated socket: three panes, typing, zoom, kill-server |
| `top-80x24` | 2.4 KiB | 24×80 | macOS 27.0.1 /usr/bin/top | alt-screen, alt-screen-1049, decawm, decckm, decstbm, ed, ich-dch, keypad-mode, meta-1034, reverse | macOS top sorted by ascending pid, 12 rows (system daemons only), first frame plus two one-second refreshes |
| `unicode` | 0.7 KiB | 24×80 | synth.py (Python 3.14.4) | ambiguous-width, combining, ed, emoji, emoji-modifier, emoji-zwj, format-chars, regional-indicator, reverse, soft-wrap, vs15, vs16, wide | wide CJK, combining marks, ZWJ emoji, skin tones, flags, VS15/VS16, wide at the margin |
| `vim-80x24` | 9.2 KiB | 24×80 | VIM - Vi IMproved 9.1 (2024 Jan 02, compiled Aug  8 2026 16:52:02) | 16-color, 256-color, alt-screen, alt-screen-1049, ambiguous-width, backspace, bold, bracketed-paste, bright-color, combining, cursor-blink, dcs, decckm, decstbm, dectcem, ed, el, emoji, focus-1004, il-dl, keypad-mode, modify-other-keys, mouse-1000, mouse-1002, mouse-1006, reverse, soft-wrap, title-stack, vs16, wide | vim --clean: scroll, split, vsplit, insert wide text, search, :qa! |
| `wrap` | 1.1 KiB | 24×80 | synth.py (Python 3.14.4) | decawm, decawm-off, ed, linefeed-scroll, reverse, soft-wrap | soft-wrapped long lines, pending-wrap at column 80, DECAWM off, wrap at the bottom |
<!-- fixtures:end -->
