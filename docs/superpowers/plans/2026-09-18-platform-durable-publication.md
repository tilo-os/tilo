# Platform Durable Publication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use the user-selected `subagent-driven-development` workflow task by task. The registry task is finished; the parent assigns a fresh bounded implementation worker and independent reviews for each task. Workers edit only assigned paths and do not stage or commit.

**Goal:** Deliver framework-owned durable replacement, private creation, native file identity and no-replace publication with real Windows/local-NTFS success, preserved Unix guarantees and phase-correct consumer recovery.

**Architecture:** Extend `atomic-file` rather than introduce another storage service. Its compatibility functions delegate to a typed, opened-directory implementation with private Unix/Windows backends. Domain services retain CRDT/key/receipt policy and leases; they distinguish failed publication from published-but-uncommitted state before compensating or acknowledging success.

**Tech Stack:** Rust 1.97.1, edition 2024, std owned files and existing `FileLease`; Unix rustix 1.1.4; Windows windows-sys 0.61.2; existing tempfile/native test infrastructure.

**Spec:** [Framework contract](2026-09-18-cross-platform-framework/framework-contract.md), [Windows publication strategy](2026-09-18-cross-platform-framework/windows-publication-strategy.md), [storage reconnaissance](2026-09-18-cross-platform-framework/storage-recon.md). Audit ownership: A09-01/A09-02 and storage portions of A08-03/A08-06. S02 lease implementation is already separate.

## Global constraints

- “Filesystem publication additionally distinguishes failure before publication from failure after publication when durability is not proven.”
- “Native handles remain local and never enter CRDT or serialized resource state.”
- “Preserve legacy stored identities, data locations, file-name encodings, checkpoint/journal formats and wire messages.”
- “Missing required native evidence remains outstanding work.”
- Preserve unrelated checkout changes. Use explicit owned paths; no workspace-wide formatting, `git stash`, lockfile regeneration, `CARGO_TARGET_DIR`, or shared worktree target directories.
- Keep native dependencies in framework services. No renderer dependency, app OS branch, caller-supplied verified principal, raw-handle public API, new action bus or second permissions registry.
- Windows must execute the selected NTFS publication path, not a permanent Unsupported stub. Unsupported profiles must fail before target mutation; failure to observe locality/privacy/durability is not proof.
- Unix sequence remains exclusive sibling create → write → file flush → atomic namespace publication → held-parent flush. Windows uses write-through staging → file flush → handle-relative rename → same retained file flush. Do not substitute directory `File::open().sync_all()`, copied bytes, or `ReplaceFileW`.
- Changes to identity publication and its compensating key/record cleanup land together (Task 6). A final-flush failure must never remove a key referenced by published metadata.
- Parent creation is Task 4's explicit native design/qualification gate. Existing-parent Windows implementation proceeds independently, but first-launch/S01 completion requires Task 4; no false mkdir durability claim.

## Boundaries and dependency edges

| Owner/files | Responsibility |
| --- | --- |
| `crates/system/atomic-file/src/lib.rs`, `publication.rs` | compatibility functions, semantic publication/error types, orchestration |
| `atomic-file/src/directory.rs`, `identity.rs` | opened parent authority, child validation, full file/volume identity |
| `atomic-file/src/native/{mod,unix,windows}.rs` | exclusive creation, native privacy/identity/publication/commit; Windows code may split into `native/windows/{acl,nt}.rs` if needed |
| `atomic-file/src/provision.rs` | durable parent creation and retained retry obligations |
| `atomic-file/src/test_support.rs`, `tests/native_publication.rs` | feature-gated production checkpoints and native conformance |
| `app-state/src/lib.rs`, `autosave.rs` | canonical source ownership, full-save durability, migration winner; CRDT/schema policy stays here |
| `accounts/src/vault.rs`, `settings-store/src/lib.rs` | private key/vault publication and first-launch settings integration |
| `runtime-identity/src/lib.rs` | private record writer and phase-aware compensating cleanup |
| `runtime-fs/src/recovery.rs` | held-directory receipt publication; existing recovery vocabulary retained |

In Task 1 add target-scoped rustix `=1.1.4` with exactly `fs` and `process` features to `atomic-file/Cargo.toml`; `process` supplies `rustix::process::geteuid()` for private-file owner validation. In Task 2 add target-scoped windows-sys `=0.61.2`. The latter needs `Win32_Foundation`, `Win32_Storage_FileSystem`, `Win32_Security`, `Win32_Security_Authorization`, `Win32_System_Threading`, `Win32_System_IO`, `Wdk_Foundation`, `Wdk_Storage_FileSystem`; add only additional feature modules actually imported. All versions already exist in Cargo.lock; only reviewed dependency edges may change. `runtime-fs` and `runtime-identity` add `atomic-file = { path = "../../system/atomic-file" }`; other listed consumers already depend on it. Update the obsolete “std-only” comment, not the leaf-crate architecture.

