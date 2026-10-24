# davey provenance

- Package: `davey` 0.1.5 from crates.io, a Rust implementation of Discord's
  DAVE end-to-end encryption for voice (daveprotocol.com) on OpenMLS.
- Repository: https://github.com/Snazzah/davey
- Archive SHA-256:
  `27466c95d1885cc506b29cec686a91ba7c472376cca09850d026cceb9c29a888`
- License: MIT; `LICENSE` is the repository's file on its `master` branch,
  which the crate archive does not carry.
- Vendored: 2026-10-23, for the Discord connector's voice bridge.

The tree is the archive's `src/` and `README.md`, with `Cargo.toml` from the
archive's `Cargo.toml.orig`, changed as `TILO-PATCH.md` describes. Drop the
copy for the crates.io release once one builds on openmls 0.9.
