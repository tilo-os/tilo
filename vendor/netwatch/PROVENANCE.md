# netwatch provenance

- Package: `netwatch` 0.19.1 (crates.io)
- Registry checksum: `4d9cbe01741347ef750d743d6690603f5eed8341e679fb51c8e629337aa11976`
- Repository: https://github.com/n0-computer/net-tools
- Upstream revision: `051ab8761006d7f2155e34a49f6bb881b582d5ab` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `netwatch`
- License: MIT OR Apache-2.0. The published archive carries no licence text;
  `LICENSE-MIT` and `LICENSE-APACHE` are the repository root's files at the
  upstream revision, byte for byte.
- Vendored: 2026-10-10 (`94d5a803a4`)

Local changes: on Linux the network monitor notices a resume or clock step
through a cancel-on-set `timerfd` (`src/netmon/clock_change.rs`, new) instead
of polling the wall clock every 15 s (`src/netmon.rs`,
`src/netmon/actor.rs`), so an idle endpoint stops waking. Marked `TILO PATCH`
in the source; `TILO-PATCH.md` has the details and an upstream-ready
description, and `vendor/PATCHES.md`, "iroh idle timers", the summary. The
archive's `Cargo.lock` is omitted.
