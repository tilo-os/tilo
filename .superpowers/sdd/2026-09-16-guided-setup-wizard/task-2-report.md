# Task 2 report: retained guided setup screens

## Changes

- Added `SetupApp`, a retained three-step setup flow for experience, folder policy, and review. It uses process-local commands, existing UI widgets, accessible names, semantic button activation, a live status region, and a scrollable body with persistent navigation.
- Added capability-injected construction for deterministic tests and screenshots. The live constructor leaves Desktop, Full qOS, Connected folders, and installation unavailable until their platform implementations can provide evidence.
- Added Copy, Connected, and Decide later choices; Help me choose guidance; Documents, Desktop, Pictures, and Downloads suggestions; native Add folder; asynchronous scanning; counts, sizes, destinations, removal, and per-folder policy overrides.
- Added native known-folder discovery. Windows requests shell-known folders through PowerShell with UTF-8 console output before strict parsing. Linux requires an explicit `user-dirs.dirs` entry before accepting `xdg-user-dir`, rejects `$HOME` fallback results, and only returns existing directories.
- Added source guards that reject operating-system folders and overlapping selections while allowing user data beneath `/var/home` on Fedora atomic layouts.
- Added a review page with retained OS, experience, folder policies, estimated space, initial existing-OS default, and current blockers. Installation remains disabled with plain user-facing reasons.
- Wired the flow behind `--setup`, added deterministic setup shot scenarios, and preserved the existing live media-writing and shot routes.
- Painted an opaque root background and tightened compact folder rows so all three choices, guidance, status, and navigation are visible at 640x640.

## TDD and verification

- Baseline: `cargo test -p imager setup::` — 28 passed, 0 failed.
- Initial interaction tests failed to compile before the view types and commands existed, providing the first RED state.
- Guard RED evidence:
  - Linux full library run: 171 passed, 2 failed (`system_folder_scan_is_refused`, `overlapping_scan_is_deduplicated_with_an_explanation`).
  - macOS `/var/home` regression: `var_home_user_folder_is_accepted` failed with 0 folders before the `/var/home` exception.
- macOS full suite before the final `/var/home` exception: `cargo test --locked -p imager` — library 172 passed; binary 2 passed; doc tests 0; exit 0.
- macOS final focused suite: `cargo test --locked -p imager --lib setup::view::tests::` — 8 passed, 0 failed; includes semantic activation, source guards, retained navigation state, folder-policy review, and worker failure handling.
- Fedora ARM64 full library suite before the final `/var/home` exception: `cargo test --locked -p imager --lib` — 173 passed, 0 failed; exit 0.
- Fedora ARM64 final focused suite: 8 passed, 0 failed, 166 filtered; includes `/var/home/qos/Documents` acceptance; exit 0 with no warnings.
- Final lint: `cargo clippy --locked -p imager --all-targets --no-deps -- -D warnings -A clippy::needless-return -A clippy::needless-borrow -A clippy::float-cmp` — passed. The three allowances cover findings in pre-existing Imager lines outside this task's diff; the new setup code has no remaining lint findings.
- Formatting and patch hygiene: direct `rustfmt` on the five owned Rust files and `git diff --check` on the owned paths passed.

## Screenshot verification

Rendered and inspected all five deterministic scenarios at 1180x820 and 640x640:

- `setup-experience`
- `setup-folders-copy`
- `setup-folders-connected`
- `setup-folders-later`
- `setup-review`

Artifacts are in `/Users/xaver/.codex/visualizations/2026/09/16/01a0aaae-f9fd-7760-8e38-cc0eb4ac8ed1/guided-setup/`. Inspection covered wrapping, selection feedback, visible navigation, status/error containment, scroll gutters, opaque background, and compact overflow. The final 640x640 folder screen shows all three choices, Help me choose, status, and navigation without initial scrolling. The 1180x820 screens keep content clear of the scrollbar.

Fedora ARM64 also rendered `linux-setup-folders-copy-640x640.png` through the native executable. The image was inspected as clean with native fonts and the full compact flow visible. The VM logged unavailable DZN/EGL probes and a locale fallback before selecting a usable Vulkan adapter; rendering completed successfully, so no source change was warranted.

## Concerns and remaining work

- Live capabilities deliberately remain false. The UI does not claim that Connected folders, installation, migration, or synchronization work before the corresponding executor and release evidence exists.
- Windows known-folder logic is covered by parser and platform-neutral behavior tests, but this task did not execute a native Windows build or folder dialog.
- The overall guided migration objective still requires real installation, migration, synchronization, packaging, and Windows/Linux release verification outside this UI task.

## Round 1 review fixes

- Corrected common `Later` semantics. Inherited and explicit `Later` entries remain remembered but are excluded from the effective plan; explicit Copy and Connected overrides remain scheduled and visible on Review.
- Rejected protected and overlapping sources before incrementing pending work or launching enumeration, while retaining completion-time checks for concurrently arriving scans.
- Added an injectable worker launcher and handled both known-folder and scan thread-launch errors. Failure now clears the matching pending state, reports an error, and stops idle sync instead of waiting forever.
- Added visible per-folder policy state: the effective button is primary and labeled `Common: …` for inherited choices or `Selected: …` for explicit overrides; the common-choice reset is disabled and labeled accordingly while already inherited.
- Replaced the misleading keyboard test with real focused Enter and Space events, retained separate assistive-technology activation coverage, and added headless host parity for the live App's focused-control fallback. The parity tests prove one activation per key press, no activation on release, and no second activation when a widget consumes the press.

