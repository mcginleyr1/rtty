//! Nothing a program writes, however malformed, and no resize panics the
//! engine or its readers.

use proptest::prelude::*;
use rtty_vt::Terminal;

/// Bytes weighted towards the ones that steer the parser.
fn byte() -> impl Strategy<Value = u8> {
    prop_oneof![
        4 => any::<u8>(),
        2 => prop::sample::select(b"\x1b[]P\\;?:$ >=<!0123456789mHJKr@PXLMSTbhlnqtu\x07\x08\t\n\r".to_vec()),
        1 => prop::sample::select(vec![0xe2, 0x80, 0x8d, 0xf0, 0x9f, 0x98, 0x80, 0xcc, 0x81, 0xe6, 0x97, 0xa5]),
    ]
}

#[derive(Clone, Debug)]
enum Step {
    Feed(Vec<u8>),
    Resize(u16, u16),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        9 => prop::collection::vec(byte(), 0..200).prop_map(Step::Feed),
        1 => (0u16..30, 0u16..90).prop_map(|(rows, cols)| Step::Resize(rows, cols)),
    ]
}

/// A kitty graphics command of keys and values picked to reach every
/// action, and a payload of base64 characters, valid or not, which a cut may
/// end early.
fn graphics_command() -> impl Strategy<Value = Vec<u8>> {
    let key = prop::sample::select(b"aqftosvSOiIpmxywhXYcrCUzPde".to_vec());
    let values =
        ["T", "t", "p", "d", "q", "f", "A", "i", "c", "z", "-5", "24", "100", "2147483648"];
    let value = prop_oneof![
        (0u32..300).prop_map(|n| n.to_string()),
        prop::sample::select(values.to_vec()).prop_map(String::from),
    ];
    let pairs = prop::collection::vec((key, value), 0..8);
    let base64 = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";
    let payload = prop::collection::vec(prop::sample::select(base64.to_vec()), 0..64);
    (pairs, payload, any::<bool>()).prop_map(|(pairs, payload, whole)| {
        let keys: Vec<String> =
            pairs.iter().map(|(key, value)| format!("{}={value}", char::from(*key))).collect();
        let mut bytes = format!("\x1b_G{};", keys.join(",")).into_bytes();
        bytes.extend(payload);
        if whole {
            bytes.extend(b"\x1b\\");
        }
        bytes
    })
}

/// Unicode placeholders in a few colours, each with up to four diacritics
/// (the first three of the row and column list, and an acute that is not on
/// it).
fn placeholders() -> impl Strategy<Value = Vec<u8>> {
    let colors = prop::sample::select(vec!["38;5;1", "38;5;2", "38;2;0;0;1", "58:5:1", "0"]);
    let marks = prop::collection::vec(
        prop::sample::select(vec!['\u{305}', '\u{30D}', '\u{30E}', '\u{301}']),
        0..4,
    );
    prop::collection::vec((colors, marks), 0..12).prop_map(|cells| {
        let mut text = String::new();
        for (color, marks) in cells {
            text.push_str(&format!("\x1b[{color}m\u{10EEEE}"));
            text.extend(marks);
        }
        text.into_bytes()
    })
}

fn graphics_step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => graphics_command().prop_map(Step::Feed),
        2 => placeholders().prop_map(Step::Feed),
        4 => prop::collection::vec(byte(), 0..100).prop_map(Step::Feed),
        1 => (0u16..30, 0u16..90).prop_map(|(rows, cols)| Step::Resize(rows, cols)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn arbitrary_bytes_and_resizes_never_panic(
        (rows, cols) in (0u16..12, 0u16..40),
        scrollback in 0usize..50,
        steps in prop::collection::vec(step(), 0..40),
    ) {
        let mut term = Terminal::new(rows, cols, scrollback);
        for step in steps {
            match step {
                Step::Feed(bytes) => term.advance(&bytes),
                Step::Resize(rows, cols) => term.resize(rows, cols),
            }
            term.take_events();
            let damage = term.take_damage();
            let _ = term.redraw(&damage);
        }
        let _ = term.snapshot();
        let _ = term.contents();
        let _ = term.marks(0, u64::MAX);
        let top = term.screen_top();
        let _ = term.text((term.oldest_row(), 0), (top + 5, u16::MAX));
        let (rows, cols) = term.size();
        let mut copy = Terminal::new(rows, cols, 0);
        copy.advance(&term.snapshot());
    }

    /// The same with graphics on, its commands among the bytes.
    #[test]
    fn graphics_commands_never_panic(
        (rows, cols) in (0u16..12, 0u16..40),
        scrollback in 0usize..50,
        cell in prop::option::of((0u16..30, 0u16..30)),
        steps in prop::collection::vec(graphics_step(), 0..40),
    ) {
        let mut term = Terminal::new(rows, cols, scrollback);
        if let Some((width, height)) = cell {
            term.set_cell_pixels(width, height);
        }
        term.set_graphics(true);
        for step in steps {
            match step {
                Step::Feed(bytes) => term.advance(&bytes),
                Step::Resize(rows, cols) => term.resize(rows, cols),
            }
            term.take_events();
            let damage = term.take_damage();
            let _ = term.redraw(&damage);
            let top = term.screen_top();
            let _ = term.placements(term.oldest_row(), top + u64::from(term.size().0));
        }
        let _ = term.snapshot();
        term.set_graphics(false);
        term.advance(b"\x1b_Ga=T,f=24,s=1,v=1;AAAA\x1b\\");
        prop_assert!(term.images().is_empty());
    }
}
