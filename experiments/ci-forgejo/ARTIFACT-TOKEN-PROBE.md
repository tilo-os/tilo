# Built-in task-token qualification

The private fixture `artifact-token-probe.yml` tests existing Forgejo behavior
without compiling software, publishing packages, installing credentials or
changing production workflow variables. Repository ID 59 is
`tilo/ci-workflow-pilot-20260918-9245-repeat`; the fixture lives only on
`codex/artifact-token-probe` there.

At source `6f909596b653ce4c49dac94637dcaf641541980d`, dispatch once with an empty
`baseline_run`, then dispatch the same source with the first successful run ID.
The second run must use its own built-in task token to list and download the
first run's small artifact, checking the exact ZIP inventory and bytes. Tokens
are passed only through the job environment and are never printed. HTTP
redirects and oversized responses are rejected.

Evidence on 2026-09-19:

- Initial run 22977 proved `GITHUB_RUN_ID` equals API run ID and
  `GITHUB_RUN_ATTEMPT` equals the current job attempt, and its token could read
  its run and job metadata. Upload failed because the Python image lacked Node.
- Corrected run 22982 succeeded and uploaded the 31-byte payload using the
  existing Node-capable runner image.
- Historical-read run 22984 succeeded. Job 84356 (task 75560, attempt 1) emitted
  `HISTORICAL_TASK_TOKEN_READ_PASS 22982 38732` after downloading baseline
  artifact 38732 with its own built-in token, without redirects, and checking
  the exact ZIP inventory and payload. Final run status was independently read
  as `success`.

Product-repository qualification also passed at source
`196ff1b0f` on the isolated packaging branch. Run 22993 produced artifact 38734;
run 23001/job 84408 (task 75579, attempt 1) emitted
`HISTORICAL_TASK_TOKEN_READ_PASS 22993 38734` and finished successfully. The
product probe uses the immutable `ci-fast` image and the existing container-pull
credential; artifact API calls use only its built-in task token. Run 22993's
container started 0.391 seconds after task receipt, with no image pull in its log.

The private fixture uses the existing runner image tag for an infrastructure API probe;
it is not a production artifact or verifier trust root. Even a passing result
does not prove generic-package registry access or partial-rerun attempt semantics.
Those require their own acceptance evidence. Product artifact read permission is
now established by the separate product probe above.
