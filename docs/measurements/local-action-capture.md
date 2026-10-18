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
and optional receipts for a live route. It does not automatically resubmit work
on restart. Owners without a document writer cannot claim a durable checkpoint.
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

A remote development-mode run of the real SDK standalone dispatcher measured
512 edits per case: median 121.9 microseconds with the removed eager `Invoke`
construction emulated versus 112.0 microseconds without it; p95 150.5 versus
137.7 microseconds. Cases alternate order. This is a descriptive single-run
microbenchmark, not a comparison of historical binaries or a performance gate.
It excludes host signing, storage, rendering, and active capture; it cannot
establish end-to-end typing latency. Authenticated host-signing RPC tests run
separately and assert barrier counts, not timing.
