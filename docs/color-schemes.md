# Color schemes

The scheme file format, where rtty looks for a scheme by name, how it follows the macOS light and
dark appearance, the `minimum_contrast` setting, and how programs are told whether the scheme is
dark or light.

## The format

Schemes are WezTerm's TOML format, so WezTerm scheme files work unchanged:

| Table | Keys |
| --- | --- |
| `[colors]` | `foreground`, `background`, `ansi` (8), `brights` (8) |
| `[colors]`, optional | `cursor_bg`, `cursor_fg`, `cursor_border`, `selection_bg`, `selection_fg`, `split` (the line between panes) |
| `[colors.tab_bar]`, optional | `background`, and `fg_color` and `bg_color` of `active_tab` (the shown tab), `inactive_tab` (the others), `inactive_tab_hover` (one under the pointer), `new_tab` (the `+` button) and `new_tab_hover` (the `+` under the pointer) |
| `[colors.indexed]`, optional | index → color |
| `[metadata]`, optional | `name` (without it the file's stem names the scheme, below) |

Colors are `#rgb`, `#rrggbb` or `#rrggbbaa` (alpha ignored). Keys rtty does not draw
(`scrollbar_thumb`, the tab bar's `inactive_tab_edge`, a tab's `intensity`, …) are ignored.

What a scheme leaves out of the tab bar and the split line is mixed from its foreground and
background:

- the bar is the background, a shade darker in a light scheme;
- the shown tab is the pane's own colors;
- the other tabs dim their titles on the bar, and the `+` is drawn as they are.

A tab that should stand out (the shown one, one under the pointer, the `+` under it) but has the
bar's own background, as in WezTerm's flat tab bars, is lifted from the bar, toward the foreground
in a dark scheme and toward white in a light one, so it is lighter either way. The folder before a
lone tab's title is the scheme's blue.

## Where rtty looks for a scheme

rtty looks for a scheme name in this order, first match wins:

1. Built in: **OneDark Pro** and **One Light** (onedarkpro.nvim's own WezTerm files, MIT, Oli
   Morris), **Solarized Dark** and **Solarized Light** (the canonical palette and 16-color mapping,
   MIT, Ethan Schoonover). Each file in `schemes/` carries its source and license.
2. `~/.config/rtty/colors/*.toml`.
3. WezTerm's `~/.config/wezterm/colors/*.toml`, so an existing WezTerm setup needs no copying.

A scheme is found by its `metadata.name`, else by its file stem (`onedarkpro` finds
`onedarkpro.toml` whatever it calls itself). Every file in those directories is read at startup
and at each reload, so a malformed one stops rtty with its path and the offending key (and keeps
the config in effect at a reload).

## Following the OS

rtty uses `scheme_dark` while the window's appearance is dark and `scheme_light` while it is
light, and switches the moment macOS flips, with no restart. The defaults are OneDark Pro and
Solarized Light. `scheme` names one scheme whatever the OS appearance; setting it together with
either of the other two is an error.

Cells keep their color indices, so everything already on screen recolors on the next frame. Colors
a program set itself (OSC 4/10/11/12) are the program's, not the scheme's, and survive a flip; a
program that cares re-sets them when it hears about the flip
([Telling programs](#telling-programs-dec-mode-2031)). OSC 10/11/12/4 queries answer with the
current scheme.

## Minimum contrast

`minimum_contrast` is a WCAG contrast ratio from 1 (the default, which changes nothing) to 21. Text
whose color falls short of it on its cell's background moves toward white on a dark background,
toward black on a light one, just far enough to reach it (or as far as it goes); only the text's
color moves, never the background. At 4.5, WCAG's ratio for text:

| Text | Its color and contrast | Draws as |
| --- | --- | --- |
| OneDark Pro's comment grey | `#5c6370` (2.3:1) | `#8e939c` |
| Solarized Light's comments, base1 | `#93a1a1` (2.5:1) | `#697373` |
| Solarized Light's own text, base00 | `#657b83` (4.1:1) | `#60747c`, a shade darker |

Text the program made inverse or hidden, the selection and search matches keep their colors. iTerm2
and Ghostty have a setting of the same name.

## Telling programs (DEC mode 2031)

rtty implements contour's color palette update notifications. `CSI ? 996 n` asks for the scheme
and gets `CSI ? 997 ; 1 n` (dark) or `CSI ? 997 ; 2 n` (light). A program that set `CSI ? 2031 h`
gets that report, unsolicited, each time the scheme changes, so Neovim and friends can switch
`background` live. `CSI ? 2031 l` or RIS turns it off, and DECRQM (`CSI ? 2031 $ p`) reports the
mode's state. Dark or light is the scheme's background, not the OS setting, so a pinned light
scheme answers light on a dark Mac. rtty-vt answers all of these itself: rtty hands it the scheme's
palette and leaning at startup, on every flip and at a reload that changes the scheme.
