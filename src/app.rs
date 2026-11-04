//! rtty as a macOS app: its menu bar, whose items run `[bind]`'s actions and show their chords.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use gpui::{App, Menu, MenuItem, actions};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSEventModifierFlags as Flags, NSMenu};
use objc2_foundation::NSString;

use crate::config::Action;

actions!(rtty, [About, Hide, HideOthers, ShowAll]);

/// A menu item's title and the `[bind]` action it runs; none for a line between items.
type Item = (&'static str, Option<Action>);

const LINE: Item = ("", None);

/// The menu bar: each menu's title and items.
const MENUS: &[(&str, &[Item])] = &[
    (
        "rtty",
        &[
            ("About rtty", Some(Action::About)),
            LINE,
            ("Hide rtty", Some(Action::Hide)),
            ("Hide Others", Some(Action::HideOthers)),
            ("Show All", Some(Action::ShowAll)),
            LINE,
            ("Quit rtty", Some(Action::Quit)),
        ],
    ),
    (
        "Shell",
        &[
            ("New Window", Some(Action::NewWindow)),
            ("New Tab", Some(Action::NewTab)),
            LINE,
            ("Split Right", Some(Action::SplitRight)),
            ("Split Down", Some(Action::SplitDown)),
            LINE,
            ("Close Pane", Some(Action::ClosePane)),
            ("Close Window", Some(Action::CloseWindow)),
            ("Undo Close", Some(Action::UndoClose)),
        ],
    ),
    (
        "Edit",
        &[
            ("Copy", Some(Action::Copy)),
            ("Paste", Some(Action::Paste)),
            LINE,
            ("Find", Some(Action::Find)),
            LINE,
            ("Copy Last Output", Some(Action::CopyLastOutput)),
            ("Clear Scrollback", Some(Action::ClearScrollback)),
        ],
    ),
    (
        "View",
        &[
            ("Bigger", Some(Action::FontBigger)),
            ("Smaller", Some(Action::FontSmaller)),
            ("Actual Size", Some(Action::FontReset)),
            LINE,
            ("Zoom Pane", Some(Action::Zoom)),
        ],
    ),
    (
        "Window",
        &[
            ("Minimize", Some(Action::Minimize)),
            ("Zoom", Some(Action::ZoomWindow)),
            LINE,
            ("Next Tab", Some(Action::NextTab)),
            ("Previous Tab", Some(Action::PrevTab)),
        ],
    ),
];

/// The menu bar, and what its app-wide items do.
pub fn init(bindings: &BTreeMap<String, Action>, cx: &mut App) {
    cx.on_action(|_: &About, _| {
        NSApplication::sharedApplication(main_thread()).orderFrontStandardAboutPanel(None)
    });
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    menus(bindings, cx);
}

/// Sets the menu bar (`bar`).
pub fn menus(bindings: &BTreeMap<String, Action>, cx: &App) {
    cx.set_menus(bar(bindings));
    // GPUI 0.2.2 hands AppKit Enter and Tab by name, which AppKit shows as a word and never
    // matches.
    for item in native_menus().flat_map(|menu| menu.itemArray()) {
        let key = match item.keyEquivalent().to_string().as_str() {
            "enter" => "\r",
            "tab" => "\t",
            _ => continue,
        };
        item.setKeyEquivalent(&NSString::from_str(key));
    }
}

/// The menu bar. An item runs its action as the action's chord does, and macOS shows the chord
/// `bindings` gives it, if any.
fn bar(bindings: &BTreeMap<String, Action>) -> Vec<Menu> {
    let item = |&(name, action): &Item| {
        let Some(action) = action else { return MenuItem::separator() };
        let bound = bindings.iter().find(|(_, bound)| **bound == action);
        let chord = bound.map_or("", |(chord, _)| chord.as_str());
        let (action, _) = crate::gpui_action(action, chord);
        MenuItem::Action { name: name.into(), action, os_action: None }
    };
    let menu = |&(name, items): &(&'static str, &[Item])| Menu {
        name: name.into(),
        items: items.iter().map(item).collect(),
    };
    MENUS.iter().map(menu).collect()
}

/// The menu bar's menus, as AppKit has them.
fn native_menus() -> impl Iterator<Item = Retained<NSMenu>> {
    let bar = NSApplication::sharedApplication(main_thread()).mainMenu();
    bar.into_iter().flat_map(|bar| bar.itemArray()).filter_map(|item| item.submenu())
}

/// The menu bar as AppKit has it, a line per menu: each item's title and the chord macOS shows
/// for it (not the hidden and alternate items AppKit adds), for `--script`'s dump.
pub fn dump() -> String {
    let mut out = String::new();
    for menu in native_menus() {
        let items: Vec<String> = menu
            .itemArray()
            .iter()
            .filter(|item| !item.isHidden() && !item.isAlternate())
            .map(|item| {
                let key = item.keyEquivalent().to_string();
                let flags = item.keyEquivalentModifierMask();
                let marks = [
                    (Flags::Control, '⌃'),
                    (Flags::Option, '⌥'),
                    (Flags::Shift, '⇧'),
                    (Flags::Command, '⌘'),
                ];
                let held = marks.into_iter().filter(|(flag, _)| flags.contains(*flag));
                let held: String = held.map(|(_, mark)| mark).collect();
                let chord = match key.as_str() {
                    "" => String::new(),
                    "\r" => format!(" {held}↩"),
                    key => format!(" {held}{}", key.to_uppercase()),
                };
                match item.isSeparatorItem() {
                    true => "-".to_owned(),
                    false => format!("{}{chord}", item.title()),
                }
            })
            .collect();
        let _ = writeln!(out, "menu {}: {}", menu.title(), items.join(" | "));
    }
    out
}

fn main_thread() -> MainThreadMarker {
    MainThreadMarker::new().expect("AppKit is called on the main thread")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::window::ClosePane;

    /// The action the menu bar's item `title` in `menu` runs, with `[bind]` from `source`.
    fn action(source: &str, menu: &str, title: &str) -> Box<dyn gpui::Action> {
        let bindings = Config::parse(source).unwrap().bindings;
        let menus = bar(&bindings).into_iter().filter(|bar| bar.name.as_ref() == menu);
        let items = menus.flat_map(|menu| menu.items);
        let found = items.into_iter().find_map(|item| match item {
            MenuItem::Action { name, action, .. } if name.as_ref() == title => Some(action),
            _ => None,
        });
        found.unwrap_or_else(|| panic!("no {title} in {menu}"))
    }

    #[test]
    fn an_item_runs_its_action_with_the_chord_bind_gives_it() {
        let close = |keystroke: &str| ClosePane { keystroke: keystroke.into() };
        assert!(action("", "Shell", "Close Pane").partial_eq(&close("cmd-w")));
        let rebound = "[bind]\n'cmd-w' = 'none'\n'alt-x' = 'close-pane'";
        assert!(action(rebound, "Shell", "Close Pane").partial_eq(&close("alt-x")));
        assert!(action("[bind]\n'cmd-w' = 'none'", "Shell", "Close Pane").partial_eq(&close("")));
        assert!(action("", "rtty", "Quit rtty").partial_eq(&crate::Quit));
        assert!(action("", "rtty", "Hide Others").partial_eq(&HideOthers));
        assert!(action("", "View", "Zoom Pane").partial_eq(&crate::window::ToggleZoom));
    }
}
