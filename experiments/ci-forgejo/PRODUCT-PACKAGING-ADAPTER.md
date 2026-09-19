# Nonpublishing app packaging adapter

Prepared from main `5ade281b0`; not pushed, dispatched or activated. This reuses
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

A successful workflow alone does **not** advance the product baseline. It stays
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
the current adapter cannot autonomously finish a successful product run.

Integrate the existing independent artifact-verification work instead of
reimplementing signing/parsing: `final_app_receipt.verify(tree, pkg, runtime,
policy_path, policy_sha, source, architecture)` verifies a fresh archive/tree
using independently pinned policy and canonical binary hashes. Its receipt does
not bind run ID or builder digest; the adapter's artifact retrieval must bind
those to the already matched run before calling it. Its publication `bind(...)`
operation is not used in this nonpublishing mode.

The current production `publish=false` job emits selected build receipts, not
the independently verified final merged collection. Do not substitute those
self-produced JSON files for this callback. A nonpublishing final-assembly/replay
stage or separately authenticated artifact retrieval remains necessary. Verify
all architecture receipts, exact bytes and persisted input identities before
returning the proof. Receipt/source claims alone are insufficient.

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
   A zero-app plan also needs an authenticated no-change proof tied to the prior
   verified artifact set; a skipped build alone must not advance the baseline.
3. Provision a dedicated least-privilege product qualification identity and
   protected credentials; the existing integrator identity has pilot-only access.
   The default interactive fgj administrator profile is not a service identity.
   Prove dispatch/read scopes and retain `publish=false` publication guards.
4. Provision one durable local SQLite database shared by all coordinator workers
   on one host (not a network filesystem), backup/recovery and strict ownership.
   Keep publication and qualification states separate. Observe monotonically
   sequenced eligible committed sources; no scheduler is installed here.
5. Enable the repository variable and external policy only for an explicitly
   bounded, capacity-approved qualification; prove full-SHA input handling and
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
