# iroh provenance

- Package: `iroh` 1.0.3 (crates.io)
- Registry checksum: `460de6bc52163b41b1646931f2897e5ab986f0966ade444467fec25024751a72`
- Repository: https://github.com/n0-computer/iroh
- Upstream revision: `f2eb930dda3779c6d852b72f3712aacd6e573ab1` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `iroh`
- License: MIT OR Apache-2.0. `LICENSE-BSD3` is the published archive's file
  (Tailscale's licence, for the parts of `src/socket` derived from it); the
  archive has no MIT or Apache text, so `LICENSE-MIT` and `LICENSE-APACHE` are
  the repository root's files at the upstream revision, byte for byte.
- Vendored: 2026-10-10 (`94d5a803a4`)

Local changes: an endpoint with an empty relay map stops its re-STUN tick and
releases its home relay, and its `[lib]` builds an `rlib` only (two path
builds of a `cdylib` crate collide in one Cargo run). Each change is marked
`TILO PATCH` in the source; `TILO-PATCH.md` has the details and an
upstream-ready description, and `vendor/PATCHES.md`, "iroh idle timers", the
summary. The archive's `Cargo.lock` is omitted.
