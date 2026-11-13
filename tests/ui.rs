//! rtty's GUI layer, checked through real windows: each test hands `--script` a script in a
//! scratch environment of its own and asserts on what the script's `dump` steps wrote.
//! `tests/ui/README.md` says how to run them, what they cover and how to add one.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How long one rtty may run. GPUI draws no frame while the display sleeps, and a script waits
/// for frames, so a stalled run is ended rather than waited for.
const TIMEOUT: Duration = Duration::from_secs(30);

/// A new directory for `test`, short enough for rtty's socket under it, with an rtty/ for the
/// config and the saved layout.
fn scratch(test: &str) -> PathBuf {
    let dir = PathBuf::from(format!("/private/tmp/rtty-ui-{}-{test}", std::process::id()));
    fs::create_dir_all(dir.join("rtty")).unwrap();
    dir
}

/// Removes a scratch directory, trying again while the shells rtty hung up as it quit still
/// write their history there.
fn remove(dir: &Path) {
    let started = Instant::now();
    while let Err(err) = fs::remove_dir_all(dir) {
        assert!(started.elapsed() < Duration::from_secs(5), "{}: {err}", dir.display());
        thread::sleep(Duration::from_millis(50));
    }
}

/// Runs `script` in a real rtty window that knows no directory but `dir`: its home, config,
/// state and TMPDIR (so its socket and log) are there, and so are those of its shell, zsh with
/// the prompt `ready> `. A line that is only `dump` writes a file of its own; what they wrote
/// comes back in order (empty for a dump never reached) with rtty's exit status, and is printed,
/// with what rtty said on stderr, for a failing test to show.
fn launch(dir: &Path, args: &[&str], script: &str) -> (ExitStatus, Vec<String>) {
    let dump = |n: usize| dir.join(format!("dump-{n}"));
    let mut dumps = 0;
    let steps = script.lines().map(|line| match line.trim() {
        "dump" => {
            dumps += 1;
            format!("dump {}\n", dump(dumps).display())
        }
        _ => format!("{line}\n"),
    });
    fs::write(dir.join("script"), steps.collect::<String>()).unwrap();
    fs::write(dir.join(".zshrc"), "PS1='ready> '\n").unwrap();
    let mut rtty = Command::new(env!("CARGO_BIN_EXE_rtty"));
    rtty.args(args).arg("--script").arg(dir.join("script")).current_dir(dir);
    rtty.env_clear().env("SHELL", "/bin/zsh").env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    for name in ["HOME", "ZDOTDIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "TMPDIR"] {
        rtty.env(name, dir);
    }
    rtty.stdin(Stdio::null()).stderr(File::create(dir.join("stderr")).unwrap());
    let mut rtty = rtty.spawn().unwrap();
    let started = Instant::now();
    let status = loop {
        match rtty.try_wait().unwrap() {
            Some(status) => break status,
            None if started.elapsed() < TIMEOUT => thread::sleep(Duration::from_millis(50)),
            None => {
                rtty.kill().unwrap();
                rtty.wait().unwrap();
                remove(dir);
                panic!(
                    "rtty still ran after {TIMEOUT:?} and was killed. Is the display awake? \
                     `pmset -g assertions` shows UserIsActive 1 when it is"
                );
            }
        }
    };
    println!("rtty: {status}\n{}", fs::read_to_string(dir.join("stderr")).unwrap());
    let dumps = (1..=dumps).map(|n| fs::read_to_string(dump(n)).unwrap_or_default());
    let dumps: Vec<String> = dumps.collect();
    for (index, dump) in dumps.iter().enumerate() {
        println!("--- dump {index}\n{dump}");
    }
    (status, dumps)
}

