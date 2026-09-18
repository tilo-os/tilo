# Alpha CI and integration workflow

Status: proposed implementation plan; routine automatic merging approved by the user.
Date: 2026-09-18. Repository: qos/qos. Inspected revision: 032ea136f.

## Objective and decisions

Optimize developer and agent feedback during alpha. The OS is not intended for production hardware. More CI capacity is forthcoming, but ordinary changes must not build the complete OS or every distribution package.

- Use short-lived branches and PRs as the unit of work. Agents create, update, review and queue PRs through `fgj` or the Forgejo API.
- Automatically merge routine changes after required CI and independent agent review. No human approval per routine PR.
- Keep main usable for development; separately identify the last snapshot that passed OS boot and integration tests. A green PR does not claim full OS validation.
- Reserve human decisions for changing integration policy, signing/publishing authority, security boundaries or intentional weakening of checks. Ordinary bug fixes within these subsystems need not automatically become release ceremonies; classify actual boundary changes conservatively.
- Give PR checks reserved capacity. Heavy image, kernel and distribution jobs must not occupy the entire feedback pool.
- Continuously package apps, coalescing toolkit changes into coherent fleet builds. Build OS candidates on schedule or request, and release OS versions only after an explicit human decision.

## Evidence

The live instance reports Forgejo 16.0.3. Its branch protection API currently returns no rules for this repository. The installed API schema exposes `head_commit_id`, `merge_when_checks_succeed`, `fast-forward-only`, required status contexts and `block_on_outdated_branch`. These fields establish available primitives, not proof of race-safe end-to-end queue behavior.

At the inspected revision:

- `check.yml` already selects affected Cargo packages and reverse dependencies with `tools/ci_select.py`, runs native architecture lanes, tests, all-target compilation and a Clippy delta gate. Preserve this machinery.
- `lint.yml` runs on all branch pushes and PRs, with a normalized concurrency key intended to cancel duplicate work.
- `build-image.yml` triggers on PRs and main pushes touching broad paths including shell, UI and OS. It uses a single non-cancelling image concurrency group.
- `package-desktop.yml` builds five distribution userspaces on matching PRs and main pushes. This is disproportionate for ordinary compositor edits.
- `release-apps.yml` also validates changed apps on PRs and main pushes. Keep useful package-contract coverage, but do not make release production a routine prerequisite.
- `nightly.yml` and `docs/ci-performance.md` already distinguish affected checks from full coverage, including isolated software-rendered visual tests.
- `.codex/hooks/watch_ci.py` delivers post-push results, but its 25-minute watch window and broad green classification are notification behavior, not an authoritative merge gate.
- Open PRs #32, #34 and #36 concern component releases. Reconcile relevant changes with current main; do not merge these old, conflicting PRs wholesale.

Sources: live repository API; checked-in files above; https://forgejo.org/docs/latest/user/repository/protection/ ; installed instance schema at https://code.podesta.ai/swagger.v1.json . No runner throughput measurements were obtained in this inspection.

## CI tiers

| Trigger | Required work | Excluded by default |
| --- | --- | --- |
| Working branch push | Changed-file formatting, policy/configuration tests | Whole OS, distribution matrix, release publication |
| Ready PR | Affected packages plus reverse dependencies; all-target compilation; scoped tests and Clippy; relevant lightweight integration contracts; independent review | Whole OS and five-distribution packaging |
| Queue candidate | Required checks on a revision containing current main; reuse evidence only if its revision and inputs match | Automatic full rebuild just because it entered the queue |
| Explicit development snapshot | Component artifacts, OS assembly, VM boot/session smoke tests at pinned inputs | Promotion to a production channel |
| Nightly/backstop | Full workspace, architecture coverage, isolated visual tests and wider integration coverage | Blocking unrelated PRs merely because a scheduled run is pending |

An app edit selects that package and dependants. A shared UI edit legitimately selects many packages. Root manifests, lockfile, toolchain and unclassified build inputs conservatively widen checks. Never implement path filtering by looking only at the last commit: use the full PR diff and dependency closure.

For OS-related changes, run cheap shell/Python/configuration and payload contracts first. Require image/VM validation before merge only where that is the relevant proof, for example a boot-chain or image-composition change that cannot be established with the lighter contracts. An explicit request can always opt a PR into heavy validation. Do not silently defer all OS correctness until nightly.

Keep current affected native architecture coverage initially. Increased capacity is a reason to protect its throughput, not prematurely remove it. Measure before changing shard counts or making a platform advisory.

