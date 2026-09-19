# Nonpublishing app packaging adapter

Consolidated with main through `f6acde7bb`; not dispatched or activated. This reuses
`app_packaging_coordinator.Store`, transactions, intent-before-POST protocol and
latest-pending state machine. There is no second queue implementation or service.
The private fixture adapter and its ten regression tests remain supported.

`tools/app_packaging_product.py` targets only repository ID 23 (`qos/qos`) and
`release-apps.yml`. It always dispatches a full source SHA and `publish=false`.
The receipt identity includes the exact builder digest, collection, architecture
list and trusted policy hash. API receipt validation checks the source, workflow,
repository ID/name, event, immutable ref, all inputs and persisted intent UUID.
`baseline_run_id` enables exact authenticated artifact lookup;
`baseline_receipt_sha256` binds the prior verification proof and
`coordinator_policy_sha256` binds the reviewed external coordinator policy.
`baseline_evidence` carries canonical proof JSON and must match that digest and
the exact source/run/builder/scope/policy before selecting an incremental base.
Uncertain POST responses leave the durable active intent blocked; `reconcile`
requires an explicit run ID and the same exact checks. No automatic retry,
cancellation, arbitrary workflow, tag mutation or publication command exists.

The CLI defaults to read-only, including no creation of a missing SQLite file.
`--apply-qualification` additionally requires a trusted local policy with
`enabled: true` and `qualification_only: true`. No enabled policy is shipped.
Workflow input handling independently requires repository variable
`TILO_APP_COORDINATOR_QUALIFICATION=true`; its absent/default value refuses these
requests. That variable has not been created. Ordinary PR/tag/manual behavior
with no coordinator inputs is preserved.

## Baseline semantics and verification seam

A successful workflow alone does **not** advance the product baseline. A build stays
active with status `awaiting-verification`, blocking later desired sources. A
trusted verifier callback must independently verify the authenticated final
artifacts, then return:

```json
{
  "schema": "tilo.coordinator-verification/v1",
  "backend": "canonical-final-artifact-v1",
  "run_id": 123,
  "desired": {
    "source_sha": "<40 lowercase hex>",
    "builder_image": "code.podesta.ai/qos/tilo-builder@sha256:<64 hex>",
    "collection": "default",
    "architectures": ["x86_64", "aarch64"],
    "policy_sha256": "<64 hex>"
  },
  "receipts": {"x86_64": "<canonical receipt SHA256>", "aarch64": "<canonical receipt SHA256>"}
}
```

This is a trusted-code callback return contract, **not** an importable JSON proof
or a flag in a candidate artifact. No CLI verifier provider exists yet. Therefore
the current adapter cannot autonomously accept a successful package build.

Zero-app plans have a separate cheap completion path. The existing trusted plan
job uploads `coordinator-plan-<plan attempt>` and logs its intent-bound content
SHA-256. The adapter rechecks the pinned workflow/planner helper bytes, exact run
and current successful plan job/attempt/task/handle, bounded artifact metadata and
bytes, log digest, source/builder/intent/baseline/scope inputs and empty plan lists.
It rereads run/jobs before accepting. A skipped build or uploaded zero-count JSON
without this provenance is insufficient. Plan verification remains outside the
SQLite write transaction; the same intent fingerprint is rechecked at commit.

This result records `handled=desired` and `outcome=verified-no-build`, clears the
active intent and permits newer pending work. It preserves the previous verified
package baseline and its evidence exactly. The next incremental build still
diffs from that package baseline. An empty first bootstrap leaves the baseline
null. Repeated ticks/restarts for the handled desired receipt remain idle. A later
verified package build clears the handled marker when installing its new baseline.

Integrate the existing independent artifact-verification work instead of
reimplementing signing/parsing: `final_app_receipt.verify(tree, pkg, runtime,
policy_path, policy_sha, source, architecture)` verifies a fresh archive/tree
using independently pinned policy and canonical binary hashes. Its receipt does
not bind run ID or builder digest; the adapter's artifact retrieval must bind
those to the already matched run before calling it. Its publication `bind(...)`
operation is not used in this nonpublishing mode.

This branch includes `assemble_verified_apps.py` and a guarded nonpublishing
final-assembly job. Changed apps must come from the current build; only unselected
apps may be reused from the exact verified baseline. The final collection is
canonically verified again and retained with its receipt and assembly provenance.
This does not install the controller's independent acceptance callback. That
callback must authenticate artifacts and bind current job attempts, source,
builder and scope before replaying verification for every architecture.
Candidate receipt or provenance claims alone are insufficient.

