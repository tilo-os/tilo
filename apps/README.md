# qOS apps

The three apps that ship with qOS:

- `terminal/` — GPU-accelerated terminal, uses `ui`. Binary: `terminal`.
- `file-explorer/` — Spatial file manager, uses `ui`. Binary: `file-explorer`.
- `browser/` — Servo-based web browser, uses `ui`. Binary: `browser`.

All three implement `ui::widget::Widget` and produce a headless PNG via `--shots PATH`.

## Building

These crates are members of the qOS workspace defined in [`qos/Cargo.toml`](https://git.tu-po.com/qos/qos). Build from there:

```bash
cargo build -p terminal -p file-explorer -p browser
```

Path dependencies (`ui = { path = "../../ui" }`) resolve only inside the main qos checkout, where `ui/` is a sibling submodule. Standalone clones of this repo are useful for reading or for component-focused PRs, but the build needs the umbrella.

## Adding a new app

Sketch a workspace member as a new subdirectory next to `terminal/`. If it grows enough to need its own contribution boundary, promote it to a nested submodule (`qos/<app>.git`) — the apps repo then just bumps the pointer.
