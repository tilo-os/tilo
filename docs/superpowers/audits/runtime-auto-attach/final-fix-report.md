# Automatic runtime attachment final fix

## Scope and base

- Dispatch base observed before edits: `27dd198915e9f4deee743b85feea81847070a0f2` (requested lineage includes `9c6de88b3`; concurrent commits advanced HEAD during the wave).
- Commit parent at staging: `86b543af3c4a375f82e6242475350918807ab843`.
- Exact owned commit: `2acfd7d8c` (`fix: discover configured runtime build artifact`).
- Committed paths only: `Cargo.lock`, `crates/runtime/runtime-portal/Cargo.toml`, `crates/runtime/runtime-portal/src/native.rs`, and `crates/runtime/runtime-portal/tests/native.rs`.

## Root cause and fix

The developer bootstrap treated successful Cargo completion as sufficient only when `CARGO_TARGET_DIR/debug/qos-runtime-mcp` existed. Cargo places the executable beneath a configured target triple when `CARGO_BUILD_TARGET` or Cargo config selects a target, so the bootstrap rejected a successful build.

The build now requests Cargo JSON messages, continuously drains stdout on a reader thread, selects the `qos-runtime-mcp` compiler artifact, validates/canonicalizes it inside the selected target directory, and records it in a private owner/link-validated cache under the existing build lock. The default `target/debug` fallback remains for compatible wrappers. The reader completion and build process remain governed by the original ten-minute deadline; timeout kills/reaps the owned process group. Cache validation occurs before truncation. Cargo environment, config, network and explicit offline behavior pass through unchanged.

## RED / GREEN evidence

- RED: `cargo test -p runtime-portal --test native developer_build_uses_reported_configured_target_artifact_and_reuses_it -- --exact --nocapture`
  - Failed with Cargo exit status 0 after reporting `.../config-file-target/debug/qos-runtime-mcp`; bootstrap emitted `native runtime build failed: exit status: 0`.
  - Log: `final-fix-red.log`.
- GREEN focused: same exact test passed, including a Cargo-home config-file-selected target and a second launch proving cached reuse (one build call total).
- GREEN suite: `cargo test -p runtime-portal --test native`
  - 14 passed, 0 failed. Retains default target, concurrent serialized build, absent/explicit offline environment, visible failure, inherited connection and transport coverage.
  - Log: `final-fix-green.log`.
- GREEN lint: `cargo clippy -p runtime-portal --lib -- -D warnings`
  - Passed.
- Diff check: `git diff --check -- Cargo.lock crates/runtime/runtime-portal/Cargo.toml crates/runtime/runtime-portal/src/native.rs crates/runtime/runtime-portal/tests/native.rs`
  - Passed before the exact-path commit.

## Limits and workspace note

No GUI/native acceptance workflow was rerun because the correction is confined to developer host artifact discovery and Native-3 remains valid. `cargo clippy -p runtime-portal --test native -- -D warnings` reaches three pre-existing test-harness policy findings (`unwrap`, `process::exit`, and `Vec::drain`); the production library lint is clean.

One incorrectly scoped command, `cargo fmt -- crates/runtime/runtime-portal/src/native.rs crates/runtime/runtime-portal/tests/native.rs`, caused Cargo fmt to visit workspace Rust files at approximately 2026-09-17 15:51 PT. No nonowned recovery writes were attempted and no nonowned path was staged or committed. The controller was notified immediately and is coordinating shared-worktree preservation. Subsequent formatting used direct `rustfmt` on the two owned Rust files.
