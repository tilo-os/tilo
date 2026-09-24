# qOS Automerge 0.8.0 patch

This directory is an exact copy of the published `automerge` 0.8.0 crate, except for the source
changes listed below. The cached crate archive used for the copy was 337476 bytes with
SHA-256 `2aab56635599ee2e9df28d9ce180c155b8dbcdd96e4c3f62895fc6a44137d328`.
Its packaged `.cargo_vcs_info.json` records upstream commit
`8246d0f8218cd49dc1af56ba6eefd88ed215665c` at path `rust/automerge`; that is package metadata,
not an independently verified checkout.

The local patch preserves Automerge's existing MakeTable wire action and package version while
making populated Tables usable through the raw public read/write/import paths required by qOS:

- `src/op_set2/types.rs` registers MakeTable object metadata.
- `src/transaction/inner.rs` admits map-key scalar/object writes to Table parents.
- `src/op_set2/change/batch.rs` imports Table-parent operations through the existing map walker.

Strict CI lint hygiene preserves the same Automerge behavior while allowing the vendored crate to
compile under the workspace's `-D warnings` policy:

- `src/lib.rs` invokes the exported logging macro through `$crate`, avoiding a macro-local unused
  import.
- `src/columnar/column_range/value.rs` omits index increments after their final read.
- `src/columnar/column_range/generic.rs` implements the private iterator dispatch directly,
  removing an otherwise unused forwarding method.

Host logging hygiene:

- `src/automerge.rs` records `load_with` / `load_with_options` errors at `debug` rather than
  `error` (`#[tracing::instrument(err(level = "debug"))]`). Callers validate single change chunks
  by loading them and expect `MissingDeps`; the error is still returned unchanged. At `error` this
  emitted about 68,000 log lines per two-minute native host session.

No other packaged file is modified. Unpatched Automerge 0.8.0 peers are known not to support this
populated-Table path; coordinated ingress/version admission remains an integration requirement.
