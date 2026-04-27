# Nirios — Testing State

Snapshot taken: 2026-04-27. Distrobox + `RUSTC_WRAPPER=` per crate. The host's `~/.cargo/config.toml` points at a missing sccache, so the wrapper override is mandatory.

The committed workspace lists `apps/browser` as a member but `quick-browser` depends on `libservo` from `git = "https://github.com/servo/servo.git"`, which upstream no longer publishes under that name. Cargo refuses to even resolve the workspace until that's gone, so for this sweep we temporarily moved `apps/browser` to `workspace.exclude` in the root `Cargo.toml`. **That edit was reverted before commit** — it must not land. The same workaround is needed for any future per-crate test runs while the libservo issue persists.

## Per-crate status

| Crate | check | clippy (#warn) | lib tests | integration tests | Notes |
|---|---|---|---|---|---|
| `quick-ui` | ok | 134 own + 2 deprecation | 122 / 122 | 87 / 88 | 1 fail: `screenshot_test::tab_bar_renders_pills_inside_six_px_insets` (pixel-equality bleed of ~5 channel-units at top inset). 2 winit deprecation warnings in `src/app.rs:374` and `:392` (upstream API moved to `ActiveEventLoop::create_window` / `EventLoop::run_app`). |
| `quick-shell` | ok | 198 own + 3 deprecation | 81 / 81 | 187 / 189 | 2 env-fail: `interaction_test::test_click_interaction_in_sandbox` (no bwrap in distrobox; 34s timeout) and `wayland_test::test_wayland_handshake_fixed` (no `alacritty` in container). With `--test-threads 1`, all 15 `e2e_shell_flows` tests pass cleanly in ~6s. |
| `quick-fs-fuse` | **fail (pre-existing)** | n/a (skipped) | n/a | n/a | Will not compile offline. `iroh 0.96.1` → `ed25519-dalek 3.0.0-pre.1` → `ed25519 3.0.0-rc.4` fails on rustc 1.94 with `E0277`: `?` cannot convert `KeyError` to `pkcs8::Error` at `ed25519-3.0.0-rc.4/src/pkcs8.rs:180`. Same blocker as the prior snapshot. Fix is upstream (or pin iroh older). |
| `quick-terminal` | ok | 5 own (lib + tests) | 7 / 7 | n/a | No `tests/` directory. Lib warnings: `too_many_arguments`, `single_match`, `module_inception`, `let_unit_value`, `implicit_saturating_sub`. |
| `file-explorer` | ok | 1 own | 13 / 13 | 30 / 30 | Single `clippy::ptr_arg` warning in lib. Parity tests all green. |
| `quick-browser` | **skip** | n/a | n/a | n/a | `libservo` git dep no longer resolvable. Excluded from this sweep; expected per CLAUDE.md guidance. |

Aggregate (excluding browser + fuse): **427 tests pass**, 1 fails on a render-correctness assertion, 2 fail because the runtime environment lacks bubblewrap / alacritty. The prior snapshot reported 397 passes, so net **+30 tests** since last sweep (~+3 in `quick-ui` lib, ~+3 in `quick-ui` integration, ~+24 in `quick-shell` integration — `nirios_e2e_test`, `active_group_wireup_test`, `apps_integration` and friends grew).

## Top clippy warnings (by frequency)

Counted across `quick-ui` + `quick-shell` + per-crate own-source (the apps re-emit `quick-ui` lib warnings; we count those once under `quick-ui`).

| Lint | quick-ui | quick-shell | quick-terminal | file-explorer | Total |
|---|---|---|---|---|---|
| `collapsible_if` | 69 | 96 | 0 | 0 | **165** |
| `unnecessary_cast` | 21 | 27 | 0 | 0 | **48** |
| `new_without_default` | 9 | 10 | 0 | 0 | **19** |
| `unnecessary_fold` | 5 | 5 | 0 | 0 | **10** |
| `too_many_arguments` | 4 | 6 | 1 | 0 | **11** |
| `type_complexity` | 4 | 5 | 0 | 0 | **9** |
| `doc_lazy_continuation` | 0 | 4 | 0 | 0 | **4** |
| `collapsible_match` | 3 | 3 | 0 | 0 | **6** |
| `cloned_ref_to_slice_refs` | 3 | 0 | 0 | 0 | **3** |
| `assertions_on_constants` | 3 | 0 | 0 | 0 | **3** |
| `derivable_impls` | 2 | 2 | 0 | 0 | **4** |
| `manual_clamp` | 1 | 2 | 0 | 0 | **3** |
| `let_unit_value` | 0 | 2 | 1 | 0 | **3** |
| `if_same_then_else` | 1 | 2 | 0 | 0 | **3** |
| `zombie_processes` | 0 | 2 | 0 | 0 | **2** |
| `ptr_arg` | 0 | 1 | 0 | 1 | **2** |
| Various single-shot lints | 11 | ~21 | 3 | 0 | ~35 |

The `collapsible_if` and `unnecessary_cast` storms dominate the noise. Both are mechanical-fix-suggestable (`cargo clippy --fix --lib`); deferring them is fine as long as no new ones leak in.

## Failing or skipped tests (with reason)

- `quick-ui::screenshot_test::tab_bar_renders_pills_inside_six_px_insets` — strict pixel equality against the magenta clear at the inset row reads (250, 0, 250) instead of (255, 0, 255). Either tab-bar shadow/AA bleeds into the inset or the magenta clear is being touched. Render-correctness signal, not a flaky test. Tolerance window would unblock.
- `quick-shell::interaction_test::test_click_interaction_in_sandbox` — needs bubblewrap to spawn a sandboxed test client and report ready over a Unix socket. Times out at the 20s wait inside distrobox (overall 34s). Environmental, not a regression.
- `quick-shell::wayland_test::test_wayland_handshake_fixed` — shells out to `alacritty`, which is not on `PATH` in the test container. Environmental.
- `quick-fs-fuse` (entire crate) — pre-existing offline-build break in the iroh→ed25519→pkcs8 chain. Cannot run any tests until iroh is unstuck.
- `quick-browser` (entire crate) — `libservo` git source no longer publishes that package name. Workspace unable to resolve unless excluded.

## Coverage gaps observed

Carried from the prior sweep — none of these were closed. Highlights:

- `quick-shell/src/shell/quick_settings.rs` — no `#[cfg(test)]` block; only indirect coverage via `shell_chrome_test::quick_settings_state_defaults_and_mutates_through_chrome`. No layout/golden test (live clock blocks pinning a snapshot).
- `quick-shell/src/shell/topbar.rs` — no tests (vs. `topbar_chrome.rs` which has state-plumbing tests). Workspace-rename keyboard flow (Esc / Enter) untested.
- `quick-shell/src/shell/sidebar_wheel.rs` and `sidebar_wheel_disc.rs` — only the snapshot in `tests/sidebar_wheel_disc_snapshot.rs`. No spoke-focus arithmetic test, no mid-rotation workspace-count-change test, no spoke keyboard navigation test.
- `quick-shell/src/shell/workspace_drop.rs::apply_workspace_drop_from_path` — no fixtures for unreadable / non-existent paths or symlink loops; no test for the active-workspace-mid-close-fade case.
- `quick-shell/src/shell/persist.rs`, `state.rs` — no in-module tests. CRDT round-trip only proven indirectly.
- `quick-shell/src/shell/system_monitor.rs` — 7 tests sample produced strings but don't pin threshold colours or `/proc` failure paths.
- `quick-shell/src/shell/{remote_cursors,search,split_row,tile,desktop_backdrop,notes_app,editor_app,chrome_state,nirios_tokens,browser_stub,overview}.rs` — no unit tests; partial indirect coverage.
- `quick-ui/src/widget/command_palette.rs` — activation tested, but no e2e test for Cmd+K → palette → "Overview" → `OverviewToggle` consumed by shell.
- `quick-ui::screenshot_test` — only tab bar has a pixel-sample golden. Window group, sidebar wheel disc, topbar chrome, overview overlay have state-only journeys, no rasterised goldens.
- `apps/terminal` — 7 lib tests only. No PTY round-trip, no GPU shader fallback, no selection / clipboard tests, no `tests/` directory.
- `apps/file-explorer` — solid parity but no trash flow, rename flow, or "metadata-present-content-not-yet-hydrated" FUSE-projection test.
- `quick-fs-fuse` — one tokio integration test that never runs because the crate doesn't build; would not cover hydration, cache eviction, 10s metadata TTL, or peer-unreachable behaviour anyway.

## Time-sensitive / flaky tests

- `quick-shell/src/shell/topbar_chrome.rs:211` and `:622` — `Local::now().format("%H:%M")` called from inside `draw`. Any pixel snapshot mismatches as the wall clock crosses a minute boundary. Until injectable (e.g. a `Clock` trait through `Theme`), the quick-settings golden cannot be pinned.
- `quick-shell/tests/wayland_test.rs::test_wayland_handshake_fixed` — depends on `alacritty` on `PATH`. Environmentally fragile.
- `quick-shell/tests/interaction_test.rs::test_click_interaction_in_sandbox` — needs bwrap. Times out at 20s when bwrap isn't usable; that 20s wait makes it the slowest single test even on success.
- `quick-shell/tests/e2e_shell_flows.rs` — historically 17.86s wall time; this run took ~6s under `--test-threads 1`, ~3s under default parallelism. Contains `Duration::from_secs(...)` somewhere; under load it can edge close to its budget. Parallel runs of this binary hit a wgpu SIGSEGV (pre-existing, unrelated to nirios).
- `quick-shell/tests/apps_integration.rs` — 17.44s wall time; nothing we can do about it.
- `quick-ui/tests/screenshot_test.rs::tab_bar_renders_pills_inside_six_px_insets` — currently failing; hardware-/AA-sensitive. Strict pixel equality on a vello-rendered scene → backend changes (lavapipe vs. llvmpipe, AA mode default) can flip the result. Replace with a tolerance window.

## How to run

All commands run inside `distrobox enter my-distrobox -- bash -lc '<cmd>'` with `RUSTC_WRAPPER=` to override the host's broken sccache config. Do **not** run a workspace-wide `cargo test` (per the project's `CLAUDE.md` rule about the shared target lock).

