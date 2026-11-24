# A crate Tilo's published crates are built from

This crate is part of [Tilo](https://code.tilo.so/tilo/tilo), a collaborative
desktop and operating system. It is published on crates.io because Tilo's other
published crates depend on it. The description on this page says what it does;
its API follows Tilo's needs and changes with them.

To build an app, start from one of these:

- [`tilo-ui`](https://crates.io/crates/tilo-ui): Tilo's UI toolkit, for drawing
  a window with the shared components, layout, text and animation.
- [`tilo-app`](https://crates.io/crates/tilo-app): the Rust app SDK. An app
  declares its state as resources, the changes it allows as actions, and views
  bound to both. It runs as an ordinary window, or inside Tilo with sync,
  history and permissions. Guide:
  [Rust apps](https://tilolang.dev/docs/rust-apps).
- [`tilolang`](https://crates.io/crates/tilolang) and
  [`tilolang-runtime`](https://crates.io/crates/tilolang-runtime): compile and
  run tilolang, the language most Tilo apps are written in. Guide:
  [tilolang docs](https://tilolang.dev/docs/).
- The [`tilolang` command](https://tilolang.dev/install) creates, runs, checks
  and builds an app.

The crate's license is the one in its `Cargo.toml`.
[LICENSING.md](https://code.tilo.so/tilo/tilo/src/branch/main/LICENSING.md)
explains which parts of Tilo are MIT and which are AGPL-3.0.
