# Non-admin integrator qualification

Live private-pilot test on 2026-09-18 UTC (2026-09-19 local time).
Repository: `qos/ci-workflow-pilot-20260918-9245-repeat`, PR #5.
No product repository grant or branch-protection change was made.

Created restricted, non-admin `qos-integrator` (user 81), with repository
collaborator `write` permission only on this pilot. Token 145 is restricted to
that repository with `read:issue` and `write:repository` scopes. Credentials
were written directly from memory into Kubernetes Secret
`forgejo/qos-integrator-pilot` (keys `username`, `password`, `token`); no credential
is included in these files or command output. The first token request included
`read:user` and was rejected before issuing a token: repository-restricted tokens
permit only repository/issue scopes. Final token inventory contained only 145;
there were no unused issued tokens to revoke. It remains available for this pilot.

The pilot protection now disables direct pushes, applies to administrators,
whitelists only `qos-integrator` for merges, requires one `claude` approval,
dismisses stale approvals, blocks outdated branches, and requires `pilot/ready`.
The earlier protection is retained in `protection-before.json` for reconstruction.

Observed results:

| Operation | Result |
| --- | --- |
| Integrator reads pilot | HTTP 200; admin false, push/pull true |
| Integrator reads product repository | HTTP 404; no product access |
| Integrator tries same-value protection edit | HTTP 403; requires repository admin |
| Author/admin `xemmerling` writes directly to pilot main | HTTP 403 |
| Integrator tries exact-head FF before approval | HTTP 405; insufficient approvals |
| Author/admin tries exact-head FF after approval | HTTP 405; user not allowed to merge |
| Integrator tries exact-head FF after approval | Merged; `merged_by=qos-integrator`, final main equals exact requested head |

Candidate `92714aab674c7c3da23aef64bf9b286e0dde05f5` adds only a non-executable
one-line fixture file. Agent `review_ci_plan` independently inspected its full
diff and approved fixture-only eligibility. The `claude` browser identity then
submitted exact-head fixture approval. `reviews.json` retains server evidence.
The green status is synthetic permission-test input, not a real CI result.

Evidence limits: the first author merge check expected 403 but got 405; its exact
body appeared in tool output and is retained as a labeled observation in
`author-merge-denied.json`, not represented as raw HTTP headers. The integrator
merge returned an empty success body, which the harness incorrectly tried to
parse as JSON. No merge retry was issued: subsequent PR and main readbacks prove
completion and identity, recorded in `merge-readback.json`. A retained HTTP status
for that success is not claimed. Future harnesses must journal before assertions
and accept empty success bodies.

This proves useful server enforcement with a non-admin merge identity. It does
not qualify production automation, CI provenance/runner trust, concurrency races,
or ordinary-agent credentials. The author and reviewer are still owner/admin
identities that can reconfigure protections administratively; `apply_to_admins`
enforced these merge/push requests but does not remove their policy-edit power.
Keep this credential out of candidate jobs. Do not enable product restrictions
until agents have a working branch-first landing path.
