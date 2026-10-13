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
    /// The line between split panes.
    pub split: Rgb,
    /// The tab bar's background, and the foreground and background of its tabs: the one shown,
    /// the others, another under the pointer, and the + button, plain and under the pointer.
    pub tab_bar: Rgb,
    pub active_tab: (Rgb, Rgb),
    pub inactive_tab: (Rgb, Rgb),
    pub inactive_tab_hover: (Rgb, Rgb),
    pub new_tab: (Rgb, Rgb),
    pub new_tab_hover: (Rgb, Rgb),
}

/// WezTerm's color scheme file. Keys rtty does not draw (`scrollbar_thumb`, the tab bar's
/// `inactive_tab_edge`, …) are ignored so WezTerm's files load unchanged.
#[derive(Deserialize)]
struct SchemeFile {
    colors: Palette,
    #[serde(default)]
    metadata: Metadata,
}

#[derive(Deserialize, Default)]
struct TabBar {
    background: Option<Hex>,
    active_tab: Option<Tab>,
    inactive_tab: Option<Tab>,
    inactive_tab_hover: Option<Tab>,
    new_tab: Option<Tab>,
    new_tab_hover: Option<Tab>,
}

#[derive(Deserialize, Clone, Copy)]
struct Tab {
    fg_color: Hex,
    bg_color: Hex,
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
    split: Option<Hex>,
    #[serde(default)]
    tab_bar: TabBar,
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
        let background = colors.background.0;
        table[FOREGROUND] = foreground;
        table[BACKGROUND] = background;
        table[CURSOR] = cursor;
        // What a scheme leaves out of the window's chrome is mixed from its foreground and
        // background: the bar is the background, a shade darker in a light scheme; the active
        // tab is the pane's own colors, the others recede. A tab that should stand out (the one
        // shown, one under the pointer) but has the bar's own background, as in WezTerm's flat
        // tab bars, is lifted from the bar: toward the foreground in a dark scheme, toward white
        // in a light one, so it is lighter either way.
        let dark = is_dark(background);
        let toward = |share: f32| mix(background, foreground, share);
        let bar = colors.tab_bar;
        let tab_bar =
            bar.background.map_or(if dark { background } else { toward(0.08) }, |color| color.0);
        let tab =
            |tab: Option<Tab>, default| tab.map_or(default, |tab| (tab.fg_color.0, tab.bg_color.0));
        let lift = |(fg, bg): (Rgb, Rgb), strength: f32| {
            let lifted = if dark {
                mix(bg, foreground, 0.12 * strength)
            } else {
                mix(bg, Rgb(255, 255, 255), 0.7 * strength)
            };
            (fg, if bg == tab_bar { lifted } else { bg })
        };
        let active_tab = lift(tab(bar.active_tab, (foreground, background)), 1.);
        let inactive_tab = tab(bar.inactive_tab, (toward(0.55), tab_bar));
        let hover = (inactive_tab.0, mix(inactive_tab.1, active_tab.1, 0.5));
        let inactive_tab_hover = lift(tab(bar.inactive_tab_hover, hover), 0.5);
        Ok(Scheme {
            name,
            path,
            table,
            cursor_fg: colors.cursor_fg.map_or(background, |color| color.0),
            cursor_border: colors.cursor_border.map_or(cursor, |color| color.0),
            selection: colors.selection_fg.zip(colors.selection_bg).map(|(fg, bg)| (fg.0, bg.0)),
            split: colors.split.map_or(toward(0.3), |color| color.0),
            tab_bar,
            active_tab,
            inactive_tab,
            inactive_tab_hover,
            new_tab: tab(bar.new_tab, inactive_tab),
            new_tab_hover: lift(tab(bar.new_tab_hover, inactive_tab_hover), 0.5),
        })
    }

    /// Palette slot `index`: 0–255, `FOREGROUND`, `BACKGROUND` or `CURSOR`.
    pub fn color(&self, index: usize) -> Rgb {
        self.table[index]
    }

    pub fn is_dark(&self) -> bool {
        is_dark(self.table[BACKGROUND])
    }
}

