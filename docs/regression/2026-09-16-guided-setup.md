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
  including accepting `/var/home/tilo/Documents` while rejecting system folders.
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

## Linux Desktop readiness

Commit `dbb79c55a` adds a read-only background host probe and exact release
selection. It checks distribution/version, native architecture, package manager,
existing package version, session chooser support, immutable-host markers and
confinement evidence. Unknown evidence blocks installation; derivatives do not
inherit support from ID_LIKE. A different installed version is reported without
performing an implicit upgrade or downgrade.

Native Fedora exposed that an ordinary user cannot rely on
`systemd-detect-virt --chroot`: it returned a permission error. The revised probe
compares strictly parsed self/PID1 root mount identities alongside the other
confinement checks. Missing, unreadable or inconsistent evidence still blocks.
A reproduced FIFO-read hang was also fixed with nonblocking evidence reads and
regular-file checks.

The final native run passed 14 regression tests and ran the actual host probe.
It reported Fedora 44/aarch64, DNF, a mutable host reported as not sandboxed, active GDM with
GNOME session entries and no installed Tilo package. The only readiness blocker
was the empty authenticated release catalog. Host tests passed 295 library tests
with one explicit ignore; Windows compilation of these exact module sources
also passed. Strict native library Clippy still reports 18 findings in earlier
modules and none in this probe; aggregate lint is not green. Independent review
approved spec compliance and code quality. Package installation and graphical
login remain unverified.

## Native package trust preparation

A separate Fedora ARM64 proof VM was cloned during an idle window; the original
VM was restarted and verified running. The clone has RPM signing/repository test
tools, and still has no Tilo session package installed.

Native DNF 5.4.4 tests used a temporary signed RPM and repository in disposable
install roots. A fixed transaction-local empty `gpgkey` override preserved
configured signature checks and refused unknown keys for a local RPM, repository
metadata and a repository package. Each refusal left that install root's keyring
and package state unchanged. The control without the override imported the test
key and installed the valid fixture only inside a separate disposable root.
The proof guest's system keyring was unchanged, and temporary keys/artifacts were
removed. These are package-trust tests, not Tilo installation or graphical login
proof. The subsequent real Tilo transaction is recorded below.

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
QEMU is available, but the configured Tilo disk/installer images are still absent;
these prerequisites do not establish an actual internal boot or installation.
The exact pinned base manifest requires private-registry authentication; the
native manifest probe confirmed this without pulling an image.

RPM build tooling is now available as ordinary-user `rpmbuild` 6.0.2. Fedora
installed six new required packages with no upgrades or removals (541 KiB
download, 2 MiB installed). No Tilo package or desktop-session change was made at that tooling step.
The subsequent package installation is recorded below; graphical-session proof
remains required.

Fresh native release builds of the launcher, unlock helper and Desktop shell
completed, and RPM assembly produced `tilo-session-0.1.9-1.fc44.aarch64.rpm` in a
temporary `rpmbuild` tree inside Fedora. Its SHA-256
is `39fdb1f7fd5baa646d4cd31b1f521f7cf66f44a9dbf814b91efbb4d015ed9a3a`.
The shell build emitted one existing `restore_documents` dead-code warning.
Extraction checks passed for the package layout, ARM64 executables, resolved
shared libraries and the host-session wrapper. A read-only DNF proposal resolved
the package and five dependencies with no upgrades or removals. Nothing was
installed by that proposal. This local artifact is not a signed release or
graphical login proof.

The verified native Desktop helper subsequently installed that exact RPM in
`Tilo Desktop Proof Fedora 44 ARM64`, a disposable clone with a pre-install
snapshot. Its production empty-catalog refusal was asserted first; a test-only
injection then permitted the local artifact while retaining every other host,
package, signature-policy and installed-session check. The real transaction
reported version `0.1.9-1.fc44` and `/usr/share/wayland-sessions/tilo.desktop`;
`rpm -V tilo-session` returned clean. Saved GNOME session entries, GDM configuration,
default target and the system signing-key list were unchanged. The original
Fedora VM was not an installation target.

The native Desktop suite passed 50 tests with two explicit native diagnostics
ignored; the actual privileged transaction test was then run separately and
passed. The host library suite passed 313 tests with one ignored. Native output
remained in a root-owned 0600 log under a 0700 operation directory; the staged
package was removed. Narrow Windows configuration compilation also passed for
the catalog, downloader, probe, helper and worker; this is not a Windows runtime
or full-app build. Native strict Imager Clippy retains 18 prior findings in
catalog/download/folder modules and no helper/worker findings. The production
Desktop catalog remains unreleased. Graphical login, identity completion,
selected-folder import and returning to the prior desktop are still unproved.

