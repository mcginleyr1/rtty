mod keys;
mod palette;
mod session;
mod view;

use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, VoidListener, WindowSize};
use alacritty_terminal::term::Config;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::tty::Shell;
use alacritty_terminal::vte::ansi::Processor;
use gpui::{
    App, AppContext, Application, Bounds, FontFeatures, KeyBinding, TitlebarOptions, WindowBounds,
    WindowOptions, font, px, size,
};

use session::Session;
use view::{Copy, DecreaseFontSize, IncreaseFontSize, Paste, Quit, TerminalView};

const USAGE: &str =
    "usage: rtty [--font NAME] [--size PT] [--ligatures on|off] [--stats] [-e PROGRAM [ARGS...]]
       rtty --smoke              run a command through the pty pipeline headless and check it
       rtty --bench [COMMAND]    time the pty -> parser -> grid pipeline (default: seq)";

const START_SIZE: WindowSize =
    WindowSize { num_lines: 24, num_cols: 80, cell_width: 8, cell_height: 16 };

// macOS seq prints 1e+06 past a million without the explicit format.
const BENCH_COMMAND: &str = "seq -f %.0f 1 2000000";

struct Options {
    font: String,
    size: f32,
    ligatures: bool,
    stats: bool,
    command: Option<Shell>,
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut options = Options {
        font: "Menlo".to_owned(),
        size: 14.,
        ligatures: true,
        stats: false,
        command: None,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--smoke" => return smoke(),
            "--bench" => {
                let command = args.collect::<Vec<_>>().join(" ");
                return bench(if command.is_empty() { BENCH_COMMAND } else { &command });
            }
            "--stats" => options.stats = true,
            "--font" => match args.next() {
                Some(name) => options.font = name,
                None => return usage_error("--font needs a font family name"),
            },
            "--size" => match args.next().and_then(|points| points.parse().ok()) {
                Some(points) if points > 0. => options.size = points,
                _ => return usage_error("--size needs a positive number of points"),
            },
            "--ligatures" => match args.next().as_deref() {
                Some("on") => options.ligatures = true,
                Some("off") => options.ligatures = false,
                _ => return usage_error("--ligatures needs on or off"),
            },
            "-e" => match args.next() {
                Some(program) => {
                    options.command = Some(Shell::new(program, args.by_ref().collect()))
                }
                None => return usage_error("-e needs a program"),
            },
            other => return usage_error(&format!("unknown argument {other}")),
        }
    }
    run(options)
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("rtty: {message}\n{USAGE}");
    ExitCode::from(2)
}

fn run(options: Options) -> ExitCode {
    let (session, events) = match Session::spawn(options.command, START_SIZE) {
        Ok(spawned) => spawned,
        Err(err) => {
            eprintln!("rtty: cannot start the shell: {err}");
            return ExitCode::FAILURE;
        }
    };
    Application::new().run(move |cx: &mut App| {
        if !cx.text_system().all_font_names().contains(&options.font) {
            eprintln!("rtty: no font family named {:?}", options.font);
            std::process::exit(2);
        }
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-c", Copy, Some("Terminal")),
            KeyBinding::new("cmd-v", Paste, Some("Terminal")),
            KeyBinding::new("cmd-=", IncreaseFontSize, Some("Terminal")),
            KeyBinding::new("cmd--", DecreaseFontSize, Some("Terminal")),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx| cx.quit()).detach();
        let mut font = font(options.font);
        font.features = FontFeatures(Arc::new(if options.ligatures {
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
        cx.open_window(window_options, |window, cx| {
            cx.new(|cx| {
                TerminalView::new(
                    session,
                    events,
                    font,
                    px(options.size),
                    options.stats,
                    window,
                    cx,
                )
            })
        })
        .expect("open the terminal window");
        cx.activate(true);
    });
    ExitCode::SUCCESS
}

fn smoke() -> ExitCode {
    let (session, mut events) =
        Session::spawn(Some(session::command("printf hello-rtty; exit 7")), START_SIZE)
            .expect("spawn /bin/sh");
    let _ = session.reader.join().expect("pty reader thread");
    let exit_code = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            Event::ChildExit(status) => Some(status.code()),
            _ => None,
        })
        .flatten();
    let screen = session::screen(&session.term.lock());
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

    let mut term = Term::new(Config::default(), &TermSize::new(80, 24), VoidListener);
    let mut parser: Processor = Processor::new();
    let started = Instant::now();
    parser.advance(&mut term, &bytes);
    let parse = started.elapsed();

    let started = Instant::now();
    let (session, _events) =
        Session::spawn(Some(session::command(command)), START_SIZE).expect("spawn /bin/sh");
    let _ = session.reader.join().expect("pty reader thread");
    let pipeline = started.elapsed();

    let megabytes = bytes.len() as f64 / 1e6;
    let line = |label: &str, took: Duration| {
        let seconds = took.as_secs_f64();
        println!("  {label:<28} {:>8.1} ms {:>8.1} MB/s", seconds * 1000., megabytes / seconds);
    };
    println!(
        "bench: `{command}`, {megabytes:.1} MB after onlcr, 80x24 grid, 10k lines of scrollback"
    );
    line("command into a pipe", produce);
    line("vte parse into Term (memory)", parse);
    line("pty + reader thread + Term", pipeline);
    let same = session::screen(&term) == session::screen(&session.term.lock());
    println!("  pipeline screen matches the in-memory parse: {same}");
    if same { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
