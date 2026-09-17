# Final fix review: Cargo target artifact discovery

## Verdict

**ADDRESSED. No new fix-only breakage identified.** Commit `2acfd7d8c` resolves the one Important finding from `final-review.md`: a missing developer host can now be discovered from Cargo's actual compiler-artifact message when ordinary Cargo configuration places it beneath a configured native target triple. The final-ready gate is clear for this finding.

## Scope and method

Reviewed the binding spec, the original finding in `final-review.md`, `final-fix-report.md`, and the exact four-file package in `final-fix-review.diff`. The diff payload matches commit `2acfd7d8c` exactly when reproduced with ten context lines. Reviewed paths were `Cargo.lock`, `crates/runtime/runtime-portal/Cargo.toml`, `crates/runtime/runtime-portal/src/native.rs`, and `crates/runtime/runtime-portal/tests/native.rs` only.

This was a scoped read-only code review apart from this report. I did not inspect or assess the unrelated workspace formatting changes, and made no source, index, branch, or commit changes. I did not use subagents, launch GUI apps, or rerun broad suites. Native-3 remains the valid baseline behavior proof; this fix changes only the missing-host developer bootstrap branch.

## Assessment

- `build_host` requests Cargo JSON output and continuously drains stdout on a dedicated reader thread, avoiding pipe backpressure while preserving rendered compiler diagnostics on stderr.
- `cargo_artifact` selects the `qos-runtime-mcp` compiler artifact and uses Cargo's reported executable instead of assuming `target/debug/qos-runtime-mcp`. Before use, the executable must exist and its canonical path must remain within the selected target directory.
- A private `.qos-native-host-path` cache records the selected executable under the existing build lock. Subsequent launches validate the cache file as a regular, single-link, current-user-owned file before reading it, validate the cached executable again, and reuse it without another build.
- Cache writes open without truncation, validate ownership/type/link count first, and only then truncate and rewrite. Symlinks are rejected with `O_NOFOLLOW`.
- The existing ten-minute deadline still bounds lock acquisition and the Cargo process. After Cargo exits, the same deadline also bounds reader completion; timeout kills and reaps the owned process group.
- The default `target/debug` path remains the first compatible reuse path and the post-build fallback for controlled wrappers or older Cargo behavior. The existing concurrent test still proves one serialized build across three launchers.
- Cargo configuration and environment are inherited. The fix adds only `--message-format=json-render-diagnostics` and the already-established explicit `--target-dir`; it does not override target selection, Cargo config, network settings, or `CARGO_NET_OFFLINE`.
- The focused regression selects a target through `CARGO_HOME/config.toml`, emits the configured-target compiler artifact, launches it successfully twice, and proves one Cargo invocation. This covers configuration-file selection rather than only `CARGO_BUILD_TARGET`.

## Verification evidence

The supplied focused/native log records **14 runtime-portal native tests passed, 0 failed**, including configured-target discovery and cache reuse, default-layout fallback, concurrent serialized construction, and absent/explicit offline propagation. `final-fix-report.md` records `cargo clippy -p runtime-portal --lib -- -D warnings` passing for production code; the disclosed test-harness lint findings are pre-existing and outside this fix.

Fresh review checks confirmed that the diff section in `final-fix-review.diff` hashes identically to `git show --unified=10 2acfd7d8c` for the four owned paths, and `git diff --check 2acfd7d8c^ 2acfd7d8c` reports no whitespace errors on those paths.

## Findings

No Critical, Important, or Minor findings in the fix-only scope.
