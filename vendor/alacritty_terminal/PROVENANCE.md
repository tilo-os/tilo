# alacritty_terminal provenance

- Package: `alacritty_terminal` 0.26.0
- Registry checksum:
  `bda177466b9524d59f1b12f0dd30b68696788e9992a7e959021c4a0ed96fcf59`
- Repository: `https://github.com/alacritty/alacritty`
- Upstream HEAD inspected for an existing fix:
  `bbb2cbf9470a6707b1205fbddbdb25109b14e2fd`
- License: Apache-2.0 (`LICENSE-APACHE`, as published).

The published archive's `tests/` (reference recordings) and changelog are left
out, and the manifest drops the `ref` test target that ran them.

Local patch, `src/term/mod.rs`: erasing above the cursor (`CSI 1 J`) cleared
the lines above it only when there were at least two, so with the cursor on
the second line the first line survived. It now clears every line above the
cursor. Remove the copy once upstream has this fix.

The workspace root selects this copy with its reviewed Cargo patch.
