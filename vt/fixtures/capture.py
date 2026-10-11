#!/usr/bin/env python3
"""Record what real programs write to a terminal: the rtty-vt fixture corpus.

Each scenario runs one program in a pty at a fixed size with a fixed
environment, plays a scripted input sequence and records only the program's
output bytes, verbatim, to <name>.out. The queries programs block on or adapt
to (DA1, DA2, DSR, CPR, OSC 10/11, DECRQM, DECRQSS SGR) are answered; every
query seen is listed in <name>.json with the answer sent, next to the size,
program version, environment, input script, settle checkpoints and the
features the stream exercises.

    python3 -I capture.py --scratch DIR                  # record every scenario
    python3 -I capture.py --scratch DIR nvim-80x24 sgr   # record some
    python3 -I capture.py --list

Standard library only. See README.md.
"""

import argparse
import errno
import hashlib
import json
import os
import pty
import re
import select
import shutil
import signal
import subprocess
import sys
import sysconfig
import termios
import time
import unicodedata
from pathlib import Path

HERE = Path(__file__).resolve().parent
ESC = b"\x1b"
CR = b"\r"
QUIET = 0.3

USER_HOME = Path.home()
BINS = {
    "nvim": USER_HOME / ".local/share/mise/installs/neovim/0.12.4/bin/nvim",
    "tmux": USER_HOME / ".local/share/mise/installs/tmux/3.6a/tmux",
    "bat": USER_HOME / ".cargo/bin/bat",
    "emacs": Path("/opt/homebrew/bin/emacs"),
    "fzf": Path("/opt/homebrew/bin/fzf"),
    "git": Path("/opt/homebrew/bin/git"),
    "vim": Path("/usr/bin/vim"),
    "less": Path("/usr/bin/less"),
    "man": Path("/usr/bin/man"),
    "top": Path("/usr/bin/top"),
    "seq": Path("/usr/bin/seq"),
    "sh": Path("/bin/sh"),
}
VERSION_FLAGS = {
    "nvim": "--version", "emacs": "--version", "vim": "--version", "less": "--version",
    "bat": "--version", "git": "--version", "fzf": "--version", "tmux": "-V",
}

FG = "rgb:d0d0/d0d0/d0d0"
BG = "rgb:1c1c/1c1c/1c1c"
CURSOR = "cursor"
MODE_STATE = "mode-state"
SGR_STATE = "sgr-state"
# Modes an xterm-class terminal supports; DECRQM reports their tracked state
# and "not recognized" for anything else.
SUPPORTED_MODES = {
    "4", "20", "?1", "?6", "?7", "?12", "?25", "?47", "?1000", "?1002", "?1003", "?1004",
    "?1005", "?1006", "?1007", "?1015", "?1047", "?1048", "?1049", "?2004", "?2026",
}
MODES_SET_AT_START = {"?7", "?25"}
ANSWERS = {
    "DA1": b"\x1b[?62;22c",
    "DA2": b"\x1b[>1;10;0c",
    "DSR 5": b"\x1b[0n",
    "CPR": CURSOR,
    "DECXCPR": CURSOR,
    "OSC 10": f"\x1b]10;{FG}".encode(),
    "OSC 11": f"\x1b]11;{BG}".encode(),
    "DECRQM": MODE_STATE,
    "DECRQSS m": SGR_STATE,
}
POLICY_NOTES = {
    CURSOR: "ESC[<row>;<col>R from a roughly tracked cursor (approximate)",
    SGR_STATE: "DCS 1$r 0;<SGR parameters since the last reset> m ST (approximate)",
    MODE_STATE: "ESC[<mode>;<1 set, 2 reset>$y from the stream's tracked mode state for "
                + " ".join(sorted(SUPPORTED_MODES, key=lambda m: int(m.lstrip("?"))))
                + "; ESC[<mode>;0$y (not recognized) for any other mode",
}


def ctrl(ch):
    return bytes([ord(ch) & 0x1F])


def show(data):
    return data.decode("utf-8", "backslashreplace")


# Escape-sequence scanning -------------------------------------------------

CSI_RE = re.compile(rb"\x1b\[([\x30-\x3f]*)([\x20-\x2f]*)([\x40-\x7e])\Z")


def split_csi(seq):
    """CSI bytes -> (private marker, params, intermediates, final) as str."""
    m = CSI_RE.match(seq)
    if not m:
        return None
    raw, inter, final = (g.decode() for g in m.groups())
    private = raw[:1] if raw[:1] in ("<", "=", ">", "?") else ""
    return private, raw[len(private):], inter, final


def string_body(seq):
    """Payload of an OSC/DCS/APC/PM/SOS string without introducer and terminator."""
    body = seq[2:]
    if body.endswith(b"\x07"):
        return body[:-1]
    return body[:-2] if body.endswith(b"\x1b\\") else body


class Scanner:
    """Splits a byte stream into text runs, C0 controls and whole escape
    sequences, carrying a partial sequence across reads. Yields
    (kind, value, end) where end indexes just past the token in the chunk."""

    CONTROL = re.compile(rb"[\x00-\x1f\x7f]")
    STRING_LIMIT = 8192

    def __init__(self):
        self.state = "ground"
        self.seq = bytearray()

    def start(self):
        self.state, self.seq = "esc", bytearray(ESC)

    def feed(self, data):
        i, n = 0, len(data)
        while i < n:
            if self.state == "ground":
                m = self.CONTROL.search(data, i)
                end = m.start() if m else n
                if end > i:
                    yield "text", data[i:end], end
                if not m:
                    return
                i = end + 1
                if data[end] == 0x1B:
                    self.start()
                else:
                    yield "ctl", data[end], i
                continue
            b = data[i]
            i += 1
            st = self.state
            if b in (0x18, 0x1A):  # CAN and SUB abort any sequence
                self.state = "ground"
            elif st in ("esc", "esc-int"):
                self.seq.append(b)
                if st == "esc" and b == 0x5B:
                    self.state = "csi"
                elif st == "esc" and b in b"]PX^_":
                    self.state = "string"
                elif 0x20 <= b <= 0x2F:
                    self.state = "esc-int"
                elif 0x30 <= b <= 0x7E:
                    self.state = "ground"
                    yield "seq", bytes(self.seq), i
                elif b == 0x1B:
                    self.start()
                else:
                    self.seq.pop()
                    yield "ctl", b, i
            elif st == "csi":
                if 0x40 <= b <= 0x7E:
                    self.seq.append(b)
                    self.state = "ground"
                    yield "seq", bytes(self.seq), i
                elif 0x20 <= b <= 0x3F:
                    self.seq.append(b)
                elif b == 0x1B:
                    self.start()
                elif b < 0x20:
                    yield "ctl", b, i
            elif st == "string":
                if b == 0x07 and self.seq[1] == 0x5D:
                    self.seq.append(b)
                    self.state = "ground"
                    yield "seq", bytes(self.seq), i
                elif b == 0x1B:
                    self.state = "string-esc"
                elif len(self.seq) < self.STRING_LIMIT:
                    self.seq.append(b)
            elif b == 0x5C:  # string-esc: ESC \ is ST
                self.seq += b"\x1b\\"
                self.state = "ground"
                yield "seq", bytes(self.seq), i
            else:  # an ESC that is not ST ends the string and starts a sequence
                yield "seq", bytes(self.seq), max(i - 2, 0)
                self.start()
                i -= 1


