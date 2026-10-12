mod config;
mod keys;
mod mouse;
mod pane;
mod render;
mod scheme;
mod script;
mod selection;
mod view;
mod window;

use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext, Application, Bounds, FontFeatures, KeyBinding, TitlebarOptions, WindowBounds,
    WindowOptions, font, px, size,
};
use portable_pty::CommandBuilder;
use rtty_vt::Terminal;

use config::Config;
use pane::{Pane, SCROLLBACK, Trace, shell_command};
use view::{Copy, DecreaseFontSize, IncreaseFontSize, Paste, Quit, Settings};
use window::TerminalWindow;

const USAGE: &str = "usage: rtty [--config FILE] [--font NAME] [--size PT] [--ligatures on|off] \
                     [--stats] [--trace FILE] [--script FILE [--activate]] [-e PROGRAM [ARGS...]]
       rtty --smoke              run a command through the pty pipeline headless and check it
       rtty --bench [COMMAND]    time the pty -> parser -> grid pipeline (default: seq)";

/// The terminal's size until the window has laid out its first frame.
const START_SIZE: (u16, u16) = (24, 80);

// macOS seq prints 1e+06 past a million without the explicit format.
const BENCH_COMMAND: &str = "seq -f %.0f 1 2000000";

/// The command line; `font`, `size` and `ligatures` override the config file's.
#[derive(Default)]
struct Options {
    config: Option<PathBuf>,
    font: Option<String>,
    size: Option<f32>,
    ligatures: Option<bool>,
    stats: bool,
    trace: Option<PathBuf>,
    script: Option<PathBuf>,
    /// A scripted window takes the keyboard only when asked to: the person at the machine may be
    /// typing elsewhere.
    activate: bool,
    command: Option<Vec<String>>,
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
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
            "--config" => match args.next() {
                Some(path) => options.config = Some(path.into()),
                None => return usage_error("--config needs a file"),
            },
            "--font" => match args.next() {
                Some(name) => options.font = Some(name),
                None => return usage_error("--font needs a font family name"),
            },
            "--size" => match args.next().and_then(|points| points.parse().ok()) {
                Some(points) => options.size = Some(points),
                None => return usage_error("--size needs a number of points"),
            },
            "--ligatures" => match args.next().as_deref() {
                Some("on") => options.ligatures = Some(true),
                Some("off") => options.ligatures = Some(false),
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

/// Reads `config.toml` (or `--config`), loads the color schemes, and runs the window.
fn run(options: Options) -> Result<(), String> {
    let config_home = config::home();
    let config = match (&options.config, &config_home) {
        (Some(path), _) if !path.exists() => {
            return Err(format!("{}: no such file", path.display()));
        }
        (Some(path), _) => Config::load(path)?,
        (None, Some(home)) => Config::load(&home.join("rtty/config.toml"))?,
        (None, None) => Config::default(),
    };
    let scheme_dirs: Vec<PathBuf> = config_home
        .iter()
        .flat_map(|home| [home.join("rtty/colors"), home.join("wezterm/colors")])
        .collect();
    let theme = config.theme(&scheme::load(&scheme_dirs)?)?;
    let family = options.font.or(config.font).unwrap_or_else(|| "Menlo".into());
    let font_size = options.size.or(config.size).unwrap_or(14.);
    let ligatures = options.ligatures.or(config.ligatures).unwrap_or(true);
    if font_size <= 0. || !font_size.is_finite() {
        return Err(format!("font size {font_size} is not a positive number of points"));
    }
    let read = |path: &PathBuf| {
        std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))
    };
    let script =
        options.script.as_ref().map(read).transpose()?.map(|source| script::parse(&source));
    let script = script.transpose()?;
    if options.activate && script.is_none() {
        return Err("--activate goes with --script".into());
    }
    let activate = script.is_none() || options.activate;
    let trace = options
        .trace
        .as_ref()
        .map(|path| Trace::create(path).map_err(|err| format!("{}: {err}", path.display())));
    let trace = trace.transpose()?;
    Application::new().run(move |cx: &mut App| {
        if !cx.text_system().all_font_names().contains(&family) {
            eprintln!("rtty: no font family named {family:?}");
            std::process::exit(2);
        }
        let (rows, cols) = START_SIZE;
        let scheme = theme.for_appearance(cx.window_appearance());
        let (pane, notices) =
            Pane::spawn(command(options.command), rows, cols, Some(scheme.as_ref()), trace)
                .unwrap_or_else(|err| {
                    eprintln!("rtty: cannot start the shell: {err}");
                    std::process::exit(1);
                });
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-c", Copy, Some("Terminal")),
            KeyBinding::new("cmd-v", Paste, Some("Terminal")),
            KeyBinding::new("cmd-=", IncreaseFontSize, Some("Terminal")),
            KeyBinding::new("cmd--", DecreaseFontSize, Some("Terminal")),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx| cx.quit()).detach();
        let mut font = font(family);
        font.features = FontFeatures(Arc::new(if ligatures {
            vec![("calt".into(), 1), ("liga".into(), 1)]
        } else {
            vec![("calt".into(), 0), ("liga".into(), 0), ("dlig".into(), 0)]
        }));
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(900.), px(600.)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions { title: Some("rtty".into()), ..Default::default() }),
            ..Default::default()
        };
        let settings = Settings { font, font_size: px(font_size), theme, stats: options.stats };
        let window = cx
            .open_window(window_options, |window, cx| {
                cx.new(|cx| TerminalWindow::new(pane, notices, settings, window, cx))
            })
            .expect("open the terminal window");
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
    let (mut pane, _notices) =
        Pane::spawn(shell_command("printf hello-rtty; exit 7"), rows, cols, None, None)
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
        Pane::spawn(shell_command(command), rows, cols, None, None).expect("spawn /bin/sh");
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