`test-support` is a default-off atomic-file feature used through consumers' **dev-dependencies**, not enabled by application production manifests. It exposes scoped, thread-local fault injection and checkpoint observation; there are no release environment-variable faults. Native private-create unit tests may inspect native ACLs internally; the test observer receives owned metadata/identity snapshots, never a raw handle or a new grant.

## Fixed contracts

These are final framework/headless APIs. Native handles and enum representations stay private; applications use existing SDK services. During Task 1, gate the new typed publication/directory/identity modules and exports with `cfg(unix)` entirely inside atomic-file; leave `lease` unconditional. Task 2 enables Windows identity and private native creation only, without advertising or requiring Windows publication. Task 3 enables the complete APIs for `cfg(any(unix, windows))` and removes the temporary legacy Windows route. No caller migration or new Windows storage-support claim is made before Task 3.

```rust
pub enum Privacy { Inherit, OwnerOnly }
pub enum PublishMode { Replace, IfAbsent }
pub enum PublicationState {
    NotPublished,
    PublishedDurabilityUnknown,
    PublicationUnknown,
}
pub struct PublicationError {
    state: PublicationState,
    source: std::io::Error,
    pending_commit: Option<CommitGuard>, // crate-private owned native state
}
pub struct StorageDirectory { inner: native::Directory }
pub struct FileIdentity { inner: native::Identity }
pub struct VolumeIdentity { inner: native::VolumeIdentity }
```

Required signatures:

```rust
impl PublicationError {
    pub fn state(&self) -> PublicationState;
    pub fn io_error(&self) -> &std::io::Error;
    pub fn into_io_error(self) -> std::io::Error;
    pub fn retry_commit(self) -> Result<(), Self>;
}
pub fn publication_state(error: &std::io::Error) -> Option<PublicationState>;
pub fn replace(path: &Path, bytes: &[u8], privacy: Privacy)
    -> Result<(), PublicationError>;
pub fn publish_if_absent(path: &Path, bytes: &[u8], privacy: Privacy)
    -> Result<(), PublicationError>;
impl StorageDirectory {
    pub fn from_file(file: std::fs::File) -> std::io::Result<Self>;
    pub fn open(path: &Path) -> std::io::Result<Self>;
    pub fn identity(&self) -> std::io::Result<FileIdentity>;
    pub fn open_read(&self, name: &OsStr, privacy: Privacy) -> std::io::Result<std::fs::File>;
    pub fn publish(&self, name: &OsStr, bytes: &[u8], mode: PublishMode,
                   privacy: Privacy) -> Result<(), PublicationError>;
}
impl FileIdentity {
    pub fn of(file: &std::fs::File) -> std::io::Result<Self>;
    pub fn volume(&self) -> &VolumeIdentity;
}
```

Enums derive Copy/Debug/Eq; identities derive Clone/Debug/Eq but not Serialize. `PublicationError` implements Error/Display with original source, and wraps itself inside `io::Error::new(original_kind, self)` for compatibility. `retry_commit` reuses owned handles only for known publication; other states return the unchanged error. Its caller retains the existing service lease. `CommitGuard` owns the published file and parent handles, has no deleting destructor, and never reissues rename.

`StorageDirectory::open` is trusted-host ambient admission of a directory; it is not a new grant provider. `from_file` consumes an already-authorized handle and never obtains authority by resolving its display path. `open_read` is one-component/no-follow, rejects reparse objects for private reads, and validates requested privacy. `OwnerOnly` applies to the file, not an implicit recursive chmod of ancestors. Constructor/native capability checks precede publication. Error means no inferred identity; comparison callers map it deliberately to SourceChanged or their existing I/O error.


Publication in this slice requires a trusted child namespace or caller-held exclusion against hostile stage-name mutation. Retaining a parent handle protects against replacement of its ambient path; it does not itself prevent another actor renaming children within that directory. This API is for admitted framework state/journal namespaces and is not a substitute for the existing granted-user-file mutation machinery. Before the receipt caller migrates, verify its private journal admission supplies this precondition. No check-then-unlink cleanup is described as race-free in a hostile shared directory.

The following describes the final Task 3 behavior, not an interim change to Windows in Task 1. Legacy signatures remain `write`, `write_creating_parent`, `write_with_mode`, `write_creating_parent_with_mode` returning `io::Result<()>`. Unix exact mode behavior is retained internally. Windows legacy `0o600` delegates to OwnerOnly; other raw modes fail before mutation unless an exact semantic mapping is expressly implemented. Portable callers move to Privacy; never silently ignore a restrictive mode.

### Task 1: Phase-aware Unix publication through the existing facade

**Files:** modify `atomic-file/{Cargo.toml,src/lib.rs}`; create `src/{publication,directory,identity,test_support,legacy}.rs`, `src/native/{mod,unix}.rs`; preserve `src/lease.rs` and its tests. Review only necessary Cargo.lock edges.

