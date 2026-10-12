use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;

use rtty_vt::{BACKGROUND, CURSOR, FOREGROUND, Rgb};
use serde::Deserialize;

const BUILT_IN: [&str; 4] = [
    include_str!("../schemes/onedark-pro.toml"),
    include_str!("../schemes/one-light.toml"),
    include_str!("../schemes/solarized-dark.toml"),
    include_str!("../schemes/solarized-light.toml"),
];

/// The palette slots rtty-vt names: 256 indexed colors, then foreground, background and cursor.
pub const SLOTS: usize = CURSOR + 1;

/// A color scheme, expanded into the terminal's whole palette (16 ANSI, the 6×6×6 cube, 24
/// grays, then foreground, background and cursor). Cells keep color indices, so a frame drawn
/// with another scheme recolors them.
#[derive(Clone, Debug, PartialEq)]
pub struct Scheme {
    pub name: String,
    /// The file it was read from; `None` for the built-ins.
    pub path: Option<PathBuf>,
    table: [Rgb; SLOTS],
    pub cursor_fg: Rgb,
    pub cursor_border: Rgb,
    /// Foreground and background of selected cells; `None` inverts them.
    pub selection: Option<(Rgb, Rgb)>,
}

/// WezTerm's color scheme file. Keys rtty does not draw (`tab_bar`, `scrollbar_thumb`, …) are
/// ignored so WezTerm's files load unchanged.
#[derive(Deserialize)]
struct SchemeFile {
    colors: Palette,
    #[serde(default)]
    metadata: Metadata,
}

#[derive(Deserialize, Default)]
struct Metadata {
    name: Option<String>,
}

#[derive(Deserialize)]
struct Palette {
    foreground: Hex,
    background: Hex,
    cursor_bg: Option<Hex>,
    cursor_fg: Option<Hex>,
    cursor_border: Option<Hex>,
    selection_bg: Option<Hex>,
    selection_fg: Option<Hex>,
    ansi: [Hex; 8],
    brights: [Hex; 8],
    #[serde(default)]
    indexed: HashMap<String, Hex>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(try_from = "String")]
struct Hex(Rgb);

impl TryFrom<String> for Hex {
    type Error = String;

    fn try_from(value: String) -> Result<Self, String> {
        parse_color(&value)
            .map(Hex)
            .ok_or_else(|| format!("invalid color {value:?}, expected #rgb, #rrggbb or #rrggbbaa"))
    }
}

impl Scheme {
    /// `path` names the file in errors and gives the name when the file's metadata has none.
    pub fn parse(source: &str, path: Option<PathBuf>) -> Result<Scheme, String> {
        let origin =
            path.as_ref().map_or("built-in scheme".into(), |path| path.display().to_string());
        let file: SchemeFile = toml::from_str(source).map_err(|err| format!("{origin}: {err}"))?;
        let stem =
            path.as_ref().and_then(|path| path.file_stem()).map(|stem| stem.to_string_lossy());
        let name = file.metadata.name.or(stem.map(String::from)).expect("built-ins have names");
        let colors = file.colors;
        let foreground = colors.foreground.0;
        let mut table: [Rgb; SLOTS] = std::array::from_fn(xterm_256);
        for (slot, color) in colors.ansi.iter().chain(&colors.brights).enumerate() {
            table[slot] = color.0;
        }
        for (key, color) in colors.indexed {
            let index: u8 = key.parse().map_err(|_| {
                format!("{origin}: colors.indexed: key {key:?} is not a color index 0-255")
            })?;
            table[usize::from(index)] = color.0;
        }
        let cursor = colors.cursor_bg.map_or(foreground, |color| color.0);
        table[FOREGROUND] = foreground;
        table[BACKGROUND] = colors.background.0;
        table[CURSOR] = cursor;
        Ok(Scheme {
            name,
            path,
            table,
            cursor_fg: colors.cursor_fg.map_or(colors.background.0, |color| color.0),
            cursor_border: colors.cursor_border.map_or(cursor, |color| color.0),
            selection: colors.selection_fg.zip(colors.selection_bg).map(|(fg, bg)| (fg.0, bg.0)),
        })
    }

    /// Palette slot `index`: 0–255, `FOREGROUND`, `BACKGROUND` or `CURSOR`.
    pub fn color(&self, index: usize) -> Rgb {
        self.table[index]
    }

