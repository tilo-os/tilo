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
