# libspa-sys provenance

- Package: `libspa-sys` 0.10.1 (crates.io)
- Registry checksum: `3b6e17bdaf63ed0d5e4144022624032b41fd9733112e8c74ac26fc9bf1291924`
- Repository: https://gitlab.freedesktop.org/pipewire/pipewire-rs
- Upstream revision: `70b20f096ba1a9758101e254da605795a02a28c7` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `libspa-sys`
- License: MIT; `LICENSE` is the published archive's file, byte for byte.
- Vendored: 2026-09-19 (`492b2e148f`)

Local changes:

- `wrapper.h` includes the SPA headers that PipeWire split out in later
  releases (`spa/param/audio/layout-types.h`, `spa/pod/body.h`,
  `spa/utils/json-core.h`) only where they exist, so the bindings build
  against Fedora 42's headers and against Ubuntu 24.04's PipeWire 1.0, and
  also includes `spa/pod/dynamic.h`;
- the manifest gains `[package.metadata.cargo-machete]` naming the build
  dependencies `build.rs` uses, which the unused-dependency check cannot see;
- `src/type_info.rs` has one import list reordered by rustfmt;
- the archive's `Cargo.lock` is omitted.
