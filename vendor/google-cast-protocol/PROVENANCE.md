# google-cast-protocol provenance

- Package: `google-cast-protocol` 0.1.0
- Registry checksum:
  `14b373dd5de8238fe295be654847a14c74e9b2a5b1215897e7c5c8ebe6c33bed`
- Repository: `https://gitlab.futo.org/videostreaming/fcast`
- Upstream revision: `50e59efdea9fab7cce5ec1e107a2a3bd089a7444`
- Upstream package path: `crates/google-cast-protocol`
- License: MIT. The published archive carries no licence text; `LICENSE` is
  the repository root's file at the upstream revision, byte for byte.

Local patch: the published crate compiles `src/googlecast.proto` in its build
script with `protox` and `prost-build`, which puts a protobuf compiler, its
parser and diagnostics stack in every build of `tilo-projector`. `src/protos.rs`
is that build script's output for the unchanged `.proto`, committed, and
`src/lib.rs` includes it instead of `$OUT_DIR/protos.rs`; the manifest drops
`build.rs` and its build dependencies. Everything else is the published source.

To regenerate after a `.proto` change, build the published crate and copy its
`$OUT_DIR/protos.rs`.

The workspace root selects this copy with its reviewed Cargo patch. Keep any
future source change explicit here; do not edit manifests or the lockfile
implicitly.
