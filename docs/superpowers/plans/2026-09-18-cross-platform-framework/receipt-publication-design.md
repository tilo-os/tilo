# Task 8 implementation brief: held journal admission and receipt publication

Status: read-only source-derived design, 2026-09-18. No product code changed, no builds or tests run, and no native qualification claimed. The parent must reconcile the accepted Task 4 probe changes and resolve the Linux profile decision below before issuing the implementation brief.

## Contract and smallest change

Task 8 migrates the Unix `OperationOwner` journal writer to `atomic-file`; it does not port grants, transport, user-file mutations, or persisted identities to Windows. Preserve receipt version 1, postcard field order/types, owner strings, UUID naming, locator byte names, device/inode fields, bounds, checkpoints, recovery classifications, and `.tilo-operation-writer` flock exclusion. Native handles and the new publication error remain process-local.

Add one semantic method to the existing public type:

```rust
impl StorageDirectory {
    pub fn validate_privacy(&self, privacy: Privacy) -> std::io::Result<()>;
}
```

`Inherit` requests no additional privacy restriction. `OwnerOnly` queries the directory actually retained by the object and verifies current native ownership and effective privacy; it neither changes permissions nor resolves a display path. This is validation, not an immutable proof, a grant registry, a parent-creation API, a durability barrier, or a recursive permission operation. Document that the trusted owner must maintain permissions and the trusted/cooperatively excluded child namespace for the operation lifetime. It does not defeat a hostile process with the same principal, privileged overrides, later owner chmod/ACL changes, or mutation through otherwise-authorized aliases. The caller's authenticated `FsOwner` metadata cannot supply native-principal proof.

Do not change ordinary `StorageDirectory::from_file/open` to require private directories globally. Their current uses include inherited/public storage. Only the receipt owner explicitly asks for this extra admission.

## Actual source risks and required ownership

* `recovery.rs:OperationOwner::open` currently checks only `is_dir`, then creates/opens the sentinel, locks, enumerates and reads receipts. `StorageDirectory::from_file` currently checks directory kind (and Linux non-`O_PATH`), not privacy. Thus neither call presently supplies the trusted child namespace that shared publication requires.
* `recovery.rs:write` reuses `<id>.pending` with CREATE/TRUNC/NOFOLLOW, writes, syncs, renames, and syncs the directory. It discards every native error as `StoreUnavailable`. Replacing this complete sequence is the migration; do not retain an alternate receipt writer.
* `update` clones the old receipt and only inserts on successful write; all write failures poison. `admit` currently writes before insertion but never poisons on failure. The latter permits further admission after an unknown receipt was left on disk.
* `operations.rs:Context::recovery` currently uses `saved.err().unwrap_or(error)`, so a follow-up poisoned-store error overwrites the original typed failure. `execute` discards item/finish errors into `Results.error: bool`; `poll` then returns only `StoreUnavailable`. A new writer error variant alone would therefore erase the phase along the actual asynchronous caller path.
* `recovery.rs:error_code` has a Debug fallback. Without an explicit arm, a new publication variant would change bounded persisted report codes and could serialize verbose native error/handle diagnostics as the code.
* The Unix private-file validator combines owner, mode, **single-link**, and macOS ACL checks. Copying it directly would reject normal directories (including directories with subdirectories).
* Windows already has a separate `acl::validate_private_security` helper, currently shared by private-file validation and the Task 4 diagnostic directory probe. This is a real overlap with the concurrent probe owner, not a reason to introduce another ACL implementation.
* A fresh `rg` over `crates`, `shell`, `fs`, and `apps` found only test `OperationOwner::open` calls. There is no observed production admission layer whose assurance can be assumed here.

Owned implementation files should explicitly expand Task 8's original list:

