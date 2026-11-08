# rtty-vt

The terminal core shared by tutti's daemon, tutti's TUI and the rtty GPU
terminal: the bytes a program writes go in, a grid with absolute-numbered
scrollback, modes, replies and events come out. No IO (but for the image
files a program names, with graphics on). The byte-level parser
is `vte`'s DEC state machine; everything above it is this crate.

```rust
let mut term = rtty_vt::Terminal::new(24, 80, 10_000);
term.advance(output);                    // the pty's bytes, in any chunks
for event in term.take_events() { … }    // replies for the pty, titles, marks…
let rows = term.take_damage();           // visible rows changed since last time
let wire = term.redraw(&rows);           // escape bytes bringing a follower level
let fresh = term.snapshot();             // escape bytes for a fresh terminal
let past = term.history(from, to);       // scrollback rows, for its scrollback
let bytes = input::encode_key(&key, term.modes(), term.kitty_keyboard()); // a key, for the pty
```

- Rows: every row the primary screen ever showed has an absolute index;
  `screen_top()` and `oldest_row()` never go backwards; `rows(from, to)` reads
  scrollback and screen; OSC 133 marks travel with their rows. A mark sits
  where the next character goes: one past the last column (`col` is the
  width) when a character filled it and a wrap is pending.
- Scrollback rows hold what they show: a row leaving the screen keeps its
  cells up to its last written one (all of them when it soft-wraps), so a
  scrollback row's `cells()` can be shorter than the terminal is wide. The
  columns past them are blank (`Cell::default()`), and `cluster`,
  `hyperlink`, `underline_color` and `push_grapheme` read them so. A row
  back on the screen (a taller resize) is its width again. A full 10k-row
  scrollback of `seq`'s lines holds 2.1 MB at 80 or 200 columns, where
  full-width rows held 14 and 34 MB (`tests/memory.rs`).
- The alternate screen's rows come with the first switch to it, at the size
  then, and go at a reset (RIS). A terminal at a prompt that never switched
  holds 36 KB at 80×24 and 82 KB at 200×24, against 69 and 162 KB with
  both screens.
- `set_command_end_reset(true)`: at a shell's mark of a command's end
  (OSC 133;D, the first of a run), the reports and key encodings a killed
  program left on go off, there in the stream (mouse, 1004, 2031,
  modifyOtherKeys, kitty's flags; bracketed paste, DECCKM and DECKPAM stay).
- OSC 52: a set is `Event::Clipboard`, a query (`52;c;?`) is
  `Event::ClipboardRead`, which the engine never answers itself; a host that
  ignores the event leaves the query unanswered, as before.
- `clear()`: the host's own clear (rtty's Cmd+K) drops the primary screen's
  scrollback whichever screen is up, and blanks the primary screen without
  ED 2's push into the scrollback; the alternate screen stays.
- `history(from, to)`: rows as bytes a terminal replays into its own
  scrollback (a `--inline` attach's history, in tutti). Each logical line —
  soft-wrapped rows joined, for that terminal to wrap at its own width — in
  its renditions, links and clusters, ended by `\r\n`; trailing plain blanks
  and pads left out. A terminal that fills a row it scrolls in with the
  current background (BCE) paints the rest of the row a coloured line wraps
  onto, as it would the program's own output.
- Resize reflows the primary screen (scrollback included); the alternate
  screen is cut or padded.
- `input`: keys and the mouse as the bytes a terminal sends its program, so
  a host only turns its own events (GPUI's, crossterm's) into rtty-vt's.
  - `encode_key(&Key, &Modes, flags)`: a `Key` is its `Code` (a character,
    unshifted, or a named key), kitty's modifier bits (`SHIFT`, `ALT`,
    `CTRL`, `SUPER`, ... and the locks), press, repeat or release, and what a
    host's own kitty terminal said of it: the text it typed, kitty's number
    for it, its shifted and base-layout keys. Encoded as the program asked:
    kitty's flags (`DISAMBIGUATE`, `EVENT_TYPES`, `ALTERNATE_KEYS`,
    `ALL_KEYS`, `ASSOCIATED_TEXT`) first, else modifyOtherKeys 1 or 2, else
    legacy xterm keys under DECCKM. `None` when the key sends nothing: a
    release or a lone modifier nobody asked for, a key with no encoding.
    A key that types text sends that text; a host with its own text input
    (an IME) compares and keeps those.
  - `encode_mouse(&Mouse, &Modes)`: a button, press, release or motion,
    Shift, Alt and Ctrl, the cell (and the pixel, for 1016), reported as
    the mouse mode (9, 1000, 1002, 1003) and encoding (default, 1005, 1006,
    1015, 1016) ask, or `None` when the mode leaves the event out.
  - Where the specs leave a choice, the tests say which and why: kitty's
    protocol never uses `SS3`; modifyOtherKeys 1 leaves Alt's ESC and the
    control characters (Ctrl+Shift+A, Ctrl+3, Ctrl+Backspace) as Emacs
    reads them; the one-byte mouse encodings clamp a far cell as xterm does.
