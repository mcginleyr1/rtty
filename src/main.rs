mod app;
mod cli;
mod config;
mod control;
mod editor;
mod glyphs;
mod keys;
mod layout;
mod links;
mod notify;
mod pane;
mod progress;
mod prompt;
mod quick;
mod render;
mod scheme;
mod script;
mod search;
mod selection;
mod session;
mod shell;
mod tabbar;
mod title;
mod view;
mod window;

use std::backtrace::Backtrace;
use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::ptr::{self, NonNull};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use std::{iter, mem, slice};

use block2::RcBlock;
use gpui::{
    App, Application, BorrowAppContext, DummyKeyboardMapper, FontFallbacks, FontFeatures, Global,
    KeyBinding, KeyBindingContextPredicate, actions, font, px,
};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags as Flags, NSView};
use portable_pty::CommandBuilder;
use rtty_vt::Terminal;

use config::{Action, Config, Overrides, ShellIntegration, Theme};
use control::Response;
use futures::StreamExt;
use pane::{Pane, SCROLLBACK, START_SIZE, Trace, shell_command};
use view::{
    ClearScrollback, Copy, CopyLastOutput, Find, Launch, NextPrompt, Paste, PreviousPrompt,
    QuickOpen, QuickSelect, Settings,
};
use window::{
    ClosePane, CloseWindow, LastPane, Minimize, MoveFocus, NewTab, NewWindow, NextTab, OpenConfig,
    PreviousTab, ResizePane, SelectTab, SplitDown, SplitRight, SwapPane, ToggleZoom, UndoClose,
    ZoomWindow,
};

actions!(rtty, [Quit, IncreaseFontSize, DecreaseFontSize, ResetFontSize, ReloadConfig]);

/// How often the files the config came from are looked at for a change.
const WATCH: Duration = Duration::from_secs(1);

/// The files the config came from (the config file and the scheme files it names) and when each
/// was last modified (`None` while it is missing), as of the last time the config was read.
struct Seen(Vec<(PathBuf, Option<SystemTime>)>);

impl Global for Seen {}

const USAGE: &str = "usage: rtty [--config FILE] [--font NAME] [--size PT] [--ligatures on|off] \
                     [--stats] [--trace FILE] [--script FILE [--activate] [--restore]]
                     [-e PROGRAM [ARGS...]]
       rtty cli COMMAND...       ask the rtty this pane runs in (`rtty cli` lists the commands)
       rtty --smoke              run a command through the pty pipeline headless and check it
       rtty --bench [COMMAND]    time the pty -> parser -> grid pipeline (default: seq)";

// macOS seq prints 1e+06 past a million without the explicit format.
const BENCH_COMMAND: &str = "seq -f %.0f 1 2000000";

/// `rtty.log`, beside the socket: when each rtty started and quit, its panes' lives, and panics
/// with their backtraces. Never what a pane shows or is sent.
static LOG: OnceLock<PathBuf> = OnceLock::new();

/// The log starts over (keeping the last as `rtty.log.old`) past this size.
const LOG_LIMIT: u64 = 1 << 20;

/// The device-dependent bits of an NSEvent's modifier flags that say which Option key is down
/// (IOKit's `NX_DEVICELALTKEYMASK` and `NX_DEVICERALTKEYMASK`).
pub const LEFT_OPTION: Flags = Flags(0x20);
pub const RIGHT_OPTION: Flags = Flags(0x40);

/// Appends a line to the log, after the local time and this rtty's pid.
pub fn note(line: &str) {
    let Some(path) = LOG.get() else { return };
    // SAFETY: time and localtime_r only write the struct they are given.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    let entry = format!(
        "{}-{:02}-{:02} {:02}:{:02}:{:02} pid {} {line}\n",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        std::process::id()
    );
    let file = OpenOptions::new().create(true).append(true).mode(0o600).open(path);
    let _ = file.and_then(|mut file| file.write_all(entry.as_bytes()));
}

