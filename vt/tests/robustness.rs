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
}
