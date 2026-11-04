use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{Keystroke, WindowAppearance};
use serde::Deserialize;
use toml::Spanned;

use crate::layout::Direction;
use crate::scheme::{self, Scheme};

/// `config.toml`. Every key is optional; an unknown key is an error.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub font: Option<String>,
    /// Families for what `font` lacks (icons, symbols), in order; unset, an installed Nerd Font.
    pub fallback: Option<Vec<String>>,
    pub size: Option<f32>,
    pub ligatures: Option<bool>,
    keys: Option<Keys>,
    pub bell: Option<Bell>,
    pub option_as_alt: Option<OptionAsAlt>,
    /// The WCAG contrast ratio, 1 to 21, text keeps with its background.
    pub minimum_contrast: Option<f32>,
    scheme: Option<String>,
    scheme_dark: Option<String>,
    scheme_light: Option<String>,
    /// `[bind]`: chords in GPUI's keystroke syntax, and the names of the actions they run.
    bind: Option<BTreeMap<Spanned<String>, String>>,
    /// Every chord rtty acts on, as GPUI writes it, and its action: the defaults, with `keys`
    /// and `[bind]`.
    #[serde(skip)]
    pub bindings: BTreeMap<String, Action>,
}

/// What a chord can do. `Action::named` gives each its name in `[bind]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Focus(Direction),
    Resize(Direction),
    Swap(Direction),
    SplitRight,
    SplitDown,
    ClosePane,
    LastPane,
    Zoom,
    NewTab,
    NextTab,
    PrevTab,
    /// The tab at this index.
    Tab(usize),
    NewWindow,
    Find,
    Copy,
    Paste,
    FontBigger,
    FontSmaller,
    PromptPrev,
    PromptNext,
    ClearScrollback,
    Quit,
}

impl Action {
    /// The action `[bind]` calls `name`.
    fn named(name: &str) -> Option<Action> {
        use Action::*;
        let toward = |verb: &str| {
            Direction::ALL
                .into_iter()
                .find(|direction| name == format!("{verb}-{}", direction.name()))
        };
        Some(match name {
            "split-right" => SplitRight,
            "split-down" => SplitDown,
            "close-pane" => ClosePane,
            "last-pane" => LastPane,
            "zoom" => Zoom,
            "new-tab" => NewTab,
            "next-tab" => NextTab,
            "prev-tab" => PrevTab,
            "new-window" => NewWindow,
            "find" => Find,
            "copy" => Copy,
            "paste" => Paste,
            "font-bigger" => FontBigger,
            "font-smaller" => FontSmaller,
            "prompt-prev" => PromptPrev,
            "prompt-next" => PromptNext,
            "clear-scrollback" => ClearScrollback,
            "quit" => Quit,
            _ => {
                return toward("focus")
                    .map(Focus)
                    .or_else(|| toward("resize").map(Resize))
                    .or_else(|| toward("swap").map(Swap))
                    .or_else(|| (1..=9).find(|n| name == format!("tab-{n}")).map(|n| Tab(n - 1)));
            }
        })
    }
}

/// `text` as GPUI writes the chord, when it is one: modifiers (`ctrl`, `alt`, `shift`, `cmd`,
/// `fn`) and a key GPUI reports, a character or a named key, joined by `-`.
fn chord(text: &str) -> Option<String> {
    let named = "space tab enter backspace escape up down left right pageup pagedown home end \
                 insert delete";
    let keystroke = Keystroke::parse(text).ok()?;
    let key = keystroke.key.as_str();
    let character = key.chars().count() == 1 && !key.trim().is_empty();
    let function = (1..=35).any(|n| key == format!("f{n}"));
    let named = named.split_whitespace().any(|name| name == key);
    (character || function || named).then(|| keystroke.unparse())
}

