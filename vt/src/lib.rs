//! A terminal state engine: the bytes a program writes in, a grid with
//! scrollback, modes, replies and events out. No IO.
//!
//! The byte-level parser is `vte`'s DEC state machine; everything above it —
//! dispatch, the grid, the modes, the answers to queries — is this crate.
//!
//! ```
//! use rtty_vt::{Event, Terminal};
//!
//! let mut term = Terminal::new(24, 80, 10_000);
//! term.advance(b"\x1b[1mhello\x1b[0m\r\n\x1b[6n");
//! assert_eq!(term.screen_row(0).text(), "hello");
//! assert_eq!(term.take_events(), [Event::Reply(b"\x1b[2;1R".to_vec())]);
//! ```
//!
//! # Rows
//!
//! Every row the primary screen ever showed has an absolute index:
//! `screen_top()` is the index of the visible screen's first row — the
//! number of rows committed to the scrollback — and `oldest_row()` the oldest
//! still held. Neither ever decreases, so a reader can tail the scrollback
//! with `rows(since, screen_top())` and OSC 133 marks (`marks`) name rows that
//! stay put while output scrolls.
//!
//! # Followers
//!
//! `snapshot()` is the escape bytes that bring a fresh terminal of the same
//! size to this one's visible state, both screens included; after it,
//! `redraw(&take_damage())` brings that follower level again with only the
//! rows that changed. A switch of screens or a resize calls for a new
//! snapshot.
//!
//! # Unicode
//!
//! A cell holds one grapheme cluster. Width is `unicode-width`'s per
//! character, as `wcwidth` gives it to the programs whose output this is:
//! a cluster takes the columns of its first character. Zero-width
//! characters (combining marks, variation selectors, ZWJ) join the cell
//! before the cursor. A character with a width of its own joins only where
//! `unicode-segmentation` keeps it in the cluster and it continues an emoji
//! sequence — after a ZWJ, an emoji modifier, the second regional indicator
//! of a flag; a narrow cluster that gains one becomes two columns wide.
//!
//! VS16 (U+FE0F) joins its base and never widens it: `wcwidth` gives it no
//! width, so a program's idea of the cursor would part from the grid's. A
//! renderer may draw the emoji presentation within the base's cells.

mod cell;
mod dispatch;
mod event;
mod grid;
mod modes;
mod reflow;
mod row;
mod snapshot;
mod term;

pub use cell::{Attrs, Cell, Color, Underline};
pub use dispatch::XTERM_VERSION;
pub use event::{Event, Mark, MarkKind, ModeChange};
pub use modes::{CursorShape, CursorStyle, Modes, MouseEncoding, MouseMode};
pub use row::{Hyperlink, Row};
pub use term::{BACKGROUND, CURSOR, ColorScheme, FOREGROUND, Rgb, Terminal};
