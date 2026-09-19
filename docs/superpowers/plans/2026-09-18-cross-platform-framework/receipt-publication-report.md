# Task 8 — receipt publication and held journal admission

Status: assigned source implementation complete, available scoped verification passed; independent review and parent-coordinated shell check remain with controller. Native Windows/Linux runtime, cross-principal, crash and production-host qualification are outstanding. This is not Windows grants/transport/first-launch or overall framework closure.

## Source ownership (against task-8-before, not HEAD)

Exactly 12 baseline files changed:

- `crates/system/atomic-file/src/directory.rs`: public held-handle `validate_privacy` and its continuing trust/permission-profile premises.
- `crates/system/atomic-file/src/native/unix.rs`: directory kind, Linux held `fstatfs` profile admission, shared owner/mode/macOS ACL checks. Private regular files retain the single-link gate; directories have no link-count restriction.
- `crates/system/atomic-file/src/native/windows.rs`: current native principal plus existing security-only ACL validator for held directories.
- `crates/system/atomic-file/src/native/windows/tests.rs`: directory ACL, missing security access, unprotected DACL, regular-file/reparse admission tests.
- `crates/system/atomic-file/src/publication.rs`: Privacy documentation includes explicit held-directory validation.
- `crates/system/atomic-file/tests/native_publication.rs`: held identity/link-policy/privacy tests, native macOS benign and malicious ACL cases, Linux procfs unsupported-profile test.
- `crates/runtime/runtime-fs/Cargo.toml`: normal atomic-file path dependency; test-support only on the dev dependency.
- `crates/runtime/runtime-fs/src/grants.rs`: owned Publication error, Display/source and retry ownership limits.
- `crates/runtime/runtime-fs/src/recovery.rs`: pre-effect privacy admission, shared writer, initial phase poisoning, explicit unchanged StoreUnavailable report code, worker-local scoped fault seam, fixed v1/admission/reopen/ownership tests.
- `crates/runtime/runtime-fs/src/operations.rs`: first-error retention through recovery and terminal item/finish failures; once-only typed polling after queue drain; real worker-thread publication fault tests.
- `crates/runtime/runtime-fs/tests/native_operations.rs`: no-effects rejection and retained-directory/writer-exclusion public-path regressions; private journal fixture provisioning.
- `Cargo.lock`: exactly one added `"atomic-file"` dependency under runtime-fs. No resolution or version changes.

`task-8-source-sha256.json` records the stable source hashes. The accepted Windows `acl.rs`, diagnostic directory-probe files, atomic-file manifest, FileLease, receipt structs/enums and native identity representation are unchanged against the snapshot. No subagents, staging, commits, stash, cargo fmt, CARGO_TARGET_DIR, lock regeneration, broad shell/UI tests, or unrelated product edits were performed. Direct rustfmt used only the ten explicitly owned Rust files with edition 2024 and skip_children=true.

## Production contract

The original input directory is cloned once for StorageDirectory admission; the original handle becomes cap-std Dir for enumeration/sentinel operations. Neither the writer file nor a lease is cloned. OwnerOnly validation precedes sentinel open, flock, enumeration and receipt loading/reconciliation. It never reopens a display path, chmods a namespace into compliance, or treats FsOwner strings as principal proof. The existing `.tilo-operation-writer` namespace, nonblocking exclusive flock and owner lifetime remain intact.

The writer still serializes and bounds the unchanged Receipt before effects. Its entire custom pending/truncate/write/sync/rename/directory-sync implementation is replaced by shared Replace/OwnerOnly publication. Historical pending files remain ignored and untouched. Successful in-memory receipt/checkpoint insertion happens only after publication succeeds.

Initial NotPublished errors retain a usable empty store. PublicationUnknown and PublishedDurabilityUnknown poison initial admission without insertion/submission. Existing update write failures retain their poisoning policy and previous in-memory receipt. Reopen reconciliation failures return the real typed publication error and no usable owner. Publication is explicitly encoded as existing `StoreUnavailable` in reports; native error/handle diagnostics are never used as a new persisted code.

Context::recovery prefers the original Publication error over a failed follow-up save, and otherwise preserves a follow-up Publication failure over the earlier native error. Terminal item/finish failures move (never clone) the first owned PublicationError into Results. Poll drains all already-queued updates, returns that error once, then returns sticky StoreUnavailable. No Finished is emitted after the terminal journal failure. No automatic commit retry, owner unpoisoning, job replay or user-file compensation was added.

