# CI emergency relief: deployed state and recovery

Updated 2026-09-19 (production timing observed 2026-09-18 UTC). This is the
emergency delivery record, not a claim that current main CI is green. It
supersedes older **branch-only** descriptions of
the changes listed here; the broader CI/build/release roadmap remains unfinished.

## Delivered changes

Deployment was confirmed by the coordinating emergency task. Commit contents
were inspected in the product and infrastructure checkouts. Infrastructure
health below is the infrastructure operator's reported snapshot, not a permanent
health guarantee.

| Repository / revision | Delivered behavior |
| --- | --- |
| `tilo/tilo` `e6449447a` | Stops broad per-main-push/PR full OS and distribution build fan-out. Preserves explicit release/manual paths and scheduled backstops; PR desktop/app planning or contracts remain lightweight. Continuous OS publication is disabled. |
| `tilo/tilo` `8e704f963` | Keeps macOS definitions deferred without requiring those lanes for enabled releases; adds bounded waiting-run coalescing. |
| `tilo/tilo` `e6f0e61aa` | Deployed integration checkpoint containing the relief changes and upstream work. This is a merge commit, not a standalone feature to revert wholesale. Includes `3fd676546`, which preserves every main commit's check/lint work during queue cleanup. |
| `tilo/tilo` `e0c6484e6` | Deployed follow-up integration checkpoint: task-branch instructions, runtime artifact layout repair, confined audio dependency helper, narrow design-only Rust applicability, fail-closed aggregate with the Forgejo sentinel-matrix workaround, and rename-owner coverage. Includes unrelated upstream work; do not revert the whole merge. |
| `tilo/tilo` `e279fbf35` | Later deployed checkpoint includes focused development helpers (`8f768b116`) and per-main-SHA check/lint concurrency groups. Deployment is not evidence that its actual CI passed. |
| `tilo/tilo` `0975d89a8` | Deployed checkpoint containing `f660a53a4`: lean pinned fast-quality image, native toolchain smoke, and the exclusive `runner-quality` lane. Its actual production lint run 22696 passed; the separate check run 22695 is not claimed green here. |
| `tilo/tilo` `f2f4664a4` | Deployed follow-up checkpoint includes isolated Clippy target/cache corrections and explicit lean images for status/report jobs. It contains unrelated upstream changes; do not revert the whole merge. |
| `tilo/tilo` `ec3facc4b` | Deployed checkpoint includes lightweight quality-lane builder resolution/control and affected-package selection. Native compilation and security jobs still use the full builder. |
| Infrastructure `93be3f6` | Enables `forgejo/tilo-ci-queue-coalescer`, using the heavy-only policy. |
| Infrastructure `8c1de67` + `4c20a8e` | Pins the attempted broker fix image and permits bounded startup reconciliation before liveness enforcement. Operator reports Argo applications synced/healthy at `4c20a8e`, but subsequent runtime inspection found the performance guard was not loaded; see below. |
| Infrastructure `506195f` | Deploys the corrected image that patches the imported runtime module, retaining the startup probe. Superseded by the current broker below. |
| Infrastructure `a30e1c8` | Adds one exclusive on-prem `runner-quality` slot, with a 2 CPU / 4 GiB Docker-in-Docker workload envelope; general/heavy jobs cannot use its reserved capacity. |
| Infrastructure `ffbd0a1` | Adds completed-cleanup tolerance and live-runner deletion guards, retained in later images. These guards do not prove the cause of historical runner loss. |
| Infrastructure `3e2e8ba` | Deploys repository-scoped full-builder seed clones with broker image `48a5bf...`. Actual repository 23 snapshot cloning is proven; its first observed job used a public image, so this is not private-builder cache-hit evidence. |
| Infrastructure `a970175` | Deploys broker `c80bd455...` with both AX41 v2 seeds. Both replicas Ready with zero restarts at acceptance. Normal repository 23 control and lint jobs received isolated snapshot clones and created their pinned containers without a pull event; timings below. |
| Infrastructure `64d2359` | Broker `ef490a74...` disables unrelated generic pre-pull only after verifying Tilo repository scope. Global reserve, readiness and lifecycle behavior remain. Both replicas Ready with zero restarts; 316 installed-runtime tests passed. |
| Infrastructure `021fb14` | Broker `8d313aa7...` adds priority 1000 with `preemptionPolicy: Never` only for actual disk-heavy demand, not speculative reserves. Both replicas Ready; 326 installed-runtime tests passed. EKS bootstrap persistence follows in `d1fdf88`. |
| Infrastructure `46e7edc` | Broker `05cdedb0...` introduced fresh ephemeral scratch formatting with `mkfs.ext4 -m0`. Existing disks remain untouched. Retry 9404 subsequently passed the unchanged 80 GiB admission guard with 84.13 GiB available. |
| Infrastructure `0370999` | Broker `51b50312...` accounts for the selected RuntimeClass in admission, including ARM's actual 7 GiB / 1 CPU overhead. Both updated replicas were Ready; 341 installed-runtime tests passed. Existing workers are not resized. |
| `tilo/tilo` `e29c756c7` | Moves bounded security invariants to reserved quality capacity. Production job 84070 passed all 64 checks in 74.41 seconds; cached container creation took 297 ms. |
| `tilo/tilo` `5ade281b0` | Generates unpublished app receipts with existing canonical Rust signature verifiers. Receipt and handoff tests passed; continuous publication remains paused and final publication receipts still require qualification. |
| `tilo/tilo` `ce0eff803` | Includes `6625e7ad3`, updating the stale literal app-builder guard to verify the resolver, output and job dependency. Both complete shell guard suites and four regression tests passed; this is not a claim that all live CI at this checkpoint passed. |

