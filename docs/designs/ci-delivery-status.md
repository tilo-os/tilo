# CI/dev workflow delivery status

Goal: deliver the full CI/dev and continuous packaging workflow described in the
companion designs. Updated 2026-09-19: emergency relief is deployed with user
authorization. Broader experiments and the production roadmap remain incomplete.

## Deployed relief versus experiments

The coordinating task inspected product checkpoint `fc2d86d54` and infrastructure
checkpoint `7195b86` on 2026-09-18 at 23:45 UTC. The deployed broker image is
`8d313aa76ff57f9589defbad60e6817278547d2a6a84b204799d342349b0b0a4`.
These are observations, not permanent health guarantees or proof that every
main revision is green. The [emergency runbook](../ci-emergency-relief.md)
on main records deployment details, recovery and per-run validation; that file
may not yet exist in older experiment checkouts.

Relief stops broad per-push OS/distribution build fan-out, retains explicit and
scheduled heavy paths, disables retained macOS lanes, and coalesces only eligible
superseded heavy work. Every main commit retains check/lint work with per-SHA
groups. A reserved quality lane and qualified lean image support short feedback.
Focused contract commands, runtime handoff repair and narrow documentation
applicability are also integrated. Hardware bring-up requests remain outside the
heavy-run cleanup policy. Continuous dependency-aware packaging remains unfinished.

Routine automatic merging is still unimplemented. Task-branch/PR instructions and
private protection experiments do not create a deployed integration service.

## Latest verified progress and remaining rollout gates

- Ordinary main quality jobs used isolated trusted snapshot clones and created
  containers in 0.34–0.54 seconds after task receipt, with no pull event. A full
  main lint completed in 127.911 seconds. These measurements apply to the seeded
  on-prem workers; unseeded AWS workers still transfer/extract the images.
- The default native builder is now resolved once to an immutable manifest
  digest. Actual authenticated run 22806 verified that the API's digest equals
  the raw registry index SHA and contains x86-64/ARM64 descriptors. App packaging
  also shares one resolved index across its native architecture jobs. No
  continuous main packaging trigger was restored by this change.
- Two cloud security workers were lost; Forgejo's existing zombie-task reaper
  marked their tasks failed. Targeted job reruns then passed the complete security
  suite for main revisions `87e99138d` and `11e501209`, without restarting their
  independent architecture jobs. This supersedes the old interrupted local
  security-suite observation below, for these exact live revisions only.
- Requested disk-heavy workers now receive priority 1000 with preemption disabled.
  Manual OS run 9331 actually started at 23:40:50 UTC on the prioritized ARM worker.
  The earlier pending-pod replacement exhausted a zero-retry Job; ordinary broker
  reconciliation created the fresh prioritized worker. Active work was preserved.
- Cloud runtime inspection found QEMU in unlimited, incorrectly interpreted
  systemd-style cgroup paths. Kata guest-agent timeouts terminated affected workers;
  the underlying stall cause is not yet proven. A reviewed isolated-node canary
  pairs corrected accounting with conservative admission overhead and a real
  image-extraction workload. Its node is reserved, but existing jobs must finish
  naturally before configuration changes. No fleet runtime correction is claimed.
- AWS EBS seed support and a bounded clean-source/snapshot/clone pilot are prepared
  and disabled. Live CSI/storage prerequisites were inspected. Actual clone and
  image-reuse acceptance wait for safe runtime accounting and capacity.
- A historical 51-package artifact passed package/runtime signature and payload
  checks, and the current assembler produced identical outputs twice. This proves
  transport/assembly compatibility, not current toolkit ABI or native execution.
- The private packaging coordinator persists dispatch intent, preserves its active
  build, replaces pending requests, and advances the comparison baseline only on
  success. Ten tests cover crash/restart/concurrent callers. Its live A→latest-C
  experiment is underway; production dispatch/publication remains disabled.
- Native ARM64 mold compatibility passed a small pinned-Rust dev/thin-LTO fixture
  with ELF hardening intact. Representative memory/performance measurements and
  exact CI-image qualification remain prerequisites for changing defaults.

The latest observed main checkpoint's lint (22821) passed after 23 seconds of
queueing and 178 seconds of execution; its check run (22820) started after one
second. The private coordinator fixture used a global uncached worker and waited
221 seconds, which is not a production qOS quality-lane latency measurement.

## Implementation and experiment history

