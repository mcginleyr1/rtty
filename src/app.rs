//! rtty as a macOS app: its menu bar, whose items run `[bind]`'s actions and show their chords,
//! the question before quitting however macOS asks rtty to quit, Secure Keyboard Entry, and the
//! global hotkey.

use std::cell::{Cell, OnceCell};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::mem;
use std::ptr::{self, NonNull};

use block2::RcBlock;
use futures::channel::oneshot;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use gpui::{App, AsyncApp, Global, Menu, MenuItem, WindowHandle, WindowId, actions};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{MainThreadMarker, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationDidBecomeActiveNotification, NSApplicationTerminateReply as Reply,
    NSApplicationWillResignActiveNotification, NSControlStateValueOff, NSControlStateValueOn,
    NSEventModifierFlags as Flags, NSMenu,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSString};
use portable_pty::CommandBuilder;

use crate::config::Action;
use crate::view::Settings;
use crate::window::TerminalWindow;

actions!(rtty, [About, Hide, HideOthers, ShowAll, ToggleSecureInput]);

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn EnableSecureEventInput() -> i32;
    fn DisableSecureEventInput() -> i32;
    fn IsSecureEventInputEnabled() -> u8;
}

/// rtty as the app: whether it is the active one, the window it gave the keyboard last, Secure
/// Keyboard Entry, and the global hotkey.
struct AppState {
    /// Under `--script`, the hotkey tells the dump what it would do, and does nothing.
    scripted: bool,
    /// rtty is the active app (NSApplication's notifications; `--script`'s `app` step).
    active: bool,
    key: Option<WindowHandle<TerminalWindow>>,
    /// `secure-input`'s toggle, which `secure_input` sets at start.
    secure_input: bool,
    /// The focused pane of the window with the keyboard reads a password (`auto_secure_input`).
    password: bool,
    /// `EnableSecureEventInput` called, and not yet undone.
    held: bool,
    /// The enable and disable calls made: as many of each, or one more enable while held.
    calls: (u32, u32),
    /// `global_hotkey`, registered with macOS for as long as the manager lives.
    hotkey: Option<(HotKey, GlobalHotKeyManager)>,
    /// Under `--script`, what the hotkey would have done.
    toggled: Vec<String>,
}

impl Global for AppState {}

const SECURE_INPUT: &str = "Secure Keyboard Entry";

thread_local! {
    /// The app, for what AppKit calls outside GPUI.
    static APP: OnceCell<AsyncApp> = const { OnceCell::new() };
    /// rtty decided to quit: `applicationShouldTerminate:` lets it go without asking.
    static QUITTING: Cell<bool> = const { Cell::new(false) };
    /// AppKit asked whether rtty may quit, and waits for the answer.
    static WAITING: Cell<bool> = const { Cell::new(false) };
    /// The window the quit question is up on, and the question's number, which tells its answer
    /// from one to a question asked since (`asking`).
    static ASKING: Cell<Option<(WindowId, u64)>> = const { Cell::new(None) };
    static QUESTIONS: Cell<u64> = const { Cell::new(0) };
}

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
            (SECURE_INPUT, Some(Action::SecureInput)),
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

