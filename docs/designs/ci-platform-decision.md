# CI platform decision: Forgejo Actions and build reuse

Updated: 2026-09-19. Status: provisional executor retained; emergency relief
deployed, with build-graph and production integration qualification still required.
Applies to the [continuous build roadmap](continuous-build-release-platform.md) and [alpha integration plan](alpha-ci-integration.md).

## Decision

The [development feedback-loop update](development-feedback-loop.md) shifts the
first qualification toward assembly, artifact handling and test-worker lifecycle.
The small Rust/Bazel probe measures build-cache semantics; it is not evidence that
compiler selection is currently the largest performance lever. Reuse the existing
retained-rootfs and targeted-test helpers before starting a broad migration.

Retain Forgejo Actions as the provisional execution platform. Dependency-based packaging is a build-graph requirement that Actions does not itself implement. Decide the build-graph owner now, before committing to a homegrown planner/cache engine or a fleet-wide YAML refactor. There is no demonstrated requirement to replace Forgejo merely because the product matrix is large; equally, retaining Forgejo must not be treated as deciding the build architecture.

Keep product selection, packaging entry points, artifact manifests, compatibility rules and promotion verification in versioned, locally callable code. Workflows should execute these contracts on the appropriate runners. Keep compiler/container caches and durable artifacts outside workflow-local storage.

Run two early qualifications: build graph/invalidation/reuse, and scheduler/recovery. Compare the existing Cargo-plus-component approach with a representative Bazel/rules_rust slice; evaluate narrower Nix derivations for Linux system-component reuse. This is evaluation now, not authorization for a wholesale migration. If scheduling needs a substantial custom service, compare Buildbot (self-hosted) or Buildkite (managed) rather than building another CI engine. Repository hosting can remain Forgejo; webhook, checkout, status reporting and identity integration must be proven separately.

Confidence: good fit as a command executor; conditional fit as the complete scheduler. Current source and API inspection do not establish throughput or failure-recovery behavior on the incoming runner fleet. This is not a claim that Forgejo has passed a load test.

Live integration evidence now supplements the schema inspection:
[isolated merge probes](../../experiments/ci-forgejo/README.md) demonstrated
required-status rejection, wrong-head rejection, a successful exact-head merge
and rejection of a sequential competing stale candidate. This supports continuing
with native primitives first. It does not qualify concurrent/native auto-merge,
trusted status issuers or runner scheduling; later review and permission probes
provide narrower evidence below.

Subsequent live probes found that queued auto-merge follows a changed head when
its new required status succeeds, and that candidate workflow code can publish
that required context using its built-in job token. Native context names alone
therefore cannot authenticate integration readiness. Prefer a small trusted
integration worker for exact-head evidence verification and immediate landing;
keep Forgejo as the CI executor while scheduler qualification continues. This
finding does not itself justify replacing the CI platform or creating a custom
build scheduler. See the same pilot report for the positive observations and limits.

The [restricted-integrator proof](../../experiments/ci-forgejo/results/integrator-permissions-20260919/README.md)
now demonstrates non-admin exact-head merging with enforced approval and merge
allowlists in a private fixture repository. Protection edits and product access
were denied. Author and reviewer remain administrators; synthetic status input
is not trusted CI evidence. Routine automatic integration is still unimplemented,
and product protections were not changed by this experiment.

Emergency relief is a separate deployed increment: the coordinating task reports
product `0975d89a8` and infrastructure `ffbd` live. The
[emergency runbook](../ci-emergency-relief.md) on main records operational details.
Every main revision retains check/lint; only eligible superseded heavy runs are
coalesced. Reserved quality capacity, a lean image and reduced automatic heavy
fan-out improve the executor without deciding the long-term build graph. This
deployment is not load/recovery qualification or delivery of continuous packaging.
Keep measuring actual queue and execution time before selecting a replacement.

