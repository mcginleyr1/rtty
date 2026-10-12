//! What a frame draws, read from the engine and independent of GPUI: per row, background spans
//! and text runs with their colors and decorations, and the cursor. The view shapes the runs and
//! paints them; everything about which cell gets which color is decided here.

use std::mem;

use rtty_vt::{Attrs, BACKGROUND, CURSOR, Color, CursorShape as Shape, FOREGROUND, Rgb, Row};
use rtty_vt::{Terminal, Underline};

use crate::scheme::{self, SLOTS, Scheme};
use crate::search::Match;
use crate::selection::Point;

/// The colors a frame resolves cells with: a program's own (OSC 4/10/11/12) over the scheme's.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    colors: [Rgb; SLOTS],
    /// The text under a block cursor.
    pub cursor_text: Rgb,
    /// The outline of the cursor while the pane is not focused.
    pub cursor_border: Rgb,
    pub selection: Option<(Rgb, Rgb)>,
    /// Foreground and background of a search match (the background on yellow), and of the
    /// current one (the background on the cursor's color).
    pub matched: (Rgb, Rgb),
    pub current_match: (Rgb, Rgb),
}

impl Palette {
    pub fn new(term: &Terminal, scheme: &Scheme) -> Palette {
        let colors: [Rgb; SLOTS] =
            std::array::from_fn(|i| term.color_override(i).unwrap_or(scheme.color(i)));
        Palette {
            cursor_text: scheme.cursor_fg,
            cursor_border: term.color_override(CURSOR).unwrap_or(scheme.cursor_border),
            selection: scheme.selection,
            matched: (colors[BACKGROUND], colors[3]),
            current_match: (colors[BACKGROUND], colors[CURSOR]),
            colors,
        }
    }

    pub fn color(&self, index: usize) -> Rgb {
        self.colors[index]
    }
}

/// One row as drawn: backgrounds where they differ from the default, and the text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line {
    pub backgrounds: Vec<Background>,
    pub runs: Vec<Run>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Background {
    pub col: u16,
    pub cells: u16,
    pub color: Rgb,
}

/// Consecutive single-width cells of a row with one style, shaped as one string so the font's
/// ligatures can form. Wide characters and clusters stand alone, and the cursor's cell is a run
/// of its own so the glyph under the cursor shows its own character. Runs of blanks with nothing
/// to draw are left out.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub col: u16,
    pub cells: u16,
    pub text: String,
    pub style: Style,
    pub single_width: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub underline: Underline,
    /// SGR 58's color, else the text's.
    pub underline_color: Rgb,
    pub strike: bool,
    pub overline: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
    /// The outline drawn while the pane is not focused.
    Hollow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// Absolute row.
    pub row: u64,
    pub col: u16,
    /// Over a wide character's two cells.
    pub wide: bool,
    pub shape: CursorShape,
    pub color: Rgb,
}

/// What every row of a frame is drawn with.
pub struct Scene<'a> {
    pub palette: &'a Palette,
    /// The default foreground and background, swapped by reverse video (DECSCNM).
    pub fg: Rgb,
    pub bg: Rgb,
    pub selection: Option<(Point, Point)>,
    pub cursor: Option<Cursor>,
    /// Search matches in view, and the current one.
    pub matches: Vec<Match>,
    pub current_match: Option<Match>,
    /// The link under the pointer while Cmd is held, underlined.
    pub link: Option<(Point, Point)>,
}

