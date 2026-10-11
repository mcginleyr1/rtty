#!/usr/bin/env python3
"""Synthetic terminal streams for the rtty-vt fixture corpus.

    python3 -I synth.py NAME

writes stream NAME to stdout. capture.py runs it in an 80x24 pty like any other
program. Output post-processing (OPOST) is switched off first so every byte,
including bare LF, reaches the terminal exactly as written here. Streams are
deterministic; each one labels what it does on screen.
"""

import base64
import sys
import termios

ESC = "\x1b"
CSI = ESC + "["
OSC = ESC + "]"
ST = ESC + "\\"
BEL = "\x07"
NL = "\r\n"
ROWS, COLS = 24, 80


def cup(row, col=1):
    return f"{CSI}{row};{col}H"


def sgr(*params):
    return f"{CSI}{';'.join(str(p) for p in params)}m"


def label(text):
    return sgr(7) + text + sgr(0)


def sgr_stream():
    out = [CSI + "H" + CSI + "2J", label("SGR attributes") + NL]
    attrs = [(1, "bold"), (2, "dim"), (3, "italic"), (4, "underline"), (5, "blink"), (6, "rapid"),
             (7, "reverse"), (8, "conceal"), (9, "strike"), (21, "dbl-ul"), (51, "framed"),
             (52, "encircled"), (53, "overline"), (73, "super"), (74, "sub")]
    out += [sgr(code) + name + sgr(0) + " " for code, name in attrs] + [NL]
    out.append(sgr(1, 2, 3, 4, 7, 9, 53) + "all-on" + sgr(22) + "-22" + sgr(23) + "-23" + sgr(24) + "-24"
               + sgr(27) + "-27" + sgr(29) + "-29" + sgr(55) + "-55" + sgr(5) + "b" + sgr(25) + "-25"
               + sgr(8) + "c" + sgr(28) + "-28" + sgr(0) + NL)
    out.append(label("underline styles") + " ")
    out += [f"{CSI}4:{style}m4:{style}{sgr(0)} " for style in range(6)]
    out.append(f"{CSI}21mSGR21{sgr(0)} {CSI}4:3;58:2::255:0:0mcurl+red{CSI}59mnocolor{sgr(0)}" + NL)
    out.append(label("underline colour") + " ")
    out += [f"{CSI}4;58;5;196m58;5{sgr(0)} ", f"{CSI}4;58;2;0;200;255m58;2{sgr(0)} ",
            f"{CSI}4:3;58:5:46m58:5{sgr(0)} ", f"{CSI}4:4;58:2::255:128:0m58:2::{sgr(0)} ",
            f"{CSI}4:5;58:2:200:0:200m58:2:{sgr(0)} ", f"{CSI}4;58;5;21m{CSI}59m59{sgr(0)}" + NL]
    out.append(label("16 colours") + " ")
    out += [sgr(c) + f"{c}" + sgr(39) + " " for c in list(range(30, 38)) + list(range(90, 98))] + [NL]
    out += [sgr(c) + f"{c}" + sgr(49) + " " for c in list(range(40, 48)) + list(range(100, 108))] + [NL]
    out.append(label("256 colours, ; form fg and bg") + NL)
    for row in range(8):
        out += [f"{CSI}48;5;{n}m{CSI}38;5;{255 - n}m{n:3}" for n in range(row * 32, row * 32 + 20)]
        out.append(sgr(0) + NL)
    out.append(label(": forms") + " ")
    out += [f"{CSI}38:5:{n}m#" for n in range(16, 232, 9)] + [sgr(0) + " "]
    out += [f"{CSI}48:2::{v}:0:{255 - v}m " for v in range(0, 256, 32)]
    out += [f"{CSI}48:2:{v}:{v}:0m " for v in range(0, 256, 32)] + [sgr(0) + NL]
    out.append(label("truecolor ; form") + NL)
    for row in range(3):
        out += [f"{CSI}48;2;{x * 3};{row * 100};{255 - x * 3}m{CSI}38;2;255;255;{x * 3}m{'▀' if x % 2 else '▄'}"
                for x in range(78)]
        out.append(sgr(0) + NL)
    out.append(label("combined, defaults, malformed") + NL)
    out.append(f"{CSI}1;4;38;5;196;48;2;0;0;128mcombined{CSI}39;49mdefaults{CSI}m reset ")
    out.append(f"{CSI};1memptyfirst{CSI}0m {CSI}38;5mtrunc5{CSI}0m {CSI}38;2;1;2mtrunc2{CSI}0m ")
    out.append(f"{CSI}99munknown{CSI}0m {CSI}38:2:::mempty-colon{CSI}0m {CSI}1;;3mgap{CSI}0m" + NL)
    return "".join(out)