**Consumes:** existing facade and lease; **produces:** the fixed contracts on Unix, including actual Unix FileIdentity/VolumeIdentity, real Unix wrapper delegation, and unchanged compiling Windows legacy facade/FileLease. No consumer files are scheduled for migration in this task; a safety fix demonstrated necessary by the wrapper error audit must accompany its enabling change rather than be deferred.

- [ ] Preserve cross-target compilation first: move the existing non-Unix write/create/mode/temp implementation unchanged into private `src/legacy.rs` selected with `cfg(not(unix))`; forward all four legacy public functions there on Windows until Task 3. The new native module in Task 1 selects only Unix, and new typed exports/tests are `cfg(unix)`. Do not add a Windows native stub returning Unsupported, reference a not-yet-created Windows module, or gate FileLease. Retain the old non-Unix privacy/durability caveats rather than presenting this bridge as support.
- [ ] Before changing wrapper routing, audit direct `atomic_file::write*` error branches with `rg -n 'atomic_file::write|publication_state' crates apps shell fs`; inspect compensating deletes/key removal surrounding those calls. The current shared facade already propagates parent-open/flush errors, so Task 1 preserves when Err can occur while adding phase/source information. Do not move formerly infallible post-publication checks into the error path without fixing any newly unsafe caller in this same task. Record findings in the task report. In particular, leave runtime-identity's separate custom writer untouched until Task 6 fixes both compensations atomically.
- [ ] Define the following exact default-off test-support contracts, restore previous hooks on unwind, and invoke the observer before a configured failure at the same checkpoint:

```rust
pub enum Checkpoint {
    StageCreated, BeforeDataFlush, DataFlushed, BeforePublish,
    PublishSubmitted, AfterPublish, BeforeCommit, Committed,
}
pub struct StageObservation {
    pub metadata: std::fs::Metadata,
    pub identity: FileIdentity,
}
pub struct CheckpointEvent {
    pub checkpoint: Checkpoint,
    pub stage: Option<StageObservation>,
}
pub fn with_failure<T>(checkpoint: Checkpoint, operation: impl FnOnce() -> T) -> T;
pub fn with_observer<T>(observer: impl Fn(&CheckpointEvent) + 'static,
                        operation: impl FnOnce() -> T) -> T;
```

  At StageCreated, `stage` must contain snapshots queried from the retained open staging file after final creation permissions are set and before writing bytes. A metadata query failure fails the operation; do not fabricate a snapshot or silently omit it. Callbacks cannot access or clone native handles. Unix mode tests inspect `event.stage.as_ref().unwrap().metadata`; Windows ACL tests inspect the real descriptor internally in the native module. Later checkpoint snapshots may be absent when no live staging object exists. Observers may signal/block on owned test channels or write the external crash-probe pipe, allowing inspection before cleanup. They run only on the operation's thread; do not hold the thread-local registry borrow while invoking callbacks. StageCreated/BeforeDataFlush/BeforePublish fail as NotPublished; PublishSubmitted/AfterPublish simulate ambiguous completion and produce PublicationUnknown; BeforeCommit produces PublishedDurabilityUnknown. `with_failure` asserts a supported failure checkpoint before invoking the operation; DataFlushed/Committed are observation-only and rejected as test-programming errors, not simulated native errors. PublishSubmitted occurs after deleting cleanup is disarmed and immediately before the native rename; AfterPublish is after the real successful syscall.
- [ ] Add this production-path regression before implementation, then run `cargo test -p atomic-file --lib --features test-support --locked --offline publication` and observe the missing API/failing assertion:

```rust
let root = tempfile::tempdir().unwrap();
let path = root.path().join("state");
atomic_file::write(&path, b"old").unwrap();
let error = with_failure(Checkpoint::BeforeCommit, ||
    atomic_file::replace(&path, b"new", Privacy::Inherit)).unwrap_err();
assert_eq!(error.state(), PublicationState::PublishedDurabilityUnknown);
assert_eq!(std::fs::read(&path).unwrap(), b"new");
assert!(error.retry_commit().is_ok());
```

- [ ] Implement `FileIdentity::of` and `StorageDirectory::identity` on Unix now using real handle metadata dev/inode; VolumeIdentity holds dev and FileIdentity retains the full inode. Identity equality includes both, no serialization or fabricated zero identity. Implement held-directory Unix create/open/rename/flush with rustix and owner validation using `rustix::process::geteuid()`. For IfAbsent use supported native `renameat_with(NOREPLACE)` on Linux/macOS, never exists-then-rename. Preserve initial mode filtering/restoration and unique exclusive staging. Keep file and directory handles alive through commit; classify syscall failure before publication conservatively when effects cannot be established.
- [ ] Delegate the Unix `write`/`write_with_mode` branches to the typed engine; Unix parent-creating wrappers use the same engine after their existing parent preparation until Task 4 replaces that preparation. Windows branches remain delegated to legacy unchanged. Keep the existing explicit permission-mode temporary-file tests operational while moving their implementation into the native module. Add tests for shorter replacement, empty data, preflush old-target preservation, no-replace collision, mode at StageCreated, preserved typed source through io wrapper, failed rename cleanup and non-deleting error drop. Run the command above and `cargo test -p atomic-file --test file_lease --locked --offline`.
- [ ] Verify the temporary bridge still compiles with `cargo check -p atomic-file --tests --target x86_64-pc-windows-msvc --locked --offline` using the installed target, and check the existing Windows FileLease test target remains included. Record compile-only evidence as such. Submit scoped diff/test evidence for review. No claim that Windows publication or parent provisioning is finished.

