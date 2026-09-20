# Durable one-active / latest-pending coordinator

`tools/app_packaging_coordinator.py` is a **nonpublishing private-fixture experiment**.
It has no production repository/workflow option and cannot cancel any run. It uses
the existing authenticated `fgj` CLI and Python's standard library only.

## State and behavior

One SQLite database stores the latest desired receipt, monotonic observation
sequence, last successful receipt, one active dispatch intent, and a retained row
for every attempted dispatch. `BEGIN IMMEDIATE` serializes claims; synchronous
commits persist the intent before sending a dispatch request. Network calls never
hold the database write lock. Multiple worker processes must share this database
on the same host's durable local filesystem; do not place SQLite on network storage.

- Observe A, dispatch A, then observe B and C: A remains active; C replaces B.
- Only an exact matching run's `success` advances the baseline. The next dispatch
  captures that successful receipt, **not the immediately preceding push**.
- Failure/cancellation/skipping retains the previous successful baseline, records
  the attempt, and blocks automatic retry of the same desired receipt. A later
  different desired receipt can proceed on a subsequent explicit tick.
- A committed intent without a known run ID blocks further dispatch. A timeout,
  broken response or process crash cannot cause a blind resend. Reconcile an
  explicitly identified run; its repository, workflow, immutable SHA, trigger,
  UUID input and baseline input must all match the persisted intent.
- If a returned ID was persisted but verification failed, later ticks retry reads
  of that exact ID only. They never POST again.
- A crash between intent commit and POST can leave an intent with no actual run.
  This prototype intentionally requires operator investigation instead of providing
  a dangerous "reset and retry" command. Do not delete its database to clear it.

No background loop is installed. Each `tick` performs at most one transition or
dispatch, with a 30-second API timeout. A production service still needs durable
storage operations, observability and bounded backoff. Independent/manual runs or
manual reruns are outside this coordinator's exclusivity; their policies need an
explicit integration decision before production activation. Tag and manual OS
release workflows are unchanged.

## Commands

Default mode is read-only and creates neither a database nor a remote run:

```sh
python tools/app_packaging_coordinator.py --state /durable/coordinator.sqlite tick
python tools/app_packaging_coordinator.py --state /durable/coordinator.sqlite status
```

Desired receipt JSON must contain an immutable full lowercase `source_sha`.
Additional immutable input identities can be retained in the receipt; the entire
canonical JSON gets a SHA256 identity. Supply monotonically increasing observation
numbers from one source observer; stale/conflicting observations are rejected.

```sh
python tools/app_packaging_coordinator.py --state /durable/coordinator.sqlite \
  --apply-private-fixture observe --sequence 1 --receipt desired.json
python tools/app_packaging_coordinator.py --state /durable/coordinator.sqlite \
  --apply-private-fixture tick
python tools/app_packaging_coordinator.py --state /durable/coordinator.sqlite \
  --apply-private-fixture reconcile --run-id VERIFIED_RUN_ID
python -m unittest discover -s tools/tests -p test_app_packaging_coordinator.py -v
```

The explicit apply flag permits only repository
`tilo/ci-workflow-pilot-20260918-9245-repeat` and `packaging-coordinator-probe.yml`.
Before dispatch, the API must confirm repository privacy and that the requested
SHA contains the exact retained fixture (normalizing CRLF to LF). The fixture only
prints its immutable source/intent/baseline; it has no checkout, build, secrets,
artifact upload or publication step. Dispatch uses the full SHA and
`return_run_info: true`, then verifies the server-owned run identity.

Ten regression tests cover active/latest replacement, successful-baseline
propagation, failure without retry, restart after uncertain acceptance, concurrent
worker claims, source movement during qualification, stale observations, unrelated
run rejection, crash before POST, dry-run CLI, and private/exact fixture gates.

## Qualification boundary

This is a coordinator, not a build system. Dependency selection, runtime/toolchain
content identities, app/platform coverage and reusable artifact lookup belong in
the desired-state planner. Resumable object publication, immutable publication
receipts and atomic final index promotion must be implemented and qualified in the
packaging workflow before successful workflow completion can mean a real packaged
baseline. Fixture success establishes orchestration only. Production continuous
packaging remains disabled; OS promotion remains manual, macOS retained disabled.
