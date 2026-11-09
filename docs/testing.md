# Testing

## The merge gate

```sh
just verify
```

A change merges when this passes. For each of the repository's two Cargo workspaces it runs
`cargo check --all-targets`, `cargo test`, `cargo clippy --all-targets -- -D warnings` and
`cargo fmt --check`: rtty at the root first, then rtty-vt with `--manifest-path vt/Cargo.toml`.

[rtty-vt](../vt/README.md), the terminal engine in `vt/`, is a Cargo workspace of its own, with
its own `Cargo.lock`. A `cargo test` at the root builds it as a dependency and runs none of its
tests, the differential tests against alacritty_terminal among them, so the gate runs every step
a second time there.

The tests the gate runs at the root are the unit tests beside the code in `src/`. Some run the
system's shells (`/bin/sh`, `/bin/zsh`, `/bin/bash`, `/bin/dash`) in real ptys; none opens a
window.

## rtty-vt's long campaigns

```sh
just campaigns
```

rtty-vt's ignored tests, in release: the long random campaigns against alacritty_terminal and
through `snapshot()` and `redraw()`, and the measurement of the heap a session holds. About a
minute; the gate leaves them out. [vt/README.md](../vt/README.md) says what each suite checks,
and how to point the fixture tests at other directories.

## The UI suite

```sh
just ui
```

The scripted UI suite drives the real rtty in windows, so it needs an awake display, and it runs
its tests one at a time (`cargo test --test ui -- --ignored --test-threads=1`). Its tests are
ignored in a plain `cargo test`: the gate and CI build and lint the suite and do not run it.
See [tests/ui/README.md](../tests/ui/README.md).

## CI

`.github/workflows/ci.yml` runs `just verify` on a macOS runner for every push to `main` and
every pull request. It runs neither the campaigns nor the UI suite.
