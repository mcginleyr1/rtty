/// The modes a program sets and a host reads to encode input or draw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Modes {
    pub alt_screen: bool,
    /// DECCKM: cursor keys send `ESC O`.
    pub cursor_keys: bool,
    /// DECKPAM / DECNKM: the keypad sends application sequences.
    pub keypad: bool,
    /// DECAWM.
    pub autowrap: bool,
    /// DECOM: cursor addressing is relative to the scroll region.
    pub origin: bool,
    /// IRM.
    pub insert: bool,
    /// LNM: LF, VT and FF also return the carriage.
    pub newline: bool,
    /// DECTCEM.
    pub cursor_visible: bool,
    /// ATT610 (`?12`).
    pub cursor_blink: bool,
    /// DECSCNM: the whole screen in reverse video.
    pub reverse_video: bool,
    pub mouse: MouseMode,
    pub mouse_encoding: MouseEncoding,
    /// 1007: the wheel sends cursor keys on the alternate screen.
    pub alternate_scroll: bool,
    /// 1004.
    pub focus_reports: bool,
    /// 2004.
    pub bracketed_paste: bool,
    /// 2026.
    pub sync_update: bool,
    /// 2031.
    pub color_scheme_reports: bool,
    /// XTMODKEYS `modifyOtherKeys` level (`CSI > 4 ; n m`).
    pub modify_other_keys: u8,
}

impl Default for Modes {
    fn default() -> Self {
        Modes {
            alt_screen: false,
            cursor_keys: false,
            keypad: false,
            autowrap: true,
            origin: false,
            insert: false,
            newline: false,
            cursor_visible: true,
            cursor_blink: false,
            reverse_video: false,
            mouse: MouseMode::None,
            mouse_encoding: MouseEncoding::Default,
            alternate_scroll: false,
            focus_reports: false,
            bracketed_paste: false,
            sync_update: false,
            color_scheme_reports: false,
            modify_other_keys: 0,
        }
    }
}

/// Which mouse events a program asked for; the modes exclude each other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseMode {
    #[default]
    None,
    /// 9: presses only.
    X10,
    /// 1000: presses and releases.
    Click,
    /// 1002: and motion with a button held.
    Drag,
    /// 1003: and all motion.
    Motion,
}

/// How mouse reports are encoded; the encodings exclude each other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseEncoding {
    #[default]
    Default,
    /// 1005.
    Utf8,
    /// 1006.
    Sgr,
    /// 1015.
    Urxvt,
    /// 1016.
    SgrPixels,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// A DECSCUSR cursor style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorStyle {
    pub shape: CursorShape,
    pub blinking: bool,
}

impl CursorStyle {
    /// The DECSCUSR parameter that selects this style.
    pub fn decscusr(self) -> u8 {
        let base = match self.shape {
            CursorShape::Block => 1,
            CursorShape::Underline => 3,
            CursorShape::Bar => 5,
        };
        base + u8::from(!self.blinking)
    }

    pub(crate) fn from_decscusr(param: u16) -> Option<Option<CursorStyle>> {
        let shape = match param {
            0 => return Some(None),
            1 | 2 => CursorShape::Block,
            3 | 4 => CursorShape::Underline,
            5 | 6 => CursorShape::Bar,
            _ => return None,
        };
        Some(Some(CursorStyle { shape, blinking: param % 2 == 1 }))
    }
}
