# Selected-job rerun eligibility and dependency effects

Private fixture run 22787 (UI #15) was rerun once more after its previous terminal
success. Its `switch` job 83703 became attempt 4, task 75193. While that exact
attempt was **running**, the same supported CLI command was invoked:

```text
fgj --hostname code.podesta.ai actions run rerun 15 --job switch \
  -R qos/ci-workflow-pilot-20260918-9245-repeat
```

The authenticated web operation was
`POST /qos/ci-workflow-pilot-20260918-9245-repeat/actions/runs/15/jobs/1/rerun`.
It returned HTTP 400: `The job cannot be rerun.` The selected job's attempt,
handle and task ID were unchanged, as was its successful prerequisite. That
single active attempt finished normally at 23:32:56 UTC; final run and jobs are
successful. No worker was destroyed, no active task was canceled, and no further
cancel probes were attempted.

The installed REST schema exposes whole-run cancellation, not selected-job
cancellation or rerun. Do not cancel the whole run to repair a single lost worker
when sibling work must be preserved. A running server task is not eligible for
this selected-job rerun operation even if its physical worker has disappeared.
Let Forgejo's native lost-task reconciliation reach a terminal state first, then
re-read the exact latest attempt before requesting one selected rerun. No custom
reaper was added.

## Partial reruns can reset blocked dependents

The first two-job pilot established that a successful **prerequisite** remains
unchanged; it did not establish that every sibling or dependent is untouched.
The root-authorized recovery agent separately retained production metadata for
failed security jobs 83674 (run 22781) and 83681 (run 22783). Copies here came from
its `qos-ci-relief/target/security-rerun-*.json` observations; this probe agent made
no product recovery mutations.

In each case, the selected failed security job advanced from attempt 1 to 2,
with a new handle and task 0 at enqueue. Its blocked dependent aggregate/report
jobs also advanced to attempt 2 while remaining blocked/task 0. Independent
builder, selector and architecture jobs kept their attempt/task identity.
Therefore validate the dependency closure after recovery, not just the requested
job, and never reuse an old aggregate receipt after a partial rerun.

The CLI takes the UI run number, not the database run ID, and accepts the exact
job name or zero-based job position. It has no expected-task/attempt compare-and-
swap parameter. A coordinator must record run head, selected job ID, attempt,
handle and task; re-read before a single request; and reconcile changed identity
after the request or a transport error. Do not blindly repeat a rerun request.
If someone has already advanced the attempt, observe that attempt instead.
