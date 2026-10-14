# automerge-test 0.12.0

Source: the published `automerge-test-0.12.0.crate`, upstream commit
`37545d0d72b50f52084a2eca2cadf2eb1a5a512a` (`rust/automerge-test`), the same
release as the `automerge` 0.12.0 that `vendor/automerge` forks.
Archive SHA-256:
`e431c5d29334a879c53be35061f410c86ab1a8998fa8ab723b385ac39803ecc3`.
Licence: MIT. The archive carries no licence text; `LICENSE` is the
repository root's file at the `rust/automerge-0.12.0` tag, byte for byte, as
for `vendor/automerge`.
Vendored: 2026-10-14.

It is the helper crate (`assert_doc!`, `map!`, `list!`, `realize`) the
Automerge test suite asserts through. Upstream's `automerge` takes it as a
`path = "../automerge-test"` dev-dependency, which crates.io publication
drops, so the fork's `tests/test.rs` could not build without it.

The one Tilo change, to `Cargo.toml` only: its `automerge` dependency is the
fork at `../automerge` (package `tilo-automerge-fork`), as upstream's own
`path = "../automerge"`, so the helpers see the fork's types. `src/lib.rs`
is verbatim. Nothing in the workspace depends on it; only the fork's tests
do (`tools/ci/test-vendored-crates.sh`).

The archive's standalone `Cargo.lock` is omitted.
