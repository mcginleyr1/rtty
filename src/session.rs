//! The windows' layout, kept for the next launch: each window's frame, its tabs, their splits with
//! ratios, zoom and focus, and each pane's working directory. Never what runs in a pane or what it
//! shows: a restored pane starts a new login shell in its directory.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::mem;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use gpui::{Bounds, Pixels};
use serde::{Deserialize, Serialize};

use crate::layout::{PaneId, Tabs};

/// A window as it was.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// Its frame on its display, in points from the display's top left.
    pub frame: Bounds<Pixels>,
    /// The UUID of the display it was on.
    pub display: Option<String>,
    pub tabs: Tabs,
    /// Each pane's working directory, as its shell last reported it (OSC 7), else as its
    /// foreground process had it.
    pub cwds: BTreeMap<PaneId, PathBuf>,
}

/// A saved frame narrower or shorter than this, in points, is no window's.
const MIN_FRAME: f32 = 100.;

/// `$XDG_STATE_HOME/rtty/layout.json`, else `~/.local/state/rtty/layout.json`. An empty or
/// relative `XDG_STATE_HOME` is ignored, as the XDG spec says.
pub fn path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let state = state.filter(|state| state.is_absolute());
    let state = state
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")));
    state.map(|state| state.join("rtty/layout.json"))
}

/// Takes the layout in `path` for this rtty for the rest of its life: an exclusive lock on the
/// file beside it, which the system lets go when the process ends. Whether it got it; another rtty
/// holding it keeps the layout instead.
pub fn take(path: &Path) -> io::Result<bool> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let lock = path.with_extension("lock");
    let file =
        OpenOptions::new().write(true).create(true).truncate(false).mode(0o600).open(lock)?;
    // SAFETY: flock on a descriptor `file` owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = io::Error::last_os_error();
        return if err.kind() == io::ErrorKind::WouldBlock { Ok(false) } else { Err(err) };
    }
    // Held open, and so locked, until the process exits.
    mem::forget(file);
    Ok(true)
}

/// The windows saved in `path`, or why there are none to restore: no file, or one whose windows do
/// not hold together (a tab's focus not among its panes, a pane in two places, a ratio past 1, a
/// pane id no rtty counts to, a frame no window can have).
pub fn read(path: &Path) -> Result<Vec<Window>, String> {
    let text = fs::read_to_string(path).map_err(|err| err.to_string())?;
    let windows: Vec<Window> = serde_json::from_str(&text).map_err(|err| err.to_string())?;
    let mut seen = HashSet::new();
    let mut panes = windows.iter().flat_map(|window| window.tabs.all()).flat_map(|tab| tab.panes());
    if !panes.all(|pane| seen.insert(pane)) {
        return Err("a pane is in two places".into());
    }
    if seen.iter().any(|&pane| pane > PaneId::from(u32::MAX)) {
        return Err("a pane id past 2^32".into());
    }
    let usable = |frame: &Bounds<Pixels>| {
        let (origin, size) = (frame.origin, frame.size);
        let numbers = [origin.x, origin.y, size.width, size.height].map(f32::from);
        numbers.iter().all(|n| n.is_finite()) && numbers[2] >= MIN_FRAME && numbers[3] >= MIN_FRAME
    };
    if !windows.iter().all(|window| usable(&window.frame)) {
        return Err("a window's frame is too small or not a number".into());
    }
    match windows.iter().all(|window| window.tabs.sound()) {
        true => Ok(windows),
        false => Err("a window's tabs do not hold together".into()),
    }
}

