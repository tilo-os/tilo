# Descriptor-relative durable storage specification

**Status: ADOPTED DESIGN (2026-09-15).** Jointly adopted with the [implementation plan](../plans/2026-09-15-descriptor-durable-storage.md) after controller review. The public boundary below is the design contract; implementation and consumer qualification remain outstanding. Adoption does not dispatch source work or accept a standalone producer.

**Portable-shell relationship:** This storage boundary supports the [portable macOS/Linux shell specification](2026-09-12-macos-shell-port-design.md), especially its shared state/storage and cross-host requirements.

## Problem and outcome

Current `atomic_file` path helpers synchronize file contents but ignore directory-sync errors. Existing app-state and View owners retain paths and later reopen them, so renaming an approved namespace and replacing its old path can redirect subsequent operations. Existing governance storage checks directory synchronization but remains path-based. None supplies the shared combination required here: descriptor-relative namespace custody, strict file and directory synchronization, explicit uncertainty after namespace mutation, and exclusive cooperative writer ownership.

The candidate outcome is an opt-in `atomic_file::dir` module for Linux and macOS. It owns directory and file handles, accepts only checked single-component native names, retains ancestor lineage, exposes a non-clone exclusive writer lease, and publishes private snapshots atomically. Policy, schemas, autosave, invocation admission, issuer queues, app-specific retries, and higher-level recovery remain with their current or later owners.

The Files Task 4 producer ruling requires an independently accepted form of this shared primitive before AppLocalStateOwner integration. That sequencing ruling does not accept this API or any standalone issuer. A later trusted issuer may supply a scoped descriptor, but this primitive neither defines nor authenticates that producer.

## Authority boundary

1. `DurableDirectory::adopt` immediately consumes an already-open directory `File`. It verifies directory type and nonzero link count, sets close-on-exec, retains the handle on success, and closes it on error. It authenticates nothing. Trusted code must select and scope the descriptor before adoption.
2. The adopted anchor must have pre-established durable existence. A newly created anchor must be created through this API from an established parent so its ancestor-sync obligation is retained. A receiver of a scoped namespace is not entitled to receive profile ancestors; the issuer must finish their creation durability before handoff.
3. Every caller-supplied name is exactly one native path component relative to a retained descriptor. The API provides no path constructor, ambient current-directory or environment discovery, `/dev/fd` or `/proc/self/fd` reconstruction, `canonicalize`, arbitrary relative path, recursive operation, or raw descriptor escape.
4. Names reject empty bytes, NUL, `/`, `.` and `..`, plus the reserved `.qos-tmp-` prefix. They preserve non-UTF8 bytes and distinct native spellings. Kernel `NAME_MAX` failure remains an explicit I/O error.
5. Open and mutation operations use retained parent descriptors. Directory and leaf opens do not follow symlinks. Opened directory and file types are checked on their descriptors; regular-file operations reject multiple hard links.
6. If an opened namespace is renamed and its old path is replaced, operations continue against the original held directory or return `NamespaceUnavailable`. They never discover or touch the replacement path. An externally moving process owns the durability of its rename.
7. The held-descriptor boundary and cooperative gate are not a sandbox against malicious native code in the same process or a mandatory lock against an uncooperative same-UID process. The primitive makes no adversarial inode compare-and-swap claim. Snapshot consumers must exclusively own their private leaf names.

## Exact public boundary

All fields below are private except the fields explicitly shown. These are the adopted design signatures, not existing code.

```rust
use std::{ffi::OsStr, fs::File, io, sync::Arc};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ComponentName(/* owned native bytes; no PathBuf authority */);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryIdentity(/* device/inode; diagnostic/live comparison only */);
#[derive(Clone)]
pub struct DurableDirectory(/* Arc<DirectoryInner>: File + retained lineage */);
pub struct DirectoryWriter(/* directory + Arc<GateInner>; not Clone */);
pub struct AppendFile<'a>(/* File + &'a mut DirectoryWriter */);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    InvalidName, Missing, AlreadyExists, NotDirectory, NotRegular, Symlink,
    MultipleLinks, Busy, TooLarge, SourceChanged, NamespaceUnavailable,
    Unsettled, Unsupported, Io,
}
pub struct StorageError(/* kind + operation/phase + optional io::Error */);
impl StorageError {
    pub fn kind(&self) -> ErrorKind;
    pub fn source_io(&self) -> Option<&io::Error>;
}
// Debug/Display/std::error::Error implemented without another error dependency.

pub type CommitResult<T> = Result<T, CommitError<T>>;
pub enum CommitError<T> {
    Failed(StorageError),
    Unsettled(Unsettled<T>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitPhase { DirectoryCreated, GateCreated, PublishAttempted,
    Published, RemovalAttempted, Removed }
#[must_use]
pub struct Unsettled<T>(/* phase, source, retained files/lineage/lease, completion */);
impl<T> Unsettled<T> {
    pub fn phase(&self) -> CommitPhase;
    pub fn error(&self) -> &StorageError;
    pub fn retry(self) -> CommitResult<T>;
}

impl ComponentName {
    pub fn new(name: &OsStr) -> Result<Self, StorageError>;
    pub fn as_os_str(&self) -> &OsStr;
}
impl DurableDirectory {
    pub fn adopt(opened: File) -> Result<Self, StorageError>;
    pub fn identity(&self) -> EntryIdentity;
    pub fn open_child(&self, name: &ComponentName) -> Result<Self, StorageError>;
    pub fn create_child(&self, name: &ComponentName) -> CommitResult<Self>;
    pub fn read(&self, name: &ComponentName, max_bytes: usize)
        -> Result<Option<Vec<u8>>, StorageError>;
    pub fn sync_lineage(&self) -> Result<(), StorageError>;
    pub fn lock_exclusive(&self, gate: &ComponentName) -> CommitResult<DirectoryWriter>;
}
impl DirectoryWriter {
    pub fn create(&mut self, name: &ComponentName, bytes: &[u8])
        -> CommitResult<EntryIdentity>;
    pub fn replace(&mut self, name: &ComponentName, bytes: &[u8])
        -> CommitResult<EntryIdentity>;
    pub fn remove(&mut self, name: &ComponentName) -> CommitResult<()>;
    pub fn open_append(&mut self, name: &ComponentName)
        -> Result<AppendFile<'_>, StorageError>;
}
pub struct AppendError { pub written: usize, pub requested: usize,
    pub source: StorageError }
impl AppendFile<'_> {
    pub fn identity(&self) -> EntryIdentity;
    pub fn append_and_sync(&mut self, bytes: &[u8]) -> Result<(), AppendError>;
    pub fn sync(&self) -> Result<(), StorageError>;
}
```

