# Live concurrency:false queue probe

Tested 2026-09-18 UTC against the private repository
`qos/ci-workflow-pilot-20260918-9245-repeat`; branch name ends in
20260919 because local time was near the date boundary. No product branch changed.

Three content commits in four seconds created runs 22631, 22632, 22633.
The exact workflow is retained in workflow.yml: one fixed workflow-level group,
`cancel-in-progress: false`, one small Python-container job sleeping 90 seconds.

Observation 2 and jobs-active show **one running plus two waiting**, with
no superseding of the middle pending run. Therefore the tested configuration
must not be relied on to implement active-plus-latest-pending coalescing on this
instance. This does not establish whether serialization itself is implemented:
runner capacity could also explain the waiting jobs. It does not test
`cancel-in-progress: true`, job-level concurrency, or newer Forgejo versions.

All three probe runs were explicitly canceled. final.json confirms terminal
cancelled for each. The middle and newest jobs began as the previous jobs were
canceled, consistent with the middle run remaining in the backlog.

Files are minimized API snapshots. setup.json binds commits to the private
branch; cancel-responses.json retains cancellation HTTP responses. Synthetic
fixture results establish scheduling behavior only, not application validity.
