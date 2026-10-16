# winit provenance

- Package: `winit` 0.30.13 (crates.io), carried as `tilo-winit` 0.30.13
- Registry checksum: `a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d`
- Repository: https://github.com/rust-windowing/winit
- Upstream revision: `e9809ef54b18499bb4f2cac945719ecc2a61061b` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: repository root
- License: Apache-2.0; `LICENSE` is the published archive's file, byte for
  byte.
- Vendored: 2026-07-11 (`b56119e8fe`)

Local changes: synthetic key events, Wayland drag sources, pointer gestures,
pen tablets, presentation-time frame announcements, focused keyboard layout
snapshots and the `tilo_text_input_v1` acknowledgement on Wayland, a macOS
quit request, web animation-frame and resize fixes. Example images are stored
as `.hex` text and decoded by `build.rs` (`fixture_build.rs`); the manifest
carries the `tilo-winit` publication identity and a cargo-machete note; the
archive's `Cargo.lock`, `Cargo.toml.orig` and `.cargo_vcs_info.json` are
omitted. `vendor/PATCHES.md` is the change log ("Published forks",
"`vendor/winit`: focused native keyboard configuration"); the standalone
history `codex/winit-dictation-ack-history` on `vendor-upstream/winit-0.30.13`
("Exportable histories") holds the changes as commits on the pristine archive,
`tools/verify-vendor-trees.py` checks this tree against it, and `TILO-FORK.md`
is the published crate's readme.
