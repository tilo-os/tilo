# qOS automerge 0.12.0 patch

Copied from the published crates.io archive `automerge-0.12.0.crate`
(SHA-256 `2fb849130973a7808a167887f56b1e8bdc36b9b541542602dcb337f34aecf47b`).
Its packaged `.cargo_vcs_info.json` records upstream commit
`37545d0d72b50f52084a2eca2cadf2eb1a5a512a` at path `rust/automerge`
(package metadata, not an independently verified checkout). The packaged
`Cargo.lock` is omitted.

This replaces the qOS automerge 0.8.0 patch. The on-disk and wire formats are
unchanged; 0.12 reads 0.8 data.

## Bounded, checked admission (`src/resource_limits*`)

Untrusted peers send automerge storage into hosts, relays and browsers.
`automerge::resource_limits` admits it under explicit ceilings and caller
credit: storage is inspected and normalized against `NativeLimits` before any
input-proportional allocation (counted fixed-scratch inflate against
compression bombs), semantically pre-validated so application is atomic, and
every load/apply/clone/save/history/read allocation is reserved on a
`NativeAllowance` before it happens. Design and threat model:
Cloud `docs/superpowers/specs/2026-09-25-automerge-012-bounded-admission.md`.

Files outside `src/resource_limits*` changed by this patch (keep exact):

- `Cargo.toml`: adds `miniz_oxide = "=0.8.9"` (counted inflate).
- `src/lib.rs`: `pub mod resource_limits`; the `log!` macro calls
  `$crate::__log!` directly (lint hygiene).
- `src/autocommit.rs`: crate-private helpers `native_mutation_is_closed`,
  `native_read_is_isolated`, `native_has_transaction`, `native_from_doc`.
- `src/patches/patch_log.rs`: crate-private `event_count`.
- `src/columnar/column_range/{generic,value}.rs`: lint hygiene (no dead
  increments, no unused forwarding method), no behaviour change.
- `src/automerge.rs`: `load_with` / `load_with_options` record errors at
  `debug` rather than `error` (`err(level = "debug")`). Callers validate
  single change chunks by loading them and expect `MissingDeps`; at `error`
  this emitted about 68,000 log lines per two-minute native host session.
  The error is still returned unchanged.

### Semantic parity with the qOS 0.8 patch

- Populated Tables: `src/op_set2/types.rs` maps `Action::MakeTable` to
  `ObjType::Table`; `src/transaction/inner.rs` admits map-key scalar and
  object writes to Table parents; `src/op_set2/change/batch.rs` imports
  Table-parent operations through the map walker; `src/automerge.rs` and
  `src/iter/doc.rs` treat Table as a map in whole-document iteration and the
  string-to-text conversion. Without these, 0.12 loaded 0.8 populated Tables
  as empty and panicked applying their changes.
- Wrapping (non-panicking) counter arithmetic: `src/value.rs`
  (`Counter::increment`), `src/op_set2/op.rs`, `src/op_set2/op_set.rs`,
  `src/op_set2/op_set/visible.rs`, `src/iter/{list_range,map_range}.rs`,
  `src/transaction/inner.rs`, and the two patch-log counter deltas in
  `src/op_set2/change/batch.rs` (`wrapping_sub`).
- `src/iter/list_range.rs`: `ListRangeItem::is_mark()`.
- `src/value.rs`: `ScalarValue` Debug goes through a crate-private
  `ScalarDebug`, byte-identical to the derived output, so
  `NativeScalar::write_legacy_debug` streams it without allocating.
- `src/exid.rs`: crate-private `BorrowedExId`; `ExId::to_bytes` uses it
  (bytes unchanged).

### Checked apply (`src/op_set2/change/batch.rs`, `batch/native_validation.rs`)

- `native_validation.rs` (new): the non-mutating pre-validator used by
  `apply_checked_bounded`, `apply_incoming_checked` and `reconstruct`.