`StorageError`, `CommitError`, and `Unsettled` implement the debugging, display, and error traits required for assertions and caller propagation. `DirectoryWriter`, recovery tokens, and append owners are not cloneable or serializable. New directories use mode 0700 and new files and gates use 0600; umask may narrow them. Existing foreign objects are never chmodded to make validation pass. The API exposes no arbitrary mode, general rename, or recursive path operation.

## Operation and durability requirements

### Directory admission and descent

`open_child` uses `openat` on its retained parent with read-only, directory, no-follow, and close-on-exec flags, then retains the parent in the child's lineage. `create_child` uses descriptor-relative `mkdirat` with mode 0700, opens the child without following links, then synchronizes the child and every retained ancestor through the adopted anchor. An existing entry returns `AlreadyExists`; callers may separately choose `open_child`.

If `mkdirat` fails, creation returns `Failed`. If it succeeds but the entry cannot be opened and verified, creation returns a phase-specific `Failed` error that states an entry may now exist; it does not claim success or delete the entry as rollback. Once the child descriptor is obtained, a synchronization failure returns `Unsettled` retaining that exact child and lineage. Retry only synchronizes those retained objects and never repeats `mkdirat` or chooses another name.

The mkdir-to-open interval assumes a private cooperative namespace: a cooperating owner does not replace the just-created entry between those syscalls. No-follow and type checks do not prove that a malicious replacement directory is the mkdir-created inode.

### Reads

A bounded read returns `None` only for a missing leaf under a still-live directory. A symlink, including a dangling symlink, is an error. The implementation performs descriptor-relative no-follow metadata/type screening, opens with no-follow, close-on-exec, and nonblocking flags, and verifies the opened descriptor is a single-link regular file. It reads no more than `max_bytes + 1` using checked arithmetic and returns `TooLarge` for oversized content or growth observed during the read. Callers retain responsibility for document-version and concurrency checks; this read is not an atomic snapshot against an unrelated writer.

### Synchronization strength

Every file and directory synchronization calls checked `File::sync_all` on its retained `File`. On Apple this preserves the pinned Rust standard library's `fcntl(F_FULLFSYNC)` behavior; elsewhere it preserves the standard library's checked fsync behavior. There is no raw-rustix fsync substitution or weaker fallback. Unsupported or failed synchronization is a failure or an explicitly unsettled outcome.

`sync_lineage` visits the leaf directory followed by every retained ancestor through the adopted anchor. Successful creation, gate acquisition, append, publication, and removal acknowledge durability only after every required file and directory synchronization succeeds. Namespace visibility, an enqueue receipt, or an issuer acknowledgement cannot substitute for storage completion. An error after namespace mutation is never reported as unchanged.

### Exclusive writer and append

Gate acquisition opens or creates a nontruncated, no-follow, close-on-exec, owner-only regular single-link file using a fresh open file description and nonblocking exclusive flock. Contention returns `Busy`. Before granting the writer and before every mutation, the implementation verifies that the held gate still matches its parent/name; replacement or unlink returns `SourceChanged`. Successful acquisition synchronizes the gate and lineage. Gate files remain in place on close and cannot be selected by writer mutations.

All cooperating consumers use one established gate name for a lock domain, such as `canonical.lock` or `issuer.lock`; the primitive does not infer equivalence among gate names. Gate descriptors are not inherited across exec or handed to components.