Error custody does not extend the writer lock's lifetime. Tests prove a commit error may survive a dropped job/owner, that its drop deletes no receipt, that a second owner remains Busy while the actual owner lives, and that successful explicit commit retry under retained owner exclusion does not repair the failed memory/checkpoint state or resume the owner. Once the owner is dropped, the error cannot be treated as retaining writer exclusion.

Linux directory privacy is admitted only for ext2/3/4, Btrfs, XFS, tmpfs and OverlayFS with effective-owner and zero group/other mode bits; other profiles fail Unsupported before journal effects. This is permission-model admission, not durability certification. Trusted host/backing namespace and continuing cooperative-owner permissions/name exclusion are documented. Required additional host profiles, including collaborative FUSE journals if needed, remain migration obligations. Granted user-file operations are not restricted or relocated by this policy.

## Red evidence and fixed v1 capture

Before replacing the production receipt writer:

1. `cargo test -p runtime-fs --lib --locked --offline receipt_v1_prechange_capture -- --nocapture`: exit 0, **1 selected/passed**, session89808. A deterministic receipt was written through the old actual writer and its on-disk bytes printed. This captured UUID/mount/owner strings, raw non-UTF8 name `66 80`, real v1 device/inode field representation using fixed values 0x1234/0xabcdef, checkpoint names, enum variants and recovery/report values. The literal 235-byte payload is now frozen in `receipt_v1_fixed_writer_bytes`; SHA256 `8018ff8d95a5063a3dc288dea9bd72c11e160ca18d9910092737da5e97e03bcd`. Expected bytes are never generated by the new serializer in the assertion. Both production-writer output and independent known-byte decode/reencode are checked.
2. `cargo test -p runtime-fs --test native_operations --locked --offline receipt_publication_rejects_public_directory`: exit101, **1 selected/failed** (session10198), expected Denied assertion failed under the original is_dir-only admission.
3. `cargo test -p atomic-file --test native_publication --features test-support --locked --offline held_directory_privacy`: exit101, **API compile red**: missing validate_privacy (E0599). This is explicitly not a native behavioral red run.
4. `cargo test -p runtime-fs --lib --locked --offline receipt_publication_initial_admission_phases`: exit101, **1 selected/failed** (session82371); old writer ignored shared BeforeDataFlush injection and incorrectly admitted the job.
5. `cargo test -p runtime-fs --lib --locked --offline receipt_publication_worker_preserves_first_failure_and_drains_updates`: exit101, **1 selected/failed** (session4276); the real custom writer ignored shared atomic checkpoints and no terminal error reached poll by the bounded deadline. The cfg(test) seam wrapped the actual write on the worker thread before production behavior changed.

After migration, fixtures initially failed Denied because tempfile directories inherited public mode bits. Only journal test provisioning changed to Builder.permissions(0700); existing checkpoint assertions were kept. Production validation was not weakened to accommodate the fixtures.

## Green verification

All commands use the existing pinned toolchain/dependencies. Host: arm64 macOS 27.0 (26A428), Darwin27.0.0. No native Linux or Windows execution is implied.

- `cargo test -p runtime-fs --lib --locked --offline receipt_publication`: final exit0, **9 passed, 0 failed/ignored, 45 filtered**, session30093.
- `cargo test -p runtime-fs --lib --locked --offline receipt_v1`: exit0, **1 passed, 0 failed/ignored, 46 filtered** at that intermediate test inventory; final full-lib run also exercised the finalized fixture and pending-file assertion.
- `cargo test -p runtime-fs --lib --locked --offline checkpoint`: exit0, **2 passed, 0 failed/ignored, 45 filtered** at that intermediate inventory; both existing checkpoint tests also passed in the final full-lib run.
- `cargo test -p runtime-fs --lib --locked --offline`: final exit0, **54 passed, 0 failed/ignored**, session58209. Includes all unchanged bounds/quota, recovery, source-retained/unknown, cancellation, worker teardown and grant tests.
- `cargo test -p runtime-fs --test native_operations --locked --offline`: exit0, **16 passed, 0 failed, 1 pre-existing ignored**, session86727. `real_cross_volume_move_copy_remove_stages` remains the unchanged pre-existing ignored test requiring two disposable volumes; it was not executed and is not counted as qualification. No ignored test was added or changed.
- `cargo test -p atomic-file --test native_publication --features test-support --locked --offline`: final exit0, **19 passed, 0 failed/ignored** (plus successful FIFO/umask child invocations, not double-counted). Includes private-file hardlink, inherited ACL, benign ACL and held-directory admission regressions after shared-helper factoring.
- `cargo check -p atomic-file -p runtime-fs --tests --locked --offline`: exit0, session82203, no warnings.
- `cargo check -p atomic-file --tests --features test-support --target x86_64-pc-windows-msvc --locked --offline`: final exit0, no warnings. **Compile-only**, includes newly added Windows directory validator/tests; runtime-fs remains Unix-gated.
- `cargo check -p atomic-file --tests --features test-support --target aarch64-unknown-linux-gnu --locked --offline`: final exit0, no warnings. **Compile-only**, includes held fstatfs and unsupported-profile regression.
- `cargo tree -p runtime-fs -e normal,build,features --locked --offline`: exit0; explicit assertion verified atomic-file test-support is absent from the production normal/build feature graph.
- `git diff --check -- crates/runtime/runtime-fs crates/system/atomic-file Cargo.lock`: exit0. A separate snapshot-relative check also checked all 12 changed files, including untracked prior-task atomic-file sources, for trailing whitespace.