The broker image is pinned by digest in the infrastructure repository.

The broker runs as Deployment `sandbox-broker` in namespace `sandbox-broker`.
Its deployment retains two replicas, rolling-update `maxUnavailable: 0`, a
240-second termination grace period and a startup probe allowing 36 attempts at
five-second intervals. Startup-probe recovery is healthy independently of the
performance patch. The `c568...` image patched a source-tree copy, while the running
broker imported its implementation from `site-packages`; the intended performance
guard therefore did **not** take effect. Infrastructure `506195f` replaces that
image with the imported-module correction. The current `0370999` deployment
retains that correction, the exclusive quality lane, and `ffbd0a1` cleanup guards
that tolerate completed resources and protect live runners. Green probes and
source changes alone do not prove end-to-end improvement; the production
measurement below is the evidence for the fast-quality path. No Forgejo server
Deployment was changed by these coalescer/broker rollouts.

The existing manual OS request 9331 received a fresh disk-demand worker at
23:29:52 UTC with priority 1000 and preemption disabled. It was still Pending
without a node at 23:32 and actually started at 23:40:50: priority does not create
capacity or evict active work. Its subsequent disk-space failure and fresh-worker
recovery procedure are described below.
Do not reproduce the attempted pending-pod replacement procedure: with the
original Job's `backoffLimit: 0`, deleting even an unstarted adopted pod exhausted
the Job. No active build was interrupted; normal broker reconciliation created
the fresh prioritized worker after the exhausted Job was cleaned up.

Separate security jobs 83674 and 83681 lost cloud workers. Forgejo's existing
zombie-task reaper marked them failed at 23:28:57 UTC; each security job was then
rerun once, preserving its compiled sibling jobs. The underlying host/runtime
failure remains under investigation. Likewise, AWS workers without snapshot
seeds still pull the full builder: the on-prem warm measurements below do not
establish fast cloud startup.

### Fresh scratch filesystem recovery

Manual OS run 9331 exposed a 100 GiB ephemeral data filesystem whose default
root reserve consumed about 5 GiB. After tooling, only 79.135 GiB was available,
below the **unchanged 80 GiB** admission guard. Broker `05cdedb0...` now uses
`mkfs.ext4 -m0` when initializing a fresh scratch disk. This changes disposable
CI data filesystems, not OS root-filesystem policy or already formatted volumes.

For a targeted retry:

1. Confirm the failed request's exact revision, target and terminal job state.
   Preserve its logs and checkpoints; leave independent active jobs alone.
2. Confirm the deployed broker image above, then inspect the newly created
   worker's manifest for its requested 100 GiB PVC and fresh-format `-m0` command.
   An existing worker does not acquire the change merely because the broker rolled.
3. Read the actual build's free-space/admission receipt after tooling installation.
   Require at least 80 GiB at the checked data path; PVC capacity alone is not proof
   of usable filesystem space. Retry 9404 recorded `admitted=true` with
   90,339,323,904 bytes (84.13 GiB) available. This proves initial admission,
   not that the complete OS build will fit or finish successfully.
