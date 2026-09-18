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

## Validation of this implementation

- Installer contracts: 11 tests passed on Linux/WSL, including the non-root
  private-user-namespace path. App packaging contracts: 17 tests passed, including
  both artifact architecture layouts. The handoff regression failed against the
  original packager before the layout fix.
- Runner failure/receipt tests: 6 passed. Immutable workflow action verification
  passed across 32 workflows. Independent review found no blocking issues.
- The wider workflow dependency invariant script still fails its RPM snapshot
  policy assertion. Re-running it from an archive of the unchanged base commit
  `76e3df05d` reproduces that failure. The local `origin/main` tracking revision
  already contains changes to that workflow and assertion; reconcile those when
  updating this branch rather than duplicating their fix.
- The required `cargo check -p shell --tests --locked --offline -j2` was attempted
  with the repository's pinned Rust 1.97.1, then interrupted when host free disk
  fell below 1 GB. This run is incomplete, not a passing check. No Rust source or
  lockfile changed in this implementation. Disk capacity and cache retention
  remain practical constraints even for checking a narrowly scoped change.
- No live Forgejo execution, native cross-platform build, VM boot, hardware
  qualification or deployment is claimed by these local results.

## Still required for the full goal

| Requirement | Evidence still needed |
| --- | --- |
| Cheap normal branch/PR path | Implement trusted affected-policy classification and trigger split; prove ordinary app/UI branches launch no OS/distro fleet work, while boot-policy changes receive relevant proof |
| Automatic routine integration and independent review | Prove exact-head review/check identity, stale-main handling, permissions and two competing merges in an isolated pilot before live protection/cutover |
| First useful retained baseline | Discover/admit actual bootable image and unsigned inputs; preserve digests, trust, source/helper identities and separate VM/board evidence |
| Targeted real installer/desktop testing | Supported commands consume that baseline, restore disposable state and produce actual test results without unrelated compilation/assembly |
| End-to-end feedback measurements | Trace real planning, queue, compilation, layer snapshots, transfers, finalization/signing, worker setup and tests; current contract timings are only a small part |
| Continuous app/runtime packaging | Native artifact handoff qualification, portable compatible artifact selection, active/latest-pending coalescing, restart/retry and stale-publication tests |
| Full enabled product matrix | Linux/qOS apps, desktop distro/architecture combinations, Windows/Linux setup and generic/device OS candidates; Windows app fleet remains a staged decision |
| macOS exclusion | Disable every macOS build/publication prerequisite and test push/tag/manual paths without removing future capability definitions |
| Manual OS version release | Prove automatic component completion cannot promote an OS version; manual promotion pins a fully tested compatible manifest |
| Forgejo platform decision | Real isolated scheduling/capacity/recovery qualification; compare Buildbot/Buildkite if it requires a general custom controller |
| Build reuse beyond the small probe | Real UI/runtime/native/generated slice, native Windows, fresh remote workers, invalidation and compatibility checks |
| Production readiness | Independent reproducibility, signed evidence, update/recovery/hardware qualification, staged promotion, retention and key lifecycle from the roadmap |

Do not mark the goal complete from the experiments, contract tests or a green
branch alone. Deployment remains subject to the user's no-main constraint until
the user authorizes that integration step.
