use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{Keystroke, WindowAppearance};
use regex::Regex;
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
    ligatures: Option<bool>,
    /// OpenType features, each `tag`, `-tag` or `tag=n` (`feature`), over `ligatures`'.
    font_features: Option<Vec<String>>,
    /// The cell's height as a share of the font's: 1 is the font's ascent plus descent.
    pub line_height: Option<f32>,
    keys: Option<Keys>,
    pub bell: Option<Bell>,
    pub option_as_alt: Option<OptionAsAlt>,
    /// The WCAG contrast ratio, 1 to 21, text keeps with its background.
    pub minimum_contrast: Option<f32>,
    /// Ask before closing a pane, tab or window, or quitting, while a program other than a shell
    /// at its prompt runs there.
    pub confirm_close: Option<bool>,
    /// How many seconds a closed pane, tab or window waits, its programs alive, for
    /// `undo-close`; 0 closes at once.
    pub undo_timeout: Option<u64>,
    pub shell_integration: Option<ShellIntegration>,
    /// Seconds a command must run for its end out of view to be notified (`notify_after()`).
    notify_after: Option<u64>,
    /// A click on the command line being typed moves the shell's cursor there.
    pub click_to_move: Option<bool>,
    /// What edits a text file a Cmd+click opens, in a new pane: a program and its arguments,
    /// `{path}`, `{line}` and `{col}` filled in; unset, the shell's `$EDITOR` (`editor::command`).
    pub editor: Option<Vec<String>>,
    /// Regexes quick select labels the matches of too (`quick_select`).
    quick_select_patterns: Option<Vec<String>>,
    /// `quick_select_patterns`, compiled.
    #[serde(skip)]
    pub quick_select: Vec<Regex>,
    /// The windows' layout is saved as it changes and restored at the next launch.
    pub restore_layout: Option<bool>,
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
    UndoClose,
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
    CopyLastOutput,
    Paste,
    FontBigger,
    FontSmaller,
    FontReset,
    PromptPrev,
    PromptNext,
    ClearScrollback,
    QuickSelect,
    QuickOpen,
    ReloadConfig,
    OpenConfig,
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
            "undo-close" => UndoClose,
            "last-pane" => LastPane,
            "zoom" => Zoom,
            "new-tab" => NewTab,
            "next-tab" => NextTab,
            "prev-tab" => PrevTab,
            "new-window" => NewWindow,
            "find" => Find,
            "copy" => Copy,
            "copy-last-output" => CopyLastOutput,
            "paste" => Paste,
            "font-bigger" => FontBigger,
            "font-smaller" => FontSmaller,
            "font-reset" => FontReset,
            "prompt-prev" => PromptPrev,
            "prompt-next" => PromptNext,
            "clear-scrollback" => ClearScrollback,
            "quick-select" => QuickSelect,
            "quick-open" => QuickOpen,
            "reload-config" => ReloadConfig,
            "open-config" => OpenConfig,
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

/// An OpenType feature as `font_features` writes it: `tag` or `+tag` turns it on, `-tag` off, and
/// `tag=n` picks the font's alternate n; a tag is four letters or digits.
fn feature(text: &str) -> Option<(String, u32)> {
    let (tag, value) = match text.split_once('=') {
        Some((tag, value)) => (tag, value.parse().ok()?),
        None => match text.strip_prefix('-') {
            Some(tag) => (tag, 0),
            None => (text.strip_prefix('+').unwrap_or(text), 1),
        },
    };
    let tag_like = tag.len() == 4 && tag.chars().all(|c| c.is_ascii_alphanumeric());
    tag_like.then(|| (tag.to_owned(), value))
}

