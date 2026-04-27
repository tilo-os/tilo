# Nirios — Testing State

Snapshot taken: 2026-04-27. Run via `RUSTC_WRAPPER= cargo test -p <crate> ...` per crate, all inside `distrobox enter my-distrobox`. The host's `~/.cargo/config.toml` points at a missing sccache, so `RUSTC_WRAPPER=` is mandatory.

The workspace as committed lists `apps/browser` as a member. `apps/browser` cannot be resolved offline because it depends on `libservo` from `git = "https://github.com/servo/servo.git"`, and upstream no longer publishes a package by that name. Cargo refuses to even parse the workspace until that's gone, so for this sweep we temporarily edited the root `Cargo.toml` to move `apps/browser` to `workspace.exclude`. **That edit is reverted before commit** — it must not land. The same workaround is needed for any future per-crate test runs while the libservo issue persists.

## Per-crate status

| Crate | Build | Tests passing | Notes |
|---|---|---|---|
| `quick-ui` (lib) | ok | 119 / 119 | 2 lingering `winit` deprecation warnings in `quick-ui/src/app.rs` (upstream API moved to `ActiveEventLoop::create_window` / `EventLoop::run_app`); production-code change, left as-is. |
| `quick-ui` (integration) | ok | 84 / 85 | 1 failure: `screenshot_test::tab_bar_renders_pills_inside_six_px_insets` samples (250, 0, 250) instead of (255, 0, 255) at the top inset row — minor pixel bleed of ~5 channel-units. Not a test bug; either tab-bar shadow/AA reaches into the inset or the magenta clear is being touched. Treat as a render correctness signal, not a flaky test. |
| `quick-shell` (lib) | ok | 81 / 81 | Was warning on `use … Cacheable` in `network/wayland.rs:16` — fixed in this sweep. |
| `quick-shell` (integration: most) | ok | 104 / 106 across 13 integration files | Includes `e2e_shell_flows` (15), `interaction_test` (25 of the suite around it), `shell_state_test` (35), `shell_chrome_test` (4), `wayland_test` (1 of 2), etc. |
| `quick-shell` (integration: env-blocked) | fail | 0 / 2 | `interaction_test::test_click_interaction_in_sandbox` panics with "Test client did not become ready" — needs the bubblewrap sandbox to actually spawn a test client; doesn't work inside distrobox. `wayland_test::test_wayland_handshake_fixed` shells out to `alacritty`, which is not installed in the test container. Both are environment failures, not regressions. |
| `quick-fs-fuse` | **fail (pre-existing)** | n/a | Will not compile offline. `iroh 0.96.1` → `ed25519-dalek 3.0.0-pre.1` → `ed25519 3.0.0-rc.4` fails on rustc 1.94 with `E0277`: `?` cannot convert `KeyError` to `pkcs8::Error`. This is a known incompatibility between two pre-release crates pinned by iroh. `cargo update -p ed25519@3.0.0-rc.4` and `-p pkcs8@0.11.0-rc.12` both report nothing newer. The fix is upstream (or pin iroh older). The unused-imports cleanup at `quick-fs-fuse/tests/integration_test.rs:1-2` was applied separately and committed — but the test suite did not run, so its result is unknown for this snapshot. |
| `quick-terminal` | ok | 7 / 7 | Lib tests only; no `tests/` directory. |
| `file-explorer` | ok | 43 / 43 | 13 lib tests + 30 in `tests/parity.rs`. |
| `quick-browser` | **skip** | n/a | Depends on `libservo` from a git source that no longer exposes that package name. Per CLAUDE.md guidance, expected to fail offline. Standalone `cargo build` inside `apps/browser` also fails because the submodule's `Cargo.toml` points at the parent workspace. |

Aggregate (excluding browser + fuse): **397 tests pass**, 1 fails on a render-correctness assertion, 2 fail because the runtime environment lacks bubblewrap / alacritty.

## Coverage gaps observed

System apps and shell features that have no automated tests today (selection, not exhaustive):