/// `launch` in a scratch directory of `test`'s own, with `config` as its config.toml, removed
/// afterwards.
fn run(test: &str, config: &str, script: &str) -> (ExitStatus, Vec<String>) {
    let dir = scratch(test);
    fs::write(dir.join("rtty/config.toml"), config).unwrap();
    let ran = launch(&dir, &[], script);
    remove(&dir);
    ran
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn tabs_open_switch_and_close_by_chord_and_on_the_bar() {
    let (status, dumps) = run(
        "tabs",
        "",
        "wait ready>
         resize 24 80
         sleep 300
         dump
         keys cmd-t
         wait ready>
         dump
         keys cmd-1
         dump
         keys cmd-}
         dump
         keys cmd-}
         dump
         keys cmd-{
         dump
         bar hover tab 1
         dump
         bar click tab 1
         sleep 300
         dump
         bar click new
         wait ready>
         dump
         bar click close 3
         sleep 300
         dump
         keys cmd-z
         dump
         keys cmd-w
         sleep 300
         dump
         quit",
    );
    let [lone, two, first, next, round, previous, hovered, clicked, plus, closed, undone, last] =
        &dumps[..]
    else {
        panic!("{} dumps", dumps.len())
    };
    assert!(status.success());
    assert!(lone.contains("window 24x80 tab 1 of 1 focus 1 zoomed false title \"~\""));
    assert!(lone.contains("bar title Some(\"~\") folder true"));
    assert!(!lone.contains("bar tab") && !lone.contains("bar new"));
    assert!(two.contains("window 24x80 tab 2 of 2 focus 2"));
    assert!(two.contains("windows 1 tabs [\"1: ~\", \"2: ~\"]"));
    assert!(
        two.contains("bar tab 1 at") && two.contains("bar tab 2 at") && two.contains("bar new at")
    );
    assert!(two.contains("\npane 2 at 0,0 24x80\n\n"));
    assert!(first.contains("tab 1 of 2 focus 1"));
    assert!(next.contains("tab 2 of 2 focus 2"));
    assert!(round.contains("tab 1 of 2 focus 1"));
    assert!(previous.contains("tab 2 of 2 focus 2"));
    assert!(hovered.contains("hover Some(Tab(0))"));
    assert!(
        hovered.lines().any(|line| line.starts_with("bar tab 1") && line.contains("close shown"))
    );
    assert!(hovered.contains("tab 2 of 2 focus 2"));
    assert!(clicked.contains("tab 1 of 2 focus 1"));
    assert!(plus.contains("tab 2 of 3 focus 3"));
    assert!(closed.contains("tab 2 of 2 focus 3"));
    assert!(closed.contains("undo-close Tab(2,") && closed.contains("panes [2] for"));
    assert!(undone.contains("tab 3 of 3 focus 2") && !undone.contains("undo-close"));
    assert!(last.contains("tab 2 of 2 focus 3"));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn closing_a_pane_or_a_tab_that_runs_a_program_asks_first() {
    let (status, dumps) = run(
        "closing",
        "",
        "wait ready>
         resize 24 80
         sleep 300
         keys cmd-d
         wait ready>
         keys cmd-w
         sleep 300
         dump
         keys cmd-z
         dump
         type cat -n
         keys enter
         type go
         keys enter
         wait 1  go
         keys cmd-w
         dump
         answer cancel
         dump
         keys cmd-w
         answer ok
         sleep 300
         dump
         keys cmd-t
         wait ready>
         type cat -n
         keys enter
         type go
         keys enter
         wait 1  go
         bar click close 2
         sleep 300
         dump
         answer ok
         sleep 300
         dump
         quit",
    );
    let [idle, undone, asked, kept, closed, tab_asked, tab_closed] = &dumps[..] else {
        panic!("{} dumps", dumps.len())
    };
    assert!(status.success());
    assert!(idle.contains("\npane 1 at 0,0 24x80\n\n") && !idle.contains("attention:"));
    assert!(idle.contains("undo-close Pane(2,"));
    assert!(undone.contains("\npane 1 at 0,0 24x40\npane 2 at 0,41 24x39\n\n"));
    assert!(undone.contains("focus 2") && !undone.contains("undo-close"));
    let pane =
        "attention: asked \"Close this pane?\" \"It is still running cat -n.\" [Close, Cancel]\n";
    assert!(asked.contains(pane));
    assert!(asked.contains("\npane 1 at 0,0 24x40\npane 2 at 0,41 24x39\n\n"));
    assert!(kept.contains("\npane 1 at 0,0 24x40\npane 2 at 0,41 24x39\n\n"));
    assert!(closed.contains("\npane 1 at 0,0 24x80\n\n"));
    assert_eq!(closed.matches("asked \"Close this pane?\"").count(), 2);
    let tab = "attention: asked \"Close this tab?\" \"1 pane in it is still running a program: cat -n.\" [Close, Cancel]\n";
    assert!(tab_asked.contains(tab));
    assert!(tab_asked.contains("tab 2 of 2 focus 3"));
    assert!(tab_closed.contains("tab 1 of 1 focus 1"));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn quitting_asks_while_a_program_runs_and_quits_on_its_ok() {
    let (status, dumps) = run(
        "quitting",
        "",
        "wait ready>
         type cat -n
         keys enter
         type go
         keys enter
         wait 1  go
         terminate
         sleep 300
         dump
         answer cancel
         sleep 300
         dump
         keys cmd-q
         dump
         answer ok
         sleep 2000
         dump",
    );
    let [asked, kept, again, after] = &dumps[..] else { panic!("{} dumps", dumps.len()) };
    assert!(status.success());
    let quit = "attention: asked \"Quit rtty?\" \"1 pane is still running a program: cat -n.\" [Quit, Cancel]\n";
    assert_eq!(asked.matches(quit).count(), 1);
    assert_eq!(kept.matches(quit).count(), 1);
    assert!(kept.contains("\ncommand Some(\"cat -n\") running "));
    assert_eq!(again.matches(quit).count(), 2);
    assert_eq!(after, "");
    // Over an idle shell nothing is asked.
    let (status, dumps) = run("quit-idle", "", "wait ready>\nterminate\nsleep 2000\ndump");
    assert!(status.success());
    assert_eq!(dumps, [""]);
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn the_last_pane_closing_as_its_shell_exits_quits_rtty() {
    let (status, dumps) = run(
        "exit",
        "",
        "wait ready>
         keys cmd-d
         wait ready>
         type exit
         keys enter
         sleep 1000
         dump
         type exit
         keys enter
         sleep 2000
         dump",
    );
    let [one, after] = &dumps[..] else { panic!("{} dumps", dumps.len()) };
    assert!(status.success());
    assert!(one.contains("focus 1") && !one.contains("\npane 2 at"));
    assert_eq!(after, "");
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn splits_lay_out_focus_resize_zoom_and_close() {
    let (status, dumps) = run(
        "splits",
        "",
        "wait ready>
         resize 24 80
         sleep 300
         keys cmd-d
         wait ready>
         dump
         keys cmd-shift-d
         wait ready>
         dump
         keys cmd-alt-left
         dump
         keys cmd-alt-right
         dump
         keys cmd-alt-up
         dump
         keys cmd-ctrl-left
         dump
         keys cmd-shift-enter
         dump
         keys cmd-shift-enter
         dump
         click 20 5
         sleep 300
         dump
         menu Shell Split Down
         wait ready>
         dump
         keys cmd-w
         sleep 300
         dump
         quit",
    );
    let [right, down, left, back, up, resized, zoomed, unzoomed, clicked, menu, closed] =
        &dumps[..]
    else {
        panic!("{} dumps", dumps.len())
    };
    assert!(status.success());
    assert!(right.contains("window 24x80 tab 1 of 1 focus 2 zoomed false"));
    assert!(right.contains("\npane 1 at 0,0 24x40\npane 2 at 0,41 24x39\n\n"));
    assert!(right.contains("\n== pane 1\n") && right.contains("\n== pane 2\n"));
    let three = "\npane 1 at 0,0 24x40\npane 2 at 0,41 12x39\npane 3 at 13,41 11x39\n\n";
    assert!(down.contains("focus 3 zoomed false") && down.contains(three));
    assert!(left.contains("focus 1 zoomed false") && left.contains(three));
    // Back to the pane focused last of the two beside it.
    assert!(back.contains("focus 3 zoomed false"));
    assert!(up.contains("focus 2 zoomed false"));
    // The divider moves 5% of its split: 4 of 80 columns.
    let moved = "\npane 1 at 0,0 24x36\npane 2 at 0,37 12x43\npane 3 at 13,37 11x43\n\n";
    assert!(resized.contains("focus 2 zoomed false") && resized.contains(moved));
    assert!(zoomed.contains("focus 2 zoomed true") && zoomed.contains("\npane 2 at 0,0 24x80\n\n"));
    assert!(!zoomed.contains("== pane 1"));
    assert!(unzoomed.contains("focus 2 zoomed false") && unzoomed.contains(moved));
    assert!(clicked.contains("focus 1 zoomed false"));
    let four = "\npane 1 at 0,0 12x36\npane 4 at 13,0 11x36\npane 2 at 0,37 12x43\npane 3 at 13,37 11x43\n\n";
    assert!(menu.contains("focus 4 zoomed false") && menu.contains(four));
    assert!(closed.contains("focus 1 zoomed false") && closed.contains(moved));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn keys_reach_the_shell_and_the_bindings_through_gpui_and_through_appkit() {
    let (status, dumps) = run(
        "keys",
        "",
        r"wait ready>
         resize 24 80
         sleep 300
         type printf '%s-%s\n' one two
         keys alt-b
         type X
         keys enter
         wait one-Xtwo
         type printf '%s-%s\n' three four
         nskey alt-b->∫
         type Y
         nskey enter
         wait three-Yfour
         type printf '%s-%s\n' five six
         keys ctrl-a
         type :;
         keys ctrl-e
         type W
         keys enter
         wait five-sixW
         nskey e c h o space k ' ' e y shift-s enter
         wait keyS
         type cat -nv
         keys enter
         type go
         keys enter
         wait 1  go
         keys up shift-tab f1 alt-x
         nskey left
         wait ^[[D
         dump
         keys ctrl-c
         nskey cmd-shift-d
         wait ready>
         dump
         nskey cmd-alt-up
         sleep 200
         dump
         keys cmd-t
         wait ready>
         nskey cmd-1
         sleep 200
         dump
         nskey cmd-shift-]
         sleep 200
         dump
         nskey cmd-=
         sleep 300
         dump
         quit",
    );
    let [typed, split, up, first, next, bigger] = &dumps[..] else {
        panic!("{} dumps", dumps.len())
    };
    assert!(status.success());
    assert!(typed.contains("\nrow 1 \"one-Xtwo\"\n"));
    assert!(typed.contains("\nrow 3 \"three-Yfour\"\n"));
    assert!(typed.contains("\nrow 4 \"ready> :;printf '%s-%s\\\\n' five sixW\"\n"));
    assert!(typed.contains("\nrow 5 \"five-sixW\"\n"));
    assert!(
        typed.contains("\nrow 6 \"ready> echo k''eyS\"\n") && typed.contains("\nrow 7 \"keyS\"\n")
    );
    assert!(typed.contains("\nrow 11 \"^[[A^[[Z^[OP^[x^[[D\"\n"));
    assert!(
        split.contains("focus 2")
            && split.contains("\npane 1 at 0,0 12x80\npane 2 at 13,0 11x80\n\n")
    );
    assert!(up.contains("focus 1"));
    assert!(first.contains("tab 1 of 2 focus 1"));
    assert!(next.contains("tab 2 of 2 focus 3"));
    // A scripted window is never the key one, so the menu bar takes Cmd+= and runs its Bigger,
    // which is the whole app's.
    assert!(next.contains(" size 14 ") && bigger.contains("\nfont \"Menlo\" FontId(0) size 15 "));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn an_option_key_is_alt_or_types_its_character_as_option_as_alt_says() {
    let (status, dumps) = run(
        "option",
        "option_as_alt = \"left\"\n",
        "wait ready>
         type cat -n
         keys enter
         type go
         keys enter
         wait 1  go
         nskey left:alt-h->˙ right:alt-h->˙ alt-h->˙
         wait ˙^[h
         dump
         quit",
    );
    assert!(status.success());
    // The left Option is Alt, the right one types, and a key that names neither is Alt.
    assert!(dumps[0].contains("\nrow 3 \"^[h˙^[h\"\n"));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn the_mouse_selects_cells_words_lines_and_blocks_and_copy_copies_them() {
    let (status, dumps) = run(
        "select",
        "",
        r"wait ready>
         resize 24 80
         sleep 300
         type printf '%s\n' 'alpha beta gamma' 'first  second  third' en''d
         keys enter
         wait end
         drag 1 5 1 9
         dump
         keys cmd-c
         dump
         click 2 8 left 2
         keys cmd-c
         dump
         click 2 8 left 3
         menu Edit Copy
         dump
         drag 1 5 2 12 alt
         keys cmd-c
         dump
         drag 1 5 2 4
         keys cmd-c
         dump
         click 3 1
         sleep 300
         keys cmd-c
         dump
         quit",
    );
    let [cells, copied, word, line, block, rows, cleared] = &dumps[..] else {
        panic!("{} dumps", dumps.len())
    };
    assert!(status.success());
    assert!(cells.contains("row 1 \"alpha beta gamma\"\n  bg 6+4 "));
    assert!(cells.contains("\nselected Cells (1, 6)..=(1, 9) \"beta\"\n"));
    // Selecting copies nothing; under `--script` a copy is the dump's alone.
    assert!(!cells.contains("\ncopied ") && copied.contains("\ncopied \"beta\"\n"));
    assert!(word.contains("\nselected Words (2, 7)..=(2, 12) \"second\"\n"));
    assert!(word.contains("\ncopied \"second\"\n"));
    assert!(line.contains("\nselected Lines (2, 0)..=(2, 79) \"first  second  third\"\n"));
    assert!(line.contains("\ncopied \"first  second  third\"\n"));
    // The same columns of each row, trailing blanks trimmed.
    assert!(block.contains("\nselected Block (1, 6)..=(2, 12) \"beta ga\\n second\"\n"));
    assert!(block.contains("\ncopied \"beta ga\\n second\"\n"));
    assert!(rows.contains("\nselected Cells (1, 6)..=(2, 4) \"beta gamma\\nfirst\"\n"));
    assert!(rows.contains("\ncopied \"beta gamma\\nfirst\"\n"));
    // A click ends the selection, and a copy of nothing copies nothing.
    assert!(
        !cleared.contains("\nselected ") && cleared.contains("\ncopied \"beta gamma\\nfirst\"\n")
    );
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn copy_mode_moves_selects_searches_and_copies() {
    let (status, dumps) = run(
        "copymode",
        "",
        r"wait ready>
         resize 24 80
         sleep 300
         type printf '%s\n' 'alpha beta gamma' 'first  second  third' en''d
         keys enter
         wait end
         keys cmd-shift-c
         dump
         keys k k k 0 w
         dump
         keys v e
         dump
         keys cmd-c
         dump
         keys w y
         dump
         keys cmd-shift-c k k shift-v j
         dump
         keys y
         dump
         keys cmd-shift-c k k k ctrl-v j l l
         dump
         keys y
         dump
         keys cmd-shift-c g g 2 j
         dump
         keys /
         type third
         keys enter
         dump
         keys q
         dump
         quit",
    );
    let [
        entered,
        moved,
        cells,
        bound,
        yanked,
        lines,
        yanked_lines,
        block,
        yanked_block,
        counted,
        found,
        left,
    ] = &dumps[..]
    else {
        panic!("{} dumps", dumps.len())
    };
    assert!(status.success());
    assert!(entered.contains("\ncopy mode at 4,7 pending \"\"\n"));
    assert!(!entered.contains("\ndrawn cursor "));
    assert!(moved.contains("\ncopy mode at 1,6 pending \"\"\n"));
    assert!(cells.contains("\ncopy mode at 1,9 pending \"\"\n"));
    assert!(cells.contains("\nselected Cells (1, 6)..=(1, 9) \"beta\"\n"));
    // A binding still works in copy mode: Cmd+C copies its selection and stays.
    assert!(!cells.contains("\ncopied ") && bound.contains("\ncopied \"beta\"\n"));
    assert!(bound.contains("\ncopy mode at 1,9 pending \"\"\n"));
    assert!(yanked.contains("\ncopied \"beta g\"\n") && yanked.contains("\nstatus \"copied\" "));
    assert!(!yanked.contains("\ncopy mode ") && !yanked.contains("\nselected "));
    assert!(lines.contains("\nselected Lines (2, 0)..=(3, 79) \"first  second  third\\nend\"\n"));
    assert!(yanked_lines.contains("\ncopied \"first  second  third\\nend\"\n"));
    assert!(block.contains("\nselected Block (1, 7)..=(2, 9) \"eta\\nsec\"\n"));
    assert!(yanked_block.contains("\ncopied \"eta\\nsec\"\n"));
    assert!(counted.contains("\ncopy mode at 2,0 pending \"\"\n"));
    assert!(found.contains("\ncopy mode at 2,15 pending \"\"\n"));
    assert!(found.contains("\nsearch \"third\" regex false matches 2 "));
    assert!(!left.contains("\ncopy mode ") && !left.contains("\nsearch "));
    // Nothing copy mode was given reached the shell.
    assert!(left.contains("row 4 \"ready>\"\n"));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn the_wheel_and_the_prompt_jumps_move_the_view_and_cmd_k_clears() {
    let (status, dumps) = run(
        "scroll",
        "",
        "wait ready>
         resize 24 80
         sleep 300
         type seq -f 'line %g' 1 100; echo do''ne
         keys enter
         wait done
         dump
         wheel 5 5 10
         dump
         wheel 5 5 -4
         dump
         type x
         wait ready> x
         dump
         keys backspace cmd-up
         dump
         keys cmd-down
         dump
         drag 10 2 -2 2 300
         dump
         keys cmd-k
         wait ready>
         sleep 300
         dump
         quit",
    );
    let [bottom, up, down, typed, prompt, output, dragged, cleared] = &dumps[..] else {
        panic!("{} dumps", dumps.len())
    };
    assert!(status.success());
    assert!(bottom.contains("\nsize 24x80 top 79 screen_top 79 oldest 0\n"));
    assert!(bottom.contains("\nrow 0 \"line 79\"\n") && bottom.contains("\nrow 23 \"ready>\"\n"));
    assert!(up.contains("\nsize 24x80 top 69 screen_top 79 oldest 0\n"));
    assert!(up.contains("\nrow 0 \"line 69\"\n") && up.contains("\nrow 23 \"line 92\"\n"));
    assert!(down.contains("\nsize 24x80 top 73 screen_top 79 oldest 0\n"));
    assert!(typed.contains("\nsize 24x80 top 79 screen_top 79 oldest 0\n"));
    assert!(prompt.contains("\nsize 24x80 top 0 screen_top 79 oldest 0\n"));
    assert!(prompt.contains("\nrow 0 \"ready> seq -f 'line %g' 1 100; echo do''ne\"\n"));
    assert!(output.contains("\nsize 24x80 top 79 screen_top 79 oldest 0\n"));
    // A selection dragged past the pane's top scrolls the scrollback under it.
    assert!(!dragged.contains(" top 79 screen_top 79 ") && dragged.contains("\nselected Cells "));
    assert!(cleared.contains("\nsize 24x80 top 79 screen_top 79 oldest 79\n"));
    assert!(cleared.contains("\ncursor (0, 7) ") && cleared.contains("\nrow 0 \"ready>\"\n"));
    assert!(!cleared.contains("\nrow 1 "));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn search_finds_matches_in_the_scrollback_and_steps_through_them() {
    let (status, dumps) = run(
        "search",
        "",
        "wait ready>
         resize 24 80
         sleep 300
         type seq -f 'line %g' 1 100; echo do''ne
         keys enter
         wait done
         keys cmd-f
         type LINE 4
         dump
         keys backspace backspace backspace backspace backspace backspace
         type line 4
         dump
         keys enter
         dump
         keys shift-enter
         dump
         keys ctrl-r
         type [0-2]$
         dump
         keys escape
         dump
         quit",
    );
    let [capital, typed, next, previous, regex, closed] = &dumps[..] else {
        panic!("{} dumps", dumps.len())
    };
    assert!(status.success());
    // A capital makes the query case-sensitive.
    let none = "\nsearch \"LINE 4\" regex false matches 0 current None None error None\n";
    assert!(capital.contains(none));
    // The last of line 4 and lines 40 to 49, scrolled into view.
    let last = "\nsearch \"line 4\" regex false matches 11 current Some(10) Some(((49, 0), (49, 5))) error None\n";
    assert!(typed.contains(last) && typed.contains("\nrow 8 \"line 49\"\n  bg 0+6 "));
    assert!(typed.contains(" top 41 screen_top 79 "));
    let first = "\nsearch \"line 4\" regex false matches 11 current Some(0) Some(((4, 0), (4, 5))) error None\n";
    assert!(next.contains(first) && next.contains(" top 0 screen_top 79 "));
    assert!(previous.contains(last));
    assert!(regex.contains("\nsearch \"line 4[0-2]$\" regex true matches 3 "));
    assert!(!closed.contains("\nsearch "));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn the_window_resizes_follows_the_appearance_and_types_only_with_the_focus() {
    let (status, dumps) = run(
        "window",
        "",
        "wait ready>
         resize 24 80
         sleep 300
         appearance dark
         dump
         appearance light
         dump
         resize 30 100
         sleep 300
         dump
         keys cmd-=
         sleep 300
         dump
         keys cmd-0
         sleep 300
         dump
         focus out
         type lost
         sleep 300
         dump
         focus in
         type found
         wait found
         dump
         quit",
    );
    let [dark, light, resized, bigger, reset, unfocused, focused] = &dumps[..] else {
        panic!("{} dumps", dumps.len())
    };
    assert!(status.success());
    assert!(dark.contains("\nscheme \"OneDark Pro\" "));
    assert!(
        dark.contains("\ndefault fg #abb2bf bg #282c34\n")
            && dark.contains("\nbar 36px bg #282c34 ")
    );
    assert!(light.contains("\nscheme \"Solarized Light\" "));
    assert!(
        light.contains("\ndefault fg #657b83 bg #fdf6e3\n")
            && light.contains("\nbar 36px bg #f1ecdb ")
    );
    assert!(light.starts_with("window 24x80 ") && light.contains("\nsize 24x80 "));
    assert!(resized.starts_with("window 30x100 ") && resized.contains("\nsize 30x100 "));
    assert!(resized.contains("\nfont \"Menlo\" FontId(0) size 14 "));
    assert!(bigger.starts_with("window 28x93 ") && bigger.contains("\nsize 28x93 "));
    assert!(bigger.contains("\nfont \"Menlo\" FontId(0) size 15 "));
    assert!(reset.starts_with("window 30x100 ") && reset.contains(" size 14 "));
    assert!(unfocused.contains("\nrow 0 \"ready>\"\n"));
    assert!(focused.contains("\nrow 0 \"ready> found\"\n"));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn a_second_window_opens_closes_and_comes_back() {
    let (status, dumps) = run(
        "windows",
        "",
        "wait ready>
         dump
         keys cmd-n
         sleep 1000
         dump
         window close 2
         sleep 300
         dump
         keys cmd-z
         sleep 300
         dump
         quit",
    );
    let [one, two, closed, undone] = &dumps[..] else { panic!("{} dumps", dumps.len()) };
    assert!(status.success());
    assert!(one.contains("\nwindows 1 tabs [\"1: ~\"] "));
    assert!(two.contains("\nwindows 2 tabs [\"1: ~\"] ") && two.contains(" tab 1 of 1 focus 1 "));
    assert!(closed.contains("\nundo-close Window panes [] for "));
    assert!(undone.contains("\nwindows 2 ") && !undone.contains("\nundo-close "));
    // With no time to undo it in, a closed window is gone.
    let (status, dumps) = run(
        "windows-final",
        "undo_timeout = 0\n",
        "wait ready>
         keys cmd-n
         sleep 1000
         window close 2
         sleep 300
         dump
         quit",
    );
    assert!(status.success());
    assert!(dumps[0].contains("\nwindows 1 ") && !dumps[0].contains("\nundo-close "));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn a_new_window_opens_a_titlebar_below_the_key_one_on_its_display() {
    let dir = scratch("cascade");
    let (status, _) = launch(
        &dir,
        &["--restore"],
        "wait ready>
         app active
         keys cmd-n
         sleep 1000
         quit",
    );
    let layout = fs::read_to_string(dir.join("rtty/layout.json"));
    remove(&dir);
    assert!(status.success());
    let windows: Vec<serde_json::Value> = serde_json::from_str(&layout.unwrap()).unwrap();
    let [key, new] = &windows[..] else { panic!("{} windows", windows.len()) };
    let origin = |window: &serde_json::Value, axis| window["frame"]["origin"][axis].as_f64();
    assert_eq!(origin(new, "x"), origin(key, "x"));
    assert_eq!(origin(new, "y"), origin(key, "y").map(|y| y + 35.));
    assert_eq!(new["frame"]["size"], key["frame"]["size"]);
    assert_eq!(new["display"], key["display"]);
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn the_config_sets_font_scheme_and_bindings_and_a_change_reloads_live() {
    let (status, dumps) = run(
        "config",
        r#"size = 16
scheme = "Solarized Dark"

[bind]
"cmd-d" = "new-tab"
"alt-\\" = "split-right"
"#,
        r#"wait ready>
         dump
         keys cmd-d
         wait ready>
         keys alt-\
         wait ready>
         dump
         type printf 'size = 12\nscheme = "One Light"\n[bind]\n"cmd-d" = "split-down"\n' > ~/rtty/config.toml; echo wr''itten
         keys enter
         wait written
         sleep 2500
         dump
         keys cmd-d
         wait ready>
         dump
         type printf '[bind]\n"cmd-d" = "split-sideways"\n' > ~/rtty/config.toml; echo ag''ain
         keys enter
         wait again
         keys cmd-<
         dump
         quit"#,
    );
    let [launched, bound, reloaded, rebound, mistaken] = &dumps[..] else {
        panic!("{} dumps", dumps.len())
    };
    assert!(status.success());
    assert!(launched.contains("\nfont \"Menlo\" FontId(0) size 16 "));
    assert!(launched.contains("\nscheme \"Solarized Dark\" "));
    assert!(launched.contains("\nmenu Shell: New Window ⌘N | New Tab ⌘D | - | Split Right ⌥\\ | "));
    assert!(
        bound.contains(" tab 2 of 2 focus 3 ")
            && bound.contains("\npane 2 at 0,0 ")
            && bound.contains("\npane 3 at 0,")
    );
    assert!(reloaded.contains("\nsaid \"config reloaded\" "));
    assert!(reloaded.contains("\nfont \"Menlo\" FontId(0) size 12 "));
    assert!(reloaded.contains("\nscheme \"One Light\" "));
    assert!(
        reloaded.contains(
            "\nmenu Shell: New Window ⌘N | New Tab ⌘T | - | Split Right | Split Down ⌘D | "
        )
    );
    assert!(rebound.contains(" tab 2 of 2 focus 4 ") && rebound.contains("\npane 4 at "));
    assert!(
        mistaken
            .contains("\nsaid \"config not reloaded, the last one stays: /private/tmp/rtty-ui-")
    );
    assert!(mistaken.contains(
        "/rtty/config.toml: line 2: \\\"cmd-d\\\": unknown action \\\"split-sideways\\\"\" "
    ));
    assert!(mistaken.contains(" size 12 ") && mistaken.contains("\nscheme \"One Light\" "));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn the_readmes_neovim_like_bindings_split_focus_resize_swap_zoom_and_close() {
    let (status, dumps) = run(
        "bindings",
        r#"keys = "tutti"

[bind]
"alt-x" = "close-pane"
"ctrl-\\" = "last-pane"
"alt-\\" = "split-right"
"alt--" = "split-down"
"alt-m" = "zoom"
"alt-shift-h" = "swap-left"
"#,
        r"wait ready>
         resize 24 80
         sleep 300
         keys alt-\
         wait ready>
         keys alt--
         wait ready>
         dump
         keys ctrl-h
         dump
         keys ctrl-\
         dump
         keys ctrl-k
         dump
         keys alt-h
         dump
         keys alt-shift-h
         dump
         keys alt-m
         dump
         keys alt-m alt-x
         sleep 300
         dump
         quit",
    );
    let [split, left, last, up, resized, swapped, zoomed, closed] = &dumps[..] else {
        panic!("{} dumps", dumps.len())
    };
    assert!(status.success());
    let three = "\npane 1 at 0,0 24x40\npane 2 at 0,41 12x39\npane 3 at 13,41 11x39\n\n";
    assert!(split.contains(" focus 3 zoomed false ") && split.contains(three));
    assert!(left.contains(" focus 1 zoomed false "));
    assert!(last.contains(" focus 3 zoomed false "));
    assert!(up.contains(" focus 2 zoomed false "));
    assert!(
        resized.contains("\npane 1 at 0,0 24x36\npane 2 at 0,37 12x43\npane 3 at 13,37 11x43\n\n")
    );
    // The panes trade places and the focus goes with its pane.
    assert!(swapped.contains(" focus 2 zoomed false "));
    assert!(
        swapped.contains("\npane 2 at 0,0 24x36\npane 1 at 0,37 12x43\npane 3 at 13,37 11x43\n\n")
    );
    assert!(
        zoomed.contains(" focus 2 zoomed true ") && zoomed.contains("\npane 2 at 0,0 24x80\n\n")
    );
    assert!(closed.contains(" focus 1 zoomed false "));
    assert!(closed.contains("\npane 1 at 0,0 12x80\npane 3 at 13,0 11x80\n\n"));
    // None of the chords reached a shell.
    assert_eq!(closed.matches("\nrow ").count(), 2);
    assert_eq!(closed.matches("\nrow 0 \"ready>\"\n").count(), 2);
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn shell_integration_marks_commands_and_copies_the_last_output() {
    let (status, dumps) = run(
        "shell",
        "",
        r"wait ready>
         resize 24 80
         sleep 300
         dump
         keys cmd-shift-a
         dump
         type printf 'hello %s there\n' $((40+2))
         keys enter
         wait hello 42 there
         sleep 300
         keys cmd-shift-a
         dump
         type false
         keys enter
         sleep 500
         keys cmd-shift-a
         dump
         type printf '%s-%s\n' abc def
         wait abc def
         click 1 12
         sleep 300
         dump
         click 3 24
         sleep 300
         dump
         type X
         keys enter
         wait Xabc-def
         type cat -n
         keys enter
         type go
         keys enter
         wait 1  go
         dump
         keys ctrl-c
         type zz
         wait ready> zz
         dump
         quit",
    );
    let [prompt, nothing, copied, silent, off_line, moved, running, interrupted] = &dumps[..]
    else {
        panic!("{} dumps", dumps.len())
    };
    assert!(status.success());
    assert!(prompt.contains("PromptStart 0,0, CommandStart 0,7\n"));
    assert!(
        nothing.contains("\nstatus \"no command output to copy\" ")
            && !nothing.contains("\ncopied ")
    );
    assert!(copied.contains("CommandStart 0,7, OutputStart 1,0, CommandEnd { exit: Some(0) } 2,0, PromptStart 2,0, CommandStart 2,7\n"));
    assert!(copied.contains("\nstatus \"copied the last command's output\" "));
    assert!(copied.contains("\ncopied \"hello 42 there\"\n"));
    assert!(silent.contains(
        "OutputStart 3,0, CommandEnd { exit: Some(1) } 3,0, PromptStart 3,0, CommandStart 3,7\n"
    ));
    assert!(silent.contains("\nstatus \"the last command printed nothing\" "));
    // A click on the output moves nothing; one on the line being typed moves the shell's cursor.
    assert!(off_line.contains("\ncursor (3, 31) "));
    assert!(moved.contains("\ncursor (3, 24) "));
    assert!(running.contains("\nrow 4 \"Xabc-def\"\n"));
    assert!(running.contains("\ncommand Some(\"cat -n\") running "));
    assert!(interrupted.contains(", CommandEnd { exit: Some(130) } "));
    assert!(!interrupted.contains("\ncommand "));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn links_underline_under_cmd_and_quick_select_copies_what_it_labels() {
    let (status, dumps) = run(
        "links",
        "",
        r"wait ready>
         resize 24 80
         sleep 300
         type printf '%s\n' 'see https://example.com/x' '3f2a9c1d0e 98765' en''d
         keys enter
         wait end
         move 1 8 cmd
         dump
         move 1 1 cmd
         dump
         click 1 8 left 1 cmd
         dump
         keys cmd-shift-space
         dump
         type a
         dump
         keys cmd-shift-space
         type F
         wait ready> https
         dump
         keys ctrl-u cmd-shift-space escape
         dump
         keys cmd-shift-o
         dump
         type a
         dump
         quit",
    );
    let [over, off, followed, labeled, picked, pasted, put_away, open, opened] = &dumps[..] else {
        panic!("{} dumps", dumps.len())
    };
    assert!(status.success());
    let link = "Link { target: Uri(\"https://example.com/x\"), start: (1, 4), end: (1, 24) }";
    assert!(over.contains(&format!("\nlink Some(({link}, None)) followed []\n")));
    assert!(
        over.contains("\n  run 4+21 \"https://example.com/x\" ")
            && over.contains(" underline Single ")
    );
    assert!(!off.contains("\nlink ") && !off.contains(" underline "));
    // Under `--script` a followed link is only recorded.
    assert!(followed.contains("\nlink None followed [\"Open https://example.com/x\"]\n"));
    // Home-row labels, the nearest to the cursor first.
    assert!(labeled.contains(
        "\nquick typed \"\": a at 2,11 Copy(\"98765\"), s at 2,0 Copy(\"3f2a9c1d0e\"), d at 1,4 Copy(\"https://example.com/x\"), "
    ));
    assert!(picked.contains("\nstatus \"copied\" ") && picked.contains("\ncopied \"98765\"\n"));
    assert!(!picked.contains("\nquick "));
    assert!(pasted.contains("\nstatus \"copied and pasted\" "));
    assert!(pasted.contains("\ncopied \"https://example.com/x\"\n"));
    assert!(pasted.contains("\nrow 4 \"ready> https://example.com/x\"\n"));
    assert!(!put_away.contains("\nquick ") && put_away.contains("\nrow 4 \"ready>\"\n"));
    // Quick open labels only what a Cmd+click would follow.
    assert!(open.contains(
        "\nquick typed \"\": a at 1,4 Open(Uri(\"https://example.com/x\")), s at 0,26 Open(Uri(\"https://example.com/x\"))\n"
    ));
    assert!(opened.contains(
        "\nlink None followed [\"Open https://example.com/x\", \"Open https://example.com/x\"]\n"
    ));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn a_restore_launch_brings_back_tabs_splits_and_directories() {
    let dir = scratch("restore");
    let (first, saved) = launch(
        &dir,
        &["--restore"],
        "wait ready>
         resize 24 80
         sleep 300
         type mkdir alpha beta && cd alpha && echo in-$((1+1))
         keys enter
         wait in-2
         keys cmd-d
         wait ready>
         type cd ../beta && echo in-$((2+1))
         keys enter
         wait in-3
         keys cmd-ctrl-left
         keys cmd-t
         wait ready>
         keys cmd-1
         dump
         quit",
    );
    let (second, restored) = launch(
        &dir,
        &["--restore"],
        "wait ready>
         dump
         keys cmd-alt-left
         wait ready>
         dump
         keys cmd-2
         wait ready>
         dump
         quit",
    );
    let layout = fs::read_to_string(dir.join("rtty/layout.json"));
    let (third, plain) = launch(&dir, &[], "wait ready>\ndump\nquit");
    let kept = fs::read_to_string(dir.join("rtty/layout.json"));
    remove(&dir);
    let (layout, kept) = (layout.unwrap(), kept.unwrap());
    assert!(first.success() && second.success() && third.success());
    let [again, left, other] = &restored[..] else { panic!("{} dumps", restored.len()) };
    let split = "\npane 1 at 0,0 24x36\npane 2 at 0,37 24x43\n\n";
    let window = "window 24x80 tab 1 of 2 focus 2 zoomed false title \"~/beta\"\n";
    assert!(saved[0].starts_with(window) && saved[0].contains(split));
    assert!(again.starts_with(window) && again.contains(split));
    let frame =
        |dump: &str| dump.lines().find(|line| line.starts_with("frame ")).map(str::to_owned);
    assert!(frame(again).is_some() && frame(again) == frame(&saved[0]));
    // Each pane's shell is back in its directory.
    assert!(left.contains("focus 1 zoomed false title \"~/alpha\"\n"));
    assert!(other.contains("tab 2 of 2 focus 3 zoomed false title \"~/beta\"\n"));
    assert!(other.contains("\npane 3 at 0,0 24x80\n\n"));
    // A scripted launch without --restore neither restores the layout nor saves its own.
    assert!(plain[0].contains(" tab 1 of 1 focus 1 zoomed false title \"~\"\n"));
    assert!(frame(&plain[0]) != frame(again));
    assert_eq!(layout, kept);
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn a_programs_title_and_progress_show_on_the_bar() {
    let (status, dumps) = run(
        "progress",
        "",
        r"wait ready>
         type printf '\e]2;my title\a\e]9;4;1;40\e\\'; cat -n
         keys enter
         type go
         keys enter
         wait 1  go
         dump
         keys cmd-t
         wait ready>
         dump
         keys cmd-1 ctrl-c
         type echo do''ne
         keys enter
         wait done
         dump
         quit",
    );
    let [lone, pills, ended] = &dumps[..] else { panic!("{} dumps", dumps.len()) };
    assert!(status.success());
    let progress = "progress Some(Progress { state: Normal, percent: Some(40) })\n";
    assert!(lone.contains(" focus 1 zoomed false title \"my title\"\n"));
    assert!(lone.contains(" window title \"my title\"\n"));
    assert!(lone.contains(&format!("\nbar title Some(\"my title\") folder true {progress}")));
    assert!(lone.contains("\ndock badge Some(\"40%\"), "));
    assert!(pills.contains("\nwindows 1 tabs [\"1: my title\", \"2: ~\"] "));
    assert!(
        pills
            .lines()
            .any(|line| line.starts_with("bar tab 1 ") && line.ends_with(progress.trim_end()))
    );
    assert!(
        pills.lines().any(|line| line.starts_with("bar tab 2 ") && line.ends_with("progress None"))
    );
    // The shell's next prompt takes the progress away.
    assert!(
        ended.lines().any(|line| line.starts_with("bar tab 1 ") && line.ends_with("progress None"))
    );
    assert!(ended.contains("\ndock badge None, "));
}

/// What a window manager reads and asks through `rtty cli`: each window's id of its own and its
/// macOS window number, each pane's progress and the user vars its program set (tutti's TUI names
/// its session so), a new window in a directory, and the one rtty running found with no
/// `RTTY_SOCKET`.
#[test]
#[ignore = "needs a display: run with `just ui`"]
fn the_cli_tells_a_window_manager_of_windows_and_opens_one() {
    let dir = scratch("manager");
    let rtty = env!("CARGO_BIN_EXE_rtty");
    let script = format!(
        r"wait ready>
          type printf '\e]9;4;1;40\e\\\e]1337;SetUserVar=TUTTI_TUI=d29yaw==\a'; {rtty} cli list > one.json; echo li''sted
          keys enter
          wait listed
          type {rtty} cli window new --cwd /private/tmp > new.json; env -u RTTY_SOCKET {rtty} cli list > two.json; echo op''ened
          keys enter
          wait opened
          sleep 500
          quit"
    );
    let (status, _) = launch(&dir, &[], &script);
    let read = |name: &str| fs::read_to_string(dir.join(name)).unwrap_or_default();
    let (one, new, two) = (read("one.json"), read("new.json"), read("two.json"));
    remove(&dir);
    assert!(status.success());
    let windows = |json: &str| -> Vec<serde_json::Value> {
        let parsed: serde_json::Value = serde_json::from_str(json).expect(json);
        parsed["windows"].as_array().expect("windows").clone()
    };
    let [first] = &windows(&one)[..] else { panic!("{one}") };
    let pane = &first["tabs"][0]["panes"][0];
    assert_eq!(pane["progress"], serde_json::json!({ "state": "normal", "percent": 40 }));
    assert_eq!(pane["vars"], serde_json::json!({ "TUTTI_TUI": "work" }));
    assert!(first["window_number"].as_i64().is_some_and(|number| number > 0), "{one}");
    let uid = first["uid"].as_str().filter(|uid| !uid.is_empty()).expect("a uid");
    // The new window is told as `list` tells it, with ids of its own, its shell in the directory.
    let [opened] = &windows(&new)[..] else { panic!("{new}") };
    assert_ne!(opened["uid"], first["uid"]);
    assert_ne!(opened["window_number"], first["window_number"]);
    let both = windows(&two);
    let uids: Vec<_> = both.iter().map(|window| window["uid"].as_str().unwrap()).collect();
    assert_eq!(uids, [uid, opened["uid"].as_str().unwrap()]);
    assert_eq!(both[1]["tabs"][0]["panes"][0]["cwd"], "/private/tmp");
    // At its next prompt the first pane's progress and vars were gone.
    assert_eq!(both[0]["tabs"][0]["panes"][0].get("vars"), None);
}

/// `tabs = false`, for a window manager with tabs of its own: no bar and no window buttons, a
/// new tab is a window, a saved window of two tabs comes back as two windows (the one that
/// showed keeping its id), and turning tabs back on brings the bar and the buttons back.
#[test]
#[ignore = "needs a display: run with `just ui`"]
fn without_tabs_a_window_has_no_bar_and_a_tab_is_a_window() {
    let dir = scratch("bare");
    let (first, _) = launch(&dir, &["--restore"], "wait ready>\nkeys cmd-t\nwait ready>\nquit");
    let saved = fs::read_to_string(dir.join("rtty/layout.json")).unwrap_or_default();
    fs::write(dir.join("rtty/config.toml"), "tabs = false\n").unwrap();
    let (second, dumps) = launch(
        &dir,
        &["--restore"],
        r"wait ready>
          sleep 500
          dump
          keys cmd-t
          sleep 1000
          dump
          type printf 'tabs = true\n' > ~/rtty/config.toml; echo wr''itten
          keys enter
          wait written
          sleep 2500
          dump
          quit",
    );
    let kept = fs::read_to_string(dir.join("rtty/layout.json")).unwrap_or_default();
    remove(&dir);
    assert!(first.success() && second.success());
    let windows =
        |json: &str| -> Vec<serde_json::Value> { serde_json::from_str(json).expect(json) };
    let [tabbed] = &windows(&saved)[..] else { panic!("{saved}") };
    let [bare, opened, again] = &dumps[..] else { panic!("{} dumps", dumps.len()) };
    assert!(bare.contains("\nwindows 2 tabs [\"1: ~\"] ") && bare.contains("\nbar 0px "));
    assert!(bare.contains("\nwindow buttons Some(\"hidden\") "));
    assert!(opened.contains("\nwindows 3 tabs [\"1: ~\"] "));
    assert!(again.contains("\nbar 36px ") && again.contains("\nwindow buttons Some(\"shown\") "));
    // Three windows of one tab each were saved: the two the tabs came back as, at the saved
    // frame, one of them with the saved window's id, and the one `new-tab` opened.
    let kept = windows(&kept);
    assert_eq!(kept.len(), 3);
    assert_eq!(kept.iter().filter(|window| window["uid"] == tabbed["uid"]).count(), 1);
    assert_eq!(kept.iter().filter(|window| window["frame"] == tabbed["frame"]).count(), 2);
}

/// The cell-border protocol in the real window, by the dump's `border` lines: a frame's top in a
/// palette color under the text and its left side in the default foreground over it, a rounded
/// corner with its outside filled, the colors following the scheme, a written cell losing its
/// borders, and the query answered ahead of DA1.
#[test]
#[ignore = "needs a display: run with `just ui`"]
fn cell_borders_are_drawn_follow_the_scheme_and_go_when_their_cell_is_written() {
    let (status, dumps) = run(
        "borders",
        "",
        r"wait ready>
         resize 24 100
         appearance dark
         type b() { printf '\eP1+bcell-border;%s\e\\' $@ }
         keys enter
         type clear; printf '\e[2;3H'; b 'set;3;32;p4;0;0;1;4' 'set;192;32;fg;0;1;2;1' 'round;1;1;1;outside=default'; printf '\e[5;1H'; echo do''ne
         keys enter
         wait done
         dump
         appearance light
         sleep 300
         dump
         type printf '\e[2;3Hx\e[6;1H'; echo wr''itten
         keys enter
         wait written
         dump
         type stty raw -echo; b q; printf '\e[c'; exec cat -v
         keys enter
         wait 22c
         dump
         quit",
    );
    let [dark, light, written, asked] = &dumps[..] else { panic!("{} dumps", dumps.len()) };
    assert!(status.success());
    let lines = |dump: &str, start: &str, color: &str| {
        let ours = |line: &&str| line.starts_with(start) && line.ends_with(color);
        dump.lines().filter(ours).count()
    };
    // Four cells' top edges, less the half the corner's arc takes, and the arc; two cells' left
    // edges over the text.
    assert_eq!(lines(dark, "border below rect ", " #61afef"), 7);
    assert_eq!(lines(dark, "border below stroke ", " #61afef"), 1);
    assert_eq!(lines(dark, "border above rect ", " #abb2bf"), 4);
    assert_eq!(lines(dark, "border ", ""), 13);
    assert!(dark.lines().any(|line| {
        line.starts_with("border outside fill ") && line.contains(" #282c34 within ")
    }));
    assert_eq!(lines(light, "border below ", " #268bd2"), 8);
    assert_eq!(lines(light, "border above rect ", " #657b83"), 4);
    assert!(light.contains(" #fdf6e3 within "));
    // The corner cell written: its borders and its rounding go, its neighbours' stay.
    assert_eq!(lines(written, "border below rect ", " #268bd2"), 6);
    assert_eq!(lines(written, "border above rect ", " #657b83"), 2);
    assert_eq!(lines(written, "border ", ""), 8);
    // As `cat -v` shows them, the dump quoting the row: the reply's ST, then DA1's answer.
    let features = "pen,rect,half,center,palette,round,priority,outside,radius";
    assert!(asked.contains(&format!("\"^[P1+bcell-border;ok;1;{features}^[\\\\^[[?62;22c\"")));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn a_dropped_file_is_typed_escaped_and_a_click_is_reported_to_a_program_that_asked() {
    let (status, dumps) = run(
        "mouse",
        "",
        r#"wait ready>
         resize 24 80
         sleep 300
         drop 0 3 "/tmp/a b.png" "/tmp/it's"
         wait /tmp/a
         dump
         keys ctrl-u
         type printf '\e[?1000h\e[?1006h'; cat -nv
         keys enter
         type go
         keys enter
         wait 1  go
         click 10 20
         wait ;11m
         dump
         quit"#,
    );
    let [dropped, reported] = &dumps[..] else { panic!("{} dumps", dumps.len()) };
    assert!(status.success());
    assert!(dropped.contains("\nrow 0 \"ready> /tmp/a\\\\ b.png /tmp/it\\\\'s\"\n"));
    assert!(reported.contains("mouse: Click, mouse_encoding: Sgr, "));
    // The press and the release, at the cell counted from 1.
    assert!(reported.contains("\nrow 3 \"^[[<0;21;11M^[[<0;21;11m\"\n"));
    assert!(!reported.contains("\nselected "));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn a_clipboard_read_asks_first_and_a_script_is_answered_with_a_stand_in() {
    let (status, dumps) = run(
        "clipboard",
        "",
        r"wait ready>
         type printf '\e]52;c;?\a'; cat -nv
         keys enter
         type go
         keys enter
         wait 1  go
         dump
         answer cancel
         sleep 300
         dump
         keys cmd-d
         wait ready>
         type printf '\e]52;c;?\a'; cat -nv
         keys enter
         type go
         keys enter
         wait 1  go
         answer ok
         wait cnR0eSAt
         dump
         quit",
    );
    let [asked, denied, allowed] = &dumps[..] else { panic!("{} dumps", dumps.len()) };
    assert!(status.success());
    let question = " wants to read the clipboard\" \"Allow sends it the text you copied last.\" [Allow, Deny] (Return and Esc: Deny)\n";
    assert_eq!(asked.matches(question).count(), 1);
    // Nothing is answered before the answer, and nothing after Deny.
    assert!(!asked.contains("\nrow 3 ") && !denied.contains("\nrow 3 "));
    assert_eq!(allowed.matches(question).count(), 2);
    // Allow answers the pane that asked, with `rtty --script's clipboard` in base64, never the
    // clipboard's text.
    assert_eq!(allowed.matches("\nrow 3 ").count(), 1);
    assert!(allowed.contains("\nrow 3 \"^[]52;c;cnR0eSAtLXNjcmlwdCdzIGNsaXBib2FyZA==^G\"\n"));
}

#[test]
#[ignore = "needs a display: run with `just ui`"]
fn a_programs_copy_and_a_paste_keep_off_the_clipboard_in_a_script() {
    let (status, dumps) = run(
        "paste",
        "",
        r"wait ready>
         resize 24 80
         sleep 300
         type printf '\e]52;c;b3NjIHRleHQ=\a'; echo do''ne
         keys enter
         wait done
         dump
         keys cmd-v
         wait ready> rtty --script's clipboard
         dump
         menu Edit Paste
         wait clipboardrtty
         dump
         quit",
    );
    let [copied, pasted, again] = &dumps[..] else { panic!("{} dumps", dumps.len()) };
    assert!(status.success());
    // `osc text` in base64: what the program copied is the dump's alone.
    assert!(copied.contains("\ncopied \"osc text\"\n"));
    // A paste types what stands in for the clipboard, by the chord and by the menu.
    assert!(pasted.contains("\nrow 2 \"ready> rtty --script's clipboard\"\n"));
    assert!(
        again.contains("\nrow 2 \"ready> rtty --script's clipboardrtty --script's clipboard\"\n")
    );
}
