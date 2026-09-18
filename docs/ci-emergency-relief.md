# CI emergency relief: deployed state and recovery

Recorded 2026-09-18. This is the emergency delivery record, not a claim that
current main CI is green. It supersedes older **branch-only** descriptions of
the changes listed here; the broader CI/build/release roadmap remains unfinished.

## Delivered changes

Deployment was confirmed by the coordinating emergency task. Commit contents
were inspected in the product and infrastructure checkouts. Infrastructure
health below is the infrastructure operator's reported snapshot, not a permanent
health guarantee.

| Repository / revision | Delivered behavior |
| --- | --- |
| `qos/qos` `e6449447a` | Stops broad per-main-push/PR full OS and distribution build fan-out. Preserves explicit release/manual paths and scheduled backstops; PR desktop/app planning or contracts remain lightweight. Continuous OS publication is disabled. |
| `qos/qos` `8e704f963` | Keeps macOS definitions deferred without requiring those lanes for enabled releases; adds bounded waiting-run coalescing. |
| `qos/qos` `e6f0e61aa` | Deployed integration checkpoint containing the relief changes and upstream work. This is a merge commit, not a standalone feature to revert wholesale. Includes `3fd676546`, which preserves every main commit's check/lint work during queue cleanup. |
| `qos/qos` `e0c6484e6` | Deployed follow-up integration checkpoint: task-branch instructions, runtime artifact layout repair, confined audio dependency helper, narrow design-only Rust applicability, fail-closed aggregate with the Forgejo sentinel-matrix workaround, and rename-owner coverage. Includes unrelated upstream work; do not revert the whole merge. |
| `qos/qos` `e279fbf35` | Later deployed checkpoint includes focused development helpers (`8f768b116`) and per-main-SHA check/lint concurrency groups. Deployment is not evidence that its actual CI passed. |
| Infrastructure `93be3f6` | Enables `forgejo/qos-ci-queue-coalescer`, using the heavy-only policy. |
| Infrastructure `8c1de67` + `4c20a8e` | Pins the attempted broker fix image and permits bounded startup reconciliation before liveness enforcement. Operator reports Argo applications synced/healthy at `4c20a8e`, but subsequent runtime inspection found the performance guard was not loaded; see below. |
| Infrastructure `506195f` | Deploys the corrected image that patches the imported runtime module, retaining the startup probe. Coordinating task confirms this runtime correction is live. |

Broker image:

```text
code.podesta.ai/podestaai/sandbox-broker@sha256:af62ba80f88fce080f1d267021d224810f54ce48332cb2df199ec534ed97eaee
```

The broker runs as Deployment `sandbox-broker` in namespace `sandbox-broker`.
Its deployment retains two replicas, rolling-update `maxUnavailable: 0`, a
240-second termination grace period and a startup probe allowing 36 attempts at
five-second intervals. Startup-probe recovery is healthy independently of the
performance patch. The `c568...` image patched a source-tree copy, while the running
broker imported its implementation from `site-packages`; the intended performance
guard therefore did **not** take effect. Infrastructure `506195f` replaces that
image with the imported-module correction shown above; the coordinating task
reports the correction live. End-to-end dispatch/feedback improvement still needs
actual observations. Image deployment and green
health probes do not establish that a particular code path changed. No Forgejo
server Deployment was changed by the coalescer rollout.

## What stays protected

- `check.yml` and `lint.yml` retain validation for every main commit. The
  coalescer never targets either workflow. Cheap feedback can still wait for
  capacity; preserving runs does not prove acceptable latency.
- The coalescer targets only superseded, unstarted **main push** runs of
  `build-image.yml`, `package-desktop.yml`, `release-apps.yml`, `fs-fuse.yml`
  and `legacy-app-e2e.yml`. It retains the newest eligible request per workflow.
- Manual hardware/bring-up requests, tags, PRs, other branches and work observed
  to have started are outside its policy. It rechecks the replacement, jobs and
  candidate before cancelling. Forgejo has no atomic cancel-if-still-waiting
  operation, so a small inspection-to-cancellation race remains.
- Full OS and packaging work is explicit or scheduled during capacity relief.
  Deliberate OS releases still require the explicit tag/manual publication path.
  This is temporary relief, not delivery of the continuous-packaging roadmap.
- Routine integration uses a task branch, coherent tested batches, an updated
  PR, independent review and applicable CI for the current head. Explicitly
  authorized emergency CI/infra fixes are the exception. These instructions do
  not by themselves deploy a trusted automatic merge service or branch protection.

The runtime handoff now transports `runtime/tilo-runtime/...` and the public
publisher key together. Fixture tests exercise both architecture layouts, not
full native signing/packaging acceptance. The audio helper's actual qualification
passed both native architectures in run 9230; this does not establish that all
tests on the latest main revision passed. Design-only skipping applies narrowly
to `docs/designs/*.md` changes; security and selection remain mandatory, mixed
changes retain Rust coverage, and explicit full checks override the skip.

## Current validation limits and next work

At 21:48 UTC, the coordinating task recorded the actual `e0c6484e6` check run
22677 (UI run 9242) and lint run 22678 (UI run 9243) as **waiting**. This document
does not upgrade those observations to green. Main can advance while they run;
match each run's full commit SHA before reporting its result.

