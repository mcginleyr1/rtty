# Measurements

Throughput, memory and drawing costs measured on an Apple silicon Mac with a release build, with
the commands that measured them. The flags used, `--bench` and `--stats`, are in
[scripting.md](scripting.md#--bench).

## `rtty --bench`

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

## Memory

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

## In the window

In the window (900×600, Menlo 14, launch to exit with `/usr/bin/time`, three runs each): five
copies of the dense file (163 MB) drain in 1.24 s with 0.89 s of user CPU (alacritty_terminal:
1.38 s, 1.24 s), five of `seq`'s output (74 MB) in 0.80–0.86 s (1.10–1.13 s). An idle window
over a quiet program uses 0.13 s of CPU in 20 s once started, as before.

## Splits

Splits (40×121 cells, timed by `/usr/bin/time -p` in the pane, three runs each): the five dense
copies drain in 0.95–1.07 s in a lone pane and 0.94–1.00 s in one pane of a 2×2 split while the
other three sit at a prompt. Idle, the window costs the same with eight panes as with one: 0.14 s
of CPU per 30 s (comparing 10 s and 40 s idle runs, startup excluded), about 5 ms a second. With
one pane of four redrawing a line, `--stats` prints one line a second: the idle panes draw
nothing.

## Drawing

Drawing (`--stats`): a frame re-reads and reshapes only the rows the engine reports changed,
plus the cursor's. A steady one-line redraw costs ~0.06–0.09 ms layout + 0.2 ms paint per frame;
the dense flood at 6 pt (~250×75 cells, a style change every word) 1.0 ms layout + 8.3 ms paint
(alacritty_terminal: 1.6 ms + 8.3 ms). CPU time; GPU time is not measured. GPUI stops drawing
while the window is occluded, so `--stats` prints nothing for a hidden window.
