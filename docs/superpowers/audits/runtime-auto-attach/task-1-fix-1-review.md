# Task 1 fix round 1 review

## Scoped verdicts

- **ADDRESSED — forced offline first-launch build.** The developer-host Cargo invocation no longer injects `--offline` (`crates/runtime/runtime-portal/src/native.rs:235`). The controlled-Cargo regression now checks the ordinary invocation and separately verifies that an explicitly supplied `CARGO_NET_OFFLINE=true` survives unchanged (`crates/runtime/runtime-portal/tests/native.rs:134`, `crates/runtime/runtime-portal/tests/native.rs:136`). This restores normal Cargo dependency, cache, configuration, and network behavior while retaining user-selected offline operation.

- **ADDRESSED — ordinary SDK child treated as a malformed inherited transfer, including same-app resume.** Automatic transfers now carry the receiving process PID (`crates/runtime/runtime-portal/src/native.rs:143`, `shell/src/runtime_host/daemon.rs:388`). Bootstrap ignores descriptor metadata stamped for an ancestor, while a missing stamp retains legacy/current authority and a current, zero, or malformed stamp remains fail-closed (`crates/runtime/runtime-portal/src/native.rs:7`, `crates/runtime/runtime-portal/src/native.rs:74`). SDK discovery applies the same provenance check before exposing collaboration or identity descriptors, and rejects negative descriptors before `BorrowedFd` construction (`crates/runtime/runtime-portal/src/lib.rs:81`, `crates/runtime/runtime-portal/src/lib.rs:88`). Fresh managed transfers clear ancestor ownership and resume metadata (`shell/src/runtime_host/unix.rs:84`); automatic `--run-app` restores only the exact process/app resume and publishes the selected profile (`shell/src/runtime_host/daemon.rs:372`, `shell/src/runtime_host/daemon.rs:387`). The isolated real SDK-parent to ordinary `Command` child regression observes two distinct registrations for the same app without per-app environment clearing (`shell/src/runtime_host/daemon_process_tests.rs:308`), and the UI regression proves an ancestor resume marker does not suppress the same-app child's command handler (`ui/src/launch.rs:1749`, `ui/src/launch.rs:1770`).

## New breakage in the fix package

None identified. The package introduces no application allowlist and no process-global environment mutation. Current stamped transfers and unstamped legacy transfers remain authoritative; partial, malformed, closed, aliased, stdio, wrong-transport, and malformed-provenance cases remain fail-closed. The exec path still preserves the original executable, opaque argv, and working directory, and now propagates the resolved profile to descendants (`shell/src/runtime_host/daemon.rs:389`).

The supplied evidence is consistent with the implementation: portal 13 passed, UI launch 32 passed, host runtime 36 passed, and the host binary build completed. Per review instructions, these suites were not rerun. `git diff --check` on the exact seven-file fix range was clean.

## Out-of-scope observations

- Browser private-runtime behavior is separately reviewed and committed as `a3720f6ff`; it is not part of this fix package.
- Actual ordinary native GUI launches, declared-action execution, visible-frame evidence, and final all-app proof remain the next proof task and are not established here.
- Previously disclosed unrelated warnings and lint limitations remain outside this review.

## Overall verdict

**PASS.** Both original Important findings are addressed by fix commit `6dce082e8`, with no new in-scope breakage found in `task-1-fix-1-review.diff`.
