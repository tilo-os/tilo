# S01 durable/private storage: bounded design reconnaissance

2026-09-18. Read-only source and installed dependency inspection plus targeted Microsoft API documentation checks. No product edits, builds, git changes, native qualification, or lease implementation. The separately assigned `FileLease` slice is excluded.

## Follow-up decision

The [Windows publication strategy](windows-publication-strategy.md) supersedes the unresolved-strategy recommendation below. It identifies a concrete local-NTFS write-through, handle-relative publication and retained-handle flush implementation candidate, while retaining native crash qualification as outstanding. The original reconnaissance is preserved for its caller and compatibility findings.

## Recommendation and decisive gap

Next implementable slice: **phase-aware atomic publication with native private staging and handle identity**, owned by `atomic-file`, with the existing Unix sequence preserved and actual Windows private-create/identity implementations. Do not call that slice complete Windows durable storage: namespace persistence on Windows remains a separate mandatory implementation/qualification gate. This is a useful slice that removes current API ambiguity and privacy loss; returning Unsupported forever would not satisfy the required framework capability.

The central unresolved design choice is the Windows namespace commit strategy. A handle-relative rename gives a clean authority model, but the reviewed APIs do not expose a documented durability flag. `MoveFileExW(WRITE_THROUGH)` has more relevant durability wording but is path-based, and its explicit flush guarantee is phrased around copy/delete. A privileged volume flush is documented but is not an acceptable implicit privilege requirement for ordinary app persistence. Do not manufacture a Windows parent-directory fsync guarantee by using `File::open(dir).sync_all()` or `FlushFileBuffers(directory)`.

Before enabling `Ok(())` from the Windows durable-publication operation, choose and substantiate a native commit strategy for the supported filesystem/OS profile, implement it, and qualify crash behavior. This report establishes neither impossibility nor certification.

## Existing seams and concrete scope

- `crates/system/atomic-file/src/lib.rs`: `write_inner` currently creates a sibling with `create_new`, writes, flushes, closes, renames, then opens and flushes the parent. The final two calls can fail after publication, but the result is undifferentiated `io::Result<()>`. `write_with_mode` explicitly ignores mode off Unix. Unix mode is set at creation and restored through the file handle before writing. Keep these Unix guarantees.
- `atomic-file/Cargo.toml`: currently std-only, deliberately lightweight. The framework contract permits focused OS libraries; adding a Windows-only bindings dependency is justified, unlike pulling renderer/runtime policy into the leaf.
- `crates/system/app-state/src/lib.rs:1169`: `same_file` compares device/inode on Unix and always returns false elsewhere. Canonical opening also uses ordinary `File::open(directory)` and `sync_directory_chain` iterates Unix-style directory flushes. Fixing leases alone will not make canonical stores work on Windows. `claim_room_marker` stages/flushed bytes and publishes via `hard_link`, then flushes the parent: it needs true publish-if-absent semantics and phase-aware errors.
- `crates/runtime/runtime-fs/src/native.rs`: `NativeIdentity` is a serialized `{ device: u64, inode: u64 }`; `NativeLocator` persists byte-valued components. `NativeGrantAuthority`/`FsSession`, rights, owner IDs, generations, revocation and owned objects are the seam to retain.
- `runtime-fs/src/operations.rs:629`: `sync_dir`, `no_replace`, no-follow relative opens and identity checks belong under private native implementation. Preserve `renameat_with(NOREPLACE)`, not check-then-rename.
- `runtime-fs/src/recovery.rs:267`: receipt write uses an authorized `Dir`, no-follow sibling open, file flush, relative rename and directory flush. Do not migrate this to the existing ambient atomic-file path API. Existing `UnknownNativeCommit`, `PublishedSourceRetained`, `OutcomeUnknown`, poisoning and reconciliation are useful domain states; integrate publication phase there rather than invent another operation bus.
- `runtime-identity/src/lib.rs:266`: its private writer currently ignores parent-open/flush errors. A later caller migration must propagate a post-publication failure without changing key protection/backend IDs. Do not conflate native file identity with cryptographic principal identity.

These address A09-01, A09-02, and the storage portions of A08-03/A08-06. A08-04 service transport is independent and should remain out of this slice.

## Proposed narrow interfaces and files

Keep existing public compatibility functions. Add `atomic-file/src/publication.rs`, `native/mod.rs`, `native/unix.rs`, and `native/windows.rs`; adjust names to the repository's first lease slice rather than creating competing native modules.

Proposed semantic types:

```rust
pub enum Privacy { Inherit, OwnerOnly }
pub enum PublicationState {
    NotPublished,
    PublishedDurabilityUnknown,
    PublicationUnknown,
}
pub struct PublicationError { /* private state + io::Error */ }
// state(), source(), into_io_error(); no caller-supplied proof fields.
pub fn replace(path: &Path, bytes: &[u8], privacy: Privacy)
    -> Result<(), PublicationError>;
pub fn publish_if_absent(path: &Path, bytes: &[u8], privacy: Privacy)
    -> Result<(), PublicationError>;
```

