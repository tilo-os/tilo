# Tilo shell on macOS

Build and open a local development app:

```sh
bash crates/session/shell/build-macos.sh
open target/macos/Tilo.app
```

Or run from the repository:

```sh
cargo run --release --bin shell
```

The native window uses the existing shell chrome, workspace tree, command
palette, UI event loop and Metal rendering. Option-Space or Command-K opens the
palette; Escape dismisses it. Files starts at your home directory. Workspace
state and shell records persist in
`~/Library/Application Support/tilo/macos-shell`. The profile is locked while
open, and malformed state is refused rather than overwritten. A new profile
starts with one empty Home workspace.

Three-finger swipes navigate the shell: up/down selects the previous/next
workspace; left/right focuses the next/previous window along the active strip.
Each swipe moves one step. Two-finger scrolling continues to scroll app content.
To let macOS deliver these swipes to Tilo, enable **Swipe between pages → Swipe
with Three Fingers** in **System Settings → Trackpad → More Gestures**. Move
conflicting Mission Control, App Exposé and full-screen app gestures to four
fingers (or disable them). Three-finger dragging must also be disabled if
enabled under **Accessibility → Pointer Control → Trackpad Options**.
System-owned gestures take precedence over app gestures; Tilo does not change
these settings. See Apple's
[trackpad event guide](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/EventOverview/HandlingTouchEvents/HandlingTouchEvents.html).

Optional arguments, passed after `--` with `cargo run`:

```sh
--state-dir /path/to/profile --files-dir /path/to/files
--shots /tmp/tilo-shell.png
```

`--files-dir` selects Files' initial directory; it does not restrict filesystem
access. `--shots` renders the same shell tree to a PNG and exits. The app bundle
is ad-hoc signed for local development, not notarized for distribution.

## Current scope

This runner hosts Tilo inside one resizable macOS window. Linux compositor,
Wayland clients, D-Bus portals, package sandboxes and Linux auxiliary binaries
remain Linux-specific. Native Terminal process services are not configured; peer
synchronization is disabled. Embedded apps retain their existing host
capabilities and limitations.

Targeted verification:

```sh
cargo test --locked -p shell --bin shell --no-default-features
cargo clippy --locked -p shell --bin shell --no-default-features
```

## UX testing

See the [desktop UX bench](../docs/ux-testing-bench.md) for repeatable user
journeys, interaction budgets, scroll checks and visual reports.
