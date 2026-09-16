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

The final integrated wizard review found hidden partial-scan outcomes and
colliding destinations for disjoint same-basename sources. Commit `b0768af20`
fixes both: skipped paths/reasons and incomplete totals persist in folder cards
and Review, while completion-time suffixes allocate distinct suggestions and
the controller rejects overlapping destinations. macOS controller/view tests
passed 7/14 respectively; native Fedora setup passed all 61 tests. Compact and
wide warning screenshots were inspected, and the final scoped re-review approved
the component. Actual target-home mapping and filesystem grants remain pending
executor work; these suggestions do not authorize writes.

## Fresh folder copy engine

Commit `4d548fb74` adds real, unprivileged copying into an exclusive fresh
folder. Fifteen real-filesystem tests pass on macOS ARM64 and Fedora ARM64:
verified bytes, empty directories, unchanged originals, cancellation,
conflicts, changed sources, corrupted staging files and timestamp handling.
The setup suites passed 59 tests on macOS and 58 on Fedora after the shared
no-follow helper extraction. Native Fedora session `48855` exited zero without
warnings. This is not yet the resumable engine or the UI import flow.

Independent task review found two destination-namespace issues:
a writable parent can allow substitution before the newly created directory's
identity is recorded, and name-based publication/cleanup can race destination
writers. Commit `c66ec996c` adds full-ancestry permission checks, private creation
and platform-specific ACL/filesystem validation. Scoped re-review approved both
fixes with no new important regressions.
The protection excludes malicious same-account and administrator processes;
unknown destination security semantics fail closed without changing permissions
on existing parents or source folders.

After these fixes, macOS setup tests passed 69 tests, with one explicitly ignored
root-only fixture. Native Fedora setup passed 65 tests before that fixture was
added. The exact-source copy suite passed 19 tests each on XFS and a disposable
Btrfs loop volume. The root-only foreign-owned sticky-child test separately
passed on Fedora; its only ownership change was inside a disposable fixture.
All loop mounts and fixtures were cleaned up. Scoped native and Windows-target
Clippy checks passed; repository lint retains documented unrelated warnings.

The exact migration and no-follow modules, including Windows-gated tests,
compile for `x86_64-pc-windows-msvc` in an isolated temporary crate. After adding
the security modules and three Windows ACL tests, its 77
dependency identities/checksums were compared against repository lock entries.
This proves only the isolated Windows Rust compilation. A full locked Imager
cross-check failed in the `ring` C build because the macOS host lacks Windows
SDK headers (`assert.h`); native Windows execution and full-app verification
remain outstanding.

## Durable copy recovery

Commit `4c8fcd403` adds private versioned journals, verified resume, persistent
directory outcomes and conflict preservation. Real subprocess exits exercise
file publication and journal replacement boundaries; changed or unrecorded
output remains intact. macOS setup tests passed 91 tests; native Fedora setup
passed 88. The disposable Btrfs copy/recovery suite passed 42 tests, and a
separate subvolume/snapshot identity test passed. Required fixture-only tests
are explicitly ignored in ordinary runs. All Btrfs loop fixtures were removed.

The Btrfs test initially passed through a same-boot device-number fallback.
A strengthened assertion correctly failed before native filesystem/subvolume
identity was added. Native probes establish the final path uses Btrfs-specific
identity rather than assuming generic UUID ioctl support. These checks do not
constitute an actual reboot or physical power-loss test.

Windows cfg compilation and scoped Clippy passed. Windows journaled copying
currently refuses before mutation pending a verified native durability adapter;
the existing nonjournal API remains available. The original complete JSON
snapshots had a 16 MiB bound and quadratic cumulative write cost. The final
component fix below replaces that format with an incremental journal.

Task review found that checkpoint failures could allow subsequent publication
and discard the accumulated report. Commit `e308647ff` stops traversal on typed
persistence failure and retains the incomplete report, distinguishing retained,
uncertain and unavailable checkpoints. The amended macOS component suite passed
49 tests and native Btrfs passed 47; a final diagnostic-only change passed its
targeted regression on both platforms. Scoped re-review approved the fix.

A bounded native Fedora XFS optimized-build measurement copied 100 one-KiB files
in 182 ms and 1,000 in 2,181 ms, with final journals of 85,814 and 842,713 bytes.
Both reports were complete. This is local scale evidence, not an unrestricted
folder-size or physical-storage guarantee.

## Folder-import UI integration checkpoint

The initial worker/store/import-view integration passed the host Imager library
suite (244 passed, one explicitly ignored) and native Fedora ARM64 setup suite
(108 passed, two explicitly ignored). The native command was
`cargo test --locked -p imager --lib setup::`; it completed without warnings.
This includes actual-worker cancellation, journal retry, application reopening,
and complete versus incomplete result handling.

