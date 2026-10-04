# macOS shell runner

**Goal:** Run the existing Tilo shell as a native macOS application window.

**Design:** Retain the Linux entry point and compositor behind the Linux target.
A macOS entry point constructs the existing `ShellOverlayHost`, workspace tree,
chrome and records, then enters the shared `ui::App` winit/Metal runner. Use
host-provided Files startup data, never the screenshot fixture path. Use real
persisted workspaces; first launch has one empty Home workspace.

This is the runnable shell portion of the existing portable-shell design. Its
complete Notes/Files capability and cross-host collaboration qualification is
a broader milestone and must not be claimed from window startup alone.

- [x] Establish native build failure: the library checks on macOS; the executable
  imports Linux-only runners, DRM pinning and portal services.
- [x] Separate platform entry points and add macOS session construction using
  the existing shell root and explicit state/Files paths.
- [x] Test real workspace mutation/persistence and Files construction with a
  controlled directory; test malformed state refusal and argument validation.
- [x] Build the macOS executable, render the live shell tree, inspect it, launch
  a native window and exercise the shell's existing input paths.
- [x] Document commands and exact supported/unsupported host capabilities.

No replacement shell, invented collaborators, demo workspace, Linux VM, or
external-window substitute qualifies as this runner.

## Verification — 2026-09-15

- `cargo test --locked -p shell --bin shell --no-default-features`: 5 passed.
  Covers real Files entries, workspace persistence, profile locking, invalid
  state preservation, CLI validation and Command-K/Escape through the shared
  UI event driver. Input verification is through that driver, not OS-injected keys.
- `cargo clippy --locked -p shell --bin shell --no-default-features`: passed.
- `bash shell/build-macos.sh`: built and ad-hoc signed `target/macos/Tilo.app`.
  `codesign --verify --deep --strict` and `plutil -lint`: passed.
- Rendered and visually inspected a `--shots` PNG from the actual
  shell host. Final render succeeded without the Linux runtime-path warning.
- Launched the app through Launch Services. CoreGraphics confirmed an onscreen
  Tilo window (800 × 632 including window decoration); the process remained
  alive after startup.
- Independent review found silent shutdown record-save failures; changed to
  `RecordStore::try_flush` and reran tests/Clippy. No other findings.
- The debug linker emits a large `__eh_frame` unwind-table warning. Build and
  runtime verification succeeded. Linux execution and full cross-host capability
  qualification were not tested on this Mac.

Commands and capability scope are documented in `shell/README.md`.