/// Dark when a background contrasts more with white than with black: relative luminance `l`
/// against white is 1.05 / (l + 0.05), against black (l + 0.05) / 0.05.
fn is_dark(background: Rgb) -> bool {
    let l = luminance(background) + 0.05;
    1.05 / l > l / 0.05
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

/// `share` of the way from `from` to `to`.
fn mix(Rgb(r1, g1, b1): Rgb, Rgb(r2, g2, b2): Rgb, share: f32) -> Rgb {
    let channel =
        |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * share).round() as u8;
    Rgb(channel(r1, r2), channel(g1, g2), channel(b1, b2))
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
        // A flat tab bar: the shown tab and the hovered one are lifted from it, lighter; the
        // + under the pointer keeps the scheme's own colors.
        let (bar, fg) = (rgb(0x282c34), rgb(0xabb2bf));
        assert_eq!(scheme.tab_bar, bar);
        assert_eq!(scheme.active_tab, (rgb(0xc678dd), mix(bar, fg, 0.12)));
        assert_eq!(scheme.inactive_tab, (rgb(0x5c6370), bar));
        assert_eq!(scheme.inactive_tab_hover, (rgb(0xc678dd), mix(bar, fg, 0.06)));
        assert_eq!(scheme.new_tab, (rgb(0xc678dd), bar));
        assert_eq!(scheme.new_tab_hover, (bar, rgb(0xc678dd)));
        let schemes = [scheme];
        assert_eq!(find(&schemes, "onedarkpro_onedark").unwrap().name, "onedarkpro_onedark");
        assert_eq!(find(&schemes, "onedarkpro").unwrap().name, "onedarkpro_onedark");
        assert!(find(&schemes, "Gruvbox").unwrap_err().contains("\"Gruvbox\""));
    }

    #[test]
    fn chrome_a_scheme_leaves_out_is_mixed_from_its_colors() {
        // Light: the bar a shade darker than the pane, the shown tab the pane's own colors.
        let light = find(&load(&[]).unwrap(), "Solarized Light").unwrap();
        let (fg, bg) = (light.color(FOREGROUND), light.color(BACKGROUND));
        assert_eq!((fg, bg), (rgb(0x657b83), rgb(0xfdf6e3)));
        let bar = mix(bg, fg, 0.08);
        assert_eq!(light.tab_bar, bar);
        assert_eq!(light.active_tab, (fg, bg));
        assert_eq!(light.inactive_tab, (mix(bg, fg, 0.55), bar));
        assert_eq!(light.inactive_tab_hover, (mix(bg, fg, 0.55), mix(bar, bg, 0.5)));
        assert_eq!(light.new_tab, light.inactive_tab);
        assert_eq!(light.new_tab_hover, light.inactive_tab_hover);
        assert_eq!(light.split, mix(bg, fg, 0.3));
        // Dark: the bar is the pane's background, so the shown tab is lifted, lighter.
        let dark = find(&load(&[]).unwrap(), "Solarized Dark").unwrap();
        let (fg, bg) = (dark.color(FOREGROUND), dark.color(BACKGROUND));
        assert_eq!(dark.tab_bar, bg);
        assert_eq!(dark.active_tab, (fg, mix(bg, fg, 0.12)));
        assert_eq!(dark.inactive_tab, (mix(bg, fg, 0.55), bg));
        assert_eq!(dark.inactive_tab_hover.1, mix(bg, mix(bg, fg, 0.12), 0.5));
        assert_eq!(mix(rgb(0x000000), rgb(0xffffff), 0.5), rgb(0x808080));
        let split = WEZTERM_FIXTURE.replacen("[colors]\n", "[colors]\nsplit = \"#123456\"\n", 1);
        assert_eq!(Scheme::parse(&split, None).unwrap().split, rgb(0x123456));
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