impl<'a> Scene<'a> {
    /// The scene for rows `top..top + rows`: the cursor shows when the program has not hidden it
    /// and its row is in view.
    pub fn new(
        term: &Terminal,
        palette: &'a Palette,
        top: u64,
        rows: u16,
        selection: Option<(Point, Point)>,
        focused: bool,
    ) -> Scene<'a> {
        let (mut fg, mut bg) = (palette.color(FOREGROUND), palette.color(BACKGROUND));
        if term.modes().reverse_video {
            mem::swap(&mut fg, &mut bg);
        }
        let (row, col) = term.cursor_position();
        let row = term.screen_top() + u64::from(row);
        let shape = match term.cursor_style().map(|style| style.shape) {
            _ if !focused => CursorShape::Hollow,
            None | Some(Shape::Block) => CursorShape::Block,
            Some(Shape::Bar) => CursorShape::Bar,
            Some(Shape::Underline) => CursorShape::Underline,
        };
        let color = match shape {
            CursorShape::Hollow => palette.cursor_border,
            _ => palette.color(CURSOR),
        };
        let visible = term.modes().cursor_visible && (top..top + u64::from(rows)).contains(&row);
        let cursor = visible.then(|| Cursor {
            row,
            col,
            wide: term.screen_row(term.cursor_position().0).cells()[usize::from(col)].is_wide(),
            shape,
            color,
        });
        let (matches, current_match, link) = (Vec::new(), None, None);
        Scene { palette, fg, bg, selection, cursor, matches, current_match, link }
    }

    fn resolve(&self, color: Color, default: Rgb) -> Rgb {
        match color {
            Color::Default => default,
            Color::Named(index) | Color::Indexed(index) => self.palette.color(usize::from(index)),
            Color::Rgb(r, g, b) => Rgb(r, g, b),
        }
    }

    fn selected(&self, point: Point) -> bool {
        self.selection.is_some_and(|(start, end)| (start..=end).contains(&point))
    }

    /// The colors of the search match covering `point`, if one does.
    fn highlight(&self, point: Point) -> Option<(Rgb, Rgb)> {
        let covers = |&(start, end): &Match| (start..=end).contains(&point);
        if self.current_match.as_ref().is_some_and(covers) {
            return Some(self.palette.current_match);
        }
        self.matches.iter().any(covers).then_some(self.palette.matched)
    }

    /// Row `index` as drawn.
    pub fn line(&self, row: &Row, index: u64) -> Line {
        let cursor = self.cursor.filter(|cursor| cursor.row == index);
        let under_block = |col: u16| {
            cursor.is_some_and(|cursor| {
                cursor.shape == CursorShape::Block
                    && (col == cursor.col || cursor.wide && col == cursor.col + 1)
            })
        };
        let starts_run_at_cursor =
            |col: u16| cursor.is_some_and(|cursor| col == cursor.col || col == cursor.col + 1);
        let mut line = Line::default();
        let mut text = String::new();
        for (col, cell) in row.cells().iter().enumerate() {
            let col = col as u16;
            let attrs = cell.attrs();
            let mut fg = self.resolve(cell.fg(), self.fg);
            let mut bg = self.resolve(cell.bg(), self.bg);
            if attrs.contains(Attrs::DIM) {
                fg = scheme::dim(fg);
            }
            if attrs.contains(Attrs::INVERSE) {
                mem::swap(&mut fg, &mut bg);
            }
            // A wide character's right half is selected with its left.
            let owner = if cell.is_spacer() { col.saturating_sub(1) } else { col };
            if let Some(colors) = self.highlight((index, col)) {
                (fg, bg) = colors;
            }
            if self.selected((index, owner)) {
                (fg, bg) = self.palette.selection.unwrap_or((bg, fg));
            }
            if attrs.contains(Attrs::HIDDEN) {
                fg = bg;
            }
            if under_block(col) {
                (fg, bg) = (self.palette.cursor_text, cursor.expect("under the cursor").color);
            }

            match line.backgrounds.last_mut() {
                Some(span) if span.col + span.cells == col && span.color == bg => span.cells += 1,
                _ if bg != self.bg => {
                    line.backgrounds.push(Background { col, cells: 1, color: bg })
                }
                _ => {}
            }

            if cell.is_spacer() {
                continue;
            }
            let linked =
                self.link.is_some_and(|(start, end)| (start..=end).contains(&(index, col)));
            let style = Style {
                fg,
                bg,
                bold: attrs.contains(Attrs::BOLD),
                italic: attrs.contains(Attrs::ITALIC),
                underline: if linked { Underline::Single } else { attrs.underline() },
                underline_color: match linked {
                    true => fg,
                    false => self.resolve(row.underline_color(usize::from(col)), fg),
                },
                strike: attrs.contains(Attrs::STRIKE),
                overline: attrs.contains(Attrs::OVERLINE),
            };
            text.clear();
            row.push_grapheme(usize::from(col), &mut text);
            let single_width = cell.width() == 1 && row.cluster(usize::from(col)).is_none();
            match line.runs.last_mut() {
                Some(run)
                    if single_width
                        && run.single_width
                        && run.col + run.cells == col
                        && run.style == style
                        && !starts_run_at_cursor(col) =>
                {
                    run.text.push_str(&text);
                    run.cells += 1;
                }
                _ => line.runs.push(Run {
                    col,
                    cells: if single_width { 1 } else { u16::from(cell.width()) },
                    text: text.clone(),
                    style,
                    single_width,
                }),
            }
        }
        for run in line.runs.iter_mut().filter(|run| run.single_width && !run.style.decorated()) {
            let kept = run.text.trim_end_matches(' ').len();
            run.cells -= (run.text.len() - kept) as u16;
            run.text.truncate(kept);
        }
        line.runs.retain(|run| !run.text.trim().is_empty() || run.style.decorated());
        line
    }
}