/// The menu bar and what its app-wide items do; Secure Keyboard Entry, whose toggle
/// `secure_input` turns on from the start; and the global hotkey, which under `--script` only
/// says what it would do.
pub fn init(cx: &mut App) {
    APP.with(|app| app.set(cx.to_async()).ok().expect("rtty starts once"));
    let settings = cx.global::<Settings>();
    let (bindings, secure_input) = (settings.bindings.clone(), settings.secure_input);
    let (hotkey, scripted) = (settings.hotkey, !settings.launch.prompts);
    let hotkey = hotkey.and_then(|hotkey| {
        let manager = GlobalHotKeyManager::new();
        match manager.and_then(|manager| manager.register(hotkey).map(|()| manager)) {
            Ok(manager) => Some((hotkey, manager)),
            Err(err) => {
                eprintln!("rtty: global hotkey {hotkey}: {err}");
                crate::note(&format!("global hotkey {hotkey}: {err}"));
                None
            }
        }
    });
    if hotkey.is_some() {
        GlobalHotKeyEvent::set_event_handler(Some(|event: GlobalHotKeyEvent| {
            if event.state == HotKeyState::Pressed {
                later(hotkey_pressed)
            }
        }));
    }
    cx.set_global(AppState {
        scripted,
        active: false,
        key: None,
        secure_input,
        password: false,
        held: false,
        calls: (0, 0),
        hotkey,
        toggled: Vec::new(),
    });
    cx.on_action(|_: &About, _| {
        NSApplication::sharedApplication(main_thread()).orderFrontStandardAboutPanel(None)
    });
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &ToggleSecureInput, cx| {
        let state = cx.global_mut::<AppState>();
        state.secure_input = !state.secure_input;
        let on = state.secure_input;
        crate::note(&format!("secure keyboard entry turned {}", if on { "on" } else { "off" }));
        check_secure_input(on);
        refresh_secure_input(cx);
    });
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: AppKit's names for its notifications, there for the process's life.
    let changes = unsafe {
        [
            (NSApplicationDidBecomeActiveNotification, true),
            (NSApplicationWillResignActiveNotification, false),
        ]
    };
    for (name, active) in changes {
        let changed =
            RcBlock::new(move |_: NonNull<NSNotification>| later(move |cx| set_active(active, cx)));
        // SAFETY: AppKit posts NSApplication's notifications on the main thread, where the block
        // runs; the observer stays for rtty's life.
        let observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &changed)
        };
        mem::forget(observer);
    }
    cx.on_app_quit(|cx| {
        cx.global_mut::<AppState>().active = false;
        refresh_secure_input(cx);
        async {}
    })
    .detach();
    menus(&bindings, cx);
}

/// Runs `f` with the app on a later turn of the run loop, out of whatever AppKit, and GPUI
/// calling into it, are doing now.
fn later(f: impl FnOnce(&mut App) + 'static) {
    APP.with(|app| {
        let app = app.get().expect("rtty runs").clone();
        let executor = app.foreground_executor().clone();
        executor.spawn(async move { app.update(f).ok() }).detach()
    })
}

/// rtty became the active app, or stopped being it.
pub fn set_active(active: bool, cx: &mut App) {
    cx.global_mut::<AppState>().active = active;
    cx.defer(refresh_secure_input);
}

/// `window` took the keyboard.
pub fn key_window(window: WindowHandle<TerminalWindow>, cx: &mut App) {
    cx.global_mut::<AppState>().key = Some(window);
    cx.defer(refresh_secure_input);
}

/// The window that had the keyboard last.
pub fn key(cx: &App) -> Option<WindowHandle<TerminalWindow>> {
    cx.global::<AppState>().key
}

/// The global hotkey: hides rtty while it is the active app; else brings it forward at the
/// window that had the keyboard last, else the first, else a new one.
pub fn hotkey_pressed(cx: &mut App) {
    let state = cx.global::<AppState>();
    let (active, key, scripted) = (state.active, state.key, state.scripted);
    let hidden = crate::window::hidden_window(cx);
    let windows = cx.windows().into_iter().filter(|window| Some(window.window_id()) != hidden);
    let windows: Vec<WindowHandle<TerminalWindow>> =
        windows.filter_map(|window| window.downcast()).collect();
    let front = key.filter(|key| windows.contains(key)).or(windows.first().copied());
    let did = match (active, front) {
        (true, _) => "hid rtty".to_owned(),
        (false, Some(window)) => format!("brought window {} forward", number(window, cx)),
        (false, None) => "opened a window".to_owned(),
    };
    crate::note(&format!("global hotkey: {did}"));
    if scripted {
        return cx.global_mut::<AppState>().toggled.push(did);
    }
    match (active, front) {
        (true, _) => cx.hide(),
        (false, Some(window)) => {
            cx.activate(true);
            window.update(cx, |_, window, _| window.activate_window()).ok();
        }
        (false, None) => {
            cx.activate(true);
            if let Err(err) = crate::window::open(CommandBuilder::new_default_prog(), cx) {
                eprintln!("rtty: {err}");
            }
        }
    }
}

/// `window`'s number, from 1, in the order rtty's windows opened.
pub fn number(window: WindowHandle<TerminalWindow>, cx: &App) -> usize {
    let windows =
        cx.windows().into_iter().position(|other| other.window_id() == window.window_id());
    windows.map_or(0, |index| index + 1)
}

