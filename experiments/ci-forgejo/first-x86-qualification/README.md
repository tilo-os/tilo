# First native x86-64 collection qualification: historical record

This directory preserves the reviewed first-qualification inputs and procedure.
Its generator, tests, 29-file workflow policy and 12-file controller inventory
belong to Tilo commit `774a6886b01fbc219ff55cc1a80713e88c492d09`; reproduce them
from a separate checkout of that exact commit, not from current main. Do not use
the installation sequence below to overwrite a live policy or rerun a dispatch.
It records the original empty-state procedure, not the current platform state.

The shared-contract extraction requires a separately reviewed 13-file controller
projection and new workflow trust closure. [PACKAGING-SERVICE.md](../PACKAGING-SERVICE.md)
describes that boundary. These original policy JSON files and the original
generator are retained unchanged; they are not a generator for the new contract.

These are local, reviewable configuration files. Nothing here installs policy,
changes Forgejo variables, unsuspends the controller, dispatches a build, or
publishes packages. `enabled: true` authorizes the proposed qualification only
when an operator later installs these exact bytes and explicitly runs the service.

## Exact scope and identities

Source inspected: `b4cba2210b4c03be13ec518822756849feadb685` on merged `qos/qos/main`.
`coordinator.json` binds all **29** production `REQUIRED_PATHS` to their committed
SHA256 values. `controller-source-hashes.json` separately records the **12** trusted
service files that must be deployed outside candidate checkouts.

This refresh follows the green merges of PR76 (managed recovery) and PR77
(builder metadata retries and PR planning without a builder lookup).
`refresh-review.json` records the complete before/after digest delta: only
`.forgejo/workflows/release-apps.yml` and `tools/ci/resolve_builder.py` changed in
the 29 trusted inputs; only `tools/ci/resolve_builder.py` and
`tools/app_packaging_service.py` changed in the 12-file controller closure.
No other trusted input changed. Final signing/verifier policy bytes and all
public identities, compatibility evidence and product scope are unchanged.

- Qualification only, service identity **82**, default collection, **x86_64** only.
- **53 apps**, enumerated in `review.json`, with no prior baseline or evidence.
  The default collection excludes the OS-integrated browser, as defined by the
  current product manifest; this qualification does not change that definition.