/// Writes `json` to `path`, readable and writable by its owner alone, whole: into a file beside
/// it, then renamed over it. A write that fails leaves nothing beside it.
pub fn write(path: &Path, json: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut partial = path.as_os_str().to_owned();
    partial.push(format!(".{}", std::process::id()));
    let written = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&partial)
        .and_then(|mut file| file.write_all(json.as_bytes()))
        .and_then(|()| fs::rename(&partial, path));
    if written.is_err() {
        let _ = fs::remove_file(&partial);
    }
    written
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use gpui::{point, px, size};

    use super::*;
    use crate::layout::Direction;

    fn window(tabs: Tabs) -> Window {
        let frame = Bounds::new(point(px(40.), px(60.)), size(px(900.), px(600.)));
        let cwds = BTreeMap::from([(1, PathBuf::from("/tmp"))]);
        Window { frame, display: Some("a-display".into()), tabs, cwds }
    }

    #[test]
    fn a_layout_comes_back_as_it_was_saved_owners_only() {
        let dir = std::env::temp_dir().join(format!("rtty-session-{}", std::process::id()));
        let path = dir.join("rtty/layout.json");
        let mut tabs = Tabs::new(1);
        tabs.active_mut().split(1, Direction::Right, 2);
        tabs.active_mut().toggle_zoom();
        tabs.open(3);
        tabs.select(0);
        let windows = vec![window(tabs), window(Tabs::new(4))];
        write(&path, &serde_json::to_string(&windows).unwrap()).unwrap();
        assert_eq!(read(&path).unwrap(), windows);
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let names: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().flatten().collect();
        assert_eq!(names.len(), 1, "nothing left beside it");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_or_broken_layout_says_why() {
        let dir = std::env::temp_dir().join(format!("rtty-broken-{}", std::process::id()));
        let path = dir.join("layout.json");
        assert!(read(&path).unwrap_err().contains("No such file"));
        let written = |windows: &[Window]| {
            write(&path, &serde_json::to_string(windows).unwrap()).unwrap();
            read(&path)
        };
        let twice = [window(Tabs::new(1)), window(Tabs::new(1))];
        assert_eq!(written(&twice).unwrap_err(), "a pane is in two places");
        let far = written(&[window(Tabs::new(1 << 32))]).unwrap_err();
        assert_eq!(far, "a pane id past 2^32");
        let mut split = Tabs::new(1);
        split.active_mut().split(1, Direction::Down, 2);
        let json = serde_json::to_string(&[window(split)]).unwrap();
        for broken in [
            json.replace("\"focus\":2", "\"focus\":7"),
            json.replace("\"previous\":1", "\"previous\":7"),
            json.replace("\"active\":0", "\"active\":1"),
            json.replace("\"ratio\":0.5", "\"ratio\":1.5"),
        ] {
            assert_ne!(broken, json);
            write(&path, &broken).unwrap();
            assert_eq!(
                read(&path).unwrap_err(),
                "a window's tabs do not hold together",
                "{broken}"
            );
        }
        let one = serde_json::to_string(&[window(Tabs::new(1))]).unwrap();
        for frame in [
            one.replace("\"width\":900.0", "\"width\":0.0"),
            one.replace("\"height\":600.0", "\"height\":99.0"),
            one.replace("\"x\":40.0", "\"x\":1e39"),
        ] {
            assert_ne!(frame, one);
            write(&path, &frame).unwrap();
            let err = read(&path).unwrap_err();
            assert_eq!(err, "a window's frame is too small or not a number", "{frame}");
        }
        write(&path, "[{\"frame\":").unwrap();
        assert!(read(&path).unwrap_err().contains("EOF"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_write_that_fails_leaves_nothing_beside_the_layout() {
        let dir = std::env::temp_dir().join(format!("rtty-unwritten-{}", std::process::id()));
        // A directory where the file goes: the rename over it fails.
        let path = dir.join("layout.json");
        fs::create_dir_all(&path).unwrap();
        assert!(write(&path, "[]").is_err());
        let names: Vec<_> =
            fs::read_dir(&dir).unwrap().flatten().map(|entry| entry.path()).collect();
        assert_eq!(names, [path]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn one_rtty_at_a_time_takes_the_layout() {
        let dir = std::env::temp_dir().join(format!("rtty-taken-{}", std::process::id()));
        let path = dir.join("rtty/layout.json");
        assert!(take(&path).unwrap(), "the first takes it");
        assert!(!take(&path).unwrap(), "the lock is held, by this test's first take");
        assert!(dir.join("rtty/layout.lock").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
