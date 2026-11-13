# Cell borders

Lines on the edges and centrelines of cells that take no cells of their own: a bordered panel
uses the same cells as a bare one, where box-drawing characters cost a row or a column per edge.
rtty takes joshka's
[cell-border protocol](https://github.com/joshka/ghostty/blob/joshka/cell-borders/specs/cell-border-protocol.md)
(draft, version 1). This page says what rtty answers, how it draws the lines, and what it leaves
out; the protocol's own document has the commands.

## What rtty answers

A program asks with `DCS 1 + b cell-border;q ST` and rtty answers

```text
DCS 1 + b cell-border;ok;1;pen,rect,half,center,palette,round,priority,outside,radius ST
```

ahead of the DA1 reply when the two are asked together, which is how the protocol tells a terminal
that has borders from one that has not without a timeout.

| Feature | What it is |
| --- | --- |
| `pen` | `pen` and `pen-clear`: the borders every cell written or erased takes |
| `rect` | `set` and `clear` on a rectangle of cells from the cursor |
| `half`, `center` | each half of an outer edge and each centre arm on its own |
| `palette` | `pN` and `fg` colors, beside `RRGGBB` |
| `round` | `round`, `square`, `pen-round`, `pen-square`: rounded corners and forks |
| `priority` | which of two centred strokes on a shared edge is drawn |
| `outside` | the color of what a rounded joint cuts off |
| `radius` | a rounded joint's radius |

Left out: `style`. A stroke that asks for `style=double`, `dashed` or `dotted` is drawn solid, as
the protocol has a terminal without the feature draw it.

## How the lines are drawn

- **Thickness.** A stroke's `WIDTH` is in 1/256 of the cell's width, for horizontal and vertical
  lines alike, rounded to whole device pixels and never under one.
- **Inside.** The stroke lies in its cell, flush with the edge; two neighbours' inside strokes on
  the edge they share are two lines.
- **Centred.** The stroke straddles the edge. Of the centred strokes two neighbours put on a
  shared edge one is drawn: the higher `priority`, then the wider, then the lower or right-hand
  cell's. Centred lines that meet at a corner point reach across each other, so the corner closes.
  On the pane's own edge a centred line is kept on the grid, a pixel of it at least, rather than
  cut away.
- **Centre arms** run where rtty draws box-drawing characters' lines
  ([fonts.md](fonts.md#characters-rtty-draws-itself)), so a border and a `─` of the same
  thickness meet.
- **Colors.** `fg` and `pN` are read from the pane's colors at each frame: they follow the
  scheme, a light/dark flip and what the program set with OSC 4 and OSC 10. A border keeps its
  color whatever its cell's colors are, selected or not.
- **Layers.** Over the cells' backgrounds come the outside fills, then the strokes under the
  text, the text, the strokes over the text, and the bar, underline and hollow cursors. A block
  cursor is its cell's background, so it lies under every stroke.
- **Rounded joints.** A rounded quadrant whose two arms are both drawn (after the merge, a
  corner's in the same placement) draws a quarter circle between them in the horizontal arm's
  stroke. The radius is `radius`, else half the cell's width, and shrinks to the arms there are:
  an edge's other half counts when it is the same stroke and the far corner is not rounded too.
  An arm stops where its arc begins unless the line goes straight on past the joint, which makes a
  fork. `outside` fills what the arc cuts off, within the cell, under every stroke.

Text selection and copying never see borders.

## How borders travel

Borders are part of their cells, as a background color is (the engine side is in
[rtty-vt's README](../vt/README.md)):

- every cell a program writes or erases takes the pen's borders, an empty pen's included, so
  writing over a bordered cell with no pen set takes its borders away;
- SGR 0 empties the pen; DECSC and DECRC, and modes 1048 and 1049, save and restore it;
- cells shifted by ICH and DCH and lines moved by scrolling, IL and DL keep theirs, into the
  scrollback and through a reflow, where a blank cell with borders counts as text;
- a wide character's two cells take the pen without the edge between them.

## In tutti

A program in a [tutti pane](tutti.md#tutti-panes) is not answered, by tutti's daemon or by the
rtty pane that shows it: the daemon keeps the pane's cells and its snapshots carry no borders, so
borders drawn from the pane's raw output would be gone at its next snapshot. The program falls
back to box-drawing characters. tutti's own TUI, running in an rtty pane, is a program like any
other and is answered.

## Not yet

- `style`.
- A look on screen: the lines are checked by unit tests of their pixels and by the `border` lines
  of `--script`'s dump ([status.md](status.md#cell-borders)), not yet by eye, and the protocol
  author's test gallery (`scripts/cell-borders.py` on the same branch) has not been run in rtty.
