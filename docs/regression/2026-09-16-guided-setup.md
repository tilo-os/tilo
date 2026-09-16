# Guided setup implementation evidence

This records incremental evidence for the Windows/Linux migration application.
It is not a claim that the full migration application is finished.

## Foundation model — f6ab9c6e7

- macOS host baseline before changes: `cargo test -p imager --lib
  --no-default-features`, 133 passed.
- After changes: focused setup model suite, 16 passed; full Imager library suite,
  149 passed. The implementer's report is committed with that change.
- Independent task review approved the capability gating, folder-mode semantics,
  storage accounting, and lexical path validation without actionable findings.
- Native Linux verification: Fedora 44 ARM64 Parallels VM, `uname -m` reports
  `aarch64`. Compiled the current shared worktree's
  `apps/imager/src/setup/model.rs` using the guest user's Rust compiler with
  `--edition=2024 --test`; ran the produced guest executable. All 16 tests passed.

The model requires capability evidence rather than enabling a route based on
the host name, counts both copied and synchronized local files, and rejects
overflow and lexical source/destination overlaps. Actual filesystem identity,
case folding, mount safety, and authorization remain execution responsibilities.

## Controller and folder discovery — bdde7f895

- Host setup tests: 27 passed; scoped Clippy exits 0 with an existing
  `needless_return` warning outside setup.
- Native Fedora ARM64 `cargo check --locked -p imager --lib`: passed.
- Native Fedora ARM64 `cargo test --locked -p imager --lib setup`: 26 passed,
  135 filtered out. The native-dialog stdout test is excluded on Linux. The
  shared UI crate emits an existing unused profiling-function warning.
- Task review found two issues: writable controller state bypassed its methods,
  and queued directory paths could follow a replacement symlink. Fix round 1 is
  active. The passing tests above predate those fixes and do not prove the race
  is fixed. The controller is not yet accepted for the dependent view.

## Controller review fixes — 0f9d911cf / eda70d280

- Controller page and default-mode state now have read-only getters.
- Folder enumeration pins directory handles and uses descriptor-relative
  no-follow traversal with identity checks.
- macOS focused setup tests: 28 passed; scoped Clippy exits 0 with the existing
  unrelated `model.rs:285` warning.
- Native Fedora ARM64 `cargo test --locked -p imager --lib setup`: 27 passed,
  zero failed, 135 filtered. This includes the deterministic test replacing a
  queued directory with a symlink.
- Scoped re-review approved both fixes with no directly introduced regressions.
  Native Windows verification remains outstanding.

## Setup screens — Task 2 verification

- Added a development `--setup` entry and five deterministic setup screenshot
  scenarios. Live installation and synchronization remain unavailable pending
  their real executors; screenshots do not establish installation support.
- macOS full Imager tests: 172 library tests and 2 binary tests passed before
  the final `/var/home` guard adjustment. After that adjustment, all 8 view tests
  passed, including system-folder rejection and overlapping-selection handling.
- Native Fedora ARM64 full Imager library suite: 173 passed, zero failed before
  that final adjustment. The final focused view suite passed all 8 tests,
  including accepting `/var/home/qos/Documents` while rejecting system folders.
- The native Linux executable rendered `setup-folders-copy` at 640x640 using
  `cargo run --locked -p imager --bin imager -- --shots <output.png>
  --scenario setup-folders-copy --viewport 640x640`. The inspected image shows
  all three modes, Help, status and navigation without overlap. VM locale and
  unavailable graphics-driver probes logged diagnostics before the usable
  Vulkan adapter rendered successfully; the process exited zero.
- Ten macOS renders cover all five scenarios at 1180x820 and 640x640. Root
  inspection led to compact mode cards, a scrollbar gutter, plain availability
  wording and an opaque theme background. The corrected compact folder view and
  review view were inspected successfully.
- Screenshots are in the task's local visualization directory under
  `guided-setup/`. Task-scoped review and fix re-review are approved.

## Guided setup review fixes

Commit `2fdba6dbc` preserves explicit folder overrides when the common policy is
Later, rejects protected or overlapping sources before scanning, recovers from
worker-launch failures, and labels inherited versus explicit folder policies.
Real Enter/Space widget tests exposed and corrected a headless/live activation
mismatch. macOS controller tests (6), view tests (12), and focused headless
activation tests (2) passed. Native Fedora setup tests excluding the separately
in-progress migration module passed all 43 tests with no warnings. Scoped
re-review found no remaining issues. Native Windows runtime testing is pending.

## Fresh folder copy engine

Commit `4d548fb74` adds real, unprivileged copying into an exclusive fresh
folder. Fifteen real-filesystem tests pass on macOS ARM64 and Fedora ARM64:
verified bytes, empty directories, unchanged originals, cancellation,
conflicts, changed sources, corrupted staging files and timestamp handling.
The setup suites passed 59 tests on macOS and 58 on Fedora after the shared
no-follow helper extraction. Native Fedora session `48855` exited zero without
warnings. This is not yet the resumable engine or the UI import flow.

Independent task review found two unresolved destination-namespace issues:
a writable parent can allow substitution before the newly created directory's
identity is recorded, and name-based publication/cleanup can race destination
writers. Fixes are required before accepting this component. Passing existing
tests does not establish those untested guarantees.

The exact migration and no-follow modules, including Windows-gated tests,
compile for `x86_64-pc-windows-msvc` in an isolated temporary crate. Its 41
dependency identities/checksums were compared against repository lock entries.
This proves only the isolated Windows Rust compilation. A full locked Imager
cross-check failed in the `ring` C build because the macOS host lacks Windows
SDK headers (`assert.h`); native Windows execution and full-app verification
remain outstanding.

## Existing installer baseline

Native Fedora ARM64 ran `bash os/tests/installer_target_invariants.sh` and
`bash os/tests/installer_boot_menu.sh`; both exited zero. The first performs
static boundary checks, including live-medium refusal and alongside partition
protection. The second uses temporary fake EFI files and an injected firmware
command to test menu ownership, cleanup, and BootOrder/BootNext restoration.
These are baseline checks, not real retained-OS boot or USB-free installation
proof. No real disk or firmware state was changed by these runs.

`bash os/tests/installer_guided_alongside.sh` also passed natively in Fedora.
It created a temporary sparse disk and loop device, added exactly two partitions
through the real guided partition helper, and verified the original partition's
bytes and GPT geometry remained unchanged. Its two expected retry refusals
reported that the map changed; the overall test exited zero. The fixture detached
its loop device and removed its temporary files. This exercises partition writes
on a disposable file, not a real retained OS or an internal-installer boot path.

## Outstanding end-state evidence

- Native Windows build, UI, folder picker, package, and retained-Windows boot.
- Linux interactive wizard, desktop-session installation, and complete boot flow.
- Internal installer bundle and same-disk protected-partition protocol.
- Host-side storage preparation, encryption handling, and recovery across crashes.
- Real copied files, resumable verification, conflicts, recoverable deletions,
  and two-way connections across boot sessions.
- First-login continuation, boot selection, default switching, signed release
  availability, and representative physical-hardware verification.

No Windows VM is currently listed by Parallels. The bundled Imager release
catalog is `unreleased` with no payload entries. These are current verification
and distribution gaps, not reasons to replace the requested native app with a
mock or to claim completion from model tests.