- Unicode: `unicode-width` per character, grapheme clusters by
  `unicode-segmentation` for combining marks and emoji sequences; see the
  crate documentation for the width rules and VS16.
- `set_graphics(true)`: kitty's graphics protocol, off by default (APC
  strings are then dropped as they always were, so tutti's daemon is
  unchanged). `vte` drops APC strings, so the engine splits them off the
  stream before it, each up to `APC_MAX` (16 MiB). A command runs only when
  ST (`ESC \`) ends it; an ESC that starts anything else drops it unrun.
  - Transmit (`a=t`, `a=T`): RGB, RGBA and PNG (`f=24`, `32`, `100`), zlib
    (`o=z`), in chunks (`m=1`), in the string or from a file (`t=d`, `t=f`,
    `t=t`) by the specification's rules: regular files only (checked on the
    path, then on the file, opened without blocking), at most the quota
    (the read too), none under `/proc`, `/sys` or `/dev`, `t=t` only in a temporary
    directory with `tty-graphics-protocol` in its path and deleted once
    read, every failure the same `EBADF`. `I` image numbers.
  - Query (`a=q`) and feature queries (`e`); quiet `q=1`, `q=2`.
  - Put (`a=p`, `a=T`): `c`/`r` (one keeps the shape, both letterbox),
    `x`/`y`/`w`/`h`, `X`/`Y`, `z`, `p`; the cursor goes to the image's last
    row, past its right edge, unless `C=1`.
  - Unicode placeholders: a virtual placement (`U=1`) lays an image out over
    a block of cells; cells of U+10EEEE show it, their diacritics naming the
    row and column of the block (a third the image id's high byte), their
    foreground the image and their underline colour the placement, a
    diacritic left out carried on from the cell to the left. Being text,
    they pass through tmux and tutti, and `snapshot()` carries them.
  - Delete (`a=d`): `d=` `a c i n p q r x y z`, and their capitals, which
    also free the images then placed nowhere.
  - Not supported, each answering `EINVAL`: shared memory (`t=s`),
    animation (`a=f`, `a=a`, `a=c`), relative placements (`P`), indexed
    colour (`f=8`, `f=16`).
  - Images are held as RGBA up to `GRAPHICS_QUOTA` (256 MiB) per terminal;
    past it the oldest go, those placed nowhere first. However small they
    are, a terminal holds at most 4096 images (the oldest going as past the
    quota) and 4096 placements, virtual ones included (the oldest going
    first). `placements(from,
    to)` gives the host each image over those rows (the image, the cell it
    counts from, where in cells, which part of it, `z`); `images()` the ones
    held, each with a generation unique in the process.
  - A placement belongs to the row of its top left cell: it scrolls into the
    scrollback, reflows with that cell (keeping its size in cells), and goes
    when its row is trimmed, scrolls out of a margin region or is reset (ED
    2, the alternate screen's clearing, RIS); ED 0 and 1 and the other
    erases leave it. While the alternate screen is up, the primary screen's
    placements (its scrollback's too) neither draw nor go with its deletes;
    a narrowing that cuts the alternate screen's cells takes their images.
  - The engine's one IO: `t=f` and `t=t` read the files a program names.

Tests: per-feature tests from the specs; differential tests against
alacritty_terminal over a randomized grammar and the recorded programs in
`fixtures/` (`DIVERGENCES.md` lists the known differences); checks that
`snapshot()` and `redraw()` reproduce the visible state, over random streams
and at every fixture checkpoint; property tests that resizes keep
lines and numbering, and that no input panics, graphics commands with graphics
on included; and an answer to every query
the recorded programs sent. The long campaigns run with
`cargo test --release -- --ignored`; throughput against alacritty_terminal and
vt100 with `cargo bench`; the heap bytes a terminal holds after a session with
`cargo test --release --test memory -- --ignored --nocapture`.

The fixture tests read `fixtures/`, or the directories `RTTY_VT_FIXTURES`
lists (a path list, as `PATH` is):
`cargo test --config 'env.RTTY_VT_FIXTURES="/one/fixtures:/other/fixtures"'`.
