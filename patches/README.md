# Patched dependencies

Two crates gpui 0.2.2 depends on, each at its last release, copied from crates.io with one change
each. Rust warns that both changed lines will become errors in a future release (`cargo report
future-incompatibilities`), and neither crate has a release that fixes them. `[patch.crates-io]`
in rtty's `Cargo.toml` builds these copies in their place. Drop a copy when gpui stops depending
on its crate or a fixed release comes out.

| crate | licence | the change |
| --- | --- | --- |
| `block` 0.1.6 (github.com/SSheldon/rust-block) | MIT, by Steven Sheldon (its `Cargo.toml`; the package has no licence file) | `Class`, the type of the `_NSConcreteStackBlock` static, is an opaque `#[repr(C)]` struct instead of an empty enum: a static of an uninhabited type is a future error ([rust#74840](https://github.com/rust-lang/rust/issues/74840)) |
| `proc-macro-error2` 2.0.1 (github.com/GnomedDev/proc-macro-error-2) | MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`) | `extern crate proc_macro` is `pub`, since `__export` re-exports it: re-exporting a private extern crate is a future error ([rust#127909](https://github.com/rust-lang/rust/issues/127909)) |

`block`'s `extern` blocks and functions also say `"C"`, which they meant already: as a path
dependency its warnings show in rtty's build, and an `extern` without an ABI is deprecated.

Left out of the copies: their tests (and the dev-dependencies and `[[test]]` entries for them) and
packaging files. Everything else is as published.
