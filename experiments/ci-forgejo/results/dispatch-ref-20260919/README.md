# Immutable workflow dispatch refs

Private pilot only, 2026-09-18 UTC. Exactly two tiny `runner-quality` jobs ran;
both completed successfully. No product workflows, tags, protections or merges
changed. The installed dispatch schema calls `ref` a Git reference and supports
`return_run_info`; it does not restrict the schema to branch/tag names.

Sequence:

1. Create dispatch-only `dispatch-ref.yml` at commit A:
   `cb30bcdcd5b9872b8c985c10f6d39b77886b6ae1`.
2. Advance its branch to B:
   `0db8e250dede46da649942d66d2418bf1ff38dce`.
3. Dispatch once with the full SHA A, then once with the branch name.
4. Advance the branch again to C before either job starts:
   `66b6c27deff467eae8d77475da90e3e8e512052c`.

Both requests returned HTTP 201 with their internal run ID, UI run number and
job names. The retained job logs agree with the server-owned run records:

| Requested ref | Run / job / task | `github.sha` | `github.ref` |
| --- | --- | --- | --- |
| Full immutable SHA A | 22799 / 83752 / 75175 | A | bare SHA A |
| `probe-dispatch-ref` | 22800 / 83753 / 75176 | B | `refs/heads/probe-dispatch-ref` |

Thus this installed Forgejo accepts a full immutable SHA directly, including a
historical commit after its branch advances. Named-branch dispatch resolves the
source head when dispatch is accepted; later branch movement does not change the
run's SHA or the job's `github.sha`.

For continuous packaging, dispatch with `ref=<full requested SHA>` and
`return_run_info=true`, then verify the returned run's `commit_sha` before tracking
its result. Avoid searching broad run lists when the server returns an exact ID.
Check out `github.sha` explicitly where the workflow needs source. This fixture
reported context only; it did not exercise checkout-action behavior or vary the
workflow definition between A and B.

Two integration details matter:

- Immutable dispatch sets `github.ref` to the **bare SHA**, not
  `refs/heads/main`. Existing branch-name guards need deliberate handling; do not
  disable release/signing guards merely to make SHA dispatch work.
- For these dispatched runs, REST `event` was empty while `trigger_event` was
  `workflow_dispatch`. Classifying dispatch from `event` alone is incorrect.

`setup.json` retains exact requests, statuses and returned IDs;
`after-advance-runs.json` records the queued run heads after movement;
`final-runs.json` and selected job logs prove final execution context. The small
Python image uses a mutable diagnostic tag; this establishes Forgejo reference
semantics, not container reproducibility or release readiness.