/// rtty's own chords and, with `keys = "tutti"`, tutti's direct keys.
fn defaults(keys: Keys) -> BTreeMap<String, Action> {
    use Action::*;
    let fixed = [
        ("cmd-q", Quit),
        ("cmd-c", Copy),
        // iTerm2's chord for the output of the last command.
        ("cmd-shift-a", CopyLastOutput),
        ("cmd-v", Paste),
        ("cmd-f", Find),
        ("cmd-up", PromptPrev),
        ("cmd-down", PromptNext),
        ("cmd-k", ClearScrollback),
        // WezTerm's quick select is Ctrl+Shift+Space; Ctrl keys are the programs' here.
        ("cmd-shift-space", QuickSelect),
        ("cmd-shift-o", QuickOpen),
        ("cmd-=", FontBigger),
        ("cmd--", FontSmaller),
        ("cmd-0", FontReset),
        ("cmd-n", NewWindow),
        ("cmd-t", NewTab),
        ("cmd-w", ClosePane),
        ("cmd-z", UndoClose),
        ("cmd-d", SplitRight),
        ("cmd-shift-d", SplitDown),
        ("cmd-shift-enter", Zoom),
        // Cmd+Shift+[ and ], as macOS reports them.
        ("cmd-{", PrevTab),
        ("cmd-}", NextTab),
        // Cmd+Shift+, as macOS reports it, and Cmd+, (a Mac app's settings): Ghostty's chords.
        ("cmd-<", ReloadConfig),
        ("cmd-,", OpenConfig),
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

/// Whether rtty's login shells load its shell integration (`shell.rs`).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ShellIntegration {
    /// zsh, bash and fish do; any other shell is left as it is.
    #[default]
    Auto,
    None,
}

/// The scheme for each window appearance.
#[derive(Clone, Debug)]
pub enum Theme {
    Follow { dark: Arc<Scheme>, light: Arc<Scheme> },
    Fixed(Arc<Scheme>),
}

/// What the command line says over the config, at launch and at every reload: the file itself
/// (`--config`, which must be there), and `--font`, `--size` and `--ligatures` over its keys.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub config: Option<PathBuf>,
    pub font: Option<String>,
    pub size: Option<f32>,
    pub ligatures: Option<bool>,
}

/// `$XDG_CONFIG_HOME`, else `~/.config`.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
}

/// Every key, commented out at its default: what `open-config` starts a config file from.
pub const TEMPLATE: &str = include_str!("../config.template.toml");