impl Style {
    /// Draws something even over blanks.
    fn decorated(&self) -> bool {
        self.underline != Underline::None || self.strike || self.overline
    }
}

/// Whether the cursor blinks: as DECSCUSR last set it, else as ATT610 (`CSI ? 12 h`) has it.
pub fn blinks(term: &Terminal) -> bool {
    term.cursor_style().map_or(term.modes().cursor_blink, |style| style.blinking)
}

/// Every row of the view `top..top + rows`, and the cursor: what a frame draws.
#[cfg(test)]
pub fn frame(
    term: &Terminal,
    palette: &Palette,
    top: u64,
    rows: u16,
    selection: Option<(Point, Point)>,
    focused: bool,
) -> (Vec<Line>, Option<Cursor>) {
    let scene = Scene::new(term, palette, top, rows, selection, focused);
    let lines = term.rows(top, top + u64::from(rows)).map(|(i, row)| scene.line(row, i)).collect();
    (lines, scene.cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheme::{find, load, rgb};

    fn onedark() -> std::sync::Arc<Scheme> {
        find(&load(&[]).unwrap(), "OneDark Pro").unwrap()
    }

    fn term(bytes: &[u8]) -> Terminal {
        let mut term = Terminal::new(3, 12, 100);
        term.advance(bytes);
        term
    }

    /// The rows and cursor of a focused, unscrolled frame.
    fn draw(term: &Terminal) -> (Vec<Line>, Option<Cursor>) {
        let palette = Palette::new(term, &onedark());
        frame(term, &palette, term.screen_top(), term.size().0, None, true)
    }

    fn texts(line: &Line) -> Vec<(u16, &str)> {
        line.runs.iter().map(|run| (run.col, run.text.as_str())).collect()
    }

    const FG: u32 = 0xabb2bf;
    const BG: u32 = 0x282c34;
    const RED: u32 = 0xe06c75;
    const MAGENTA: u32 = 0xc678dd;

    #[test]
    fn same_style_cells_form_one_run_and_style_changes_split_it() {
        let (lines, _) = draw(&term(b"a -> b \x1b[31m=>\x1b[0m c\r\n"));
        // Blanks at the end of a run draw nothing and are left out.
        assert_eq!(texts(&lines[0]), [(0, "a -> b"), (7, "=>"), (9, " c")]);
        assert_eq!(lines[0].runs[0].cells, 6);
        assert_eq!(lines[0].runs[1].style.fg, rgb(RED));
        assert_eq!(lines[0].runs[0].style.fg, rgb(FG));
        assert!(lines[0].runs.iter().all(|run| run.single_width && run.style.bg == rgb(BG)));
        assert_eq!(lines[0].backgrounds, []);
    }

    #[test]
    fn the_cursor_cell_is_a_run_of_its_own() {
        // The cursor on the ">" of "->": the "-" and ">" no longer ligate.
        let (lines, cursor) = draw(&term(b"x -> y\x1b[1;4H"));
        assert_eq!(texts(&lines[0]), [(0, "x -"), (3, ">"), (4, " y")]);
        let cursor = cursor.unwrap();
        assert_eq!((cursor.row, cursor.col, cursor.shape), (0, 3, CursorShape::Block));
        assert_eq!(cursor.color, rgb(MAGENTA));
        // The block cursor's cell takes the cursor color, its text the scheme's cursor_fg.
        assert_eq!(lines[0].runs[1].style.fg, rgb(BG));
        assert_eq!(lines[0].backgrounds, [Background { col: 3, cells: 1, color: rgb(MAGENTA) }]);
    }

    #[test]
    fn wide_characters_and_clusters_stand_alone() {
        let (lines, _) = draw(&term("ab日e\u{301}cd".as_bytes()));
        assert_eq!(texts(&lines[0]), [(0, "ab"), (2, "日"), (4, "e\u{301}"), (5, "cd")]);
        let cells: Vec<_> = lines[0].runs.iter().map(|run| (run.cells, run.single_width)).collect();
        assert_eq!(cells, [(2, true), (2, false), (1, false), (2, true)]);
    }

    #[test]
    fn colors_resolve_through_the_scheme_and_the_program() {
        let mut term = term(b"\x1b[?25l\x1b[38;5;214mA\x1b[38;2;1;2;3mB\x1b[44mC\x1b[0m");
        let (lines, _) = draw(&term);
        let fgs: Vec<_> = lines[0].runs.iter().map(|run| run.style.fg).collect();
        assert_eq!(fgs, [rgb(0xd19a66), rgb(0x010203), rgb(0x010203)]);
        assert_eq!(lines[0].backgrounds, [Background { col: 2, cells: 1, color: rgb(0x61afef) }]);
        // A program's OSC 4 and OSC 11 win over the scheme.
        term.advance(b"\x1b]4;214;rgb:ff/00/00\x07\x1b]11;#000000\x07");
        let (lines, _) = draw(&term);
        assert_eq!(lines[0].runs[0].style.fg, rgb(0xff0000));
        assert_eq!(lines[0].runs[0].style.bg, rgb(0));
    }

    #[test]
    fn inverse_dim_and_hidden_cells() {
        let (lines, _) = draw(&term(b"\x1b[?25l\x1b[7mI\x1b[0;2mD\x1b[0;8mH\x1b[0m"));
        let styles: Vec<_> = lines[0].runs.iter().map(|run| (run.style.fg, run.style.bg)).collect();
        let dim = scheme::dim(rgb(FG));
        assert_eq!(styles, [(rgb(BG), rgb(FG)), (dim, rgb(BG)), (rgb(BG), rgb(BG))]);
        assert_eq!(lines[0].backgrounds, [Background { col: 0, cells: 1, color: rgb(FG) }]);
    }

    #[test]
    fn reverse_video_swaps_the_default_colors() {
        let (lines, _) = draw(&term(b"\x1b[?25l\x1b[?5hab\x1b[31mc"));
        assert_eq!(lines[0].runs[0].style.fg, rgb(BG));
        assert_eq!(lines[0].runs[1].style.fg, rgb(RED));
        // The default background is now the scheme's foreground, which the pane is filled with.
        assert!(lines[0].runs.iter().all(|run| run.style.bg == rgb(FG)));
        assert_eq!(lines[0].backgrounds, []);
    }

    #[test]
    fn a_selection_takes_the_scheme_selection_colors() {
        let term = term("ab日cd\x1b[?25l".as_bytes());
        let palette = Palette::new(&term, &onedark());
        let (lines, _) = frame(&term, &palette, 0, 3, Some(((0, 1), (0, 2))), true);
        assert_eq!(texts(&lines[0]), [(0, "a"), (1, "b"), (2, "日"), (4, "cd")]);
        assert_eq!(lines[0].runs[1].style.bg, rgb(MAGENTA));
        assert_eq!(lines[0].backgrounds, [Background { col: 1, cells: 3, color: rgb(MAGENTA) }]);
    }

    #[test]
    fn search_matches_take_yellow_the_current_one_the_cursor_color_under_a_selection() {
        let term = term("ab日cd ab\x1b[?25l".as_bytes());
        let palette = Palette::new(&term, &onedark());
        let mut scene = Scene::new(&term, &palette, 0, 3, Some(((0, 8), (0, 8))), true);
        // "日c" (cells 2–4) is the current match; "ab" at 7–8 another, its "b" also selected.
        scene.current_match = Some(((0, 2), (0, 4)));
        scene.matches = vec![((0, 2), (0, 4)), ((0, 7), (0, 8))];
        let line = scene.line(term.row(0).unwrap(), 0);
        let (yellow, purple, bg) = (rgb(0xe5c07b), rgb(MAGENTA), rgb(0x282c34));
        assert_eq!(
            line.backgrounds,
            [
                Background { col: 2, cells: 3, color: purple },
                Background { col: 7, cells: 1, color: yellow },
                Background { col: 8, cells: 1, color: palette.selection.unwrap().1 },
            ]
        );
        let run = |col| line.runs.iter().find(|run| run.col == col).unwrap().style;
        assert_eq!((run(2).fg, run(2).bg), (bg, purple));
        assert_eq!((run(4).fg, run(4).bg), (bg, purple));
        assert_eq!((run(7).fg, run(7).bg), (bg, yellow));
        // The selection's own colors win where the two meet.
        assert_eq!(run(8).fg, palette.selection.unwrap().0);
    }

    #[test]
    fn the_link_under_the_pointer_is_underlined_in_its_text_color() {
        let term = term(b"\x1b[31mgo\x1b[m https://x.y\x1b[?25l");
        let palette = Palette::new(&term, &onedark());
        let mut scene = Scene::new(&term, &palette, 0, 3, None, true);
        scene.link = Some(((0, 3), (0, 11)));
        let line = scene.line(term.row(0).unwrap(), 0);
        assert_eq!(texts(&line), [(0, "go"), (3, "https://x")]);
        let link = line.runs[1].style;
        assert_eq!((link.underline, link.underline_color), (Underline::Single, link.fg));
        assert_eq!(line.runs[0].style.underline, Underline::None);
    }

    #[test]
    fn the_cursor_follows_decscusr_focus_and_dectcem() {
        let mut term = term(b"\x1b[6 q");
        assert_eq!(draw(&term).1.unwrap().shape, CursorShape::Bar);
        term.advance(b"\x1b[4 q");
        assert_eq!(draw(&term).1.unwrap().shape, CursorShape::Underline);
        term.advance(b"\x1b[0 q");
        assert_eq!(draw(&term).1.unwrap().shape, CursorShape::Block);
        let palette = Palette::new(&term, &onedark());
        let (_, unfocused) = frame(&term, &palette, 0, 3, None, false);
        assert_eq!(unfocused.unwrap().shape, CursorShape::Hollow);
        term.advance(b"\x1b[?25l");
        assert_eq!(draw(&term).1, None);
    }

    #[test]
    fn decscusr_and_att610_say_whether_the_cursor_blinks() {
        let mut term = term(b"");
        assert!(!blinks(&term));
        // nvim's insert mode, then normal mode: a steady bar, a steady block.
        term.advance(b"\x1b[6 q");
        assert!(!blinks(&term));
        term.advance(b"\x1b[5 q");
        assert!(blinks(&term));
        term.advance(b"\x1b[2 q\x1b[?12h");
        assert!(!blinks(&term), "DECSCUSR's steady block wins over ATT610");
        term.advance(b"\x1b[0 q");
        assert!(blinks(&term), "the host's default, then ATT610");
        term.advance(b"\x1b[?12l");
        assert!(!blinks(&term));
    }

    #[test]
    fn a_scrolled_view_draws_scrollback_rows_and_hides_a_cursor_below_it() {
        let term = term(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let palette = Palette::new(&term, &onedark());
        let (lines, cursor) = frame(&term, &palette, 0, 3, None, true);
        let rows: Vec<_> = lines.iter().map(|line| line.runs[0].text.as_str()).collect();
        assert_eq!(rows, ["one", "two", "three"]);
        assert_eq!(cursor, None);
        let (_, cursor) = frame(&term, &palette, 1, 3, None, true);
        assert_eq!(cursor, None);
        let (_, cursor) = frame(&term, &palette, 2, 3, None, true);
        assert_eq!(cursor.map(|cursor| (cursor.row, cursor.col)), Some((4, 4)));
    }

    #[test]
    fn underline_styles_and_strikethrough_keep_blank_runs() {
        let (lines, _) = draw(&term(b"\x1b[4:3mcurl\x1b[4:0m \x1b[9m  \x1b[0m"));
        let runs: Vec<_> = lines[0].runs.iter().map(|run| (run.col, run.style.underline)).collect();
        assert_eq!(runs, [(0, Underline::Curly), (5, Underline::None)]);
        assert!(lines[0].runs[1].style.strike);
    }

    #[test]
    fn every_underline_style_and_its_color() {
        let mut term = term(b"");
        // nvim's diagnostics: undercurl in an RGB underline color (colon form), as it sends them.
        term.advance(b"\x1b[0;4:3m\x1b[58:2::255:192:185mA\x1b[4:2;58;5;1mB");
        term.advance(b"\x1b[4:4;59mC\x1b[4:5;31mD\x1b[4mE\x1b[24;53mF\x1b[0m");
        let (lines, _) = draw(&term);
        let runs: Vec<_> = lines[0]
            .runs
            .iter()
            .map(|run| (run.text.as_str(), run.style.underline, run.style.underline_color))
            .collect();
        assert_eq!(
            runs,
            [
                ("A", Underline::Curly, rgb(0xffc0b9)),
                ("B", Underline::Double, rgb(RED)),
                ("C", Underline::Dotted, rgb(FG)),
                ("D", Underline::Dashed, rgb(RED)),
                ("E", Underline::Single, rgb(RED)),
                ("F", Underline::None, rgb(RED)),
            ]
        );
        assert!(lines[0].runs[5].style.overline && !lines[0].runs[4].style.overline);
    }
}