def decscusr_stream():
    shapes = ["default (0)", "blinking block", "steady block", "blinking underline",
              "steady underline", "blinking bar", "steady bar"]
    out = [CSI + "H" + CSI + "2J", label("DECSCUSR") + NL]
    for ps, name in enumerate(shapes):
        out.append(f"{CSI}{ps} q{ps}: {name}" + NL)
    out.append(f"{CSI} qdefault form without a parameter" + NL)
    out.append(f"{CSI}?12lblink off {CSI}?12hblink on {CSI}?25lhidden {CSI}?25hshown" + NL)
    out.append(f"{CSI}6 qending with a steady bar, then {CSI}0 qback to default" + NL)
    return "".join(out)


def alt_screen_stream():
    out = [CSI + "H" + CSI + "2J"]
    out += [f"main screen line {i}" + NL for i in range(1, 6)]
    out.append(cup(3, 30) + "main cursor at 3;30")
    for mode in ("47", "1047"):
        out.append(f"{CSI}?{mode}h" + cup(1) + label(f"alt screen via {mode}") + NL + f"content on alt {mode}")
        out.append(cup(10, 10) + f"alt {mode} row 10")
        out.append(f"{CSI}?{mode}l" + cup(6) + f"back on main after {mode}" + NL)
    out.append(cup(8, 5) + "cursor saved here by 1049")
    out.append(f"{CSI}?1049h" + label("alt screen via 1049 (cleared on entry)"))
    out += [cup(r, 1) + f"alt 1049 row {r}" for r in range(2, ROWS + 1)]
    out.append(f"{CSI}?1049h" + cup(5, 40) + "1049h again while on alt")
    out.append(f"{CSI}?1049l" + " <- restored cursor after 1049l")
    out.append(f"{CSI}?1048h" + cup(20, 1) + "1048 saved, moved" + f"{CSI}?1048l" + " <- restored by 1048")
    out.append(cup(22) + "end on main screen" + NL)
    return "".join(out)


def decstbm_stream():
    """Every operation leaves a visible trace on the final screen."""
    out = [CSI + "H" + CSI + "2J"]
    out += [cup(r) + f"row {r:02} " + "." * 20 for r in range(1, ROWS + 1)]
    out.append(f"{CSI}3;6r" + cup(6, 30) + "\n\n" + "<- bare LF x2 at bottom of 3-6")
    out.append(f"{CSI}7;10r" + cup(7, 30) + ESC + "M" + "<- RI at top of 7-10")
    out.append(cup(2) + f"{CSI}L" + cup(2, 30) + "<- IL above 7-10 (xterm: no effect)")
    out.append(f"{CSI}11;13r" + f"{CSI}S" + cup(13, 30) + "<- SU 1 in 11-13")
    out.append(f"{CSI}?6h" + cup(1, 48) + "<- DECOM 1;48" + cup(30, 64) + "<- clamp" + f"{CSI}?6l")
    out.append(f"{CSI}14;16r" + f"{CSI}T" + cup(14, 30) + "<- SD 1 in 14-16")
    out.append(f"{CSI}17;19r" + cup(18) + f"{CSI}L" + cup(18, 30) + "<- IL 1 at 18 in 17-19")
    out.append(f"{CSI}20;22r" + cup(20) + f"{CSI}M" + cup(22, 30) + "<- DL 1 at 20 in 20-22")
    out.append(f"{CSI}r" + "DECSTBM reset homed the cursor")
    out.append(f"{CSI}23;24r" + cup(24) + NL + f"{CSI}24;30H" + "<- CRLF scrolled 23-24" + f"{CSI}r")
    return "".join(out)


