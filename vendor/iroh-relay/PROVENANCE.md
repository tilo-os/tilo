# iroh-relay provenance

- Package: `iroh-relay` 1.0.3 (crates.io)
- Registry checksum: `24bd586cf927f7b700f56ec3639b53cb5fa901ce284784051ff71092bfbf8193`
- Repository: https://github.com/n0-computer/iroh
- Upstream revision: `f2eb930dda3779c6d852b72f3712aacd6e573ab1` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `iroh-relay`
- License: MIT OR Apache-2.0. `LICENSE-BSD3` is the published archive's file;
  `LICENSE-MIT` and `LICENSE-APACHE` are the repository root's files at the
  upstream revision, as in `vendor/iroh`.
- Vendored: 2026-11-10

Local changes: a client may send right behind its answer to the relay's
challenge (`ClientBuilder::send_before_confirmed`), and the `[lib]` builds an
`rlib` only. Each change is marked `TILO PATCH` in the source;
`TILO-PATCH.md` has the details and an upstream-ready description. The
archive's `Cargo.lock` is omitted.