Eight initial screenshots used real worker outcomes for progress, a skipped
symbolic link, a modified destination followed by resume, and completion. Visual
inspection requested simpler user wording, an optional technical-details view,
and current import status before the empty new-import form on compact windows.
The amended eight renders at 640×640 and 1180×820 passed visual inspection;
the four focused import-view tests also passed natively in Fedora. Independent
review then found an unreachable setup entry and stale counts during retry;
commit `28888974b` fixes both and passed scoped re-review. The amended native
setup suite passed 109 tests with two explicit fixture-only ignores. These
checks do not establish native Windows support or first-login/Files integration.

## Folder-copy final component fixes

Commit `0cbf9635f` replaces whole-journal rewrites with bounded, sequenced,
checksummed append records and indexed receipt lookup. Valid old journals are
converted only after root and location validation. Recovery rejects complete
corrupt records and repairs only an incomplete final record. The operation index
keeps counts and previews; immutable detail files retain all issue reasons.
Background checkpoint writes and coalesced progress keep the UI responsive, and
an OS-held store lock prevents two app instances from overwriting restart state.
The lock remains held through queued writes and live workers after UI closure.

A native Fedora XFS release workload copied 25,000 real long-named files. It
stopped after 20,000 verified files with a 32,899,577-byte journal, then resumed
in a new process to completion with a 41,125,876-byte journal. A third process
preserved an edited destination while verifying the other 24,999 files. Every
source and destination was checked, and the disposable fixture was removed.
This establishes orderly process restart beyond the former size ceiling; it
is not physical power-loss evidence.

The amended native Btrfs suite passed 52 tests with two explicit fixture-only
ignores. The integrated Fedora setup suite passed 125 tests with two ignores;
the final report-identity changes then passed all 14 focused store tests.
Final host checks passed 14 store tests, eight import-view tests and strict
library Clippy. The broader app test-target Clippy command still reports four
unrelated concurrent main.rs diagnostics; it is not claimed green.
Windows compilation of the exact engine, journal, worker and store test sources
passed, with seven harness-only unused-code warnings because UI callers are
excluded. Native Windows execution remains outstanding.

The actual-worker issue-page scenario shows all 40 issue reasons through
asynchronous pages of 32 and eight. Compact and normal renders passed visual
inspection; the regression exercises navigation through the last issue.
The single scoped final rereview passed: all three Important findings and nine
minor diagnostics were addressed, with no new actionable issues. This accepts
the copy component, subject to relevant CI; the overall app remains incomplete.

## Desktop package catalog and download

Commit `fe27e5ab0` adds an embedded, strict Desktop release catalog and an
unprivileged verified package cache. Selectors match exact distro/version and
architecture; runtime evidence is bound to the package digest. The production
catalog remains explicitly unreleased and empty. The generator measures actual
local package bytes and requires supplied native-session evidence.

Downloads use private exclusive staging, same-origin transport, bounded resume,
size/digest verification and no-replacement publication. Cached content is
revalidated, and a replaced cache directory is refused before returning a path.
The later privileged helper must independently validate the package again.

All 18 focused Rust tests and five generator tests passed on macOS and native
Fedora ARM64. The host Imager library suite passed 281 tests with one explicit
ignore, including all nine unchanged media-download tests. An exact-source
Windows check passed for the catalog and non-Unix refusal branch only; it
excluded TLS transport and does not establish a full native Windows build.
Independent Task 1 review approved spec compliance and code quality with no
findings. Host readiness, installation and graphical-session proof remain open.

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

The Fedora ARM64 test VM was restored after an external suspension. Podman
5.8.4 and its required dependencies were installed from configured Fedora
repositories (14 additions, no upgrades or removals). The system reports running
with no failed units. Rootless Podman reports the overlay driver using the
per-command `--cgroup-manager=cgroupfs` option for the non-login test session.
QEMU is available, but the configured qOS disk/installer images are still absent;
these prerequisites do not establish an actual internal boot or installation.
The exact pinned base manifest requires private-registry authentication; the
native manifest probe confirmed this without pulling an image.

RPM build tooling is now available as ordinary-user `rpmbuild` 6.0.2. Fedora
installed six new required packages with no upgrades or removals (541 KiB
download, 2 MiB installed). No qOS package or desktop-session change was made;
a package installation and graphical-session proof remain required.

Fresh native release builds of the launcher, unlock helper and Desktop shell
completed, and RPM assembly produced `qos-session-0.1.9-1.fc44.aarch64.rpm` in
`/home/parallels/qos-desktop-package.UhBqUX/RPMS/aarch64/` inside Fedora. Its SHA-256
is `39fdb1f7fd5baa646d4cd31b1f521f7cf66f44a9dbf814b91efbb4d015ed9a3a`.
The shell build emitted one existing `restore_documents` dead-code warning.
Extraction checks passed for the package layout, ARM64 executables, resolved
shared libraries and the host-session wrapper. A read-only DNF proposal resolved
the package and five dependencies with no upgrades or removals. Nothing was
installed. This local artifact is not a signed release or graphical login proof.

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
