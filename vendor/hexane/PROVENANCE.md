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

The qOS allocation-bound helpers that patched hexane 0.2.1 (`allocation.rs`,
`SharedBytes`, slab/tree construction bounds) are not carried forward: the
types they described (`ColumnData`, `SlabWriter`, `SlabTree`, Arc-backed
slabs) do not exist in 1.0. automerge's `resource_limits::dims` bounds the
1.0 structures instead.
