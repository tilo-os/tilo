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