## Integration flow

### Preserve the speed of continuous integration

PRs here are an automated transaction and review record, not a human scheduling
ceremony. The user's observation about direct-to-main agent development is valid:
skipping repeated branch updates and approval round trips can substantially reduce
integration latency. Compare approaches by time to a usable integrated artifact,
not by how quickly a commit appears on main.

Use automatic small PRs with a short affected-check gate initially, because routine
automatic landing and independent review are already approved. Keep full packaging,
distribution matrices and routine OS builds outside this gate. Measure stale-main
revalidation and queue residence explicitly. If a serialized queue cannot keep up,
batch compatible candidates with attributable failures or use an established merge
scheduler; do not make every agent repeatedly rebase a long-running test against
every intervening commit. Prefer proven native scheduling before adding a service.

Direct integration with stabilization afterward remains a legitimate alpha option.
Its contract would be different: main is an integration stream, the last verified
revision/artifact is the development consumption point, and every regression needs
an owner and a quick revert/repair path. Independent review can be asynchronous in
that model, but that is a change to the approved pre-merge review policy, not an
equivalent implementation. Incoming agents must not mistake unverified main for a
known-good base. Choose this only if measured gate cost exceeds recovery cost and
the team explicitly accepts broken-head development; no such policy change is
enabled by this experiment.

Both approaches need the same dependency graph, cache correctness, last-good
artifact tracking and coalescing of heavy work. A PR does not require a full OS
build; direct pushes do not eliminate that build's cost. The
[experiment report](ci-build-graph-experiments.md) separates observed cache behavior
from the illustrative packaging queue model.

1. The author agent opens a draft PR early and marks it ready once its scoped verification is done. Drafts receive cheap feedback only; marking ready starts affected-package validation. Heavy checks require trusted classification or explicit opt-in. The draft-to-ready transition must schedule required checks before queue eligibility.
2. A separate reviewer inspects the diff and test evidence. Review is tied to the exact head SHA. Any new code invalidates approval. An automated service identity may publish a machine-verifiable review result; a label or a comment alone is not approval.
3. After the native pilot, select exactly one integration owner: native orchestration if it satisfies the invariants, otherwise a single minimal worker. That owner chooses ready PRs in order. It does not rewrite a branch while its author is editing it. Updates use a recorded expected head and abort on concurrent changes.
4. If the branch lacks current main, the worker incorporates main into the PR branch, then reruns required checks and refreshes review for the resulting head. Start with merge updates to avoid force-pushing agents' work; require a clean result or return the conflict to the author.
5. Prefer fast-forward-only landing of the tested head, with `head_commit_id` and required branch-protection checks. If main advanced, reject and revalidate. Do not switch silently to squash/rebase/ordinary merge, which creates an untested commit.
6. Waiting for CI must consume no build runner. Reconstruct readiness from Forgejo PR/review/run state wherever possible. Only if an external worker is needed, retain a small coordination lease and any state that cannot be reconstructed outside Actions. On restart, reconcile actual PR/main/run state before retrying. Failed PRs leave the ready queue so independent work can proceed.

The pilot is an implementation decision gate: do not build fallback queue infrastructure before testing native orchestration. It must verify fast-forward refusal when main moves, approval invalidation, exact-head rejection, status-context behavior and worker recovery. The API schema alone does not establish these semantics. If Forgejo's built-in auto-merge can satisfy the invariants, use it with a small serial dispatcher. Otherwise use a minimal external queue worker; do not build a speculative batching platform in phase one.

Only the integration identity may routinely merge to main. Disable ordinary direct pushes and force pushes once the candidate path is proven. Test permissions using an ordinary agent identity as well as the integration identity; a shared administrator credential defeats this boundary. Do not enable blocking protection until actual required contexts and credentials have been validated.

## Required check contract

Introduce a stable aggregate result, provisionally `pr-ready`, rather than requiring every conditional matrix job by name. Its trusted policy computes applicable checks and requires all of them to succeed. Failed, cancelled, missing or unexpectedly skipped checks fail closed; intentional non-applicability must be explicit. A trusted, versioned selection result records base/head SHAs, selected checks and reasons for every exclusion; failed selection blocks merging. It also requires current independent review and any narrowly applicable human decision.