### Task 2: Native identity and privacy used by held-directory operations

**Files:** `atomic-file/Cargo.toml` and its reviewed lock edge, `src/{lib,identity,directory}.rs`, `src/native/{mod,unix,windows}.rs`, `tests/native_publication.rs`, `src/lease.rs` only for the narrow identity accessor.

**Consumes:** Task 1 Unix directory/identity engine and temporary Windows legacy route; **produces:** Windows FileIdentity/VolumeIdentity and private native create/read tested without publication, for Task 3. Keep complete Windows publication/directory exports gated and legacy wrappers unchanged. Enable only identity exports on Windows here; private helper tests live inside the Windows module.

Before implementing Task 2, inspect the existing Imager native helpers as extraction/reuse inputs: `apps/imager/src/setup/connection/windows_native.rs` already wraps `FILE_ID_INFO` and relative NtCreateFile; `apps/imager/src/setup/internal_cache/windows.rs` preserves raw UTF-16 and implements private descriptor handling; `apps/imager/src/setup/destination_security/windows.rs` queries owner/DACL and rejects NULL DACL. The framework must not depend on these app modules. Reuse correct low-level patterns without importing app evidence/policy types; preserve distinct policy and native-name requirements. Any temporary duplicate remains assigned to the later coherent Imager extraction and cannot count as closure. The connection helper currently rejects unpaired surrogates whereas the cache preserves them, so do not select a name policy merely by copying one helper.

- [ ] Add identity tests: a cloned handle and a hardlink identify the same file; renaming retains identity; replacing the name changes identity; identical bytes/timestamps do not make two files equal. Assert `identity.volume()` equality only for real same-volume objects.
- [ ] Preserve Task 1 Unix dev/inode implementation and extend identity with Windows `GetFileInformationByHandleEx(FileIdInfo)` preserving volume serial plus all 128 ID bits. Do not use unstable std Windows MetadataExt, zero/None defaults, path hashes or serialized replacement of runtime-fs's v1 identity struct. Add `FileLease::identity() -> io::Result<FileIdentity>` inside the lease owner; expose no handle clone or unlock.
- [ ] Verify macOS extended ACL handling as part of Unix OwnerOnly privacy: mode0600 can coexist with inherited allow-read ACEs (native probe confirmed). Privacy admission must inspect the held file before writing secret bytes and reject unproven extra-principal access; existing private reads require the same check. A temporary fail-closed ACL restriction is not final conformance for benign supported ACLs. Preserve legacy explicit-mode behavior separately from the semantic OwnerOnly contract.
- [ ] Implement Windows exclusive relative creation with NtCreateFile, protected DACL for the trusted process principal, non-inherited handle and privacy validation before bytes. Query token SID natively; reject unsupported impersonation explicitly rather than guessing a principal. Validate existing private files' owner, DACL, regular kind and hardlink policy. Keep Unix effective-owner/mode checks in its backend.
- [ ] Use this identity test before adding Windows publication in Task 3; test private staging creation inside the native module against its real creation helper, with ACL inspection before any bytes and distinct-principal deny tests:

```rust
let first = std::fs::File::create(root.path().join("first")).unwrap();
let cloned = first.try_clone().unwrap();
let second = std::fs::File::create(root.path().join("second")).unwrap();
assert_eq!(FileIdentity::of(&first).unwrap(), FileIdentity::of(&cloned).unwrap());
assert_ne!(FileIdentity::of(&first).unwrap(), FileIdentity::of(&second).unwrap());
```

- [ ] Run `cargo test -p atomic-file --test native_publication --features test-support --locked --offline identity` and the private-create tests on each available native OS. Missing second-user qualification is a failed qualification prerequisite, not an ignored test. Review identity/privacy changes independently before Task 3.

### Task 3: Real Windows local-NTFS publication and compatibility delegation

**Files:** `atomic-file/src/native/windows.rs` (and focused `windows/{acl,nt}.rs` if split), `publication.rs`, `directory.rs`, `native/mod.rs`, `lib.rs`, `tests/native_publication.rs`; remove `src/legacy.rs` for the supported Windows route once delegation is switched.

**Consumes:** Tasks 1–2 semantic API, held parent and private native handles; **produces:** successful Windows replace/IfAbsent and commit retry, reached by existing write functions.

