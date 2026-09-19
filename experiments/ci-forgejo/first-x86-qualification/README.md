# First native x86-64 collection qualification: review candidate

These are local, reviewable configuration files. Nothing here installs policy,
changes Forgejo variables, unsuspends the controller, dispatches a build, or
publishes packages. `enabled: true` authorizes the proposed qualification only
when an operator later installs these exact bytes and explicitly runs the service.

## Exact scope and identities

Source inspected: `b34678dbfd154b0ad8979ae9e8527942768ec5ca` on merged `qos/qos/main`.
`coordinator.json` binds all **29** production `REQUIRED_PATHS` to their committed
SHA256 values. `controller-source-hashes.json` separately records the **12** trusted
service files that must be deployed outside candidate checkouts.

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
`770316f152a7fa88116f4fb6d02e70b954b669e5288fc4c540c786ce4708d0a7`.
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

## Reproduce and verify locally

Run from a native Git/Python environment for this worktree (Windows Git worktrees
may carry paths that WSL Git cannot resolve):

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
the retained native Pod is the separate ABI evidence. The existing 69 packaging
policy/service/coordinator/transport tests also pass.

## One controlled qualification, after review

1. Keep the CronJob suspended. Confirm its read-only source projection matches the
   twelve recorded hashes and its image, account82 credentials, ZFS state PVC and
   invocation lock match the qualified deployment. Read state with the existing
   service status path: no active intent, verified baseline or unresolved dispatch
   may be silently discarded or reset to obtain a fresh run.
2. Re-read current main and compare every `trusted_files` entry to it. The service
   intentionally observes current main at invocation time. App-only movement is
   allowed; changed trusted workflow/tool/config bytes require renewed review and
   policy generation. Record the actual observed source and selected app set.
3. Install reviewed `coordinator.json` and exact `final-policy.json` in the existing
   protected policy ConfigMap. Set `TILO_APP_FINAL_RECEIPT_POLICY` to those exact
   final-policy bytes and `TILO_APP_FINAL_RECEIPT_POLICY_SHA256` to their hash;
   read both back and verify exact binding. Confirm the dedicated verifier package
   read secret and existing app/runtime signing secrets are present. Set
   `TILO_APP_COORDINATOR_QUALIFICATION=true` only for this controlled qualification.
4. Create **one** manual Job from the existing suspended controller template, with
   the same source/policy/state/token mounts and ordinary scheduler admission.
   Its existing command is `python3 -I tools/ci/python.py tools/app_packaging_service.py`
   with `--state /state/coordinator.sqlite --policy /etc/packaging/coordinator.json`
   `--final-policy /etc/packaging/final-policy.json --token-file /var/run/packaging-token/token --apply`.
   One invocation persists a unique intent and dispatches the exact observed source
   with `publish=false`, `apps=""`, `collection=default`, `architectures=x86_64`,
   the pinned builder/policy identities and all baseline fields empty. Do not
   handcraft an intent, replace the state machine, or resend an uncertain POST.
5. Retain its exact run/job/attempt/task and intended source. Let Forgejo admit the
   existing jobs. If the dispatch becomes uncertain, reconcile that durable intent;
   do not dispatch another collection. Failures preserve state and never become a
   successful baseline. Do not lower the trust/ISA/verification gates to finish.
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
release matrix. Preserve continuous dependency-based Linux/qOS app packaging,
ARM apps, desktop/Linux-distribution packages, Linux/Windows setup, Windows apps
subject to toolkit qualification, and device/architecture OS outputs. OS release
remains manual; macOS definitions remain retained and disabled. Expansion needs
native evidence per new target and separate release acceptance before automation.