/// Writes `TEMPLATE` to `path`, and the directories it is in, unless a file is there already.
pub fn create(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file.write_all(TEMPLATE.as_bytes()),
        Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
    }
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
        if config.editor.as_ref().is_some_and(Vec::is_empty) {
            return Err("editor = []: a program, then its arguments".into());
        }
        let patterns = config.quick_select_patterns.iter().flatten();
        let compiled = patterns.map(|pattern| {
            Regex::new(pattern).map_err(|err| format!("quick_select_patterns: {pattern:?}: {err}"))
        });
        config.quick_select = compiled.collect::<Result<_, _>>()?;
        if let Some(height) = config.line_height.filter(|height| !(0.5..=3.).contains(height)) {
            return Err(format!("line_height = {height}: a share of the font's height, 0.5 to 3"));
        }
        if let Some(wrong) =
            config.font_features.iter().flatten().find(|text| feature(text).is_none())
        {
            return Err(format!(
                "font_features: {wrong:?} is not a feature: a four-letter OpenType tag, alone \
                 (on), after - (off), or with =n (alternate n)"
            ));
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

    /// The OpenType features text is shaped with: `ligatures`' (on: `calt` and `liga`; off: those
    /// and `dlig` off), then `font_features`, then `--ligatures`, the last word on a tag winning.
    pub fn features(&self, overrides: &Overrides) -> Vec<(String, u32)> {
        let ligatures = |on: bool| {
            let tags: &[_] = if on { &["calt", "liga"] } else { &["calt", "liga", "dlig"] };
            tags.iter().map(move |tag| (tag.to_string(), u32::from(on)))
        };
        let words = ligatures(self.ligatures.unwrap_or(true))
            .chain(self.font_features.iter().flatten().filter_map(|text| feature(text)))
            .chain(overrides.ligatures.into_iter().flat_map(ligatures));
        let mut features: Vec<(String, u32)> = Vec::new();
        for (tag, value) in words {
            features.retain(|(other, _)| *other != tag);
            features.push((tag, value));
        }
        features
    }

    /// How long a command must run for its end out of view to be notified: `notify_after`
    /// seconds, 10 when it is left out; `None`, never, for 0.
    pub fn notify_after(&self) -> Option<Duration> {
        let seconds = self.notify_after.unwrap_or(10);
        (seconds > 0).then(|| Duration::from_secs(seconds))
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
    /// The files its schemes were read from; none for the built-ins.
    pub fn files(&self) -> Vec<&Path> {
        let schemes = match self {
            Theme::Follow { dark, light } => vec![dark, light],
            Theme::Fixed(scheme) => vec![scheme],
        };
        schemes.into_iter().filter_map(|scheme| scheme.path.as_deref()).collect()
    }

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
    fn the_template_holds_every_key_commented_out_at_its_default() {
        let defaults = Config::parse(TEMPLATE).unwrap();
        assert_eq!(defaults.bindings, Config::parse("").unwrap().bindings);
        // Each setting line, as it would read uncommented.
        let uncommented: Vec<&str> = TEMPLATE
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(setting) if setting.contains(" = ") || setting.starts_with("[bind]") => {
                    setting
                }
                _ => line,
            })
            .collect();
        let config = Config::parse(&uncommented.join("\n")).unwrap();
        assert_eq!(
            (config.font.as_deref(), config.size, config.ligatures),
            (Some("Menlo"), Some(14.), Some(true))
        );
        let none = Overrides::default();
        assert_eq!(config.features(&none), defaults.features(&none));
        assert_eq!(config.line_height, Some(1.));
        assert_eq!(config.keys.unwrap_or_default(), Keys::default());
        assert_eq!(config.bell.unwrap_or_default(), Bell::default());
        assert_eq!(config.option_as_alt.unwrap_or_default(), OptionAsAlt::default());
        assert_eq!(config.shell_integration.unwrap_or_default(), ShellIntegration::default());
        assert_eq!((config.minimum_contrast, config.undo_timeout), (Some(1.), Some(5)));
        assert_eq!(config.notify_after(), defaults.notify_after());
        assert_eq!((config.confirm_close, config.click_to_move), (Some(true), Some(true)));
        assert!(config.editor.is_some() && config.quick_select.is_empty());
        assert_eq!(config.restore_layout, Some(true));
        assert_eq!(config.bindings["alt-\\"], Action::SplitRight);
        assert!(!config.bindings.contains_key("cmd-shift-d"));
    }

    #[test]
    fn creating_the_config_leaves_one_that_is_there() {
        let dir = std::env::temp_dir().join(format!("rtty-config-{}", std::process::id()));
        let path = dir.join("rtty/config.toml");
        create(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), TEMPLATE);
        fs::write(&path, "size = 9").unwrap();
        create(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "size = 9");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_theme_is_watched_through_its_schemes_files() {
        let source = "[colors]\nforeground = '#ffffff'\nbackground = '#000000'\n\
                      ansi = ['#000000', '#000000', '#000000', '#000000', '#000000', '#000000', \
                      '#000000', '#000000']\nbrights = ['#000000', '#000000', '#000000', \
                      '#000000', '#000000', '#000000', '#000000', '#000000']";
        let mine = Scheme::parse(source, Some("/colors/mine.toml".into())).unwrap();
        let schemes = [scheme::load(&[]).unwrap(), vec![mine]].concat();
        let theme = |source| toml::from_str::<Config>(source).unwrap().theme(&schemes).unwrap();
        assert!(theme("").files().is_empty(), "the built-ins are in no file");
        assert_eq!(theme("scheme = 'mine'").files(), [Path::new("/colors/mine.toml")]);
        let pair = theme("scheme_light = 'mine'");
        assert_eq!(pair.files(), [Path::new("/colors/mine.toml")]);
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
    fn font_features_go_over_ligatures_and_the_command_line_over_both() {
        let features = |source: &str, ligatures: Option<bool>| {
            let overrides = Overrides { ligatures, ..Overrides::default() };
            Config::parse(source).unwrap().features(&overrides)
        };
        let tags = |tags: &[(&str, u32)]| -> Vec<(String, u32)> {
            tags.iter().map(|(tag, value)| (tag.to_string(), *value)).collect()
        };
        assert_eq!(features("", None), tags(&[("calt", 1), ("liga", 1)]));
        let off = tags(&[("calt", 0), ("liga", 0), ("dlig", 0)]);
        assert_eq!(features("ligatures = false", None), off);
        assert_eq!(features("", Some(false)), off);
        let mine = "font_features = ['ss01', '-calt', 'zero', 'cv01=2', '+dlig']";
        let expected =
            [("liga", 1), ("ss01", 1), ("calt", 0), ("zero", 1), ("cv01", 2), ("dlig", 1)];
        assert_eq!(features(mine, None), tags(&expected));
        // `--ligatures` has the last word on the tags it names.
        let expected = [("zero", 1), ("calt", 1), ("liga", 1)];
        assert_eq!(features("font_features = ['-calt', 'zero']", Some(true)), tags(&expected));
    }

    #[test]
    fn font_feature_and_line_height_mistakes_are_errors() {
        for wrong in ["cal", "-", "calt=x", "ss01 ", "ligatures", "-c lt", "=1"] {
            let err = Config::parse(&format!("font_features = ['{wrong}']")).unwrap_err();
            assert!(
                err.starts_with(&format!("font_features: {wrong:?} is not a feature")),
                "{err}"
            );
        }
        assert!(Config::parse("font_features = 'calt'").is_err(), "a list, not a tag");
        let height = |source: &str| Config::parse(source).map(|config| config.line_height);
        assert_eq!(height("line_height = 1.2").unwrap(), Some(1.2));
        assert_eq!(height("").unwrap(), None);
        for wrong in ["0.4", "3.5", "nan"] {
            let err = height(&format!("line_height = {wrong}")).unwrap_err();
            assert!(err.ends_with("a share of the font's height, 0.5 to 3"), "{err}");
        }
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
        assert_eq!(defaults.len(), 41);
        assert_eq!(defaults["cmd-d"], SplitRight);
        assert_eq!((defaults["cmd-<"], defaults["cmd-,"]), (ReloadConfig, OpenConfig));
        assert_eq!((defaults["cmd-k"], defaults["cmd-0"]), (ClearScrollback, FontReset));
        assert_eq!(defaults["cmd-z"], UndoClose);
        assert_eq!(defaults["cmd-shift-a"], CopyLastOutput);
        assert_eq!(
            (defaults["cmd-shift-space"], defaults["cmd-shift-o"]),
            (QuickSelect, QuickOpen)
        );
        assert_eq!(defaults["cmd-{"], PrevTab);
        assert_eq!(defaults["cmd-9"], Tab(8));
        // As GPUI writes chords: ctrl, alt, cmd, shift.
        assert_eq!(defaults["alt-cmd-left"], Focus(Direction::Left));
        assert_eq!(defaults["ctrl-cmd-down"], Resize(Direction::Down));
        assert!(!defaults.contains_key("ctrl-h"));
        let tutti = bindings("keys = 'tutti'");
        assert_eq!(tutti.len(), 49);
        assert_eq!(
            (tutti["ctrl-h"], tutti["alt-l"]),
            (Focus(Direction::Left), Resize(Direction::Right))
        );
        // Modifiers in any order, a capital as Shift; `none` unbinds.
        let mine = bindings(
            "keys = 'tutti'\n[bind]\n'alt-\\' = 'split-right'\n'ctrl-alt-H' = 'zoom'\n\
             'cmd-d' = 'new-tab'\n'shift-cmd-d' = 'none'\n'ctrl-h' = 'none'\n'alt--' = 'split-down'",
        );
        assert_eq!(mine.len(), 49 + 3 - 2);
        assert_eq!((mine["alt-\\"], mine["alt--"]), (SplitRight, SplitDown));
        assert_eq!((mine["ctrl-alt-shift-h"], mine["cmd-d"]), (Zoom, NewTab));
        assert!(!mine.contains_key("cmd-shift-d") && !mine.contains_key("ctrl-h"));
    }

    #[test]
    fn every_action_has_its_name() {
        let names = "focus-left focus-right focus-up focus-down resize-left resize-right \
                     resize-up resize-down swap-left swap-right swap-up swap-down split-right \
                     split-down close-pane undo-close last-pane zoom new-tab next-tab prev-tab tab-1 tab-9 \
                     new-window find copy copy-last-output paste font-bigger font-smaller font-reset \
                     prompt-prev prompt-next clear-scrollback quick-select quick-open reload-config \
                     open-config quit";
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
    fn closing_asks_first_unless_told_not_to() {
        let confirm = |source| toml::from_str::<Config>(source).map(|config| config.confirm_close);
        assert_eq!(confirm("").unwrap(), None);
        assert_eq!(confirm("confirm_close = false").unwrap(), Some(false));
        assert!(confirm("confirm-close = false").is_err(), "keys are written with underscores");
    }

    #[test]
    fn a_close_waits_for_undo_whole_seconds() {
        let wait = |source| toml::from_str::<Config>(source).map(|config| config.undo_timeout);
        assert_eq!(wait("").unwrap(), None);
        assert_eq!(wait("undo_timeout = 0").unwrap(), Some(0));
        assert_eq!(wait("undo_timeout = 30").unwrap(), Some(30));
        assert!(wait("undo_timeout = -1").is_err());
        assert!(wait("undo_timeout = 2.5").is_err());
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
    fn the_editor_is_a_program_and_its_arguments() {
        let editor = |source: &str| Config::parse(source).map(|config| config.editor);
        assert_eq!(editor("").unwrap(), None);
        let template = editor("editor = ['nvim', '+{line}', '{path}']").unwrap().unwrap();
        assert_eq!(template, ["nvim", "+{line}", "{path}"]);
        assert_eq!(
            editor("editor = []").unwrap_err(),
            "editor = []: a program, then its arguments"
        );
        assert!(
            editor("editor = 'nvim +{line} {path}'").unwrap_err().contains("expected a sequence")
        );
    }

    #[test]
    fn quick_select_patterns_are_regexes_and_a_bad_one_is_named() {
        let patterns = |source: &str| Config::parse(source).map(|config| config.quick_select);
        assert!(patterns("").unwrap().is_empty());
        let mine = patterns(r#"quick_select_patterns = ['[A-Z]+-\d+', 'sha256:(\w+)']"#).unwrap();
        let sources: Vec<&str> = mine.iter().map(Regex::as_str).collect();
        assert_eq!(sources, [r"[A-Z]+-\d+", r"sha256:(\w+)"]);
        let err = patterns("quick_select_patterns = ['ok', '(unclosed']").unwrap_err();
        assert!(
            err.starts_with("quick_select_patterns: \"(unclosed\": regex parse error"),
            "{err}"
        );
        assert!(patterns("quick_select_patterns = 'x'").is_err(), "a list, not a pattern");
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

    #[test]
    fn shell_integration_keys_read_as_written() {
        let defaults: Config = toml::from_str("").unwrap();
        assert_eq!(defaults.shell_integration.unwrap_or_default(), ShellIntegration::Auto);
        assert_eq!(defaults.notify_after(), Some(Duration::from_secs(10)), "the default notifies");
        assert_eq!(defaults.click_to_move, None);
        let source = "shell_integration = 'none'\nnotify_after = 0\nclick_to_move = false";
        let config: Config = toml::from_str(source).unwrap();
        assert_eq!(config.shell_integration, Some(ShellIntegration::None));
        assert_eq!((config.notify_after(), config.click_to_move), (None, Some(false)));
        let soon: Config = toml::from_str("notify_after = 3").unwrap();
        assert_eq!(soon.notify_after(), Some(Duration::from_secs(3)));
        let err = toml::from_str::<Config>("shell_integration = 'zsh'").unwrap_err().to_string();
        assert!(err.contains("unknown variant `zsh`, expected `auto` or `none`"), "{err}");
        assert!(toml::from_str::<Config>("notify_after = -1").is_err());
        let dashed = toml::from_str::<Config>("notify-after = 5");
        assert!(dashed.is_err(), "written with underscores, as every key is");
    }
}
