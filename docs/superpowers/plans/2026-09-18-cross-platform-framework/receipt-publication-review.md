# Spec Compliance

- ✅ Spec compliant for the bounded Task 8 source migration. Reviewed the complete 12-file snapshot-relative `task-8-review.diff` once against `task-8-before`, the brief, report, and binding receipt-publication design. No blocking missing, extra, or misunderstood implementation was found.
- ✅ Held-directory admission precedes sentinel creation, locking, enumeration, receipt reads, and reconciliation; only the incoming directory is duplicated (`crates/runtime/runtime-fs/src/recovery.rs:164`). Shared Replace/OwnerOnly publication replaces the complete custom writer (`crates/runtime/runtime-fs/src/recovery.rs:274`).
- ✅ Initial uncertain publication poisons without inserting the receipt; initial NotPublished remains retryable. Updates poison on write failure and insert memory only after success (`crates/runtime/runtime-fs/src/recovery.rs:374`, `:425`). Publication retains its owned source and the persisted code remains exactly StoreUnavailable (`crates/runtime/runtime-fs/src/grants.rs:99`, `:118`; `crates/runtime/runtime-fs/src/recovery.rs:740`).
- ✅ First publication custody survives failed recovery and terminal item/finish writes, with queued updates drained before one typed error and then sticky StoreUnavailable (`crates/runtime/runtime-fs/src/operations.rs:119`, `:476`, `:543`, `:563`). No commit guard or lock authority is cloned.
- ✅ Linux uses the selected held-fstatfs permission profiles and explicit Unsupported for other profiles; native ownership/mode checks follow. Directory admission has no fixed link-count gate, while private files retain single-link enforcement (`crates/system/atomic-file/src/native/unix.rs:75`, `:383`). Windows composes the accepted current-principal security helper without altering it (`crates/system/atomic-file/src/native/windows.rs:144`). Continuing host/backing-namespace premises and the separation from durability are documented (`crates/system/atomic-file/src/directory.rs:27`).
- ⚠️ Native Windows/Linux runtime, distinct-principal/wrong-owner enforcement, malformed/unreadable macOS ACL fixtures, crash qualification, actual production journal provisioning, and the pre-existing ignored cross-volume test remain unqualified, as explicitly reported in `task-8-report.md:83`. Source approval does not close those obligations. The required shell test-target check remains separately coordinated by the parent (`task-8-report.md:66`).

# Strengths

- The test-only worker seam wraps the actual bounded writer with scoped atomic-file faults on the calling worker thread, rather than manufacturing an error (`crates/runtime/runtime-fs/src/recovery.rs:363`). Its 16-case worker matrix checks exact typed phases/source, failed-checkpoint memory, poison, retained staging/destination bytes, no Finished, and reopen classification (`crates/runtime/runtime-fs/src/operations.rs:1867`).
- A separate two-item barrier test observes queued success draining before the retained terminal publication error (`crates/runtime/runtime-fs/src/operations.rs:1959`). Move, restore, and recovery-after-native-failure tests check actual retained user-file effects (`:2004`, `:2042`, `:2094`).
- Commit retry and lifetime tests explicitly distinguish publication handles from writer exclusion, and show that manual retry cannot repair journal memory or unpoison the owner (`crates/runtime/runtime-fs/src/recovery.rs:979`).
- The fixed v1 payload is an independent literal compared with production writer output and decode/re-encode, including non-UTF8 names and legacy native identity fields; the same test verifies historical pending evidence remains untouched (`crates/runtime/runtime-fs/src/recovery.rs:1069`). Its prechange capture provenance is documented in `task-8-report.md:39`.
- Admission regressions check rejection before effects using unchanged existing bytes/mtime and namespace count; renamed-directory tests exercise both before-open and after-open authority, writer contention, and independent replacement admission (`crates/runtime/runtime-fs/tests/native_operations.rs:671`, `:696`, `:732`).

# Issues

## Critical

- None found.

## Important

- None found.

## Minor

- `crates/runtime/runtime-fs/src/operations.rs:283`: `FsSession::start` still documents only StoreUnavailable/Busy/LimitExceeded/live-scope admission errors, although failed initial receipt publication now returns an owned Publication error. Add that variant and distinguish initial NotPublished from uncertain publication poisoning, or link to the relevant error contract. This is a public API documentation gap, not a runtime blocker.

# Focused checks and evidence

- Read the cut-off `OperationOwner::open`, `write`, `update`, and `admit` function context (`crates/runtime/runtime-fs/src/recovery.rs:164`, `:274`, `:321`, `:382`) to check pre-effect ordering, reconciliation propagation, bounds, poison, and insertion order.
- Named risk: new privacy errors could lose explicit Unsupported classification. Checked `crates/runtime/runtime-fs/src/native.rs:206`; PermissionDenied becomes Denied and Unsupported remains the original I/O error.
- Named risk: shared native validation could weaken regular-file/reparse policy or fabricate security evidence. Checked the unchanged Windows helper (`crates/system/atomic-file/src/native/windows/acl.rs:157`, `:168`), Windows handle admission (`crates/system/atomic-file/src/native/windows.rs:103`, `:123`), Unix constructor (`crates/system/atomic-file/src/native/unix.rs:50`), and reused macOS ACL validator (`:447`). File-only link checks and held native security queries remain in place; the new directory path uses the security-only helper.
- Named risk: asynchronous custody could be invalidated by job/owner lifetime or a checkpoint failure becoming a generic item error. Checked Work/admission and Context failure/checkpoint helpers (`crates/runtime/runtime-fs/src/operations.rs:248`, `:302`, `:403`, `:442`) and restore's post-effect path (`:1749`). Owner authority is retained separately; original errors remain owned through the reviewed failure paths.
- Named risk: worker fault injection could leak or miss the actual thread. Checked existing thread-local scoped failure restoration (`crates/system/atomic-file/src/test_support.rs:52`, `:58`, `:105`) and the worker-local call in `recovery.rs:368`; the guard restores prior hooks on unwind.
- Read reported red/green evidence and exact selected counts (`task-8-report.md:37`, `:49`); no warnings were reported in final checks. Did not repeat successful test suites, regenerate fixtures, inspect a broad git diff, or mutate product source. Cross-target compilation was not treated as runtime proof.

# Assessment

- **Task quality: Approved**, with the minor documentation suggestion above.
- The production changes preserve publication uncertainty and ownership through the real asynchronous path while keeping v1 persistence and existing user-effect classifications intact. This is approval of the bounded source migration, subject to the parent's coordinated integration check; it is not native qualification or broader framework closure.
