# S01 Windows publication strategy

2026-09-18. Read-only follow-up to `/tmp/tilo-platform-durable-storage-design.md`; no product changes, builds, git changes or native qualification. This supersedes the earlier recommendation to stop at identity/private creation: implement the following concrete NTFS backend next.

## Recommended backend

**Write-through staging → staged-file flush → handle-relative POSIX rename → flush the same retained file handle.** Scope the initial implementation to local NTFS, an existing authorized/durably provisioned parent, same-directory sibling publication, and a Windows profile supporting extended rename semantics. Do not use directory flushing or ambient-path reconstruction after parent admission.

This gives Windows a real success path. Other filesystems require their own qualified implementation; permanently returning Unsupported on the required Windows profile does not satisfy S01.

## Official evidence and limits

- **NTFS rename metadata:** CreateFileW's Caching Behavior explicitly says write-through causes NTFS to flush resulting metadata changes, including renames. Buffered write-through is allowed; unbuffered I/O is separate and has alignment constraints. Use buffered write-through with explicit flushes for arbitrary-length state files. The hardware caveat remains: storage must honor flush/write-through requests. [CreateFileW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew#caching-behavior).
- **Flushing file metadata:** File Caching states that metadata changes must be flushed through the file or write-through. FlushViewOfFile documentation assigns physical completion of file metadata and dirty data to the subsequent FlushFileBuffers call. These support flushing the retained writable file, without inferring a directory-handle guarantee. [File Caching](https://learn.microsoft.com/en-us/windows/win32/fileio/file-caching), [FlushViewOfFile](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-flushviewoffile#remarks), [FlushFileBuffers](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers).
- **Handle-relative publication:** FILE_RENAME_INFO permits RootDirectory plus a relative target name. Extended POSIX replacement semantics keep old handles valid while later opens select the renamed object. [FILE_RENAME_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_rename_info), [FILE_RENAME_INFORMATION](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information).
- **Native availability:** Microsoft documents FileRenameInformationEx from Windows 10 version 1709. Qualify actual supported Windows builds/architectures; exposing an entry point is not native conformance. [FILE_INFORMATION_CLASS](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/ne-wdm-_file_information_class).

**Composition inference:** applying extended rename to the original write-through file object, followed by a flush of that same object, composes these NTFS metadata rules into durable publication. Microsoft does not provide an exhaustive specification of this exact multi-call protocol and every crash point. This is a defensible implementation candidate now; atomic visibility, acknowledged power-loss durability and filter/storage compatibility still need native tests. It is substantially better grounded than a claimed directory fsync equivalent.

## Concrete sequence

1. Retain the authorized parent handle and service writer lease. Validate target as a single native component. Admit the local-NTFS profile from native object/volume evidence; a drive letter or an NTFS name returned by a remote server is insufficient. Missing evidence must fail admission.
2. Exclusively create a sibling stage relative to that handle, with the framework's private security descriptor where requested. `NtCreateFile` supports RootDirectory plus a bounded UTF-16 relative name. Use `FILE_CREATE` and `FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_WRITE_THROUGH | FILE_OPEN_REPARSE_POINT`. Retain write/delete/metadata access; make sharing explicit. Avoid `FILE_ATTRIBUTE_TEMPORARY` and delete-on-close.
3. Validate stage privacy/kind through the opened handle, write all bytes, then `FlushFileBuffers(stage)`. Errors so far leave the target untouched.
4. Build a checked, aligned `FILE_RENAME_INFO` buffer; destination RootDirectory is the retained parent, FileName is the target component, length is in bytes. Call `SetFileInformationByHandle(stage, FileRenameInfoEx, ...)`. Replacement uses `FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS`; publish-if-absent omits replacement. Never copy, delete the target first, bypass access checks or override read-only policy.
5. On successful rename, record publication immediately. Keep the **same writable handle** and call `FlushFileBuffers(stage)` again. Do not reopen the target to flush it.
6. Only after this final flush succeeds return durable success for the admitted profile and advance committed bookkeeping. Release handles/lease normally.

Relative user-mode creation and ntdll linkage are documented; FILE_WRITE_THROUGH comes from the native options contract. Its value is 0x2, **not** the Win32 FILE_FLAG_WRITE_THROUGH value 0x80000000. [NtCreateFile user-mode](https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntcreatefile), [native options](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntcreatefile).

Installed windows-sys 0.61.2 already supplies these APIs/constants, including NtCreateFile, FILE_WRITE_THROUGH, extended rename flags, FileRenameInfoEx, FlushFileBuffers and RtlNtStatusToDosError. Relative creation adds Wdk_Foundation/Wdk_Storage_FileSystem/Win32_System_IO features to the filesystem/security features previously identified. Check NTSTATUS/completion and use owned handles; do not copy native flag values into app interfaces.

## Error transitions and cleanup

| Native transition | Result | Cleanup/recovery |
| --- | --- | --- |
| Creation/write/privacy validation/first flush failed | `NotPublished` | Target unchanged; remove only the owned stage. |
| Proven no-replace name collision | `NotPublished` + Conflict | Preserve winner; clean owned stage. |
| Rename call failed with effects not proven absent | `PublicationUnknown` | Preserve recovery obligation; no blind rollback or stage deletion. |
| Rename succeeded; final flush failed | `PublishedDurabilityUnknown` | New bytes were published; no durable acknowledgement or destination removal. |
| Final flush succeeded | Success under admitted profile | Commit caller bookkeeping, release resources. |

**Disarm automatic staging deletion before submitting rename**, not only after it returns successfully. A write-through error could follow namespace work; a destructor deleting the retained stage handle could then delete the published object. Cleanup after uncertainty needs positive native identity/name evidence under live authority, not existence checks.

For a known successful rename with failed final flush, retry the flush on the retained handle under the existing service lease. After restart, reacquire authority and revalidate identity/content/receipt; a saved path or raw handle is not a recovery grant. No-replace retries must distinguish their own prior winner from another publisher's file. Preserve the original native error alongside phase.

## MoveFileExW alternative

MOVEFILE_WRITE_THROUGH has on-disk wording but its explicit flush guarantee describes copy/delete. It is path-based and cannot directly preserve the held-directory authority model. It may be evaluated for a separately proven trusted private namespace, with same-volume enforcement, no COPY_ALLOWED and a retained-file postflush; it is not a fallback for grant-based runtime-fs. Prefer the handle-relative backend above. [MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw).

## Qualification gates

- Production hooks at first flush, rename submission/completion and final flush: assert phase, retained errors and no destructive ambiguous cleanup.
- Independent readers during replacement: no absent-name interval or partial bytes; old open handles remain old, later opens see new. Include real sharing conflicts.
- Concurrent no-replace writers and retry after failed final flush: one complete winner, never overwritten by loser recovery.
- Parent-path/stage-name swaps: native operations remain on held authority or refuse safely. Test full file identity and private-from-creation ACLs.
- Abrupt native/VM storage interruption at each transition and immediately after externally recorded acknowledgement. Drop volatile storage state, not a graceful VM stop or merely process kill; verify NTFS recovery and complete expected data.

The existing-parent precondition is deliberate. Recursive/new parent provisioning has separate ancestor persistence obligations and remains a required S01 integration item; this sequence does not certify `create_dir_all` followed by a file flush. A complete Windows service must implement that first-launch path as well. Local-NTFS qualification also does not certify ReFS, remote or cloud-backed storage.
