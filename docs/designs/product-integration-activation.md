# Product integration: operator report and activation proposal

Status: **read-only implementation; activation not performed**. No product merge
grant, protection setting, service or token was changed. The private pilot CLI
continues to hard-deny `qos/qos`.

## Live starting point

The [retained report](../../experiments/ci-forgejo/results/product-operator-20260919/report.json)
was collected on 2026-09-19 at 00:53 UTC against main
`5ade281b038a0e4f402fd80b6220cb4de7bd0d8f`; its second main read matched.
These are observations, not durable assertions about later repository settings.

| Setting | Observed product value | Required activation change |
|---|---|---|
| Repository | private `qos/qos`, ID 23; Actions enabled | retain |
| Branch protection rules | empty array | create exact `main` protection |
| Fast-forward-only merge support | enabled | use explicit FF-only requests; do not rely on default merge style (`merge`) |
| `qos-integrator`, user 81 | non-admin, repository permission `none` | grant repository `write` only, separate product-scoped token |
| `claude`, user 5 | non-admin, repository permission `owner` | existing reviewer can approve; no extra reviewer grant needed |

The API's `active:false` profile flag is recorded, not interpreted as an account
disable requirement: the existing private pilot successfully used these identities.
Do not change account activation or organization ownership speculatively.

Open PRs at collection were 61, 38, 36, 34 and 32. PR61 had check running and lint
failed; the older PRs had failed/cancelled checks. All are reported blocked. No
policy or approval was invented to manufacture a ready candidate. A source search
in `infra-ci-emergency/apps` found no product integrator deployment; this is not a
claim that no ad-hoc process exists anywhere.

## Repeatable operator command

Run from reviewed tooling, with `fgj` authenticated for read access:

```sh
python3 tools/ci_product_report.py --out /tmp/qos-integration-report-UNIQUE
```

The output directory must not exist. JSON retains minimized settings, role grants,
all open PR heads, complete exact-head run inventories, reviews, and reasons. The
Markdown summary is for scanning. This command only calls GET APIs, writes its new
local report directory and always records `merge_authorized: false`. Collection
errors terminate without a report; an absent report must never count as ready.

For actual applicability audits, supply an operator-owned JSON map from PR number
to the existing `ci_integration_readiness.py` policy, plus a clean reviewed checkout
at the current main (native Linux, committed file bytes; no candidate checkout):

```sh
python3 tools/ci_product_report.py --out /tmp/qos-audit-UNIQUE \
  --policies /trusted/policies-by-pr.json --git-root /trusted/qos-base
```

Each policy must enumerate both `check.yml` and `lint.yml`, complete expected jobs,
allowed exact-head reviewers and the complete protected executable closure. The
report refuses selection of an older observed PR run or substitution of a push
run. It delegates fresh job/attempt, review, base, harness and zero-Rust proof
validation to the existing audited tool. Expected jobs are never copied from the
candidate's observed job list. A zero-Rust proof requires current-base committed
selector bytes and rejects deletion/rename/mode/type changes. Missing native
matrix rows remain failures, and only the explicit reviewed PR skips are allowed.

This command packages inventory and existing audits; it does **not** yet derive
complete native matrix policies automatically. A newer run may be created after
inventory, and main/reviews may change after collection. No snapshot is a merge
authorization. Product activation requires the executor's final fresh selection
and server checks, not a saved report.

## Smallest activation change set for review

1. **Trusted policy and executor, before a product token.** Implement product mode
   as separately reviewed code, leaving the existing private command hard-denied.
   Generate complete expected job names from reviewed workflow/selector inputs,
   independently of candidate output; classify security, CI/build policy, signing,
   packaging and harness edits for explicit policy review. Pin the executable
   closure (workflows, invoked helpers, configuration and transitive imports).
   Changes to the closure cannot certify themselves. First run this executor in
   GET-only shadow mode on one fresh routine PR. The existing report is usable now;
   automatic policy derivation and product mutation remain unfinished work.
2. **One operator-owned service.** Use a single service outside candidate Actions,
   fixed reviewed code/image and read-only source inputs, durable intent journal,
   and one-at-a-time merge admission. Reuse the pilot's intent-before-POST and
   GET-only uncertain-result reconciliation. Never mount its token into a build
   job, checkout candidate code in its runtime, or load policy from the PR. The
   service needs repository/API read and a narrowly scoped merge credential; no
   cluster-admin, organization owner, registry publication or signing permissions.
   Verify existing candidate-visible credentials cannot edit branch protections
   or impersonate this service before activation. Their effective grants have not
   been established by this report; runner cache isolation is not that proof.
3. **Product grant.** Add only user81 `qos-integrator` as a `write` collaborator to
   repository23 and provision a separate product-repository-scoped token using
   the qualified pilot's repository write API scope. Keep the private pilot token
   separate. Read back actual identity and effective role; token labels supplied
   by a caller are not authority. No additional grant to the reviewer is needed.
4. **Exact `main` protection.** Set `enable_push=false`, `apply_to_admins=true`,
   `enable_merge_whitelist=true`, `merge_whitelist_usernames=["qos-integrator"]`,
   `merge_whitelist_teams=[]`, `required_approvals=1`,
   `enable_approvals_whitelist=true`, `approvals_whitelist_username=["claude"]`,
   `approvals_whitelist_teams=[]`, `dismiss_stale_approvals=true`,
   `block_on_outdated_branch=true`, and `block_on_rejected_reviews=true`.
   Enable required statuses for the live observed PR contexts
   `CI / affected packages / checks (pull_request)` and
   `CI / fast quality / quality (pull_request)`; retain real run/job auditing as
   the stronger admission condition. Status success alone is forgeable by existing
   workflow credentials and is insufficient. Do not use the pilot's `pilot/ready`
   context in product. Snapshot existing settings first; currently there are none.
5. **One approved canary, then routine operation.** Fresh exact-head independent
   approval, current successful complete check/lint attempts, current-main ancestry,
   stable harness and no scope escalation are mandatory. Send FF-only with exact
   `head_commit_id`, `force_merge=false`, `merge_when_checks_succeed=false`. Require
   exact merged SHA and actual integrator actor on readback. Preserve every main
   SHA's check/lint workflow and the heavy-only coalescer policy. Do not cancel
   manual hardware jobs, disable tests or accept a missing observation.

No new custom queue engine is needed. If main advances, stop that candidate once,
mark it stale, and return it for one new base/update/review/CI cycle. Do not repeatedly
rebase, recreate commits, enqueue duplicate runs or silently move an approved head.
Workflow-native auto-merge was observed to follow moving heads; it is not the
exact-head executor for this proposal. Final-read rerun races remain a limit of the
current APIs; server protection reduces the gap but does not make snapshots atomic.

## Rollback

Stop the new integrator service first and revoke **only its product token**. Remove
its product collaborator write grant if pausing automation indefinitely. Retain
main protections and per-SHA verification by default; a paused service should
block automated merges, not reopen direct pushes. Emergency manual restoration of
merge authority must be an explicit operator change with saved settings and audit
evidence, not an automatic fallback. The preactivation empty protection array is
an exact rollback record, **not** a recommended automatic rollback target.

An uncertain merge is reconciled by GET and its persisted exact intent, never
retried with a new journal. Read-only reports remain available after rollback.
No cancellation of unrelated CI, scheduled image candidates, manual hardware
tests, app publication, or OS release requests is part of rollback.