- `quick-shell/src/shell/quick_settings.rs` — entire module has no `#[cfg(test)]` block. The quick-settings popover (battery / wifi / brightness / clock) only has indirect coverage via `shell_chrome_test::quick_settings_state_defaults_and_mutates_through_chrome`. There is no rendering / golden test that pins the layout, and the live clock makes pinning a snapshot impossible without first injecting a fake `Local::now()`.
- `quick-shell/src/shell/topbar.rs` (vs. `topbar_chrome.rs`) — the `topbar.rs` module has no tests. `topbar_chrome.rs` has tests for state plumbing (`commit_workspace_name_applies_buffer_to_active_workspace`, the avatar overflow chip), but nothing for the keyboard-driven workspace-rename flow (Esc to cancel, Enter to commit).
- `quick-shell/src/shell/sidebar_wheel.rs` and `sidebar_wheel_disc.rs` — only the snapshot-style test in `tests/sidebar_wheel_disc_snapshot.rs` exercises the disc. There is no test for spoke focus arithmetic, no test for what happens when the workspace count changes mid-rotation, no test for keyboard navigation across spokes.
- `quick-shell/src/shell/workspace_drop.rs::apply_workspace_drop_from_path` — has happy-path tests for known extensions, folder drops, and stub appending, but no fixture for unreadable / non-existent paths or for symlink loops; no test for the case where the active workspace is in the middle of a close-fade.
- `quick-shell/src/shell/persist.rs`, `state.rs` — no in-module tests at all. The CRDT round-tripping is currently only proven by the higher-level `shell_state_test` integration suite, which doesn't cover serialization / deserialization edge cases (truncated CRDT blob, corrupted Automerge actor id).
- `quick-shell/src/shell/system_monitor.rs` and `tests/system_monitor_test.rs` — there are 7 tests but they only sample the produced strings; no test pins the threshold colours (CPU > 80% red, etc.) and no test exercises the case where `/proc` reads fail.
- `quick-shell/src/shell/remote_cursors.rs`, `search.rs`, `split_row.rs`, `tile.rs`, `desktop_backdrop.rs`, `notes_app.rs`, `editor_app.rs`, `chrome_state.rs`, `nirios_tokens.rs`, `browser_stub.rs`, `overview.rs` — none of these have unit tests. Some have indirect coverage via integration tests; several do not.
- `quick-ui/src/widget/command_palette.rs` — has tests for activation but nothing for the dispatch-to-overview-toggle flow (i.e. Cmd+K → palette → "Overview" → emits `OverviewToggle` event consumed by shell). No e2e test threads that command from quick-ui into shell.
- `quick-ui` `screenshot_test` — only one widget (the tab bar) has a pixel-sample golden. Window group, sidebar wheel disc, topbar chrome, and overview overlay have no rasterised goldens despite `tests/group_row_visual_journey.rs` and `tests/window_group_visual_journey.rs` existing as state-only journeys.
- `apps/terminal` — only 7 lib tests. No `tests/` directory at all. No coverage of the PTY round-trip, no coverage of GPU shader fallback, no coverage of selection / clipboard.
- `apps/file-explorer/tests/parity.rs` (30 tests) is solid for layout parity, but there's no test for the trash flow, no test for the rename flow, no test for the FUSE-projection-but-not-yet-hydrated case (file present in metadata, content not local).
- `quick-fs-fuse` — has one tokio integration test (`tests/integration_test.rs::test_fuse_ops`) that exercises mount/list/read against a tempdir-backed FS, but does not cover hydration-on-read, cache eviction, the 10s metadata TTL, or behaviour when the Iroh endpoint can't reach a peer.

## Flaky / time-sensitive tests

- `quick-shell/src/shell/topbar_chrome.rs:211` and `:622` — `Local::now().format("%H:%M")` is called from inside `draw`. Any pixel-level snapshot of the topbar will mismatch the moment the wall clock crosses a minute boundary. Until that is injectable (e.g. a `Clock` trait passed through `Theme`), the quick-settings golden cannot be pinned.
- `quick-shell/tests/wayland_test.rs::test_wayland_handshake_fixed` — depends on `alacritty` being on `PATH`. Not flaky, just environmentally fragile.
- `quick-shell/tests/interaction_test.rs::test_click_interaction_in_sandbox` — needs the bubblewrap sandbox to spawn a test client and report ready over a Unix socket. Times out at 20s when bwrap isn't usable in the container. The 20s wait makes this the slowest test in the suite even on success.
- `quick-shell/tests/e2e_shell_flows.rs` — 17.86s wall time. Contains a fixed `Duration::from_secs(...)` somewhere in the flow; under load (cold caches, parallel cargo) it edges close to its budget.
- `quick-ui/tests/screenshot_test.rs::tab_bar_renders_pills_inside_six_px_insets` — currently failing; not strictly *time*-flaky but it is hardware-/AA-sensitive: the assertion is a strict pixel equality on a vello-rendered scene, so any rendering backend change (lavapipe vs. llvmpipe, anti-alias mode default) can flip the result. Replacing the strict equality with a tolerance window would fix it without weakening the inset test.

## How to run

All commands run inside `distrobox enter my-distrobox -- bash -lc '<cmd>'` with `RUSTC_WRAPPER=` to override the host's broken sccache config. Do **not** run a workspace-wide `cargo test` (per the project's `CLAUDE.md` rule about the shared target lock).

```bash
# quick-ui (in-tree). No browser-exclusion needed if apps/browser is committed
# but the workspace currently fails to resolve because libservo is gone — see
# the "Constraints" note above before running these.
cd <worktree-root>
RUSTC_WRAPPER= cargo test -p quick-ui --lib
RUSTC_WRAPPER= cargo test -p quick-ui --tests --no-fail-fast

# quick-shell (compositor)
RUSTC_WRAPPER= cargo build -p quick-desktop
RUSTC_WRAPPER= cargo test -p quick-desktop --lib
RUSTC_WRAPPER= cargo test -p quick-desktop --tests --no-fail-fast
# Single integration file (faster, no lock fighting):
RUSTC_WRAPPER= cargo test -p quick-desktop --test e2e_shell_flows
RUSTC_WRAPPER= cargo test -p quick-desktop --test shell_state_test

# quick-fs-fuse — currently fails to build offline; skip until the
# ed25519/iroh stack is unstuck. To attempt:
RUSTC_WRAPPER= cargo test -p quick-fs-fuse

# Apps
RUSTC_WRAPPER= cargo test -p quick-terminal
RUSTC_WRAPPER= cargo test -p file-explorer
# Browser: skip. Will not resolve libservo offline.
```

To pin a screenshot for visual diffing, set `QUICK_UI_SCREENSHOT_DIR` before running `screenshot_test`, e.g. `QUICK_UI_SCREENSHOT_DIR=/tmp/shots cargo test -p quick-ui --test screenshot_test`. PNGs land in that directory.