Evaluate merge policy from trusted main or a trusted service, not an implementation supplied by the PR being judged. Candidate workflows must not be able to approve their own policy changes. Keep merge/publication credentials outside candidate code execution. Required status names alone do not authenticate their issuer. The pilot must try to forge the required context using candidate and ordinary-agent credentials. If Forgejo cannot restrict the issuer, disable autonomous native landing and require a trusted integration worker to verify producer identity, exact SHA and underlying evidence directly before merging; only that worker holds routine merge authority.

Distinguish useful immediate feedback from queue feedback: run scoped PR checks early, but only repeat them if the candidate or relevant inputs changed. Cancel obsolete runs of the same PR, not unrelated PRs. Post-merge smoke checks can run cheaply; avoid blindly duplicating the entire just-completed validation when landing preserves the tested revision.

## Development snapshots and continuous builds

The later user requirement makes continuous app/component production part of the initial scope, including during toolkit stabilization with batched fleet rebuilds. OS candidate builds are scheduled or on demand; OS version release is manual. See [continuous build and release roadmap](continuous-build-release-platform.md) for the full matrix, cache/artifact architecture and production stages. macOS builds are disabled; Windows setup is included and Windows app-fleet packaging is a staged pilot after incoming toolkit support merges. Keep PR validation separate from main-branch artifact production.

For enabled automatic snapshots, maintain one active immutable candidate and one replaceable pending request per target. New commits update the pending request without restarting the active useful build. Resolve mutable component channels once into a manifest containing source revisions, artifact digests and build inputs. Never assemble using whatever each channel happens to contain at a later step. Explicit release requests are pinned and cannot be silently superseded.

Retain successfully tested candidates for internal development use. An OS release/update channel advances only through an explicit pinned release request; component publication and automatic candidate builds cannot perform that promotion. A failure retains the previous tested snapshot and reports the failure to its owner. Report source revision and snapshot age prominently. Release signing and public distribution remain separate workflows.

## Alternatives and implementation order

Native auto-merge with branch protection is the smallest option, but needs a pilot to establish exact-candidate behavior. A small serialized integration worker offers explicit ordering and recovery using the same Forgejo primitives; this is the fallback and the recommended orchestration if native automation permits competing stale candidates. Speculative batching could improve throughput later but increases failure attribution and retry complexity; defer it until queue measurements justify it.

1. Decouple ordinary PRs from broad OS/distribution production triggers. Retain current main production until each replacement passes an end-to-end affected-target scheduling test; then replace broad per-main-push fan-out with the companion roadmap's continuous impact planner, artifact reuse and per-target coalescing. Preserve targeted pre-merge proof and exact heavy-build dispatch. Update workflow contract tests alongside trigger changes.
2. Add stable required-check aggregation and exact-head independent-review evidence. Teach agent instructions and the existing CI notification path to report ready, queued, failed and merged states.
3. Pilot controlled landing with two competing PRs, then enable protection and routine automatic merging. Have a rollback procedure for a stuck queue, without permanently bypassing checks.
4. Reserve PR runner capacity and measure service times. Retain the current cache design until measurements identify a bottleneck.
5. Implement continuous app/component builds, coalesced toolkit-driven fleet work and scheduled/on-demand OS/device candidates with the companion roadmap. OS releases require explicit pinned requests. Compiler caching and immutable artifact reuse are first-class workstreams.

## Acceptance and measurements

- A representative app/UI PR starts no OS build or five-distribution matrix.
- A shared dependency change selects its transitive dependants; unknown build inputs widen coverage. Mixed app/boot changes, workflow/selector edits, failed selection and draft-to-ready transitions are covered by classifier and trigger tests. Candidate changes to workflow definitions receive trusted policy validation.
- Two independently green PRs cannot land against stale main without candidate validation.
- Changing a reviewed head invalidates review; missing or cancelled required results never permit merging.
- A failed/conflicted PR does not block unrelated ready work indefinitely.
- A worker restart or duplicate event cannot merge twice or use stale approval.
- Ordinary agents cannot directly push main or impersonate the trusted gate.
- An on-demand snapshot can be traced to immutable source and component inputs, and failure retains the last usable snapshot.
- Record median/p95 time to first feedback, ready-to-merge time, runner queue wait, cancelled CPU time, main regressions and last successful snapshot age. Set numerical budgets after sampling representative warm and cold changes; do not claim timings from historical cache measurements as today's baseline.

Next concrete step: implement and validate the workflow-trigger split alongside the continuous roadmap's target inventory and artifact-handoff audit, then pilot the merge path before changing live branch protection. This document makes no live CI or repository-policy changes.