/// Starts the log in `dir` and has every panic, on any thread, written to it before Rust's own
/// report on stderr.
fn start_log(dir: &std::path::Path) {
    let path = dir.join("rtty.log");
    if fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_LIMIT) {
        let _ = fs::rename(&path, dir.join("rtty.log.old"));
    }
    LOG.get_or_init(|| path);
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        note(&format!("panic on thread {name}: {info}\n{}", Backtrace::force_capture()));
        report(info);
    }));
    note(&format!("rtty {} started", env!("CARGO_PKG_VERSION")));
}

/// The command line.
#[derive(Default)]
struct Options {
    overrides: Overrides,
    stats: bool,
    trace: Option<PathBuf>,
    script: Option<PathBuf>,
    /// A scripted window takes the keyboard only when asked to: the person at the machine may be
    /// typing elsewhere.
    activate: bool,
    /// A scripted launch restores and saves the layout only when asked to, as a normal one does.
    restore: bool,
    command: Option<Vec<String>>,
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "cli" => return cli::run(args.collect()),
            "--smoke" => return smoke(),
            "--bench" => {
                let command = args.collect::<Vec<_>>().join(" ");
                return bench(if command.is_empty() { BENCH_COMMAND } else { &command });
            }
            "--stats" => options.stats = true,
            "--trace" => match args.next() {
                Some(path) => options.trace = Some(path.into()),
                None => return usage_error("--trace needs a file"),
            },
            "--script" => match args.next() {
                Some(path) => options.script = Some(path.into()),
                None => return usage_error("--script needs a file"),
            },
            "--activate" => options.activate = true,
            "--restore" => options.restore = true,
            "--config" => match args.next() {
                Some(path) => options.overrides.config = Some(path.into()),
                None => return usage_error("--config needs a file"),
            },
            "--font" => match args.next() {
                Some(name) => options.overrides.font = Some(name),
                None => return usage_error("--font needs a font family name"),
            },
            "--size" => match args.next().and_then(|points| points.parse().ok()) {
                Some(points) => options.overrides.size = Some(points),
                None => return usage_error("--size needs a number of points"),
            },
            "--ligatures" => match args.next().as_deref() {
                Some("on") => options.overrides.ligatures = Some(true),
                Some("off") => options.overrides.ligatures = Some(false),
                _ => return usage_error("--ligatures needs on or off"),
            },
            "-e" => match args.next() {
                Some(program) => {
                    options.command = Some(std::iter::once(program).chain(args.by_ref()).collect())
                }
                None => return usage_error("-e needs a program"),
            },
            other => return usage_error(&format!("unknown argument {other}")),
        }
    }
    if let Err(message) = run(options) {
        eprintln!("rtty: {message}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

/// The fallback when the config names none: an installed Nerd Font, for the icons prompts and
/// file listers print. Symbols Nerd Font Mono has nothing but the icons; else a whole family's
/// Mono variant, the one whose icons keep to a cell of its own.
fn nerd_font(installed: &[String]) -> Option<String> {
    let named = |name: &str| installed.iter().find(|family| *family == name);
    let ending = |suffix: &str| installed.iter().filter(|family| family.ends_with(suffix)).min();
    named("Symbols Nerd Font Mono")
        .or_else(|| named("Symbols Nerd Font"))
        .or_else(|| ending(" Nerd Font Mono"))
        .or_else(|| ending(" Nerd Font"))
        .cloned()
}

/// What `action` is to GPUI, which keys and the menu bar run, and the key context its chord works
/// in: the pane's own actions in the pane's, the rest anywhere. Focus, resize, close and last pane
/// carry their chord, `chord`, which a pane that claims the keys gets instead
/// (`PaneView::claimed_chord`).
pub fn gpui_action(action: Action, chord: &str) -> (Box<dyn gpui::Action>, Option<&'static str>) {
    let keystroke = chord.to_owned();
    let pane = Some("Terminal");
    match action {
        Action::Focus(direction) => (Box::new(MoveFocus { direction, keystroke }), None),
        Action::Resize(direction) => (Box::new(ResizePane { direction, keystroke }), None),
        Action::Swap(direction) => (Box::new(SwapPane(direction)), None),
        Action::SplitRight => (Box::new(SplitRight), None),
        Action::SplitDown => (Box::new(SplitDown), None),
        Action::ClosePane => (Box::new(ClosePane { keystroke }), None),
        Action::UndoClose => (Box::new(UndoClose), None),
        Action::LastPane => (Box::new(LastPane { keystroke }), None),
        Action::Zoom => (Box::new(ToggleZoom), None),
        Action::NewTab => (Box::new(NewTab), None),
        Action::NextTab => (Box::new(NextTab), None),
        Action::PrevTab => (Box::new(PreviousTab), None),
        Action::Tab(index) => (Box::new(SelectTab(index)), None),
        Action::NewWindow => (Box::new(NewWindow), None),
        Action::Find => (Box::new(Find), pane),
        Action::Copy => (Box::new(Copy), pane),
        Action::CopyLastOutput => (Box::new(CopyLastOutput), pane),
        Action::Paste => (Box::new(Paste), pane),
        Action::PromptPrev => (Box::new(PreviousPrompt), pane),
        Action::PromptNext => (Box::new(NextPrompt), pane),
        Action::ClearScrollback => (Box::new(ClearScrollback), pane),
        Action::QuickSelect => (Box::new(QuickSelect), pane),
        Action::QuickOpen => (Box::new(QuickOpen), pane),
        Action::FontBigger => (Box::new(IncreaseFontSize), None),
        Action::FontSmaller => (Box::new(DecreaseFontSize), None),
        Action::FontReset => (Box::new(ResetFontSize), None),
        Action::ReloadConfig => (Box::new(ReloadConfig), None),
        Action::OpenConfig => (Box::new(OpenConfig), None),
        Action::Quit => (Box::new(Quit), None),
        Action::CloseWindow => (Box::new(CloseWindow), None),
        Action::Minimize => (Box::new(Minimize), None),
        Action::ZoomWindow => (Box::new(ZoomWindow), None),
        Action::About => (Box::new(app::About), None),
        Action::Hide => (Box::new(app::Hide), None),
        Action::HideOthers => (Box::new(app::HideOthers), None),
        Action::ShowAll => (Box::new(app::ShowAll), None),
        Action::SecureInput => (Box::new(app::ToggleSecureInput), None),
    }
}

/// The GPUI binding of `chord` to `action` (`gpui_action`).
fn key_binding(chord: &str, action: Action) -> KeyBinding {
    let (action, context) = gpui_action(action, chord);
    let context = context.map(|context| {
        KeyBindingContextPredicate::parse(context).expect("rtty's key contexts parse").into()
    });
    KeyBinding::load(chord, action, context, false, None, &DummyKeyboardMapper)
        .expect("rtty's chords parse")
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("rtty: {message}\n{USAGE}");
    ExitCode::from(2)
}

/// The user's login shell, started in their home directory; or `-e`'s program, started here.
fn command(argv: Option<Vec<String>>) -> CommandBuilder {
    let Some(argv) = argv else { return CommandBuilder::new_default_prog() };
    let mut command = CommandBuilder::from_argv(argv.into_iter().map(Into::into).collect());
    if let Ok(dir) = std::env::current_dir() {
        command.cwd(dir);
    }
    command
}

/// The config file (`None`: the defaults) and the color schemes, read and checked, with the
/// command line's overrides.
struct Loaded {
    config: Config,
    theme: Theme,
    family: String,
    size: f32,
}

fn load(path: Option<&Path>, overrides: &Overrides) -> Result<Loaded, String> {
    let config = match path {
        // At a reload too: a file named on the command line and gone is no default config.
        Some(path) if overrides.config.is_some() && !path.exists() => {
            return Err(format!("{}: no such file", path.display()));
        }
        Some(path) => Config::load(path)?,
        None => Config::parse("")?,
    };
    let scheme_dirs: Vec<PathBuf> = config::home()
        .iter()
        .flat_map(|home| [home.join("rtty/colors"), home.join("wezterm/colors")])
        .collect();
    let theme = config.theme(&scheme::load(&scheme_dirs)?)?;
    let family = overrides.font.clone().or(config.font.clone()).unwrap_or_else(|| "Menlo".into());
    let size = overrides.size.or(config.size).unwrap_or(14.);
    if size <= 0. || !size.is_finite() {
        return Err(format!("font size {size} is not a positive number of points"));
    }
    Ok(Loaded { config, theme, family, size })
}

/// `Settings` as `loaded` says, for `launch`: the font once its families are known to be
/// installed (with no `fallback`, an installed Nerd Font, `nerd_font`), and the shell
/// integration's scripts written beside the socket.
fn settings(loaded: Loaded, launch: Launch, cx: &App) -> Result<Settings, String> {
    let Loaded { config, theme, family, size } = loaded;
    let (notify_after, features) = (config.notify_after(), config.features(&launch.overrides));
    let installed = cx.text_system().all_font_names();
    let fallbacks = config.fallback.unwrap_or_else(|| nerd_font(&installed).into_iter().collect());
    if let Some(missing) =
        iter::once(&family).chain(&fallbacks).find(|name| !installed.contains(name))
    {
        return Err(format!("no font family named {missing:?}"));
    }
    let mut font = font(family);
    font.features = FontFeatures(Arc::new(features));
    // Ahead of macOS's own cascade, which knows no font with Nerd Font icons.
    font.fallbacks = (!fallbacks.is_empty()).then(|| FontFallbacks::from_fonts(fallbacks));
    let dir = launch.socket.parent().expect("the socket is in its directory");
    let shell_integration = match config.shell_integration.unwrap_or_default() {
        ShellIntegration::Auto => {
            Some(shell::install(dir).map_err(|err| format!("{}: {err}", dir.display()))?)
        }
        ShellIntegration::None => None,
    };
    Ok(Settings {
        font,
        font_size: px(size),
        size: px(size),
        line_height: config.line_height.unwrap_or(1.),
        theme,
        notify_after,
        bindings: config.bindings,
        option_as_alt: config.option_as_alt.unwrap_or_default(),
        bell: config.bell.unwrap_or_default(),
        minimum_contrast: config.minimum_contrast.unwrap_or(1.),
        confirm_close: config.confirm_close.unwrap_or(true),
        undo_timeout: Duration::from_secs(config.undo_timeout.unwrap_or(5)),
        shell_integration,
        click_to_move: config.click_to_move.unwrap_or(true),
        editor: config.editor,
        quick_select: config.quick_select,
        restore_layout: config.restore_layout.unwrap_or(true),
        graphics: config.graphics.unwrap_or(true),
        secure_input: config.secure_input.unwrap_or(false),
        auto_secure_input: config.auto_secure_input.unwrap_or(true),
        hotkey: config.hotkey,
        clipboard_read: config.clipboard_read.unwrap_or_default(),
        launch,
    })
}

/// Puts `settings` into effect: the keymap, and every window and pane, which read them as they
/// draw and act. A size picked with Cmd+= and Cmd+- stays unless the configured one changed.
fn configure(mut settings: Settings, cx: &mut App) {
    let old = cx.try_global::<Settings>();
    if let Some(old) = old.filter(|old| old.size == settings.size) {
        settings.font_size = old.font_size;
    }
    let toggled = old.is_none_or(|old| old.secure_input != settings.secure_input);
    cx.clear_key_bindings();
    cx.bind_keys(settings.bindings.iter().map(|(chord, action)| key_binding(chord, *action)));
    cx.set_global(settings);
    // The menu bar's chords from the keymap just bound.
    app::configure(toggled, cx);
}

/// The files the config came from, and when each was last modified.
fn watched(cx: &App) -> Vec<(PathBuf, Option<SystemTime>)> {
    let settings = cx.global::<Settings>();
    let files = settings.launch.config.as_deref().into_iter().chain(settings.theme.files());
    let modified = |path: &Path| fs::metadata(path).and_then(|meta| meta.modified()).ok();
    files.map(|path| (path.to_owned(), modified(path))).collect()
}

/// Reads the config again and puts it into effect in every window and pane. A mistake keeps the
/// config in effect; either way every window says what came of it, and so does the log.
fn reload(cx: &mut App) {
    let launch = cx.global::<Settings>().launch.clone();
    let loaded = load(launch.config.as_deref(), &launch.overrides);
    let said = match loaded.and_then(|loaded| settings(loaded, launch, cx)) {
        Ok(settings) => {
            configure(settings, cx);
            "config reloaded".to_owned()
        }
        Err(err) => format!("config not reloaded, the last one stays: {err}"),
    };
    note(&said);
    window::reloaded(&said, cx);
    cx.set_global(Seen(watched(cx)));
}

/// The layout last written, and why the writes since have failed, if they have.
#[derive(Default)]
struct Saved {
    json: String,
    failed: Option<String>,
}

/// Keeps every window's layout for the next launch, when this launch keeps it and it changed
/// since it was last written. A write that fails is tried again at the next call, and its error
/// logged once, until it changes.
fn save_layout(saved: &mut Saved, cx: &mut App) {
    let settings = cx.global::<Settings>();
    let Some(path) = settings.launch.layout.clone().filter(|_| settings.restore_layout) else {
        return;
    };
    let json = serde_json::to_string_pretty(&window::session(cx)).expect("a layout is JSON");
    if json == saved.json {
        return;
    }
    match session::write(&path, &json) {
        Ok(()) => *saved = Saved { json, failed: None },
        Err(err) => {
            let err = format!("layout not saved: {}: {err}", path.display());
            if saved.failed.as_ref() != Some(&err) {
                note(&err);
            }
            saved.failed = Some(err);
        }
    }
}

/// The windows the last launch left, or none (and why, in the log).
fn saved_layout(path: &Path) -> Vec<session::Window> {
    let windows = session::read(path).unwrap_or_else(|err| {
        note(&format!("no layout restored: {}: {err}", path.display()));
        Vec::new()
    });
    if !windows.is_empty() {
        note(&format!("restoring the layout in {} (windows: {})", path.display(), windows.len()));
    }
    windows
}

/// Reads `config.toml` (or `--config`), loads the color schemes, and runs the window.
fn run(options: Options) -> Result<(), String> {
    // Absolute, so that it names the same file from an editor's directory (`open-config`).
    let config = match (&options.overrides.config, config::home()) {
        (Some(path), _) => {
            Some(std::path::absolute(path).map_err(|err| format!("{}: {err}", path.display()))?)
        }
        (None, home) => home.map(|home| home.join("rtty/config.toml")),
    };
    let loaded = load(config.as_deref(), &options.overrides)?;
    let read = |path: &PathBuf| {
        std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))
    };
    let script =
        options.script.as_ref().map(read).transpose()?.map(|source| script::parse(&source));
    let script = script.transpose()?;
    if options.activate && script.is_none() {
        return Err("--activate goes with --script".into());
    }
    if options.restore && script.is_none() {
        return Err("--restore goes with --script".into());
    }
    let activate = script.is_none() || options.activate;
    let (open_links, clipboard) = (script.is_none(), script.is_none());
    let trace = options
        .trace
        .as_ref()
        .map(|path| Trace::create(path).map_err(|err| format!("{}: {err}", path.display())));
    let trace = trace.transpose()?;
    let socket = control::socket_path()?;
    let dir = socket.parent().expect("the socket is in its directory");
    start_log(dir);
    control::sweep(dir);
    let requests =
        control::listen(&socket).map_err(|err| format!("{}: {err}", socket.display()))?;
    // One rtty at a time keeps the layout: the first to lock it.
    let keeps_layout = options.command.is_none() && (script.is_none() || options.restore);
    let layout = session::path().filter(|_| keeps_layout).filter(|path| {
        let taken = session::take(path);
        match &taken {
            Ok(true) => {}
            Ok(false) => note("another rtty keeps the layout"),
            Err(err) => note(&format!("layout not kept: {}: {err}", path.display())),
        }
        taken.is_ok_and(|taken| taken)
    });
    let launch = Launch {
        stats: options.stats,
        trace,
        socket: socket.clone(),
        config,
        overrides: options.overrides,
        layout,
        open_links,
        clipboard,
        prompts: script.is_none(),
    };
    app::ask_before_terminating();
    Application::new().with_assets(window::Icons).run(move |cx: &mut App| {
        let settings = settings(loaded, launch, cx).unwrap_or_else(|err| {
            eprintln!("rtty: {err}");
            std::process::exit(2);
        });
        // What the key monitor below reads, which cannot reach the settings.
        let option_as_alt = Rc::new(Cell::new(settings.option_as_alt));
        app::init(!settings.launch.prompts, cx);
        configure(settings, cx);
        cx.observe_global::<Settings>({
            let option_as_alt = option_as_alt.clone();
            move |cx| option_as_alt.set(cx.global::<Settings>().option_as_alt)
        })
        .detach();
        cx.set_global(Seen(watched(cx)));
        // Once the window that took the chord is done with it, so that it says what came of it.
        cx.on_action(|_: &ReloadConfig, cx| cx.defer(reload));
        // Edits to the config take effect once saved, with no reload by hand, and the layout is
        // kept as it changes.
        cx.spawn(async move |cx| {
            let mut saved = Saved::default();
            loop {
                cx.background_executor().timer(WATCH).await;
                let ticked = cx.update(|cx| {
                    if cx.global::<Seen>().0 != watched(cx) {
                        reload(cx)
                    }
                    save_layout(&mut saved, cx)
                });
                if ticked.is_err() {
                    break;
                }
            }
        })
        .detach();
        // A Cmd chord GPUI dispatched: the contexts it met, the actions bound to it there, and the
        // one that ran. Only Cmd chords, which are rtty's: never what goes to a pane.
        cx.observe_keystrokes(|event, window, cx| {
            let keystroke = &event.keystroke;
            if !keystroke.modifiers.platform {
                return;
            }
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let (bound, _) =
                keymap.bindings_for_input(slice::from_ref(keystroke), &event.context_stack);
            let bound: Vec<&str> = bound.iter().map(|binding| binding.action().name()).collect();
            let ran = event.action.as_ref().map_or("nothing", |action| action.name());
            let (contexts, active) = (&event.context_stack, window.is_window_active());
            note(&format!(
                "key {keystroke}, window active {active}, contexts {contexts:?}: bound to \
                 {bound:?}, ran {ran}"
            ));
        })
        .detach();
        // Keys as AppKit hands them to rtty, before GPUI sees them. Cmd chords are logged: one
        // logged here and not above never reached GPUI's dispatch.
        let monitor = RcBlock::new(move |event: NonNull<NSEvent>| {
            // SAFETY: AppKit calls the monitor with a live event.
            let key = unsafe { event.as_ref() };
            let flags = key.modifierFlags();
            let chars = key.charactersIgnoringModifiers().map(|chars| chars.to_string());
            let chars = chars.unwrap_or_default();
            if flags.contains(Flags::Command) {
                let marks = [(Flags::Control, '⌃'), (Flags::Option, '⌥'), (Flags::Shift, '⇧')];
                let held: String = marks
                    .into_iter()
                    .filter_map(|(flag, mark)| flags.contains(flag).then_some(mark))
                    .collect();
                let code = key.keyCode();
                note(&format!("AppKit key down {held}⌘{chars:?} (key code {code})"));
            }
            // An Option that is not Alt, with a key that types text (no control character, none
            // of AppKit's function-key characters from U+F700): the window's text input takes
            // it, as a macOS text view's does (é, €, a dead key's accent), and GPUI never sees
            // it, so no `alt-` binding and no ESC prefix.
            let option =
                flags.contains(Flags::Option) && !flags.intersects(Flags::Command | Flags::Control);
            let (left, right) = (flags.contains(LEFT_OPTION), flags.contains(RIGHT_OPTION));
            let alt = option_as_alt.get().alt(left, right);
            let text = chars
                .chars()
                .next()
                .is_some_and(|c| !c.is_control() && !('\u{f700}'..='\u{f8ff}').contains(&c));
            if option && !alt && text {
                let main =
                    MainThreadMarker::new().expect("AppKit calls monitors on the main thread");
                let view = key.window(main).and_then(|window| window.firstResponder());
                let view = view.and_then(|responder| responder.downcast::<NSView>().ok());
                if view
                    .and_then(|view| view.inputContext())
                    .is_some_and(|input| input.handleEvent(key))
                {
                    return ptr::null_mut();
                }
            }
            event.as_ptr()
        });
        // SAFETY: the monitor returns the event it is given, or null for one the text input
        // took. It stays for rtty's life.
        mem::forget(unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &monitor)
        });
        // Once the window that took the chord is done with it, so the question can show on one.
        cx.on_action(|_: &Quit, cx| cx.defer(window::quit));
        // Every pane in every window follows the one size, and reflows when it changes.
        let step = |points: f32| {
            move |cx: &mut App| {
                cx.update_global::<Settings, _>(|settings, _| {
                    settings.font_size = (settings.font_size + px(points)).clamp(px(6.), px(72.))
                })
            }
        };
        cx.on_action({
            let grow = step(1.);
            move |_: &IncreaseFontSize, cx| grow(cx)
        });
        cx.on_action({
            let shrink = step(-1.);
            move |_: &DecreaseFontSize, cx| shrink(cx)
        });
        cx.on_action(|_: &ResetFontSize, cx| {
            cx.update_global::<Settings, _>(|settings, _| settings.font_size = settings.size)
        });
        cx.spawn(async move |cx| {
            let mut requests = requests;
            while let Some((request, answer)) = requests.next().await {
                let response = cx.update(|cx| window::answer(request, cx));
                let _ =
                    answer.send(response.unwrap_or_else(|err| Response::Error(err.to_string())));
            }
        })
        .detach();
        // A click on one of rtty's notifications brings rtty forward at the pane that sent it.
        let (clicks, mut clicked) = futures::channel::mpsc::unbounded();
        notify::init(clicks);
        cx.spawn(async move |cx| {
            while let Some(pane) = clicked.next().await {
                let _ = cx.update(|cx| {
                    window::answer(control::Request::Focus { pane }, cx);
                    cx.activate(true);
                });
            }
        })
        .detach();
        cx.on_app_quit({
            let socket = socket.clone();
            move |cx| {
                // Before the windows go.
                save_layout(&mut Saved::default(), cx);
                let _ = std::fs::remove_file(&socket);
                note("quit");
                async {}
            }
        })
        .detach();
        cx.on_window_closed(|cx| {
            window::refresh_badge(cx);
            if cx.windows().is_empty() {
                app::quit("the last window closed", cx)
            } else {
                // Once no window is in an update, so every one can be asked and read.
                cx.defer(app::window_closed)
            }
        })
        .detach();
        let settings = cx.global::<Settings>();
        let layout = settings.launch.layout.clone().filter(|_| settings.restore_layout);
        let restored = layout.map_or_else(Vec::new, |path| saved_layout(&path));
        let restored = restored.into_iter().filter_map(|saved| {
            window::restore(saved, cx).map_err(|err| note(&format!("not restored: {err}"))).ok()
        });
        let restored: Vec<_> = restored.collect();
        let window = restored
            .first()
            .copied()
            .map_or_else(|| window::open(command(options.command), cx), Ok);
        let window = window.unwrap_or_else(|err| {
            eprintln!("rtty: {err}");
            std::process::exit(1);
        });
        if let Some(steps) = script {
            window
                .update(cx, |_, window, cx| script::run(steps, window, cx))
                .expect("the window just opened");
        }
        if activate {
            cx.activate(true);
        }
    });
    Ok(())
}

