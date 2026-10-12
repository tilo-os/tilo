# Constrained notification center

Baseline: main at `6ea52d50ca3b8bf425360391446a5374e8c3a55f`.
The fixture paints the production portable NotificationCenter used by both
the native shell and desktop surface. Notices are fixture-only; no real desktop,
accounts, microphone or policy state is used.

At the native 320 px panel width with 200% text, the baseline reduces the
panel title to “Not…” and wraps “Notifications cleared” outside its status band.
The measured header now gives the title a separate line when needed. The
existing shared DrillInList empty painter receives that measured header offset.
The cleared status gets a measured caption line above the shared Undo button.
The normal 400 px light and dark populated captures are byte-for-byte identical.

The existing visible Back button now has an accessible button node, bounds,
focus and Enter/Space activation. It is appended after the existing children,
preserving the row, Clear and Undo paths. Choosing another control with the
pointer retires the previous Back focus.

## Qualification

The header-space regression failed on baseline production and passes after
the fix. Five focused additions cover measured title space, complete row/pager
budgets, normal geometry, repeated Clear/Undo and nested Back/Escape journeys,
Back accessibility/activation, pointer focus retirement and full cleared status.
All 17 portable notification-center tests pass.

A separate included native test module avoids the known #3710 end-of-file test
append. Native qualification exercises the actual shell panel-width/sizing caller and
Clear/Undo/Back routing, alongside existing native notification journeys.
Required gates include strict all-target Clippy for desktop-surface and shell,
`cargo check --locked -p shell --tests`, repository hooks and affected-package
selection. Installed-device and VM qualification remain distinct coverage.

## Captures

The exact baseline has 24 PNGs and the final matrix has 35 PNGs. Representative
before/after PNGs are published with the PR; internal logs and raw manifests stay
private. Inspected states include populated application errors, Clear focus,
accessible Back focus, row-dismiss focus, empty/Undo and restoration; light,
dark, high contrast, 200% text and combined 150% UI / 130% text scaling.

```sh
cargo test --locked -p tilo-desktop-surface --lib notifications::center
cargo run --locked -p tilo-desktop-surface --example notification_center_shots -- target/notification-center-shots
cargo test --locked -p shell --lib overlay_event_absorb_tests::notification
cargo clippy --locked -p tilo-desktop-surface -p shell --all-targets -- -D warnings
cargo check --locked -p shell --tests
```

All compilation, input tests and rendering ran on the supported pinned remote
builder. The screenshots use its CPU Vulkan renderer; no visible application
or compositor was launched on the laptop.