- [ ] Add Windows tests for replace-over-existing, empty bytes, IfAbsent winner, old held readers versus newly opened readers, full identity preservation from stage to destination, and unsupported-volume refusal before target effects. Test local storage admission with positive native device/volume evidence; an NTFS string or failed remote-protocol query alone is insufficient.
- [ ] Implement the approved sequence, using checked native struct allocation and status conversion:

```text
NtCreateFile(relative sibling, FILE_CREATE,
  FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT |
  FILE_WRITE_THROUGH | FILE_OPEN_REPARSE_POINT)
write_all -> FlushFileBuffers(staged)
disarm deleting cleanup BEFORE submission
SetFileInformationByHandle(staged, FileRenameInfoEx,
  RootDirectory=held_parent, FileName=component,
  Flags=REPLACE_IF_EXISTS|POSIX_SEMANTICS or no replacement)
mark Published -> FlushFileBuffers(the_same_staged_handle)
```

- [ ] Preserve native source errors and exact phase. Proven collision is NotPublished/Conflict; ambiguous rename failure is PublicationUnknown; final flush failure is PublishedDurabilityUnknown with CommitGuard. No MoveFileEx/ReplaceFile/copy fallback and no directory FlushFileBuffers. Drop must never delete after ambiguous submission. Validate cleanup's still-owned staging identity before deletion.
- [ ] Enable complete typed APIs and StorageDirectory on Windows, route all four Windows compatibility functions to the native implementation, and delete the temporary Windows legacy implementation/delegation from Task 1. Any unrelated unsupported-host compatibility branch must remain explicitly separate; it cannot be reached by Windows. Then assert native call ordering using the production checkpoints, and assert `write_with_mode(..., 0o600)` reaches this private path. Run on native Windows: `cargo test -p atomic-file --test native_publication --features test-support --locked --offline`; cross-check both Windows target architectures separately when their toolchains are installed. Compile-only evidence is recorded as compile-only.
- [ ] Review the actual native path and failure classification. On supported local NTFS successful publication returns Ok; retain the document's explicit composition-inference/crash-qualification boundary.

### Task 4: First-launch parent durability, with a finite native decision gate

Read the [anchor/retry contract refinement](2026-09-18-cross-platform-framework/directory-provision-contract.md) before dispatch. Its proposed type names/signatures are design candidates, not yet accepted public API. The bounded Windows probe source is accepted (see [report](2026-09-18-cross-platform-framework/directory-probe-report.md)); its native fixture execution is the next Windows gate; successful production Windows first launch remains gated on both directory barriers and initial anchor establishment. A path, existing directory, Known Folder lookup or in-tree marker is insufficient anchor evidence. Do not move this obligation to apps.

Keep app-state source and its existing retry assertions intact during this task's probe/foundation work. Before Task5, resolve the difference between its exact leaf-to-root/full-descendant assertions and the staged component protocol; no ignored tests, synthetic visits or faults injected only after successful provisioning. Owned retry must clearly distinguish committing a partial prefix from completing the full requested hierarchy. Do not expose a retry result that claims a final directory when later components have never been created.

The next Unix production increment is specified in [unix-parent-provision-design.md](2026-09-18-cross-platform-framework/unix-parent-provision-design.md), after Task8 privacy admission is accepted. It creates/opens the full chain, then commits retained directories leaf-to-root and migrates the real parent-creating wrappers/settings caller. Partial-prefix retry cannot certify a requested leaf; unproven submitted mkdir failures retain NamespaceUnknown. App-state remains unchanged in that increment, and Windows first-launch remains unfinished.

Use the [bounded directory evidence and exact native probe](2026-09-18-cross-platform-framework/windows-directory-publication.md) as the refined candidate and qualification input; it is not native execution evidence.

**Files:** create `atomic-file/src/provision.rs`, `examples/directory_commit_probe.rs`, `tests/parent_provision.rs`; modify `src/{lib,directory}.rs`, native modules; write evidence to `docs/superpowers/plans/2026-09-18-cross-platform-framework/windows-directory-publication.md`.

**Consumes:** held-directory admission/identity; **produces:** `provision_directory(path: &Path, privacy: Privacy) -> Result<StorageDirectory, ProvisionError>` and working `write_creating_parent*` delegation. `ProvisionError` preserves native source plus `DirectoryState::{Unchanged,CreatedDurabilityUnknown,NamespaceUnknown}`; “NotPublished” for the eventual file must not conceal directory side effects.

