# CI/dev workflow delivery status

Goal: deliver the full CI/dev and continuous packaging workflow described in the
companion designs. Branch-local implementations below are not deployed policy.
Main and the ongoing hardware bring-up remain untouched.

## Implemented on the experiment branch

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
  request a non-publishing integration candidate. These are branch-local changes,
  not a claim about the live instance's deployed policy.
- Private isolated Forgejo merge probes now demonstrate missing-status and
  expected-head rejection, exact-head fast-forward landing and refusal of a
  sequential competing stale candidate. See `experiments/ci-forgejo/README.md`
  for raw evidence and the initial inconclusive run. Product repository settings
  remain unchanged; independent review and permission isolation are not proven.

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
- No live Forgejo execution, native cross-platform build, VM boot, hardware
  qualification or deployment is claimed by these local results.

## Still required for the full goal

| Requirement | Evidence still needed |
| --- | --- |
| Cheap normal branch/PR path | Qualify the implemented trigger split on Forgejo; implement trusted affected-policy classification and relevant required proof before automatic merging |
| Automatic routine integration and independent review | Build on the sequential live pilot: prove simultaneous/native automatic merges, exact-head independent review, trusted check identity, ordinary-user permissions and recovery before product protection/cutover |
| First useful retained baseline | Discover/admit actual bootable image and unsigned inputs; preserve digests, trust, source/helper identities and separate VM/board evidence |
| Targeted real installer/desktop testing | Supported commands consume that baseline, restore disposable state and produce actual test results without unrelated compilation/assembly |
| End-to-end feedback measurements | Trace real planning, queue, compilation, layer snapshots, transfers, finalization/signing, worker setup and tests; current contract timings are only a small part |
| Continuous app/runtime packaging | Native artifact handoff qualification, portable compatible artifact selection, active/latest-pending coalescing, restart/retry and stale-publication tests |
| Full enabled product matrix | Linux/qOS apps, desktop distro/architecture combinations, Windows/Linux setup and generic/device OS candidates; Windows app fleet remains a staged decision |
| macOS exclusion | Disable every macOS build/publication prerequisite and test push/tag/manual paths without removing future capability definitions |
| Manual OS version release | Qualify the locally tested publication restrictions with real credentials; manual promotion must pin a fully tested compatible manifest |
| Forgejo platform decision | Real isolated scheduling/capacity/recovery qualification; compare Buildbot/Buildkite if it requires a general custom controller |
| Build reuse beyond the small probe | Real UI/runtime/native/generated slice, native Windows, fresh remote workers, invalidation and compatibility checks |
| Production readiness | Independent reproducibility, signed evidence, update/recovery/hardware qualification, staged promotion, retention and key lifecycle from the roadmap |

Do not mark the goal complete from the experiments, contract tests or a green
branch alone. Deployment remains subject to the user's no-main constraint until
the user authorizes that integration step.