/// Holds Secure Keyboard Entry while rtty is the active app and its toggle is on or the focused
/// pane of the window with the keyboard reads a password (`Pane::reads_password`), and lets it go
/// otherwise. macOS counts the calls, and `held` keeps one `DisableSecureEventInput` for each
/// `EnableSecureEventInput`.
pub fn refresh_secure_input(cx: &mut App) {
    let state = cx.global::<AppState>();
    let auto = cx.global::<Settings>().auto_secure_input;
    let key = state.key.filter(|_| state.active && auto);
    let pane = key.and_then(|key| key.read(cx).ok()?.focused());
    let password = pane.is_some_and(|pane| pane.read(cx).reads_password());
    let state = cx.global_mut::<AppState>();
    state.password = password;
    let hold = state.active && (state.secure_input || password);
    if hold == state.held {
        return;
    }
    state.held = hold;
    // SAFETY: Carbon calls taking nothing; `held` pairs them.
    let status = if hold {
        state.calls.0 += 1;
        unsafe { EnableSecureEventInput() }
    } else {
        state.calls.1 += 1;
        unsafe { DisableSecureEventInput() }
    };
    let why = if password { "a password prompt" } else { "the toggle" };
    let what = if hold { format!("on for {why}") } else { "off".into() };
    crate::note(&format!("secure keyboard entry {what} (status {status})"));
    // The lock in every window's bar.
    cx.refresh_windows();
}

/// Secure Keyboard Entry is on, and the bars show a lock.
pub fn secure_input_held(cx: &App) -> bool {
    cx.global::<AppState>().held
}

/// The App menu's Secure Keyboard Entry, checked while its toggle is on.
fn check_secure_input(on: bool) {
    let title = NSString::from_str(SECURE_INPUT);
    for item in native_menus().filter_map(|menu| menu.itemWithTitle(&title)) {
        item.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
    }
}

/// Has macOS ask rtty before it quits it (the dock's Quit, logging out, a quit Apple event), by
/// giving GPUI's application delegate the `applicationShouldTerminate:` it lacks; before the app
/// runs, so AppKit sees it on its delegate from the start.
pub fn ask_before_terminating() {
    let delegate = AnyClass::get(c"GPUIApplicationDelegate").expect("GPUI's application delegate");
    let method: extern "C" fn(&AnyObject, Sel, *mut AnyObject) -> Reply = should_terminate;
    // SAFETY: `method` takes and returns what `applicationShouldTerminate:` does (`Q@:@`), and
    // the class is GPUI's live delegate class, which adding a method to leaves as it was. Plain
    // `extern "C"`, a panic in it aborts (after rtty's hook logs it) instead of unwinding
    // through AppKit.
    let added = unsafe {
        objc2::ffi::class_addMethod(
            ptr::from_ref(delegate).cast_mut(),
            sel!(applicationShouldTerminate:),
            mem::transmute::<extern "C" fn(&AnyObject, Sel, *mut AnyObject) -> Reply, Imp>(method),
            c"Q@:@".as_ptr(),
        )
    };
    assert!(added.as_bool(), "GPUI's delegate answers applicationShouldTerminate: itself");
}

/// `applicationShouldTerminate:`. A quit rtty decided on goes on; any other asks first, as
/// `Cmd+Q` does (`window::quit`), and AppKit waits for the answer (`asking`), which every way
/// the question can end gives it (`quit`, `stay`). Cancelled when the question was due and no
/// window could take it; with GPUI gone, nothing is left to ask about.
extern "C" fn should_terminate(_: &AnyObject, _: Sel, _: *mut AnyObject) -> Reply {
    if !QUITTING.get() {
        crate::note("macOS asked rtty to quit: the dock's Quit, logging out, or a quit event");
        let asked = APP.with(|app| app.get()?.update(crate::window::quit).ok());
        if asked.is_none() {
            return Reply::TerminateNow;
        }
    }
    if QUITTING.get() {
        return Reply::TerminateNow;
    }
    if ASKING.get().is_none() {
        return Reply::TerminateCancel;
    }
    WAITING.set(true);
    Reply::TerminateLater
}

