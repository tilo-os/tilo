# Nonpublishing app packaging adapter

Source checkpoint `d4e96bedb`; product packaging qualification and continuous
publication remain inactive. The separate service-account fixture dispatch
described below does not qualify the packaging workflow. This reuses
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
The platform-owned coordinator policy includes `final_receipt_policy_sha256`.
Rotating the final verifier policy therefore changes the desired policy identity
and forces full packaging. A same-policy baseline proof with a different final
policy hash is rejected before dispatch; it cannot silently authorize reuse.
`--apply-qualification` additionally requires a trusted local policy with
`enabled: true` and `qualification_only: true`. No enabled policy is shipped.
Workflow input handling independently requires repository variable
`TILO_APP_COORDINATOR_QUALIFICATION=true`; its absent/default value refuses these
requests. That variable has not been created. Ordinary PR/tag/manual behavior
with no coordinator inputs is preserved.

## Baseline semantics and verification seam

A successful workflow alone does **not** advance the product baseline. Without
an installed verifier, a build stays active with status `awaiting-verification`,
blocking later desired sources. A
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
  "receipts": {"x86_64": "<canonical receipt SHA256>", "aarch64": "<canonical receipt SHA256>"},
  "final_policy_sha256": "<platform-owned final verifier policy SHA256>"
}
```

This is a trusted-code callback return contract, **not** an importable JSON proof
or a flag in a candidate artifact. The low-level product CLI deliberately has no
verifier installed and cannot accept a successful package build by itself. The
[managed service entrypoint](PACKAGING-SERVICE.md) installs
`FinalArtifactAcceptance` with platform-owned policies and bounded authenticated
clients before invoking the same coordinator. That implementation is locally
tested; complete live collection acceptance remains unqualified.

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

The callback reuses the existing independent artifact-verification work:
`final_app_receipt.verify(tree, pkg, runtime,
policy_path, policy_sha, source, architecture)` verifies a fresh archive/tree
using independently pinned policy and canonical binary hashes. Its receipt does
not bind run ID or builder digest; the adapter's artifact retrieval must bind
those to the already matched run before calling it. Its publication `bind(...)`
operation is not used in this nonpublishing mode.

This branch includes `assemble_verified_apps.py` and a guarded nonpublishing
final-assembly job. Changed apps must come from the current build; only unselected
apps may be reused from the exact verified baseline. The final collection is
canonically verified again and retained with its receipt and assembly provenance.
The managed service's independent acceptance callback authenticates artifacts
and binds current job attempts, source, builder and scope before replaying
verification for every requested architecture. Candidate receipt or provenance
claims alone are insufficient. Assembly wiring and local callback tests do not
establish a successfully accepted live collection.

Read-only live API checks on 2026-09-19 confirmed the jobs endpoint's array
response and `attempt`, `task_id`, `handle`, `repo_id`, `run_id` fields; artifact
listing also returns an array with run identity, expiry and byte size. Fetching
artifact 34748's ZIP returned HTTP 200 without a redirect using an operator
credential. Subsequently, nonadmin service account `tilo-packaging-controller`
(user 82) dispatched the tiny `artifact-token-probe.yml` fixture at immutable
source `a987e76ba`: API run 23095/job 84767 succeeded, and its built-in job token
read historical artifact 38734 from run 22993. Infra records this bounded result
in `apps/forgejo/packaging-controller/qualification/service-dispatch.json`.
This proves that historical-token read for that fixture; the real packaging
plan artifact's current-attempt/partial-rerun binding and end-to-end collection
acceptance still need live qualification.

The dedicated account's read/Git permission boundaries and the exact x86 native
verifier bundle's signed-fixture acceptance/corruption rejection are also
qualified separately in infra's qualification record. These results do not
qualify ARM replay, an actual full app collection, incremental reuse, or
continuous packaging activation.

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
2. Qualify the implemented managed callback on actual final nonpublishing
   collection artifacts with independent policies and canonical executable
   hashes. Prove run/job/current-attempt, source, builder, scope and receipt
   bindings live. The low-level CLI without a verifier continues to stop at
   `awaiting-verification`; using the managed entrypoint does not waive acceptance.
   The validator callback executes outside the SQLite write transaction. Before
   committing its result, the coordinator rechecks the complete active intent.
   New desired revisions can be recorded during replay; a concurrent change to
   the active intent rejects the stale result. Two-connection tests exercise both
   cases. The callback still requires fresh artifact provenance checks.
   The implemented zero-app path requires the trusted plan provenance above;
   it records handled work without advancing package/publication evidence.
   Every plan-job Python helper runs through pinned `tools/ci/python.py` using
   `python3 -I`. The launcher appends local tools after standard-library paths
   and prevents absent platform-specific standard modules from falling back to
   candidate files (for example, the Windows `msvcrt` probe on Linux),
   and uses a fresh private bytecode cache namespace, so candidate modules,
   `PYTHONPATH`/user site hooks and candidate `__pycache__` cannot replace those
   imports. The local import closure is plan → qualification → product →
   coordinator/ci.resolve_builder, and app selector → ci.affected_packages;
   the shared ci package initializer is also a required pin. All helper source
   paths follow the tools/ci layout; no legacy duplicate files are installed.
   The immutable CI image remains trusted for its interpreter and system site.
   Cargo metadata also executes a toolchain: the policy pins
   `rust-toolchain.toml` and `.cargo/config.toml`. Source directory listings prove
   that legacy precedence overrides `rust-toolchain` and `.cargo/config` are
   absent; failed listings are errors, never interpreted as absence. Both the
   source verifier and workflow guard reject path toolchains and configuration
   includes, which would extend the executable trust closure. The workflow guard
   repeats the local regular-file/override checks before selecting apps.
   Metadata runs at the checkout root with `--locked --no-deps`, so member-package
   Cargo configs and build scripts are not executed. Activation still requires
   runner-owned checkout parents, PATH, CARGO_HOME, RUSTUP_HOME, environment and
   Rustup override database: these inherited configuration sources must not be
   writable by candidate jobs. This patch does not qualify that platform boundary.
   This path is locally tested, not operationally qualified: a live fixture must
   establish that `GITHUB_RUN_ID` is the API's internal run ID and that
   `github.run_attempt` identifies the current plan job's `attempt`, including a
   partial rerun. It must also verify the installed CLI's raw ZIP/log responses.
   Raw capture reads at most the limit plus one byte and kills/reaps the CLI on
   overflow or timeout; this does not bound the CLI's internal HTTP buffering.
3. Use the provisioned nonadmin packaging account and protected credentials,
   preserving the qualified read/Git boundaries and `publish=false` guards.
   Its successful fixture dispatch does not qualify the real packaging workflow.
   Actions write remains repository-wide; managed transport restrictions do not
   make the server grant workflow-specific. The separate integrator identity has
   pilot-only access, and the interactive fgj administrator profile is not a
   service identity.
4. Use one durable local SQLite database shared by all coordinator workers,
   with strict ownership and qualified backup/recovery. Infra's separate
   `controller-runtime.json` records actual local ZFS PVC persistence and locking
   across two Pods with a simulated dispatch crash. That bounded proof does not
   qualify backup restoration or a real ambiguous Forgejo dispatch. Keep
   publication and qualification states separate and observe monotonically
   sequenced eligible committed sources; the production controller remains disabled.
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