`Ok` means the whole required durability contract completed, not merely visible bytes. `AlreadyExists` is a conflict with `NotPublished` for a proven no-replace failure. `PublicationUnknown` is necessary if a native API can mutate names on a failed call; do not label every rename error “before publication.” Preserve the original native `io::Error` as source and sanitized diagnostic context.

Compatibility `write* -> io::Result<()>` can wrap the typed error without losing it to a string; expose a documented accessor/downcast helper during transition. Native-aware callers must move to the typed result in the same coherent caller migration. Keep Unix mode APIs for legacy non-private modes; replace portable private calls with `Privacy::OwnerOnly`. On Windows, translate only explicitly defined private mode compatibility cases, and reject other unsupported Unix modes before mutation rather than silently pretending they worked.

Owned `FileIdentity` should expose equality and identity acquisition from an already opened file, with a private native representation. Use a separate `VolumeIdentity` when needed for accounting/cleanup. A path comparison helper must reopen and compare handles according to the caller's required no-follow policy; a failure is an error, not a fabricated identity. Do not serialize this new in-memory enum directly over the old receipt structs.

The next runtime-fs slice should add a private handle-relative storage primitive used by the existing `NativeGrantAuthority` and receipt owner. It should accept owned/borrowed opened directories and validated single components, not recover ambient paths. Share the low-level code with atomic-file only where that preserves authority; the old ambient helper is not a grant API. Keep ordinary caller-facing paths limited to trusted-host admission.

## Windows implementation constraints

**Private creation.** Construct an explicit security descriptor for the host-selected principal and pass it in `SECURITY_ATTRIBUTES` to exclusive creation, with handle inheritance false. Owner-only means a protected DACL allowing the selected user, with no inherited broad allow ACEs; any SYSTEM/administrator access must be an explicit documented host policy, not silently equated to arbitrary other users. Obtain the SID from a trusted native token, not a username, environment string, or app parameter. Handle impersonation deliberately: bind to the host's intended principal rather than whichever token happens to be active.

Validate the created object's owner/DACL through the opened handle before writing secret bytes. Creation of a private parent must also be private at creation. Existing objects require validation: supplying a security descriptor while opening them does not reset their ACL. Reject reparse points for private-object traversal, retain no-follow ancestry, reject disallowed hardlinks, and deny privacy claims on filesystems that cannot enforce the policy. These are proposed policy choices; implement and test exact ACL semantics rather than relying on read-only attributes. [CreateFileW documentation](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew).

**Identity.** Use `GetFileInformationByHandleEx(FileIdInfo)` on held handles, retaining `VolumeSerialNumber` plus the full 128-bit ID. Do not truncate to the existing u64 inode or use canonical path, size, timestamp or display name. IDs are local observations, not permanent globally unique resource IDs; hold handles for live authority and continue receipt digest/fresh-authority revalidation after restart. [FILE_ID_INFO documentation](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_info).

**Visibility and no-replace publication.** The current `FILE_RENAME_INFO` documentation allows `RootDirectory` with a relative name. `SetFileInformationByHandle` can rename the held staged file into an opened parent; false `ReplaceIfExists` reports conflict without check-then-act. The structure's variable-size, UTF-16 byte length and alignment need checked allocation, and components must exclude alternate stream/path syntax. This is the preferred authority-preserving candidate, subject to production tests and filesystem support. Handle/share flags must permit the rename without unintentionally widening write access. [FILE_RENAME_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_rename_info), [native rename information](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information).

**Durability.** Stage bytes and flush the writable file before publication. That does not establish persistence of the new name. `FlushFileBuffers` documents `GENERIC_WRITE` and an administrative volume-flush mechanism, but does not specify a portable directory-entry commit contract. Opening a directory requires Windows-specific handling; adding `BACKUP_SEMANTICS` enables opening, not durability. [FlushFileBuffers](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers).

`MoveFileExW` with `MOVEFILE_WRITE_THROUGH` is a candidate to investigate for ordinary trusted private storage; never include `MOVEFILE_COPY_ALLOWED` for atomic publication. Microsoft says it waits for the move on disk, then specifically guarantees the flush for copy/delete moves. This report does not interpret that as sufficient evidence of atomic, same-volume, power-loss-safe replacement on all supported filesystems. It also requires resolving the path-versus-held-directory authority issue. [MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw).

Do not select `ReplaceFileW` casually: its write-through flag is unsupported; it preserves the destination DACL; some error cases leave changed names or metadata. That risks widening a newly private replacement and invalidates a universal “failure leaves old target intact” claim. [ReplaceFileW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew).

## Verified dependencies and installed sources

