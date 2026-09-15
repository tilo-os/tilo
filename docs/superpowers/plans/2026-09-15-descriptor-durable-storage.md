# Descriptor-relative DurableDirectory Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task, only after the controller dispatches implementation. One source author, then review and both-host gates, before the next task.

**Status: ADOPTED PLAN (2026-09-15), implementation not dispatched.** Jointly adopted with the companion specification after full controller review of V4 and its preserved V3 delta. This plan preserves the reviewed API, limits, four-task boundaries and both-host gates. No storage implementation or consumer integration is qualified by document adoption.

**Goal:** Supply the minimal shared descriptor-relative storage operations needed by later canonical app-state, View, and issuer-journal consumers, with retained ownership and truthful durability failures.

**Architecture:** Add an opt-in `atomic_file::dir` module with owned directory/file handles, checked single-component names, strict directory synchronization, an exclusive writer lease, and atomic snapshot publication. Reuse locked rustix descriptor syscalls and std's checked `File::sync_all`; keep policy, schemas, autosave, invocation admission, and higher-level recovery decisions with their existing owners.

**Tech Stack:** Rust workspace/root lockfile, `std::fs::File`, locked rustix 1.1.4 (`fs`), workspace getrandom 0.2 (locked 0.2.17), existing tempfile, isolated nextest. Linux and macOS only for the new module; existing default path APIs remain available on their current targets.

**Spec:** [Descriptor-relative durable storage](../specs/2026-09-15-descriptor-durable-storage.md). This self-contained specification is normative; ignored preparation documents are historical provenance only. Files Task 4 requires the independently accepted shared primitive before AppLocalStateOwner integration. Preserve checked std `File::sync_all` semantics on files **and directories**, with no weaker fallback; unsupported sync is failure/uncertainty. This plan does not define a session issuer/queue or integrate any consumer.

## Global constraints

- “Keep Linux defaults and HDR behavior.” This storage-only change alters no launch/compositor/graphics behavior.
- Every pathname supplied to a new operation is exactly one checked native component relative to a retained descriptor. No path constructor, cwd/env discovery, `/dev/fd`/`/proc/self/fd` reconstruction, `canonicalize`, or fallback path reopen.
- Existing `atomic_file::{write,write_with_mode,write_creating_parent,write_creating_parent_with_mode}` retain signatures, implementation behavior, default dependency footprint, and tests, including their existing best-effort directory sync. New strict consumers must opt in; do not silently migrate existing callers.
- Successful publication requires actual file sync plus every required retained directory sync. Metadata visibility/queue receipt is not durability. An error after namespace mutation is not reported as “unchanged”.
- No shell/app-state/ui/runtime-fs source edits in these tasks. Task1 may mechanically normalize the existing runtime-fs rustix manifest edge into workspace inheritance as specified below; this does not change its resolved version or features. No new service, transaction database, schema, policy grant, or second canonical document writer.
- Tests own real temporary directories, use explicit inputs, and never mutate HOME or the parent process environment. Native filesystem tests are not power-cycle qualification or proof against malicious native code in the same process.

## Preflight interface table

| Current source / pinned implementation | Already available | Missing contract supplied here |
| --- | --- | --- |
| `crates/system/atomic-file/src/lib.rs:101–142`; manifest | Std-only path replacement; file sync; directory sync errors ignored | Descriptor-relative operations, retained ancestors and strict error propagation, opt-in feature |
| `crates/system/app-state/src/lib.rs:731–754,982–1027,1053–1087` | Canonical directory checks, append/recovery owner, ancestor-sync intent, exclusive flock | Same operations against held handles without later path reopening; integration is a later plan |
| `ui/src/application_view.rs:112–140` | Existing View owner and snapshot serialization | Descriptor consumer, later; this plan only supplies its storage primitive |
| `crates/collab/channel-crypto/src/governance.rs:268–296` | Exclusive random temp, checked file sync, rename and checked directory sync | Reusable strict sequence with held descriptors; do not reuse MLS journal types |
| Cargo.lock rustix 1.1.4; cached `rustix-1.1.4/src/fs/at.rs:78,199,236,264,296`, `fs/fd.rs:156,317` | `openat`, `mkdirat`, `unlinkat`, `renameat`, macOS/Linux `renameat_with(NOREPLACE)`, fstat and flock; safe OwnedFd APIs | Ownership, name checks, mutation outcomes and uncertainty retention around these syscalls |
| Cargo.lock cap-std/cap-primitives 3.4.6; cached cap-std `fs/dir.rs:63,109,391` | Adopting std directory files and capability-rooted general path resolution | Does not itself impose this single-component/no-symlink/strict-publication contract. No need to add this dependency here |
| `.superpowers/.../real-files-provider-plan-draft.md:135,276,369–408` | Planned `EntryName`, anchored Files provider and private fixed-key state owner | Plans, not implemented DurableDirectory APIs. Files permits some contained symlink traversal and no-replace public effects; this private storage primitive rejects symlinks and permits explicit private snapshot replacement. Do not merge their authorities/types |
| `Cargo.toml:345`, locked getrandom 0.2.17 `src/lib.rs:370` | `getrandom(&mut [u8])` for exclusive temporary names | New consumer dependency edge only; no RNG framework |
| Pinned Rust 1.97.1 std `library/std/src/sys/fs/unix.rs:1391–1402` | `File::sync_all` uses `fcntl(F_FULLFSYNC)` on Apple and fsync elsewhere | Must preserve this behavior, not substitute raw rustix fsync on macOS |