/// rtty's own chords and, with `keys = "tutti"`, tutti's direct keys.
fn defaults(keys: Keys) -> BTreeMap<String, Action> {
    use Action::*;
    let fixed = [
        ("cmd-q", Quit),
        ("cmd-c", Copy),
        ("cmd-v", Paste),
        ("cmd-f", Find),
        ("cmd-up", PromptPrev),
        ("cmd-down", PromptNext),
        ("cmd-k", ClearScrollback),
        ("cmd-=", FontBigger),
        ("cmd--", FontSmaller),
        ("cmd-n", NewWindow),
        ("cmd-t", NewTab),
        ("cmd-w", ClosePane),
        ("cmd-d", SplitRight),
        ("cmd-shift-d", SplitDown),
        ("cmd-shift-enter", Zoom),
        // Cmd+Shift+[ and ], as macOS reports them.
        ("cmd-{", PrevTab),
        ("cmd-}", NextTab),
    ];
    let fixed = fixed.into_iter().map(|(chord, action)| (chord.to_owned(), action));
    let arrows = Direction::ALL.into_iter().flat_map(|direction| {
        let key = direction.name();
        [
            (format!("cmd-alt-{key}"), Focus(direction)),
            (format!("cmd-ctrl-{key}"), Resize(direction)),
        ]
    });
    let tabs = (1..=9).map(|digit| (format!("cmd-{digit}"), Tab(digit - 1)));
    let vim = [("h", Direction::Left), ("j", Direction::Down), ("k", Direction::Up)];
    let vim = vim.into_iter().chain([("l", Direction::Right)]).filter(|_| keys == Keys::Tutti);
    let tutti = vim.flat_map(|(key, direction)| {
        [(format!("ctrl-{key}"), Focus(direction)), (format!("alt-{key}"), Resize(direction))]
    });
    let all = fixed.chain(arrows).chain(tabs).chain(tutti);
    all.map(|(text, action)| (chord(&text).expect("rtty's chords parse"), action)).collect()
}

/// Which keys move between panes and resize them.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Keys {
    /// Cmd+Opt+arrows and Cmd+Ctrl+arrows: every Ctrl key stays with the programs.
    #[default]
    Rtty,
    /// Also tutti's direct keys, Ctrl+h/j/k/l and Alt+h/j/k/l, unless the pane claims them.
    Tutti,
}

/// What BEL does.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Bell {
    /// The pane flashes, and the dock asks for attention while the window is not focused.
    #[default]
    Visual,
    None,
}

/// Which Option keys are Alt (an ESC prefix, and `alt-` bindings) when they type text; the
/// others type what macOS gives them (é, €, dead keys) and are Alt only for keys that type none.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OptionAsAlt {
    #[default]
    Both,
    Left,
    Right,
    None,
}

