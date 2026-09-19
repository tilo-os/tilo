# Trusted private landing CLI qualification

Repository: `qos/ci-workflow-pilot-20260918-9245-repeat`. No product repository
grant, protection, branch or release changed in this experiment.

1. PR 6 established the trusted `.forgejo/workflows/landing-fixture.yml` and a
   bootstrap data file at `769e0d1c3040834a6da7095f5a31a9a5b39f2ee8`.
   Independent agent `review_ci_plan` reviewed the exact diff and actual successful
   push run 22849 (job 83902). Existing `claude` identity submitted approval 55.
   Existing restricted `qos-integrator` performed one immediate exact-head
   fast-forward merge; HTTP 200 and subsequent readback agreed. This explicit
   bootstrap establishes the harness; it is not evidence of the final CLI yet.
2. PR 7 changed only `landing-data.txt` from `fixture:bootstrap` to
   `fixture:candidate`, head `c34a3b29ab4818dfa78a55033fddc6a718fc8e24`.
   The final policy pins the trusted bootstrap revision and complete `.forgejo`
   executable closure, expected push run 22878 and its exact `fixture-tests` job.
   Bare Git objects were fetched outside the CLI; no candidate code was checked out
   or executed by the trusted collector. The workflow validates exact-head data
   through the API before posting its readiness status. Status alone is not used
   as CI evidence by the CLI.
3. The initial CLI dry run rejected missing approval and waiting CI. Run 22878
   later succeeded (job 83990/task 75348). The independent reviewer refreshed the
   exact data-only diff and successful check. `claude` approval 56 names that head.
4. A new dry-run journal reports `dry_run_ready`. A separate `--execute` invocation
   performed its own fresh double collection, Git provenance and protection
   checks, persisted intent, sent one immediate expected-head fast-forward request,
   and reconciled HTTP 200 against PR/main. The result is `merged_verified` with
   head, merge SHA and main all matching the candidate; actor is `qos-integrator`.

The reviewer credential was read from existing operator configuration in memory;
its authenticated `/user` identity was verified as `claude` ID 5. The merge token
came from existing Kubernetes Secret `forgejo/qos-integrator-pilot`, captured and
decoded in a trusted process and passed directly to CLI stdin. No credentials are
included in journals/files/command arguments. No account, token or grant was added.
Repository-scoped token identity depends on this trusted provisioning route and
the sole-integrator server whitelist; the tool does not claim `/user` scope.

Duplicate private-main push runs 22876 and 22904 were cancelled after their exact
heads had already passed candidate runs; neither had a worker task when inspected.
Final candidate run 22878 is successful. No product run was cancelled.

Verify retained evidence with:

```sh
python experiments/ci-forgejo/verify-trusted-landing-evidence.py
python -m unittest discover -s tools/tests -p test_ci_pilot_land.py
python -m unittest discover -s tools/tests -p test_ci_integration_readiness.py
```

Nine landing tests and eighteen readiness tests pass on Linux. Tests cover changed
head/base/current attempts, forged green status over failed execution, role and
protection mismatches, fixed merge arguments, redirect refusal, uncertain response
reconciliation and refusing to reuse a prior journal for another POST.

Limits: this is a tiny data fixture using a mutable diagnostic Python image.
Author/reviewer identities remain administrators; the integrator is restricted.
Runner isolation, product policy completeness, final-read-to-merge rerun races,
and production automatic integration remain unqualified. The CLI deliberately
hard-denies product repositories. A successful pilot does not remove those limits.
