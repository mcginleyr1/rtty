# Fonts and ligatures

How rtty finds a font for the characters the configured font lacks, turns OpenType features on and
off, sets the line height, draws box, block and powerline characters itself, and keeps ligatures on
the cell grid.

The keys named here (`font`, `fallback`, `size`, `ligatures`, `font_features`, `line_height`) are
the config file's ([Configuration](configuration.md)). The font is Menlo at 14 points unless `font`
and `size` say otherwise, and `--font`, `--size` and `--ligatures` override the file
([Flags](cli.md#flags)).

## Fallback fonts

A character `font` lacks comes from the `fallback` families, in order, then from macOS's own
fallback (emoji, CJK). macOS's knows no font with Nerd Font icons, so with no `fallback` key rtty
adds an installed Nerd Font: Symbols Nerd Font Mono, else Symbols Nerd Font, else the first `… Nerd
Font Mono` family by name, else the first `… Nerd Font`. rtty bundles no font; with no Nerd Font
installed the icons stay missing. An unknown family, in `font` or in `fallback`, is an error.

## Font features

`font_features` turns OpenType features of the font on and off, as CoreText shapes the text:
`"ss01"` or `"+ss01"` turns one on, `"-calt"` off, and `"cv01=2"` picks the font's second alternate
where a feature has several. A tag is four letters or digits; anything else is an error. Which
features there are is the font's: Fira Code's `zero` slashes its zero and `ss01` gives another `r`,
and a font without a feature ignores it. `ligatures` is two of these features too
([Ligatures](#ligatures)): `true` turns on `calt` and `liga`, `false` turns off those and `dlig`.
The words apply in order, the last on a tag winning: `ligatures`, then `font_features`, then
`--ligatures`. So `font_features = ["-calt"]` turns off the contextual alternates that make Fira
Code's, JetBrains Mono's and Cascadia's `->` and `!=`, and `"-liga"` the standard ligatures some
other fonts use; `--ligatures on` turns them back on for that launch.

## Line height

`line_height` makes the cell taller (or shorter) than the font's ascent plus descent, from 0.5 to 3
times it, rounded up to whole points; the text is centered in the taller cell. Box drawing, block
and powerline characters, which rtty draws itself, fill the whole cell, so they still join from row
to row. An underline keeps its place under the text, and a strikethrough through it; in a cell
shorter than the font's text (below about 0.95 for Menlo) they rise to stay inside the cell, never
in the next row's: the underline to the cell's bottom, a double or curly one to a thickness above
it.

## Characters rtty draws itself

rtty draws box drawing (U+2500–257F), block elements (U+2580–259F) and the powerline separators
(U+E0B0–E0BF) itself, filling each cell exactly: lines meet the next cell's, blocks tile, and the
separators fit the cell's height whatever the font, Menlo included. An icon (a private use area
character) or any glyph from another font whose ink is wider than its cell (a Nerd Font that is
not its Mono variant draws its icons about two cells wide) takes the next cell too when that one
is blank, as prompts leave one after each icon, and is centered across both, shrunk only if it is
wider still, as WezTerm and Ghostty draw them; with no blank after it, it is drawn smaller and
centered in its own cell instead of covering its neighbour. A wide character keeps to its two
cells, and anything taller than the line shrinks to it. This is drawing only: the cells stay as
the terminal has them. The font's own glyphs are left alone, so a ligature still reaches over the
cells after it.

## Ligatures

Programming ligatures (`->`, `=>`, `!=`, `===`, `<=`, `|>`, …) render with fonts that have them
(checked with Fira Code, JetBrains Mono, Iosevka and Victor Mono). `--ligatures off` turns off the
font's `calt`, `liga` and `dlig` features; `font_features` turns off (or on) any one of them, over
`ligatures` ([Font features](#font-features)). rtty's own part below (runs shaped whole, glyphs
snapped to their cells, the cursor's cell alone) is the same whatever the features: it only lets
the font's ligatures form, and with `calt` and `liga` off the font forms none.

- Each run of single-width cells with the same foreground, background and font style goes
  through CoreText as one string, so the font's contextual alternates can form.
- The glyphs stay on the grid: every glyph is placed on the cell of the character it was shaped
  from (the shaper reports each glyph's first character). A ligature over N cells starts on its
  first cell, the font draws it across N cell widths (the cell width is the font's own advance),
  and the glyph after it starts N cells later, whatever advances the font or a fallback font
  reported. The run is shaped without GPUI's `force_width`, which gives every glyph one cell by
  glyph order and so shifts everything after a glyph that stands for several characters; instead
  the glyph positions in the shaped layout are rewritten to their cells, and GPUI paints each
  glyph (`Window::paint_glyph`) at the run's origin plus that position. Most coding fonts keep one
  glyph per cell (a spacer plus the ligature glyph), and their glyphs already sit on the grid;
  the snapping matters for fonts whose ligatures replace N glyphs with one, and for fallback
  glyphs with other advances.
- The cell under a visible cursor is shaped on its own, so the cursor shows the plain character it
  sits on (`-` or `>` of an `->`), as kitty, WezTerm and Ghostty do. A selection edge ends a run
  too, since the selection changes the background.
- No ligature forms across a change of color, background, bold/italic, underline or strikethrough;
  across a wide (CJK, emoji) or combining-mark cell, which are shaped alone; or across a line
  wrap.