impl OptionAsAlt {
    /// Whether Option, held on the left side, the right or (as an event that names neither
    /// says) on no side known, is Alt.
    pub fn alt(self, left: bool, right: bool) -> bool {
        match self {
            OptionAsAlt::Both => true,
            OptionAsAlt::Left => left || !right,
            OptionAsAlt::Right => right || !left,
            OptionAsAlt::None => false,
        }
    }
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
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(err) if err.kind() == ErrorKind::NotFound => String::new(),
            Err(err) => return Err(format!("{}: {err}", path.display())),
        };
        Config::parse(&source).map_err(|err| format!("{}: {err}", path.display()))
    }

    /// A config file's text. `[bind]` adds bindings and replaces them, and `none` unbinds a
    /// chord; a mistake there names its chord and line.
    pub fn parse(source: &str) -> Result<Config, String> {
        let mut config: Config = toml::from_str(source).map_err(|err| err.to_string())?;
        if let Some(ratio) = config.minimum_contrast.filter(|ratio| !(1. ..=21.).contains(ratio)) {
            return Err(format!("minimum_contrast ={ratio}: a contrast ratio from 1 to 21"));
        }
        config.bindings = defaults(config.keys.unwrap_or_default());
        let mut lines = HashMap::new();
        for (text, name) in config.bind.iter().flatten() {
            let line = source[..text.span().start].matches('\n').count() + 1;
            let error = |what: String| Err(format!("line {line}: {:?}: {what}", text.get_ref()));
            let Some(chord) = chord(text.get_ref()) else {
                return error("not a chord: modifiers and one key, joined by -".into());
            };
            if let Some(other) = lines.insert(chord.clone(), line) {
                return error(format!("the same chord as line {other}, {chord}"));
            }
            if name == "none" {
                if config.bindings.remove(&chord).is_none() {
                    return error(format!("none, but {chord} has no binding to remove"));
                }
            } else {
                let Some(action) = Action::named(name) else {
                    return error(format!("unknown action {name:?}"));
                };
                config.bindings.insert(chord, action);
            }
        }
        Ok(config)
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
        let source = "font = 'Menlo'\nfallback = ['Symbols Nerd Font Mono', 'Hack Nerd Font Mono']\n\
                      size = 12\nligatures = false";
        let config: Config = toml::from_str(source).unwrap();
        assert_eq!(config.font.as_deref(), Some("Menlo"));
        let fallback = config.fallback.unwrap();
        assert_eq!(fallback, ["Symbols Nerd Font Mono", "Hack Nerd Font Mono"]);
        assert_eq!(config.size, Some(12.));
        assert_eq!(config.ligatures, Some(false));
        assert!(toml::from_str::<Config>("fallback = 'Hack'").is_err(), "a list, not a name");
    }

    #[test]
    fn keys_are_rttys_or_tuttis() {
        let keys = |source| toml::from_str::<Config>(source).map(|config| config.keys);
        assert_eq!(keys("").unwrap(), None);
        assert_eq!(keys("keys = 'rtty'").unwrap(), Some(Keys::Rtty));
        assert_eq!(keys("keys = 'tutti'").unwrap(), Some(Keys::Tutti));
        let err = keys("keys = 'vim'").unwrap_err().to_string();
        assert!(err.contains("unknown variant `vim`, expected `rtty` or `tutti`"), "{err}");
    }

    #[test]
    fn bindings_are_the_defaults_then_tuttis_keys_then_bind() {
        use Action::*;
        let bindings = |source| Config::parse(source).unwrap().bindings;
        let defaults = bindings("");
        assert_eq!(defaults.len(), 34);
        assert_eq!(defaults["cmd-d"], SplitRight);
        assert_eq!(defaults["cmd-k"], ClearScrollback);
        assert_eq!(defaults["cmd-{"], PrevTab);
        assert_eq!(defaults["cmd-9"], Tab(8));
        // As GPUI writes chords: ctrl, alt, cmd, shift.
        assert_eq!(defaults["alt-cmd-left"], Focus(Direction::Left));
        assert_eq!(defaults["ctrl-cmd-down"], Resize(Direction::Down));
        assert!(!defaults.contains_key("ctrl-h"));
        let tutti = bindings("keys = 'tutti'");
        assert_eq!(tutti.len(), 42);
        assert_eq!(
            (tutti["ctrl-h"], tutti["alt-l"]),
            (Focus(Direction::Left), Resize(Direction::Right))
        );
        // Modifiers in any order, a capital as Shift; `none` unbinds.
        let mine = bindings(
            "keys = 'tutti'\n[bind]\n'alt-\\' = 'split-right'\n'ctrl-alt-H' = 'zoom'\n\
             'cmd-d' = 'new-tab'\n'shift-cmd-d' = 'none'\n'ctrl-h' = 'none'\n'alt--' = 'split-down'",
        );
        assert_eq!(mine.len(), 42 + 3 - 2);
        assert_eq!((mine["alt-\\"], mine["alt--"]), (SplitRight, SplitDown));
        assert_eq!((mine["ctrl-alt-shift-h"], mine["cmd-d"]), (Zoom, NewTab));
        assert!(!mine.contains_key("cmd-shift-d") && !mine.contains_key("ctrl-h"));
    }

    #[test]
    fn every_action_has_its_name() {
        let names = "focus-left focus-right focus-up focus-down resize-left resize-right \
                     resize-up resize-down swap-left swap-right swap-up swap-down split-right \
                     split-down close-pane last-pane zoom new-tab next-tab prev-tab tab-1 tab-9 \
                     new-window find copy paste font-bigger font-smaller prompt-prev \
                     prompt-next clear-scrollback quit";
        for name in names.split_whitespace() {
            assert!(Action::named(name).is_some(), "{name}");
        }
        assert_eq!(Action::named("resize-up"), Some(Action::Resize(Direction::Up)));
        assert_eq!(Action::named("swap-left"), Some(Action::Swap(Direction::Left)));
        assert_eq!(Action::named("tab-1"), Some(Action::Tab(0)));
        for name in ["tab-0", "tab-10", "focus-", "Quit", "none"] {
            assert_eq!(Action::named(name), None, "{name}");
        }
    }

    #[test]
    fn binding_mistakes_name_their_chord_and_line() {
        let err = |bind: &str| Config::parse(&format!("size = 12\n[bind]\n{bind}")).unwrap_err();
        assert_eq!(err("'alt-x' = 'close'"), "line 3: \"alt-x\": unknown action \"close\"");
        for chord in ["ctrl-a b", "ctrl-x-y", "ctrl", "ctrl-hh", "", "ctrl- "] {
            let err = err(&format!("'{chord}' = 'zoom'"));
            assert!(err.starts_with(&format!("line 3: {chord:?}: not a chord")), "{err}");
        }
        assert_eq!(
            err("'ctrl-h' = 'none'"),
            "line 3: \"ctrl-h\": none, but ctrl-h has no binding to remove"
        );
        assert_eq!(
            err("'alt-ctrl-x' = 'zoom'\n'ctrl-alt-x' = 'quit'"),
            "line 4: \"ctrl-alt-x\": the same chord as line 3, ctrl-alt-x"
        );
        let typed = err("'cmd-d' = 1");
        assert!(typed.contains("line 3") && typed.contains("expected a string"), "{typed}");
    }

    #[test]
    fn the_bell_is_visual_or_none() {
        let bell = |source| toml::from_str::<Config>(source).map(|config| config.bell);
        assert_eq!(bell("").unwrap().unwrap_or_default(), Bell::Visual);
        assert_eq!(bell("bell = 'none'").unwrap(), Some(Bell::None));
        let err = bell("bell = 'audible'").unwrap_err().to_string();
        assert!(err.contains("unknown variant `audible`, expected `visual` or `none`"), "{err}");
    }

    #[test]
    fn minimum_contrast_is_a_ratio_from_1_to_21() {
        let contrast = |source: &str| Config::parse(source).map(|config| config.minimum_contrast);
        assert_eq!(contrast("").unwrap(), None);
        assert_eq!(contrast("minimum_contrast =4.5").unwrap(), Some(4.5));
        assert_eq!(contrast("minimum_contrast =21").unwrap(), Some(21.));
        for wrong in ["0.5", "22", "nan"] {
            let err = contrast(&format!("minimum_contrast ={wrong}")).unwrap_err();
            assert!(err.ends_with(": a contrast ratio from 1 to 21"), "{err}");
        }
    }

    #[test]
    fn option_as_alt_names_the_sides_that_are_alt() {
        let option = |source| toml::from_str::<Config>(source).map(|config| config.option_as_alt);
        assert_eq!(option("").unwrap().unwrap_or_default(), OptionAsAlt::Both);
        assert_eq!(option("option_as_alt = 'left'").unwrap(), Some(OptionAsAlt::Left));
        assert_eq!(option("option_as_alt = 'right'").unwrap(), Some(OptionAsAlt::Right));
        assert_eq!(option("option_as_alt = 'none'").unwrap(), Some(OptionAsAlt::None));
        let err = option("option_as_alt = 'meta'").unwrap_err().to_string();
        assert!(err.contains("expected one of `both`, `left`, `right`, `none`"), "{err}");
        // Held on (left, right): one side, the other, both, or no side the event names.
        let sides = [(true, false), (false, true), (true, true), (false, false)];
        let alt = |option: OptionAsAlt| sides.map(|(left, right)| option.alt(left, right));
        assert_eq!(alt(OptionAsAlt::Both), [true, true, true, true]);
        assert_eq!(alt(OptionAsAlt::Left), [true, false, true, true]);
        assert_eq!(alt(OptionAsAlt::Right), [false, true, true, true]);
        assert_eq!(alt(OptionAsAlt::None), [false, false, false, false]);
    }
}
