# File-lease slice integration review

## Verdicts

**Spec compliance: Approved for this implementation slice.** The primitive and both consumers meet the file-lease plan. This is not approval of the complete platform migration or native Linux/Windows qualification.

**Code quality: Approved, with two nonblocking test-evidence limitations below.** No Critical or Important code finding was identified in the supplied snapshot-relative diff.

**Ready to merge: Yes for the reviewed code slice; shared-checkout delivery remains conditional on controller reconciliation of the formatter incident.** This review does not certify that reconciliation as complete.

## Strengths

- `crates/system/atomic-file/src/lease.rs:68`: acquisition consumes the already-open file, returns a guard only after successful native locking, and retains a private handle. Shared/exclusive and Busy/I/O distinctions are preserved. There is no path reopen, clone, explicit unlock surface, or silent mutex fallback.
- `crates/packaging/app-catalogue/src/client.rs:281`: the named lease covers checkpoint read, rollback/fork validation, serialization, and atomic publication. The explicit-root extraction retains the existing live config location, filename hash, no-config behavior, checkpoint format, and error vocabulary.
- `crates/packaging/app-catalogue/src/client.rs:301` and `crates/packaging/registry-server/src/moderation.rs:539`: both callers retain the exact dedicated sentinel, open without truncation, and propagate open/acquisition errors. Atomic replacement of data does not replace the locked sentinel.
- `crates/packaging/registry-server/src/moderation.rs:526`: field declaration order closes the lease before releasing the process mutex, preserving the previous native-unlock ordering. Existing production transaction guard bindings remain intact at lines 54, 112, 178, 211, and 267.
- The supplied manifest/lockfile changes remove only obsolete direct libc ownership and the two corresponding dependency edges. No dependency resolution, version, or storage format change is part of this review package.
- The integration tests use production leases and real child processes, including normal and abrupt release. The metadata replacement test distinguishes the held inode from its former path. Parent cleanup owns child termination/reaping.

## Issues

### Critical

None.

### Important

None in the scoped implementation.

### Minor

1. **Timed negative observations are scheduling-sensitive.** `crates/packaging/app-catalogue/src/client.rs:562` and `crates/packaging/registry-server/src/moderation.rs:762` observe 150 ms after a readiness marker written before acquisition. A descheduled child can satisfy the negative assertion without attempting the lock in that interval. Subsequent independent Busy probes do prove actual sentinel ownership, so this does not invalidate the integration or imply a production defect. Keep the limitation explicit; a future stronger acquisition-observation harness should prove entry into the blocked operation. The primitive's observation at `crates/system/atomic-file/tests/file_lease.rs:236` retains a smaller version of that scheduling gap, but has additional held-lock proof and recorded mutation-test evidence; the approved fix need not be reopened.

2. **Concurrent checkpoint start does not guarantee overlapping transactions.** `crates/packaging/app-catalogue/src/client.rs:614` releases both writers together, but scheduling may serialize their read/validate/write sections. The test verifies real persisted outcomes, yet could miss a future premature guard drop when the writers happen not to overlap. A future controlled transaction pause with a contender probe would strengthen this regression. Current production scope at line 281 is correct, so this remains nonblocking.

## Evidence and remaining obligations

- Reviewed the full supplied `slice-review.diff`, implementation plan, framework contract, reports, task re-review, and progress ledger. Focused current-source inspection confirmed production guard bindings, declaration order, and cited test locations. No product edits, staging, commits, formatters, subagents, or test reruns were performed.
- Reported native macOS evidence: atomic-file 12 existing unit tests plus 10 integration tests; catalogue client 7 tests; registry moderation filter 14 tests; all passed. Affected test-target compilation passed after the dependency edits. The controller additionally reports `FFMPEG_DIR=/opt/homebrew/opt/ffmpeg@8 cargo check -p shell --tests --locked --offline` passed with existing warnings. These are reported execution results, not new executions by this reviewer.
- Primitive Linux ARM64 and Windows x64 test-target compilation is compilation evidence only. Native contention, transaction behavior, and host qualification remain outstanding. Windows publication/privacy and full S02 acceptance are outside this completed slice.
- The A08-16 entry in `docs/superpowers/plans/2026-09-18-cross-platform-framework/finding-register.json:3922` now records the production migration and macOS evidence while leaving cross-host work outstanding; the prior register omission is resolved.
- The deferred existing launcher warnings are warning debt rather than a finding introduced by this diff; they do not justify unrelated edits or further test runs here.
- The formatter incident recorded in `task-2-report.md` remains a controller checkout-preservation obligation. This scoped review cannot certify unrelated UI state or authorize a broad revert.

## Assessment

The shared primitive and migrated callers form a coherent ownership boundary: native acquisition is fallible, each persistent sentinel stays owned for the transaction, and the existing domain semantics remain intact. Accept the reviewed slice while retaining the two minor regression limitations and the separate reconciliation/native-qualification obligations.
