# Task 7 — Native fullscreen report

## Scope and baseline

- Branch: `codex/macos-shell`
- Starting HEAD: `e1537202a945565fb6f01fbc01c0e5ca5e4225b2`
- Binding inputs read: `task-7-brief.md`, `task-7-preflight.md`, `task-7-parent-qa.md`
- Native artifact hold: released by parent before implementation.
- GUI acceptance remains parent-owned; this worker does not launch the macOS app.
- Known first-navigation/paint/shutdown stalls remain outside Task 7 and are not patched here.

## TDD log

### Cycle 1 — Rust Chromium fullscreen protocol

- RED test added: `fullscreen_protocol_decodes_valid_edges_and_rejects_malformed_known_events`.
- Covers true/false fullscreen edges, payload-free renderer termination, malformed-known-event rejection, and exit command encoding.
- RED command: `cargo test -p ui fullscreen_protocol_decodes_valid_edges_and_rejects_malformed_known_events --lib`
- RED result: compile failed as expected with missing `WebViewEvent::FullscreenChanged`, `WebViewEvent::RendererTerminated`, and `WebViewCommand::ExitFullscreen` variants.
- GREEN command: `cargo test -p ui fullscreen_protocol_decodes_valid_edges_and_rejects_malformed_known_events --lib`
- GREEN result: 1 passed, 0 failed (3407 filtered); linker emitted the pre-existing large `__eh_frame` warning.
- Follow-up RED test added for latest-edge coalescing while preserving renderer termination under ordinary progress noise; the state matcher was temporarily removed before running it.
- Coalescing RED command: `cargo test -p ui fullscreen_state_events_coalesce_without_dropping_renderer_termination --lib`
- Coalescing RED result: failed as expected with both `FullscreenChanged(true)` and `FullscreenChanged(false)` retained instead of only the latest false edge; renderer termination and final progress remained present.
- Coalescing GREEN is included in `cargo test -p ui fullscreen --lib`: the latest false edge, renderer termination, and final progress were retained in order.

### Cycle 2 — Per-tab fullscreen state and exit routing

- RED tests added: `fullscreen_events_and_renderer_loss_drive_tab_state`, `exiting_content_fullscreen_is_idempotent_and_routes_native_command_once`.
- Covers true/false CEF edges, renderer-loss restoration, structure invalidation, and exactly-once native exit routing.
- RED command: `cargo test -p browser fullscreen --lib`
- RED result: compile failed as expected because the content fullscreen field/accessors/exit method and new event match arms did not exist.
- GREEN command: `cargo test -p browser fullscreen --lib`
- GREEN result: 2 passed, 0 failed (188 filtered); two expected temporary dead-code warnings identify the not-yet-wired BrowserTabs callback path.

### Cycle 3 — Root reconciliation, immersive chrome, hint, Escape, and lifecycle

- RED tests added for root fullscreen intent, actual-window acknowledgement, both chrome layers, hint visibility, early Escape interception, ordinary OS fullscreen, tab switch, and active close.
- RED command: `cargo test -p browser fullscreen --lib`
- RED result: 2 existing per-tab tests passed and 3 new root/lifecycle tests failed because `BrowserTabs::fullscreen_intent` still returned `None`; the exact three-second helper was then added before production wiring.
- A later bounds assertion caught a zero-height hint under absolute layout; the overlay was given an explicit 48 px safe-area row before re-running GREEN.
- GREEN command: `cargo test -p browser fullscreen --lib`
- GREEN result: 6 passed, 0 failed (188 filtered), with fullscreen surface bounds `1000x700`, both chrome layers absent, and a non-zero top-inset hint asserted.

### Cycle 4 — Native CEF fullscreen ownership

- RED test added: `fullscreen_state_test.cc` for edge-only fullscreen state and renderer-termination reset.
- RED command: `clang++ -std=c++20 -Wall -Wextra -Werror -I apps/browser/chromium/src apps/browser/chromium/tests/fullscreen_state_test.cc -o /tmp/qos-fullscreen-state-test`
- RED result: compilation failed as expected because `fullscreen_state.h` did not exist.
- GREEN command: `clang++ -std=c++20 -Wall -Wextra -Werror -I apps/browser/chromium/src apps/browser/chromium/tests/fullscreen_state_test.cc -o /tmp/qos-fullscreen-state-test && /tmp/qos-fullscreen-state-test`
- GREEN result: exit 0.
- Native build command: `python3 scripts/browser/build-host.py --platform macosarm64 --jobs 4`
- Native build result: exit 0; `qos-chromium-host`, all helper targets, protocol test, and fullscreen-state test built.
- Native focused command: `target/chromium-tools/bin/ctest --test-dir target/chromium-build --output-on-failure -R '^(protocol|fullscreen-state)$'`
- Native focused result: 2/2 passed.

## Regression verification

- `cargo test -p ui fullscreen --lib`: 17 passed, 0 failed; the linker emitted the existing large `__eh_frame` warning.
- Fresh final `cargo test -p browser --lib` after the hint-row correction: 191 passed, 0 failed, 3 ignored.
- `git diff --check` on every owned tracked source path: clean.

## Commits

- `7ead72f254857d38c09ce59b990e19c81df9ea8e` — `Implement native browser fullscreen flow` (owned source and tests only).

## Verification and artifacts

- Required single optimized Rust build: `cargo build -p browser --profile thin-app` exited 0 in 2m00s.
- Parent candidate: `target/thin-app/browser`, 27,900,192 bytes, SHA-256 `95c99934a4d92bc4fba8fee9485ab74bfcbc435e3b1af9a70301ca5f5bf44ae3`.
- Native host: `target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host`, 8,535,168 bytes, SHA-256 `330f5211430dccdcb700fc6ddfdb937e5f0c50f65a631622a3841cca109852bf`.
- Bare `ctest` was unavailable on `PATH` (exit 127); the repository-provisioned `target/chromium-tools/bin/ctest` command above was used and passed.
- No owned source path remained modified after source commit and the optimized build.

## Limitations

- No GUI/native-window acceptance is performed by this worker per parent instruction.
- The live page/video, physical Escape, tab-switch, native window-state, and deterministic renderer-loss acceptance matrix remains parent-owned.
- The synthetic canvas-stream fixture does not establish ordinary codec coverage.
- Existing Task 5/6 first-navigation/paint/shutdown stalls remain outside this task and were not changed.
