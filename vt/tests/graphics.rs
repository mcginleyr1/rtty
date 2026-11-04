//! The kitty graphics protocol, from its specification: transmission in each
//! format, medium and chunking, queries and quiet modes, placements and where
//! they go, deletion by every specifier, the storage quota, placements
//! riding their rows through scrolling, trimming, erasing and reflow, and
//! Unicode placeholders.

use std::sync::Arc;

use rtty_vt::{Event, GRAPHICS_QUOTA, Image, PLACEHOLDER, Placement, Terminal};

/// Standard base64, padded.
fn encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            let c = ALPHABET[(n >> (18 - 6 * i) & 63) as usize];
            out.push(if i <= chunk.len() { char::from(c) } else { '=' });
        }
    }
    out
}

/// A graphics command: `ESC _ G control ; base64(payload) ESC \`.
fn command(control: &str, payload: &[u8]) -> Vec<u8> {
    let payload = if payload.is_empty() { String::new() } else { format!(";{}", encode(payload)) };
    format!("\x1b_G{control}{payload}\x1b\\").into_bytes()
}

/// A 10×20-row terminal of 10×20-pixel cells, graphics on.
fn term() -> Terminal {
    let mut term = Terminal::new(10, 20, 100);
    term.set_cell_pixels(10, 20);
    term.set_graphics(true);
    term
}

fn replies(term: &mut Terminal, bytes: &[u8]) -> Vec<String> {
    term.advance(bytes);
    term.take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Reply(bytes) => Some(String::from_utf8(bytes).unwrap()),
            _ => None,
        })
        .collect()
}

/// `w`×`h` pixels of one RGB colour.
fn rgb(w: usize, h: usize, pixel: [u8; 3]) -> Vec<u8> {
    pixel.repeat(w * h)
}

fn png(w: u32, h: u32, color: png::ColorType, pixels: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(pixels).unwrap();
    writer.finish().unwrap();
    out
}

fn placements(term: &Terminal) -> Vec<Placement> {
    let top = term.screen_top();
    term.placements(term.oldest_row(), top + u64::from(term.size().0))
}

/// Each placement as (image id, row, column).
fn spots(term: &Terminal) -> Vec<(u32, u64, u16)> {
    placements(term).iter().map(|p| (p.image.id, p.row, p.col)).collect()
}

fn ids(term: &Terminal) -> Vec<u32> {
    term.images().iter().map(|image| image.id).collect()
}

#[test]
fn with_graphics_off_commands_are_dropped_as_every_apc_string_is() {
    let mut term = Terminal::new(10, 20, 100);
    term.set_cell_pixels(10, 20);
    let mut bytes = b"a".to_vec();
    bytes.extend(command("a=T,i=1,f=24,s=1,v=1", &[1, 2, 3]));
    bytes.extend(command("a=q,i=2,f=24,s=1,v=1", &[1, 2, 3]));
    bytes.extend(b"b");
    assert!(replies(&mut term, &bytes).is_empty());
    assert_eq!(term.screen_row(0).text(), "ab");
    assert_eq!(term.cursor_position(), (0, 2));
    assert!(term.images().is_empty() && placements(&term).is_empty());
}

/// The support probe the specification gives: a query answers, and stores nothing.
#[test]
fn a_query_answers_and_holds_nothing() {
    let mut term = term();
    let probe = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c";
    assert_eq!(replies(&mut term, probe), ["\x1b_Gi=31;OK\x1b\\", "\x1b[?62;22c"]);
    assert!(term.images().is_empty());
    let bad = replies(&mut term, &command("a=q,i=32,f=24,s=2,v=1", &[0; 3]));
    assert_eq!(bad, ["\x1b_Gi=32;EINVAL:the data is not s by v pixels\x1b\\"]);
}

#[test]
fn rgb_rgba_and_png_decode_to_rgba() {
    let mut term = term();
    let ok = |id| vec![format!("\x1b_Gi={id};OK\x1b\\")];
    assert_eq!(replies(&mut term, &command("i=1,f=24,s=2,v=1", &[1, 2, 3, 4, 5, 6])), ok(1));
    assert_eq!(replies(&mut term, &command("i=2,s=1,v=1", &[1, 2, 3, 4])), ok(2));
    let grey = png(2, 1, png::ColorType::GrayscaleAlpha, &[10, 20, 30, 40]);
    assert_eq!(replies(&mut term, &command("i=3,f=100", &grey)), ok(3));
    let images: Vec<(u32, u32, &[u8])> =
        term.images().iter().map(|i| (i.width, i.height, &i.rgba[..])).collect();
    assert_eq!(
        images,
        [
            (2, 1, &[1, 2, 3, 255, 4, 5, 6, 255][..]),
            (1, 1, &[1, 2, 3, 4][..]),
            (2, 1, &[10, 10, 10, 20, 30, 30, 30, 40][..]),
        ]
    );
    let bad = replies(&mut term, &command("i=4,f=100", b"not a png"));
    assert_eq!(bad, ["\x1b_Gi=4;EINVAL:not a PNG this terminal reads\x1b\\"]);
}