| File | Change |
| --- | --- |
| `crates/system/atomic-file/src/directory.rs` | Add/document privacy validation method |
| `crates/system/atomic-file/src/native/unix.rs` | Held directory privacy admission; factor shared principal/mode/ACL checks without changing file hardlink rule |
| `crates/system/atomic-file/src/native/windows.rs` | Compose directory admission with current-principal security validation |
| `crates/system/atomic-file/src/native/windows/acl.rs` | Only if accepted probe diff has not already supplied the security-only helper; otherwise reuse unchanged |
| `crates/system/atomic-file/src/publication.rs` | Narrow `Privacy` documentation update to include held directories, if needed |
| `crates/system/atomic-file/tests/native_publication.rs` and existing native Windows unit-test file | Native directory privacy tests |
| `crates/runtime/runtime-fs/Cargo.toml` | Normal atomic-file dependency and default-off test-support dev dependency |
| `crates/runtime/runtime-fs/src/recovery.rs` | Held storage field, admission order, shared writes, initial poisoning, explicit report code mapping and v1 tests |
| `crates/runtime/runtime-fs/src/grants.rs` | Typed publication variant, Display/source, optional phase accessor |
| `crates/runtime/runtime-fs/src/operations.rs` | Small production first-error retention changes below, plus worker-local native fault coverage |
| `crates/runtime/runtime-fs/tests/native_operations.rs` | Admission/no-effects and renamed-directory public-path tests |

Do not edit new `directory_probe` files or the diagnostic example. Do not add a dependency on them. Re-read accepted `windows/acl.rs` before editing; its security-only helper must retain the existing regular-file kind/link gate in `validate_private`.

## Native directory privacy policy

### Unix common structure

From the retained directory handle, verify directory kind and `metadata.uid() == geteuid()`. For `OwnerOnly`, require `metadata.mode() & 0o077 == 0`, deliberately stronger than merely denying other-principal write. Do not require exact `0700`: absence of owner permissions may cause later ordinary I/O failure but is not disclosure. Do not chmod to obtain the desired result. Do not impose any `nlink == 1`, `nlink == 2`, or fixed directory link-count condition.

Factor owner/mode plus host ACL policy from the existing file validator. Keep the file-specific `nlink == 1` check in private-file validation. Preserve legacy explicit-mode behavior. Query failures are errors, never empty-ACL assumptions. Rejecting privacy maps to PermissionDenied; unsupported/unproven native permission profiles map to Unsupported; failed native metadata/ACL queries retain their original I/O source.

### macOS

Reuse `macos_acl::validate` on the held directory FD. The existing implementation examines extended ACLs, resolves allow-entry UUIDs to the effective native UID, admits ordinary deny entries, and rejects other-principal or unrecognized allows. Preserve acceptance of effective-owner allows and benign denies; do not substitute “any ACL is forbidden.” Apply its conservative allow policy to inheritance-only/inherited entries as well, so a private parent cannot silently give a new receipt access to another principal. No ordinary directory link gate is added.

Required cases: private ordinary directory containing a subdirectory accepted; effective-owner allow accepted; deny-only benign ACL accepted; another user/group/everyone allow rejected even with mode 0700; malformed/unreadable/unsupported ACL evidence fails closed. The file hardlink rejection remains covered separately. The parent may request finer ACL semantics later; do not broaden this task to an ACL evaluator rewrite.

### Linux: mechanism and explicit parent decision

Linux POSIX access ACLs are bounded by their mask: group mode bits represent that mask when present, and named users/groups cannot gain permissions beyond it. Consequently owner equality plus zero group/other bits proves the required DAC restriction **for this permission model**, including masked named entries. Default ACLs influence newly created children rather than the directory's current access, and requested child creation mode bounds inherited effective permissions. The engine still validates each OwnerOnly receipt before writing bytes. This reasoning does not establish arbitrary NFSv4/server/FUSE permission behavior. See [Linux ACL contract](https://man7.org/linux/man-pages/man5/acl.5.html).