/// The quit question is up on `window`. Its answer quits or stays. A question that goes
/// unanswered, closed with its window (`window_closed`) or torn down, is asked again
/// (`window::quit`), on another window, about what still runs; an answer to a question asked
/// again since counts for nothing.
pub fn asking(window: WindowId, answer: oneshot::Receiver<usize>, cx: &mut App) {
    let question = QUESTIONS.get() + 1;
    QUESTIONS.set(question);
    ASKING.set(Some((window, question)));
    cx.spawn(async move |cx| {
        let answer = answer.await;
        if ASKING.get().is_none_or(|(_, asked)| asked != question) {
            return;
        }
        ASKING.set(None);
        let answered = cx.update(|cx| match answer {
            Ok(0) => quit("as asked", cx),
            Ok(_) => {
                crate::note("quitting cancelled");
                stay()
            }
            Err(_) => ask_again("the question went unanswered", cx),
        });
        // GPUI gone: nothing is left to ask about.
        if answered.is_err() && WAITING.take() {
            reply(true)
        }
    })
    .detach();
}

/// A window closed. When the quit question was up on it, quitting asks again.
pub fn window_closed(cx: &mut App) {
    let Some((window, _)) = ASKING.get() else { return };
    if cx.windows().iter().all(|other| other.window_id() != window) {
        ASKING.set(None);
        ask_again("the window asking closed", cx);
    }
}

/// Asks the quit question again, about what runs now: quits when nothing does, and when no
/// window can take the question, AppKit, waiting, hears No.
fn ask_again(why: &str, cx: &mut App) {
    crate::note(&format!("asking whether to quit again: {why}"));
    crate::window::quit(cx);
    if ASKING.get().is_none() && !QUITTING.get() {
        stay()
    }
}

/// Quits: tells AppKit to go on when it waits on the question (`should_terminate`), else asks
/// it to terminate, which then asks nothing.
pub fn quit(why: &str, cx: &mut App) {
    crate::note(&format!("quitting: {why}"));
    QUITTING.set(true);
    ASKING.set(None);
    if WAITING.take() { reply(true) } else { cx.quit() }
}

/// The question quitting asked was cancelled: AppKit, when it waits on it, hears so.
pub fn stay() {
    if WAITING.take() {
        reply(false)
    }
}

/// From a later turn of the run loop: AppKit, terminating, calls GPUI back.
fn reply(go: bool) {
    APP.with(|app| {
        let app = app.get().expect("rtty runs");
        let terminate = async move {
            NSApplication::sharedApplication(main_thread()).replyToApplicationShouldTerminate(go)
        };
        app.foreground_executor().spawn(terminate).detach()
    })
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
    check_secure_input(cx.global::<AppState>().secure_input);
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

/// For `--script`'s dump: the menu bar as AppKit has it, a line per menu, each item's title and
/// the chord macOS shows for it (not the hidden and alternate items AppKit adds), checked ones
/// marked; then rtty as the app and Secure Keyboard Entry, with the calls made and whether
/// macOS has it on, for any process.
pub fn dump(cx: &App) -> String {
    let mut out = String::new();
    let state = cx.global::<AppState>();
    let key = state.key.map(|key| number(key, cx));
    let _ = writeln!(out, "app active {} key window {key:?}", state.active);
    let hotkey = state.hotkey.as_ref().map(|(hotkey, _)| hotkey.to_string());
    let _ = writeln!(out, "global hotkey {hotkey:?} registered, would have {:?}", state.toggled);
    // SAFETY: a Carbon call taking nothing.
    let system = unsafe { IsSecureEventInputEnabled() } != 0;
    let _ = writeln!(
        out,
        "secure input toggle {} auto {} password {} held {} enable calls {} disable calls {}, \
         on in macOS {system}",
        state.secure_input,
        cx.global::<Settings>().auto_secure_input,
        state.password,
        state.held,
        state.calls.0,
        state.calls.1
    );
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
                let check = if item.state() == NSControlStateValueOn { "✓ " } else { "" };
                match item.isSeparatorItem() {
                    true => "-".to_owned(),
                    false => format!("{check}{}{chord}", item.title()),
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
    fn gpuis_delegate_answers_whether_rtty_may_quit() {
        ask_before_terminating();
        let delegate = AnyClass::get(c"GPUIApplicationDelegate").unwrap();
        assert!(delegate.instance_method(sel!(applicationShouldTerminate:)).is_some());
        // With no app (GPUI gone), nothing is left to ask about, and AppKit waits on nothing; a
        // quit rtty decided on goes on at once too.
        let object = objc2_foundation::NSObject::new();
        let ask = || should_terminate(&object, sel!(applicationShouldTerminate:), ptr::null_mut());
        assert_eq!(ask(), Reply::TerminateNow);
        assert!(!WAITING.get());
        QUITTING.set(true);
        assert_eq!(ask(), Reply::TerminateNow);
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