Read-only live API checks on 2026-09-19 confirmed the jobs endpoint's array
response and `attempt`, `task_id`, `handle`, `repo_id`, `run_id` fields; artifact
listing also returns an array with run identity, expiry and byte size. Fetching
artifact 34748's ZIP returned HTTP 200 without a redirect using an operator
credential. Built-in workflow-token access to historical artifacts and the
new plan artifact's run/attempt binding still need live qualification. The
operator probe does not establish either permission or end-to-end acceptance.

The first verified build has no baseline and selects the whole collection.
Subsequent compatible default-collection builds use the prior **verified packaging** source and the
hash of its retained verification proof, never `event.before` or the latest
push. Changing builder, trusted policy, collection or architecture scope forces
a full collection build. Non-ancestor baselines fail closed in the workflow.
Nondefault collections always select their full scope because the existing
`app_release.py --base` selector is catalog-wide, not a collection intersection.
The same Git source with a new immutable builder is a distinct desired receipt
and forces full packaging. The builder digest covers its baked compiler/native
environment; runtime recipes, lockfiles and asset manifests are pinned by source
and must remain covered by the production planner's input closure. Any future
externally supplied runtime bundle needs its own immutable identity added to the
desired receipt and compatibility test before reuse is allowed.
Publication is a separate future acceptance/promotion operation; qualification
baseline does not claim a downloadable release has been published.

## Activation prerequisites

1. Independently review and land the guarded workflow/helpers; integrate adjacent
   final-receipt and input-closure changes before creating trusted hashes. The
   policy must pin the complete executable workflow/helper closure, including
   local actions and artifact verifier code. The adapter's minimum required path
   set is a guardrail, not an automatic proof of complete closure. Use a policy
   stored outside candidate checkout, signed/reviewed immutable revisions and
   matching immutable builder trust. Do not accept hashes proposed by a candidate.
2. Complete and test the callback above on authenticated final nonpublishing
   artifacts with independent policies and canonical executable hashes. Bind
   run/job/current attempts, source, builder, scope and receipt bytes. Until then
   successful runs intentionally stop at `awaiting-verification`.
   The validator callback executes outside the SQLite write transaction. Before
   committing its result, the coordinator rechecks the complete active intent.
   New desired revisions can be recorded during replay; a concurrent change to
   the active intent rejects the stale result. Two-connection tests exercise both
   cases. The callback still requires fresh artifact provenance checks.
   The implemented zero-app path requires the trusted plan provenance above;
   it records handled work without advancing package/publication evidence.
   Every plan-job Python helper runs through pinned `tools/ci_python.py` using
   `python3 -I`. The launcher appends local tools after standard-library paths
   and uses a fresh private bytecode cache namespace, so candidate modules,
   `PYTHONPATH`/user site hooks and candidate `__pycache__` cannot replace those
   imports. The local import closure is plan → qualification → product →
   coordinator/resolver, and app selector → ci_select; all are required pins.
   The immutable CI image remains trusted for its interpreter and system site.
   This path is locally tested, not operationally qualified: a live fixture must
   establish that `GITHUB_RUN_ID` is the API's internal run ID and that
   `github.run_attempt` identifies the current plan job's `attempt`, including a
   partial rerun. It must also verify the installed CLI's raw ZIP/log responses.
   Raw capture reads at most the limit plus one byte and kills/reaps the CLI on
   overflow or timeout; this does not bound the CLI's internal HTTP buffering.
3. Provision a dedicated least-privilege product qualification identity and
   protected credentials; the existing integrator identity has pilot-only access.
   The default interactive fgj administrator profile is not a service identity.
   Prove dispatch/read scopes and retain `publish=false` publication guards.
4. Provision one durable local SQLite database shared by all coordinator workers
   on one host (not a network filesystem), backup/recovery and strict ownership.
   Keep publication and qualification states separate. Observe monotonically
   sequenced eligible committed sources; no scheduler is installed here.
5. Enable the repository variable and external policy only for an explicitly
   bounded qualification through normal scheduler admission; prove full-SHA input handling and
   artifact verification live. Reconcile uncertain dispatches before any repeat.
   Default production activation and routine publication remain disabled.

Read-only example with an operator-owned disabled policy:

```sh
python3 tools/app_packaging_product.py --state /durable/coordinator.sqlite \
  --policy /trusted/qualification-policy.json status
```

Validation:

```sh
python3 -m unittest discover -s tools/tests -p 'test_app_packaging_coordinator.py'
python3 -m unittest discover -s tools/tests -p 'test_app_packaging_product.py'
```
