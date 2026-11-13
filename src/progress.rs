//! A program's progress as it reports it, ConEmu's OSC 9;4 or OSC 7501's program status, for its
//! tab's pill and the Dock's badge: what each report means, and many panes' progress as one.

use serde::{Deserialize, Serialize};

/// How the work goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Normal,
    Error,
    /// Going, with no telling how far.
    Indeterminate,
    /// Held: paused, or waiting on the person (OSC 7501's `blocked`).
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub state: State,
    pub percent: Option<u8>,
}

/// OSC 9;4's `state` and `percent`: 0 removes the progress.
pub fn reported(state: u8, percent: Option<u8>) -> Option<Progress> {
    let state = match state {
        1 => State::Normal,
        2 => State::Error,
        3 => State::Indeterminate,
        4 => State::Paused,
        _ => return None,
    };
    Some(Progress { state, percent: percent.filter(|_| state != State::Indeterminate) })
}

/// OSC 7501's `key=value:…` body: `Some(None)` when it ends the progress (`done`, `idle`,
/// `clear`); `None` when it says nothing to go by: no known state, a `progress` past 100, or a
/// sub-record (one with an `id`; as in tutti, only the root record counts).
pub fn status(body: &str) -> Option<Option<Progress>> {
    let field = |key: &str| {
        body.split(':').filter_map(|pair| pair.split_once('=')).find(|(name, _)| *name == key)
    };
    if field("id").is_some() {
        return None;
    }
    let percent = match field("progress") {
        Some((_, percent)) => Some(percent.parse().ok().filter(|&percent| percent <= 100)?),
        None => None,
    };
    let state = match field("state")?.1 {
        "working" if percent.is_some() => State::Normal,
        "working" => State::Indeterminate,
        "blocked" => State::Paused,
        "error" => State::Error,
        "done" | "idle" | "clear" => return Some(None),
        _ => return None,
    };
    Some(Some(Progress { state, percent }))
}

/// Many panes' progress as one: an error first, else the least done, else one going with no
/// percent.
pub fn combine(all: impl IntoIterator<Item = Progress>) -> Option<Progress> {
    all.into_iter().min_by_key(|progress| {
        (progress.state != State::Error, progress.percent.unwrap_or(u8::MAX))
    })
}

/// The Dock's badge for it: `!` on an error, else its percent, else `…` while it cannot say.
pub fn badge(progress: Option<Progress>) -> Option<String> {
    let progress = progress?;
    Some(match (progress.state, progress.percent) {
        (State::Error, _) => "!".into(),
        (_, Some(percent)) => format!("{percent}%"),
        (_, None) => "…".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(state: State, percent: Option<u8>) -> Progress {
        Progress { state, percent }
    }

    #[test]
    fn osc_9_4_sets_errs_goes_on_pauses_and_removes() {
        assert_eq!(reported(1, Some(40)), Some(at(State::Normal, Some(40))));
        assert_eq!(reported(2, None), Some(at(State::Error, None)));
        assert_eq!(reported(3, Some(40)), Some(at(State::Indeterminate, None)));
        assert_eq!(reported(4, Some(70)), Some(at(State::Paused, Some(70))));
        assert_eq!(reported(0, Some(40)), None);
    }

    #[test]
    fn osc_7501_reads_its_state_and_progress() {
        let working = status("state=working:app=build:progress=40");
        assert_eq!(working, Some(Some(at(State::Normal, Some(40)))));
        assert_eq!(status("app=claude:state=working"), Some(Some(at(State::Indeterminate, None))));
        assert_eq!(status("state=blocked:kind=permission"), Some(Some(at(State::Paused, None))));
        assert_eq!(status("state=error:progress=90"), Some(Some(at(State::Error, Some(90)))));
        for ended in ["state=done:msg=QnVpbHQ=", "state=idle", "state=clear"] {
            assert_eq!(status(ended), Some(None), "{ended}");
        }
        // Nothing to go by: no state, an unknown one, a bad progress, a record with an id.
        let ignored = ["app=x", "state=asleep", "state=working:progress=101", "state=working:id=2"];
        for body in ignored {
            assert_eq!(status(body), None, "{body}");
        }
    }

    #[test]
    fn many_panes_show_an_error_else_the_least_done_else_going() {
        let going = at(State::Indeterminate, None);
        let (forty, seventy) = (at(State::Normal, Some(40)), at(State::Paused, Some(70)));
        assert_eq!(combine([going, seventy, forty]), Some(forty));
        assert_eq!(combine([forty, at(State::Error, None), going]), Some(at(State::Error, None)));
        assert_eq!(combine([going]), Some(going));
        assert_eq!(combine([]), None);
    }

    #[test]
    fn the_badge_is_the_percent_or_an_error_or_going() {
        let badge = |state, percent| badge(Some(at(state, percent)));
        assert_eq!(badge(State::Normal, Some(40)).as_deref(), Some("40%"));
        assert_eq!(badge(State::Paused, Some(70)).as_deref(), Some("70%"));
        assert_eq!(badge(State::Error, Some(90)).as_deref(), Some("!"));
        assert_eq!(badge(State::Indeterminate, None).as_deref(), Some("…"));
        assert_eq!(super::badge(None), None, "none once every program is done");
    }
}