`DirectoryWriter` is non-clone and mutations require `&mut self`. `AppendFile` borrows the writer mutably, opens an existing regular single-link leaf without truncation, and appends with truthful partial-write accounting. `append_and_sync` reports the exact written/requested byte counts on failure and synchronizes the held file before success. Higher-level journal revision and recovery policy remains outside this primitive.

### Atomic create, replace, and remove

Create and replace reserve a random `.qos-tmp-<32 hex digits>` name from 16 random bytes with descriptor-relative exclusive creation, mode 0600, no-follow, and close-on-exec. The operation writes all bytes, synchronizes the held temporary file, publishes through the same held parent descriptor, then synchronizes the parent and retained lineage. Create uses no-replace rename support and never degrades to check-then-replace. Replace requires an existing regular single-link private leaf and never predeletes or truncates it in place. The new file descriptor remains open through durability acknowledgement or retry.

Before publication, failure leaves the destination unchanged and performs best-effort cleanup only for that operation's reserved temporary name. Cleanup failure remains diagnostic. An inconclusive rename failure returns `PublishAttempted` uncertainty and retains the temporary/new file plus gate. Retry determines whether the destination's no-follow identity equals the held new file; it never blindly repeats rename. After successful publication, identity-check or synchronization failure returns `Published` uncertainty. Retry requires the same new inode at the destination, synchronizes the held file and lineage, and cannot overwrite an intervening revision.

Remove accepts only a regular single-link leaf outside the gate and reserved temporary namespace. A missing leaf before unlink returns `Missing`. After successful unlink, synchronization failure returns `Removed` uncertainty; retry requires continued absence before synchronizing and refuses a newly installed entry. An ambiguous unlink error returns `RemovalAttempted` uncertainty and does not replay unlink. The primitive adds no recursive deletion, rollback, or garbage collector.

### Uncertainty ownership and lifetime

An unsettled mutation retains every file, directory lineage, completion state, and gate needed to reconcile the exact operation. It independently retains the gate and latches its originating writer unavailable. Successful retry clears that latch. Dropping a token does not silently clear the latch or unlock a still-owned gate; flock is released only when all owners close. No destructor performs I/O or claims safe shutdown. Higher-level outcome owners must retain unsettled state beyond widget, mount, or session closure.

## Packaging and compatibility

The module is opt-in under feature `durable-dir`, available only on Linux and macOS. Existing `atomic_file::{write, write_with_mode, write_creating_parent, write_creating_parent_with_mode}` signatures, behavior, default dependency footprint, and tests remain unchanged, including their current best-effort directory synchronization. No existing caller is migrated implicitly. Unsupported targets expose no `dir` module and keep existing path APIs.

The only shared manifest normalization allowed by this work is root workspace dependency `rustix = { version = "1.1.4", features = ["fs"] }`, with runtime-fs's equivalent direct edge changed to workspace inheritance and atomic-file's target-specific optional edge inheriting it. Preserve locked rustix 1.1.4 and the existing `fs` feature. Do not alter vendor/winit's independent rustix 0.38 edge, cap-std, or unrelated packages. The default atomic-file configuration must not activate rustix or getrandom.

## Consumer boundaries

App-state retains its single canonical writer, schemas, dirty/checkpoint ownership, document-version checks, and recovery decisions. View retains desired bytes and reports strict save failure. An issuer journal serializes its own revisions and reconciles uncertain state. Files Task 4 keeps schemas, intents, retries, and namespace ownership in AppLocalStateOwner. Each consumer chooses fixed private names and explicitly opts into the feature only after this primitive is accepted.

This specification does not define a profile path, storage-binding wire format, session issuer, invocation queue, policy grant, service, transaction database, migration, or second canonical document writer. It does not reinterpret public Files grants as private state authority. It does not change launch, compositor, graphics, Linux defaults, or HDR behavior.

## Qualification boundary

Implementation acceptance requires the V4 plan's four sequential task gates on native macOS and native Fedora at the same reviewed SHA, lockfile, Rust 1.97.1 toolchain, feature selection, and exact test inventory. Tests must use owned temporary directories and explicit inputs, never HOME or parent-process environment mutation. Rename/replacement, symlink, special-node, lock-lifetime, fault-injection, partial-write, uncertainty, and process-crash/control-flow cases must demonstrate truthful behavior against the original held tree. Process helpers need bounded readiness, teardown, and reap ownership.

These filesystem tests do not prove physical power-loss survival, network-filesystem behavior, behavior of uncooperative writers, or minimum-OS support. The existing local feasibility probe recorded five successful checked `File::sync_all` calls on disposable workspace storage: an empty directory, a regular file, the directory after file creation, a child directory, and the parent after child creation. That one-host result is feasibility evidence only. It is not implementation qualification, cross-filesystem qualification, or permission to weaken failure handling, and it is not rerun for this document preparation.

## Adoption criteria

The spec and V4 implementation plan were jointly adopted after full controller review. This tracked pair preserves the API, limits, four-task boundaries, sequential review gates, checked file-and-directory synchronization, explicit pre-established anchor, opt-in compatibility boundary, and exact rustix normalization above. Adoption and later implementation dispatch are separate decisions.
