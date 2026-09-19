# Prepared controller durable-runtime qualification

Prepared against reviewed controller source `d4e96bedb`; not scheduled or run on
Kubernetes. The scope is **simulated-dispatch-crash/storage/lock qualification
only**, not live packaging acceptance. No Forgejo request, credential, policy,
package publication or application build participates.

The trusted helper is `tools/controller_runtime_probe.py`, invoked through the
reviewed `tools/ci/python.py` launcher. Mount the reviewed source read-only and
record its full commit and helper hashes in operator evidence. The probe verifies
and reports exact committed coordinator/service/launcher SHA256s before accessing
the state volume; this is not a substitute for pinning the complete mounted source.
Imports use the
actual `app_packaging_service.invocation_lock`, `app_packaging_service.invoke`,
`app_packaging_coordinator.Store`, `next_action` and `tick`; none is reimplemented.
Only dispatch is explicitly simulated by an object that exits its process with
code 73 immediately after a separate read-only connection observes the committed
dispatch intent. A socket audit hook forbids network operations.

## Two sequential Pods on the same PVC

Use the planned `qos-packaging-controller-state` PVC at `/state`, with no active
controller on it. The helper has no configurable state path: it owns only
`/state/runtime-qualification.sqlite` and `.lock`. It refuses to proceed if
`/state/coordinator.sqlite` exists, or fixture files are symlinks/hardlinks or have
another owner. SQLite's actual commit temporarily uses its own
`runtime-qualification.sqlite-journal`; the helper does not change journaling to
make the test pass, and requires all SQLite sidecars absent before cleanup.

Both Pods must use UID/GID 1000, readonly root/source, no service-account token,
no credentials, no host mounts, no added capabilities and no privilege escalation.
Mount a small writable `/tmp` for the isolated launcher's private bytecode namespace;
keep `/state` on the actual PVC. The launcher removes `/tmp` scratch normally.
Use the reviewed controller image and actual production storage class/access mode,
not a replacement `emptyDir`. Set a short deadline and keep both Pods sequential.
This document and script do not create Pods or mutate Kubernetes.

1. **Write Pod:**

   ```sh
   python3 -I /trusted/tools/ci/python.py \
     /trusted/tools/controller_runtime_probe.py write
   ```

   This holds the actual service lock while another process must immediately
   reject contention. It records one clearly labelled fixture desired state,
   then crashes a child during the simulated dispatcher after the real committed
   intent. The parent checks the durable intent and reports its `intent_id`.
   Preserve the exact JSON and Pod UID/node/image/exit evidence. Remove the
   completed write Pod while retaining the PVC; do not run the write phase twice.

2. **Fresh restart Pod:** use the same PVC and reviewed source, passing the exact
   intent UUID from the first Pod's successful output:

   ```sh
   python3 -I /trusted/tools/ci/python.py \
     /trusted/tools/controller_runtime_probe.py restart --expected-intent INTENT_UUID
   ```

   It must read `reconcile-required`. Calling real `tick` with `NeverAPI` must
   access no API attribute at all. Real `service.invoke(apply=False)` must observe
   the same persisted state without any policy/token file. All SQLite connections
   close, the database hash remains unchanged, and only the two fixture files are
   removed after releasing the lock. The real coordinator database must still be
   absent. Preserve the restart JSON and actual fresh Pod/PVC identity evidence.

Any mismatch, timeout, inaccessible mount or unexpected sidecar is failure, not a
skip. Failed probes retain their fixture state for diagnosis; never delete the PVC
or unrelated files to recover. No test automatically resends the simulated uncertain
dispatch. A passing probe establishes only this storage/lock/process-loss behavior;
API authority, real dispatch, acceptance, native verifier semantics and automatic
packaging still require their separate qualifications.

Local Linux preparation tests use temporary directories, real SQLite/flock and real
child processes. They exercise both phases and refusal of real-state/symlink paths;
they do not establish PVC persistence or Kubernetes deployment success.