Follow-up: [isolated experiment results](ci-build-graph-experiments.md) demonstrate
affected selection on this workspace and fine-grained caching on a small Linux
module slice. They do **not** qualify a full toolkit/runtime graph, native Windows,
remote workers or Forgejo scheduling. Keep the representative-slice gate below.
The rules_rust maintainers explicitly state limited Windows support and disabled
Windows tests; require a successful Windows setup/app qualification before making
it the cross-platform default. [rules_rust platform support](https://bazelbuild.github.io/rules_rust/)

## Separate three decisions

| Layer | Required job | Proposed choice now |
| --- | --- | --- |
| Source and review | PRs, checks, protected main, agent interaction | Existing Forgejo |
| Execution and scheduling | Select runners, run jobs, order dependencies, retain logs, cancel/retry work | Forgejo Actions, subject to a focused pilot |
| Dependency-aware build | Determine the input graph and affected outputs; reuse valid compiled/package artifacts | Early comparison: existing Cargo and coarse component reuse versus Bazel/rules_rust; Nix for appropriate Linux components |
| Release semantics | Validate compatibility and promote immutable product versions | Portable product manifests and promotion policy, independent of build system/CI |

A CI migration addresses execution and scheduling. It does not itself eliminate runtime ABI invalidations, expensive final linking, untracked build inputs, or repeated compilation inside package scripts. Conversely, adopting Bazel would be a build-system migration, not merely changing CI.

### What “only build changed apps” actually requires

There are two separate operations: select potentially affected apps from changes, then determine which outputs for those apps are already valid. Existing `tools/app_release.py` uses Cargo metadata and reverse dependencies for the first operation. That does not make it an artifact-level incremental build engine. Cargo provides local incremental/freshness behavior and sccache reuses eligible compiler outputs; neither alone supplies a complete cross-run packaging graph.

The useful dependency path is `app source + toolkit/runtime + generated/native inputs → target payload → package wrappers → collection → optional OS composition`. A package-only change should not compile the app again; an unrelated app edit should not invalidate a peer whose complete input set is unchanged; a toolkit or runtime change legitimately invalidates dependent apps. Manual OS promotion remains outside automatic packaging.

The current qOS thin-runtime graph is a special constraint: exact Rust runtime identity and Cargo feature unification can require a cohort rebuild. A new build system cannot simply declare that dependency away. Measure and preserve the existing ABI, then redesign the boundary separately if it prevents the desired granularity.

The existing Nix desktop derivation takes a broadly filtered repository source and builds one desktop unit. Installing a binary cache on that derivation does not automatically make it app-granular: source boundaries and derivations must change. Nix's supported native platforms are Linux and macOS, so it should not be assumed to replace native Windows build/install qualification. [Nix supported platforms](https://nix.dev/manual/nix/stable/installation/supported-platforms.html)

Bazel is a serious candidate for the fine-grained graph because its remote action cache addresses declared inputs and outputs. rules_rust's crate_universe uses Cargo to generate Bazel dependency targets, but our workspace targets, build scripts, bindgen/Cap'n Proto/native libraries, feature unification, linking and packaging still need correct modeling and validation. Reusing Cargo metadata is not proof that our complete project ports cheaply. [rules_rust crate_universe](https://github.com/bazelbuild/rules_rust/blob/main/crate_universe/README.md), [external crates](https://bazelbuild.github.io/rules_rust/external_crates.html)

## Evidence and limits

The live repository's API reports Forgejo 16.0.3. The runner versions and incoming hardware inventory have not been established. Consult the installed server/runner combination in the pilot; latest documentation currently describes the v16 series and can include later fixes.

Forgejo documents matrices, job dependencies, workflow dispatch, reusable workflows and runner selection. Runner labels can route work by capabilities, including host execution and special hardware. These are enough to express native Linux/Windows jobs, architecture pools, packaging stages and on-demand OS candidates at the workflow level. Actual environments, isolation and native tests remain our responsibility. [Workflow reference](https://forgejo.org/docs/latest/user/actions/reference/), [runner configuration](https://forgejo.org/docs/latest/admin/actions/configuration/)

Relevant limitations:

- Workflow concurrency is documented as best-effort and does not govern jobs within a workflow. Exact sequential ordering is not guaranteed. `cancel-in-progress: false` queues work rather than establishing the desired “finish active, retain only latest pending” policy. Correct publication therefore needs idempotent immutable writes and a guarded channel update independently of this setting. [Concurrency reference](https://forgejo.org/docs/latest/user/actions/reference/#concurrency)
- The checked-in desktop packaging workflow explicitly serializes five distribution builds because its authors observed that matrix `max-parallel` and job concurrency were not enforced. Treat that as repository evidence to reproduce on installed versions, not a claim that all future versions behave identically.
- Forgejo explicitly targets familiarity with GitHub Actions rather than full compatibility. Some job keys are ignored, and OIDC uses a different switch. Do not copy a GitHub design and assume its scheduling or permission guarantees survive unchanged. [Compatibility guide](https://forgejo.org/docs/latest/user/actions/github-actions/)
- Durable artifact identity, runtime compatibility and channel promotion are product-specific requirements. A successful workflow or an Actions cache entry does not implement them.

None of these findings establish that the large matrix itself exceeds Forgejo capacity. They identify where a proof is needed and where correctness cannot depend solely on workflow syntax.

## Alternatives

| Approach | Fits this project when | Principal cost or gap | Decision |
| --- | --- | --- | --- |
| Forgejo Actions with portable build tooling | Bounded workflows, labeled resource pools and simple per-product coalescing meet actual throughput needs | Some scheduling guarantees and product state need explicit handling | Default for the pilot and first rollout |
| Buildbot | Self-hosted operation needs programmable request collapsing, priorities, resource locks and dependency triggering across a heterogeneous fleet | Operate another controller/database/workers and maintain Python configuration plus Forgejo integration | First self-hosted alternative if the scheduler pilot fails |
| Buildkite | A managed control plane is acceptable and dynamic pipelines/concurrency controls reduce operational work | Commercial service relationship, platform configuration and Forgejo event/status/identity integration | Managed alternative; no purchase or migration proposed yet |
| Bazel/rules_rust or scoped Nix derivations | Explicit dependency/invalidation modeling provides useful reuse without fragile bespoke rules | Recipe/toolchain migration and native compatibility work; Nix alone does not cover native Windows | Evaluate a representative slice now; do not yet convert the whole repository |

Buildbot natively exposes request collapsing and prioritization, builder locks and triggerable/scheduled builders. These are relevant replacements for custom scheduling machinery, although policies and integration still require engineering. [Builders](https://docs.buildbot.net/latest/manual/configuration/builders.html), [global scheduling settings](https://docs.buildbot.net/latest/manual/configuration/global.html), [schedulers](https://docs.buildbot.net/latest/manual/configuration/schedulers.html)

Buildkite supports generating/uploading pipeline steps and explicit concurrency groups. Its own documentation describes retry/duplicate-step and large-upload considerations: adopting it is not a substitute for idempotent artifact/promotion logic. [Dynamic pipelines](https://buildkite.com/docs/pipelines/configure/dynamic-pipelines), [concurrency](https://buildkite.com/docs/pipelines/configure/workflows/controlling-concurrency)

Bazel remote caching operates over declared build actions, their inputs/outputs, and a content-addressed store. That can address finer-grained build reuse, but only after recipes are correctly modeled in Bazel; it does not transparently cache existing arbitrary Cargo/packaging scripts. Retain the existing Nix path where it is already useful rather than converting the whole repository by assumption. [Bazel remote caching](https://bazel.build/remote/caching)

## Bound the supporting code

Acceptable custom code is specific to this product: target catalog, affected-component plan, immutable artifact manifest, app/runtime compatibility checks, and signed publication policy. These remain useful under any CI executor.

An acceptable minimal coordinator records a requested revision/input set, suppresses duplicate dispatches, maintains active/latest-pending state per product cohort and reconciles completed runs. It must not hold build runners while waiting. Start with one owner and reconstructible state; do not assume it needs a distributed service.

Stop and select an existing orchestrator if the proposed coordinator grows to need a general DAG execution engine, elaborate resource brokerage, distributed leases/recovery, priority/preemption machinery, custom retry framework or a second CI status UI. Those capabilities are the reason to consider Buildbot/Buildkite. Publication compare-and-set/idempotency is still required with any scheduler; it is not itself a reason to migrate.

Likewise, do not implement a homemade fine-grained compiler action cache. Reuse Cargo/sccache and existing image/Nix mechanisms. Initially reuse completed components at conservative, recorded input boundaries. Narrow those boundaries only with invalidation tests; a false hit is worse than a legitimate rebuild.

## Build-graph qualification before custom infrastructure

Use a representative slice, not a dependency-free demo: two apps sharing the toolkit/runtime, a generated/native dependency, one real target payload and two package forms. Add the Windows slice once toolkit support has actually merged. Keep the committed Cargo.lock and production behavior intact.

Compare current Cargo/sccache plus conservative component artifacts against a Bazel/rules_rust prototype; use the existing Nix path to judge Linux component derivation granularity rather than requiring an unrelated all-project Nix conversion. Measure correctness, rebuild scope, cold/warm elapsed and CPU time, final linking, remote transfer, and the amount of duplicate build configuration required.

Required experiments:

1. Repeat on a fresh worker: reuse valid dependencies/payloads; distinguish compiler-object hits from complete action/artifact hits.
2. Change one app: rebuild its true dependency closure, leaving the other app reusable unless a demonstrated runtime-cohort dependency requires rebuilding it.
3. Change the shared toolkit/runtime: rebuild all real consumers and retain runtime compatibility checks.
4. Change package metadata: reuse compiled output unless metadata is embedded in compilation; repackage and validate the final package.
5. Change an SDK/header, generated file, feature, linker flag or build script: invalidate required outputs, with no false hit.
6. Change unrelated documentation: no application build invalidation solely because the repository commit changed, except where the current recipe truthfully consumes that commit.
7. Compare Cargo and prototype payload behavior, ABI/runtime identities and test results. A cache saving cannot justify changing the app/runtime contract or omitting native tests.

Choose an established build graph early if achieving these semantics with current tooling requires a growing custom dependency/fingerprint engine. Keep Cargo plus coarse component reuse if it meets the required behavior with a small auditable boundary and materially less maintenance. Do not start writing the sophisticated custom engine while this choice remains open. Forgejo can execute either result, so this decision need not move repository hosting or all CI at once.

## Scheduler qualification before broad investment

Time-box an initial engineering spike to roughly two focused implementation days once test runners are available. This is a planning estimate, not a promised benchmark duration or a scheduled task. Use disposable fixtures for scheduler behavior and a small number of real representative artifacts for cache/build behavior. Do not repeatedly build the entire OS to test queue semantics.

| Probe | Pass condition | If it fails |
| --- | --- | --- |
| Burst of toolkit changes | Active build finishes; only latest pending cohort proceeds; an explicit pinned request is preserved | Try minimal dispatch/coalescing adapter; reject a growing scheduler workaround |
| Mixed resource demand | Reserved PR pool starts feedback while native package/VM work runs; no reliance on unimplemented matrix limits | Correct pool/capacity setup; migrate if required policy still needs substantial custom scheduling |
| Missing Windows or device worker | Unrelated Linux products finish; blocked target remains visible; no executor is occupied merely waiting | Split workflow boundaries; evaluate alternate orchestration if recovery remains unreliable |
| Controller/worker restart and duplicate completion | State reconciles, no duplicate publication, no lost pinned request or stuck queue | Fix portable idempotency first; repeated scheduler-state failures favor migration |
| Dynamic selection and partial retry | Correct enabled target set appears; failed lane retries without rebuilding completed compatible outputs | Simplify DAG/workflow boundaries, then compare alternative controller if still problematic |
| Cross-worker reuse | Fresh compatible worker reuses compiler objects and completed component artifacts; changed inputs invalidate correctly | Fix cache/input model; this is not automatically a CI-platform failure |
| Manual OS release | Automatic app/desktop builds cannot publish an OS version; one explicit pinned request can complete safely | Fix release authority and immutable manifest contract, regardless of scheduler |
| Forgejo integration | Exact revision checkout, authoritative PR results and review invalidation work; runner/server versions recorded | Treat this as a prerequisite for automated merging under either executor |

Record queue delay separately from compile/link/test time, along with idle runner capacity, scheduler/API failures, dispatch latency and cache transfer cost. Saturated hardware, a runtime-wide ABI change and a scheduler bottleneck require different remedies. Forecast incoming fleet demand using observed job durations rather than counting YAML files or matrix entries.

Run the comparison on the same portable commands and artifacts. If Forgejo passes with thin wrappers, commit to it for the next roadmap phase. If it fails after straightforward pool/workflow corrections, test only the failing scenarios on Buildbot (or Buildkite if a managed service is preferred), then choose before expanding the full fleet YAML. Do not use sunk cost to justify custom infrastructure that duplicates an existing CI engine.

## Migration-safe implementation contract

- Commands accept explicit source/input manifests and output paths, and can run outside Forgejo. Read event contexts only in a thin workflow adapter.
- Preserve build scripts, builder images, target catalog, immutable artifacts, cache storage and test receipts across a scheduler move.
- Separate the desired product graph from the YAML used to execute it. Avoid hundreds of hand-copied target workflows.
- Use durable registry/object storage for cross-run artifacts. Keep Actions artifacts for run-local diagnostics and handoff only where appropriate.
- Document credential and OIDC assumptions. A scheduler migration changes issuer/claims and requires a deliberate signing-broker reconfiguration; it is not a drop-in move of secrets.
- Treat Forgejo status names and run IDs as integration metadata, not portable artifact identity.
- Keep OS release approval/promotion independent of automatic application packaging triggers.

## What would change this decision

Move execution/orchestration earlier if measured scheduling or recovery remains unreliable with free suitable runners, if mandatory resource policies cannot be expressed without a substantial service, or if maintaining cross-workflow dependencies starts to dominate work on build/package correctness.

Choose the build-system path using the early graph experiment above. If unnecessary compilation/linking persists or tracking the true input graph by scripts becomes fragile, prefer the established graph tool that passed the representative test. A later change in workload may warrant reassessment; do not postpone the initial dependency-graph decision until after writing a custom replacement.

The immediate commitment is to portable build/artifact interfaces and an early scheduler qualification, not to a permanent Forgejo-only architecture and not to a premature replatforming.