```bash
# Workspace-resolution prerequisite: temporarily exclude apps/browser
# (libservo no longer resolves). Revert before commit.

cd <worktree-root>

# quick-ui
RUSTC_WRAPPER= cargo check  -p quick-ui
RUSTC_WRAPPER= cargo clippy -p quick-ui --all-targets
RUSTC_WRAPPER= cargo test   -p quick-ui --lib
RUSTC_WRAPPER= cargo test   -p quick-ui --tests --no-fail-fast

# quick-shell (compositor)
RUSTC_WRAPPER= cargo check  -p quick-desktop
RUSTC_WRAPPER= cargo clippy -p quick-desktop --all-targets
RUSTC_WRAPPER= cargo test   -p quick-desktop --lib
RUSTC_WRAPPER= cargo test   -p quick-desktop --tests --no-fail-fast
# e2e binary — must run single-threaded; parallel hits a wgpu SIGSEGV.
RUSTC_WRAPPER= cargo test   -p quick-desktop --test e2e_shell_flows -- --test-threads 1
# Per-binary spot checks:
RUSTC_WRAPPER= cargo test   -p quick-desktop --test shell_state_test
RUSTC_WRAPPER= cargo test   -p quick-desktop --test nirios_e2e_test

# quick-fs-fuse — currently fails to build offline; skip until the
# ed25519/iroh stack is unstuck.
RUSTC_WRAPPER= cargo check  -p quick-fs-fuse   # currently errors

# Apps
RUSTC_WRAPPER= cargo check  -p quick-terminal
RUSTC_WRAPPER= cargo clippy -p quick-terminal --all-targets
RUSTC_WRAPPER= cargo test   -p quick-terminal --lib

RUSTC_WRAPPER= cargo check  -p file-explorer
RUSTC_WRAPPER= cargo clippy -p file-explorer --all-targets
RUSTC_WRAPPER= cargo test   -p file-explorer --lib
RUSTC_WRAPPER= cargo test   -p file-explorer --tests

# quick-browser: skip. Will not resolve libservo offline.
```

To pin a screenshot for visual diffing, set `QUICK_UI_SCREENSHOT_DIR` before running `screenshot_test`, e.g. `QUICK_UI_SCREENSHOT_DIR=/tmp/shots cargo test -p quick-ui --test screenshot_test`. PNGs land in that directory.