4. If it still fails, inspect the mounted filesystem, used bytes and provisioning
   result before another targeted retry. Do not lower the guard, reformat an
   existing disk, run reserve-changing commands against active volumes, or delete
   a worker to force this update. Preserve manual hardware/signing work.

Cloud inspection also found QEMU guest memory backed by one shared `/dev/shm`
filesystem with a 31 GiB ceiling, below the admitted guest-memory total. A captured
KVM `Bad address` failure and backing-file pressure support this failure mechanism;
they do not explain every historical worker loss. Both existing AMD64 CI nodes
now have a verified 60 GiB ceiling on the same filesystem, with mount security
flags and running QEMU processes preserved. The current eligible CPU/profile
combinations admit at most 54 GiB of guest backing; changing those profiles or
instance sizes requires recalculating that bound. Fresh-node persistence is still
being qualified, so replacement nodes must not be assumed fixed.

Correct QEMU cgroup placement is a separate, **not fleet-deployed** change. An
isolated canary with a conservative 31 GiB Pod cap completed cold extraction,
Rust commands and a 180-second hold; measured peak was 12.62 GiB. Original node
configuration and temporary resources were restored. A second canary tests the
ordinary envelope with a 14 GiB Pod cap and bounded anonymous-memory load;
its result must be checked before adopting the lower overhead. Reservation taint
isolation and autoscaler protection are required for either test. AWS EBS seed support/pilot remains
**disabled** pending safe runtime accounting, capacity and actual clone/image-reuse
acceptance. Neither the scratch fix nor on-prem seed timings qualifies AWS reuse.

## What stays protected

- `check.yml` and `lint.yml` retain validation for every main commit. The
  coalescer never targets either workflow. Cheap feedback can still wait for
  capacity; preserving runs does not prove acceptable latency.
- The coalescer targets only superseded, unstarted **main push** runs of
  `build-image.yml`, `release-desktop.yml`, `release-apps.yml`, `ci-filesystem.yml`
  and `ci-legacy-apps.yml`. It retains the newest eligible request per workflow.
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
tests on the latest main revision passed. Rust skipping applies narrowly to
`docs/designs/*.md`, the explicit audited CI/developer-prose allowlist, and the
verified helper/control mappings in `tools/ci/affected_packages.py`; mixed
changes retain Rust coverage and explicit full checks override the skip. On pull
requests only, security may also skip when every changed path is existing audited
prose or an exact `QUALITY_ONLY_HELPERS` source/test path with its observer
present. Workflow/control, OS, security, manifest, unknown, mixed, deleted, and
unobserved-helper changes keep security required. Main and manual runs require
security, and the aggregate requires successful builder and selector jobs before
accepting either intentional skip.

## Current validation limits and next work