#[test]
fn compressed_data_inflates() {
    let mut term = term();
    let data = rgb(4, 4, [9, 8, 7]);
    let deflated = miniz_oxide::deflate::compress_to_vec_zlib(&data, 6);
    let reply = replies(&mut term, &command("i=5,f=24,s=4,v=4,o=z", &deflated));
    assert_eq!(reply, ["\x1b_Gi=5;OK\x1b\\"]);
    assert_eq!(&term.images()[0].rgba[..4], &[9, 8, 7, 255]);
    let bad = replies(&mut term, &command("i=6,f=24,s=4,v=4,o=z", &data));
    assert_eq!(bad, ["\x1b_Gi=6;EINVAL:the data does not inflate\x1b\\"]);
}

/// A transmission in chunks of base64, each a multiple of 4 but the last; the image is placed
/// where the cursor is when the last arrives.
#[test]
fn chunks_add_up_and_the_last_one_places() {
    let mut term = term();
    let encoded = encode(&rgb(3, 2, [7, 7, 7]));
    let (first, rest) = encoded.split_at(8);
    let (second, last) = rest.split_at(8);
    let mut bytes = format!("\x1b_Ga=T,i=9,f=24,s=3,v=2,m=1;{first}\x1b\\").into_bytes();
    bytes.extend(format!("\x1b_Gm=1;{second}\x1b\\xy\x1b_Gm=0;{last}\x1b\\").into_bytes());
    assert_eq!(replies(&mut term, &bytes), ["\x1b_Gi=9;OK\x1b\\"]);
    assert_eq!(spots(&term), [(9, 0, 2)]);
    assert_eq!(term.images()[0].rgba.len(), 3 * 2 * 4);
    // A command with an action of its own drops a transmission left unfinished.
    term.advance(&command("a=t,i=10,f=24,s=3,v=2,m=1", &[0; 6]));
    term.advance(&command("a=d,d=a", b""));
    term.advance(&command("m=0", &[0; 12]));
    assert_eq!(ids(&term), [9]);
}

/// Bytes split anywhere, even one at a time, run the same commands.
#[test]
fn a_command_split_across_reads_runs_once_whole() {
    let mut whole = term();
    let mut bytes = b"ab".to_vec();
    bytes.extend(command("a=T,i=3,f=24,s=1,v=1", &[1, 2, 3]));
    bytes.extend(b"\x1b\x05_Gi=4,a=q,f=24,s=1,v=1;AQID\x1b\\cd");
    let want = replies(&mut whole, &bytes);
    let mut split = term();
    let mut got = Vec::new();
    for byte in &bytes {
        got.extend(replies(&mut split, std::slice::from_ref(byte)));
    }
    assert_eq!(got, want);
    assert_eq!(want, ["\x1b_Gi=3;OK\x1b\\", "\x1b_Gi=4;OK\x1b\\"]);
    assert_eq!((split.screen_row(0).text(), split.cursor_position()), ("ab cd".into(), (0, 5)));
    assert_eq!(spots(&split), spots(&whole));
}

#[test]
fn an_apc_string_past_its_limit_is_dropped_and_the_stream_goes_on() {
    let mut term = term();
    let mut bytes = b"\x1b_Ga=T,i=1,f=24,s=1,v=1;".to_vec();
    bytes.resize(bytes.len() + rtty_vt::APC_MAX + 4, b'A');
    bytes.extend(b"\x1b\\after");
    assert!(replies(&mut term, &bytes).is_empty());
    assert_eq!(term.screen_row(0).text(), "after");
    assert!(term.images().is_empty());
}

/// `command` without the `\` of its ST.
fn unended(control: &str, payload: &[u8]) -> Vec<u8> {
    let mut bytes = command(control, payload);
    bytes.pop();
    bytes
}

