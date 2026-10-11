use crate::modes::{CursorStyle, MouseEncoding, MouseMode};

/// Something the stream asked of the host, drained with
/// `Terminal::take_events`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Bytes to write back to the pty: the answer to a query.
    Reply(Vec<u8>),
    /// BEL outside a string. A run of bells with nothing between them is one
    /// event.
    Bell,
    /// OSC 0 or 2, or a title popped off the stack (XTWINOPS 23).
    Title(String),
    /// OSC 0 or 1.
    IconName(String),
    /// OSC 9 (a body) or OSC 777 `notify` (a title and/or a body).
    Notification {
        title: Option<String>,
        body: Option<String>,
    },
    /// ConEmu's OSC 9;4: `state` 0 clears, 1 normal, 2 error, 3
    /// indeterminate, 4 paused.
    Progress {
        state: u8,
        percent: Option<u8>,
    },
    /// OSC 52 set, decoded: `selection` is the request's target list (`c`,
    /// `p`, `s`, `0`–`7`; empty means `s0`). Reads are never answered.
    Clipboard {
        selection: String,
        text: String,
    },
    /// OSC 7: the directory a `file://host/path` URL names, decoded.
    Cwd(String),
    /// OSC 133 shell integration, at the cursor when it arrived.
    Mark(Mark),
    /// iTerm2's OSC 1337 `SetUserVar`, decoded; an empty value unsets.
    UserVar {
        name: String,
        value: String,
    },
    /// OSC 7501's body, after `7501;`, as the program sent it.
    ProgramStatus(String),
    Mode(ModeChange),
    /// DECSCUSR: `None` asks for the host's default.
    CursorStyle(Option<CursorStyle>),
}

/// A mode a host acts on, as it changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeChange {
    AltScreen(bool),
    Mouse(MouseMode),
    MouseEncoding(MouseEncoding),
    /// 1004.
    FocusReports(bool),
    /// 2004.
    BracketedPaste(bool),
    /// 2026: hold rendering while set.
    SyncUpdate(bool),
    /// 2031: the program wants `?997` reports when the colour scheme flips.
    ColorSchemeReports(bool),
    /// The active kitty keyboard flags.
    KittyKeyboard(u8),
    /// The xterm modifyOtherKeys level (`CSI > 4 ; n m`): 0 off, 1, 2.
    ModifyOtherKeys(u8),
}

/// An OSC 133 mark: where on the grid a shell said a prompt, a command or
/// its output began, or a command ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub kind: MarkKind,
    /// The absolute row (`Terminal::row`).
    pub row: u64,
    pub col: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkKind {
    /// `A`.
    PromptStart,
    /// `B`: the prompt ended, the command line begins.
    CommandStart,
    /// `C`: the command runs, its output begins.
    OutputStart,
    /// `D[;exit]`.
    CommandEnd { exit: Option<i32> },
}
