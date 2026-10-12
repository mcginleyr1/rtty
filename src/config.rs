use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::WindowAppearance;
use serde::Deserialize;

use crate::scheme::{self, Scheme};

/// `config.toml`. Every key is optional; an unknown key is an error.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub font: Option<String>,
    pub size: Option<f32>,
    pub ligatures: Option<bool>,
    scheme: Option<String>,
    scheme_dark: Option<String>,
    scheme_light: Option<String>,
}

/// The scheme for each window appearance.
#[derive(Clone, Debug)]
pub enum Theme {
    Follow { dark: Arc<Scheme>, light: Arc<Scheme> },
    Fixed(Arc<Scheme>),
}

/// `$XDG_CONFIG_HOME`, else `~/.config`.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
}

impl Config {
    /// A missing file is the defaults.
    pub fn load(path: &Path) -> Result<Config, String> {
        match fs::read_to_string(path) {
            Ok(source) => {
                toml::from_str(&source).map_err(|err| format!("{}: {err}", path.display()))
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(Config::default()),
            Err(err) => Err(format!("{}: {err}", path.display())),
        }
    }

    /// `scheme` pins one scheme; otherwise `scheme_dark` and `scheme_light` follow the OS.
    pub fn theme(&self, schemes: &[Scheme]) -> Result<Theme, String> {
        match (&self.scheme, &self.scheme_dark, &self.scheme_light) {
            (Some(name), None, None) => Ok(Theme::Fixed(scheme::find(schemes, name)?)),
            (Some(_), _, _) => Err("set `scheme` or `scheme_dark`/`scheme_light`, not both".into()),
            (None, dark, light) => Ok(Theme::Follow {
                dark: scheme::find(schemes, dark.as_deref().unwrap_or("OneDark Pro"))?,
                light: scheme::find(schemes, light.as_deref().unwrap_or("Solarized Light"))?,
            }),
        }
    }
}

impl Theme {
    pub fn for_appearance(&self, appearance: WindowAppearance) -> &Arc<Scheme> {
        match (self, appearance) {
            (Theme::Fixed(scheme), _) => scheme,
            (
                Theme::Follow { dark, .. },
                WindowAppearance::Dark | WindowAppearance::VibrantDark,
            ) => dark,
            (
                Theme::Follow { light, .. },
                WindowAppearance::Light | WindowAppearance::VibrantLight,
            ) => light,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(source: &str) -> Result<Theme, String> {
        toml::from_str::<Config>(source).map_err(|err| err.to_string())?.theme(&scheme::load(&[])?)
    }

    #[test]
    fn the_appearance_picks_the_scheme() {
        let defaults = theme("").unwrap();
        assert_eq!(defaults.for_appearance(WindowAppearance::Dark).name, "OneDark Pro");
        assert_eq!(defaults.for_appearance(WindowAppearance::VibrantDark).name, "OneDark Pro");
        assert_eq!(defaults.for_appearance(WindowAppearance::Light).name, "Solarized Light");
        assert_eq!(defaults.for_appearance(WindowAppearance::VibrantLight).name, "Solarized Light");

        let pair = theme("scheme_dark = 'Solarized Dark'\nscheme_light = 'One Light'").unwrap();
        assert_eq!(pair.for_appearance(WindowAppearance::Dark).name, "Solarized Dark");
        assert_eq!(pair.for_appearance(WindowAppearance::Light).name, "One Light");

        let fixed = theme("scheme = 'One Light'").unwrap();
        assert_eq!(fixed.for_appearance(WindowAppearance::Dark).name, "One Light");
        assert_eq!(fixed.for_appearance(WindowAppearance::Light).name, "One Light");
    }

    #[test]
    fn mistakes_are_errors_that_name_them() {
        assert!(theme("colour = 'red'").unwrap_err().contains("unknown field `colour`"));
        assert!(theme("scheme_dark = 'Nope'").unwrap_err().contains("\"Nope\""));
        assert!(
            theme("scheme = 'One Light'\nscheme_dark = 'OneDark Pro'")
                .unwrap_err()
                .contains("not both")
        );
    }

    #[test]
    fn font_settings_read_as_written() {
        let config: Config =
            toml::from_str("font = 'FiraCode Nerd Font'\nsize = 12\nligatures = false").unwrap();
        assert_eq!(config.font.as_deref(), Some("FiraCode Nerd Font"));
        assert_eq!(config.size, Some(12.));
        assert_eq!(config.ligatures, Some(false));
    }
}
