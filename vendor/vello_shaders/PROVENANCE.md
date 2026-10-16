# vello_shaders provenance

- Package: `vello_shaders` 0.9.0 (crates.io), carried as `tilo-vello-shaders` 0.9.0
- Registry checksum: `9dd38937516fa4b47423d9255bb5e4a65e839ec9d57c38c4af6189ce56bf46b7`
- Repository: https://github.com/linebender/vello
- Upstream revision: `875f324f21da93019cae9e8e61d4abfd69893206` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `vello_shaders`
- License: Apache-2.0 OR MIT; `LICENSE-APACHE` and `LICENSE-MIT` are the
  published archive's files, byte for byte.
- Vendored: 2026-09-05 (`a14e30d53d`)

Local changes: the fine stage terminates on any PTCL stream and clamps fill
segment counts (a malformed stream hung the GPU), area coverage is evaluated
in a translation-stable form, no workgroup array is read at index -1, coarse
checks its `segments` allocation, a canonical segment order for repeatable
exact-pixel renders (`shader/canonical_segment_order.wgsl`, new), a CPU draw
prefix (`src/cpu/draw_prefix.rs`, new), and the build script's shader
fingerprint. The manifest carries the `tilo-vello-shaders` publication
identity and the archive's `Cargo.lock` is omitted. `vendor/PATCHES.md` is the
change log (the `vendor/vello_shaders/...` sections, "Published forks");
`TILO-FORK.md` is the published crate's readme.