def tabs_stream():
    out = [CSI + "H" + CSI + "2J", label("tabs") + NL]
    out.append("a\tb\tc\td\te\tf\tg\th\ti\tj" + NL)
    out.append(f"{CSI}3g" + "cleared all stops:\tafter" + NL)
    out.append(cup(4, 5) + ESC + "H" + cup(4, 12) + ESC + "H" + cup(4, 30) + ESC + "H")
    out.append(cup(5) + "x\ty\tz\tend" + NL)
    out.append(cup(6, 12) + f"{CSI}g" + cup(6) + "TBC current:\tq\tr" + NL)
    out.append(f"{CSI}3g" + "".join(cup(7, c) + ESC + "H" for c in range(9, COLS, 8)))
    out.append(cup(7) + "HTS every 8 again:\t|\t|" + NL)
    out.append(f"CHT 3{CSI}3I|" + f"{CSI}2Z" + "CBT 2" + NL)
    out.append("tab past last stop" + "\t" * 12 + "#" + NL)
    out.append("XXXXXXXXXXXXXXXXXXXXXXXX\r\tover\twrite" + NL)
    out.append("\tindented line\t\twith\tgaps" + NL)
    out.append(f"{CSI}3g{CSI}?5W" + "DECST8C (VT510) after TBC 3:\t|\t|" + NL)
    return "".join(out)


def charset_dec_stream():
    out = [CSI + "H" + CSI + "2J", label("DEC special graphics") + NL]
    out.append(ESC + "(0" + "lqqqqqqqqwqqqqqqqqk" + NL + "x        x        x" + NL + "tqqqqqqqqnqqqqqqqqu" + NL
               + "x        x        x" + NL + "mqqqqqqqqvqqqqqqqqj" + ESC + "(B" + NL)
    out.append("G0 all: " + ESC + "(0" + "".join(chr(c) for c in range(0x5F, 0x7F)) + ESC + "(B" + NL)
    out.append(ESC + ")0" + "G1 via SO: \x0elqqk\x0f back via SI, \x0exx\x0f" + NL)
    out.append(ESC + "(A" + "UK set: # becomes pound" + ESC + "(B" + " ASCII: #" + NL)
    out.append(ESC + "(0" + "a" * 10 + ESC + "(B" + " checkerboard" + NL)
    return "".join(out)


ACUTE = "\N{COMBINING ACUTE ACCENT}"
ZWJ = "\N{ZERO WIDTH JOINER}"
VS15 = "\N{VARIATION SELECTOR-15}"
VS16 = "\N{VARIATION SELECTOR-16}"


def unicode_stream():
    marks = ["e" + ACUTE, "a\N{COMBINING GRAVE ACCENT}", "n\N{COMBINING TILDE}",
             "o\N{COMBINING DIAERESIS}\N{COMBINING MACRON}", "x" + "\N{COMBINING LONG STROKE OVERLAY}" * 2,
             "q\N{COMBINING DOT BELOW}\N{COMBINING DOT ABOVE}"]
    family = ZWJ.join("👨👩👧👦")
    out = [CSI + "H" + CSI + "2J", label("unicode") + NL]
    out.append("wide: 端末エミュレータ 한국어 中文 ｆｕｌｌｗｉｄｔｈ|" + NL)
    out.append("combining: " + " ".join(marks) + "|" + NL)
    out.append("scripts: नमस्ते สวัสดี مرحبا שלום|" + NL)
    out.append(f"emoji: 😀 👍\N{EMOJI MODIFIER FITZPATRICK TYPE-4} {family} 🏳{VS16}{ZWJ}🌈 🇯🇵 🇺🇸|" + NL)
    out.append(f"vs16: ❤{VS16} ☀{VS16} ✔{VS16} ⚠{VS16}  vs15: ❤{VS15} ☀{VS15}  bare: ❤ ☀|" + NL)
    out.append("ambiguous: § ° ± × ÷ ① ★ ←|" + NL)
    out.append("zero-width: a\N{ZERO WIDTH SPACE}b a\N{ZERO WIDTH NON-JOINER}b soft\N{SOFT HYPHEN}hyphen|" + NL)
    out.append(cup(10, COLS) + "端" + " <- a wide char at column 80 wraps")
    out.append(cup(13) + "端末" + f"{CSI}D" + "X <- overwrote the right half" + NL)
    out.append("端末" + f"{CSI}4D" + "Y <- overwrote the left half of the first wide char" + NL)
    out.append(cup(16, COLS - 1) + "😀" + "<- emoji at column 79 fits")
    out.append(cup(18) + "e" + ACUTE + "\N{COMBINING CIRCUMFLEX ACCENT} stacked marks, then a bare mark at line start:"
               + NL + ACUTE + "x" + NL)
    return "".join(out)


