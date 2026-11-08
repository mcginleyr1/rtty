//! The heap bytes a `Terminal` holds after a session, counted by a global
//! allocator (bytes asked for, not what the allocator rounds them up to):
//! `cargo test --release --test memory -- --ignored --nocapture`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use rtty_vt::Terminal;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size(), Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE.fetch_add(new_size, Relaxed);
        LIVE.fetch_sub(layout.size(), Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

const ROWS: u16 = 24;
const SCROLLBACK: usize = 10_000;
/// Enough lines to fill the scrollback and the screen.
const LINES: usize = 12_000;

/// `seq 1 LINES`.
fn seq() -> Vec<u8> {
    (1..=LINES).map(|i| format!("{i}\r\n")).collect::<String>().into_bytes()
}

/// Lines exactly `cols` wide, a colour change every six cells.
fn dense(cols: usize) -> Vec<u8> {
    let mut out = String::new();
    for i in 0..LINES {
        for col in 0..cols {
            if col % 6 == 0 {
                let (fg, bg) = ((i + col) % 256, (i * 7 + col) % 256);
                out.push_str(&format!("\x1b[1;38;5;{fg};48;5;{bg}m"));
            }
            out.push(char::from(b'a' + (col % 26) as u8));
        }
        out.push_str("\x1b[0m\r\n");
    }
    out.into_bytes()
}

/// Three commands at a shell that marks its prompts, and a fourth prompt.
fn prompts() -> Vec<u8> {
    let mut out = String::new();
    for i in 0..3 {
        out.push_str("\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07");
        out.push_str(&format!("file{i}  notes.txt  src\r\n\x1b]133;D;0\x07"));
    }
    out.push_str("\x1b]133;A\x07$ \x1b]133;B\x07");
    out.into_bytes()
}

/// The bytes a terminal of `cols` columns holds once `stream` is fed to it in
/// 8 KiB reads, as a pty reader feeds it, and the rows it holds.
fn held(cols: u16, stream: &[u8]) -> (usize, u64) {
    let before = LIVE.load(Relaxed);
    let mut term = Terminal::new(ROWS, cols, SCROLLBACK);
    for read in stream.chunks(8192) {
        term.advance(read);
        drop(term.take_events());
    }
    let bytes = LIVE.load(Relaxed) - before;
    (bytes, term.screen_top() + u64::from(ROWS) - term.oldest_row())
}

#[test]
#[ignore = "a measurement: run with --release --nocapture"]
fn bytes_held_after_a_session() {
    let sessions = [
        ("seq", 80, seq()),
        ("seq", 200, seq()),
        ("dense colour", 80, dense(80)),
        ("dense colour", 200, dense(200)),
        ("empty lines", 80, b"\r\n".repeat(LINES)),
        ("empty lines", 200, b"\r\n".repeat(LINES)),
        ("a few prompts", 80, prompts()),
        ("a few prompts", 200, prompts()),
    ];
    println!("{:<14} {:>5} {:>7} {:>12} {:>9}", "session", "cols", "rows", "bytes", "per row");
    for (name, cols, stream) in &sessions {
        let (bytes, rows) = held(*cols, stream);
        let per_row = bytes as u64 / rows;
        println!("{name:<14} {cols:>5} {rows:>7} {bytes:>12} {per_row:>9}");
        // A scrollback row holds what it shows, whatever the terminal's width.
        assert!(*name != "seq" || per_row < 256, "{name} at {cols} columns: {per_row} a row");
    }
}
