# Local action capture: bounded receipts and edit cost

Local typed reports attest to admission and an application's account of an
already optimistic edit. They do not execute actions, authorize publication,
or prove execution. They must not use the execution journal's per-invocation
durability barrier.

## Storage and retry boundaries

The broker keeps a bounded acceleration cache (128 receipts, 4 MiB), not a
permanent local-report journal. A random capture identity and the complete
canonical report determine the invocation identity. Changing encoded input,
context, original heads, mutation hashes, or semantic address changes that
identity. Exact cached retries revalidate live custody without resolving a
released directory pin or spending another one-use grant. Evicted receipts can
be deterministically re-attested when their original declaration is still
available; an old declaration is never replaced with a current one.

The SDK stages private report/receipt history through the exact document
owner's existing `DurableStore` and `DocWriter` save tick. A bounded replacement
checkpoint (128 entries, 4 MiB) is separate from the shared CRDT: private input
and receipt openings must not be replicated as document values. The existing
storage worker coalesces checkpoint writes; staging an edit does not flush or
wait. There is no second database or growing invocation log. This intentionally
has the ordinary document autosave durability window, not synchronous admission
durability. Recovery exposes only durable checkpoints whose referenced changes
also survived in the owner's durable document history. Receipt-only writes and
failed document writes cannot make unsaved changes look durable.

`App::local_document_action_checkpoint` returns the original report identities
and optional receipts for a live route. Recovery retries pending reports only
when the original actor, authenticated principal and destination still match;
it never reattributes an old report to the newly selected identity. Owners
without a document writer cannot claim a durable checkpoint. Owner-linked slots
bound aggregate retained history to 1,024 entries and 32 MiB plus headers.
Old entries are bounded recent history, not an indefinite audit archive.

Directory pins share one immutable verified snapshot per revision. Both the
number of distinct revisions and their conservatively estimated retained memory
are bounded; estimates are computed during publication, not every gesture.
Capture budgeting uses a bounded counting serializer over borrowed fields, not
allocated synthetic reports. No complete application document clone was added.

## Reproducible checks

```sh
t cargo test -p tilo-app --lib framework::dispatch_tests -- --nocapture
t cargo test -p tilo-ui --lib local_input_capture -- --nocapture
t cargo test -p tilo-ui --lib local_action_persistence -- --nocapture
t cargo test -p tilo-app-state --lib private_history -- --nocapture
t cargo test -p tilo-action-broker --lib local_document_ -- --nocapture
```

The blocked-host tests withhold all responses, exercise the real SDK dispatch
and App local action doors, and assert edits are visible before any host reply.
Saturation refuses before the next mutation. These are ordering/progress proofs,
not a claim of zero latency. Other checks verify one executor encoding and no
additional capture encoding for standalone dispatch, sustained report admission
without journal writes/barriers, bounded retry caches and pin memory, checkpoint
recovery and retry, and batched private-history submission.

## Historical binary comparison

Identical headless Calculator and Notes Dispatcher benchmarks were built remotely
from historical commit `cd82cc6921` and candidate `167e9186f7`, then run locally in
eight interleaved ABBA blocks with isolated state. After warmup there were 8,192
samples per revision and case. These debug-build comparisons include intervening
main changes and exclude rendering, active capture and storage.

| Case | Before p50 / p95 / p99 (ms) | Candidate p50 / p95 / p99 (ms) |
| --- | --- | --- |
| Calculator immediate | 0.945 / 1.114 / 1.368 | 0.952 / 1.107 / 1.302 |
| Calculator group | 9.742 / 10.612 / 11.262 | 9.793 / 10.683 / 11.330 |
| Notes immediate | 0.461 / 0.582 / 0.813 | 0.459 / 0.539 / 0.673 |
| Notes group | 1.051 / 1.254 / 1.602 | 1.067 / 1.288 / 1.575 |

The immediate Notes allocation delta was zero. Grouped Notes had one additional
32-byte allocation, subsequently removed by evaluating `change_kind` once per
dispatch. Calculator allocation counts varied with generated history identities.
These measurements do not prove zero latency overhead or end-to-end typing parity.

A separate cached host-signer sample measured 41.840 / 44.151 / 54.821 microseconds
at p50 / p95 / p99, with 477.756 microseconds for cold loading. This is signer cost,
not connected broker round-trip latency. Blocked-host and journal-barrier tests
establish nonblocking local application and no per-report fsync; they are not a
substitute for complete input-to-presentation measurements.