def wrap_stream():
    out = [CSI + "H" + CSI + "2J", label("autowrap") + NL]
    out.append("".join(f"{i % 10}" for i in range(200)) + NL)
    out.append("A" * COLS + NL + "^ exactly 80 then CRLF: no blank line between" + NL)
    out.append("B" * COLS + "C <- 81st char wrapped" + NL)
    out.append("D" * COLS + "\rE <- CR after a full line returns to column 1" + NL)
    out.append(f"{CSI}?7l" + "F" * 100 + f"{CSI}?7h" + NL + "^ DECAWM off: last column overwritten" + NL)
    out.append("G" * 150 + f"{CSI}A{CSI}10G" + "<overwrite in the wrapped part" + NL)
    out.append(cup(ROWS) + "H" * (COLS * 2 + 10) + NL + "wrapped at the bottom, scrolling")
    return "".join(out)


def osc8_stream():
    out = [CSI + "H" + CSI + "2J", label("OSC 8 hyperlinks") + NL]
    out.append(f"plain: {OSC}8;;https://example.com/plain{ST}a link{OSC}8;;{ST} after" + NL)
    out.append(f"bel: {OSC}8;;https://example.com/bel{BEL}terminated by BEL{OSC}8;;{BEL}" + NL)
    out.append(f"id: {OSC}8;id=one;https://example.com/split{ST}first half{OSC}8;;{ST} gap "
               f"{OSC}8;id=one;https://example.com/split{ST}second half{OSC}8;;{ST}" + NL)
    out.append(f"wrapping: {OSC}8;;https://example.com/long{ST}" + "long link text " * 8 + f"{OSC}8;;{ST}" + NL)
    out.append(f"styled: {OSC}8;;https://example.com/s{ST}{sgr(1, 4, 34)}端末 link{sgr(0)}{OSC}8;;{ST}" + NL)
    out.append(f"file: {OSC}8;;file://fixture-host/home/fixture/notes.txt{ST}notes.txt{OSC}8;;{ST}" + NL)
    out.append(f"switch: {OSC}8;;https://example.com/a{ST}a{OSC}8;;https://example.com/b{ST}b{OSC}8;;{ST}" + NL)
    out.append(f"unterminated: {OSC}8;;https://example.com/open{ST}left open to the end" + NL)
    return "".join(out)


def osc_shell_stream():
    out = [CSI + "H" + CSI + "2J"]
    out.append(f"{OSC}0;fixture shell{BEL}{OSC}2;window title{ST}{OSC}1;icon{ST}")
    out.append(f"{OSC}7;file://fixture-host/home/fixture/work{ST}")
    var = base64.b64encode("rtty-vt fixture ✓".encode()).decode()
    out.append(f"{OSC}1337;SetUserVar=fixture_var={var}{BEL}")
    commands = [("ls", ["Cargo.toml", "README.md", "src"], 0),
                ("cat missing.txt", ["cat: missing.txt: No such file or directory"], 1),
                ("printf 'colour\\n'", [sgr(32) + "colour" + sgr(0)], 0)]
    for cmd, lines, status in commands:
        out.append(f"{OSC}133;A{ST}{sgr(1, 34)}~/work{sgr(0)} $ {OSC}133;B{ST}{cmd}" + NL)
        out.append(f"{OSC}133;C{ST}" + "".join(line + NL for line in lines) + f"{OSC}133;D;{status}{ST}")
    out.append(f"{OSC}7;file://fixture-host/home/fixture/work/src{ST}{OSC}133;A;aid=7{BEL}src $ {OSC}133;B{BEL}")
    out.append(f"{OSC}1337;CurrentDir=/home/fixture/work/src{BEL}")
    out.append(f"{OSC}52;c;{base64.b64encode(b'copied by OSC 52').decode()}{ST}")
    out.append(f"{OSC}9;4;1;50{ST}{OSC}9;4;0;0{ST}")
    out.append(f"{OSC}133;D{ST}" + NL + "end" + NL)
    return "".join(out)


def sync_stream():
    out = [CSI + "H" + CSI + "2J"]
    for frame in range(60):
        out.append(f"{CSI}?2026h")
        for row in range(1, ROWS + 1):
            colour = 16 + (row * 7 + frame) % 216
            out.append(cup(row) + f"{CSI}38;5;{colour}m" + f"frame {frame:02} row {row:02} " + "#" * ((row + frame) % 50) + f"{CSI}0m{CSI}K")
        out.append(f"{CSI}?2026l")
    out.append(cup(ROWS) + "done" + NL)
    return "".join(out)


