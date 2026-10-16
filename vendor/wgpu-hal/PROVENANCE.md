# wgpu-hal provenance

- Package: `wgpu-hal` 29.0.3 (crates.io)
- Registry checksum: `31f8e1a9e7a8512f276f7c62e018c7fa8d60954303fed2e5750114332049193f`
- Repository: https://github.com/gfx-rs/wgpu
- Upstream revision: `4cbe6232b2d7c289b6e1a38416a6ae1461a22e81` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `wgpu-hal`
- License: MIT OR Apache-2.0; `LICENSE.MIT` and `LICENSE.APACHE` are the
  published archive's files, byte for byte.
- Vendored: 2026-09-18 (`c5eefeb39f`)

Local changes: a DX12 compiled-shader cache import/export
(`src/dx12/shader_cache.rs`, `src/dx12/shader_cache_key.rs`, new), simulated
device loss and presentation damage on Vulkan, IEEE-safe Metal shaders, and a
one-shot Metal presentation observer (`Surface::observe_next_present`). The
imported source follows the workspace rustfmt style and gives reasons for
retained upstream lint exceptions. The archive's `Cargo.lock` is omitted.
`TILO-PATCH.md` and `TILO-PRESENTATION-FEEDBACK.md` have the details;
`vendor/PATCHES.md`, "Published forks", says why it stays a `[patch]`.
