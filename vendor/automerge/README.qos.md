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
- Lane additions are listed below as they land.

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
