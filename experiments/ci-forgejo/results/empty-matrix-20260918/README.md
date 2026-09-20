# Empty matrix qualification

Private pilot repository only; no product workflow was pushed by this probe.
Both fixtures copy the production aggregate Bash verbatim. Selection emits a
fixture output rather than invoking Cargo; security success is a fixed premise.
Local selector and aggregate tests separately exercise real selection and
security failure. Python Alpine is a mutable diagnostic image, not a release
reproducibility claim.

- Run 22644: successful selection emitted `include: []`. Forgejo removed the
  affected job, leaving checks blocked. Retained records show that state; the
  run was subsequently cancelled before launching replacements.
- Run 22645: failed selection caused the incomplete matrix and checks to fail
  without assigning runner tasks. The aggregate Bash did not execute.
- Run 22660: use an inert runner row when `rust_required` is not true. The
  job-level condition skipped affected without a task; checks executed the
  production Bash and succeeded. This is the production workaround.
- Run 22661: the same workaround still fails closed after selector failure.
  Forgejo propagates failure before executing aggregate Bash, so the custom
  failure explanation is unavailable in this case.

The checked-in fixtures, exact commit IDs, and server run/job records preserve
both the failed approach and replacement. Verify the replacement with:

```sh
python experiments/ci-forgejo/verify-empty-matrix-evidence.py
```

The server behavior, not the local Bash tests, is why the workflow needs a
sentinel matrix despite having a job-level `if` condition.
