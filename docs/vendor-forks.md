# Exportable vendor-fork histories

The source under `vendor/vello`, `vendor/vello_encoding`, and `vendor/winit`
comes from standalone topic histories rather than opaque copy-and-edit trees.
Run `tools/vendor-history.sh verify` to check the recorded roots and tips and
to prove that the checked-in source still corresponds to them. Run
`tools/vendor-history.sh export OUTPUT_DIRECTORY` to produce Git bundles and
one patch file per reviewable commit.

## Winit's Tilo-only depackaging overlay

The winit history deliberately remains six upstream-facing patches. Its root
is the normalized crates.io source for winit 0.30.13, whose archive contains
four pieces of Cargo publication bookkeeping that Tilo does not commit:

- `.cargo-ok`
- `.cargo_vcs_info.json`
- `Cargo.lock`
- `Cargo.toml.orig`

The vendored normalized `Cargo.toml` has one further metadata-only block:
`package.metadata.cargo-machete.ignored`. It tells the repository-wide unused
dependency audit about build-script, target-conditional, and
feature-forwarding dependencies that source scanning cannot discover. It does
not alter compilation and does not belong in an upstream winit patch.

`tools/verify-vendor-trees.py` applies exactly that normalization while
comparing the histories: it excludes only the four named publication files,
removes exactly one known `cargo-machete` block from the vendored manifest,
and requires every remaining tracked path and byte to match. Any new local
delta fails verification instead of silently becoming part of the fork.

## Librespot build identity

The vendored librespot-core 0.8.0 build script reports the upstream revision
from its crates.io `.cargo_vcs_info.json`, rather than Tilo's enclosing Git
repository. Its build ID is the package version and upstream revision; dates
absent from publication metadata are explicitly `unknown`. This local build
metadata patch keeps the shared runtime identical across unrelated application
commits and build times, so signed partial app collections can reuse it safely.
The collection runtime-root integrity check remains mandatory.