A subsequent real GDM login attempt with a disposable fixture account exposed
an actual graphical blocker in that local package: Tilo reported zero usable DRM
outputs and returned to the greeter. GNOME rendered successfully for the same
account before and after the attempt. The connected VM output is `Virtual-1`;
the existing compositor connector filter rejects names containing `virtual`
along with writeback connectors. That policy needs a focused regression, fix
and rebuilt-package verification. It is not acceptable to substitute the
passing headless startup test for graphical login evidence. Temporary GDM
autologin settings were restored to their original bytes; the original user
account was not changed and installed package verification remains clean.

- Native Windows build, UI, folder picker, package, and retained-Windows boot.
- Linux interactive wizard, graphical desktop-session lifecycle, and complete boot flow.
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


## Guided Desktop installation UI

Commits `6ab541720` and `1e2c78e35` connect the guided setup to live host
readiness, verified downloads and the native installation helper. The review
shows the exact host and package version before installation. Selected folders
retain their modes and estimated sizes while destinations remain unresolved;
that intent cannot authorize a copy. Completion explains how to select Tilo at
login and leaves the existing session default unchanged.

Independent review found and verified fixes for two reachable failures:
accepted cancellation now prevents authorization even when download success
was already queued, and the first screen displays live readiness failures and
a non-installing recheck. A same-app recovery test retains folder intent and
its explicit Later override while restoring navigation.

The initial native setup suite passed 188 tests with four explicit fixture
ignores. After the review fixes, native Fedora ARM64 flow tests passed 7/7 and
view tests 20/20 without warnings. macOS focused tests matched those results.
Compact checking, blocked and failed screenshots were visually inspected;
status and recheck remain visible above the fold. The scoped re-review found
both issues addressed and no new breakage.

This establishes the installation UI boundary. Durable first-login continuation,
packaged import delivery and the actual graphical lifecycle remain outstanding.
The production catalog stays empty. Narrow Windows compilation is not evidence
of a complete Windows application or retained-Windows boot support. Previously
recorded strict-Clippy findings remain for the final component review.

## Native virtual-display fix and graphical startup

Commit `ccc2ebae0` replaces connector-name substring filtering with typed DRM
eligibility. A connected Virtual connector with a usable mode is eligible;
Writeback, Unknown, disconnected and modeless connectors remain excluded.
Native RED reproduced the Virtual rejection; the corrected scanout suite passed
all 20 tests, and independent review approved the change.

The production compositor rebuilt successfully with default features disabled.
A frozen three-binary payload produced a local Fedora ARM64 RPM with SHA-256
`800f97c97998b992f67f16396d9546c3e06d8432e1430c5fdd1d8e2bc8036d9f`.
A guarded, explicitly test-only same-version reinstall updated the disposable
proof clone. This is runtime-artifact proof, not another execution of the
production initial-install helper. Installed shell SHA-256 is
`4dc4c892ac5e5b67401c495eda68cfa2eccb0b32ff027bf97c4f44aee0b5b4eb`;
`rpm -V tilo-session` passed, and existing desktop/default files and signing keys
were unchanged.

Actual GDM login now finds one live DRM output and renders the Tilo identity
setup screen. Selecting GNOME again renders its normal desktop. Both screens
were captured and visually inspected. The fixture's original GDM configuration
was restored byte-for-byte afterward; the original user was not modified.
This resolves the earlier zero-output startup blocker for this exact guest.

Limits remain: the local RPM is unsigned; identity creation and selected-folder
import were not completed. DNF reported transient systemd-trigger connection
warnings, and Tilo logged one initial buffer-fence timeout before the observed
successful render. Neither is hidden as a pristine-run claim. Final first-login
migration, packaged import delivery and broader distro/architecture evidence
still require the remaining Desktop tasks.

## Reviewed first-login continuation and import delivery

Commits `ee24c0f82` and `3861200a7` add bounded private continuation storage,
installed import delivery, and a read-only session readiness check authenticated
against the installed trusted shell. Pending first-login imports wait for
trusted identity completion and explicit Copy. Ordinary standalone Add folders
remains available. Copy, per-folder Later choices and real destination review
reuse the existing durable migration worker and journal.

Independent review identified two defects and approved their fixes: in-place
source changes after final review now invalidate a saved metadata revision,
and unpublished jobs retain explicit re-review/retarget controls after a
collision or destination substitution. The revision is enforced at fresh
journal creation, ordinary resume, actual copy-entry staging and final scan.
This is change detection, not a frozen content snapshot. Scans run off the UI
thread and are limited to 100,000 entries, depth 128 and relative path length
4096; exceeding those bounds asks for a smaller selection. Previously copied
valid output is retained when later work fails.

