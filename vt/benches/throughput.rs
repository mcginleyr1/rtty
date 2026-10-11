//! Throughput in MB/s of rtty-vt, alacritty_terminal and vt100 fed the same
//! streams in 8 KiB reads, as a pty reader feeds them:
//! `cargo bench --manifest-path vt/Cargo.toml`.

use std::time::{Duration, Instant};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi;

const READ: usize = 8192;
const SCROLLBACK: usize = 10_000;

/// `seq 1 N`: short lines, every one a scroll.
fn seq(n: usize) -> Vec<u8> {
    (1..=n).map(|i| format!("{i}\r\n")).collect::<String>().into_bytes()
}

/// rtty's dense.awk: full-width lines with a colour and attribute change on
/// every word.
fn dense(lines: usize) -> Vec<u8> {
    let mut out = String::new();
    for i in 0..lines {
        for j in 0..60 {
            let attr = if j % 2 == 1 { 1 } else { 3 };
            out.push_str(&format!(
                "\x1b[{attr};38;5;{};48;5;{}mword{j:02}",
                (i + j) % 256,
                (i * 7 + j) % 256
            ));
        }
        out.push_str("\x1b[0m\n");
    }
    out.into_bytes()
}

/// Unicode text: CJK, combining marks and emoji among ASCII.
fn unicode(lines: usize) -> Vec<u8> {
    let line = "日本語のテキスト e\u{301}t\u{e9} 😀 naïve café — ünïcödé 中文字符 ";
    (0..lines).map(|i| format!("{i} {line}{line}\r\n")).collect::<String>().into_bytes()
}

/// The best of `runs` timings of a fresh engine fed `stream`, in MB/s.
fn best(runs: usize, stream: &[u8], mut feed: impl FnMut(&[u8])) -> f64 {
    let best = (0..runs)
        .map(|_| {
            let start = Instant::now();
            feed(stream);
            start.elapsed()
        })
        .min()
        .unwrap_or(Duration::MAX);
    stream.len() as f64 / best.as_secs_f64() / 1e6
}

fn main() {
    let (rows, cols) = (24u16, 80u16);
    let streams =
        [("seq", seq(2_000_000)), ("dense SGR", dense(5_000)), ("unicode", unicode(100_000))];
    // `cargo bench -- seq` runs the streams whose names hold `seq`.
    let filter = std::env::args().skip(1).find(|arg| !arg.starts_with('-')).unwrap_or_default();
    println!(
        "{:<12} {:>8} {:>14} {:>18} {:>10}",
        "stream", "MB", "rtty-vt MB/s", "alacritty MB/s", "vt100 MB/s"
    );
    for (name, stream) in streams.iter().filter(|(name, _)| name.contains(filter.as_str())) {
        let ours = best(5, stream, |stream| {
            let mut term = rtty_vt::Terminal::new(rows, cols, SCROLLBACK);
            for read in stream.chunks(READ) {
                term.advance(read);
                std::hint::black_box(term.take_events());
            }
            std::hint::black_box(term.screen_top());
        });
        let alacritty = best(5, stream, |stream| {
            let config = Config { scrolling_history: SCROLLBACK, ..Config::default() };
            let size = TermSize::new(usize::from(cols), usize::from(rows));
            let mut term = Term::new(config, &size, VoidListener);
            let mut parser: ansi::Processor = ansi::Processor::new();
            for read in stream.chunks(READ) {
                parser.advance(&mut term, read);
            }
            std::hint::black_box(term.grid().cursor.point);
        });
        let vt100 = best(5, stream, |stream| {
            let mut parser = vt100::Parser::new(rows, cols, SCROLLBACK);
            for read in stream.chunks(READ) {
                parser.process(read);
            }
            std::hint::black_box(parser.screen().cursor_position());
        });
        let mb = stream.len() as f64 / 1e6;
        println!("{name:<12} {mb:>8.1} {ours:>14.1} {alacritty:>18.1} {vt100:>10.1}");
    }
}
