# Live Forgejo qualification

These experiments use private `qos/ci-workflow-pilot-*` repositories containing
only fixture text. They do not modify `qos/qos`, its main branch, credentials or
protection rules. No build runners are used by the merge probe.

## Observed API merge behavior, 2026-09-18

The [repeat pilot](https://code.podesta.ai/qos/ci-workflow-pilot-20260918-9245-repeat)
uses branch protection applying to administrators, required context `pilot/ready`,
outdated-branch blocking, and `fast-forward-only` merge requests. Required reviews
are zero for this probe: its single repository-admin identity cannot qualify
independent review or ordinary-user permissions. Statuses are explicit fixtures,
not claims that a build ran.

| Probe | Actual result |
| --- | --- |
| Merge without required status | HTTP 405: `Not all required status checks successful` |
| Merge with deliberately wrong expected head | HTTP 409: `head out of date` |
| Merge the first green candidate | HTTP 200 |
| Merge the second candidate, created from the same original base | HTTP 405: `The head branch is behind the base branch` |
| Inspect final main | Exactly `02d49e600045f5108dbbfb127033fe564d909a9e`, the first candidate's tested head |

Evidence is in `results/merge-repeat-20260918/`: requests with HTTP responses,
result SHAs, and a read-back of repository/protection configuration. These results
establish the behavior of the **combined** configuration, not which individual
mechanism prevents a stale merge. Requests were sequential, not simultaneous.

The initial pilot's result is retained under `results/merge-20260918/`. Candidate
A was temporarily refused even after its synthetic status was posted, and B then
merged. That run therefore does **not** test a stale B: its recorded
`stale_candidate_rejected=false` is an invalid inference because A did not merge.
The first harness did not retain HTTP response bodies, so the precise reason for
A's refusal is unknown. The repeat retains bodies and requires the positive
control to succeed before attempting the stale candidate.

## Harness and repeatability

`merge-pilot.py` requires an explicitly named, fresh private pilot repository with
only its initial `main` branch and a new output directory. It enables protection,
creates two fixture PRs and mutates **that pilot repository only**. Do not run it
against an existing experiment to reset history. Inspect durable state after an
interruption before deciding how to continue.

After the successful repeat, independent review identified a weakness in the
harness: arbitrary API errors were counted as rejection. The harness now requires
the expected HTTP code and reason, records request bodies (all fixture data), and
reconciles PR state before retrying a failed merge response. The original repeat
responses independently contain the exact expected codes/reasons. No third live
run of the hardened harness has yet been claimed.

## Native auto-merge follows a changed head

The next probe used PR #3 in the repeat pilot, with the same status protection.
It queued fast-forward-only auto-merge with expected head
`704ba5786efe66c56601ab5881d7a830420e9b2d` while `pilot/ready` was pending. The
API returned HTTP 201. The author then advanced the branch to
`9077ef57b0818e490a0b64c4f5e187486aa838a2` before making the old head green.

The PR remained open during a bounded 12-second observation after the old head
became green. Once the **new** head received its own successful required context,
Forgejo automatically merged that new head, without another queue request. The
PR's merge SHA and final pilot main both equal the new head. The later cancellation
returned 404 because the PR had already merged. Raw request timestamps and SHAs
are in `results/auto-merge-head-20260918/`; `auto-merge-head-pilot.py` is the fixture.

This establishes that the supplied `head_commit_id` does not permanently pin a
native auto-merge subscription. It does **not** show that checks were bypassed:
the new head had its own successful synthetic context. Native auto-merge remains
plausible only if every new head independently earns trusted required checks and
fresh review. If policy requires queue authorization for one immutable head,
use an exact-head immediate merge after trusted readiness instead of assuming
the queued request provides that guarantee. Review invalidation is not tested
here: this fixture deliberately has zero required reviews and one identity.

## Candidate job token can publish the required context

The candidate-controlled `status-issuer-probe.yml` used only `${{ github.token }}`
to POST a successful `pilot/ready` status to its own commit. This is the context
required by the pilot branch protection. No administrator API token or stored
registry credential was supplied to the workflow.

Run `22547` / job `82848` spent its three-minute job limit pulling the default
`code.podesta.ai/public/runner-image:latest` and was cancelled before executing
the probe. A changed workflow selected a small Python container. Run `22563` /
job `82892` succeeded and printed `CANDIDATE_STATUS_HTTP=201`. A status read-back
for its exact commit `a26a2f4edeea5b50ccce44996fbe68f029306d91` confirmed the
synthetic `pilot/ready=success`. Evidence is in `results/status-issuer-20260918/`.

Both this synthetic status and the normal Actions job status have null creators
in the API response. A verifier must not treat a missing creator as trusted.
This proves context impersonation by candidate workflow code on a same-repository
branch. It does not test fork-token permissions or bypass of an independent
required review. No PR from the status-forging fixture was merged.

The explicit image uses a mutable tag in this isolated diagnostic. Its resolved
digest was not emitted in the retained logs; this is permission evidence, not
reproducible-build evidence. The failed default-image pull motivates measuring
worker setup separately; one failed pull is not a throughput benchmark.

## Different-identity review invalidation

PR #4 in the repeat pilot was authored as `xemmerling`. Its protection was changed
only in that private pilot to require one approval, whitelist reviewer `claude`,
dismiss stale approvals and apply the rule to administrators. Synthetic required
checks were green throughout the relevant merge attempts.

1. Before any review, immediate exact-head merge returned HTTP 405, explicitly
   reporting insufficient approvals.
2. The existing `claude` web-UI login approved head
   `cc1461402a4cfcfdd4575adda2aabb342ab68c4e`. Review #52 was official, approved,
   not dismissed, and bound to that exact commit.
3. The author advanced the branch to `41d99b13834f1243937be2264b130de52baf15c4`.
   Review #52 became dismissed. Even with a green status on the new head, its
   immediate merge returned HTTP 405 for insufficient approvals.
4. `claude` approved the new head as review #53. Immediate fast-forward merge
   returned HTTP 200; both the PR merge SHA and final pilot main equal that head.

Raw policy, reviews, HTTP responses and final state are under
`results/review-head-20260918/`. Run `python3
experiments/ci-forgejo/verify-review-evidence.py` to check their consistency.
The SHA-bound status readbacks were collected after the experiment; their server
creation and update timestamps precede the corresponding rejected merge requests.
The verifier checks that ordering against each retained HTTP response's Date header.
The browser inspected and approved only the two trivial fixture text files.

This establishes native exact-head stale-review enforcement using different
authenticated identities. Both identities have owner-level repository access,
and one experiment operator exercised them. It does not demonstrate independent
agent code-review quality, ordinary-user permission isolation, review races during
landing, or resistance to an owner changing the protection itself. No product
review policy or account permissions were changed.

## Misleading success versus underlying execution

Branch `probe-misleading-status` in the repeat private pilot ran
`misleading-status-probe.yml` at `168f7b957e1478dff8990518333930355fafdc0b`.
Run **22588**, job **82967**, posted required context `pilot/ready=success`
with its candidate job token (HTTP 201), then deliberately exited 42. The
server-owned run and job records both report `failure`; the required context
remains `success`. The combined status is correctly `failure` because it also
includes the normal Actions context. No PR or merge was attempted in this probe.

`audit-run.py` reads run/jobs directly and requires the explicit repository,
commit, workflow path and exact job set, with every job and the run successful.
It exits 2 on rejected execution evidence. It rejected this live failed run and
accepted the earlier successful run 22563 as successful execution only. Retained
observations and log excerpts are in `results/misleading-status-20260918/`.
Seven tests exercise those real records and missing, duplicate, foreign, skipped,
cancelled and mismatched evidence through the same checker.

This is one part of a future trusted gate, **not merge authorization**. A workflow
path is not a trusted workflow definition; a candidate can change commands and
still produce a genuinely successful Actions job. Trusted policy must choose the
expected jobs and verify the workflow plus its invoked scripts/actions, runner
isolation, review and current base/head. The experiment does not establish that
candidate credentials cannot mutate run/job records through another interface.
Complete job enumeration and consistent run/attempt snapshots during retries
also need qualification; these live fixtures each contain one job and one attempt.
The audit therefore always records `merge_authorized=false`; there is no merge
endpoint or credential in this tool.

## Decision and next evidence

Keep native Forgejo primitives for immediate merging. There is now
positive live evidence for required-status enforcement, expected-head rejection
and exact-head fast-forward landing with stale-base refusal. This is insufficient
to enable automatic product-repository merges. A native required context alone
does not authenticate its producer. Keep autonomous native auto-merge disabled
for the product until a trusted gate can resist candidate impersonation. The
current direction is a small trusted integration worker that verifies exact-head
review and underlying CI evidence, then immediately requests an exact-head
fast-forward merge. It must not become a new build scheduler.

Still unqualified: simultaneous merges, native auto-merge with independent review,
review/merge races, resistance to the demonstrated issuer spoofing, non-admin direct-push
restrictions, restart/duplicate-event recovery, runner resource isolation and
latest-pending coalescing. These are the next isolated probes. A trusted merge
identity and independent reviewer are still needed. The user identified `claude`;
its existing configured web-UI authentication was exercised as a different
identity from API user `xemmerling` in the review-invalidation probe above.

The pilot repositories remain private and retained for inspection. The head-change
PR has merged and has no pending auto-merge. Both issuer-probe runs are terminal.