/// A command runs when ST ends it and only then: an ESC that starts anything else drops it, with
/// nothing held, answered or read; a read that ends on the ESC waits for the next.
#[test]
fn only_st_runs_a_command() {
    let mut term = term();
    let mut bytes = unended("a=T,i=1,f=24,s=1,v=1", &[1, 2, 3]);
    bytes.extend(b"[1mbold");
    assert!(replies(&mut term, &bytes).is_empty());
    assert_eq!(term.screen_row(0).text(), "bold", "the CSI after the ESC runs");
    assert!(term.images().is_empty() && placements(&term).is_empty());

    let dir = std::env::temp_dir()
        .join(format!("rtty-vt-tty-graphics-protocol-st-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("image.rgb");
    std::fs::write(&file, rgb(1, 1, [4, 5, 6])).unwrap();
    let mut cut = unended("i=2,t=t,f=24,s=1,v=1", file.to_str().unwrap().as_bytes());
    cut.extend(b"[m");
    assert!(replies(&mut term, &cut).is_empty());
    assert!(file.exists(), "neither read nor deleted");
    std::fs::remove_file(file).unwrap();
    std::fs::remove_dir(dir).unwrap();

    let query = command("a=q,i=3,f=24,s=1,v=1", &[0; 3]);
    let (head, tail) = query.split_at(query.len() - 1);
    assert!(replies(&mut term, head).is_empty(), "the ESC alone runs nothing");
    assert_eq!(replies(&mut term, tail), ["\x1b_Gi=3;OK\x1b\\"]);

    // An ESC _ opens the next string: the one it cut short is dropped.
    let mut two = unended("a=q,i=4,f=24,s=1,v=1", &[0; 3]);
    two.extend(&command("a=q,i=5,f=24,s=1,v=1", &[0; 3])[1..]);
    assert_eq!(replies(&mut term, &two), ["\x1b_Gi=5;OK\x1b\\"]);
}

/// While the alternate screen is up, the primary screen's placements, those in its scrollback
/// reaching onto the screen included, neither draw nor go with the alternate screen's deletes;
/// cells the alternate screen loses to a narrowing take their images with them.
#[test]
fn the_alternate_screen_keeps_to_its_own_placements() {
    let mut term = Terminal::new(4, 10, 100);
    term.set_cell_pixels(10, 20);
    term.set_graphics(true);
    term.advance(b"\x1b[2;1H");
    term.advance(&command("a=T,i=1,f=24,s=10,v=60,C=1", &rgb(10, 60, [0; 3])));
    term.advance(b"\x1b[4;1H\n\n\n");
    assert_eq!(term.screen_top(), 3);
    let shown = |term: &Terminal| term.placements(term.screen_top(), term.screen_top() + 4);
    assert_eq!(shown(&term).len(), 1, "from the scrollback onto the screen");
    term.advance(b"\x1b[?1049h");
    assert!(shown(&term).is_empty(), "under the alternate screen");
    term.advance(&command("a=d,d=A", b""));
    term.advance(b"\x1b[1;9H");
    term.advance(&command("a=T,i=2,f=24,s=10,v=20,C=1", &rgb(10, 20, [0; 3])));
    assert_eq!(shown(&term).iter().map(|p| p.image.id).collect::<Vec<_>>(), [2]);
    term.resize(4, 5);
    term.resize(4, 10);
    assert!(shown(&term).is_empty(), "its cell was cut");
    term.advance(b"\x1b[?1049l");
    assert_eq!(shown(&term).iter().map(|p| p.image.id).collect::<Vec<_>>(), [1]);
}

/// One small image placed over and over, or many small images, cost next to nothing against the
/// quota: 4096 placements and 4096 images bound them, the oldest going first.
#[test]
fn placements_and_images_are_bounded_in_number() {
    let mut term = term();
    term.advance(&command("a=T,i=2,f=24,s=1,v=1,C=1,q=2", &[0; 3]));
    term.advance(&command("i=1,f=24,s=1,v=1,q=2", &[0; 3]));
    term.advance(&command("a=p,i=1,C=1,q=2", b"").repeat(4096));
    let ids: Vec<u32> = placements(&term).iter().map(|p| p.image.id).collect();
    assert_eq!(ids.len(), 4096);
    assert!(ids.iter().all(|&id| id == 1), "image 2's placement, the oldest, went");
    assert_eq!(term.images().len(), 2, "its image stays, to be placed again");

    let mut term = self::term();
    let images: Vec<u8> =
        (1..=4097).flat_map(|id| command(&format!("i={id},f=24,s=1,v=1,q=2"), &[0; 3])).collect();
    term.advance(&images);
    assert_eq!(term.images().len(), 4096);
    assert_eq!(term.images()[0].id, 2, "image 1, the oldest, went");
}

#[test]
fn quiet_modes_and_ids() {
    let mut term = term();
    let pixel = [1, 2, 3];
    assert!(replies(&mut term, &command("i=1,f=24,s=1,v=1,q=1", &pixel)).is_empty());
    let error = command("i=2,f=24,s=2,v=2,q=1", &pixel);
    assert_eq!(
        replies(&mut term, &error),
        ["\x1b_Gi=2;EINVAL:the data is not s by v pixels\x1b\\"]
    );
    assert!(replies(&mut term, &command("i=2,f=24,s=2,v=2,q=2", &pixel)).is_empty());
    // No id, no answer.
    assert!(replies(&mut term, &command("f=24,s=1,v=1", &pixel)).is_empty());
    assert_eq!(
        replies(&mut term, &command("i=7,I=8,f=24,s=1,v=1", &pixel)),
        ["\x1b_Gi=7,I=8;EINVAL:both i and I\x1b\\"]
    );
    // An image number gets an id of the terminal's, from 2³¹ up; `I` names the newest.
    let first = replies(&mut term, &command("I=13,f=24,s=1,v=1", &pixel));
    let second = replies(&mut term, &command("I=13,f=24,s=1,v=1", &pixel));
    assert_eq!(first, ["\x1b_Gi=2147483648,I=13;OK\x1b\\"]);
    assert_eq!(second, ["\x1b_Gi=2147483649,I=13;OK\x1b\\"]);
    term.advance(&command("a=p,I=13,q=2", b""));
    assert_eq!(spots(&term), [(2147483649, 0, 0)]);
    assert_eq!(
        replies(&mut term, &command("a=p,i=99", b"")),
        ["\x1b_Gi=99;ENOENT:no such image\x1b\\"]
    );
    assert_eq!(
        replies(&mut term, &command("a=f,i=1", b"")),
        ["\x1b_Gi=1;EINVAL:animation is not supported\x1b\\"]
    );
}

#[test]
fn a_feature_query_names_what_is_supported() {
    let mut term = term();
    let reply = replies(&mut term, b"\x1b_Ga=q,i=31,e=T,C=1,f=100,o=z,t=s,X=0\x1b\\");
    assert_eq!(reply, ["\x1b_Gi=31,e=T,C=1,f=100,o=z;OK\x1b\\"]);
    assert_eq!(replies(&mut term, b"\x1b_Ga=q,i=32,e=f,r=1\x1b\\"), ["\x1b_Gi=32;OK\x1b\\"]);
    assert_eq!(
        replies(&mut term, b"\x1b_Ga=q,i=33,e=d,d=Z\x1b\\"),
        ["\x1b_Gi=33,e=d,d=Z;OK\x1b\\"]
    );
}

/// At its own size an image covers the cells its pixels reach (a 25×30 image, 3×2 cells of
/// 10×20); `c` and `r` scale it, one of them keeping its shape, both fitting it centred.
#[test]
fn placements_cover_cells_as_the_keys_say() {
    let mut term = term();
    term.advance(&command("i=1,f=24,s=25,v=30", &rgb(25, 30, [0, 0, 0])));
    let place = |term: &mut Terminal, keys: &str| {
        term.advance(b"\x1b[H");
        term.advance(&command(&format!("a=p,i=1,{keys}"), b""));
        let placement = placements(term).pop().unwrap();
        term.advance(&command("a=d", b""));
        term.take_events();
        (placement.dest, placement.source, term.cursor_position())
    };
    assert_eq!(place(&mut term, "C=0"), ([0., 0., 2.5, 1.5], [0., 0., 25., 30.], (1, 3)));
    assert_eq!(place(&mut term, "c=5"), ([0., 0., 5., 3.], [0., 0., 25., 30.], (2, 5)));
    assert_eq!(place(&mut term, "r=3"), ([0., 0., 5., 3.], [0., 0., 25., 30.], (2, 5)));
    // 6×2 cells (60×40 pixels): the height decides, 33⅓ pixels wide, centred.
    let (dest, _, cursor) = place(&mut term, "c=6,r=2");
    assert_eq!(cursor, (1, 6));
    assert!((dest[0] - 1.333).abs() < 0.01 && (dest[2] - 3.333).abs() < 0.01, "{dest:?}");
    assert_eq!((dest[1], dest[3]), (0., 2.));
    assert_eq!(
        place(&mut term, "x=5,y=10,w=10,h=10,X=4,Y=6,C=1"),
        ([0.4, 0.3, 1., 0.5], [5., 10., 10., 10.], (0, 0))
    );
    let empty = replies(&mut term, &command("a=p,i=1,x=100", b""));
    assert_eq!(empty, ["\x1b_Gi=1;EINVAL:the source rectangle is empty\x1b\\"]);
}

/// The cursor goes to the last row the image covers, scrolling as a linefeed would, past its
/// right edge; the placement rides its row into the scrollback.
#[test]
fn placing_moves_the_cursor_and_scrolls() {
    let mut term = term();
    term.advance(b"\x1b[9;3H");
    term.advance(&command("a=T,i=1,f=24,s=10,v=60", &rgb(10, 60, [5, 5, 5])));
    assert_eq!(term.cursor_position(), (9, 3));
    assert_eq!(term.screen_top(), 1);
    assert_eq!(spots(&term), [(1, 8, 2)]);
}

#[test]
fn the_same_image_and_placement_id_replace_a_placement() {
    let mut term = term();
    term.advance(&command("i=1,f=24,s=1,v=1", &[1, 1, 1]));
    term.advance(&command("a=p,i=1,p=5", b""));
    term.advance(&command("a=p,i=1,p=6", b""));
    term.advance(b"\x1b[3;4H");
    term.take_events();
    assert_eq!(replies(&mut term, &command("a=p,i=1,p=5", b"")), ["\x1b_Gi=1,p=5;OK\x1b\\"]);
    let mut spots = spots(&term);
    spots.sort();
    assert_eq!(spots, [(1, 0, 1), (1, 2, 3)]);
    // Sent again, an image loses its placements.
    term.advance(&command("i=1,f=24,s=1,v=1", &[2, 2, 2]));
    assert!(placements(&term).is_empty());
    assert_eq!(term.images().len(), 1);
}

#[test]
fn placements_stack_by_z_then_image_id() {
    let mut term = term();
    for (id, z) in [(3, 0), (1, 5), (2, -1), (4, i32::MIN), (0, 0)] {
        term.advance(&command(&format!("a=T,i={id},z={z},f=24,s=1,v=1,C=1"), &[0; 3]));
    }
    let order: Vec<(i32, u32)> = placements(&term).iter().map(|p| (p.z, p.image.id)).collect();
    assert_eq!(order, [(i32::MIN, 4), (-1, 2), (0, 0), (0, 3), (5, 1)]);
}

/// Three images of 1×1 cell at rows 0, 1, 2, columns 0, 1, 2; then each specifier.
fn diagonal() -> Terminal {
    let mut term = term();
    for (id, z) in [(1, 1), (2, 2), (3, 3)] {
        term.advance(&command(&format!("a=T,i={id},p=1,z={z},f=24,s=1,v=1"), &[0; 3]));
        term.advance(b"\r\n");
        term.advance(&format!("\x1b[{}G", id + 1).into_bytes());
    }
    term.advance(b"\x1b[H");
    term
}

#[test]
fn deletes_pick_placements_by_each_specifier() {
    let left = |delete: &str| {
        let mut term = diagonal();
        term.advance(&command(delete, b""));
        let mut ids: Vec<u32> = placements(&term).iter().map(|p| p.image.id).collect();
        ids.sort();
        ids
    };
    assert_eq!(left("a=d"), [] as [u32; 0]);
    assert_eq!(left("a=d,d=i,i=2"), [1, 3]);
    assert_eq!(left("a=d,d=i,i=2,p=7"), [1, 2, 3], "no placement 7");
    assert_eq!(left("a=d,d=c"), [2, 3], "the cursor is home");
    assert_eq!(left("a=d,d=p,x=2,y=2"), [1, 3]);
    assert_eq!(left("a=d,d=q,x=3,y=3,z=2"), [1, 2, 3]);
    assert_eq!(left("a=d,d=q,x=3,y=3,z=3"), [1, 2]);
    assert_eq!(left("a=d,d=x,x=3"), [1, 2]);
    assert_eq!(left("a=d,d=y,y=1"), [2, 3]);
    assert_eq!(left("a=d,d=z,z=2"), [1, 3]);
    assert_eq!(left("a=d,d=r,x=2,y=3"), [1]);
    assert_eq!(left("a=d,d=f"), [1, 2, 3], "no frames to delete");
}

/// Lower case keeps the image to place again; upper case forgets it once it is placed nowhere,
/// the scrollback included.
#[test]
fn upper_case_deletes_free_images_placed_nowhere_else() {
    let mut term = diagonal();
    term.advance(&command("a=d,d=i,i=2", b""));
    assert_eq!(ids(&term), [1, 2, 3]);
    term.advance(&command("a=d,d=I,i=2", b""));
    assert_eq!(ids(&term), [1, 3]);
    // Image 1 placed again at the bottom, the diagonal scrolled into the scrollback.
    term.advance(b"\x1b[10;1H");
    term.advance(&command("a=p,i=1,p=2", b""));
    term.advance(b"\n\n\n");
    term.advance(&command("a=d,d=A", b""));
    assert_eq!(ids(&term), [1, 3], "1 is placed in the scrollback still");
    assert_eq!(spots(&term), [(1, 0, 0), (3, 2, 2)]);
    term.advance(&command("a=d,d=I,i=1,p=1", b""));
    assert_eq!(ids(&term), [3]);
    term.advance(&command("a=d,d=I,i=3", b""));
    assert!(term.images().is_empty() && placements(&term).is_empty());
}

#[test]
fn image_numbers_delete_the_newest() {
    let mut term = term();
    term.advance(&command("a=T,I=5,f=24,s=1,v=1,q=2", &[0; 3]));
    term.advance(&command("a=T,I=5,f=24,s=1,v=1,q=2", &[0; 3]));
    term.advance(&command("a=d,d=N,I=5", b""));
    let left: Vec<u32> = term.images().iter().map(|image| image.id).collect();
    assert_eq!(left, [1 << 31]);
}

/// Placements belong to their rows: they scroll into the scrollback, go when it trims them, stay
/// under ED 0 and 1, and go with ED 2 and 3, the alternate screen's clearing and RIS.
#[test]
fn placements_ride_their_rows() {
    let mut term = Terminal::new(3, 10, 2);
    term.set_cell_pixels(10, 20);
    term.set_graphics(true);
    term.advance(&command("a=T,i=1,f=24,s=1,v=1", &[0; 3]));
    term.advance(b"\r\n\n\n");
    assert_eq!(spots(&term), [(1, 0, 0)], "in the scrollback");
    term.advance(b"\n\n");
    assert!(placements(&term).is_empty(), "trimmed with its row");

    let mut term = self::term();
    let image = command("a=T,i=1,f=24,s=1,v=1", &[0; 3]);
    term.advance(&image);
    term.advance(b"\x1b[2;1H\x1b[J\x1b[1J\x1b[2K\x1b[3;1H\x1b[1J");
    assert_eq!(spots(&term), [(1, 0, 0)], "erasing text leaves images");
    term.advance(b"\x1b[H\x1b[2J");
    assert!(placements(&term).is_empty());
    assert_eq!(ids(&term), [1], "ED 2 keeps the image to place again");
    term.advance(b"x\x1b[H");
    term.advance(&image);
    term.advance(b"\x1b[?1049h\x1b[3;1H");
    term.advance(&image);
    assert_eq!(spots(&term), [(1, 2, 0)], "the alternate screen's own");
    term.advance(b"\x1b[?1049l\x1b[?1049h");
    assert!(placements(&term).is_empty(), "cleared with the alternate screen");
    term.advance(b"\x1b[?1049l");
    term.advance(&image);
    term.advance(b"\x1bc");
    assert!(placements(&term).is_empty() && term.images().is_empty(), "RIS");
}

/// Reflow carries a placement with the cell it starts on; the image keeps its size in cells.
#[test]
fn a_placement_reflows_with_its_cell() {
    let mut term = term();
    term.advance(b"abcdefghijklmnopqrstuvw");
    term.advance(b"\x1b[2;2H");
    term.advance(&command("a=T,i=1,f=24,s=20,v=20,C=1", &rgb(20, 20, [0; 3])));
    assert_eq!(spots(&term), [(1, 1, 1)], "on the v");
    term.resize(10, 10);
    assert_eq!(spots(&term), [(1, 2, 1)], "still on the v");
    assert_eq!(placements(&term)[0].dest, [0., 0., 2., 1.]);
    term.resize(10, 30);
    assert_eq!(spots(&term), [(1, 0, 21)]);
}

/// Placements far above the rows asked for still reach into them.
#[test]
fn a_tall_placement_reaches_rows_below_its_own() {
    let mut term = term();
    term.advance(&command("a=T,i=1,f=24,s=10,v=100,C=1", &rgb(10, 100, [0; 3])));
    assert_eq!(term.placements(4, 5).len(), 1);
    assert!(term.placements(5, 6).is_empty());
}

#[test]
fn files_are_read_and_temporary_ones_deleted() {
    let dir =
        std::env::temp_dir().join(format!("rtty-vt-tty-graphics-protocol-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("image.rgb");
    std::fs::write(&file, rgb(2, 2, [4, 5, 6])).unwrap();
    let path = file.to_str().unwrap().as_bytes();
    let mut term = term();
    assert_eq!(replies(&mut term, &command("i=1,t=f,f=24,s=2,v=2", path)), ["\x1b_Gi=1;OK\x1b\\"]);
    assert!(file.exists());
    // `S` and `O`: the second pixel only.
    let reply = replies(&mut term, &command("i=2,t=f,f=24,s=1,v=1,S=3,O=3", path));
    assert_eq!(reply, ["\x1b_Gi=2;OK\x1b\\"]);
    assert_eq!(replies(&mut term, &command("i=3,t=t,f=24,s=2,v=2", path)), ["\x1b_Gi=3;OK\x1b\\"]);
    assert!(!file.exists(), "a temporary file goes once read");
    // Every failure answers alike, and at once: gone, a directory, a FIFO with no writer, a
    // device, short of `S` (however much it asks for), a temporary file whose path does not say so.
    let elsewhere = std::env::temp_dir().join(format!("rtty-vt-image-{}", std::process::id()));
    std::fs::write(&elsewhere, rgb(1, 1, [0; 3])).unwrap();
    let fifo = dir.join("fifo");
    assert!(std::process::Command::new("/usr/bin/mkfifo").arg(&fifo).status().unwrap().success());
    for (keys, path) in [
        ("t=f", file.to_str().unwrap()),
        ("t=f", dir.to_str().unwrap()),
        ("t=f", fifo.to_str().unwrap()),
        ("t=f", "/dev/zero"),
        ("t=f,S=9", elsewhere.to_str().unwrap()),
        ("t=f,S=4294967295", elsewhere.to_str().unwrap()),
        ("t=t", elsewhere.to_str().unwrap()),
    ] {
        let reply =
            replies(&mut term, &command(&format!("i=4,{keys},f=24,s=1,v=1"), path.as_bytes()));
        assert_eq!(reply, ["\x1b_Gi=4;EBADF:Failed to read image file\x1b\\"], "{keys} {path}");
    }
    assert!(elsewhere.exists());
    std::fs::remove_file(elsewhere).unwrap();
    std::fs::remove_file(fifo).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

/// 64 MiB images: the quota holds four. A fifth evicts the oldest placed nowhere, and once every
/// image is placed, the oldest goes with its placement.
#[test]
fn the_quota_evicts_the_oldest_unplaced_images_first() {
    let side = 4096;
    let deflated = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0; side * side * 4], 1);
    let mut term = term();
    let send = |term: &mut Terminal, id: u32, action: &str| {
        term.advance(&command(&format!("a={action},i={id},s={side},v={side},o=z,C=1"), &deflated));
    };
    send(&mut term, 1, "T");
    for id in 2..=4 {
        send(&mut term, id, "t");
    }
    assert_eq!(GRAPHICS_QUOTA, 4 * side * side * 4);
    send(&mut term, 5, "T");
    assert_eq!(ids(&term), [1, 3, 4, 5]);
    for id in [3, 4] {
        term.advance(&command(&format!("a=p,i={id},C=1"), b""));
    }
    send(&mut term, 6, "t");
    assert_eq!(ids(&term), [3, 4, 5, 6]);
    assert_eq!(spots(&term).iter().map(|spot| spot.0).collect::<Vec<_>>(), [3, 4, 5]);
}

#[test]
fn turning_graphics_off_forgets_every_image() {
    let mut term = term();
    term.advance(&command("a=T,i=1,f=24,s=1,v=1", &[0; 3]));
    let image: Arc<Image> = term.images()[0].clone();
    term.set_graphics(false);
    assert!(term.images().is_empty() && placements(&term).is_empty());
    term.set_graphics(true);
    assert!(placements(&term).is_empty());
    assert_eq!(image.rgba.len(), 4, "a host's copy stays whole");
}

/// A piece as (image id, row, column, dest, source).
type Piece = (u32, u64, u16, [f32; 4], [f32; 4]);

fn pieces(term: &Terminal) -> Vec<Piece> {
    placements(term).iter().map(|p| (p.image.id, p.row, p.col, p.dest, p.source)).collect()
}

/// Image `id`, 20×40 pixels, quietly sent, and its virtual placement over `c`×`r` cells.
fn virtual_image(term: &mut Terminal, id: u32, keys: &str) {
    term.advance(&command(&format!("a=t,i={id},f=24,s=20,v=40,q=2"), &rgb(20, 40, [1, 2, 3])));
    term.advance(&command(&format!("a=p,U=1,i={id},q=2,{keys}"), b""));
}

/// The specification's 2×2 placeholder for image 42: each row of cells shows its half.
#[test]
fn placeholders_show_their_cells_of_a_virtual_placement() {
    let mut term = term();
    virtual_image(&mut term, 42, "c=2,r=2");
    assert!(placements(&term).is_empty(), "a virtual placement shows nothing itself");
    assert_eq!(term.cursor_position(), (0, 0), "nor moves the cursor");
    term.advance(
        "\x1b[38;5;42m\u{10EEEE}\u{305}\u{305}\u{10EEEE}\u{305}\u{30D}\x1b[39m\r\n".as_bytes(),
    );
    term.advance(
        "\x1b[38;5;42m\u{10EEEE}\u{30D}\u{305}\u{10EEEE}\u{30D}\u{30D}\x1b[39m\r\n".as_bytes(),
    );
    assert_eq!(term.screen_row(0).cells()[1].ch(), PLACEHOLDER, "one cell each");
    assert_eq!(
        pieces(&term),
        [
            (42, 0, 0, [0., 0., 2., 1.], [0., 0., 20., 20.]),
            (42, 1, 0, [0., 0., 2., 1.], [0., 20., 20., 20.]),
        ]
    );
    // They are text: they scroll, and a cell overwritten shows nothing more.
    term.advance(b"\x1b[1;2Hx\x1b[10;1H\n");
    assert_eq!(
        pieces(&term),
        [
            (42, 0, 0, [0., 0., 1., 1.], [0., 0., 10., 20.]),
            (42, 1, 0, [0., 0., 2., 1.], [0., 20., 20., 20.]),
        ]
    );
    assert_eq!(term.screen_top(), 1);
}

/// Diacritics left out are carried on from the placeholder to the left: here only the first
/// cell of each row names its row.
#[test]
fn missing_diacritics_follow_the_cell_to_the_left() {
    let mut term = term();
    virtual_image(&mut term, 42, "c=3,r=2");
    term.advance("\x1b[38;5;42m\u{10EEEE}\u{305}\u{10EEEE}\u{10EEEE}\r\n".as_bytes());
    term.advance("\u{10EEEE}\u{30D}\u{10EEEE}\u{10EEEE}\r\n".as_bytes());
    // 20×40 fitted into 30×40 pixels: centred, half a cell in.
    assert_eq!(
        pieces(&term),
        [
            (42, 0, 0, [0.5, 0., 2., 1.], [0., 0., 20., 20.]),
            (42, 1, 0, [0.5, 0., 2., 1.], [0., 20., 20., 20.]),
        ]
    );
    // Another colour starts again from row 0, column 0 (of image 43, which there is not).
    let mut term = self::term();
    virtual_image(&mut term, 42, "c=3,r=2");
    term.advance("\x1b[38;5;42m\u{10EEEE}\u{30D}\u{10EEEE}\x1b[38;5;43m\u{10EEEE}".as_bytes());
    assert_eq!(pieces(&term), [(42, 0, 0, [0.5, 0., 1.5, 1.], [0., 20., 15., 20.])]);
}

/// Ids past 8 bits: a third diacritic's high byte, an RGB foreground's 24 bits; the underline
/// colour picks the placement.
#[test]
fn ids_come_from_the_colours_and_a_third_diacritic() {
    let mut term = term();
    virtual_image(&mut term, 42 + (2 << 24), "c=2,r=2");
    term.advance("\x1b[38;5;42m\u{10EEEE}\u{305}\u{305}\u{30E}\u{10EEEE}\r\n".as_bytes());
    term.advance("\u{10EEEE}\u{305}\u{305}\r\n".as_bytes());
    assert_eq!(pieces(&term), [(42 + (2 << 24), 0, 0, [0., 0., 2., 1.], [0., 0., 20., 20.])]);
    // The 257th diacritic is no high byte: it names no image, image 42 least of all.
    let mut term = self::term();
    virtual_image(&mut term, 42, "c=2,r=2");
    term.advance("\x1b[38;5;42m\u{10EEEE}\u{305}\u{305}\u{A8E6}".as_bytes());
    assert!(pieces(&term).is_empty());

    let mut term = self::term();
    let id = 0x010203;
    virtual_image(&mut term, id, "c=2,r=2,p=7");
    term.advance(&command(&format!("a=p,U=1,i={id},c=4,r=4,p=8,q=2"), b""));
    term.advance("\x1b[38;2;1;2;3;58:5:7m\u{10EEEE}\u{305}\u{305}\u{10EEEE}\r\n".as_bytes());
    term.advance("\x1b[58:5:8m\u{10EEEE}\u{305}\u{305}\u{10EEEE}".as_bytes());
    assert_eq!(
        pieces(&term),
        [
            (id, 0, 0, [0., 0., 2., 1.], [0., 0., 20., 20.]),
            (id, 1, 0, [0., 0., 2., 1.], [0., 0., 10., 10.]),
        ]
    );
}

/// Placeholders with no image or virtual placement behind them show nothing; deleting by id
/// takes a virtual placement, deleting by place does not.
#[test]
fn placeholders_need_their_image_and_virtual_placement() {
    let mut term = term();
    term.advance("\x1b[38;5;9m\u{10EEEE}\u{305}\u{305}".as_bytes());
    assert!(placements(&term).is_empty());
    virtual_image(&mut term, 9, "c=1,r=1");
    term.advance(&command("a=d,d=A", b""));
    assert_eq!(pieces(&term).len(), 1);
    term.advance(&command("a=d,d=i,i=9", b""));
    assert!(placements(&term).is_empty());
    assert_eq!(ids(&term), [9]);
}

/// What lets images through tmux and tutti: the placeholders are text, so a snapshot carries
/// them to a follower, which shows the same image where it holds it too.
#[test]
fn placeholders_travel_as_text() {
    let mut term = term();
    virtual_image(&mut term, 42, "c=2,r=2");
    term.advance("\x1b[3;5H\x1b[38;5;42;58:5:0m\u{10EEEE}\u{305}\u{305}\u{10EEEE}".as_bytes());
    let mut follower = self::term();
    virtual_image(&mut follower, 42, "c=2,r=2");
    follower.advance(&term.snapshot());
    assert_eq!(pieces(&follower), pieces(&term));
    assert_eq!(pieces(&term).len(), 1);
}

#[test]
fn a_feature_query_names_unicode_placeholders() {
    let mut term = term();
    let reply = replies(&mut term, b"\x1b_Ga=q,i=31,e=T,U=1,C=1\x1b\\");
    assert_eq!(reply, ["\x1b_Gi=31,e=T,U=1,C=1;OK\x1b\\"]);
}

/// Virtual placements count among the 4096 placements: one for each id, the oldest going first.
#[test]
fn virtual_placements_are_bounded_too() {
    let mut term = term();
    virtual_image(&mut term, 42, "c=1,r=1,p=1");
    let more: Vec<u8> =
        (2..=4096).flat_map(|p| command(&format!("a=p,U=1,i=42,c=1,r=1,p={p},q=2"), b"")).collect();
    term.advance(&more);
    term.advance("\x1b[38;5;42;58:5:1m\u{10EEEE}\u{305}\u{305}".as_bytes());
    assert_eq!(pieces(&term).len(), 1, "placement 1 is still there");
    term.advance(&command("a=p,U=1,i=42,c=1,r=1,p=4097,q=2", b""));
    assert!(pieces(&term).is_empty(), "placement 1, the oldest, went");
}
