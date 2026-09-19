# Deterministic native source order

Upstream: `openh264-sys2` 0.9.6, crates.io archive SHA-256
`fa9e072e9b270f3b291c80488dc160abc31ecc214ab3bfde937213cfd8c83b32`.
Upstream commit: `b8c508d223c9cc2c40dc9c27508b7b321c8e44f0`.

The only upstream code change is `.sort_by_file_name()` in `build.rs`'s
`glob_import`. The previous `WalkDir` filesystem order reached both `cc` and
`nasm-rs`, which preserve the supplied object order in native archives. Identical
registry source trees can enumerate differently, changing link input order.
Sorting every traversed directory makes that order stable while preserving the
existing extension and exclusion filters for C++, NASM and ARM assembly.

All other imported upstream source and license content is unchanged; Git applies
the repository's usual LF normalization. The archive's own Cargo.lock is omitted
under the existing `vendor/*/Cargo.lock` ignore rule: the workspace lock governs
dependencies. The patch retains version 0.9.6 and its existing dependency
versions; only this package's registry source/checksum entries are removed from
the workspace Cargo.lock.

`python3 os/tests/test_openh264_source_order.py` compiles the actual production
enumeration function, tests opposite file creation orders and exclusion filters,
and compiles a small native archive fixture. It does not build the codec or OS.
This fixes an uncontrolled input, not a claim that the observed complete-runtime
hash difference has already been fully attributed or resolved. Native runtime
qualification and the existing exact signed-runtime guard remain required.
