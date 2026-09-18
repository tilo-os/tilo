# Read-only integration readiness audit

`tools/ci_integration_readiness.py` is a bounded diagnostic for a ready PR. It does
not merge, publish statuses, update branches, or change repository settings.
Run it from a trusted checkout with Git objects for current main and the candidate
already fetched. It never checks out or executes candidate code. Missing objects
fail observation; no candidate fetch is hidden inside the audit.

The operator supplies a JSON policy from outside candidate control:

```json
{
  "trusted_revision": "FULL_40_CHARACTER_SHA_OF_REVIEWED_POLICY",
  "protected_paths": [".forgejo/workflows", "tools", "scripts", "os/tests"],
  "reviewers": ["claude"],
  "runs": [
    {
      "workflow": "check.yml",
      "run_id": 12345,
      "events": ["pull_request"],
      "jobs": ["EXACT_EXPECTED_JOB_NAMES_FROM_TRUSTED_SELECTION"]
    }
  ]
}
```

This is an illustrative schema, **not a deployable policy**. Include every
applicable workflow and the exact job set, including matrix expansion, from
trusted affected-check selection. Do not copy observed job names into the policy
and call that completeness validation. Protected paths must cover the executable
workflow/helper closure, local actions, selection configuration and other policy
inputs. The tool compares complete Git tree entries under those literal paths,
including additions/deletions and file mode changes, against the pinned trusted
revision. It cannot infer that a hand-supplied closure is complete.

```text
python tools/ci_integration_readiness.py --repo qos/qos --pr 123 \
  --git-root /trusted/read-only-objects --policy /trusted/candidate-policy.json \
  --out /receipts/pr-123-head.json
```

A passing `observed_ready` means the current main is an ancestor of the candidate;
protected harness content matches the trusted revision; at least one allowed,
different-from-author reviewer has an official, undismissed exact-head approval;
no official outstanding change request exists; and the named server-owned
run/jobs succeeded on that exact head. It checks explicit job completeness,
repository/run identity, event and retry attempts. It collects twice and rejects
head/base/review/job changes between observations. No commit status is consulted.
The installed API schema exposes the jobs endpoint as an unpaginated array with
no page/limit parameters; missing jobs fail the explicit expected-set comparison.
Uniform attempt numbers and two stable observations do not independently prove
that these are the latest attempts. That needs separate server qualification.
Exit 2 means incomplete or rejected evidence. Receipts are created exclusively;
prior evidence cannot be overwritten accidentally.

Every receipt retains `merge_authorized: false`. This is intentional: snapshots
are not atomic authorization, runner isolation is unqualified, credentials are
not separated yet, and a path list alone cannot prove its own completeness.
Retries after the final read are still possible. A later integrator must rely on
server protections and exact-head immediate fast-forward merging, never accept a
stored receipt as a timeless capability.

## Smallest branch-first deployment

Read-only API inspection on 2026-09-18 found no `qos/qos` branch protection rules.
Fast-forward-only is available, but default merging is ordinary merge. The native
pilots already prove missing-review rejection, approval invalidation, exact-head
rejection, stale-base rejection and immediate fast-forward success. Native queued
auto-merge follows a new head; candidate jobs can forge required status contexts.

The smallest enforceable path is one trusted integration process outside candidate
jobs, using these native primitives. It polls ready PRs without occupying a build
runner, validates fresh evidence and immediately calls the merge API with
`Do=fast-forward-only`, `head_commit_id=<exact SHA>`, `force_merge=false` and
`merge_when_checks_succeed=false`. The installed `fgj pr merge` command does not
expose expected-head or fast-forward-only flags, so use the REST API directly.
Do not create a general scheduler for this responsibility.

Before enforcement, provide agents the working branch/PR/review/landing command
path and verify an ordinary-account fixture. Then configure main protection with
direct pushes disabled, enforcement for administrators, stale approval dismissal,
required independent approval, outdated-branch blocking and a merge whitelist
containing only the integration identity. Required CI status contexts provide
defense in depth; they cannot establish the issuer and are insufficient alone.
Protection changes need no Forgejo restart. Keep an explicit owner recovery path;
do not hand everyday agents an administrator credential that can remove policy.

Identity needs:

- Author agents: branch write/PR access; no main push or routine merge authority.
- Reviewer agents: allowed review identity different from PR author; no merge
  secret inside code execution. `claude` is currently available for this role.
- Integrator: a dedicated non-admin account/token permitted to merge main and
  read evidence, kept only in the trusted integration process.

Using `claude` as integrator is possible only with another reviewer identity for
PRs authored by `claude`, and a separately operated reviewer for real independence.
Both currently tested identities (`xemmerling` and `claude`) have owner-level
access; renaming their roles does not supply least privilege. A dedicated account
and ordinary-author permission test remain necessary. Never enable blocking
protection first and leave existing agents without a usable landing path.
