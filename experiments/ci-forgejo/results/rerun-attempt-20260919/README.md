# Current job attempts and partial reruns

Private repository `qos/ci-workflow-pilot-20260918-9245-repeat`, run 22787
(UI #15), exact head `f2806833e594ae657ba25a91b7411ba37f1512df`.
Two small Python-container jobs use the dedicated `runner-quality` lane.
An earlier generic-lane run 22786 was canceled while both jobs had task 0;
it was replaced to avoid consuming native-build capacity. No product settings,
permissions, merge actions or publication were changed.

The installed API schema explicitly defines:

- `attempt`: number of attempts including the current attempt;
- `handle`: opaque identity of a single job attempt;
- `task_id`: latest task ID for the job.

There is no run-wide attempt field. The `/runs/{id}/jobs` endpoint exposes an
unpaginated array. `installed-api-schema.json` retains those definitions.

The fixture first succeeds in both jobs. We change the separate fixture state
branch to `fail`, then rerun only `switch` through `fgj actions run rerun 15
--job switch`. Forgejo 16 has no rerun REST endpoint; this CLI operation uses
the existing authenticated web UI identity. The workflow fetches the mutable
fixture mode only to create deterministic success/failure attempts; this is
deliberately not a reproducible-build or production trust test.

Observed snapshots:

| Snapshot | Run | steady job 83702 | switch job 83703 |
| --- | --- | --- | --- |
| first-green | success | attempt 1, success, task 75136 | attempt 1, success, task 75141 |
| second-immediate | waiting | same attempt/handle/task, success | attempt 2, new handle, task 0, waiting |
| second-failed | failure | same attempt/handle/task, success | attempt 2, task 75146, failure |
| third-immediate | waiting | same attempt/handle/task, success | attempt 3, new handle, task 0, waiting |
| third-running | running | same attempt/handle/task, success | attempt 3, task 75160, running |
| third-green | success | same attempt/handle/task, success | attempt 3, task 75160, success |

Run and job IDs remain stable. The API immediately replaces the rerun job's old
green attempt rather than appending historical attempts. The untouched job stays
green at its earlier attempt. Run status resets to waiting, with started/stopped
timestamps cleared, then reflects the new aggregate outcome. Counters therefore
must be checked **per job**, not required to match across an entire run.

The verifier now requires each current job's positive attempt, nonempty unique
handle, positive latest task ID and success status, plus the existing exact job
set/run/head/repository checks. It includes handles in the double-read evidence
fingerprint. Retained live snapshots test that an old success cannot mask the
subsequent pending/failed attempt, including a change between the two reads.
The recovered third attempt passes with current attempt counters `{1, 3}`.
All fixture work is terminal: superseded generic run 22786 canceled, quality
run 22787 successful after the third attempt. No unrelated runs were canceled.

Limits: this establishes current attempts within the explicitly selected run.
Choosing the authoritative run among independently-created run IDs remains a
trusted-policy responsibility. A later rerun can still occur after the final
read. No stored receipt is timeless authorization, and the tool continues to set
`merge_authorized: false`. Runner isolation and production integration remain
separate qualifications.
