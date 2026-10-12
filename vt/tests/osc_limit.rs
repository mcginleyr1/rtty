//! A program that opens an OSC string and never ends it costs the engine no
//! more than `OSC_MAX`. Alone in its binary: the allocator it counts with
//! would count other tests too.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use rtty_vt::{OSC_MAX, Terminal};

/// The system allocator, counting the bytes live and the most ever live.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
        PEAK.fetch_max(live, Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn an_endless_osc_string_holds_no_more_than_the_limit() {
    let read = vec![b'x'; 65_536];
    let mut term = Terminal::new(24, 80, 0);
    term.advance(b"\x1b]0;");
    let before = LIVE.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    // 64 MiB of one string, in 64 KiB reads.
    for _ in 0..1024 {
        term.advance(&read);
    }
    let grown = PEAK.load(Ordering::SeqCst) - before;
    assert!(grown <= 2 * OSC_MAX, "the parser grew by {grown} bytes");
    term.advance(b"\x07after");
    assert_eq!(term.screen_row(0).text(), "after");
    assert!(term.take_events().is_empty(), "the string was dispatched");
}
