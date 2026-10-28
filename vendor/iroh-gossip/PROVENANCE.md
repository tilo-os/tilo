# iroh-gossip provenance

- Package: `iroh-gossip` 0.101.0 (crates.io)
- Registry checksum: `4e1dc4b05f73e7a1b9e83b531eb63c3fd671b0af3aeb13b59c546dd7ca747515`
- Repository: https://github.com/n0-computer/iroh-gossip
- Upstream revision: `2ce78afe09d89d41d123f28eac19bdc831609cc8` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: repository root
- License: MIT OR Apache-2.0 (the manifest writes `MIT/Apache-2.0`);
  `LICENSE-MIT` and `LICENSE-APACHE` are the published archive's files, byte
  for byte.
- Vendored: 2026-10-10 (`94d5a803a4`)

Local changes: plumtree sweeps a topic's message cache only while it holds
messages (`src/proto/plumtree.rs`, marked `TILO PATCH`); the application may
share gossip's connections (`Builder::connections`, `Gossip::connection` in
`src/net.rs`, marked `TILO PATCH`); and the `[lib]` builds an `rlib` only. `TILO-PATCH.md` has the details and an upstream-ready
description; `vendor/PATCHES.md`, "iroh idle timers", the summary. The
archive's `Cargo.lock` is omitted.
