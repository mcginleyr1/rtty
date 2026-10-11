# Where rtty-vt parts from alacritty_terminal

`tests/differential.rs` feeds identical streams to rtty-vt and
alacritty_terminal 0.26 and compares the visible grid (text, colours,
attributes, wide-character layout, underline colour, links), soft-wrap flags,
the cursor and its pending wrap, the modes both keep, and the scrollback. The
cases below are known differences. Each is kept out of the random streams by a
predicate in `tests/common/mod.rs` (`Model::diverges`, or the `Oracle` veto
that reads alacritty's own state), by the way the grammar writes a token, or
by leaving the feature out of the oracle grammar; the snapshot and unit tests
cover those features instead.

## Deliberate: the spec or xterm, where alacritty differs

| Case | rtty-vt | alacritty_terminal | Kept out by |
|---|---|---|---|
| LF, VT, FF, IND, RI with a wrap pending | the pending wrap is cleared (any cursor movement clears DEC's last-column flag) | it stays: the next character wraps again, skipping a row | predicate |
| HT with a wrap pending | the cursor stays on the last column, the wrap still pending (xterm) | wraps to the next row | predicate |
| CBT with a wrap pending | clears it | keeps it | predicate |
| LNM (`CSI 20 h`) | LF, VT and FF also return the carriage | the mode is stored but LF ignores it | predicate |
| CUU, CUD, CNL, CPL inside a scroll region | stop at the margin (VT510) | clamp to the screen only | predicate |
| IL, DL | the cursor goes to column 1 (VT510) | the column stays | grammar (`CR` follows) |
| DECOM reset (`CSI ? 6 l`) | homes the cursor (VT510) | leaves it | grammar (`CUP` follows) |
| DECALN | also resets the margins and homes the cursor (VT510) | neither | grammar (`DECSTBM`, `CUP` follow) |
| DECSC / DECRC, and 1049's save and restore | origin mode (DECOM) and the invoked character set (GL) are saved and restored (VT510) | neither is | predicate |
| Modes 47, 1047, 1048 | the alternate screen without the cursor save, with a clear on the way out, and the save alone | ignored | left out of the oracle grammar |
| SGR 21 | double underline (ECMA-48, xterm) | bold off | left out |
| Erasing, inserting or deleting across half a wide character (ECH, EL, ED, ICH, DCH, insert mode) | the other half is blanked too (xterm) | a wide character is left without its spacer, or a spacer without its character | predicate |
| A grapheme cluster | stops growing at 32 characters | unbounded | predicate |
| A combining mark with the cursor on a pad (a wide character that did not fit the last column) | dropped: a pad holds no text | attaches it to its leading spacer | predicate |
| ZWJ sequences, emoji modifiers, regional-indicator flags | one cluster in one cell pair (`unicode-segmentation`) | each emoji its own cell | left out (ZWJ, modifiers and indicators are not in the oracle's character pool) |
| REP after a DEL, or after RIS | repeats the last character written (none after a reset) | repeats the DEL it ignored, or the character from before the reset | predicate |
| REP after a character-set change | repeats the character as it was printed | maps it through the current set | predicate |
| REP in insert mode | the insert-mode write, wide characters kept whole | wide characters shifted apart | predicate |
| A wide character rewritten where the pad above made room for it | the pad stays a pad | it becomes a plain blank, for good | the comparison treats our pad as their blank |
| A row's soft wrap after DCH then an insert | any insert or delete ends the soft wrap | the flag lives on the last cell and travels with it, so an insert can move it back to the end and wrap the row again | `Oracle` veto (alacritty has a wrap flag away from a row's end) |
| Blink, overline | kept | not kept | not compared |
| `vte` 0.15 dropping a character after a UTF-8 character split across reads | split characters are held back until whole | the character after a split one can be lost | alacritty is fed on character boundaries |

## alacritty_terminal bugs found by the comparison

| Case | rtty-vt | alacritty_terminal |
|---|---|---|
| DCH of more cells than remain right of the cursor | blanks from the cursor | `end = columns - count` blanks cells left of the cursor too |
| ED 1 with the cursor on row 2 | clears row 1 and row 2 up to the cursor | `if cursor.line > 1` skips row 1 |
| ED 2 on an empty screen with no scrollback | pushes nothing | pushes one blank row into the scrollback |
| CHA / HPA in origin mode with the region not at the top | the cursor keeps its row | `goto` adds the region's top to the row again |
| DECSTBM with the top at or below the last row and the bottom past it | ignored | a one-row region |

## Recorded programs

`fixtures_match_alacritty` compares at every byte offset a fixture's metadata
names (its checkpoints and queries) and at its end, for the fixtures in
`fixtures/` or the directories `RTTY_VT_FIXTURES` lists; `fixtures_round_trip` (`tests/snapshot.rs`) checks
`snapshot()` and a `redraw` follower at the same offsets. A fixture that uses
one of the deliberate differences above is compared up to its first use and
the test prints which one stopped it; the rest must match to the end. Two
allowances: offsets inside an open synchronized update are skipped, and blank
rows leading the scrollback are ignored on both sides (alacritty's ED 2 bug
pushes one).

## Not compared

- Resize and reflow: the two reflow differently (alacritty keeps the cursor's
  distance from the bottom; rtty-vt keeps the screen's first line at the top
  while the cursor stays in view). `tests/scrollback.rs` property-tests that
  lines, the cursor and the numbering survive any resizes.
- Synchronized updates (2026): alacritty buffers the input while one is open;
  rtty-vt applies it and reports the mode, leaving the frame to the renderer.
- Replies and events: alacritty sends them through its event listener; the
  query and event tests check rtty-vt's against the specs and the recorded
  programs. The identification replies differ:

  | Query | rtty-vt | alacritty_terminal 0.26 |
  |---|---|---|
  | DA1 `CSI c` | `CSI ? 62 ; 22 c`: a VT220 with ANSI colour | `CSI ? 6 c`: a VT102 |
  | DA2 `CSI > c` | `CSI > 1 ; 277 ; 0 c`: xterm patch level `XTERM_VERSION`, past the 242 emacs and the 277 vim turn xterm features on at | `CSI > 0 ; 2600 ; 1 c`: its crate version |
  | XTVERSION `CSI > q` | `DCS > \| rtty-vt <crate version> ST` | no answer |
- The scrollback while the alternate screen is up: alacritty's grid is then
  the alternate one; rtty-vt keeps the primary's scrollback in reach.
- Tab characters: alacritty writes `\t` into the cell a tab starts from; the
  comparison reads it as a space.