- Toolchain `1.97.1` with installed rust-src. Its Windows `std::fs::rename` calls `MoveFileExW(REPLACE_EXISTING)` without WRITE_THROUGH, with a `FileRenameInfoEx` fallback. `sync_all` delegates to FlushFileBuffers. Stable source still marks `MetadataExt::{volume_serial_number,file_index,number_of_links}` unstable (`windows_by_handle`), so std-only native identity is not a viable implementation here.
- `runtime-fs` directly specifies `cap-std = 3.4.6` and `rustix = 1.1.4` with `fs`. Lock resolves both exactly; `cap-primitives` is 3.4.6.
- Installed cap-primitives Windows `rename_unchecked.rs` obtains full paths with `concatenate` and calls `fs::rename`. It is **not evidence of a handle-relative rename guarantee**. Its Windows open implementation does include relative handle opening; audit each used operation separately instead of trusting the crate name.
- `windows-sys` 0.61.2 is already locked and installed, as are older 0.45.0/0.48.0/0.52.0/0.59.0/0.60.2 lock versions. 0.61.2 bindings include CreateFileW, SetFileInformationByHandle, FILE_RENAME_INFO, GetFileInformationByHandleEx, FILE_ID_INFO, GetTokenInformation, GetSecurityInfo, ACL/SID helpers, and NtCreateFile/NtSetInformationFile if a relative-create native wrapper is required.
- Recommend a target-specific `windows-sys = "=0.61.2"` dependency in the implementation owner. Required Win32 features are Foundation, Storage_FileSystem, Security, Security_Authorization and System_Threading; add System_Memory only for APIs chosen to free allocated descriptors, and Wdk_Foundation/Wdk_Storage_FileSystem/System_IO only if using NT creation. Features were found in the installed manifest; do not enable WDK broadly by default.
- Adding the direct dependency still changes the owner's dependency edge in Cargo.lock even though the version is already present. Review that small diff; never regenerate the lock. No dependency/manifests were modified in this reconnaissance.

## Meaningful tests for the slice

Use fault hooks at production transition points, not tests of a duplicate model.

1. Fail creation, write, private validation and staged-file flush; assert old target bytes/identity survive, no final publication occurred, state is `NotPublished`, and owned staging cleanup is attempted.
2. Perform the actual rename, then inject commit-flush failure; assert target contains complete new bytes, error is `PublishedDurabilityUnknown`, old target is not claimed preserved, and cleanup never deletes the published file. Retry under the service's existing lease and confirm actual persisted state is revalidated.
3. Inject an ambiguous native publication failure; assert `PublicationUnknown`, no destructive rollback, and service reconciliation does not advance durable CRDT/migration bookkeeping.
4. Concurrent `publish_if_absent` from separate processes: exactly one winner, all others conflict, complete winning bytes, no overwrite. Couple app-state migration winner with canonical owner validation.
5. Pause after private staging creation and before data writing; inspect real Unix mode or Windows DACL/owner, then attempt access using a distinct native principal. Checking only final metadata misses the exposure window. A qualification job unable to create/use that principal must fail, not skip or claim privacy proven.
6. Hold a file/directory handle, rename or replace its path, and confirm same-handle/hardlink identity equality but replacement inequality. Test reparse-point and hardlink attacks, parent swaps, link-count checks and rejected cross-volume cleanup.
7. Test existing broad target ACL replacement retains the requested new privacy policy; test inherited broad parent ACL, null DACL, wrong owner and unavailable ACL support. Inject ACL validation failure before secret writes.
8. Runtime-fs receipt v1 fixtures must remain byte-compatible/readable. A future Windows encoding/identity receipt format needs explicit v2 decode/migration tests; inserting an enum into postcard v1 structs silently breaks the contract. Windows names need lossless UTF-16 representation plus rejection of separators, NUL, dot-parent, alternate streams and namespace aliases. Do not change legacy Unix name bytes.
9. Process-kill/restart tests verify cancellation/recovery and lease release, **not power loss**. Qualifying power-loss durability requires a native crash/VM storage harness with controlled interruption around file flush/publication/commit; assert old or new complete data and the promised acknowledged state. Successful injected tests and cross-compilation alone cannot certify it.

Preserve the existing Unix file flush -> atomic publication -> parent flush sequence. Missing-parent helpers need ancestor persistence obligations when they create directories; app-state already tests retry of these obligations even once ancestors exist. Cleanup errors must not erase publication state. Keep macOS qualification distinct: preserving current fsync behavior is not new evidence about device-cache power-loss guarantees.

Completion of this next slice is the typed production contract, preserved Unix behavior, and tested native Windows private-create/identity primitives. Completion of S01 additionally requires the selected Windows durable-publication implementation, native conformance, caller migrations, receipt compatibility, and integration checks including `cargo check -p shell --tests --locked`.
