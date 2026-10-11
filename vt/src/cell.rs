/// A colour as a program set it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Color {
    /// The terminal's default foreground or background.
    #[default]
    Default,
    /// One of the 16 ANSI colours, from SGR 30–37, 40–47, 90–97 or 100–107.
    Named(u8),
    /// A 256-colour palette index, from `38;5;n` and its kin.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl Underline {
    const ALL: [Underline; 6] = [
        Underline::None,
        Underline::Single,
        Underline::Double,
        Underline::Curly,
        Underline::Dotted,
        Underline::Dashed,
    ];
}

/// SGR rendition flags and the underline style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Attrs(u16);

impl Attrs {
    pub const BOLD: Attrs = Attrs(1);
    pub const DIM: Attrs = Attrs(1 << 1);
    pub const ITALIC: Attrs = Attrs(1 << 2);
    pub const BLINK: Attrs = Attrs(1 << 3);
    pub const INVERSE: Attrs = Attrs(1 << 4);
    pub const HIDDEN: Attrs = Attrs(1 << 5);
    pub const STRIKE: Attrs = Attrs(1 << 6);
    pub const OVERLINE: Attrs = Attrs(1 << 7);
    const UNDERLINE_SHIFT: u16 = 8;
    const UNDERLINE: u16 = 0b111 << Self::UNDERLINE_SHIFT;
    /// Everything a program can set; the bits above are the cell's layout.
    const RENDITION: u16 = (1 << 11) - 1;

    pub fn contains(self, other: Attrs) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Attrs) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Attrs) {
        self.0 &= !other.0;
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn underline(self) -> Underline {
        Underline::ALL[usize::from((self.0 & Self::UNDERLINE) >> Self::UNDERLINE_SHIFT)]
    }

    pub fn set_underline(&mut self, underline: Underline) {
        self.0 = self.0 & !Self::UNDERLINE | (underline as u16) << Self::UNDERLINE_SHIFT;
    }
}

/// Where a cell stands in a wide character, kept in the bits above the
/// rendition.
const WIDE: u16 = 1 << 11;
const SPACER: u16 = 1 << 12;
/// The blank left at the end of a wrapped row when the wide character that
/// came next did not fit.
const PAD: u16 = 1 << 13;

/// One grid cell: the first character of its grapheme cluster, colours,
/// rendition and layout. The rest of a multi-character cluster, an
/// underline colour and a hyperlink live in the row (`Row::cluster`,
/// `Row::underline_color`, `Row::hyperlink`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub(crate) ch: char,
    pub(crate) fg: Color,
    pub(crate) bg: Color,
    attrs: u16,
    /// One past the index of the row's side entry for this cell; 0 for none.
    pub(crate) extra: u16,
}

impl Default for Cell {
    fn default() -> Self {
        Cell::blank(Color::Default)
    }
}

impl Cell {
    pub(crate) const fn blank(bg: Color) -> Cell {
        Cell { ch: ' ', fg: Color::Default, bg, attrs: 0, extra: 0 }
    }

    pub(crate) fn new(ch: char, fg: Color, bg: Color, attrs: Attrs, extra: u16) -> Cell {
        Cell { ch, fg, bg, attrs: attrs.0, extra }
    }

    /// The cluster's first character; `' '` for a blank or a spacer.
    pub fn ch(&self) -> char {
        self.ch
    }

    pub fn fg(&self) -> Color {
        self.fg
    }

    pub fn bg(&self) -> Color {
        self.bg
    }

    pub fn attrs(&self) -> Attrs {
        Attrs(self.attrs & Attrs::RENDITION)
    }

    /// Columns this cell starts: 2 for a wide character, 0 for the spacer
    /// that follows one, else 1.
    pub fn width(&self) -> u8 {
        match self.attrs & (WIDE | SPACER) {
            WIDE => 2,
            SPACER => 0,
            _ => 1,
        }
    }

    pub fn is_wide(&self) -> bool {
        self.attrs & WIDE != 0
    }

    /// The right half of a wide character.
    pub fn is_spacer(&self) -> bool {
        self.attrs & SPACER != 0
    }

    /// The blank ending a wrapped row whose next wide character did not fit.
    pub fn is_pad(&self) -> bool {
        self.attrs & PAD != 0
    }

    /// Erased or never written: a space in the default rendition.
    pub fn is_default(&self) -> bool {
        *self == Cell::default()
    }

    pub(crate) fn set_wide(&mut self) {
        self.attrs |= WIDE;
    }

    pub(crate) fn set_spacer(&mut self) {
        self.attrs |= SPACER;
    }

    pub(crate) fn set_pad(&mut self) {
        self.attrs |= PAD;
    }

    /// Drop the wide-character layout, leaving a one-column space in the
    /// same rendition: the half that survives when the other is overwritten.
    /// The row drops its cluster (`Row::orphan`).
    pub(crate) fn orphan(&mut self) {
        self.ch = ' ';
        self.narrow();
    }

    /// Drop the layout, keeping the character.
    pub(crate) fn narrow(&mut self) {
        self.attrs &= Attrs::RENDITION;
    }

    /// Nothing a reader would see or copy: a space with no colour, no
    /// visible decoration and no layout. Bold, dim, italic, blink, hidden
    /// and overline on a space show nothing.
    pub(crate) fn is_empty(&self) -> bool {
        const VISIBLE: u16 =
            Attrs::INVERSE.0 | Attrs::STRIKE.0 | Attrs::UNDERLINE | WIDE | SPACER | PAD;
        self.ch == ' '
            && self.fg == Color::Default
            && self.bg == Color::Default
            && self.attrs & VISIBLE == 0
    }

    pub(crate) fn clear_pad(&mut self) {
        self.attrs &= !PAD;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_is_sixteen_bytes() {
        assert_eq!(std::mem::size_of::<Cell>(), 16);
    }

    #[test]
    fn underline_styles_round_trip_without_touching_flags() {
        let mut attrs = Attrs::BOLD;
        for style in Underline::ALL {
            attrs.set_underline(style);
            assert_eq!(attrs.underline(), style);
            assert!(attrs.contains(Attrs::BOLD));
        }
    }
}