### Round 1 TDD and verification

- Controller RED: `common_later_preserves_explicit_copy_and_connected_overrides` expected two scheduled overrides and received zero before the effective-mode filter.
- Headless RED: `enter_and_space_activate_a_focused_button_once_per_press` expected one activation after Enter and received zero before the FrameHarness fallback.
- macOS controller: `cargo test --locked -p imager --lib setup::controller::tests::` — 6 passed, 0 failed, 178 filtered.
- macOS view: `cargo test --locked -p imager --lib setup::view::tests::` — 12 passed, 0 failed, 172 filtered. This includes deterministic launch failure, pre-scan guards, real Enter/Space, assistive activation, and selection feedback.
- macOS headless parity: the two exact `ui` tests `enter_and_space_activate_a_focused_button_once_per_press` and `consumed_enter_does_not_run_the_focused_activation_fallback` each passed; the macOS linker emitted its existing oversized `__eh_frame` warning.
- Fedora ARM64: `cargo test --locked -p imager --lib setup:: -- --skip setup::migration` — 43 passed, 0 failed, 142 filtered; exit 0 with no warnings. The paused migration task remained intentionally excluded.
- Imager lint: `cargo clippy --locked -p imager --lib --no-deps -- -D warnings -A clippy::needless-return -A clippy::missing-errors-doc` — passed. The allowances cover an existing model finding and the separate paused migration file. A strict `ui` package Clippy run reached 91 findings in unrelated concurrent UI work and reported none in `ui/src/headless.rs`; those files were not changed in this task.
- Formatting and patch hygiene: direct `rustfmt` on `controller.rs`, `view.rs`, and `ui/src/headless.rs`, plus `git diff --check` on owned paths, passed.

## Final integrated wizard review fixes

- Successful scans with skipped entries now remain visibly incomplete. The folder card and Review show the skipped count, each skipped path, and its reason; the review total explicitly covers counted files only. These warnings persist when status text changes and after navigating to Review and back.
- Completed scans now receive stable collision-free suggested destinations. The first basename keeps its original suggestion and later collisions receive ` (2)`, ` (3)`, and so on. The controller also rejects duplicate, ancestor, and descendant destinations at its boundary using a platform-neutral lexical check.
- Replaced the panic-based worker-launch test spy with the existing deterministic error-returning spy. The pre-launch guard assertions still prove rejected sources never increment pending work or reach a successful launch, without triggering `clippy::panic_in_result_fn`.

### Final review TDD and verification

- Warning RED: `partial_scan_warnings_persist_on_folder_and_review_screens` failed because a successful partial scan still reported `ready to copy` and exposed no warning semantics.
- Destination RED: `concurrent_same_basename_completions_receive_distinct_destinations` received one remembered folder instead of two because both scans proposed the same destination. The controller boundary regression likewise accepted a duplicate destination before the guard.
- macOS controller: `cargo test --locked -p imager --lib setup::controller::tests::` — 7 passed, 0 failed, 188 filtered; exit 0.
- macOS view after the final wording and lint fixes: `cargo test --locked -p imager --lib setup::view::tests::` — 14 passed, 0 failed, 181 filtered; exit 0.
- Fedora ARM64 final setup suite after the singular wording correction: 61 passed, 0 failed, 135 filtered; exit 0 with no warnings.
- Test-inclusive Imager lint: `cargo clippy --locked -p imager --lib --tests --no-deps -- -D warnings -A clippy::needless-return -A clippy::needless-borrow -A clippy::float-cmp` — passed. A strict run first identified and led to fixes for all new `view.rs` findings; the allowances cover existing findings in `model.rs` and `main.rs` outside this task's owned diff.
- Formatting and patch hygiene: `cargo fmt -- apps/imager/src/setup/controller.rs apps/imager/src/setup/view.rs` and `git diff --check` on the two source files and this report passed.

### Final warning layout evidence

- Compact: `/Users/xaver/.codex/visualizations/2026/09/16/01a0aaae-f9fd-7760-8e38-cc0eb4ac8ed1/guided-setup/final-review-warning-640x640.png`
- Wide: `/Users/xaver/.codex/visualizations/2026/09/16/01a0aaae-f9fd-7760-8e38-cc0eb4ac8ed1/guided-setup/final-review-warning-1180x820.png`

Both renders completed successfully and were inspected on macOS and by the root reviewer. Skipped-entry warnings, incomplete totals, and navigation remain legible at both sizes; compact overflow scrolls without overlapping the sticky status and navigation.

The new destination check remains a lexical planning guard. Actual destination existence, user-home mapping, grants, and filesystem authorization remain the responsibility of the future executor.
