# zlink-macros provenance

- Package: `zlink-macros` 0.7.0 (crates.io)
- Registry checksum: `2fc623b8323797bf2d48333e7633e143077dd92cc66d9cca170014b66c43b1b5`
- Repository: https://github.com/z-galaxy/zlink
- Upstream revision: `5dce357e001e0aa5c6df3e51323f58056770652c` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `zlink-macros`
- License: MIT; `LICENSE` is the published archive's file, byte for byte.
- Vendored: 2026-10-04 (`816c8025c6`)

Local changes: the service macro emits its `ReplyStreamParams` variants sorted
by type instead of in `HashMap` order, so every build of a binary that expands
it is the same (`src/service/codegen.rs`). Not a workspace member: the OS
image's reproducible bootc build uses it. The archive's `Cargo.lock` is
omitted. See `vendor/PATCHES.md`, "zlink-macros".