- `batch.rs`: `mod native_validation`; internal consistency panics
  (missing object index, unconsumed insertion anchor, leftover conflict
  marker, unpositioned op, `import_ops` actor lookup, op-order debug
  assertion) return `AutomergeError::Fail`; a pre-mutation `admissible`
  check refuses non-contiguous per-actor sequences, unknown actors not
  introduced at seq 1, and authors after seq 1, restoring the queue; the
  text patch path no longer panics ("exposed value must record the value it
  replaces") on two concurrent puts to one text element followed by a
  delete of only the lower one (valid history).

### Snapshot admission memory

- `src/patches/patch_log.rs`: a crate-private discarding `PatchLog` (a
  `discard` flag; the recording methods return early). 0.12 batch apply
  writes an event per applied op into any log, active or not; checked apply
  and checked load pass a discarding log unless the document has an armed
  diff cursor (an inactive log's events are never read).
- `src/autocommit.rs`: `native_apply_batch` (the above), `native_is_blank`
  and `native_replace_blank`; `src/automerge.rs`: `native_adopt_identity`.
  `apply_incoming_checked` admits a snapshot into a blank document by the
  checked strict load instead of re-applying every change through batch
  apply (upstream `load_incremental` takes the same shortcut).
- Change rebuild without buffer doubling (lane L1; memory only, identical
  bytes). Load verification rebuilds every change of a document chunk, and
  a large value used to be held up to 6 times: the raw value column, the
  collector's column `data` doubled by growth, and the chunk `data`
  reserved for the columns only, then regrown by the header while the old
  copy lived. Now:
  - `src/op_set2/change.rs` `write_change_ops` and
    `src/op_set2/change/collector.rs` `ProgressiveEncoder::save_to` encode
    the columns after the value column into a small buffer first, reserve
    `data` exactly for the values plus that tail, then append both
    (ranges shifted);
  - `src/op_set2/change.rs` `assemble_change_chunk` writes
    `header || prefix || columns || extra` into one exactly reserved buffer
    (the header length follows from the data length; the header is filled
    in after hashing the data in place), used by `build_change_inner`,
    `ChangeBuilder::finish` (collector) and `storage::change::ChangeBuilder::build`
    (`src/storage/change.rs`).
  A verified load of a single large value now peaks at 3x the value (the
  raw value column, the column data and the chunk); a 10 MiB value
  snapshot admits within one 64 MiB turn.

### Allocation-free dimension hooks (lane L1, read-only)

`src/automerge.rs`, `src/autocommit.rs` (a trailing `impl` block),
`src/change.rs` (`native_pred_count`),
`src/storage/change/change_op_columns.rs`, `src/change_graph.rs` and
`src/change_graph/native_dims.rs`, `src/change_queue.rs` (`has_hash`),
`src/marks.rs`, `src/op_set2/op_set/{native_dims,mark_index}.rs`,
`src/op_set2/types.rs`, `src/transaction/inner.rs` (`mod native_heap`) and
`src/transaction/inner/native_heap.rs`: crate-private accessors that read
capacities and lengths for `resource_limits::dims`. No behaviour change.

### Load-path panic hardening (lane L6)

Release builds use `panic = "abort"`, so a panic on hostile storage is a
remote crash; `catch_unwind` is no defense. These changes turn every panic
(and one hang) that checksum-preserving mutations of valid documents,
change chunks and bundles reached on plain `AutoCommit::load`,
`load_incremental` and the checked `normalize_input -> reconstruct` path
into errors. Valid input loads byte-for-byte as before. Regression inputs
and the mutation sweep: `crates/doc/tilo-automerge/tests/am012_hardening.rs`.

- `src/storage/bundle/validate.rs` (new) and `src/storage/bundle/storage.rs`:
  at bundle parse, every change and op column's run framing is checked with
  the value type its streaming decoder uses (truncated varints, literal runs
  longer than the data, unknown actions, invalid UTF-8, null runs in
  non-nullable columns, delta sums overflowing `i64`), before any decoder
  exists. Non-canonical runs stay accepted.
- `src/storage/bundle.rs`: `mod validate`.
- `src/storage/bundle/builder.rs`: `validate_change_columns` /
  `validate_op_columns` (the decoder type table); forged `extra_count` and
  value lengths use `split_at_checked` (were "mid > len" panics); `deps` and
  `pred` preallocation capped at 1024 (a forged count was a capacity-overflow
  panic or allocation abort); op, object, key and pred counters above
  `u32::MAX` are refused (`OpId::new` unwraps a `u32` conversion).
- `src/op_set2/change/collector.rs`: `ChangeCollector` refuses (new
  `invalid` flag, reported by `finish`/`unbundle`/`collect`) ops whose
  actor indexes are past the actor table (was an index panic in
  `ActorMapper::process_actor`), increments with a non-numeric value (a
  `Verified` change's `iter_ops` unwrapped that check later), and change
  metadata with `start_op == 0`, `max_op > u32::MAX` or `max_op + 1 <
  start_op`; `num_ops`/`max_op` use saturating arithmetic. (The duplicate op
  id, dependency index and `unbundle` guards are L3's, same file.)
- `src/storage/load/change_collector.rs`: `Error::InvalidOp`.
- `src/op_set2/columns.rs`: `Columns::load` refuses documents whose value
  lengths do not add up to the value column (`RawColumnIter::take` panicked
  "past end" in op reads) or whose op/succ/obj/key actor indexes are past
  the actor table. O(runs), no allocation.
- `src/change_graph.rs` (inside `ChangeGraphCols::load` only): one dep count
  per change is required before the deps loop, a dep index past the change
  list is `InvalidColumnLength` (both were `max_ops[..]` index panics), and
  the extra-bytes lengths must add up to the extra-bytes column (change
  metadata iteration sliced past it).
- Batch apply seq contiguity (lane L2, `src/op_set2/change/batch.rs`): a
  ready change from an unknown actor with `seq != 1` used to panic in
  `Automerge::import_ops` (`lookup_actor(..).unwrap()`) and a seq gap hit
  `ChangeGraph::add_changes` `assert_eq!`; L2's guard refuses both.
  Regression inputs: `am012_hardening.rs` `BATCH_SEQ_GAPS`.