    /// Dark when its background contrasts more with white than with black: relative luminance
    /// `l` against white is 1.05 / (l + 0.05), against black (l + 0.05) / 0.05.
    pub fn is_dark(&self) -> bool {
        let l = luminance(self.table[BACKGROUND]) + 0.05;
        1.05 / l > l / 0.05
    }
}

/// The built-ins, then every `*.toml` in each directory in order; a missing directory is skipped,
/// a malformed file is an error naming it and the key.
pub fn load(dirs: &[PathBuf]) -> Result<Vec<Scheme>, String> {
    let mut schemes =
        BUILT_IN.iter().map(|source| Scheme::parse(source, None)).collect::<Result<Vec<_>, _>>()?;
    for dir in dirs {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => return Err(format!("{}: {err}", dir.display())),
        };
        let mut paths = entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| format!("{}: {err}", dir.display()))?;
        paths.retain(|path| path.extension().is_some_and(|extension| extension == "toml"));
        paths.sort();
        for path in paths {
            let source =
                fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
            schemes.push(Scheme::parse(&source, Some(path))?);
        }
    }
    Ok(schemes)
}

/// The first scheme with this name, else the first whose file stem is this name.
pub fn find(schemes: &[Scheme], name: &str) -> Result<Arc<Scheme>, String> {
    let stem_is = |scheme: &&Scheme| {
        scheme.path.as_ref().and_then(|path| path.file_stem()).is_some_and(|stem| stem == name)
    };
    let found =
        schemes.iter().find(|scheme| scheme.name == name).or_else(|| schemes.iter().find(stem_is));
    found.map(|scheme| Arc::new(scheme.clone())).ok_or_else(|| {
        let known: Vec<&str> = schemes.iter().map(|scheme| scheme.name.as_str()).collect();
        format!("no color scheme named {name:?}; known: {}", known.join(", "))
    })
}

/// SGR 2 (faint): two thirds of the color.
pub fn dim(Rgb(r, g, b): Rgb) -> Rgb {
    let third = |channel: u8| (f32::from(channel) * 0.66) as u8;
    Rgb(third(r), third(g), third(b))
}

