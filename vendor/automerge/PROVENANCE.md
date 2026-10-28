# automerge provenance

- Package: `automerge` 0.12.0 (crates.io), carried as `tilo-automerge-fork` 0.12.0
- Registry checksum: `2fb849130973a7808a167887f56b1e8bdc36b9b541542602dcb337f34aecf47b`
- Repository: https://github.com/automerge/automerge
- Upstream revision: the archive's `.cargo_vcs_info.json` names
  `37545d0d72b50f52084a2eca2cadf2eb1a5a512a`, which the public repository does
  not serve; the release tag `rust/automerge-0.12.0` is
  `1755d0dbd9a467cb849cf08a5c86e207835f5533`.
- Upstream package path: `rust/automerge`
- License: MIT. The published archive carries no licence text; `LICENSE` is the
  repository root's file at the `rust/automerge-0.12.0` tag, byte for byte.
- Vendored: 2026-09-25 (`6dad784c33`)

Local changes, each described with its reason and pinning test in
`TILO-PATCH.md` (and `parse_changes` in `vendor/PATCHES.md`, "`vendor/automerge`:
parse changes without applying them"):

- a mark event extends the previous one only on the same object, so a diff
  over several text objects keeps each object's mark patch (undo of a mark
  across paragraphs);
- `committed_*` readers of the committed document through `&self`;
- `ChangeGraph::remove_ancestors` returns early, so an idle sync message does
  not walk the whole history;
- `Automerge::change_hashes` lists the change log's hashes without rebuilding
  changes;
- map, list and text applies seek over ops a batch cannot affect
  (`TILO(map-apply-skip)`, `TILO(seq-apply-skip)`), so a remote change costs
  the same at any history length;
- `LoadOptions::known_change_hashes` loads with the hashes the saver listed,
  and empty-map lookups are skipped while loading;
- a peer's change is checked before any of it is applied
  (`TILO(peer-change-check)`), so a crafted change is refused instead of
  panicking or corrupting the saved document;
- `Automerge::parse_changes` parses and checksums changes without applying
  them;
- `automerge::work()` counts, per thread, the ops an apply walks, the op id
  slabs searched and the changes an ancestry walk visits
  (`TILO(work-count)`), for tests that pin cost without timing it;
- three `#[allow]`s for warnings the current compiler raises in upstream code.

The manifest gives the fork its own package name. Binary test fixtures and
fuzz crashers are stored as `.hex` text and decoded by
`tests/support/fixture_bytes.rs`, so the tree holds only text; the archive's
`Cargo.lock`, `.gitignore` and `.cargo_vcs_info.json` are omitted.