/// Waits for the pane's output to end and its child to exit; the exit code.
fn finish(pane: &mut Pane) -> Option<u32> {
    pane.reader.take().expect("a reader thread").join().expect("the pty reader");
    pane.waiter.take().expect("a waiter thread").join().expect("the child waiter")
}

fn smoke() -> ExitCode {
    let (rows, cols) = START_SIZE;
    let (mut pane, _notices) = Pane::spawn(
        shell_command("printf hello-rtty; exit 7"),
        rows,
        cols,
        None,
        None,
        None,
        false,
    )
    .expect("spawn /bin/sh");
    let exit_code = finish(&mut pane);
    let screen = pane.lock().contents();
    let passed = screen.contains("hello-rtty") && exit_code == Some(7);
    println!(
        "smoke {}: screen {:?}, exit code {exit_code:?}",
        if passed { "ok" } else { "FAILED" },
        screen.trim_end()
    );
    if passed { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Times the command into a pipe, the parser alone on that output, and the whole pty pipeline.
fn bench(command: &str) -> ExitCode {
    let started = Instant::now();
    let output = Command::new("/bin/sh").args(["-c", command]).output().expect("run the command");
    let produce = started.elapsed();
    // What the pty's onlcr turns the output into.
    let bytes = String::from_utf8_lossy(&output.stdout).replace('\n', "\r\n").into_bytes();

    let (rows, cols) = START_SIZE;
    let mut term = Terminal::new(rows, cols, SCROLLBACK);
    let started = Instant::now();
    term.advance(&bytes);
    let parse = started.elapsed();

    let started = Instant::now();
    let (mut pane, _notices) =
        Pane::spawn(shell_command(command), rows, cols, None, None, None, false)
            .expect("spawn /bin/sh");
    finish(&mut pane);
    let pipeline = started.elapsed();

    let megabytes = bytes.len() as f64 / 1e6;
    let line = |label: &str, took: Duration| {
        let seconds = took.as_secs_f64();
        println!("  {label:<28} {:>8.1} ms {:>8.1} MB/s", seconds * 1000., megabytes / seconds);
    };
    println!(
        "bench: `{command}`, {megabytes:.1} MB after onlcr, {cols}x{rows} grid, {SCROLLBACK} rows of scrollback"
    );
    line("command into a pipe", produce);
    line("parse into rtty-vt (memory)", parse);
    line("pty + reader thread + rtty-vt", pipeline);
    let same = term.contents() == pane.lock().contents();
    println!("  pipeline screen matches the in-memory parse: {same}");
    if same { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_fallback_is_symbols_nerd_font_else_a_mono_nerd_font() {
        let names = |names: &[&str]| names.iter().map(|name| name.to_string()).collect::<Vec<_>>();
        let families = names(&[
            "Menlo",
            "Iosevka Nerd Font Propo",
            "Iosevka Nerd Font",
            "Iosevka Nerd Font Mono",
            "Hack Nerd Font Mono",
            "Symbols Nerd Font",
        ]);
        assert_eq!(nerd_font(&families).as_deref(), Some("Symbols Nerd Font"));
        let mut more = families.clone();
        more.push("Symbols Nerd Font Mono".into());
        assert_eq!(nerd_font(&more).as_deref(), Some("Symbols Nerd Font Mono"));
        assert_eq!(nerd_font(&families[..5]).as_deref(), Some("Hack Nerd Font Mono"));
        assert_eq!(nerd_font(&families[..3]).as_deref(), Some("Iosevka Nerd Font"));
        assert_eq!(nerd_font(&families[..2]), None);
    }
}