/// WCAG relative luminance, 0 for black to 1 for white.
fn luminance(Rgb(r, g, b): Rgb) -> f32 {
    let linear = |channel: u8| {
        let c = f32::from(channel) / 255.;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

/// xterm's 6×6×6 cube and gray ramp for 16..256; the other slots are filled from the scheme.
fn xterm_256(index: usize) -> Rgb {
    let level = |v: usize| if v == 0 { 0 } else { 55 + 40 * v as u8 };
    match index {
        16..232 => {
            let i = index - 16;
            Rgb(level(i / 36), level(i / 6 % 6), level(i % 6))
        }
        232..256 => {
            let v = 8 + 10 * (index - 232) as u8;
            Rgb(v, v, v)
        }
        _ => Rgb(0, 0, 0),
    }
}

fn parse_color(value: &str) -> Option<Rgb> {
    let hex = value.strip_prefix('#').filter(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))?;
    let channel = |at: usize, width: usize| u8::from_str_radix(&hex[at..at + width], 16).ok();
    match hex.len() {
        3 => Some(Rgb(channel(0, 1)? * 17, channel(1, 1)? * 17, channel(2, 1)? * 17)),
        6 | 8 => Some(Rgb(channel(0, 2)?, channel(2, 2)?, channel(4, 2)?)),
        _ => None,
    }
}

#[cfg(test)]
pub fn rgb(value: u32) -> Rgb {
    Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEZTERM_FIXTURE: &str = include_str!("../tests/fixtures/onedarkpro_onedark.toml");

    #[test]
    fn every_built_in_parses_with_source_and_license() {
        let schemes = load(&[]).unwrap();
        let names: Vec<&str> = schemes.iter().map(|scheme| scheme.name.as_str()).collect();
        assert_eq!(names, ["OneDark Pro", "One Light", "Solarized Dark", "Solarized Light"]);
        for source in BUILT_IN {
            assert!(source.contains("license = \"MIT\"") && source.contains("origin_url = "));
        }
        let dark = find(&schemes, "OneDark Pro").unwrap();
        assert!(dark.is_dark());
        assert_eq!(dark.color(BACKGROUND), rgb(0x282c34));
        assert_eq!(dark.color(214), rgb(0xd19a66));
        assert!(!find(&schemes, "Solarized Light").unwrap().is_dark());
        assert!(find(&schemes, "Solarized Dark").unwrap().is_dark());
        assert!(!find(&schemes, "One Light").unwrap().is_dark());
    }

    #[test]
    fn a_wezterm_file_loads_unchanged() {
        // onedarkpro.nvim's own WezTerm file, tab bar tables and all.
        let path = PathBuf::from("/colors/onedarkpro.toml");
        let scheme = Scheme::parse(WEZTERM_FIXTURE, Some(path)).unwrap();
        assert_eq!(scheme.name, "onedarkpro_onedark");
        assert_eq!(scheme.color(1), rgb(0xe06c75));
        assert_eq!(scheme.color(8), rgb(0x5c6370));
        assert_eq!(scheme.color(100), rgb(0xe2be7d));
        assert_eq!(scheme.color(196), rgb(0xff0000));
        assert_eq!(scheme.color(CURSOR), rgb(0xc678dd));
        assert_eq!(scheme.cursor_fg, rgb(0x282c34));
        assert_eq!(scheme.selection, Some((rgb(0xabb2bf), rgb(0xc678dd))));
        let schemes = [scheme];
        assert_eq!(find(&schemes, "onedarkpro_onedark").unwrap().name, "onedarkpro_onedark");
        assert_eq!(find(&schemes, "onedarkpro").unwrap().name, "onedarkpro_onedark");
        assert!(find(&schemes, "Gruvbox").unwrap_err().contains("\"Gruvbox\""));
    }

    #[test]
    fn a_file_without_a_name_is_named_after_its_stem() {
        let source = WEZTERM_FIXTURE.replace("name = \"onedarkpro_onedark\"", "");
        let scheme = Scheme::parse(&source, Some(PathBuf::from("/colors/mine.toml"))).unwrap();
        assert_eq!(scheme.name, "mine");
    }

    #[test]
    fn a_malformed_scheme_names_the_file_and_key() {
        let path = || Some(PathBuf::from("/colors/broken.toml"));
        let bad_color =
            WEZTERM_FIXTURE.replace("background = \"#282c34\"", "background = \"#28zc34\"");
        let err = Scheme::parse(&bad_color, path()).unwrap_err();
        assert!(err.starts_with("/colors/broken.toml"), "{err}");
        assert!(err.contains("background") && err.contains("#28zc34"), "{err}");

        let short_ansi = WEZTERM_FIXTURE.replace("  \"#abb2bf\",\n]\nbackground", "]\nbackground");
        let err = Scheme::parse(&short_ansi, path()).unwrap_err();
        assert!(err.contains("ansi") && err.contains("length 7"), "{err}");

        let no_foreground = WEZTERM_FIXTURE.replace("foreground = \"#abb2bf\"", "");
        let err = Scheme::parse(&no_foreground, path()).unwrap_err();
        assert!(err.contains("missing field `foreground`"), "{err}");

        let bad_index = WEZTERM_FIXTURE.replace("59 = ", "fifty-nine = ");
        let err = Scheme::parse(&bad_index, path()).unwrap_err();
        assert!(err.contains("colors.indexed") && err.contains("fifty-nine"), "{err}");
    }

    #[test]
    fn colors_parse_in_hex_forms() {
        assert_eq!(parse_color("#abc"), Some(rgb(0xaabbcc)));
        assert_eq!(parse_color("#a1b2c3"), Some(rgb(0xa1b2c3)));
        assert_eq!(parse_color("#a1b2c3ff"), Some(rgb(0xa1b2c3)));
        assert_eq!(parse_color("a1b2c3"), None);
        assert_eq!(parse_color("#a1b2c"), None);
        assert_eq!(parse_color("#+1b2c3"), None);
    }

    #[test]
    fn cube_and_gray_ramp_span_black_to_white() {
        assert_eq!(xterm_256(16), rgb(0x000000));
        assert_eq!(xterm_256(231), rgb(0xffffff));
        assert_eq!(xterm_256(232), rgb(0x080808));
        assert_eq!(xterm_256(255), rgb(0xeeeeee));
    }
}