All build sessions are terminal; no process or source hold remains. Shared build-lock waits were polled instead of restarted. Parent/History own the required coordinated shell --tests check and its final evidence.

## What the new tests observe

- Ordinary directories with subdirectories pass without a file single-link rule; validation continues to use the held object after rename/replacement. Mode0770 and macOS inheritable other-principal ACL journals reject before sentinel/read/reconciliation, preserving preexisting bytes/mtime and exact entry count.
- Initial fault matrix covers BeforeDataFlush, BeforePublish, PublishSubmitted, AfterPublish and BeforeCommit with exact typed phases, empty in-memory store, poisoning policy and valid/absent target receipts. Actual session-start matrix covers four phases and zero submissions/user destinations. Unknown staging is never swept.
- Worker matrix covers Staged, Published, Item and Finished × BeforeDataFlush, PublishSubmitted, AfterPublish and BeforeCommit (**16 cases**), invoking the shared hook on the actual worker thread. It asserts error source downcast, phase, poison, unchanged failed checkpoint memory, retained staged or destination bytes, sticky subsequent polls, no Finished and reopen recovery behavior. A separate two-item barrier test proves a queued successful first item drains before a later original typed error.
- Native rename/move and staged restore regressions prove receipt NotPublished cannot mean the completed user-file operation had no effect. Reopened moved-file recovery is still UnknownNativeCommit/OutcomeUnknown. Copy recovery remains PublishedSourceRetained where appropriate.
- A barrier after SourceRemovalStarted injects the shared publication fault into a follow-up recovery save after the native removal error; this proves the new publication error supersedes that earlier native failure. The regular Staged/Published matrix proves an original Publication survives the poisoned follow-up recovery/item saves.
- Known prechange bytes remain exact v1, including raw name/native identity fields and report code vocabulary. Reopen tests exercise admitted/interrupted and terminal/recovery receipts without a schema migration. Historical `.pending` content remains intact.

## Self-review and qualification limits

Read the exact snapshot-relative production diff for shared native/privacy, admission/writer/poisoning and asynchronous error custody. The original schema and native identity definitions were not edited; source searches confirm no duplicate receipt writer remains. No raw native handle or assumed-principal constructor was exposed, no lock-bearing handle cloned and no public error/commit authority cloned. Existing private-file single-link behavior and accepted Windows security helper are preserved.

Remaining native evidence is explicit:

- No Windows native test execution, cross-principal access run, Windows runtime-fs provider, transport port or first-launch implementation is claimed. Windows ACL/reparse/security-query cases compiled only. Native symlink privilege and the existing distinct-principal runner remain real qualification prerequisites.
- Linux profile support compiled only; native owner/ACL behavior on every selected filesystem (including OverlayFS backing-namespace premise), procfs negative admission and storage interruption remain unrun.
- No fixture changed native directory ownership to a distinct UID/SID. FsOwner string isolation is not native principal evidence. Native wrong-owner admission/cross-principal enforcement remains unqualified.
- The reused macOS ACL query validator fails closed on malformed/unreadable/unsupported evidence in source; this task executed benign-owner/deny and malicious group/everyone inheritance cases, not a native malformed/unreadable ACL harness. No fabricated evidence or passing skip represents those missing cases.
- No power-loss/crash-durability experiment was run. tmpfs permission admission does not imply durable storage. The pre-existing ignored real two-volume integration test remains unexecuted.
- No production OperationOwner admission/provisioning call site was added or assumed. A real host composition must provision and bind the supported private journal root; new required permission profiles cannot be silently relocated or declared final unsupported closure.
