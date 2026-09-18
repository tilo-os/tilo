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

## Decision and next evidence

Keep native Forgejo primitives as the first integration candidate. There is now
positive live evidence for required-status enforcement, expected-head rejection
and exact-head fast-forward landing with stale-base refusal. This is insufficient
to enable automatic product-repository merges.

Still unqualified: simultaneous merges, native auto-merge after a head change,
review invalidation, required-context issuer spoofing, non-admin direct-push
restrictions, restart/duplicate-event recovery, runner resource isolation and
latest-pending coalescing. These are the next isolated probes. A trusted merge
identity and independent reviewer are still needed; an administrator posting
`pilot/ready` does not establish that candidates cannot forge the real gate.

The pilot repositories remain private and retained for inspection. Their open
stale fixture PRs have no workflows or automatic merge requests running.