- [ ] Write failing first-launch tests for two absent levels, private-from-creation ancestors, partial creation failure, ancestor replacement, and retry after a directory commit failure even when names now exist. Existing `app-state::canonical_directory_sync_failure_is_retried_even_when_ancestors_exist` is a required regression, not permission to weaken obligations.
- [ ] Implement Unix component-wise creation/open under retained parent descriptors, private modes where requested, and child/parent flush ordering up to the previously durable anchor. Persist retry obligations in owned state and repeat barriers for uncertain existing descendants; never drop the obligation because mkdir now reports AlreadyExists.
- [ ] Resolve the Windows directory commit mechanism **before wiring successful first launch**. The probe must execute real native requests on local NTFS: exclusive `NtCreateFile(FILE_DIRECTORY_FILE|FILE_WRITE_THROUGH)` with authorized directory rights; handle-relative no-replace publication of a staged directory through a write-through handle; and, separately, native full-metadata flush where the API supports the directory object. Record native status/rights and inspect Microsoft's directory/create/flush contracts. A successful directory open or flush call alone is not a durability argument. Do not equate this experiment to the approved regular-file strategy.
- [ ] Run the finite native matrix documented in the accepted probe report, for example `cargo run -p atomic-file --example directory_commit_probe --features test-support --locked --offline -- --root <absolute-native-ntfs-test-root> --profile local-ntfs --anchor-evidence <absolute-controller-evidence.json> --report <absolute-new-external-report.json> --case direct --access minimal`, then crash-test created parent reachability at native completion boundaries. The example's CLI must reject a missing root/profile instead of silently using another disk. Select and document the smallest protocol supported by the official contract plus these observations, including retry of a prior uncertain creation. If neither candidate supports the promised parent result, report the precise failed native operation as a blocking Task 4 result and revise the protocol with the parent; do not substitute always-Unsupported and mark the task done.
- [ ] Implement the selected native provisioning protocol and wire both parent-creating compatibility wrappers. Run `cargo test -p atomic-file --test parent_provision --features test-support --locked --offline` on native Unix/Windows. File publication is attempted only after required parent commit succeeds; a parent error retains its own state/source inside the compatibility I/O error.
- [ ] Review native evidence and first-launch success together. This gate is a concrete unresolved directory-protocol decision, not an additional user approval flow or permission to certify S01 without first launch.

### Task 5: App-state canonical/full-save and migration integration

**Files:** `crates/system/app-state/src/{lib,autosave}.rs`, `Cargo.toml` dev-dependency feature only; atomic-file directory/lease API only if a reviewed consumer requirement needs it.

**Consumes:** Tasks 1–4, `FileLease::identity`; **produces:** actual DocStore save/actor/canonical/migration calls through native framework publication, retaining formats and paths.

- [ ] Add failing regressions to existing lib tests: successful native canonical first launch/reopen, gate/parent replacement detection, failed final flush leaves new bytes but does not advance `persisted` heads; use autosave's existing worker-level test injection to verify saved counters rather than expecting a parent-thread fault hook to cross threads, and two processes cannot both claim different room owners. Run `cargo test -p app-state --lib --locked --offline canonical` and `cargo test -p app-state --lib --locked --offline claim`.
- [ ] Replace false-on-non-Unix `same_file` with held-handle FileIdentity comparisons; use StorageDirectory for canonical root rather than ordinary directory File::open. Coordinate existing gate fields with S02: consume one FileLease, do not keep a duplicate handle merely to inspect identity. Preserve source-changed rejection before actor/document effects.
- [ ] Route `write_full` and actor sidecar creation through typed publication; preserve StoreError::Io compatibility by wrapping PublicationError and add `StoreError::publication_state() -> Option<PublicationState>`. Hold the canonical directory during publication. Replace Unix-only `sync_directory_chain` implementation with provisioning obligations from Task 4; preserve its existing fault/retry assertions.
- [ ] Replace `publish_claim` staging/hardlink implementation with held-directory IfAbsent while retaining newline payload, `.tilo-`/`.qos-` aliases and `.recovered` conflict policy. Conflict reads validate the actual winner. On own published-but-uncommitted claim, retry/revalidate its commit before returning true; merely reading one's room ID is not proof of durable ownership.
- [ ] Run canonical, claim, source-replacement and autosave durability tests; add this assertion at the production `save` boundary:

```rust
let before = store.persisted.clone();
let result = with_failure(Checkpoint::BeforeCommit, || store.save(&mut doc));
assert_eq!(result.unwrap_err().publication_state(),
           Some(PublicationState::PublishedDurabilityUnknown));
assert_eq!(store.persisted, before);
```

- [ ] Review scoped implementation and tests. App-state's append/tail-repair policy remains shared; it must not advance durability state after any native I/O failure.

### Task 6: Identity publication and compensating cleanup in one change

**Files:** `runtime-identity/{Cargo.toml,src/lib.rs}` and existing inline identity-provider tests. Add atomic-file normal dependency and test-support dev dependency; do not port broker transport/keychain policy here.

**Consumes:** typed errors/private held-directory publication; **produces:** `write_private_atomic` delegation and safe cleanup at both call chains.