Destination recovery preserves the stable operation ID. An existing journal
prevents retargeting; eligible unpublished work requires genuine absence checks.
Both durable-write crash orderings fail closed and remain explicitly repairable.
Regression tests cover same-size edits with restored modification time, edits
during copying, ordinary resume, replaced destination parents, same-basename
folders, asynchronous review and refused-start state preservation.

The amended native Fedora ARM64 setup suite passed 211 tests with five explicit
fixture ignores; the local suite passed 211 with one ignore. Post-extraction
focused semantic and late-edit/resume checks also passed. Strict library Clippy
reported zero findings in the changed files, while 18 native and 13 local
baseline findings remain outside this fix. The narrow Windows backend compile
check includes the new inventory module and passes with ten unused-caller
warnings from omitted UI callers; it proves neither Windows runtime behavior
nor durable Windows copying. Four refreshed compact/normal screenshots were
visually checked, including the retained destination controls and disabled Copy
while waiting or choosing Later.

The host package definition now includes the import binary and desktop entry.
Native payload fixtures passed except for two explicit private D-Bus fixture
skips caused by missing dbus-daemon tools. The final four-binary package's actual
GDM, identity, import and return-to-GNOME lifecycle is still unproved. The earlier
three-binary RPM's runtime evidence does not apply to it. Login-hint recovery
for all-Later choices or interrupted completion acknowledgement remains a
tracked minor finding for final component review. Production catalogs remain
empty pending genuine authenticated releases and their required runtime proof.

## Four-binary package and native launch regression

The first four-binary Fedora ARM64 package built and passed extracted payload,
ELF dependency and importer startup checks. Its SHA-256 is
`b7ca017b3b6527fadf7791a774717464b1b4af6b19f6a073176a50dc5bd33818`.
After a disposable-VM snapshot, the older Tilo test package alone was removed
with dependency autoremove disabled. The real transaction-core fixture then
installed the new package and passed verification. Existing desktop/default
files and signing keys remained unchanged, and `rpm -V tilo-session` passed.
This is a local unsigned fixture, not authenticated release availability.

Installed shell SHA-256 was
`aac3ff153b54b7ee984832ba4e8357167d5160b9f4ff0444b969e082e7e406fd`;
installed importer SHA-256 was
`4c2b71978066e4c99a59f1819414f696c42960f16092c60485ea6bb99ad45aad`.
A restricted ordinary-user test adapter seeded synthetic Documents as Copy and
Pictures as Later through the real selection/capture/persistence APIs. Actual
GDM login rendered identity setup without launching the importer beforehand.
The UI subsequently advanced during external input-control interruptions, so
this run does not establish that the test runner completed the identity
ceremony. The live readiness service reported ready and saved choices remained
pending, but no importer launched.

That run exposed an integration defect: continuation queued a `.desktop`
filename, while the host-app registry strips that suffix from its IDs. Commit
`40466cf53` fixes the request through the existing host-app activation path.
A real scanner regression reproduced the exact mismatch before the change;
all three native continuation tests passed afterward, including the readiness
and launch-once tests. Independent review approved the narrow fix.

The package hash above predates that correction. A rebuilt package and a fresh
installed-session launch/import/return test remain required. Launch must not
start copying; destination review may create an AwaitingReview record, and
publication requires explicit Copy. No successful automatic import is claimed
from this incomplete native run.

The corrected launch-ID shell built successfully in the native Fedora ARM64 VM (11m31s). Its replacement RPM has SHA-256 `0f9ef68085eae6710db325e58bdce7d34795c67836d6351ba131d6737c8ccab5`; packaged shell SHA-256 is `a1028676c6b122d648bd09cbcc00c0827eb77ef730a3ee17e6d2356d259b0bc8`. All four binaries passed payload, architecture, importer smoke and shared-library checks. Pre-strip staging matched the release binaries exactly, and extracted loadable contents matched after normal RPM stripping. An ordinary-user, isolated-home headless run of the extracted shell reached first-login setup, painted frames and created no primary identity. The new RPM has not yet replaced the active graphical fixture; fresh installed-session autolaunch remains pending.

The frozen private D-Bus handoff fixture now passes on native Fedora ARM64 using a signature-verified Fedora `dbus-daemon` package extracted into a private test directory; no system package or bus configuration was changed. The companion private user-manager fixture still skips: its isolated systemd process exits with status 1 before exposing a private control socket. Retained diagnostics do not establish the cause, so user-manager handoff coverage remains unresolved.