The std source was read at `/Users/user/.rustup/toolchains/1.97.1-aarch64-apple-darwin/lib/rustlib/src/rust/library/std/src/sys/fs/unix.rs`. Root verified [Apple fsync semantics](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fsync.2.html) and [Apple disk-write guidance](https://developer.apple.com/documentation/xcode/reducing-disk-writes). Existing std-based helpers already request the stronger Apple file sync. The verified current weakness is ignored directory-sync failure in atomic-file, not an assumed raw-fsync implementation. New directory F_FULLFSYNC failures must surface; do not weaken the guarantee to make a platform gate pass.

## Files and exact public boundary

Create `crates/system/atomic-file/src/dir.rs` for public types, checked names, admission, children and bounded reads; `src/dir/lease.rs` for gate lifetime and append handles; `src/dir/atomic.rs` for snapshot publication/removal and unresolved-sync ownership. Private syscall-bound fault hooks and unit tests live in their owning modules under `cfg(test)`. Create `tests/durable_directory.rs` for real filesystem/process integration tests. Modify atomic-file's manifest and `src/lib.rs`, the root workspace rustix dependency declaration and runtime-fs's rustix manifest edge as specified below, plus root Cargo.lock dependency edges if Cargo requires them. Update manifest commentary to say **default** std-only leaf.

```toml
[features]
default = []
durable-dir = ["dep:rustix", "dep:getrandom"]

[target.'cfg(any(target_os = "linux", target_os = "macos"))'.dependencies]
rustix = { workspace = true, optional = true }
getrandom = { workspace = true, optional = true }
```

```rust
#[cfg(all(feature = "durable-dir", any(target_os = "linux", target_os = "macos")))]
pub mod dir;
```

After accepted Files Task1, runtime-fs and atomic-file are two workspace consumers of rustix. Add `rustix = { version = "1.1.4", features = ["fs"] }` under root `[workspace.dependencies]` and replace runtime-fs's existing equivalent direct edge with `rustix.workspace = true`; atomic-file inherits it optionally as above. Preserve the locked 1.1.4 version and the existing Files fs feature set. If another task has already centralized this exact edge, reuse it instead of duplicating it. Do not change vendor/winit's independent rustix 0.38 edge, cap-std, or unrelated dependencies. Use the root workspace/lockfile; add no new package versions and no nested manifests/locks. Default builds must not activate rustix/getrandom through atomic-file. Unsupported targets simply have no `dir` module; their existing path APIs remain unchanged.

All following public fields are private unless explicitly shown. These signatures are the candidate interface, not existing code:

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

`StorageError::kind` returns the small Copy enum. CommitError/Unsettled implement Debug/Display/Error where required by their bounds so assertions and caller propagation work. No `Clone`/serde for writer/recovery/append owners, no raw descriptor escape API, no arbitrary mode/recursive path/rename API. New directories are 0700 and new files/gates 0600 (umask may narrow them); never chmod an existing foreign object to make validation pass. Consumers retain their own namespace header/policy validation and fixed filename mapping.

### Boundary semantics that all tasks must preserve

1. **Names and custody.** Reject empty, NUL, `/`, `.` and `..`; reject the reserved `.qos-tmp-` prefix from caller names. Preserve non-UTF8 bytes and distinct spellings. Kernel NAME_MAX remains an explicit I/O failure. `adopt` consumes File immediately, verifies directory type and nonzero link count, sets CLOEXEC, and closes on error. It authenticates nothing: trusted issuer admission already selected the descriptor. The adopted anchor must already have durable existence; a newly created anchor must be created through this API from an established parent, not adopted to discard its ancestor-sync obligation.
2. **Anchoring.** `open_child` uses `openat(parent, name, RDONLY|DIRECTORY|NOFOLLOW|CLOEXEC, empty)` and retains the parent Arc. `create_child` does `mkdirat(..., 0700)`, opens that child without following links, then syncs child and every retained ancestor up to the adopted anchor before returning it. Existing entry returns AlreadyExists, never silently chooses it; consumers can explicitly `open_child`. At all times mutations occur through retained parents. File/child type is checked on the opened FD. No reopening an ancestor by its former pathname. The mkdir→open interval assumes the private cooperative namespace stated in item 8: a cooperating owner does not replace that entry between the two syscalls. No-follow/type checks do not provide adversarial inode compare-and-swap or prove that a malicious replacement directory is the mkdir-created inode. Once opened, the actual descriptor is retained; this candidate adds no stronger race claim.
3. **Directory creation failure.** If mkdir fails, report Failed. If mkdir succeeds but the child cannot be opened/verified, report a phase-specific Failed error explicitly saying an entry may now exist; no descriptor/namespace was admitted and no success acknowledged. Do not delete that entry as rollback. Once its FD is obtained, any failed sync returns Unsettled retaining that exact child and its ancestors; retry only syncs those objects and returns that child. Never repeat mkdir or choose a replacement name during this retry.
4. **Reads and special nodes.** Return None only for a missing leaf under a still-live directory. A symlink/dangling symlink is an error, not absence. Use no-follow metadata/type screening, `openat` with NOFOLLOW|CLOEXEC|NONBLOCK, and FD verification; reject nonregular files and multiple hard links. Read at most `max_bytes + 1` with checked overflow and return TooLarge, including growth during read. The caller owns document-version/concurrency validation; a read is not an atomic snapshot against an unrelated writer.
5. **Sync strength/lineage.** All syncs call `File::sync_all` on the retained File. `sync_lineage` visits leaf to anchor, including previously existing ancestors retained during descent; failures propagate. This preserves newly created ancestors across later writes and retry. Receiving a scoped namespace FD does not entitle a component to receive profile ancestors: the issuer must finish their creation durability before handoff; the receiver adopts that already-established scoped anchor. Changing or moving the namespace externally does not authorize discovery of a new parent. Durability of an external mover's rename is the mover's obligation, not silently inferred here.
6. **Locks.** Gate open is no-follow, CLOEXEC, regular, one link, owner-only when created, nontruncating. Use a fresh open file description and nonblocking exclusive flock; contention is Busy, not a second writer or a read-only success. Validate the held gate still matches its parent/name before granting and before each mutation; replacement/unlink is SourceChanged. Successful gate acquisition also synchronizes gate and lineage. Gate files are never unlinked on close or by writer methods, and writer methods reject the selected gate name. All cooperating consumers must use their one established gate name (`canonical.lock`/`issuer.lock`); this primitive does not infer multiple names are one lock domain. No lock FDs are handed to components or inherited across exec.
7. **Writer lifetime.** DirectoryWriter is non-Clone and mutation methods take `&mut self`; a higher-level owner may put it behind Arc/Mutex on its worker. Unsettled publication retains the Arc gate independently of the writer and latches that writer unavailable for further mutations. Successful retry clears the latch; dropping the token does not silently clear it or unlock a still-owned gate. Dropping all owners releases flock through FD close. Pending outcome owners must survive mount/widget closure; no Drop-time I/O or false safe-shutdown acknowledgement.
8. **Namespace trust limit.** A held directory survives its root being renamed and the old path replaced: operations continue at the original directory, or return NamespaceUnavailable if it is unlinked/unusable. The replacement path is never touched. A held child likewise stays the original child after rename. Gates plus private namespaces coordinate cooperative writers; they are not mandatory locking against a malicious same-UID process. No-follow prevents symlink traversal even during entry changes, but metadata-check-then-rename is not a cross-platform conditional replacement syscall. Snapshot callers must exclusively own private leaf names; the primitive does not claim adversarial inode compare-and-swap.

## Task 1: Owned directory admission, names and bounded reads

**Files:** Modify atomic-file `Cargo.toml` and `src/lib.rs`, root `Cargo.toml` and runtime-fs `Cargo.toml` only for the exact rustix workspace normalization above, and root `Cargo.lock` only for required dependency edges; create `src/dir.rs` and `tests/durable_directory.rs`.

**Consumes:** Existing std File and locked rustix/getrandom. **Produces:** ComponentName, EntryIdentity, StorageError/ErrorKind, DurableDirectory::{adopt,identity,open_child,read,sync_lineage}; no mutations exposed yet.

- [ ] Add feature/dependency declarations above and cfg-gated test file. Add a real fixture helper: `tempfile::tempdir()`, `File::open(temp.path())`, then `DurableDirectory::adopt`. Keep TempDir alive separately from the handle. Write these failing tests before implementation:

```rust
#[test]
fn invalid_components_are_rejected() {
    for s in ["", ".", "..", "a/b", "/a", "a\0b", ".qos-tmp-client"] {
        assert!(ComponentName::new(OsStr::new(s)).is_err(), "{s:?}");
    }
}
#[test]
fn missing_is_distinct_from_a_dangling_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let d = DurableDirectory::adopt(File::open(tmp.path()).unwrap()).unwrap();
    let n = ComponentName::new(OsStr::new("state")).unwrap();
    assert_eq!(d.read(&n, 16).unwrap(), None);
    std::os::unix::fs::symlink("absent", tmp.path().join("state")).unwrap();
    assert!(d.read(&n, 16).is_err());
}
```

- [ ] Add `adopt_regular_file_rejects_and_closes`, `read_limit_checks_growth_and_overflow`, `fifo_and_directory_leaf_refuse_without_blocking`, `hardlinked_leaf_refuses`, `non_utf8_component_bytes_remain_distinct`, `child_symlink_is_not_followed`, and `opened_root_rename_reads_original_not_replacement`. Tests assert outside/symlink-target bytes remain untouched; use rustix mkfifo for the FIFO fixture, no process-global umask manipulation.
- [ ] Run Task 1 gates below; record meaningful missing-API/test failures, not unrelated compiler/dependency setup failures.
- [ ] Implement checked native bytes (`OsStrExt::as_bytes`, owned CString/OsString), safe File↔OwnedFd conversion, rustix `fstat/openat` and `fcntl_setfd` CLOEXEC. DirectoryInner retains `Arc<DirectoryInner>` parent or None for the adopted anchor. `sync_lineage` calls held `File::sync_all` leaf-to-root; no ambient paths. Bounded reads use `Read::take(limit.checked_add(1)?)` and verify maximum afterward. Store operation/phase and original io error in StorageError. Implement existing-type checks exactly as boundary semantics describe.
- [ ] Run Task 1 gates, self-review name/no-follow/absence behavior and default feature graph, commit only these files as `feat(atomic-file): add owned directory reads`. Controller reviews the commit and runs the same Fedora gates before Task 2.

## Task 2: Durable child creation and retained uncertain sync

**Files:** Extend `src/dir.rs`; create `src/dir/atomic.rs` for shared CommitResult/CommitError/Unsettled machinery (publication methods come in Task 4). Add unit tests in `dir.rs` and real cases in `tests/durable_directory.rs`.

**Consumes:** Task 1 directories/names/errors. **Produces:** create_child, CommitPhase, Unsettled<T>::retry and private retained completion/sync-work representation used by Tasks 3–4.

- [ ] Write `new_child_requires_all_ancestor_syncs`, `ancestor_sync_failure_retains_original_child`, `failed_child_open_reports_entry_may_exist`, `existing_child_never_reinitializes`, and `renamed_child_sync_retry_never_opens_replacement`. Add private per-owner fault hooks at syscall boundaries; no public fake backend/global environment toggle. Hook records the EntryIdentity of each attempted sync and can fail one selected identity once. Tests use real directories even when injecting one error.

```rust
// Unit-test core: inject only a sync failure after real mkdir/open.
let result = root.create_child(&name("records"));
let pending = match result {
    Err(CommitError::Unsettled(p)) => p,
    other => panic!("expected retained sync failure: {other:?}"),
};
assert!(matches!(pending.phase(), CommitPhase::DirectoryCreated));
std::fs::rename(tmp.path().join("records"), tmp.path().join("original")).unwrap();
std::fs::create_dir(tmp.path().join("records")).unwrap();
let opened = pending.retry().unwrap();
assert_eq!(opened.identity(), identity_of(&tmp.path().join("original")));
```

`name` is a local helper calling ComponentName::new; `identity_of` adopts an explicitly opened test directory and returns identity. Install the fail-once hook for the parent's next sync immediately before the shown call; hooks are owner-local and private to unit tests.

- [ ] Run Task 2 gates to expose missing behavior. Implement mkdir→open→child/ancestor sync, returning exact held completion on Unsettled. Error before obtaining a child FD explicitly records DirectoryCreated/entry-may-exist when applicable. Retry performs only retained sync work; if it fails again it returns the same owned uncertainty, not a fresh path lookup. Add checked counter/identity assertions showing every newly created level is synchronized, even after an ancestor was renamed.
- [ ] Add documentation and one complete example creating `package`, then `workspace`, then `records` by repeated create_child from a pre-established anchor. On AlreadyExists the example explicitly open_child; any other failure propagates. Never turn a partial namespace into an empty successful store.
- [ ] Run Task 2 gates; review the failure ownership graph for descriptor leaks/double-close and implicit rollback. Commit `feat(atomic-file): retain directory durability obligations`; controller review and same-SHA Fedora gates precede Task 3.

## Task 3: Exclusive writer lease and retained append handle

**Files:** Create `src/dir/lease.rs` and test-only `tests/support/child_guard.rs`; extend dir exports/uncertainty completion and `tests/durable_directory.rs`.

**Consumes:** Task 2 strict creation/sync outcomes. **Produces:** lock_exclusive, DirectoryWriter ownership and mutation-latch plumbing, AppendFile and AppendError.

- [ ] Add `independent_process_writer_is_busy`, `gate_is_not_truncated_or_unlinked`, `gate_symlink_and_hardlink_refuse`, `gate_replacement_invalidates_writer`, `gate_sync_failure_keeps_lock_until_recovery_drops`, `append_hits_original_after_root_rename`, `append_missing_never_creates`, and `partial_append_error_reports_written_bytes`. Unit fault hooks interrupt after one real prefix write or before file sync; never call a synthetic document backend.
- [ ] Add an ignored integration helper test `child_lock_holder`: launch the **current compiled test executable** with `--exact child_lock_holder --ignored --nocapture`, passing the explicit temp-root path through child-only Command environment. Wrap every successfully spawned Child in the parent-owned RAII guard described below immediately, before taking pipes, reading readiness, or asserting anything. Read the `LOCKED` marker with the bounded protocol; parent opens a separate descriptor and expects Busy while the helper lives, then successful acquisition after confirmed reap. No nested Cargo, unbounded sleeps/reads, PID-as-identity recovery, or persistent service.
- [ ] Implement private test-only `ChildGuard` in `tests/support/child_guard.rs` and import it from the integration test. The unit crash-fixture parents in Task 4 must use the same helper via a cfg(test) path module; this is test support, not an additional production API. `ChildGuard::spawn(&mut Command)` moves the Child directly into the guard as the first action after successful spawn, with no intervening assertion or fallible pipe setup. The guard is non-Clone, never detaches a live child, and is disarmed only after wait/reap succeeded. Its Drop path requests kill and waits/reaps on early return, readiness failure, assertion failure, or panic; cleanup must not panic during unwinding. Kill returning an error because the child already exited still requires wait/reap. Store/report other cleanup errors rather than claiming fixture success.
- [ ] Use monotonic deadlines: readiness 10 seconds; requested normal exit 2 seconds; forced termination/reap 5 seconds. Read stdout through nonblocking bounded reads (set NONBLOCK with already available rustix fcntl flags), cap buffered readiness output at 64 KiB, handle WouldBlock with deadline-limited polling, and check premature child exit via try_wait. Polling waits are at most 10 ms and never exceed the remaining deadline. Do not call an unbounded read_line, wait_with_output, or pipe-thread join. On readiness/output/normal-exit failure, kill immediately and reap; termination polling is bounded, and a final wait after try_wait reports exit is cached/nonblocking. A helper that cannot be killed/reaped by the deadline is a cleanup failure, not an ignored or passing test; ownership must not be released as if cleanup succeeded. The guard must retain the Child for kill+wait cleanup on failure paths. Helpers spawn no descendants and explicitly flush readiness markers.
- [ ] Add normal parent regressions `helper_readiness_timeout_reaps_child`, `helper_premature_exit_is_reaped`, and `helper_assertion_unwind_reaps_child`. Use child-only modes of the same ignored helper for no readiness, immediate exit, and held-lock readiness. For unwind use catch_unwind around an assertion after readiness, then verify the guard reaped the child and the gate can be acquired. Preserve the TempDir until cleanup completes. These tests must prove the error path itself closes the owned helper, rather than relying on nextest's outer test timeout.
- [ ] Run Task 3 gates. Implement O_CREAT nontruncating gate open with NOFOLLOW|CLOEXEC|NONBLOCK and mode0600, regular/single-link checks, nonblocking flock, identity recheck, then gate+lineage sync. Keep the exact gate FD in GateInner; no explicit flock Unlock on partial owner drops. If sync fails, Unsettled owns the already-locked writer; a second process stays Busy.
- [ ] Implement open_append with APPEND|WRONLY|NOFOLLOW|CLOEXEC|NONBLOCK, **no CREATE**, precheck + fstat and gate binding validation. The AppendFile borrows the exclusive writer, retains the opened original regular file and parent/name binding, refuses a no-follow identity mismatch or unlinked file before append/sync, and performs a counted Write loop (retry Interrupted without increasing the count; WriteZero fails without inventing progress). Successful append synchronizes that file and directory lineage; an error reports exactly the observed completed prefix and requested total. `sync()` retries only file/lineage synchronization, never appends bytes. If the prefix is partial, sync success does not mean the requested append succeeded: the canonical consumer must retain dirty state and perform its existing full-checkpoint recovery. Even a complete append with sync failure must not be blindly re-appended. This primitive adds no Automerge or new retry policy.
- [ ] Add these two compile-fail doctests to DirectoryWriter documentation and verify its lifetime through the real child test:

```compile_fail
use atomic_file::dir::DirectoryWriter;
fn duplicate(writer: DirectoryWriter) { let _other = writer.clone(); }
```

```compile_fail
use atomic_file::dir::DirectoryWriter;
use std::os::fd::AsRawFd;
fn escape(writer: DirectoryWriter) { let _raw = writer.as_raw_fd(); }
```

- [ ] Run Task 3 gates and commit `feat(atomic-file): hold exclusive descriptor writers`; controller reviews and repeats Fedora gates before Task 4.

## Task 4: Strict snapshot create/replace/remove and cross-boundary recovery

**Files:** Implement publication/removal in `src/dir/atomic.rs`; extend `lease.rs`, module documentation, unit fault tests and integration tests.

**Consumes:** Task 3 exclusive writer and Task 2 retained sync completion. **Produces:** DirectoryWriter::{create,replace,remove}; complete primitive available to later, separately adopted consumer plans.

- [ ] Write real `create_collision_preserves_regular_and_symlink_entries`, `replacement_is_complete_old_or_new`, `replace_requires_existing_regular_leaf`, `temp_creation_collision_never_truncates`, `file_sync_failure_preserves_target`, `post_rename_sync_failure_is_unsettled`, `unsettled_owns_gate_after_writer_drop`, `unsettled_blocks_next_mutation`, `remove_sync_failure_is_not_reported_unchanged`, and `root_and_namespace_replacement_are_never_written` tests before implementation. An owner-local RNG seam forces repeated temp-name collision; after 128 exclusive attempts return failure without changing target. Production randomness remains getrandom.

```rust
// Unit test after a real successful rename, fault at first directory sync.
let failure = writer.replace(&name("state"), b"new").unwrap_err();
let pending = match failure {
    CommitError::Unsettled(p) => p,
    other => panic!("must report visible-but-unsettled publication: {other:?}"),
};
assert!(matches!(pending.phase(), CommitPhase::Published));
assert_eq!(directory.read(&name("state"), 16).unwrap().unwrap(), b"new");
assert!(writer.replace(&name("state"), b"next").is_err());
let committed_identity = pending.retry().unwrap();
assert_eq!(committed_identity, regular_file_identity_in_fixture("state"));
writer.replace(&name("state"), b"next").unwrap();
```

The fixture identity helper opens the explicit test leaf no-follow and obtains metadata; production does not gain a path-based identity helper. Also assert an Unsettled token retains the lock after dropping the writer, and repeat a sync failure to verify the token/FD remains owned.

- [ ] Run Task 4 failing gates. Implement the exact sequence: validate writer/gate and protected name; for replace require current regular single-link leaf; allocate `.qos-tmp-<32 hex digits>` from 16 random bytes with O_CREAT|O_EXCL|WRONLY|NOFOLLOW|CLOEXEC mode0600; write all bytes; sync the held temp File; publish with `renameat_with(NOREPLACE)` for create or `renameat` for replace using the **same held parent FD**; synchronize the parent/retained lineage; return the new EntryIdentity. Never pre-delete a target, truncate it in place, follow symlinks, or downgrade unavailable no-replace semantics to a check-and-replace fallback. Keep the new file FD open through the durability acknowledgement/retry.
- [ ] Prior to publication, failures leave the target unchanged; best-effort cleanup unlinks only this operation's reserved temp name via held parent. Preserve a cleanup error in diagnostics; no successful save is claimed. A failed rename whose publication is not conclusively known returns phase PublishAttempted uncertainty, retaining temp/new FD and gate. Retry checks the target's no-follow identity against that held new file: if it is not the published file, return retained SourceChanged/unsettled for higher-level reconciliation, **never retry rename automatically**. After a successful rename, any failure checking identity or syncing returns phase Published uncertainty. Retry verifies the same new inode at the destination and syncs held file/lineage; it cannot overwrite an intervening revision. This is live inode validation under exclusive private ownership, not a persisted inode grant.
- [ ] Implement remove for a regular single-link leaf, excluding the gate and reserved temp namespace. Perform descriptor-relative unlink and strict parent/lineage sync. Missing before unlink returns Missing. After successful unlink, failed sync is Removed uncertainty; retry verifies absence then syncs, refusing a newly installed entry. An ambiguous unlink error is RemovalAttempted uncertainty and does not replay unlink. No recursive deletion/rollback/garbage collection is introduced.
- [ ] Add private barrier tests whose parent uses the same immediate RAII guard and readiness/termination deadlines to spawn the compiled unit-test executable with `--exact dir::atomic::tests::child_crash_fixture --ignored --nocapture`. This is an explicitly ignored helper; it receives its stage through child-only Command environment and exits at `temp_written`, `temp_synced`, `before_publish`, `after_publish`, `after_directory_sync`. The parent confirms the child was reaped before inspecting the fixture. Parent reopens the **original explicitly identified temp namespace** and inspects actual bytes: before publication old destination survives; afterward any visible destination is complete, never a prefix. Check retained temp behavior and lock release on child death. Label these process-crash/control-flow tests, not hardware power-loss tests.
- [ ] Run the complete rename matrix with root and nested namespace renamed and replaced **between opening handles and each operation**, and between failed sync and retry: create, bounded read, append, full replacement, removal, child creation and lock acquisition must reach the original held tree or fail explicitly. Assert replacement-tree sentinel files and outside symlink targets are byte-identical. Inject file-sync, every ancestor-sync, open/type, rename and unlink faults; all failed operations remain truthful about visible changes. Error injections live only under cfg(test); integration tests call real production APIs.
- [ ] Document consumer obligations: app-state retains its one canonical writer and existing dirty/checkpoint owner; View retains desired bytes and reports strict save failure; journal serializes revisions and reconciles uncertain state. Each consumer will opt into `durable-dir` later and map fixed names; no integration is part of this commit. Run final Task 4 gates and commit `feat(atomic-file): publish durable snapshots by descriptor`; controller reviews that commit and verifies Fedora before declaring this primitive accepted.

## Required gates — future execution, not current evidence

All tasks use the root manifest/lock/config, Rust 1.97.1, retries0 and nonzero selected tests. Run sequentially. The author runs on native macOS; controller reviews the commit then runs identical Cargo payloads on native Fedora at that exact SHA/lockfile before the next author task. Check toolchain with `rustc -vV` and record SHA, command, selected/executed/passed/ignored counts and exit status. No GPU/window/VM launch belongs to these primitive gates.

Mac command prefix:

```sh
CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py
```

Fedora controller uses that host's explicit checkout/target path with the same helper, not the Mac path. The helper clears QOS/QUICK/XDG and provides disposable XDG locations while retaining HOME/toolchain. These leaf-crate commands need no ffmpeg flags. The helper is an existing ignored execution asset; its absence on a host is a precise prerequisite to supply before running gates, not permission to drop isolation.

Before each matching run, save the JSON listing from the corresponding command below. These are discovery commands, not execution evidence. The local `cargo nextest list --help` confirms profile, run-ignored, message-format and config-file options; no listing/build/test was run while preparing this candidate. Feature/package/target selection, profile/config, ignore policy, SHA, lockfile and toolchain must match the subsequent run. No filters may be added only to one side.

```sh
cargo nextest list --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --run-ignored default --message-format json -p atomic-file --features durable-dir --test durable_directory
cargo nextest list --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --run-ignored default --message-format json -p atomic-file --features durable-dir --lib
cargo nextest list --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --run-ignored default --message-format json -p atomic-file --no-default-features --lib
```

Record each selected normal identity as `(binary ID, full test name)` from the listing's filter-match/ignored metadata. Require a nonempty selected-normal set and exact set equality with the completed runtime/JUnit identities for that run; counts alone do not catch substitution. Record and explain any listed-but-skipped identities; an unexpected runtime skip, missing normal identity, duplicate normal execution with retries0, or unlisted normal execution fails the gate. Keep feature-enabled library and default-library inventories separate even when some names are shared. Check discovery again after any source/feature change that can change the inventory.

Inventory ignored helper identities separately, with the same build/feature/target context:

```sh
cargo nextest list --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --run-ignored only --message-format json -p atomic-file --features durable-dir --test durable_directory
cargo nextest list --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --run-ignored only --message-format json -p atomic-file --features durable-dir --lib
```

Tasks 1–2 expect no ignored helpers. Task 3 adds integration identity `child_lock_holder`; Task 4 also adds library identity `dir::atomic::tests::child_crash_fixture`. Unexpected ignored helpers require explicit review; do not add blanket ignored execution to the normal suites. Each fixture invocation is separately recorded by its normal parent test with exact executable/test identity, child-only mode/stage, readiness result, exit or signal, and successful reap. The parent uses only the literal `--exact ... --ignored --nocapture` invocations above: child helpers are never counted as normally selected nextest cases, and parent pass counts do not disguise absent child execution. Runtime accounting therefore has two explicit sets: selected normal nextest identities and the helper invocations required by those parents.

Append each run/check payload separately to the host prefix:

```sh
cargo nextest run --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --run-ignored default --retries 0 --no-tests fail -p atomic-file --features durable-dir --test durable_directory
cargo nextest run --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --run-ignored default --retries 0 --no-tests fail -p atomic-file --features durable-dir --lib
cargo check --manifest-path Cargo.toml --locked -p atomic-file --all-targets --features durable-dir
cargo clippy --manifest-path Cargo.toml --locked -p atomic-file --all-targets --features durable-dir -- -D warnings
cargo test --manifest-path Cargo.toml --locked -p atomic-file --features durable-dir --doc
cargo nextest run --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --run-ignored default --retries 0 --no-tests fail -p atomic-file --no-default-features --lib
cargo check --manifest-path Cargo.toml --locked -p atomic-file --no-default-features --all-targets
cargo clippy --manifest-path Cargo.toml --locked -p atomic-file --no-default-features --all-targets -- -D warnings
cargo tree --manifest-path Cargo.toml --locked -p atomic-file --no-default-features -e normal
cargo fmt --all --check
```

Task 1 has no added doctests yet: its doc command may report zero doctests, but none of the three normal nextest run selections may select zero tests. Task 3 onward must execute the two compile-fail lease doctests. Integration and unit helper tests remain ignored in ordinary runs and are deliberately invoked as bounded, RAII-owned child fixtures by their parent tests; their separate discovery/invocation accounting above is required. Cargo tree must show no new normal dependencies in the default configuration. Existing atomic-file regression tests execute in default and enabled builds. Keep any required Cargo.lock update restricted to the new dependency edges; if `--locked` initially refuses them, resolve with the root Cargo graph offline and review that diff before gates, never update unrelated packages.

A macOS directory full-sync failure is a real unsupported durability condition: retain/report failure and give the controller exact filesystem/error evidence; never count a skipped assertion or raw-fsync fallback as a pass. Fault injection must include this result even on filesystems that support it. Default helper compatibility remains separately tested. Physical power interruption, network filesystem semantics and uncooperative writers are outside these tests; no broader runtime capability claim follows.

## Review provenance and handoff

V2 introduced exact list/run identity accounting, ignored-helper inventories, immediate RAII subprocess ownership with bounded readiness/cleanup, and the cooperative mkdir→open assumption. V3 changed only the shared dependency declaration/scope and feasibility/adoption wording. V4 changes only title/status/reference provenance and the explanation that the Files Task 4 prerequisite ruling accepts sequencing, not this candidate API or a producer. All public storage signatures, limits, tasks, gates, and behavior remain unchanged from V3. Coverage maps: names/read/adoption/root-rename→Task1; ancestor creation/retained sync→Task2; gate/append ownership→Task3; atomic publication/removal/uncertain retry/process boundaries→Task4. Public signatures are defined above and consumed unchanged by later tasks. No session/Notes/policy work or consumer integration was added. The existing Files plan can consume this stricter private snapshot primitive only after joint tracked adoption and independent acceptance, without treating its grant paths as state authority.

**Resolved design decisions and adoption state:** retain the explicit pre-established anchor boundary and opt-in dependency feature; preserve checked Apple F_FULLFSYNC-through-std behavior, with failure/uncertainty and no weaker fallback. The companion spec records the existing feasibility evidence: five successful actual `File::sync_all` calls on this host's disposable workspace storage (empty directory, regular file, directory after file creation, child directory, and parent after child creation). This is API feasibility only; it does not qualify this unimplemented module, other filesystems, minimum OS, or physical power-loss durability. The Files Task 4 ruling makes an independently accepted primitive a prerequisite to AppLocalStateOwner integration; it does not itself accept this API or a standalone producer. Joint tracked adoption is complete. Implementation dispatch remains a separate controller step; document adoption alone does not authorize a worker to begin source changes.
