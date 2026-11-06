# cargo, patched

The CI builder (`os/Containerfile.builder`) replaces the pinned toolchain's
`bin/cargo` with cargo 1.97.1 built from its upstream source and the patch
here. rustc and every other component stay the toolchain's, so compiles and
their cached outputs are unchanged. `tools/ci/install-cargo.sh` builds and
installs it.

- Upstream: <https://github.com/rust-lang/cargo>, MIT OR Apache-2.0.
- Base: the cargo Rust 1.97.1 ships, commit
  `c980f4866141969fab6254a680546a277789d6f0` (`cargo-c980f48.tar.gz` in
  `../github-assets.tsv`).
- `cargo --version` reports
  `cargo 1.97.1 (c980f4866 2026-06-30) (tilo: rust-lang/cargo#17412)`.

## 0001-do-not-build-sbom-unless-build-sbom-is-set.patch

Upstream pull request: <https://github.com/rust-lang/cargo/pull/17412>
(commit `97888918b8c4b0f3447c5bc5980ac7cbb558f3f0`), first in cargo 1.100.
Cherry-picked without conflicts.

cargo 1.97.1 builds every unit's SBOM (its whole dependency closure) when it
prepares the unit's compile, whether or not `build.sbom` asks for one, and
throws it away unwritten. The prepared compiles are held until they run. Under
t's compiler wrapper, which queues compiles, a family as large as `test`
(about 3,000 units) holds about 1.2 GB of these, about three quarters of the
driver's 1.6 GB peak; 280 such drivers on one CI node held about 450 GB. With
the patch the SBOM is built only when an SBOM file is an output, and the
family's driver peaks at about 0.7 GB.

Delete this directory and `tools/ci/install-cargo.sh` when
`rust-toolchain.toml` moves to 1.100 or later; the script fails while the
toolchain's cargo is not 1.97.1.
