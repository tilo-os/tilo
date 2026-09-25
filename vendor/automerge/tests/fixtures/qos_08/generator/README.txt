Generator for this corpus. It is a standalone Cargo project built against the
qOS-patched automerge 0.8.0 from the Cloud alignment branch
(origin/codex/cloud-main-alignment-20260921, vendor/automerge + vendor/hexane).
To regenerate: copy Cargo.toml.txt -> Cargo.toml and main.rs.txt -> src/main.rs
in a scratch directory, point the two paths at a checkout of that vendor tree,
then run `cargo run -- <this directory>`. Commit times are fixed, so output is
deterministic apart from DEFLATE encoder changes.