- Dependency/cache experiments and the architecture/production roadmap, including
  direct integration versus automatic PR integration tradeoffs.
- Fixed app runtime handoff: the packager now writes `runtime/tilo-runtime/...` and
  the public publisher key under the directory uploaded and assembled by the
  release workflow. Runtime files and publisher key are checksummed and archived.
- Executable regression follows the real packager, runtime-bake shell helper,
  workflow upload patterns and collection assembler. Both architecture layouts
  pass; private fixture identities are excluded; assembly invokes no Cargo and
  produces repeatable archives. Compiler, ELF and cryptographic binaries are
  fixtures, so native/signature acceptance remains unproven by this test.
- `python3 tools/dev.py check installer|app-packaging`: focused executable contracts
  with a preview plan, live output and atomic timing/result receipts. Missing tools,
  process failures and unrun steps remain visible. Receipts are diagnostic, not
  release or VM evidence. Installer materialization tests use a private user
  namespace when non-root, preserving production root-ownership validation.
- Fast-quality workflow invokes the same installer checks and retains their
  receipt. Existing policy discovery runs the app handoff regression.
- Merged the available upstream snapshot into this branch, retaining both its RPM
  scheduling checks and our focused installer checks. No main checkout was changed.
- Ordinary app/UI-only PR paths no longer trigger OS assembly; explicit image,
  boot/security and installed AT-SPI inputs retain automatic OS builds. Desktop
  PRs run preflight contracts, with distribution builds on main/explicit runs.
- Removed continuous OS publication and the desktop-to-OS release dispatch.
  OS tags and explicit manual main publication remain. RPM completion can still
  request a non-publishing integration candidate. Later deployed relief supersedes this initial branch-local
  trigger policy; see the runbook above.
- Private isolated Forgejo merge probes now demonstrate missing-status and
  expected-head rejection, exact-head fast-forward landing and refusal of a
  sequential competing stale candidate. See `experiments/ci-forgejo/README.md`
  for raw evidence and the initial inconclusive run. Product repository settings
  were unchanged by those probes; later permission evidence follows below.
- A live queued-auto-merge probe followed a changed head once that new head was
  green. A candidate-controlled Actions job also minted the required status using
  its built-in token. Native context names are insufficient as trusted readiness;
  a trusted integration boundary remains required. Both probe runs are terminal.
- Confirmed the user-supplied `claude` identity and its existing web-UI login for
  the upcoming independent-review probe. No new account or credential was created.
- The follow-up review probe passed: `claude` approved an exact head authored by
  `xemmerling`; a new commit dismissed that approval and could not merge despite
  green checks; a fresh approval allowed exact-head landing. Both accounts have
  owner access, so non-admin isolation and actual independent-agent review remain
  distinct unqualified requirements. Only private pilot protection was changed.
- A second issuer probe posted a green required status and then deliberately
  failed (exit 42). Forgejo run/job records correctly retained failure. The new
  read-only `experiments/ci-forgejo/audit-run.py` rejects that execution evidence;
  its seven tests cover missing/mismatched/non-success records. This is not yet a
  trusted readiness gate: workflow/script provenance and runner trust remain.

- The [restricted integrator pilot](../../experiments/ci-forgejo/results/integrator-permissions-20260919/README.md)
  proved non-admin `qos-integrator` could merge the approved exact fixture head,
  could not edit protection or access the product repository, and could not merge
  without approval. Direct author push and author merge were rejected. The green
  status was synthetic; author/reviewer retained administrative policy-edit power.
  Product grants, trusted CI provenance and the automatic worker remain unqualified.

## Validation of this implementation

- Installer contracts: 11 tests passed on Linux/WSL, including the non-root
  private-user-namespace path. App packaging contracts: 17 tests passed, including
  both artifact architecture layouts. The handoff regression failed against the
  original packager before the layout fix.
- Runner failure/receipt tests: 6 passed. Immutable workflow action verification
  passed across 32 workflows. Independent review found no blocking issues.
- After upstream synchronization, the release pipeline and workflow dependency
  invariant scripts pass, resolving the earlier baseline RPM policy failure.
- Four new workflow-tier tests pass, including executing both actual OS mode
  scripts across nine event/ref/input combinations. Testing the pre-change merge
  commit `71b2be254` produces nine failed assertions, establishing that the tests
  detect the old policies. These tests inspect local workflow definitions and
  execute their mode scripts; they do not emulate the Forgejo scheduler.
