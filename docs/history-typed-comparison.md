# Typed historical comparison

Framework resources retain `diff` unchanged: it returns structural `items` with
`item`/`change`, version labels, and full `from_heads`/`to_heads`. The additive
`compare` Read action returns public typed content instead of document snapshots.

Arguments:

- `from`: a change id, checkpoint, or exact causal frontier (full hashes joined
  with `+`).
- `to`: the later frontier; omitted means the committed current frontier.
- `items`: 1–32 distinct published resource ids, at most 512 bytes each. Omit
  this argument to compare the whole resource through its content reader.

For example, `{"from":"before-edit","items":["notes/pages/page-id"]}`.
The resource's published tree supplies the item ids; restore selection paths
are a separate vocabulary, such as `pages/page-id`.

The result carries version labels, full causal head arrays, and `comparisons`:

```json
{
  "item": "notes/pages/page-id",
  "before": {"status": "absent"},
  "after": {"status": "present", "content": {"title": "A page"}}
}
```

Each frontier is resolved once and forked before either side is decoded with
`TypedRoot::read`. Content comes only from the resource's `read_content` adapter.
Absent means the decoded and demanded state has no published child of that id;
a model, demand-load, or content decoder failure fails the request rather than
claiming absence. `READABLE=false` never calls the content adapter. Unknown ids
are absent on both sides. Content deliberately omitted by an adapter stays
omitted. No private snapshot JSON is returned. The entire serialized result is
limited to 256 KiB; oversized results fail, never truncate.

## What the tests prove

`crates/sdk/tilo-app/tests/history_compare.rs` exercises the real framework action
door: public before/after data, omitted private keys, explicit additions and
removals, full concurrent frontiers, Viewer Read access, schema/adapter failure,
non-readable resources, oversized results and unchanged structural diff.
`crates/sdk/tilo-app/tests/history_review.rs` couples an exact historical read and
typed comparison to the native reviewed restore handler, preserving other
collaborators' work and checking attribution and undo.

The closed-record history fixture in
`crates/collab/tilo-action-broker/src/collab_broker_capnp/launchable.rs` tests the
production session action host and retained launchable catalogue before and
after broker restart. It checks exact record launch, read → review → restore
arguments, result custody and delivery. Its fake app supplies answers: this is
routing proof, not native historical execution or durable document mutation.
The native handler tests above supply the complementary execution proof.

Remote callers currently cannot use these retained local launch providers.
The existing broker remote-denial test covers that restriction, not remotely
granted historical read/review/restore transport. These tests make no claim
that a remote collaborator can launch and restore a closed record.