The earlier deployed product checkpoint `0975d89a87367b8625e8bb3b6d898cf997a684af`
provided the following historical timing evidence.
Its production lint [run 9257](https://code.tilo.so/tilo/tilo/actions/runs/9257)
(API run 22696, job 83334) finished **successfully**. The timing record in
`target/main-fast-quality-timing.json` and a fresh exact-run API readback agree:

| Production lint measurement | Observed value |
| --- | --- |
| Created / started / stopped (UTC) | 22:13:26 / 22:13:27 / 22:15:41 on 2026-09-18 |
| Queue wait | 1 second |
| Run wall time, started to stopped | 134 seconds |
| Image pull request to container creation | 9.905 seconds |

The pull interval includes work between those two log events; it is not an
isolated network-bandwidth measurement. This is one successful production run,
not a latency guarantee for every future worker. The separate main **check** run
22695 is not claimed green. Match each run's full SHA before reporting results
as main advances.

The lean image
`registry.tilo.so/tilo/ci-fast@sha256:fd2d462eaa130052fded4fbb05c2e9c7e44b6d6fa70b3bc6eae484447920551a`
contains the pinned Rust toolchain and lint tools, without the full builder's
native graphics/media/application stack. Compressed layers total 393 MB versus
1.694 GB for its source builder. Its unchanged OpenSSL exclusion invariant
passed; no runtime-policy exemption was introduced.

Before production rollout, the complete lint workflow passed on the exclusive
2 CPU / 4 GiB lane in [run 9253](https://code.tilo.so/tilo/tilo/actions/runs/9253).
That qualification took 3m49s end-to-end, including about 93s pull/unpack and 76s
dependency-usage analysis. Another image pilot measured a 36s pull interval.
These observations show material worker/network variation; the 9.905s
production observation must not be generalized into a universal cold-start bound.
Lint, the final check aggregate, and receipt jobs now use `runner-quality`.
Builder resolution now uses the pinned `ci-control` image and selection uses
`ci-fast`, both on the quality lane. Applicable native compilation and security
checks retain their full-builder/architecture lanes. Native control qualification run **22746**
passed, including a roughly **172-second cold pull**. This proves the control path
works; it does not demonstrate consistently fast cold provisioning.

The actual main check run **22755**, for `ec3facc4b`, subsequently completed builder
job **83538** and selector job **83539** successfully (API observation 22:53 UTC).
Selection requested three contract packages on both native architectures. At that
observation, security/native jobs were still waiting and the aggregate was blocked.
This qualifies lightweight planning in the real pipeline, not the whole check run.

Later production lint run **22728** also passed, taking **342 seconds**, including
about **185 seconds** between image pull and container creation. The earlier
134-second result remains valid for that run, but is not a universal latency
claim. Keep queue time, image acquisition and actual checks separate when comparing
workers; a lean image alone does not eliminate cold-pull variance.

The reservation is one total on-prem quality slot: busy, idle and pending quality
guests all count, and general/heavy work cannot consume its reserved capacity.
This prevents heavy builds from occupying every feedback slot; it does not make
queueing impossible under sustained quality demand. Main check/lint concurrency
uses the commit SHA and keeps `cancel-in-progress: false`, preserving every main
commit's result. The heavy-only coalescer still excludes both workflows.

The earlier image pilot (run 22684) lost its runner heartbeat and ended in
failure without a final step result. Its log/evidence remains retained. Cleanup
guards are now deployed at `ffbd0a1`, but the historical loss is **not proven** to
have been caused by cleanup, OOM, or the image. Do not recast a later green run
as a diagnosis of that earlier failure.

An isolated full-builder disk-clone experiment reported a reduction from about
222 seconds to 1 second. This is **prototype evidence**, separate from production
lint's lean-image result. Full-builder seed cloning is now deployed at `3e2e8ba`,
and an actual repository 23 request received its isolated snapshot clone. That
job used a public image, so private-builder warm reuse remains unproven by it.
A trusted read-only seed must produce an isolated
writable disk per job; never share a mutable Docker root or copy candidate output,
job credentials or signing material into a seed. ARM64/EBS restore needs its own
measurement; the on-prem result does not establish equivalent cloud timings.

The second AX41 node's three-image seed experiment reported an **18-second clone**
and all three image pulls below **one second**. Infrastructure `d600500` configures
that verified seed. Production v2 runtime and normal-job acceptance subsequently
passed at infrastructure `a970175`: both AX41 nodes supplied repository 23 with
isolated writable snapshot clones under broker `c80bd455...`; both broker replicas
were Ready with zero restarts at the observed acceptance point.

Normal main revision `6d221fd9` supplied these actual workflow observations:

| Request | Pinned image | Received to container creation | Observed result |
| --- | --- | --- | --- |
| Check 22761, builder job 83565, task 75054 | `ci-control@sha256:cc33624d156302fa0c62c065b9babf1d18c6d3103116ff9b0ecf4e0f316d9ce5` | 22:55:12.230 to 22:55:12.766 UTC: 535.6 ms | Job succeeded at 22:55:27.021, about 14.8 seconds after receipt |
| [Lint 22762](https://code.tilo.so/tilo/tilo/actions/runs/9318), job 83571, task 75056 | `ci-fast@sha256:fd2d462eaa130052fded4fbb05c2e9c7e44b6d6fa70b3bc6eae484447920551a` | 22:55:20.276 to 22:55:20.620 UTC: 343.9 ms | Succeeded at 22:57:28.187: 127.911 seconds from task receipt through policy, cargo-deny and cargo-machete |

Neither log contained a Docker pull event. These intervals are **task receipt to
container creation**, not pull-to-create or isolated network timings. They prove
warm startup for these normal pinned control/quality jobs. The complete lint run
is now successful; the complete check run is not claimed green. The linked UI run
9318 corresponds to API run ID 22762 and exact main SHA
`6d221fd91261761bfbadbc36b3ff30f901857a9c`. Cloud/ARM cold startup remains outside this proof.
The separate generic full-builder pilot 22744 measured a **236.876-second cold
pull**; do not represent these two lean-image hits as eliminating that path.

The later `64d2359` rollout preserves the historical `c80bd455...` cache acceptance
above. A naturally created cold ARM repository 23 worker at 23:03:43 UTC had
`CI_WARM=false` and no snapshot in its manifest and was still awaiting scheduling
at observation. This establishes requested cold-worker configuration, not executed
ARM startup, cache behavior or runtime performance.

The separate storage recovery task reports expansion/cleanup without a Forgejo
restart. This is an operator report, not a retained-space or sustained-write
measurement in this runbook. Verify backing-volume free space (including snapshot
usage) and service writes after cleanup; deleting image references alone does not
prove physical space reclamation.

Continuous dependency-aware packaging, last-green coverage selection, broader
scheduler qualification and reliable automatic integration remain unfinished.
Keep manual hardware/bring-up requests intact while delivering those changes.

## Observe before changing anything

Run these read-only commands on an operations host with the configured contexts:

```sh
fgj --hostname code.tilo.so api repos/tilo/tilo/branches/main
fgj --hostname code.tilo.so api repos/tilo/tilo/actions/runs/RUN_ID
fgj --hostname code.tilo.so api repos/tilo/tilo/actions/runs/RUN_ID/jobs
kubectl -n forgejo get cronjob tilo-ci-queue-coalescer
kubectl -n forgejo get jobs -l app=tilo-ci-queue-coalescer
kubectl -n sandbox-broker get deployment sandbox-broker
kubectl -n sandbox-broker rollout status deployment/sandbox-broker --timeout=180s
```

Replace `RUN_ID` with the request under investigation and match its full commit
SHA. Forgejo UI run numbers and API run IDs differ; do not substitute one for the
other or use the historical timing runs above as the current health check.

Inspect a specific coalescer Job's logs before taking action. Do not print the
token Secret. The infrastructure policy and runbook live in
`apps/forgejo/ci_queue_coalesce.py`, `apps/forgejo/ci-queue-coalescer.yaml` and
`apps/forgejo/ci-queue-coalescer.md`; broker configuration lives in
`apps/sandbox-broker/deployment.yaml`.

## Stop or roll back narrowly

These are operator procedures, not commands executed by this document. Argo
automatically syncs and self-heals both `forgejo` and `sandbox-broker`; a live-only
patch is temporary unless its matching GitOps source change is committed and
published through the authorized infrastructure path.

**Stop coalescing first if its behavior is suspect:**

```sh
kubectl -n forgejo patch cronjob tilo-ci-queue-coalescer --type=merge -p '{"spec":{"suspend":true}}'
kubectl -n forgejo get jobs -l app=tilo-ci-queue-coalescer
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

**Broker rollback:** retain the startup probe, graceful drain, exclusive quality
routing and live-runner protections wherever possible. Reverting `4c20a8e` would
remove the startup restart-loop mitigation. Older `3526...`/`af62...` images must
not be substituted blindly: they predate the current quality-lane/cleanup behavior
and may strand workflows requesting `runner-quality` or restore deletion hazards.

If the current image is the diagnosed cause, select and verify a compatible
replacement, then change only that image through the authorized GitOps path.
The immediate predecessor to `ffbd0a1` was `0ab15dde6aea3c11b0f3cb366565160dd15518f283e2813d0570dc28f22fc55b`;
it lacks the new cleanup guards and is not automatically a safe recovery target.
Inspect active jobs and the candidate image's label/admission contract before
any rollback. Verify rollout health and a real quality run afterward. Do not
delete broker/runner pods or interrupt active signing/hardware sessions to force
a rollout.

**Product CI rollback:** use a new task branch and a narrowly reviewed fix to the
specific selector, helper or workflow. Keep per-commit check/lint triggers and
preserve manual/tag/scheduled heavy paths. Do not `reset` main or revert the merge
checkpoints above: they contain other contributors' work. Do not restore broad
heavy per-push triggers merely to obtain one artifact; dispatch that particular
workflow and target instead. Re-enabling continuous packaging needs a separately
qualified capacity/coalescing policy, not a blanket reversal of relief.
