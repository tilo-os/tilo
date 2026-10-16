# wgpu provenance

- Package: `wgpu` 29.0.3 (crates.io)
- Registry checksum: `bb3feacc458f7bee8bc1737149b42b6c731aa461039a4264a67bb6681646b250`
- Repository: https://github.com/gfx-rs/wgpu
- Upstream revision: `4cbe6232b2d7c289b6e1a38416a6ae1461a22e81` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `wgpu`
- License: MIT OR Apache-2.0; `LICENSE.MIT` and `LICENSE.APACHE` are the
  published archive's files, byte for byte.
- Vendored: 2026-09-29 (`7e71c2f331`)

Local changes: `Device::poll` and `Instance::poll_all_devices` report a lost
device instead of panicking, so the compositor survives a GPU reset
(`src/backend/wgpu_core.rs`); `Device::create_compute_pipeline_async`, which
on the WebGPU backend is `createComputePipelineAsync`, so a browser compiles
vello's pipelines off its GPU process's main thread (`src/api/device.rs`,
`src/backend/webgpu.rs`; other backends create the pipeline synchronously);
lint allowances for `wasm32-unknown-unknown` under `-D warnings`
(`src/lib.rs`); the two SPIR-V test fixtures are stored as `.hex` text and
decoded by `build.rs` (`fixture_build.rs`, `src/macros/mod.rs`). The
archive's `Cargo.lock` is omitted. `TILO-PATCH.md` has the details of the
lost-device change;
`vendor/PATCHES.md`, "Published forks", says why it stays a `[patch]`.