def edit_ops_stream():
    """Edits on top of a DECALN fill, so erased and shifted cells stay visible."""
    out = [CSI + "H" + CSI + "2J" + CSI + "3J" + ESC + "#8"]
    out.append(cup(1, 40) + f"{CSI}1J" + cup(1) + "ED1 cleared to 1;40")
    ops = [("ICH 3 at 8", f"{CSI}8G{CSI}3@"), ("DCH 4 at 5", f"{CSI}5G{CSI}4P"), ("ECH 4 at 5", f"{CSI}5G{CSI}4X"),
           ("EL0 at 9", f"{CSI}9G{CSI}K"), ("EL1 at 9", f"{CSI}9G{CSI}1K"), ("EL2", f"{CSI}2K"),
           ("REP 20", f"x{CSI}20b"), ("IRM insert", f"{CSI}3G{CSI}4hINS{CSI}4l"),
           ("EL0 blue bg", f"{CSI}17G{sgr(44)}{CSI}K{sgr(0)}")]
    for row, (name, seq) in enumerate(ops, start=3):
        out.append(cup(row) + "0123456789ABCDEF" + cup(row) + seq + cup(row, 45) + "<- " + name)
    out += [cup(row) + f"line {row} " + "=" * 20 for row in range(13, 19)]
    out.append(cup(14) + f"{CSI}2L" + cup(14, 45) + "<- IL 2 at 14" + cup(17) + f"{CSI}M" + cup(17, 45) + "<- DL 1 at 17")
    out.append(cup(22, 40) + f"{CSI}J" + cup(22) + "ED0 from 22;40 ->")
    return "".join(out)


def modes_stream():
    out = [CSI + "H" + CSI + "2J", label("input modes") + NL]
    steps = [
        ("mouse 1000+1006", f"{CSI}?1000h{CSI}?1006h"), ("mouse 1002", f"{CSI}?1002h"),
        ("mouse 1003", f"{CSI}?1003h"), ("mouse 1015", f"{CSI}?1015h"), ("mouse 1005", f"{CSI}?1005h"),
        ("mouse off", f"{CSI}?1005l{CSI}?1015l{CSI}?1003l{CSI}?1002l{CSI}?1006l{CSI}?1000l"),
        ("focus 1004", f"{CSI}?1004h"), ("bracketed paste", f"{CSI}?2004h"), ("DECCKM", f"{CSI}?1h"),
        ("keypad app", ESC + "="), ("kitty push 1", f"{CSI}>1u"), ("kitty push 31", f"{CSI}>31u"),
        ("kitty set 5;1", f"{CSI}=5;1u"), ("kitty pop 2", f"{CSI}<2u"), ("modifyOtherKeys 2", f"{CSI}>4;2m"),
        ("title push", f"{CSI}22;0t{OSC}2;pushed title{ST}"), ("title pop", f"{CSI}23;0t"),
        ("alternate scroll", f"{CSI}?1007h"), ("everything off",
         f"{CSI}?1004l{CSI}?2004l{CSI}?1l" + ESC + f">{CSI}<u{CSI}>4m{CSI}?1007l"),
    ]
    out += [name + seq + NL for name, seq in steps]
    return "".join(out)


STREAMS = {
    "sgr": sgr_stream, "decscusr": decscusr_stream, "alt-screen": alt_screen_stream,
    "decstbm": decstbm_stream, "tabs": tabs_stream, "charset-dec": charset_dec_stream,
    "unicode": unicode_stream, "wrap": wrap_stream, "osc8": osc8_stream, "osc-shell": osc_shell_stream,
    "sync-2026": sync_stream, "edit-ops": edit_ops_stream, "modes": modes_stream,
}


def main():
    stream = STREAMS[sys.argv[1]]().encode()
    if sys.stdout.isatty():
        attrs = termios.tcgetattr(1)
        attrs[1] &= ~termios.OPOST
        termios.tcsetattr(1, termios.TCSADRAIN, attrs)
    sys.stdout.buffer.write(stream)
    sys.stdout.buffer.flush()


if __name__ == "__main__":
    main()