- The full security boundary suite passed its first 25 entries, then was
  interrupted while its kernel-module signing test waited to compile the native
  crypto helper. This broader run is incomplete; the release and workflow
  invariants above were run independently and passed.
- The required `cargo check -p shell --tests --locked --offline -j2` passed after
  upstream synchronization using the pinned Rust 1.97.1 (5m 22s). The earlier
  pre-sync attempt was interrupted for disk pressure; the successful later run
  supersedes it. Disk capacity and cache retention remain practical constraints.
- Local checks alone establish no native cross-platform build, VM boot or hardware
  qualification. Live pilot and deployment evidence above establish only their
  explicitly described scopes.

## Still required for the full goal

The affected-package selector now treats moves as deletion plus addition and
reads NUL-delimited Git paths. A real Git regression fixture proved that the old
reader omitted the source package and quoted Unicode filenames; all 12 selector
tests pass with the fix. Cargo metadata now uses `--locked`.
An earlier metadata probe on the then-current 141-package workspace selected 4 packages
for `apps/calculator/src/main.rs` (2 native matrix jobs), 68 for `ui/src/lib.rs`
(4 jobs), and 3 contract packages for `docs/architecture.md` (2 jobs). These are
selection counts, not build-time measurements. Later policy omits Rust for design prose and four audited developer/CI documents.
The toolkit has since moved under `crates/sdk/ui-core`; remeasure current graph
counts rather than treating those historical paths or counts as a present contract.
Live Forgejo testing found empty matrices could block the aggregate; the deployed
sentinel row is skipped without allocating a runner and preserves an explicit
aggregate result. Security/selection remain mandatory; full requests override it.

The imager workflow now explicitly disables the retained macOS build and cask
test jobs. Linux/Windows publication no longer waits for macOS assets; Linux
Homebrew publication leaves the retained cask publisher disabled through
`TILO_IMAGER_MACOS_ENABLED=0`. Existing published casks are not advanced or deleted.
The real release artifact gate passes with both enabled architectures and still
rejects missing/corrupt enabled artifacts. macOS deferral is included in deployed relief; this local
artifact test remains narrower than native acceptance.
Website promotion rejects rendered macOS download links until the separate
website supports the enabled matrix; its repository lookup returned HTTP 404,
so that consumer remains unqualified. No website state was changed.

| Requirement | Evidence still needed |
| --- | --- |
| Cheap normal branch/PR path | Qualify the implemented trigger split on Forgejo; implement trusted affected-policy classification and relevant required proof before automatic merging |
| Automatic routine integration and independent review | Implement trusted worker; qualify CI provenance, simultaneous merges, ordinary author/reviewer permissions and recovery; restricted integrator enforcement is proven only in the private pilot |
| First useful retained baseline | Discover/admit actual bootable image and unsigned inputs; preserve digests, trust, source/helper identities and separate VM/board evidence |
| Targeted real installer/desktop testing | Supported commands consume that baseline, restore disposable state and produce actual test results without unrelated compilation/assembly |
| End-to-end feedback measurements | Extend verified production quality startup/queue results to native compilation, AWS snapshots/transfers, linking, finalization/signing and tests |
| Continuous app/runtime packaging | Current native/runtime ABI qualification beyond historical artifact replay; production active/latest-pending dispatch, compatible artifact selection, recovery and atomic stale-safe channel promotion |
| Full enabled product matrix | Linux/qOS apps, desktop distro/architecture combinations, Windows/Linux setup and generic/device OS candidates; Windows app fleet remains a staged decision |
| macOS exclusion | Disable every macOS build/publication prerequisite and test push/tag/manual paths without removing future capability definitions |
| Manual OS version release | Qualify the locally tested publication restrictions with real credentials; manual promotion must pin a fully tested compatible manifest |
| Forgejo platform decision | Real isolated scheduling/capacity/recovery qualification; compare Buildbot/Buildkite if it requires a general custom controller |
| Build reuse beyond the small probe | Real UI/runtime/native/generated slice, native Windows, fresh remote workers, invalidation and compatibility checks |
| Production readiness | Independent reproducibility, signed evidence, update/recovery/hardware qualification, staged promotion, retention and key lifecycle from the roadmap |

Do not mark the goal complete from the experiments, contract tests or a green
branch alone. Emergency CI deployment was subsequently authorized and performed;
this does not establish readiness or authorization for unrelated product
protection, release promotion or wholesale build-system migration.