def text_width(run):
    if run.isascii():
        return len(run)
    return sum(char_width(c) for c in run.decode("utf-8", "replace"))


def char_width(c):
    if unicodedata.combining(c) or unicodedata.category(c) in ("Mn", "Me", "Cf"):
        return 0
    return 2 if unicodedata.east_asian_width(c) in ("W", "F") else 1


class Cursor:
    """A rough cursor position, only good enough to answer CPR: printing,
    CR/LF/BS/HT and the common cursor motions. No scroll regions, origin
    mode, or grapheme clustering."""

    def __init__(self, rows, cols):
        self.rows, self.cols = rows, cols
        self.row = self.col = 0
        self.saved = (0, 0)
        self.wrapped = False
        self.scrolled = False

    def report(self):
        return self.row + 1, min(self.col, self.cols - 1) + 1

    def clamp(self):
        self.row = max(0, min(self.row, self.rows - 1))
        self.col = max(0, min(self.col, self.cols - 1))

    def down(self):
        self.scrolled |= self.row == self.rows - 1
        self.row = min(self.row + 1, self.rows - 1)

    def advance(self, n):
        while n > 0:
            if self.col >= self.cols:
                self.col = 0
                self.down()
                self.wrapped = True
            step = min(n, self.cols - self.col)
            self.col += step
            n -= step

    def ctl(self, b):
        if b == 0x0D:
            self.col = 0
        elif b in (0x0A, 0x0B, 0x0C):
            self.down()
        elif b == 0x08:
            self.col = max(0, min(self.col, self.cols - 1) - 1)
        elif b == 0x09:
            self.col = min(self.cols - 1, (self.col // 8 + 1) * 8)

    def esc(self, seq):
        final = seq[1:]
        if final == b"7":
            self.saved = (self.row, self.col)
        elif final == b"8":
            self.row, self.col = self.saved
        elif final == b"M":
            self.row = max(0, self.row - 1)
        elif final in (b"D", b"E"):
            self.down()
            self.col = 0 if final == b"E" else self.col
        elif final == b"c":
            self.row = self.col = 0

    def csi(self, private, params, inter, final):
        if inter:
            return
        if private == "?":
            modes = params.split(";")
            if final in "hl" and ("1049" in modes or "1048" in modes):
                if final == "h":
                    self.saved = (self.row, self.col)
                else:
                    self.row, self.col = self.saved
            return
        if private:
            return
        p = [int(x) if x.isdigit() else 0 for x in re.split("[;:]", params)] if params else []

        def arg(k, default=1):
            return (p[k] if k < len(p) else 0) or default

        col = min(self.col, self.cols - 1)
        if final in "Hf":
            self.row, self.col = arg(0) - 1, arg(1) - 1
        elif final == "A":
            self.row -= arg(0)
        elif final in "Be":
            self.row += arg(0)
        elif final in "Ca":
            self.col = col + arg(0)
        elif final == "D":
            self.col = col - arg(0)
        elif final in "EF":
            self.row += arg(0) if final == "E" else -arg(0)
            self.col = 0
        elif final in "G`":
            self.col = arg(0) - 1
        elif final == "d":
            self.row = arg(0) - 1
        elif final == "r":
            self.row = self.col = 0
        elif final == "s" and not params:
            self.saved = (self.row, self.col)
        elif final == "u" and not params:
            self.row, self.col = self.saved
        else:
            return
        self.clamp()


# Query classification ------------------------------------------------------

WINOPS_QUERIES = {"11", "13", "14", "15", "16", "18", "19", "20", "21"}


def classify(seq):
    """A query (kind, detail) if the sequence asks the terminal for a reply."""
    if seq.startswith(b"\x1b["):
        parts = split_csi(seq)
        if not parts:
            return None
        private, params, inter, final = parts
        key = (private, inter, final)
        if final == "c" and not inter and params in ("", "0") and private in ("", ">", "="):
            return {"": "DA1", ">": "DA2", "=": "DA3"}[private], ""
        if key == ("", "", "n") and params in ("5", "6"):
            return ("DSR", "5") if params == "5" else ("CPR", "")
        if key == ("?", "", "n"):
            return ("DECXCPR", "") if params == "6" else ("DSR", "?" + params)
        if inter == "$" and final == "p":
            return "DECRQM", private + params
        if key == (">", "", "q") and params in ("", "0"):
            return "XTVERSION", ""
        if key == ("?", "", "u") and not params:
            return "KITTY-KBD", ""
        if key == ("", "", "t") and params.split(";")[0] in WINOPS_QUERIES:
            return "XTWINOPS", params
        if key == ("?", "", "m"):
            return "XTQMODKEYS", params
        if key == ("?", "", "S") and params.count(";") == 2 and params.split(";")[1] in ("1", "4"):
            return "XTSMGRAPHICS", params
        if key == ("", "", "x") and params in ("", "0", "1"):
            return "DECREQTPARM", params
        return None
    if seq.startswith(b"\x1b]"):
        body = string_body(seq)
        fields = body.split(b";")
        if b"?" in fields[1:]:
            return "OSC", show(body[: body.index(b";?")])
        return None
    if seq.startswith(b"\x1bP"):
        body = string_body(seq)
        if body.startswith(b"+q"):
            names = [bytes.fromhex(h.decode()).decode("utf-8", "replace") if re.fullmatch(rb"(?:[0-9a-fA-F]{2})*", h) else show(h)
                     for h in body[2:].split(b";")]
            return "XTGETTCAP", ",".join(names)
        if body.startswith(b"$q"):
            return "DECRQSS", show(body[2:])
        return None
    if seq.startswith(b"\x1b_G") and re.search(rb"(^|,)a=q", string_body(seq)[1:]):
        return "KITTY-GFX", ""
    return None


def query_id(kind, detail):
    return f"{kind} {detail}" if detail else kind


# Feature detection ---------------------------------------------------------

PRIVATE_MODES = {
    "1": "decckm", "3": "deccolm", "5": "decscnm", "6": "decom", "7": "decawm",
    "9": "mouse-x10", "12": "cursor-blink", "25": "dectcem", "45": "reverse-wrap",
    "47": "alt-screen", "1047": "alt-screen", "1049": "alt-screen", "1048": "save-cursor-1048",
    "66": "decnkm", "69": "declrmm", "1000": "mouse-1000", "1001": "mouse-1001",
    "1002": "mouse-1002", "1003": "mouse-1003", "1004": "focus-1004", "1005": "mouse-1005",
    "1006": "mouse-1006", "1007": "alternate-scroll-1007", "1015": "mouse-1015",
    "1016": "mouse-1016", "1034": "meta-1034", "1036": "meta-esc-1036",
    "1039": "alt-esc-1039", "2004": "bracketed-paste", "2026": "sync-2026",
    "2027": "grapheme-2027", "2031": "color-scheme-2031", "2048": "in-band-resize-2048",
}
SGR_FEATURES = {
    1: "bold", 2: "dim", 3: "italic", 4: "underline", 5: "blink", 6: "blink",
    7: "reverse", 8: "conceal", 9: "strikethrough", 21: "double-underline",
    51: "framed", 52: "encircled", 53: "overline", 73: "superscript", 74: "subscript",
}
UNDERLINE_STYLES = {
    "0": "underline", "1": "underline", "2": "double-underline", "3": "undercurl",
    "4": "dotted-underline", "5": "dashed-underline",
}
CSI_FEATURES = {
    "L": "il-dl", "M": "il-dl", "@": "ich-dch", "P": "ich-dch", "X": "ech", "b": "rep",
    "S": "scroll-su-sd", "T": "scroll-su-sd", "g": "tabs", "I": "tabs", "Z": "tabs",
    "J": "ed", "K": "el",
}
OSC_FEATURES = {
    "0": "title", "1": "title", "2": "title", "4": "osc4-palette", "7": "osc7", "8": "osc8",
    "10": "osc-dynamic-colors", "11": "osc-dynamic-colors", "12": "osc-dynamic-colors",
    "22": "osc22-pointer", "52": "osc52", "104": "osc104", "110": "osc-dynamic-colors",
    "111": "osc-dynamic-colors", "112": "osc-dynamic-colors", "133": "osc133",
    "633": "osc633", "777": "osc777", "1337": "osc1337",
}


def sgr_features(params, out):
    if ":" in params:
        out.add("sgr-colon")
    items = params.split(";") if params else ["0"]
    i = 0
    while i < len(items):
        sub = items[i].split(":")
        i += 1
        code = int(sub[0]) if sub[0].isdigit() else 0
        if code in (38, 48, 58):
            if len(sub) > 1:
                model = sub[1]
            else:
                model = items[i] if i < len(items) else ""
                i += {"5": 2, "2": 4}.get(model, 1)
            out.add({"5": "256-color", "2": "truecolor"}.get(model, "sgr-extended-color"))
            if code == 58:
                out.add("underline-color")
        elif code == 4 and len(sub) > 1:
            out.add(UNDERLINE_STYLES.get(sub[1], "underline-style-other"))
        elif 30 <= code <= 37 or 40 <= code <= 47:
            out.add("16-color")
        elif 90 <= code <= 97 or 100 <= code <= 107:
            out.add("bright-color")
        elif code in SGR_FEATURES:
            out.add(SGR_FEATURES[code])


def seq_features(seq, out):
    if seq.startswith(b"\x1b["):
        parts = split_csi(seq)
        if not parts:
            out.add("csi-malformed")
            return
        private, params, inter, final = parts
        if final == "m" and not private and not inter:
            sgr_features(params, out)
        elif final == "h" and private == "?" and not inter:
            for mode in params.split(";"):
                name = PRIVATE_MODES.get(mode, f"dec-mode-{mode}")
                out.add(name)
                if name == "alt-screen":
                    out.add(f"alt-screen-{mode}")
        elif final == "l" and private == "?" and not inter:
            for mode in set(params.split(";")) & {"7", "25"}:
                out.add("decawm-off" if mode == "7" else "dectcem")
        elif final == "h" and not private and not inter:
            out.update({"4": "irm", "20": "lnm"}.get(m, f"ansi-mode-{m}") for m in params.split(";"))
        elif final == "q" and inter == " ":
            out.add("decscusr")
        elif final == "r" and not private and not inter:
            out.add("decstbm")
        elif final in "sr" and private == "?":
            out.add("xtsave-xtrestore")
        elif final == "u" and private in (">", "<", "="):
            out.add("kitty-kbd")
        elif final == "m" and private == ">":
            out.add("modify-other-keys")
        elif final == "t" and not private:
            out.add("title-stack" if params.split(";")[0] in ("22", "23") else "xtwinops")
        elif final in "su" and not private and not params:
            out.add("scosc-scorc")
        elif not private and not inter and final in CSI_FEATURES:
            out.add(CSI_FEATURES[final])
    elif seq.startswith(b"\x1b]"):
        num = show(string_body(seq).split(b";", 1)[0])
        if num == "9":
            out.add("osc9-progress" if string_body(seq).startswith(b"9;4;") else "osc9-notify")
        else:
            out.add(OSC_FEATURES.get(num, f"osc{num}"))
    elif seq.startswith(b"\x1bP"):
        body = string_body(seq)
        if body.startswith(b"tmux;"):
            out.add("dcs-tmux-passthrough")
        elif re.match(rb"[0-9;]*q", body):
            out.add("sixel")
        elif not body.startswith((b"+q", b"$q")):
            out.add("dcs")
    elif seq.startswith(b"\x1b_"):
        out.add("kitty-graphics" if seq.startswith(b"\x1b_G") else "apc")
    elif len(seq) == 3 and seq[1:2] in b"()*+-./" and seq[2:] == b"0":
        out.add("charset-dec")
    elif len(seq) == 3 and seq[1:2] in b"()*+" and seq[2:] not in (b"B", b"0"):
        out.add("charset-other")
    elif seq == b"\x1b#8":
        out.add("decaln")
    elif seq in (b"\x1b7", b"\x1b8"):
        out.add("decsc-decrc")
    elif seq == b"\x1bM":
        out.add("reverse-index")
    elif seq in (b"\x1b=", b"\x1b>"):
        out.add("keypad-mode")
    elif seq == b"\x1bH":
        out.add("tabs")
    elif seq == b"\x1bc":
        out.add("ris")


def text_features(run, out):
    if run.isascii():
        return
    for c in run.decode("utf-8", "replace"):
        o = ord(c)
        if o < 0x80 or o == 0xFFFD:
            continue
        if o == 0x200D:
            out.add("emoji-zwj")
        elif o == 0xFE0F:
            out.add("vs16")
        elif o == 0xFE0E:
            out.add("vs15")
        elif 0x1F3FB <= o <= 0x1F3FF:
            out.add("emoji-modifier")
        elif 0x1F1E6 <= o <= 0x1F1FF:
            out.add("regional-indicator")
        elif unicodedata.combining(c) or unicodedata.category(c) in ("Mn", "Me"):
            out.add("combining")
        elif unicodedata.category(c) == "Cf":
            out.add("format-chars")
        elif 0xE000 <= o <= 0xF8FF:
            out.add("private-use")
        elif 0x2500 <= o <= 0x259F:
            out.add("box-drawing")
        elif unicodedata.east_asian_width(c) in ("W", "F"):
            out.add("emoji" if o >= 0x1F000 else "wide")
        elif unicodedata.east_asian_width(c) == "A":
            out.add("ambiguous-width")


CTL_FEATURES = {0x07: "bel", 0x09: "tabs", 0x0E: "charset-shift", 0x0F: "charset-shift", 0x08: "backspace"}


# Recording -----------------------------------------------------------------

class ScenarioError(Exception):
    pass


class Session:
    def __init__(self, fd, pid, rows, cols, answers):
        self.fd, self.pid = fd, pid
        self.answers = answers
        self.out = bytearray()
        self.scanner = Scanner()
        self.cursor = Cursor(rows, cols)
        self.features = set()
        self.queries = []
        self.checkpoints = []
        self.modes = set(MODES_SET_AT_START)
        self.sgr = []
        self.sent_at = 0
        self.eof = False
        self.status = None

    def pump(self, timeout):
        """Read whatever arrives within timeout. True if anything did."""
        if self.eof:
            time.sleep(timeout)
            return False
        ready, _, _ = select.select([self.fd], [], [], timeout)
        if not ready:
            return False
        try:
            data = os.read(self.fd, 65536)
        except OSError as e:
            if e.errno != errno.EIO:
                raise
            data = b""
        if not data:
            self.eof = True
            return False
        self.feed(data)
        return True

    def feed(self, data):
        base = len(self.out)
        self.out += data
        for kind, value, end in self.scanner.feed(data):
            if kind == "text":
                self.cursor.advance(text_width(value))
                text_features(value, self.features)
            elif kind == "ctl":
                self.cursor.ctl(value)
                if value in CTL_FEATURES:
                    self.features.add(CTL_FEATURES[value])
            else:
                self.on_seq(value, base + end)
        if self.cursor.wrapped:
            self.features.add("soft-wrap")
        if self.cursor.scrolled:
            self.features.add("linefeed-scroll")

    def on_seq(self, seq, offset):
        if seq.startswith(b"\x1b["):
            parts = split_csi(seq)
            if parts:
                self.cursor.csi(*parts)
                private, params, inter, final = parts
                if final in "hl" and private in ("", "?") and not inter:
                    modes = {private + m for m in params.split(";")}
                    self.modes = self.modes | modes if final == "h" else self.modes - modes
                if final == "m" and not private and not inter:
                    items = params.split(";")
                    reset = items[0] in ("", "0")
                    self.sgr = items[1:] if reset else self.sgr + items
        elif len(seq) == 2:
            self.cursor.esc(seq)
        seq_features(seq, self.features)
        query = classify(seq)
        if not query:
            return
        qid = query_id(*query)
        policy = self.answers.get(qid, self.answers.get(query[0]))
        if policy == CURSOR:
            row, col = self.cursor.report()
            answer = (f"\x1b[?{row};{col};1R" if query[0] == "DECXCPR" else f"\x1b[{row};{col}R").encode()
        elif policy == MODE_STATE:
            mode = query[1]
            state = (1 if mode in self.modes else 2) if mode in SUPPORTED_MODES else 0
            answer = f"\x1b[{mode};{state}$y".encode()
        elif policy == SGR_STATE:
            answer = f"\x1bP1$r{';'.join(['0', *self.sgr])}m\x1b\\".encode()
        elif policy and query[0] == "OSC":
            answer = policy + (b"\x07" if seq.endswith(b"\x07") else b"\x1b\\")
        else:
            answer = policy
        if answer:
            os.write(self.fd, answer)
        self.queries.append({"offset": offset, "query": qid, "seq": show(seq), "answer": show(answer) if answer else None})

    def checkpoint(self, step):
        if not self.checkpoints or self.checkpoints[-1]["offset"] != len(self.out):
            self.checkpoints.append({"offset": len(self.out), "after_step": step})

    def settle(self, quiet, limit=15.0):
        deadline = time.monotonic() + limit
        while time.monotonic() < deadline and self.pump(quiet):
            pass

    def wait_for(self, pattern, timeout):
        """Wait until pattern shows up in the output since the last input sent."""
        regex = re.compile(pattern)
        deadline = time.monotonic() + timeout
        while not regex.search(self.out, self.sent_at):
            if time.monotonic() > deadline or self.eof:
                raise ScenarioError(f"waited {timeout}s for {pattern!r}")
            self.pump(0.05)

    def sleep(self, seconds):
        deadline = time.monotonic() + seconds
        while (left := deadline - time.monotonic()) > 0:
            self.pump(min(left, 0.05))

    def send(self, data, delay=0.0):
        self.sent_at = len(self.out)
        if not delay:
            os.write(self.fd, data)
            return
        for ch in data.decode():
            os.write(self.fd, ch.encode())
            self.sleep(delay)

    def reap(self):
        if self.status is None:
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                self.status = os.waitstatus_to_exitcode(status)
        return self.status is not None

    def wait_exit(self, timeout):
        deadline = time.monotonic() + timeout
        while not self.reap():
            if time.monotonic() > deadline:
                raise ScenarioError(f"program still running after {timeout}s")
            self.pump(0.05)
        drain = time.monotonic() + 1.0
        while not self.eof and time.monotonic() < drain:
            self.pump(0.05)

    def kill(self):
        if self.reap():
            return
        for sig in (signal.SIGTERM, signal.SIGKILL):
            os.kill(self.pid, sig)
            for _ in range(20):
                if self.reap():
                    return
                time.sleep(0.05)


def run_steps(sess, steps):
    for i, step in enumerate(steps):
        op = step[0]
        if op == "keys":
            sess.send(step[1])
        elif op == "type":
            sess.send(step[1], step[2])
        elif op == "settle":
            sess.settle(step[1])
            sess.checkpoint(i)
        elif op == "wait":
            sess.wait_for(step[1], step[2])
        elif op == "sleep":
            sess.sleep(step[1])
            sess.checkpoint(i)
        elif op == "exit":
            sess.wait_exit(step[1])
            sess.checkpoint(i)


def step_json(step):
    op = step[0]
    if op in ("keys", "type"):
        d = {op: show(step[1])}
        return d | ({"delay": step[2]} if op == "type" else {})
    if op == "wait":
        return {"wait_for": show(step[1]), "timeout": step[2]}
    return {op: step[1]}


# Scenario steps --------------------------------------------------------------

def keys(data, quiet=QUIET):
    return [("keys", data), ("settle", quiet)]


def typed(data, delay=0.06, quiet=QUIET):
    return [("type", data, delay), ("settle", quiet)]


def ex(cmd, quiet=QUIET):
    return keys(b":" + cmd.encode() + CR, quiet)


def wait(pattern, timeout=15, quiet=QUIET):
    return [("wait", pattern, timeout), ("settle", quiet)]


def started(quiet=0.6):
    return wait(rb"[\s\S]", 15, quiet)


def sleep(seconds):
    return [("sleep", seconds)]


def exit_(timeout=15):
    return [("exit", timeout)]


def mouse_click(row, col, button=0):
    """An SGR (1006) mouse press and release at a 1-based cell."""
    return f"\x1b[<{button};{col};{row}M\x1b[<{button};{col};{row}m".encode()


DOWN = b"\x1b[B"
UP = b"\x1b[A"

# Scenario inputs: real third-party text or generated text, never user files.

def copy_stdlib(*names):
    def setup(work, ctx):
        stdlib = Path(sysconfig.get_paths()["stdlib"])
        for name in names:
            shutil.copy(stdlib / name, work / name)
            data = (work / name).read_bytes()
            lines = data.count(b"\n")
            ctx["inputs"][name] = (f"CPython {sys.version.split()[0]} Lib/{name}, {lines} lines, "
                                   f"sha256 {hashlib.sha256(data).hexdigest()[:16]}")
    return setup


WORDS = [
    "alpha", "anchor", "buffer", "cache", "cell", "cursor", "damage", "glyph", "grid", "input",
    "kitty", "layout", "line", "margin", "mode", "parser", "render", "reflow", "region", "scroll",
    "selection", "shader", "sixel", "sync", "tab", "term", "theme", "title", "unicode", "viewport",
    "vte", "wrap", "atlas", "blink", "charset", "color", "decoder", "encoder", "font",
]


def write_wordlist(work, ctx):
    dirs = ["src", "src/vt", "src/render", "src/input", "tests", "docs", "benches"]
    exts = ["rs", "md", "toml", "txt", "py"]
    lines = [f"{d}/{w}_{(i * 7 + j) % 13:02}.{exts[(i + j) % len(exts)]}"
             for i, d in enumerate(dirs) for j, w in enumerate(WORDS)]
    (work / "words.txt").write_text("\n".join(lines) + "\n")
    ctx["inputs"]["words.txt"] = f"generated: {len(lines)} synthetic paths"


def make_repo(work, ctx):
    """A synthetic git repository with fixed authors and dates: deterministic hashes."""
    repo = work / "repo"
    repo.mkdir()
    env = ctx["env"] | {
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_AUTHOR_NAME": "Fixture Author", "GIT_AUTHOR_EMAIL": "author@example.invalid",
        "GIT_COMMITTER_NAME": "Fixture Committer", "GIT_COMMITTER_EMAIL": "committer@example.invalid",
    }
    clock = [1767225600]  # 2026-01-01T00:00:00Z

    def git(*args):
        clock[0] += 3600
        date = f"{clock[0]} +0000"
        subprocess.run([str(BINS["git"]), *args], cwd=repo, check=True, capture_output=True,
                       env=env | {"GIT_AUTHOR_DATE": date, "GIT_COMMITTER_DATE": date})

    def body(name, n, salt):
        return "".join(f"{name} line {i:03}: {WORDS[(i * salt) % len(WORDS)]} {WORDS[(i + salt) % len(WORDS)]}\n"
                       for i in range(n))

    def commit(msg, files):
        for path, text in files.items():
            (repo / path).parent.mkdir(parents=True, exist_ok=True)
            (repo / path).write_text(text)
        git("add", "-A")
        git("commit", "-q", "-m", msg)

    git("init", "-q", "-b", "main")
    commit("Initial commit", {"README.md": "# fixture\n", "src/grid.c": body("grid", 40, 3)})
    for k in range(1, 5):
        commit(f"grid: step {k}", {"src/grid.c": body("grid", 40 + k * 3, 3 + k)})
    git("checkout", "-q", "-b", "feature/parser")
    for k in range(1, 5):
        commit(f"parser: handle case {k}", {"src/parser.c": body("parser", 20 + k * 5, 5 + k)})
    git("checkout", "-q", "main")
    commit("docs: describe the grid", {"README.md": "# fixture\n\nA grid.\n"})
    git("merge", "-q", "--no-ff", "-m", "Merge branch 'feature/parser'", "feature/parser")
    git("checkout", "-q", "-b", "fix/unicode")
    commit("unicode: 端末 width table ✨", {"src/width.c": body("width", 30, 7)})
    commit("unicode: treat VS16 as emoji presentation", {"src/width.c": body("width", 34, 8)})
    git("checkout", "-q", "main")
    commit("A deliberately long commit subject that runs past the eighty column edge of the terminal window",
           {"src/grid.c": body("grid", 60, 9)})
    git("merge", "-q", "--no-ff", "-m", "Merge branch 'fix/unicode'", "fix/unicode")
    git("checkout", "-q", "-b", "wip/render")
    for k in range(1, 4):
        commit(f"render: wip {k}", {"src/render.c": body("render", 25 + k, 11 + k)})
    git("checkout", "-q", "main")
    for k in range(1, 6):
        commit(f"release: prepare 0.{k}.0", {"CHANGELOG.md": body("changelog", 5 * k, 13 + k)})
    git("tag", "v0.5.0")
    # Unstaged edits for git diff: changed, added and removed lines, a tab,
    # trailing whitespace and wide text.
    grid = (repo / "src/grid.c").read_text().splitlines(keepends=True)
    grid[5:9] = ["\tgrid_resize(cols, rows);\n", "changed line with trailing space   \n", "端末 wide text ✓\n"]
    grid[30:30] = [f"added line {i}\n" for i in range(12)]
    del grid[50:55]
    (repo / "src/grid.c").write_text("".join(grid))
    (repo / "README.md").write_text("# fixture\n\nA grid, now with a parser.\n")
    count = subprocess.run([str(BINS["git"]), "rev-list", "--all", "--count"], cwd=repo, env=env,
                           capture_output=True, text=True, check=True).stdout.strip()
    ctx["inputs"]["repo"] = f"generated git repository: {count} commits, 4 branches, a tag, unstaged edits"


def tmux_cleanup(ctx):
    subprocess.run([str(BINS["tmux"]), "-L", TMUX_SOCKET, "kill-server"], env=ctx["env"], capture_output=True,
                   check=False)


TMUX_SOCKET = "rtty-vt-fixtures"


# Scenarios -----------------------------------------------------------------

SCENARIOS = {}


def scenario(name, program, argv, steps, description, rows=24, cols=80, env=None,
             setup=(), teardown=None, answers=None, cwd="work"):
    SCENARIOS[name] = {
        "name": name, "program": program, "argv": argv, "steps": steps, "description": description,
        "rows": rows, "cols": cols, "env": env or {}, "setup": setup, "teardown": teardown,
        "answers": ANSWERS | (answers or {}), "cwd": cwd,
    }


# nvim's default diagnostic underline is plain; give each severity an extended style.
DIAGNOSTIC_STYLES = [
    "hi DiagnosticUnderlineError gui=undercurl guisp=#ff5f5f",
    "hi DiagnosticUnderlineWarn gui=underdashed guisp=#ffaf00",
    "hi DiagnosticUnderlineInfo gui=underdouble guisp=#5fafff",
    "hi DiagnosticUnderlineHint gui=underdotted guisp=#5fd7af",
]
LUA_DIAGNOSTICS = (
    "lua vim.diagnostic.config({virtual_text=true}) "
    "vim.diagnostic.set(vim.api.nvim_create_namespace('fixture'),0,{"
    "{lnum=1,col=0,end_col=4,severity=1,message='fixture error'},"
    "{lnum=3,col=4,end_col=11,severity=2,message='fixture warning'},"
    "{lnum=2,col=0,end_col=6,severity=3,message='fixture info'},"
    "{lnum=5,col=0,end_col=8,severity=4,message='fixture hint'}})"
)
LUA_FLOAT = (
    "lua local b=vim.api.nvim_create_buf(false,true) "
    "vim.api.nvim_buf_set_lines(b,0,-1,false,{'floating window','wide: 端末エミュ','emoji: 😀 ✓ e\N{COMBINING ACUTE ACCENT}'}) "
    "vim.api.nvim_open_win(b,false,{relative='editor',row=3,col=8,width=30,height=3,border='rounded',title=' fixture '})"
)
INSERT_TEXT = ("# 端末 terminal ✓ 😀 " + "\N{ZERO WIDTH JOINER}".join("👨👩👧")
               + " e\N{COMBINING ACUTE ACCENT} ❤\N{VARIATION SELECTOR-16}").encode()


def nvim_steps(rows):
    return (
        wait(rb"fnmatch\.py") + keys(b"20j") + keys(ctrl("d")) + keys(ctrl("d")) + keys(ctrl("u"))
        + keys(b"G") + keys(b"gg") + keys(ctrl("e") * 3)
        + ex("set notermguicolors") + ex("set termguicolors") + ex("colorscheme habamax")
        + ex("vsplit") + ex("split") + keys(ctrl("w") + b"l") + keys(b"30j")
        + keys(ctrl("w") + b"x") + keys(ctrl("w") + b"K") + keys(ctrl("w") + b"=")
        + keys(mouse_click(rows - 4, 5))
        + keys(b"gg") + keys(b"o") + keys(INSERT_TEXT) + keys(ESC)
        + keys(b"R") + keys(b"REPLACED") + keys(ESC)
        + keys(b"o") + keys(b"\x1b[200~pasted line one\rpasted line two\x1b[201~") + keys(ESC)
        + keys(b"V5j") + keys(ESC) + keys(ctrl("v") + b"3j8l") + keys(ESC)
        + keys(b"/match\r") + keys(b"n") + keys(b"n") + ex("nohlsearch")
        + keys(b"gg") + [step for cmd in DIAGNOSTIC_STYLES for step in ex(cmd)] + ex(LUA_DIAGNOSTICS) + keys(b"2G") + ex("lua vim.diagnostic.open_float()")
        + ex(LUA_FLOAT)
        + ex("only") + ex("terminal printf '\\033[1;32mgreen\\033[0m wide:端末\\n'; seq 1 5")
        + wait(rb"Process exited")
        + ex("qa!") + exit_()
    )


def emacs_steps(rows):
    return (
        wait(rb"fnmatch\.py", 20, 1.0) + keys(ctrl("v")) + keys(ctrl("v")) + keys(ESC + b"v")
        + keys(ctrl("x") + b"2") + keys(ctrl("x") + b"3") + keys(ctrl("x") + b"o") + keys(ctrl("n") * 5)
        + keys(ESC + b"x") + typed(b"list-colors-display") + keys(CR, 0.6)
        + keys(ctrl("x") + b"o") + keys(ctrl("v"))
        + keys(ESC + b"x") + typed(b"xterm-mouse-mode") + keys(CR)
        + keys(mouse_click(3, 10))
        + keys(ctrl("x") + b"1")
        + keys(ctrl("x") + b"b") + typed(b"*scratch*") + keys(CR)
        + typed("端末 wide ✓ 😀 e\N{COMBINING ACUTE ACCENT}\r".encode())
        + keys(ctrl("x") + ctrl("c")) + exit_()
    )


for _rows, _cols in ((24, 80), (40, 120)):
    scenario(f"nvim-{_cols}x{_rows}", "nvim", [BINS["nvim"], "--clean", "fnmatch.py"], nvim_steps(_rows),
             "nvim --clean: scroll, 256-color/truecolor, habamax, splits, mouse, insert/replace/visual, "
             "paste, search, diagnostics with curly/dashed/double/dotted coloured underlines, floats, "
             ":terminal, :qa!",
             rows=_rows, cols=_cols, setup=[copy_stdlib("fnmatch.py")])
    scenario(f"emacs-{_cols}x{_rows}", "emacs", [BINS["emacs"], "-nw", "-Q", "fnmatch.py"], emacs_steps(_rows),
             "emacs -nw -Q: font-locked Python, C-x 2/3, M-x list-colors-display, xterm-mouse-mode, "
             "wide text in *scratch*, C-x C-c",
             rows=_rows, cols=_cols, setup=[copy_stdlib("fnmatch.py")])

scenario("emacs-80x24-alacritty-da2", "emacs", [BINS["emacs"], "-nw", "-Q", "fnmatch.py"], emacs_steps(24),
         "emacs as above, with an alacritty-style DA2 reply (version 2500): xterm.el queries the "
         "background and enables modifyOtherKeys",
         setup=[copy_stdlib("fnmatch.py")], answers={"DA2": b"\x1b[>0;2500;1c"})

scenario("vim-80x24", "vim", [BINS["vim"], "--clean", "fnmatch.py"],
         wait(rb"fnmatch\.py") + keys(ctrl("f")) + keys(ctrl("d")) + keys(b"10j") + keys(b"G") + keys(b"gg")
         + ex("split") + ex("vsplit") + keys(ctrl("w") + b"w") + keys(ctrl("f"))
         + keys(b"o") + keys(INSERT_TEXT) + keys(ESC) + keys(b"/match\r") + keys(b"n")
         + ex("qa!") + exit_(),
         "vim --clean: scroll, split, vsplit, insert wide text, search, :qa!",
         setup=[copy_stdlib("fnmatch.py")])

scenario("less-80x24", "less", [BINS["less"], "argparse.py"],
         started() + keys(b" ") + keys(b" ") + keys(b"b") + keys(b"/add_argument\r") + keys(b"n")
         + keys(b"n") + keys(b"N") + keys(b"G") + keys(b"g") + keys(b"-N") + keys(b"j") + keys(b"q") + exit_(),
         "less on CPython's argparse.py (~2,800 lines): page, search, jump to end and back, toggle line numbers",
         setup=[copy_stdlib("argparse.py")])

scenario("man-ls-80x24", "man", [BINS["man"], "ls"],
         wait(rb"NAME", 20, 0.5) + keys(b" ") + keys(b"/Recursively\r") + keys(b"n") + keys(b"q") + exit_(),
         "man ls through its pager: overstrike bold/underline rendered by less, paging, search")

scenario("bat-80x24", "bat", [BINS["bat"], "--style=full", "--paging=never", "fnmatch.py"],
         exit_(20), "bat --style=full: line numbers, grid, header, truecolor syntax highlighting",
         setup=[copy_stdlib("fnmatch.py")])

scenario("git-log-80x24", "git", [BINS["git"], "log", "--graph", "--oneline", "--color", "--all"],
         started() + keys(b" ") + keys(b" ") + keys(b"q") + exit_(),
         "git log --graph --oneline --color --all of a synthetic repo, paged by less -FRX",
         setup=[make_repo], cwd="work/repo", env={"GIT_CONFIG_NOSYSTEM": "1"})

scenario("git-diff-80x24", "git", [BINS["git"], "diff", "--color"],
         started() + keys(b" ") + keys(b"q") + exit_(),
         "git diff --color of unstaged edits in a synthetic repo, paged by less -FRX",
         setup=[make_repo], cwd="work/repo", env={"GIT_CONFIG_NOSYSTEM": "1"})

scenario("fzf-80x24", "fzf", [BINS["sh"], "-c", f"{BINS['fzf']} < words.txt"],
         started() + typed(b"rend", 0.12) + keys(DOWN) + keys(DOWN) + keys(UP) + typed(b"er", 0.12)
         + keys(CR) + exit_(),
         "fzf full screen over generated paths: type a query, move, select",
         setup=[write_wordlist])

scenario("fzf-height-80x24", "fzf", [BINS["sh"], "-c", f"{BINS['seq']} 1 5; {BINS['fzf']} --height=40% --border < words.txt"],
         started() + typed(b"sync", 0.12) + keys(DOWN) + keys(CR) + exit_(),
         "fzf --height=40% --border below five lines of output: inline mode, uses CPR",
         setup=[write_wordlist])

scenario("top-80x24", "top", [BINS["top"], "-o", "+pid", "-n", "12", "-s", "1"],
         started(0.2) + sleep(2.6) + keys(b"q") + exit_(),
         "macOS top sorted by ascending pid, 12 rows (system daemons only), first frame plus two one-second refreshes")

scenario("tmux-80x24", "tmux",
         [BINS["tmux"], "-L", TMUX_SOCKET, "-f", "/dev/null", "new-session", "-s", "fx", "/bin/sh",
          ";", "set-option", "-g", "default-command", "/bin/sh",
          ";", "set-option", "-g", "status-right", " rtty-vt fixture "],
         wait(rb"\$ ") + typed(b"printf '\\033[1;31mred\\033[0m wide:\xe7\xab\xaf\xe6\x9c\xab\\n'\r", 0.02)
         + keys(ctrl("b") + b"%") + typed(b"seq 1 40\r", 0.02)
         + keys(ctrl("b") + b'"') + typed(b"echo hello from pane three\r", 0.02)
         + keys(ctrl("b") + b"o") + keys(ctrl("b") + b"z") + keys(ctrl("b") + b"z")
         + keys(ctrl("b") + b":") + typed(b"kill-server\r", 0.02) + exit_(),
         "tmux -f /dev/null on an isolated socket: three panes, typing, zoom, kill-server",
         env={"SHELL": "/bin/sh", "PS1": "$ "}, teardown=tmux_cleanup)

scenario("seq-flood-80x24", "seq", [BINS["seq"], "1", "200000"], exit_(60),
         "seq 1 200000: a 1.3 MB flood of short lines, scrolling the whole screen")

SYNTHETIC = {
    "sgr": "every SGR attribute: 16/256/truecolor in ; and : forms, all underline styles, 58/59 underline colour, malformed params",
    "decscusr": "DECSCUSR 0-6 and the default form, DECTCEM and cursor-blink mode",
    "alt-screen": "alternate screen via 47, 1047 and 1049 with content on both screens and cursor save/restore",
    "decstbm": "scroll regions: LF and RI at margins, SU/SD, IL/DL inside and outside, origin mode, reset",
    "tabs": "default tab stops, HTS/TBC, CHT/CBT, tabs at the right margin, tabs not erasing",
    "charset-dec": "DEC special graphics via G0 and G1 with SO/SI, UK charset, back to ASCII",
    "unicode": "wide CJK, combining marks, ZWJ emoji, skin tones, flags, VS15/VS16, wide at the margin",
    "wrap": "soft-wrapped long lines, pending-wrap at column 80, DECAWM off, wrap at the bottom",
    "osc8": "OSC 8 hyperlinks: plain, with id, split across lines, BEL and ST terminators",
    "osc-shell": "a shell session with OSC 133 prompt marks, OSC 7 cwd, OSC 1337 SetUserVar, titles, OSC 52",
    "sync-2026": "60 full-screen frames, each inside a mode 2026 synchronized update",
    "edit-ops": "ICH/DCH/ECH/EL/ED/REP/IL/DL, insert mode, DECALN",
    "modes": "input-affecting modes: mouse 1000-1006/1015, focus, bracketed paste, DECCKM, keypad, kitty keyboard push/pop, modifyOtherKeys, title stack",
}
for _name, _desc in SYNTHETIC.items():
    scenario(_name, "synth.py", [Path(sys.executable), "-I", HERE / "synth.py", _name], exit_(30), _desc)


# Driver --------------------------------------------------------------------

def base_env(scratch):
    return {
        "TERM": "xterm-256color",
        "COLORTERM": "truecolor",
        "LANG": "en_US.UTF-8",
        "HOME": str(scratch / "home"),
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin",
        "SHELL": "/bin/sh",
        "TMPDIR": str(scratch / "tmp"),
        "TMUX_TMPDIR": str(scratch / "tmux"),
    }


def program_version(program, env):
    if program == "synth.py":
        return f"synth.py (Python {sys.version.split()[0]})"
    flag = VERSION_FLAGS.get(program)
    if flag:
        out = subprocess.run([str(BINS[program]), flag], env=env, capture_output=True, text=True, timeout=20,
                             check=True)
        return (out.stdout or out.stderr).strip().splitlines()[0]
    macos = subprocess.run(["/usr/bin/sw_vers", "-productVersion"], capture_output=True, text=True,
                           check=True).stdout.strip()
    return f"macOS {macos} {BINS[program]}"


def public(value, scratch):
    """Strip machine-specific paths before they go into the JSON."""
    text = str(value)
    for name, path in BINS.items():
        if path.is_relative_to(USER_HOME):
            text = text.replace(str(path), name)
    return (text.replace(str(scratch), "<scratch>").replace(str(HERE / "synth.py"), "synth.py")
            .replace(sys.executable, "python3"))


def spawn(argv, env, cwd, rows, cols):
    pid, fd = pty.fork()
    if pid == 0:
        try:
            termios.tcsetwinsize(0, (rows, cols))
            attrs = termios.tcgetattr(0)
            attrs[0] |= termios.IUTF8
            termios.tcsetattr(0, termios.TCSANOW, attrs)
            os.chdir(cwd)
            os.execve(argv[0], argv, env)
        except BaseException as e:  # noqa: BLE001 - the forked child must never return into the parent's code
            os.write(2, f"capture.py: cannot start {argv[0]}: {e}\r\n".encode())
        os._exit(127)
    return pid, fd


def record(sc, scratch, out_dir):
    for sub in ("home", "tmp", "tmux"):
        shutil.rmtree(scratch / sub, ignore_errors=True)
        (scratch / sub).mkdir(parents=True)
    (scratch / "tmux").chmod(0o700)
    work = scratch / "home" / "work"
    work.mkdir()
    env = base_env(scratch) | sc["env"]
    ctx = {"env": env, "inputs": {}}
    for setup in sc["setup"]:
        setup(work, ctx)
    argv = [str(a) for a in sc["argv"]]
    began = time.monotonic()
    pid, fd = spawn(argv, env, scratch / "home" / sc["cwd"], sc["rows"], sc["cols"])
    sess = Session(fd, pid, sc["rows"], sc["cols"], sc["answers"])
    try:
        run_steps(sess, sc["steps"])
    except ScenarioError:
        (scratch / f"{sc['name']}.partial.out").write_bytes(bytes(sess.out))
        raise
    finally:
        sess.kill()
        os.close(fd)
        if sc["teardown"]:
            sc["teardown"](ctx)
    data = bytes(sess.out)
    meta = {
        "name": sc["name"],
        "description": sc["description"],
        "program": sc["program"],
        "version": public(program_version(sc["program"], env), scratch),
        "argv": [public(a, scratch) for a in argv],
        "rows": sc["rows"],
        "cols": sc["cols"],
        "env": {k: public(v, scratch) for k, v in env.items()},
        "inputs": ctx["inputs"],
        "input": [step_json(s) for s in sc["steps"]],
        "answers": {k: POLICY_NOTES[v] if isinstance(v, str) else show(v) for k, v in sc["answers"].items()},
        "queries": sess.queries,
        "features": sorted(sess.features),
        "checkpoints": sess.checkpoints,
        "bytes": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
        "exit_status": sess.status,
        "duration_s": round(time.monotonic() - began, 1),
        "recorded": time.strftime("%Y-%m-%d"),
    }
    (out_dir / f"{sc['name']}.out").write_bytes(data)
    (out_dir / f"{sc['name']}.json").write_text(json.dumps(meta, indent=1, ensure_ascii=False) + "\n")
    return meta


TABLE_BEGIN = "<!-- fixtures:begin -->"
TABLE_END = "<!-- fixtures:end -->"


def update_readme(out_dir):
    readme = out_dir / "README.md"
    if not readme.exists() or TABLE_BEGIN not in readme.read_text():
        return
    rows = ["| fixture | size | rows×cols | program | features | description |",
            "|---|---:|---|---|---|---|"]
    for path in sorted(out_dir.glob("*.json")):
        m = json.loads(path.read_text())
        rows.append(f"| `{m['name']}` | {m['bytes'] / 1024:,.1f} KiB | {m['rows']}×{m['cols']} | "
                    f"{m['version']} | {', '.join(m['features'])} | {m['description']} |")
    text = readme.read_text()
    head, rest = text.split(TABLE_BEGIN, 1)
    tail = rest.split(TABLE_END, 1)[1]
    readme.write_text(f"{head}{TABLE_BEGIN}\n{chr(10).join(rows)}\n{TABLE_END}{tail}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("scenarios", nargs="*", help="scenario names (default: all)")
    parser.add_argument("--scratch", type=Path, help="scratch directory: HOME, TMPDIR and inputs live here")
    parser.add_argument("--out", type=Path, default=HERE, help="where .out/.json go (default: next to this script)")
    parser.add_argument("--bin", action="append", default=[], metavar="NAME=PATH", help="override a program path")
    parser.add_argument("--list", action="store_true", help="list scenarios and exit")
    args = parser.parse_args()
    if args.list:
        for sc in SCENARIOS.values():
            print(f"{sc['name']:28} {sc['description']}")
        return 0
    if not args.scratch:
        parser.error("--scratch is required")
    for override in args.bin:
        name, path = override.split("=", 1)
        BINS[name] = Path(path)
    unknown = [n for n in args.scenarios if n not in SCENARIOS]
    if unknown:
        parser.error(f"unknown scenarios: {', '.join(unknown)}")
    scratch = args.scratch.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    failed = []
    for name in args.scenarios or SCENARIOS:
        try:
            meta = record(SCENARIOS[name], scratch, args.out)
        except ScenarioError as e:
            print(f"{name}: FAILED: {e}", file=sys.stderr)
            failed.append(name)
            continue
        print(f"{name}: {meta['bytes']} bytes, {len(meta['queries'])} queries, exit {meta['exit_status']}, "
              f"{meta['duration_s']}s")
    update_readme(args.out)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
