# rustypipe provenance

- Package: `rustypipe` 0.11.4 (crates.io)
- Registry checksum: `3bf8d7c42ac7860a078f63f90d6513dd3833585337e65b5fca9f74ddba2d59ef`
- Repository: https://codeberg.org/ThetaDev/rustypipe
- Upstream revision: `1f4c9c85b95944f5e0da4fce5485b7525abaaced` (release v0.11.4; the archive's `.cargo_vcs_info.json`)
- License: GPL-3.0; `LICENSE` is the archive's file.
- Vendored: 2026-10-22, replacing the git fork `tilo/rustypipe` at
  `aeb292fb23856c335ee3423745ed98ae0e683760` (the same release plus the change
  below), so a build needs no git host.

Local change, in `Cargo.toml` only: reqwest 0.13 instead of 0.12 and
fancy-regex 0.16 instead of 0.14, so the lock holds one reqwest (and no second
QUIC stack) and one fancy-regex. The source compiles unchanged against both.
reqwest 0.13 renamed its TLS features, so the TLS features map to the new
names: `native-tls` to `reqwest/native-tls-no-alpn`, `native-tls-alpn` to
`reqwest/native-tls`, `native-tls-vendored` to
`reqwest/native-tls-vendored-no-alpn`; new `rustls` and `rustls-no-provider`
features forward to reqwest's; `rustls-tls-webpki-roots` and
`rustls-tls-native-roots` are aliases of `rustls`, as reqwest 0.13 verifies with
the platform verifier. One source line differs: `Frameset::urls` in
`src/model/frameset.rs` names its iterator's lifetime (`FramesetUrls<'_>`),
which current rustc's `mismatched_lifetime_syntaxes` lint requires under
`-D warnings` now that the crate builds as a path dependency, whose lints
Cargo does not cap; the signature means the same. Every other source file
is the release's. The archive's `Cargo.lock` is omitted. 0.11.4 is the newest release (checked 2026-10-22). See
`vendor/PATCHES.md`, "Accessibility and YouTube manifests".