- App publisher: `1cb2f164d3517879cfce6e24106a1d4bc3686096b30b674e7859e6b1c896219a`.
- Current runtime publisher: `50d099efd7805a4783847f11ba2dd4ecf338e2f6b254a3f8eb5822eec21f3dd1`,
  freshly derived through the production signer in [run 9734](https://code.sandbox.example/qos/qos/actions/runs/9734)
  (API23237, job85267), retained in `runtime-publisher.json`.
- Immutable builder: `code.sandbox.example/qos/tilo-builder@sha256:027e43c051f1f34ce9ac1539172af0e711958243115e2efd08901ffa58729c5a`.
- Canonical bundle SHA256: `10ad658a452b21545cf2521fa875536da5d951ee8dd509533251a0a3a0517bde`,
  **6,113,401 bytes**, exact descriptor and binary/adapter identities in `final-policy.json`.
- Allowed x86 package ISA: **v3**. ARM is deliberately absent from this first native
  qualification policy; it requires its own approved native bundle before expansion.

The coordinator's semantic policy identity is
`25f1bedff8a3d23f8fd5a98843a0adb738271aa26819326fdf4c9427d815c720`.
It hashes canonical JSON, so it differs from the pretty-printed file SHA256 in
`review.json`. The final policy binds **exact file bytes**, including the trailing
newline, with SHA256
`5d0aed6416d1561ea45c9401f6a4c006f8745eb6ba920c0bc9bdf462253a6856`.
Do not reformat that file during variable or ConfigMap installation.

## Why native compatibility is approved

The retained `native-runtime.json` and `native-semantic.json` come from infra
commit `bfa31ddbe62ba2602f7539f10f778b97166c0b55`. The exact builder and canonical
bundle executed on a native x86 node as UID1000, accepted newly signed package
and runtime fixtures, and rejected corrupt signed roots and runtime payloads.
`native-source-hashes.json` binds the smoke helper and inputs; the preparer task
independently rehashed retained smoke.py, manifest.toml and descriptor.json and
matched those records. The public identity probe separately establishes the
current protected runtime key rather than trusting the fixture's temporary key.

That is sufficient for this builder/bundle's ABI and verifier semantics approval.
It does **not** qualify the full collection, any app's interactive behavior, ARM,
or continuous packaging. In particular, historical artifact34748 is never used
as a baseline. Full-collection success and independent replay remain the next gate.

## Reproduce the historical snapshot locally

Run the following only in the separate checkout of
`774a6886b01fbc219ff55cc1a80713e88c492d09`, using native Git/Python for that
checkout (Windows worktree paths may not resolve in WSL Git):

```sh
python experiments/ci-forgejo/first-x86-qualification/prepare.py
python -m unittest discover -s experiments/ci-forgejo/first-x86-qualification -p test_policy.py -v
```

Run the existing service suite on Linux (its process-lock checks require `fcntl`):
`python3 -m unittest discover -s tools/tests -p 'test_app_packaging*.py' -v`.

The preparer reads the recorded immutable source with `git show`, checks the
production contract and app manifests still match it, validates retained public
proof identities, and writes only local JSON. Six concrete-policy tests exercise
the real service/product/dispatch guards, including changed trusted files, missing
closure members, modified policy bytes, wrong architecture, disabled controls and
publication attempts. Architecture patching in these offline tests tests schema;
the retained native Pod is the separate ABI evidence. The existing 80 packaging
policy/service/coordinator/transport tests also pass. Resolver and app-release
workflow contract tests cover the newly landed metadata and PR-plan changes.

## Original empty-state qualification procedure

1. Start from the verified live state: the controller CronJob and policy ConfigMap
   are **absent**; only the qualified state PVC and credential Secrets are retained.
   Prepare installation from the reviewed staged manifests under infra
   `apps/forgejo/packaging-controller/`, preserving the existing PVC and Secrets.
   The earlier staged source ConfigMap at
   `f622cb6ef0478f1e0802c957d83a8be96771eed0` is now stale: its managed service
   and builder resolver differ. Regenerate the immutable source bundle from the
   clean, reviewed refreshed policy checkout using the existing infra source
   generator. Compare every projected file hash to `controller-source-hashes.json`
   for candidate main `b4cba2210`; different commit labels are acceptable only
   when all twelve required source files match. Reject the old source bundle.
2. Re-read current main and compare every `trusted_files` entry to it. The service
   intentionally observes current main at invocation time. App-only movement is
   allowed; changed trusted workflow/tool/config bytes require renewed review and
   policy generation. Record the actual observed source and selected app set.
3. Install the reviewed staged immutable source ConfigMap, create a protected
   **immutable** `tilo-packaging-controller-policy` ConfigMap containing reviewed
   `coordinator.json` and exact `final-policy.json`, and install the staged CronJob
   manifest with `spec.suspend: true`. Use its existing manifest template and source
   reference patch; do not substitute another coordinator. Read back the installed
   source and policy bytes, image and mount references. Verify the CronJob remains
   suspended and has created **no automatic Jobs** before proceeding. Confirm
   account82 credentials, the retained ZFS PVC and shared invocation lock are used.
   Set `TILO_APP_FINAL_RECEIPT_POLICY` to the exact final-policy bytes and
   `TILO_APP_FINAL_RECEIPT_POLICY_SHA256` to their hash; read both back and verify
   exact binding. Confirm the dedicated verifier package-read secret and existing
   app/runtime signing secrets are present. Set
   `TILO_APP_COORDINATOR_QUALIFICATION=true` only for this controlled qualification.
4. Before an applied invocation, inspect retained state through the service's
   documented read-only status path; never discard or reset an active intent,
   verified baseline or unresolved dispatch. Create **one** manual Job from the
   **newly installed, verified suspended** CronJob, using its existing template,
   identical source/policy/state/token mounts and ordinary scheduler admission.
   Its existing command is `python3 -I tools/ci/python.py tools/app_packaging_service.py`
   with `--state /state/coordinator.sqlite --policy /etc/packaging/coordinator.json`
   `--final-policy /etc/packaging/final-policy.json --token-file /var/run/packaging-token/token --apply`.
   One invocation persists a unique intent and dispatches the exact observed source
   with `publish=false`, `apps=""`, `collection=default`, `architectures=x86_64`,
   the pinned builder/policy identities and all baseline fields empty. Do not
   handcraft an intent, replace the state machine, or resend an uncertain POST.
5. Retain its exact run/job/attempt/task and intended source. Let Forgejo admit the
   existing jobs. If the dispatch becomes uncertain, reconcile that durable intent;
   do not dispatch another collection. The newly reviewed managed recovery path
   uses the same service and mounts with `--apply --reconcile-run-id EXACT_RUN_ID`;
   it is GET-only and requires exact original intent/source/actor/policy metadata.
   Missing proof blocks recovery, and no recovery command belongs in a retry loop.
   Failures preserve state and never become a successful baseline. Do not lower the trust/ISA/verification gates to finish.
6. After the attached run finishes, run **one** more manual controller invocation
   to accept it through `FinalArtifactAcceptance`: exact dispatch, current qualifier
   log markers, artifact hashes and independent canonical replay. `core.tick`'s
   poll path returns after acceptance and cannot dispatch another build in that
   same tick. Record resulting baseline/evidence and stop manual ticks; keep the
   CronJob suspended and disable the qualification variable again after completion.
   A green workflow alone is not complete acceptance.

## Expected resources and next scope

The first run has a cheap `runner-quality` plan (15-minute timeout), **one** native
`runner` x86 app build (240-minute cap), then **one** native `runner` final verifier
(60-minute cap). These are caps, not measured completion estimates. The publisher
job is skipped. It builds the shared runtime and selected 53 apps from source;
there is no trusted baseline yet. It reuses existing Cargo downloads and the
`tilo/apps/v2/x86_64` compiler cache, with no OS image or kernel build. The shared
scheduler owns capacity; agents do not exchange build-slot messages.

Controller invocations request 1CPU/1Gi, cap at 2CPU/4Gi, and have a 900-second
Job deadline, 1Gi durable ZFS state, 12Gi disk scratch and 14Gi ephemeral-storage
limit. Replay bounds each archive/extraction operation to 2Gi and frees per-arch
scratch before another architecture. Those limits do not guarantee that every
future collection fits; retain observed peak usage during this first run and
fail closed if they are insufficient. No app payloads are executed by this
canonical artifact acceptance, so interactive/install testing remains separate.

This single x86 gate is an initial qualification, not a reduction of the requested
release matrix. Preserve continuous dependency-based Linux/Tilo app packaging,
ARM apps, desktop/Linux-distribution packages, Linux/Windows setup, Windows apps
subject to toolkit qualification, and device/architecture OS outputs. OS release
remains manual; macOS definitions remain retained and disabled. Expansion needs
native evidence per new target and separate release acceptance before automation.
