# rtty

A GPU terminal emulator in Rust, for macOS: [GPUI](https://gpui.rs) (Zed's UI framework) draws it,
[rtty-vt](vt/README.md), our own terminal engine in `vt/`, holds the terminal state, and
[portable-pty](https://crates.io/crates/portable-pty) opens the pty.

Like WezTerm, rtty has its own windows, tabs and splits. A tab is divided into panes, and each
pane is one terminal running your login shell or a program. It also has:

- scrollback with search, a vi-style copy mode, and quick select, which labels the URLs, paths and
  hashes on screen so that typing a label copies one;
- links: a `Cmd+click` opens a URL, or opens a path such as `src/main.rs:12:5` in your editor at
  that line;
- shell integration for zsh, bash and fish that loads without an edit to your dotfiles: jumps
  between prompts, a copy of the last command's output, a notification when a long command ends
  out of view;
- ligatures, kitty's keyboard protocol and kitty's graphics protocol for images;
- color schemes in WezTerm's format that follow the macOS light and dark appearance;
- windows, tabs and splits that come back at the next launch.

## rtty and tutti

rtty is one half of a tmux replacement. The other half is tutti, the multiplexer for AI coding
agents: a TUI you run in any terminal, whose daemon keeps sessions alive across detaches. They
pair in two ways:

- tutti's TUI runs in an rtty pane, and one set of keys moves through Neovim's windows, tutti's
  panes and rtty's panes.
- rtty is also a tutti client. `rtty --tutti work` opens a window that is the tutti session
  `work`, with tutti's tabs and splits as its own, and `rtty --tutti work:3` shows one pane of
  it. tutti's daemon keeps the programs running; rtty draws them, so rtty's scrollback, search,
  selection and links work on them.

tutti's TUI is the primary front end for a tutti session. It is the complete one: the agent
sidebar, the approvals popup and diffs are there, and new agent features land there first. rtty's
native tutti panes, the second way above, are a plain view of a session: rtty draws the panes,
shows an agent's state on the tab's pill, and notifies when an agent is blocked or done. No
further agent UI is planned in rtty.

Both run the same engine, rtty-vt, so a program behaves the same in either. More in
[docs/tutti.md](docs/tutti.md).

## Build and install

macOS only. Building needs a Rust toolchain with edition 2024 and Xcode's SDK.

With [just](https://github.com/casey/just):

```sh
just install    # builds rtty.app into /Applications and links ~/.local/bin/rtty to its binary
just uninstall  # removes the app, and the link install made
```

Or run it from the source tree without installing:

```sh
cargo run --release
```

Notifications need the installed app. More in [docs/install.md](docs/install.md).

## Everyday use

```sh
rtty                              # a window running your login shell
rtty -e htop                      # a program instead of the shell
rtty --font "SF Mono" --size 13   # over the config file's font and size
rtty --tutti work                 # a window that is tutti session `work`
rtty cli list                     # from inside a pane: the windows, tabs and panes, as JSON
```

From the source tree, `cargo run --release --` stands in for `rtty`. Every flag, and every `rtty
cli` command, is in [docs/cli.md](docs/cli.md).

Ctrl keys belong to the program in the pane; Cmd chords belong to rtty. The ones used most:

| Key | Does |
| --- | --- |
| `Cmd+N`, `Cmd+T` | new window, new tab |
| `Cmd+D`, `Cmd+Shift+D` | split the pane right, down |
| `Cmd+W` | close the pane, asking first while it runs a program |
| `Cmd+Z` | bring back the pane, tab or window closed last, within 5 seconds |
| `Cmd+Opt+arrows` | focus the nearest pane that way |
| `Cmd+Ctrl+arrows` | move the pane's nearest divider that way |
| `Cmd+Shift+Enter` | zoom the pane to fill its tab, or back |
| `Cmd+1`–`9` | show that tab |
| `Cmd+C`, `Cmd+V` | copy the selection, paste |
| `Cmd+F` | search the pane's scrollback and screen |
| `Cmd+Shift+Space` | quick select: label what is worth copying on screen |
| `Cmd+Shift+C` | copy mode: select with vi's keys |
| `Cmd+Up`, `Cmd+Down` | the view to the shell's previous, next prompt |
| `Cmd+K` | clear the pane's scrollback and screen |
| `Cmd+=`, `Cmd+-`, `Cmd+0` | font size up, down, back to the configured one |
| `Cmd+,` | open the config file in your editor |
| `Cmd+click` | follow the link under the pointer |

Every key and its action name, the mouse, and the keys an editor can claim are in
[docs/keys.md](docs/keys.md).

## Configuration

The config file is `~/.config/rtty/config.toml` (`$XDG_CONFIG_HOME/rtty/config.toml` when that is
set). `Cmd+,` opens it in your editor, written first from a commented template when there is none.
Every key is optional, and a saved change takes effect by itself.

```toml
font = "FiraCode Nerd Font"
size = 12
scheme_dark = "OneDark Pro"       # the default
scheme_light = "Solarized Light"  # the default
keys = "tutti"                    # Ctrl+h/j/k/l focus panes and Alt+h/j/k/l resize them too

[bind]                            # chords and their actions, over the defaults
"alt-\\" = "split-right"
```

Every key is in [docs/configuration.md](docs/configuration.md), and
[`config.template.toml`](config.template.toml) lists each at its default.

## Where things are documented

Each topic has one file. When a feature changes, the file that covers it is the one to update;
this README only summarizes.

| File | Covers |
| --- | --- |
| [docs/install.md](docs/install.md) | requirements, running from source, `just install`, the app bundle, the icon |
| [docs/cli.md](docs/cli.md) | every flag, the environment a pane's program gets, `rtty cli` |
| [docs/configuration.md](docs/configuration.md) | the config file, every key, `[bind]`'s syntax, what a reload changes |
| [docs/keys.md](docs/keys.md) | default keys and actions, the mouse, the menu bar, keys a pane can claim, Secure Keyboard Entry, the global hotkey, key encodings (modifyOtherKeys, the kitty protocol) |
| [docs/windows.md](docs/windows.md) | new panes, closing and undoing a close, the tab bar, progress, titles, the cursor, the bell and notifications, restoring windows |
| [docs/selection.md](docs/selection.md) | block selection, search, links and paths, quick select, copy mode, the clipboard (OSC 52) |
| [docs/fonts.md](docs/fonts.md) | fallback fonts, font features, line height, the characters rtty draws itself, ligatures |
| [docs/color-schemes.md](docs/color-schemes.md) | the scheme format, where schemes are found, following the OS, minimum contrast, DEC mode 2031 |
| [docs/shell-integration.md](docs/shell-integration.md) | the OSC 133 marks, how the scripts load in zsh, bash and fish, what the marks give |
| [docs/images.md](docs/images.md) | kitty's graphics protocol as rtty draws it, the cell size it reports |
| [docs/tutti.md](docs/tutti.md) | the two front ends for a tutti session, tutti's TUI in an rtty pane, tutti panes, a window that is a session |
| [docs/scripting.md](docs/scripting.md) | `--script` and its `dump`, `--trace`, `--stats`, `--smoke`, `--bench`, the log |
| [docs/testing.md](docs/testing.md) | the merge gate, CI, the UI test suite |
| [docs/internals.md](docs/internals.md) | what each file in `src/` does, the GPUI pin, patched dependencies, licensing |
| [docs/measurements.md](docs/measurements.md) | throughput, memory and drawing costs |
| [docs/status.md](docs/status.md) | what works, how it was checked, what is not yet checked on screen, the roadmap |
| [vt/README.md](vt/README.md) | rtty-vt, the terminal engine: its API, the kitty graphics subset, its tests |
| [vt/DIVERGENCES.md](vt/DIVERGENCES.md) | where rtty-vt parts from alacritty_terminal |
| [vt/fixtures/README.md](vt/fixtures/README.md) | the recorded terminal byte streams rtty-vt is tested with |
| [editors/README.md](editors/README.md) | the Neovim plugin and the Emacs package, and the contract they speak |
| [patches/README.md](patches/README.md) | the two patched dependencies and what was changed in each |

## Status

What works, how each part was checked, what is not yet checked on screen and what is left to do
are in [docs/status.md](docs/status.md). No Linux port is planned.

## License

Apache-2.0: see [LICENSE](LICENSE).