- [ ] Add real-writer fault tests around `secure_record_with_provider_unlocked` and the import caller: seed a provider, inject BeforeCommit during v2 metadata write, assert disk metadata references a still-present provider value; assert import does not delete that published record. Add prepublication failure tests preserving the existing permissible cleanup.
- [ ] In the same diff replace private staging/ignored parent-sync code with OwnerOnly publication **and** guard both cleanup branches. `secure_record_with_provider_unlocked` must not `provider.remove(locator)` for PublishedDurabilityUnknown or PublicationUnknown. Import must not `remove_file(path)` for those downstream phases. Preserve phase when wrapping errors; only confirmed prepublication cleanup may remove newly stored material.

```rust
let state = atomic_file::publication_state(&error);
let may_compensate = matches!(state, Some(PublicationState::NotPublished));
// For errors without publication evidence, use the existing operation's
// proven pre-publication position; never classify an arbitrary Io error as safe.
```

- [ ] Preserve phase through `secure_record_unlocked` preferred-provider fallback too. Current fallback retries on every SecretService error; after uncertain/published metadata this can return a different-provider InvalidInput error and erase publication evidence before import cleanup. Propagate PublicationUnknown/PublishedDurabilityUnknown without fallback. Add a real-writer regression proving no fallback or key/record deletion at these phases; retain the existing permissible prepublication fallback policy.
- [ ] Ensure key protection enum/backend IDs, locator identity, serialized record versions, zeroization and active-selector formats remain unchanged. Private reads retain owner/link checks; no unavailable keychain downgrade is introduced.
- [ ] Run `cargo test -p runtime-identity --lib --locked --offline secure_record` plus the exact new `publication_cleanup` tests. Confirm fault hits the real atomic-file syscall sequence, not a replacement fake writer. Review helper delegation and both compensations together; no intermediate branch may propagate new errors with old destructive cleanup.

### Task 7: Private vault and settings first-launch consumers

**Compatibility refinement:** [Legacy vault admission](2026-09-18-cross-platform-framework/vault-directory-compat-design.md) preserves ordinary owned0755 legacy directories via separate held namespace-control validation without weakening OwnerOnly key/new-directory privacy or rewriting permissions. Read-only load never creates a key; proven key absence alongside existing ciphertext requires recovery, not regeneration. Task5 shared existing-file/name reconciliation remains required for save-time key durability.

**Files:** `accounts/src/vault.rs`, `accounts/Cargo.toml` dev dependency, `settings-store/src/lib.rs`, `settings-store/Cargo.toml` dev feature.

**Consumes:** OwnerOnly, IfAbsent, validated read, Task 4 provisioning; **produces:** real portable private-key and settings paths, no unused facade.

- [ ] Add native tests: first vault save/reopen under an absent root; second-process key creation selects the same durable winner; malformed/private-invalid/read-denied existing key never regenerates; failed final key flush never permits vault encryption under an uncommitted key. Run current `vault_put_save_load_round_trips` and `key_file_is_owner_only` before changing implementation.
- [ ] Change `local_file_key`: only NotFound may generate; use OwnerOnly no-replace key publication. On collision validate/read the winner instead of returning the losing generated bytes. Retry known final commit while the operation owns the handle; otherwise return the existing unavailable/error outcome without changing custody or overwriting the key. Replace `write_key_file` raw Unix mode call with semantic privacy and provision the vault directory through Task 4.
- [ ] Use semantic publication for vault ciphertext. Keep `try_save`'s public Result<String> compatibility but encode phase in its diagnostic and preserve the rule that downstream account metadata is not published after any failure; do not interpret a published-durability error as permission to regenerate keys.
- [ ] Keep settings JSON layout, newline and location resolution unchanged. Its `write_atomic` calls the fixed parent-creating facade; add first-launch/retry tests and confirm `publication_state(&io_error)` remains observable to framework callers.
- [ ] Run `cargo test -p accounts --lib --locked --offline vault` and `cargo test -p settings-store --lib --locked --offline`. Review key winner/error policy separately from unrelated Secret Service work.

### Task 8: Runtime-fs receipt publication preserves handle authority

**Files:** `runtime-fs/{Cargo.toml,src/recovery.rs,src/grants.rs,src/operations.rs}`, plus the narrowly scoped shared held-directory privacy validator in atomic-file. Operations changes must preserve the original publication failure through the real asynchronous caller path as well as add fault coverage. Do not ungate the entire Unix-only grant provider or change receipt schemas.

**Consumes:** StorageDirectory::from_file and typed publication; **produces:** existing OperationOwner receipt writes through the shared engine, preserving failure poisoning/recovery.

