# Hexane 1.0.0-alpha.5 provenance

Copied from the published crates.io archive `hexane-1.0.0-alpha.5.crate`
(SHA-256 `81df830f11babb0eee9a082e09135ef0a7d8791f3fe9b279bb9485bf7eaf23e0`).
Its packaged `.cargo_vcs_info.json` records upstream commit
`8d20da046282a43b5af3c3bdacd4467b48545087` at path `rust/hexane`
(package metadata, not an independently verified checkout). The packaged
`Cargo.lock` is omitted; the workspace lockfile governs.

Local modifications (keep this list exact):

- `src/lib.rs`: the exported `log!` macro calls `$crate::__log!` directly
  instead of importing it, so the vendored crate compiles warning-free under
  the workspace's `-D warnings` policy. No behaviour change.

- Load-path panic hardening (qOS lane L6): the streaming decoders also read
  raw, unvalidated bytes (automerge's change-graph and bundle columns), where
  malformed input used to panic or hang. Minimal, behaviour-preserving for
  valid input (same values, same cost on the hot paths):
  - `src/bool.rs`: `BoolDecoder::advance_run` marks the input exhausted when
    a run count cannot be read. It used to leave `byte_pos` in place, so
    `next`/`nth` retried the same truncated varint forever (CPU DoS).
  - `src/lib.rs`: new provided `RleValue::unpack_or_none` (`unpack` that
    returns `None` on malformed bytes; default `try_unpack().ok()`,
    `Option<T>` delegates, `String` bounds-checks without re-validating
    UTF-8, like `unpack`). SAFETY comments on both `from_utf8_unchecked`
    sites state the proof: raw-byte `String` decoding happens only in
    automerge's bundle iterators, whose columns
    `automerge/src/storage/bundle/validate.rs` validates (UTF-8 included)
    at parse; every other slab is validated by `Column::load` or written by
    `pack`.
  - `src/rle/decoder.rs`: `RleDecoder` ends the column (new `exhaust`)
    instead of panicking on a malformed repeat or literal value
    (`unpack_or_none`), a truncated null count, or a null run in a
    non-nullable column (`get_null` panics); literal run length uses
    `unsigned_abs` (`-i64::MIN` overflowed). `nth`/`next_run`/`resume` keep
    their validated-slab contract (automerge does not call them on raw bytes).
  - `src/delta/decoder.rs`: `DeltaDecoder` accumulates with wrapping
    arithmetic (the release-build behaviour), so an overflowing running sum
    cannot panic in debug builds.

The qOS allocation-bound helpers that patched hexane 0.2.1 (`allocation.rs`,
`SharedBytes`, slab/tree construction bounds) are not carried forward: the
types they described (`ColumnData`, `SlabWriter`, `SlabTree`, Arc-backed
slabs) do not exist in 1.0. automerge's `resource_limits::dims` bounds the
1.0 structures instead.
