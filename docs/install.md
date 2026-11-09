# Building and installing

How to build rtty, run it from the source tree, install it as `rtty.app` with `just`, and redraw
its icon.

## Requirements

macOS only (GPUI's Metal renderer, and AppKit for notifications and Finder); no Linux port is
planned. Needs a Rust toolchain with edition 2024 and Xcode's SDK; the offline Metal compiler is
**not** needed (see [the GPUI pin](internals.md#the-gpui-pin)).

## Running from source

```sh
cargo run --release                     # your login shell
cargo run --release -- --font "SF Mono" --size 13
cargo run --release -- --font "FiraCode Nerd Font Mono" --ligatures off
cargo run --release -- -e htop          # a program instead of the shell
cargo run --release -- --tutti work     # a window that is tutti session `work`
cargo run --release -- --tutti work:3   # pane 3 of tutti session `work` instead of the shell
cargo run --release -- --smoke          # headless pipeline check, exits 0 or 1
cargo run --release -- --bench          # headless pipeline throughput
cargo run --release -- --stats          # print frame timings to stderr while drawing
rtty cli list                           # from inside a pane: the windows, tabs and panes
```

Every flag is in [cli.md](cli.md#flags). A build run this way is not an app bundle, so it posts no
notifications ([Bell and notifications](windows.md#bell-and-notifications)).

## Installing the app

With [just](https://github.com/casey/just):

```sh
just install                          # /Applications/rtty.app, and ~/.local/bin/rtty
just install ~/Applications ~/bin     # somewhere else: the app's folder, then the link's
just uninstall                        # the same arguments as install
just bundle                           # only build target/rtty.app
```

`just install` does these in order:

1. builds the release binary;
2. assembles `rtty.app` (the binary, `bundle/Info.plist` with Cargo.toml's version filled in, the
   icon);
3. signs it;
4. moves it into the folder in place of the one there (moved whole, so the folder never holds a
   copy under another name);
5. links `~/.local/bin/rtty` to the binary inside it, so `rtty cli` works from any shell with
   `~/.local/bin` on its `PATH`.

A `~/.local/bin/rtty` that is a file rather than a link stops the install untouched. `just
uninstall` removes the app, and the link only when it is the one install made.

The signature is ad hoc (`codesign --sign -`, no developer identity), which is all an app built on
the Mac that runs it needs: Gatekeeper's notarization check applies to downloaded apps, and a
local build is not one.

## The icon

`bundle/icon.py` draws the icon from shapes, no fonts: a ❯_ prompt in One Dark Pro's colors on
macOS's rounded app-icon square, with Pillow at 4× and downsampled. `just icon` (needs
[uv](https://docs.astral.sh/uv/)) reruns it and rebuilds `bundle/rtty.icns` with `sips` and
`iconutil`, leaving `target/icon/icon-sizes.png`, the icon at 16 to 256 px, to check it still
reads small.