Proposed implementation choice for the parent to approve before dispatch: use held `fstatfs` to restrict this new privacy admission to documented local POSIX-DAC profiles, initially ext2/3/4 (their shared filesystem type), Btrfs, XFS, tmpfs, and OverlayFS. Unknown types return Unsupported before sentinel effects. This is a **privacy admission restriction only**, not a claim of power-loss durability for tmpfs or any filesystem. Mode/owner/ACL checks remain mandatory after profile selection. OverlayFS is relevant to Fedora Bootc and performs a local DAC/POSIX-ACL check in addition to the backing access check; qualification must state the host-owned backing namespace precondition rather than treating its magic value as security proof. See [kernel OverlayFS permission model](https://cdn.kernel.org/doc/html/latest/filesystems/overlayfs.html).

This proposed list is provisional, not an already-approved framework restriction. No production journal location has been found. Do not silently redirect a journal or make collaborative FUSE storage the journal: a FUSE resource grant is not evidence of a private durable host metadata namespace. If required journals are intended to live on the collaborative mount or another unlisted filesystem, the parent must select and implement native permission evidence for that concrete profile, or explicitly constrain host journal admission before dispatch. An unrestricted Linux mode-only implementation with a universal privacy claim is not the alternative. Other Unix ACL models likewise need their own proof or an explicit Unsupported result from this new method; do not accidentally equate `cfg(unix)` with Linux/macOS policy.

### Windows composition, independently of runtime-fs

Implement the public method honestly on Windows, while leaving runtime-fs's grant modules Unix-gated. `Directory::from_file` already rejects non-directory/reparse objects and invokes volume admission. For OwnerOnly, obtain `acl::Principal::current()` internally, then invoke `acl::validate_private_security(&self.file, &principal)`. That helper checks native owner, valid non-null DACL, protected inheritance policy, owner-only ordinary allows, benign denies, and unsupported ACE rejection. Do not call `validate_private`, whose non-directory/single-link condition is intentionally file-only. Do not accept a caller-supplied SID or expose the handle. A handle missing READ_CONTROL must fail on the native query, not succeed because directory enumeration was permitted.

Native Windows tests prove protected owner-only directory success, benign deny/owner allow acceptance, other-principal/unprotected/null-DACL rejection, regular-file-as-directory/reparse rejection, and failure on unavailable security evidence. Cross compilation alone cannot establish those results. This API does not solve Task 4's directory-creation commit protocol or claim a Windows runtime-fs provider.

## `OperationOwner::open` ordering and held authority

1. Keep ownership of the supplied `std::fs::File`; duplicate **that directory** with `try_clone`, construct `StorageDirectory::from_file`, and validate OwnerOnly privacy before `Dir::from_std_file`, sentinel open, flock, enumeration, or receipt reads. All operations are on these held handles. Admission failure creates no sentinel, temporary file, receipt, permission change, or reconciliation write.
2. Preserve the existing original-handle `cap_std::fs::Dir` for enumeration and lock-relative operations. Store the validated `StorageDirectory` for publication. The duplicated handle is the same native object, including after ambient renames. Never duplicate `_writer`, a future FileLease, or any lock-bearing handle.
3. Open `.tilo-operation-writer` using the existing no-follow flags and retain the existing nonblocking flock lifetime. Keep `Busy` for contention. Directory privacy admission and cooperative writer exclusion are separate requirements.
4. Continue bounded receipt loading and owner/version/name checks, then reconciliation. A failure while opening/reconciling returns no usable owner. Successfully published but uncommitted metadata must remain on disk for subsequent recovery; dropping its typed error cannot delete it.

Receipt reads can remain the existing held-directory/no-follow path in this bounded migration. The task does not need to rewrite all read policy or weaken/add a receipt schema requirement. If review finds a concrete reason to require `open_read(OwnerOnly)` for legacy receipts, make that an explicit security/compatibility change with fixtures; do not silently fail existing v1 files merely as incidental cleanup.

`write` continues to serialize/bound first, then calls:

```rust
self.storage.publish(
    std::ffi::OsStr::new(&format!("{}.receipt", receipt.id)),
    &bytes,
    atomic_file::PublishMode::Replace,
    atomic_file::Privacy::OwnerOnly,
).map_err(FsError::Publication)
```

The shared engine owns unique exclusive staging and cleanup. Do not retain `.pending` cleanup logic, sweep old pending files, reopen journal paths, or “roll back” receipt targets on an error. Old `.pending` entries remain ignored as today; final receipt names and bytes are unchanged.

## Phase mapping and poisoning

Add `FsError::Publication(atomic_file::PublicationError)` with Display and `Error::source` returning the original error. A narrow `FsError::publication_state() -> Option<PublicationState>` is useful but not necessary if tests/callers directly match the variant. Do not send it through `native::io_error`, whose kind mapping loses phase. Do not serialize this new enum variant. Map it explicitly to the existing `"StoreUnavailable"` persisted error code.

| Position/result | Memory and callable result | Disk/effect policy |
| --- | --- | --- |
| Privacy/constructor failure before journal effects | No owner; map privacy denial to Denied, other native failures through existing deliberate I/O classification | No sentinel/read/reconciliation/write; do not chmod |
| Bounds/serialization/admission reservation failure before `publish` | Existing LimitExceeded/StoreUnavailable; no new poisoning just because a preflight rejected | No receipt publication or user-file effect |
| Initial `admit`, NotPublished | Return typed error; no receipt insertion; keep store usable | No user-file effect; proven prepublication retry remains possible |
| Initial `admit`, PublicationUnknown or PublishedDurabilityUnknown | Return typed error; poison; no receipt insertion/job submission | Do not delete or count unknown on-disk receipt as absent; later admission blocked until owner is closed/reopened |
| Existing `update`, any publication error | Return typed error; poison; retain prior receipt/checkpoints in memory | Do not advance bookkeeping; preserve any user-file effects and recovery evidence already present |
| Reopen reconciliation publication error | Return typed error; no usable owner escapes | No compensating user-file operation or journal deletion |
| Later calls after poisoning | Existing StoreUnavailable | No subsequent publication or user-file operation starts through this owner |

`BeforeDataFlush` / `BeforePublish` faults produce NotPublished. `PublishSubmitted` and `AfterPublish` faults produce PublicationUnknown. `BeforeCommit` produces PublishedDurabilityUnknown. This describes **receipt publication**, not the user's native operation outcome. A NotPublished receipt update can follow a successful user-file rename. Never translate that into “the user operation had no effect.” Keep the existing operation-path decisions for KnownStaged, UnknownNativeCommit/OutcomeUnknown, PublishedSourceRetained/CopiedSourceRetained and NeedsRevalidation. No automatic `retry_commit`, automatic owner unpoisoning, compensating delete, or resubmission is added.

## Minimal asynchronous first-error retention

The parent explicitly expanded design scope to production changes in `operations.rs`, because phase preservation only inside `write` is insufficient.

1. In `Context::recovery`, prefer the original error if it is Publication and the follow-up save failed. Otherwise prefer a follow-up Publication error over a nonpublication original error; retain current behavior for other combinations. Thus a secondary poisoned-store failure cannot erase the first real publication phase, but a real receipt failure remains visible after an earlier native user-operation failure.
2. Keep sticky `Results.error: bool` for current terminal-failure semantics and add `publication_error: Option<PublicationError>` (not Clone, no duplicated guard). At failure of `owner.item`, move a Publication error from the failed outcome into this slot first; otherwise move the item-write Publication error. At `finish` failure, move its Publication error if the slot is empty. Never overwrite an earlier publication error with a later poison error. These paths stop the worker exactly as today.
3. `poll` continues draining already-queued updates first. When terminal failure is pending and the queue is empty, take and return `FsError::Publication` once if retained; subsequent polls still return StoreUnavailable via the sticky flag. Update its public error documentation to say precisely this. This is the smallest honest callable phase promise; storing a phase that no caller can retrieve is insufficient. Existing public report codes remain StoreUnavailable and stored operation/recovery classifications remain unchanged.
4. Do not attempt to clone/reconstruct PublicationError or its CommitGuard. Dropping job/errors has no deleting side effect. Any caller using `retry_commit` must still retain the existing owner/writer exclusion, and successful retry alone does not unpoison/resume the owner.

`native.rs:repeat_error` currently has a fallback that would collapse Publication, but it is used for retained grant read-handle opening errors, not this receipt worker path. Do not widen that helper or introduce cloned publication authority speculatively. Test the actual start/update/poll path instead.

## Required tests and evidence boundaries

Use the real atomic-file fault hooks, never only `Hooks.fail_checkpoint`, for new publication-phase assertions. Existing synthetic checkpoint tests remain useful and unchanged. Atomic-file hooks are thread-local: an outer `with_failure` around `session.start` reaches initial admission, **not** the worker. Add a cfg(test)-only hook selecting a receipt checkpoint and atomic checkpoint; wrap the actual `self.write` on the worker's thread at that selected checkpoint. It must invoke shared publication and observe the requested native checkpoint, not return a fake FsError. Hooks must remain default-off production-wise and scoped/unwind-safe.

* Native directory admission cases above, including ordinary directory link counts and retained-handle identity after path rename. Existing private-file hardlink/ACL tests must still hold.
* Externally writable journal (e.g. mode 0770/0777), and macOS other-principal ACL with mode 0700: `OperationOwner::open` rejects before sentinel creation. Assert exact directory entries before/after and untouched preexisting receipt bytes/mtime or other direct no-reconciliation evidence. A FIFO named `*.receipt` can be a bounded rejection-order test only if the test cannot hang; do not rely on timeout as a success assertion.
* Native wrong-owner directory admission needs actual ownership evidence from a native fixture/harness that can construct it; merely using different FsOwner strings is not this test. Missing cross-principal capability is outstanding native evidence, not a passing skipped assertion. Separately preserve existing string-owner mismatch tests.
* Rename the opened journal directory and create a replacement at its old path, then open/publish through the retained input and through an already-created OperationOwner. New sentinel/receipts must appear only in the original object. Preserve second-writer Busy on that original object and independent admission of the replacement where appropriate.
* Initial admission matrix: BeforeDataFlush, PublishSubmitted, AfterPublish, BeforeCommit. Assert exact typed phase, no started job/user destination/submission, no in-memory receipt, initial NotPublished permits a later fault-free admission, uncertain phases poison and block later admission. Inspect retained bytes: absent target before publication, valid v1 target after AfterPublish/BeforeCommit; PublishSubmitted may leave staging but no submitted rename in this injection. Do not sweep leftovers to satisfy assertions.
* Update matrix after real user-file effects, targeting `Staged`, `Published`, `Item`, and `Finished` as appropriate: all write failures poison; failed checkpoint is not inserted in memory; copied/moved/restored bytes retain the actual completed effect; report/reopen recovery classifications follow existing v1 rules. Include BeforeDataFlush **after** a native user-file effect so receipt NotPublished cannot be mistaken for no effect.
* First-error propagation: `Context::recovery` original Publication survives its poisoned follow-up checkpoint; failed `item` preserves that original phase; terminal `item` and `finish` own faults reach the first error-returning `poll`; already-queued successful updates drain first; no Finished follows terminal receipt failure; subsequent poll stays StoreUnavailable. Assert source downcast and original phase, not string matching alone.
* Fixed v1 byte fixtures should include deterministic receipt UUID/mount, owner strings, raw non-UTF8 locator name, legacy native device/inode, steps/checkpoints, and existing enum variants. Decode known prechange bytes and assert exact re-encoding; run a production writer fixture against an independently fixed expected payload. Do not generate the expected bytes with the very serializer under test. Reopen admitted/interrupted and terminal receipts without inventing a v2 migration.
* Bounded metadata, report quotas, unchanged owner isolation, writer contention, existing source-retained/unknown recovery tests remain required. Error code for Publication is exactly `StoreUnavailable`, not a Debug rendering or a new wire vocabulary.

Planned commands after implementation (not executed for this brief):

```text
cargo test -p atomic-file --test native_publication --features test-support --locked --offline
cargo test -p runtime-fs --lib --locked --offline checkpoint
cargo test -p runtime-fs --lib --locked --offline receipt_publication
cargo test -p runtime-fs --lib --locked --offline receipt_v1
cargo test -p runtime-fs --test native_operations --locked --offline
cargo check -p atomic-file -p runtime-fs --tests --locked --offline
```

Give new regression names these filters or report exact replacement filters; a filter matching zero tests is not evidence. Run native macOS/Linux and Windows atomic-file admission cases for each support claim; Windows cross-check is compile-only. Parent owns the repository-required shell check/handoff coordination. No shell build, cross-target/native success, crash-durability result, or completed Task 8 is claimed here.

## Controller decision on Linux privacy admission

For this bounded implementation, admit the proposed ext2/3/4, Btrfs, XFS, tmpfs and OverlayFS permission profiles through held `fstatfs`, plus owner and effective mode checks. This selects a supported permission mechanism; it does not independently prove durability, absence of external backing aliases, or broad filesystem support. Document the trusted host/backing namespace premise. Unknown profiles fail before journal effects with explicit Unsupported; never silently relocate storage. Required host composition must supply a supported private journal root, and remains incomplete until real production binding/native tests prove it. FUSE journal admission remains an explicit later capability obligation if that host composition requires it; this decision does not restrict granted user-file operations on the collaborative mount.

Basis verified against [Linux POSIX ACL semantics](https://man7.org/linux/man-pages/man5/acl.5.html), [OverlayFS permission checks](https://docs.kernel.org/filesystems/overlayfs.html#permission-model) and [FUSE permission options](https://docs.kernel.org/filesystems/fuse/fuse.html#mount-options). The concrete reason for profile admission is that FUSE need not enforce mode permissions by default. Cost if wrong: extend admission with explicit evidence for an additional required profile; do not weaken namespace privacy or declare that required storage unsupported as final closure.