The lean CI image and additional quality-job isolation are under development,
not deployed by the revisions above. An image recipe on disk is not a published,
qualified image. Last-green selection and broader scheduling/release qualification
also remain separate work. Retain explicit manual hardware runs while pursuing
these improvements.

The later product checkpoint `e279fbf35` is now live:

- Product commit `8f768b116` adds locally tested focused development contract
  commands and receipts and is included in that deployed checkpoint.
- Main check/lint concurrency groups changed from a shared branch key
  to the commit SHA, retaining `cancel-in-progress: false` on main. A live queue
  observation of one running and seven waiting runs motivated removal of that
  head-of-line block; it is not yet evidence that the deployed grouping improves
  latency. Branch/PR supersession behavior remains unchanged. Runner admission
  must still bound resource use; independent workflow groups do not create capacity.
- The broker's imported-module correction is deployed at infrastructure `506195f`.
  New-worker warm-builder preload and end-to-end performance qualification remain
  pending. Source patches or image tags alone do not satisfy that work.

After rollout, verify the imported module path and effective guard in the running
broker, then observe actual request/dispatch latency and new-worker image readiness.
Recheck the specific main CI runs; neither a healthy broker nor an idle runner is
a substitute for completed per-commit results.

## Observe before changing anything

Run these read-only commands on an operations host with the configured contexts:

```sh
fgj --hostname code.podesta.ai api repos/qos/qos/branches/main
fgj --hostname code.podesta.ai api repos/qos/qos/actions/runs/22677
fgj --hostname code.podesta.ai api repos/qos/qos/actions/runs/22677/jobs
fgj --hostname code.podesta.ai api repos/qos/qos/actions/runs/22678
kubectl -n forgejo get cronjob qos-ci-queue-coalescer
kubectl -n forgejo get jobs -l app=qos-ci-queue-coalescer
kubectl -n sandbox-broker get deployment sandbox-broker
kubectl -n sandbox-broker rollout status deployment/sandbox-broker --timeout=180s
```

Inspect a specific coalescer Job's logs before taking action. Do not print the
token Secret. The infrastructure policy and runbook live in
`apps/forgejo/ci_queue_coalesce.py`, `apps/forgejo/ci-queue-coalescer.yaml` and
`apps/forgejo/ci-queue-coalescer.md`; broker configuration lives in
`apps/sandbox-broker/deployment.yaml`. The inspected infrastructure checkout was
`C:/Users/xaver/infra-ci-emergency`.

## Stop or roll back narrowly

These are operator procedures, not commands executed by this document. Argo
automatically syncs and self-heals both `forgejo` and `sandbox-broker`; a live-only
patch is temporary unless its matching GitOps source change is committed and
published through the authorized infrastructure path.

**Stop coalescing first if its behavior is suspect:**

```sh
kubectl -n forgejo patch cronjob qos-ci-queue-coalescer --type=merge -p '{"spec":{"suspend":true}}'
kubectl -n forgejo get jobs -l app=qos-ci-queue-coalescer
```

Set `spec.suspend: true` in `apps/forgejo/ci-queue-coalescer.yaml` and publish that
matching change promptly so Argo does not undo the suspension. Suspension stops
future Jobs; it does not stop an already active helper. If necessary, inspect its
owner reference and delete **only that exact coalescer Job**, replacing the literal
placeholder below. Do not delete Forgejo runner Jobs or hardware workers.

```sh
kubectl -n forgejo get job COALESCER_JOB_NAME -o yaml
kubectl -n forgejo delete job COALESCER_JOB_NAME
```

Previously cancelled runs are not resurrected. If one is needed, inspect its
revision and event and explicitly rerun that single request in Forgejo. Never
cancel a repository-wide queue or all running builds as a rollback procedure.

**Broker rollback:** retain the startup probe; reverting `4c20a8e` would remove
the mitigation for the observed startup restart loop. If the new image itself is
the diagnosed cause, change only its image in GitOps, keeping the startup and
grace-period settings. The recorded previous image was:

```sh
kubectl -n sandbox-broker set image deployment/sandbox-broker broker=code.podesta.ai/podestaai/sandbox-broker@sha256:3526f0173d964d765840ac27cbc42d5f77dbe830a5e7dd2834171742b7a0a4d7
kubectl -n sandbox-broker rollout status deployment/sandbox-broker --timeout=180s
```

Publish the matching image-only GitOps change. Neither the previous image nor the
ineffective `c568...` source patch establishes resolution of legacy reconciliation;
rollback is a recovery option to assess, not an assertion of health. Reassess these
recorded rollback digests against the corrected `af62...` runtime now deployed.
Do not delete broker/runner pods or
interrupt active signing/hardware sessions to force a rollout.

**Product CI rollback:** use a new task branch and a narrowly reviewed fix to the
specific selector, helper or workflow. Keep per-commit check/lint triggers and
preserve manual/tag/scheduled heavy paths. Do not `reset` main or revert the merge
checkpoints above: they contain other contributors' work. Do not restore broad
heavy per-push triggers merely to obtain one artifact; dispatch that particular
workflow and target instead. Re-enabling continuous packaging needs a separately
qualified capacity/coalescing policy, not a blanket reversal of relief.