- [ ] Add receipt write fault tests at preflush, ambiguous publication and final commit; require the existing store-poisoning/OutcomeUnknown rules to preserve effects already performed. Test a renamed ambient journal directory still writes only to its originally opened directory.
- [ ] Establish journal namespace admission before migrating writes. Current `OperationOwner::open` checks only `is_dir()` and takes an advisory writer lock; that does not prove exclusion against another principal mutating child names. Validate native owner and effective directory permissions/ACL through framework-owned held-handle admission before opening the sentinel or reading receipts. Reject an unproven writable namespace, preserve cooperative writer exclusion, and add a regression for an externally writable journal with no journal effects before rejection. Caller-supplied owner strings are not native principal proof. A repository search on 2026-09-18 found only test call sites, so do not infer production host admission exists.
- [ ] Construct a StorageDirectory from a duplicated **journal directory** handle already admitted by OperationOwner; never clone the FileLease's lock-bearing file. Route serialized bounded receipt bytes to `publish(final_name, bytes, Replace, OwnerOnly)`. Keep owner matching, admission bounds, lock namespace, receipt filename and postcard v1 bytes unchanged.
- [ ] Preserve phase as a source/typed `FsError::Publication(PublicationError)` where needed, while retaining public report codes and recovery classifications. Audit `Context::recovery`, terminal item/finish failures and job polling: a later poisoned-store error or boolean must not erase the first publication failure. Specify owned-error delivery and subsequent poll behavior without cloning commit authority. Include initial receipt admission as well as post-effect writes in the fault/poisoning policy. Do not advance checkpoint bookkeeping after a failed commit. Remove only the replaced private receipt-write syscall sequence; leave granted user-file operations/transport for their own migration unit.
- [ ] Run `cargo test -p runtime-fs --lib --locked --offline checkpoint`, `cargo test -p runtime-fs --test native_operations --locked --offline` and v1 receipt round-trip fixtures. Review the held-authority path and disk compatibility; this task does not claim a Windows grant-provider port.

### Task 9: Native conformance, crash evidence and closure

**Files:** `atomic-file/tests/native_publication.rs`, `tests/parent_provision.rs`, `examples/publication_crash_probe.rs`; update framework `execution.md` and the finding register only with actual completed scope/evidence. Add `docs/superpowers/plans/2026-09-18-cross-platform-framework/storage-native-evidence.md`.

- [ ] Implement `publication_crash_probe --root <path> --mode replace|if-absent|first-launch --checkpoint <name> --ack <external-pipe>` using the production API/checkpoints. Checkpoints are `stage-created`, `data-flushed`, `publish-submitted`, `published`, `committed`; flush and frame its external acknowledgement independently of the filesystem under test. A missing observer/pipe/storage profile fails the probe.
- [ ] Run native tests on Linux/macOS and Windows x64/arm64 for each claimed profile. Record exact OS/filesystem/architecture, command, status and limitations. Existing test successes must be confirmed at this base; no inherited known-red list, ignored assertion or skip counts as success.
- [ ] Drive abrupt guest/storage power loss before/after each checkpoint from an external native harness, dropping volatile writes without graceful shutdown. After reboot require old-or-new complete contents during replacement, only the correct IfAbsent winner, retained private ACL/identity policy, and reachable newly created ancestors after acknowledged first launch. A process kill is separate recovery evidence, not power loss.
- [ ] Run scoped integration checks after the preceding tasks: `cargo check -p atomic-file -p app-state -p accounts -p settings-store -p runtime-identity -p runtime-fs --tests --locked --offline`, then repository-required `cargo check -p shell --tests --locked --offline`. On this macOS host the validated native prerequisite is `FFMPEG_DIR=/opt/homebrew/opt/ffmpeg@8`; default Homebrew ffmpeg 9 is incompatible. Use this machine-specific variable only where needed, never CARGO_TARGET_DIR.
- [ ] Format only assigned Rust files with direct `rustfmt --edition 2024 <explicit-owned-files>` during implementation; never invoke workspace cargo fmt. Review exact Cargo.lock diff and source boundary scan. Submit final scoped review with implemented paths versus native-qualified claims separated.

## Execution order and unresolved decision

Task 1 delivers a used Unix engine while preserving unchanged compiling Windows legacy facade/FileLease; Task 2 extends Windows identity/private staging only. Task 3 removes that temporary Windows legacy route and enables the real existing-parent Windows engine. Tasks 1 → 2 → 3 are sequential scaffolding, not a permanent Unsupported design. Task 4 closes parent provisioning and first launch; Tasks 5 and 7 depend on it. Task 6 may proceed after Tasks 1–3 for existing admitted identity directories, but enabling its propagated errors and fixing its two compensations are indivisible. Task 8 is a separate bounded receipt-consumer migration. Task 9 closes evidence, not missing implementations.

The only protocol decision deliberately left open is **which unprivileged Windows/local-NTFS directory-creation commit barrier establishes durable ancestor reachability and can retry an uncertain prior creation**. Task 4 specifies the native candidates, observations, official-contract review and mandatory acceptance tests needed to decide it. The regular-file publication strategy is selected and implementable now. Until the directory decision is resolved and integrated, report S01/Windows first launch as incomplete; do not silently return success, require apps to supply an already-created directory, or label a permanent Unsupported path complete.

No task claims all Windows framework services work: runtime-identity broker and runtime-fs grant transport still have independent platform work. This plan's completion is native durable/private storage plus the named real consumer paths and exact evidence, not closure of unrelated audit findings.
