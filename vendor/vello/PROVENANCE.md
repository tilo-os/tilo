# vello provenance

- Package: `vello` 0.9.0 (crates.io), carried as `tilo-vello` 0.9.0
- Registry checksum: `261359dbef879f8110ef7e1c442246c838d33d3d91cb05e0ea9288d432760c9f`
- Repository: https://github.com/linebender/vello
- Upstream revision: `875f324f21da93019cae9e8e61d4abfd69893206` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `vello`
- License: Apache-2.0 OR MIT; `LICENSE-APACHE` and `LICENSE-MIT` are the
  published archive's files, byte for byte.
- Vendored: 2026-08-27 (`1ea1c718aa`)

Local changes: bump-allocated scratch buffers sized from measured frame usage
and shrunk again when a scene gets smaller (`src/bump_budget.rs`, new), image
atlas limits, eviction and stats, images drawn from a texture
(`src/image_source.rs`, new) or an external source, deferred pipeline
creation, one init thread on GL, transient and freed GPU buffers destroyed on
WebGPU, and the shader fingerprint. The manifest carries the `tilo-vello`
publication identity. `vendor/PATCHES.md` is the change log ("Vello changes"
and the `vendor/vello/...` sections after it, "Published forks"); the
standalone history `vendor-upstream/vello-0.9` ("Exportable histories") holds
the changes as reviewable commits on the pristine archive, and
`TILO-FORK.md` is the published crate's readme.
