use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, Rgb};

const ANSI: [u32; 16] = [
    0x181818, 0xac4242, 0x90a959, 0xf4bf75, 0x6a9fb5, 0xaa759f, 0x75b5aa, 0xd8d8d8, //
    0x6b6b6b, 0xc55555, 0xaac474, 0xfeca88, 0x82b8c8, 0xc28cb8, 0x93d3c3, 0xf8f8f8,
];
const FOREGROUND: u32 = 0xd8d8d8;
const BACKGROUND: u32 = 0x181818;
const CURSOR: u32 = 0xc0c0c0;

/// The color a terminal color index has until the program overrides it (OSC 4/10/11/12).
/// Indices follow alacritty's layout: 16 ANSI, 216 cube, 24 gray, then the named slots.
pub fn default(index: usize) -> Rgb {
    let level = |v: usize| if v == 0 { 0 } else { 55 + 40 * v as u8 };
    match index {
        0..16 => hex(ANSI[index]),
        16..232 => {
            let i = index - 16;
            Rgb { r: level(i / 36), g: level(i / 6 % 6), b: level(i % 6) }
        }
        232..256 => {
            let v = 8 + 10 * (index - 232) as u8;
            Rgb { r: v, g: v, b: v }
        }
        256 | 267 => hex(FOREGROUND),
        257 => hex(BACKGROUND),
        258 => hex(CURSOR),
        259..267 => dim(default(index - 259)),
        268 => dim(hex(FOREGROUND)),
        _ => panic!("terminal color index {index} out of range"),
    }
}

pub fn resolve(color: Color, overrides: &Colors) -> Rgb {
    let index = match color {
        Color::Spec(rgb) => return rgb,
        Color::Named(named) => named as usize,
        Color::Indexed(index) => index as usize,
    };
    overrides[index].unwrap_or_else(|| default(index))
}

pub fn dim(rgb: Rgb) -> Rgb {
    rgb * 0.66
}

fn hex(value: u32) -> Rgb {
    Rgb { r: (value >> 16) as u8, g: (value >> 8) as u8, b: value as u8 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::NamedColor;

    #[test]
    fn cube_and_gray_ramp_span_black_to_white() {
        assert_eq!(default(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(default(231), Rgb { r: 255, g: 255, b: 255 });
        assert_eq!(default(196), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(default(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(default(255), Rgb { r: 238, g: 238, b: 238 });
    }

    #[test]
    fn program_overrides_win_over_defaults() {
        let mut overrides = Colors::default();
        let red = Rgb { r: 255, g: 0, b: 0 };
        overrides[NamedColor::Background] = Some(red);
        assert_eq!(resolve(Color::Named(NamedColor::Background), &overrides), red);
        assert_eq!(resolve(Color::Indexed(1), &overrides), hex(ANSI[1]));
        assert_eq!(resolve(Color::Spec(red), &Colors::default()), red);
    }
}
