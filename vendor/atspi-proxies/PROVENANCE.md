# atspi-proxies provenance

- Package: `atspi-proxies` 0.13.0 (crates.io)
- Registry checksum: `2230e48787ed3eb4088996eab66a32ca20c0b67bbd4fd6cdfe79f04f1f04c9fc`
- Repository: https://github.com/odilia-app/atspi
- Upstream revision: `f486755b9bf993613f63a779133aafc54925004a` (tag v0.29.0; the archive's `.cargo_vcs_info.json`)
- Upstream package path: `atspi-proxies`
- License: Apache-2.0 OR MIT; `LICENSE-APACHE2.txt` and `LICENSE-MIT.txt` are the archive's files.
- Vendored: 2026-10-22, replacing the git fork `tilo/atspi` at
  `9183f58d2e18e7c7cd76a1436f7fedc3ada015c2` (the same release plus the change
  below), so a build needs no git host.

Local change, in `Cargo.toml` only: zbus's runtime backend follows new
`async-io` and `tokio` features (`async-io` is a default feature, so a
default build is unchanged) instead of `async-io` always, and the atspi crates
depend on each other without default features, forwarding `wrappers`,
`async-io` and `tokio` explicitly. A tokio build then compiles one async
runtime. Every source file is the release's. The archive's `Cargo.lock` is
omitted. `atspi-connection` is not vendored: nothing in this workspace enables
atspi's `connection` feature, and the crates.io release it would resolve to has
no `async-io`/`tokio` features, so atspi does not forward them to it. atspi
0.30.0 still enables `zbus/async-io` unconditionally (checked 2026-10-22). See
`vendor/PATCHES.md`, "Accessibility and YouTube manifests".
