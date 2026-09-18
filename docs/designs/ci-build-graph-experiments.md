# Isolated CI and build-graph experiments

Date: 2026-09-18. Baseline: `032ea136f5d6a7cbd2ddcf87e5dd7ab80fbd971a`.
Branch: `codex/ci-build-graph-experiments`. No main changes, remote workflow
changes, registry publication, branch-protection changes or OS builds.

## Decision after this spike

Keep Forgejo Actions as the executor while improving the build and packaging
boundaries. Dependency-aware packaging alone is not a reason to migrate CI.
The repo already has useful reverse-dependency selection; the release workflow and
shared Rust runtime impose broader work than the selector itself needs.

Bazel demonstrated more complete cached-action reuse than Cargo/sccache in this
small probe, including the final executables and archives. This justifies a real
toolkit/runtime qualification, not adopting Bazel across the repo now. In particular,
the rules_rust project states that its maintainers lack Windows expertise and have
disabled many Windows tests. Windows setup is required here, so that is a material
migration gate. [rules_rust documentation](https://bazelbuild.github.io/rules_rust/)

Preserve fast integration with small automatic PRs and independent review, reserving
heavy packaging capacity separately. Direct-to-main stabilization is a viable alpha
tradeoff with a known-good artifact frontier and rapid recovery, but has not been
shown faster end-to-end for this repo. The [integration plan](alpha-ci-integration.md)
now explicitly compares these policies. This spike does not enable either policy.

## What actually ran

- Existing planner test suites: 11 CI-selection tests and 14 app-release tests.
- Both planners against real Cargo metadata: 140 workspace packages and 54 app
  manifests at the pinned baseline.
- All eight calculator engine unit tests passed under Cargo and Bazel.
- Bazel 9.2.0 / rules_rust 0.74.0 and Cargo/Rust 1.97.1, native Linux ARM64 in WSL.
  Bazel's downloaded binary SHA-256 was checked against the official checksum.
- Two small executable harnesses using copied production calculator expression,
  qdoc ordering and atomic-file modules. Both executables call the shared atomic
  writer and verify its output. Their dependency topology is a fixture, not a claim
  that these are the actual GUI apps or the shared UI runtime.
- Cold, warm, app-module change, shared-module change, package metadata change,
  unrelated documentation, a fresh Bazel output directory and compiler-flag changes.
- Cargo workspaces at two different paths and a fresh Cargo target at the original
  path, sharing only an isolated sccache 0.12.0 store.
- A deterministic, hypothetical packaging queue model. It is not a Forgejo load test.
- Repository-required `cargo check -p shell --tests --locked --offline -j2` passed
  under Rust 1.97.1 (eight minutes; three existing dead-code warnings). This compiles
  test targets but does not execute shell integration tests.

The executable probes and reports are in
[`experiments/ci-build-graph`](../../experiments/ci-build-graph/README.md).
No source in the product checkout is changed by the probes. Sources are copied to
a fresh experiment directory, and the product Cargo.lock is not regenerated.
Bazel's final run ignores inherited rc files and explicitly disables remote
cache/execution/downloader endpoints. sccache uses an isolated empty config and a
bounded local cache. The experiment servers are stopped on completion.

## Real dependency selection

| Changed input | App-release selection | CI check packages |
| --- | ---: | ---: |
| Calculator engine | 1 | 4 |
| UI toolkit | 53 | 68 |
| Cargo.lock | 53 | 140 |
| App packaging script | 53 | 3 |
| README | 0 | 3 |
| Shared atomic-file crate | 53 | 94 |
| Browser source | 1 downstream app (`agent`) | 7 |

The standalone app catalog has 54 entries; the browser itself is excluded from the
generic release builder because it uses the OS-integrated browser path. Changes to
it still select its downstream app. The three contract check packages are
intentionally included even for documentation-only input by the current selector.

These are potential build/check selections, not counts of compulsory compilations.
For example, the packaging-script change selects all app packages but should reuse
unchanged compiled payloads. A toolkit edit legitimately reaches the whole app set.

At this baseline, `release-apps.yml` selects the default collection for ordinary
relevant main pushes. `package-apps.sh` also supplies the full default collection to
the thin-app builder even for selected output packages, because shared-runtime
Cargo feature unification and Rust dylib identity require consistency. Changing the
workflow's selected app list alone will not remove that build coupling. Preserve
the exact runtime-root compatibility check until a replacement boundary is proven.

## Cache behavior observed

The Bazel execution logs distinguish actions that actually ran from cached actions.
Warm and documentation-only builds needed no execution actions. An app-module edit
reran that module and its executable, leaving the other executable untouched. A
shared-module edit reran the shared library and both executable consumers. A package
metadata edit ran just the two packaging actions, without Rust compilation.

The source edits append an unused exported function to a copied module. They test
dependency invalidation, not an ABI change or a behavior-changing patch. Unchanged
final executable bytes can therefore permit downstream archive reuse. Compiler
flags are another tested input; real SDK/header, generated-code and feature-union
invalidation remain open qualification work.

A fresh Bazel output directory reused nine cached actions: seven Rust actions
(including two rules_rust support targets) and two archive actions. This includes
final linking, unlike sccache's Rust compiler-only reuse. The completed probe
extracts and executes the packaged binaries, so success requires usable archives,
not just a green build command. This is a **local shared disk action cache on one
machine**, not a measured remote service or two physical runners.

| Cargo/sccache case | Rust library hits | Rust library misses | Final binaries |
| --- | ---: | ---: | --- |
| First isolated checkout | 0 | 3 | Linked locally |
| Identical source, different checkout path | 0 | 3 | Linked locally |
| Fresh target, original checkout path | 3 | 0 | Linked locally |

The two binaries were explicitly reported non-cacheable by crate type; other
non-cacheable requests were Cargo's compiler probes. There were no cache read/write
errors. All built executables passed their assertions.

This supports standardizing the checkout path **inside isolated runner containers**
and measuring real hits across fresh workers. It does not justify sharing live
Cargo target directories or setting `CARGO_TARGET_DIR`. Newer upstream sccache
documents `SCCACHE_BASEDIRS`, but that option is absent from the pinned 0.12.0
documentation used for this experiment; do not assume an environment variable alone
fixes the deployed version. Qualify any upgrade/path normalization with the same
cross-checkout test. [sccache 0.12.0 configuration](https://github.com/mozilla/sccache/blob/v0.12.0/docs/Configuration.md),
[current configuration](https://github.com/mozilla/sccache/blob/main/docs/Configuration.md)

Treat elapsed times in the machine-readable results as diagnostic observations.
Bazel's first run includes dependency/toolchain bootstrap, Cargo's toolchain is
already installed, Cargo timing excludes packaging, settings are not identical,
and other host compilation can contend for resources. These tiny probes are not
an apples-to-apples speed benchmark or a forecast for full applications.

## Packaging queue experiment

Model assumptions: 40 revisions arrive every 2 minutes, starting at minute 0;
one build takes 12 minutes; one slot; no build failures; every input supersedes the
previous desired snapshot. Completion is processed before an arrival at the same
instant. These numbers are illustrative, not measured fleet parameters.

| Policy | Builds finished | Busy slot minutes | Latest revision ready |
| --- | ---: | ---: | ---: |
| Build every revision in order | 40 | 480 | Minute 480 |
| Cancel active build on each revision | 1 | 90 | Minute 90 |
| Finish active, replace pending with latest | 8 | 96 | Minute 96 |

The cancellation policy discards 78 minutes and delivers nothing during the burst.
Finishing active work supplies intermediate usable snapshots while bounding the
pending queue to one. FIFO produces every revision but falls far behind. Explicit
release requests must be pinned and are not supersedable like these development
snapshots. The model assumes the scheduler implements each policy correctly; it
does not prove Forgejo's concurrency configuration does so.

A read-only live sample on the same date showed a main push spawning seven workflow
runs, including image, desktop and app release work. App/image runs were waiting;
an earlier desktop run was cancelled by the next push. This corroborates broad
fan-out and supersession in the live system, but is not a representative latency
distribution or proof of any particular root cause. No live test jobs were injected
into the shared runner queue.

## Limits and next implementation steps

1. Keep the current Cargo selection logic, narrow normal branch/PR triggers, reserve
   fast-check capacity, and validate the replacement before retiring main production.
2. Make Linux/qOS app output continuous now, with coherent runtime cohorts and one
   active/latest-pending request. Stabilizing the toolkit should shrink how often
   the cohort changes; correctness does not permit ignoring toolkit dependencies.
3. Run a **real** two-app/UI-runtime slice with native/generated dependencies, qpkg
   compatibility and two package forms. Compare Cargo/component reuse against
   Bazel; include clean remote workers and Windows setup after toolkit support lands.
   The small source-module slice here does not satisfy that gate.
4. Qualify scheduler behavior in an isolated Forgejo test repository/runner pool:
   burst delivery, cancellation, matrix capacity, active/latest-pending semantics,
   failure/restart recovery and stale-publish rejection. If this requires building a
   general scheduler, choose Buildbot/Buildkite rather than expanding custom glue.
5. Follow the [full product/release roadmap](continuous-build-release-platform.md):
   Linux distro desktop packages, Linux/Windows setup, x86-64/ARM64 and declared
   device images. Keep macOS disabled, Windows app-fleet packaging staged, OS
   candidate builds scheduled/on-demand and OS version promotion manual.

No full GUI-app, OS boot, device, Windows, signing, remote-cache or production
scheduler qualification is claimed. This branch is an experiment and decision
record, not an enabled replacement CI system.
