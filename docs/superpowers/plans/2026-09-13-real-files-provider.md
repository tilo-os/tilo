# Real Files Grants, Operations, and Record Opening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task, with one implementation author at a time. Steps use checkbox (`- [ ]`) syntax for tracking. Continue the already-authorized slices after each review and exact-SHA host verification.

**Goal:** Run the actual `FileExplorerWidget` with explicit, revocable file/directory grants and real filesystem effects on Mac and Fedora, and discover/open an exact canonical Notes record through the common application host.

**Architecture:** Extend `runtime-fs` with typed hosted grants and a real descriptor-backed provider; keep its existing Cap'n Proto client compatible. Add narrow typed Files/record-opening inputs to the common application contract, reusing the real Files widget, model, resource operations, and search. The host owns grant admission, record owners, clipboard/drop payloads and open routing; apps never import shell or derive authority from paths.

**Tech Stack:** Existing Rust workspace, runtime-fs/runtime-portal, qos-app/ui application contract, qos-actions invocation updates, cap-std 3.4.6 and rustix 1.1.4 already present in Cargo.lock, pinned trash 5.2.6 native backend, tempfile, existing isolated nextest workflow. No FUSE requirement for ordinary local files or Notes app-record discovery; no new generic service locator or process transport.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md` §§4, 6–8, 11; `docs/superpowers/specs/2026-09-12-macos-app-capability-map.md` Files row and Notes/Files acceptance. **Authoritative framework producer:** `docs/superpowers/plans/2026-09-13-common-real-app-framework.md`. **Standalone consumer dependency:** `.superpowers/sdd/2026-09-12-macos-shell-build-foundation/standalone-host-bootstrap-plan-draft.md`; its authenticated-component-session predecessor is explicitly unimplemented, so this reference does not establish a runnable standalone binary.

## Global Constraints

- “The first milestone is **real Notes and Files, with a complete capability map for every other app**.” This plan delivers the Files slice; it does not qualify all 47 apps or complete cross-host collaboration by itself.
- “Apps and `ui` must not depend on `shell` to request a service.”
- “A capability query drives UI availability but never grants authority. Validate scope again at execution.”
- “A construction or restoration failure leaves a retryable error tile and retains the original persisted data.”
- “Headless execution remains a first-class runner of this same host contract.”
- “Benchmarks and tests must not load the user's real state or secrets.” Do not replace HOME, read real account files, touch real Trash, or discover a FUSE mount in automated tests.
- “Keep Linux defaults and HDR behavior.” Preserve Linux package launch/compositor routes and existing collaborative filesystem authorization.
- “Cross-volume moves report their copy/remove stages and do not remove a source before its destination succeeds.”
- “Multi-item operations report per-item completion, partial failure and cancellation; cancellation does not claim to undo completed effects.”
- Use the same application definition, source SHA, toolchain/lockfile and relevant gates on Mac and Fedora. Native dialogs are an explicit subsequent main-thread adapter consumer if the window API is not ready, never an implicit fallback to ambient home.

---

## Current source baseline and reconciliation

Inspected at `f142217836ffebbe950ad37513b1b3965580c45a`, with the direct core author's dirty files visible on 2026-09-13. Before execution reread those final committed files and framework API definitions; preserve the listed behavior, adapting names only where the completed dependency changed them.

| Source evidence | Concrete decision |
| --- | --- |
| `apps/file-explorer/src/files_app.rs:404,721` | `StartupInputs { initial_dir, workspace_roots, record_roots, remote_accounts, trusted_humans }` and `with_startup_inputs` already exist. These are explicit snapshots, documented as **not authorization**. Preserve their use in core-cutover fixtures; do not relabel them a complete provider. |
| `shell/src/shell/hosted_inputs.rs` | `HostedTileInputs::fixture` creates an owned temporary directory and an actual Welcome.txt. Preserve lifetime/cleanup and headless isolation. Later production preparation supplies grants; a fixture is never the qualification result. |
| `files_app.rs:766,1497,1610` | Initial load/navigation/refresh still call `read_dir_sync`; initial errors become an empty list. Production hosted construction must issue provider enumeration and retain failure separately from empty success. |
| `files_app.rs:4642` and `external_drag.rs` | Opener is thread-local; drag slot is process-global; clipboard is `Vec<PathBuf>` in the widget. Replace hosted routes with instance-bound opener and host-owned transfer handles. Preserve legacy exported APIs during migration of unmigrated callers. |
| `fileops.rs:rename,unique_destination,move_into,copy_recursive` | Reuse leaf-name validation, numeric suffix presentation and symlink-copy behavior. Current exists-then-rename can overwrite after a race; move falls back after **any** rename error. Hosted backend must use no-replace commit and fall back only for cross-device errors. Do not call existing path-based mutations after a check and call that grant enforcement. |
| `files_app.rs:start_resource_operation,poll_resource_operation` | Existing resource v2 protocol supports Accepted/progress/outcome; local mutations currently often end in empty success and polling ignores cancellation. Preserve protocol, return actual per-item outcomes, and poll cancellation. |
| `actions.rs` | Files has no replayable document-domain actions: navigation/preferences/selection are local and filesystem changes are Effects. Keep `take_committed_action` empty for these effects. |
| `model.rs:491,1308` | `WorkspaceRecord` is a UUID/path filesystem row; `Entry` contains display path and FUSE metadata. Keep file rows and add a separate app-record variant; `ui::RecordId` cannot be coerced into this UUID/path shape. |
| `model.rs:WorkspaceRecordIndexPager`; `search_provider.rs` | Reuse revision/reset/continuation and retained search behavior. The palette must never scan disks or issue RPC on each keystroke. |
| `runtime-fs/src/lib.rs`, `schema/fs.capnp` | Existing FsClient/FileHandle offer read/write/list/stat/mkdir/remove, no revocable directory grant or complete effects. Repository-wide search finds the only `workspace_fs::Server` implementation in `runtime-fs/tests/mock_fs.rs`; client comments do not establish a production proxy. Add the real in-process provider in runtime-fs, retain old wire methods unchanged. Do not use FakeFs for acceptance. |
| `runtime-portal/src/lib.rs:60` | Reuse NotAvailable for transport/service/version failures; add filesystem-specific typed errors in runtime-fs instead of encoding permission/revocation/conflict as generic successful responses. |
| `fs/src/record_access.rs`, `record_repository.rs`, `record_replication.rs` | Existing authenticated file-record authority is distinct from ordinary host directories and UI app records. Preserve its live `RecordAuthorizer` checks. No broad grant minted from a FUSE mount path or record-index entry. |
| Pinned `cap-std-3.4.6/src/fs/dir.rs`, `cap-primitives-3.4.6/src/fs/rename.rs` | Descriptor-rooted resolution exists; `Dir::from_std_file` admits an already opened directory. Ordinary cap-std rename still has replacement semantics; pair anchored parent resolution with rustix no-replace rename. |
| Pinned `rustix-1.1.4/src/fs/at.rs:285` | `renameat_with` supports Apple and Linux and maps to renameatx_np/renameat2. Use `RenameFlags::NOREPLACE`; unsupported filesystem support returns an error, never a check-then-replacing fallback. |
| Pinned `trash-5.2.6/src/macos/mod.rs` | Default mode invokes Finder via osascript. Explicit `NsFileManager` mode calls `trashItemAtURL`; it supplies no portable list/restore/purge API and does not guarantee Finder Put Back. Do not claim those capabilities. |

Pinned dependency sources were inspected locally; no claims about future AppKit panel APIs are needed by this candidate. Add direct Cargo dependencies at the already locked versions rather than upgrading them incidentally.

## Delivery boundaries and file responsibility map

1. Runtime grant authority + real reads + scope/revocation tests; independent of framework completion.
2. Runtime mutations + outcome/cancellation semantics; independent of framework completion.
3. Real trash adapters + observation + transferable grants; independent of GUI/window completion.
4. Files typed construction + real widget/resource wiring; depends on common framework Tasks 1–5.
5. Authorized canonical app-record index + exact open routing + one definition; library/runner conformance uses explicit preparation and depends on real Notes definition/record owner. Production standalone cutover additionally requires the SDK host bootstrap.
6. Concrete shell/library integration and exact-SHA semantic headless conformance; follows the final direct core module layout and requires no standalone issuer or native window walkthrough.
7. Separately gated native-window and standalone binary qualification; consumes the real native host and, for standalone, accepted authenticated component sessions plus SDK bootstrap.

Files to create or modify during **later execution**:

- `crates/runtime/runtime-fs/src/grants.rs`: opaque grant tokens, scope, rights, lease/generation validation, typed errors and provider interface.
- `crates/runtime/runtime-fs/src/native.rs`: descriptor-backed admission/read/metadata/enumeration, filesystem identity and anchored resolution.
- `crates/runtime/runtime-fs/src/operations.rs`: bounded worker and batch state machine, conflict-safe commits, cancellation, outcomes.
- `crates/runtime/runtime-fs/src/recovery.rs`: explicit durable operation/report owner, recovery identity, replay-free receipt state machine and host resolution API.
- `crates/runtime/runtime-fs/src/app_local_state.rs`: private app-local state owner, fixed keys, compare-and-swap atomic snapshots and durable acknowledgements; produced in Task 4.
- `crates/runtime/runtime-fs/src/trash.rs`: injected trash roots/staging, receipts/recovery, explicit native adapters.
- `crates/runtime/runtime-fs/src/observe.rs`: descriptor-backed change observation and bounded invalidations.
- `crates/runtime/runtime-fs/src/transfer.rs`: host-owned clipboard/drop resources and delegated leases.
- `crates/runtime/runtime-fs/src/lib.rs`, `Cargo.toml`: exports and direct locked dependencies. No changes to existing fs.capnp ordinals or ambient compatibility client in these tasks.
- `crates/runtime/runtime-fs/tests/{native_grants,native_operations,native_trash,native_observe_transfer}.rs`: real isolated filesystem integration tests; `tests/support/mod.rs` holds shared test setup only.
- `ui/src/application_files.rs`: narrow Files context/open interfaces below shell; `ui/src/application.rs`, `ui/src/lib.rs`, `crates/app/qos-app/src/lib.rs`: typed context/target extension and reexports. These paths are additions to the pending common-framework plan, not assertions of existing files.
- `ui/src/record_index.rs`, `ui/src/record.rs`: authorized paged app-record metadata index produced by canonical record owners; no Files writer. Add `ui/tests/record_index.rs` and register it in `ui/tests/it.rs`.
- `apps/file-explorer/src/hosted.rs`: typed host construction of the **existing** widget and exported ApplicationDefinition.
- `apps/file-explorer/src/files_app.rs`, `model.rs`, `search_provider.rs`, `command.rs`, `external_drag.rs`, `fileops.rs`, `config.rs`, `favorites.rs`, `templates.rs`, `lib.rs`, `main.rs`, `Cargo.toml`: narrow provider conversion/adapters; keep rendering/selection/layout/sorting and domain behavior shared.
- `apps/file-explorer/tests/hosted_provider.rs`, `hosted_record_open.rs`, existing `tests/{it,parity,drag_drop_move,double_click_open}.rs`: register new modules in `it.rs` (`autotests=false`) and preserve existing tests.
- `shell/src/shell/{hosted_inputs,host_apps,doc_open,placement,mod}.rs`: final common catalogue preparation and instance routing. Reconcile these exact files after direct cutover; do not revive deleted compositor wrappers.
- `shell/src/shell/real_files_host_tests.rs` registered under `#[cfg(test)]` in `shell/src/shell/mod.rs`: real shell catalogue/resource integration through the library. This compiles on Mac before the native binary entrypoint exists. Update the capability-map Files row with measured support and explicit remaining gaps.

## Shared runtime vocabulary

All types below are new runtime-fs additions unless noted. Public grant fields remain private, grants are not deserializable, and diagnostic paths never reconstruct a grant. The authority owns descriptors/rights/generations; a cloned grant is only a reference into that live authority. `FsSession` is a mount-scoped client; the trusted host owns `NativeGrantAuthority`. Trusted native code shares an address space: this is enforcement against accidental authority leaks, not a malicious-native-code sandbox.

```rust
// runtime_fs::grants. Uuid comes from the workspace uuid dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsOwner { pub workspace: String, pub app_id: String }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FsRight { Enumerate, Read, Create, Rename, Copy, Move, Trash, Purge }
#[derive(Clone, Debug)] pub struct DirectoryGrant { lease: GrantLease }
#[derive(Clone, Debug)] pub struct FileGrant { lease: GrantLease }
#[derive(Clone, Debug)] pub enum GrantedEntry { Directory(DirectoryGrant), File(FileGrant) }
// GrantLease is private: authority identity, slot, generation, owner and rights.
#[derive(Clone, Debug)] pub struct EntryName(std::ffi::OsString);
impl EntryName { pub fn new(value: std::ffi::OsString) -> Result<Self, FsError>; }
pub enum FsError {
    Unsupported(&'static str), Denied, Revoked, Closed, StaleEntry, StalePage,
    NotFound, InvalidName, Conflict, Busy, Cancelled, LimitExceeded, StoreUnavailable,
    Transport(runtime_portal::NotAvailable), Io(std::io::Error),
}
pub struct FsEntry { pub name: EntryName, pub grant: GrantedEntry, pub metadata: crate::Metadata }
pub struct PageCursor { generation: u64, offset: usize }
pub struct DirectoryPage { pub entries: Vec<FsEntry>, pub next: Option<PageCursor>, pub generation: u64 }
#[derive(Clone)]
pub struct FsSession { /* private shared authority, owner, mount UUID and closed flag */ }
pub struct NativeGrantAuthority { /* private owned grant table and native backend */ }
impl NativeGrantAuthority {
    pub fn new() -> Self;
    pub fn admit_directory(&self, opened: std::fs::File, owner: FsOwner,
        rights: Vec<FsRight>) -> Result<DirectoryGrant, FsError>;
    pub fn admit_file(&self, opened: std::fs::File, owner: FsOwner,
        rights: Vec<FsRight>) -> Result<FileGrant, FsError>;
    pub fn bind(&self, owner: FsOwner, mount: uuid::Uuid,
        directories: &[DirectoryGrant], files: &[FileGrant]) -> Result<FsSession, FsError>;
    pub fn revoke_directory(&self, grant: &DirectoryGrant) -> Result<(), FsError>;
    pub fn revoke_file(&self, grant: &FileGrant) -> Result<(), FsError>;
}
impl FsSession {
    // Blocking provider calls: execute these on bounded workers, never in paint/layout.
    pub fn list(&self, dir: &DirectoryGrant, cursor: Option<PageCursor>) -> Result<DirectoryPage, FsError>;
    pub fn lookup(&self, dir: &DirectoryGrant, name: &EntryName) -> Result<GrantedEntry, FsError>;
    pub fn metadata(&self, entry: &GrantedEntry) -> Result<crate::Metadata, FsError>;
    pub fn read_at(&self, file: &FileGrant, offset: u64, max_len: u32) -> Result<Vec<u8>, FsError>;
    pub fn owner(&self) -> &FsOwner;
    pub fn mount_id(&self) -> uuid::Uuid;
    pub fn check_directory(&self, grant: &DirectoryGrant) -> Result<(), FsError>;
    pub fn attach_lifetime(&self, is_alive: std::sync::Arc<dyn Fn() -> bool + Send + Sync>)
        -> Result<(), FsError>;
    pub fn close(&self);
}
```

`EntryName` accepts exactly one nonempty normal native component, rejecting NUL, `/`, `.` and `..`; preserve non-UTF8 OS bytes as identity and use lossy text **only** for labels. Directory grants may navigate descendants through provider lookup; a directly admitted file descriptor grants bytes/metadata only. Rename/move/trash of a file also need its parent authority: an isolated file grant cannot authorize sibling access. `GrantedEntry::File` covers a symlink entry with `Metadata.kind == Symlink`; reads resolve only under the original granted directory root. Absolute/out-of-root symlinks are denied; dangling links can be listed/copied/moved as link objects without dereference. Device/fifo/socket nodes return Unsupported for reads/copies.

The host opens explicitly selected/configured resources and admits owned descriptors. The trusted host admits an explicitly selected/configured resource; an app factory or restored string may not call admission. The accepted standalone CLI has zero positionals and obtains initial directories from its authenticated session descriptors, never a CLI path. Bind owners from authenticated workspace/application construction, not requested app strings from an untrusted payload. Each session has an explicit allowed-grant set: matching app/workspace alone is insufficient, so two instances may have disjoint roots. Bind rejects wrong authority/owner or revoked grants, and descendant lookup must trace to an allowed live ancestor. Host-authorized transfer delegation creates a distinct restricted operation lease rather than widening the destination session. Revocation increments the ancestor grant generation, invalidates derived grants/subscriptions and is checked before each syscall/chunk/commit. Closing a mount closes its session; reopen creates a new mount UUID and fresh session even when the durable tile ID is reused. A completed filesystem syscall cannot be undone by later revocation; report committed work and prevent subsequent work.

## Task 1: Grant-backed enumeration, bytes and revocation

**Files:** runtime-fs `grants.rs`, `native.rs`, exports/Cargo.toml, `tests/native_grants.rs`, `tests/support/mod.rs`.

**Consumes:** Existing `Metadata`, `EntryKind`, `MAX_READ_CHUNK`, `NotAvailable`; already locked descriptor libraries.

**Produces:** Exact runtime vocabulary above and honest available/unavailable capability facts.

- [ ] Add a support helper using a real owned temporary directory, explicit directory descriptor and owner. It must not modify process environment:

```rust
pub fn granted_root() -> (tempfile::TempDir, runtime_fs::NativeGrantAuthority,
    runtime_fs::FsSession, runtime_fs::DirectoryGrant) {
    let temp = tempfile::tempdir().unwrap();
    let authority = runtime_fs::NativeGrantAuthority::new();
    let owner = runtime_fs::FsOwner { workspace: "test-room".into(), app_id: "dev.qos.file-explorer".into() };
    let root = authority.admit_directory(std::fs::File::open(temp.path()).unwrap(), owner.clone(),
        vec![runtime_fs::FsRight::Enumerate, runtime_fs::FsRight::Read,
             runtime_fs::FsRight::Create, runtime_fs::FsRight::Rename,
             runtime_fs::FsRight::Copy, runtime_fs::FsRight::Move,
             runtime_fs::FsRight::Trash]).unwrap();
    let session = authority.bind(owner, uuid::Uuid::new_v4(), std::slice::from_ref(&root), &[]).unwrap();
    (temp, authority, session, root)
}
```

- [ ] Add failing `revoked_root_rejects_existing_descendant_handle`, `wrong_owner_cannot_list`, `same_owner_disjoint_sessions_cannot_use_each_others_roots`, `isolated_file_cannot_enumerate_parent`, `symlink_escape_is_denied`, `renamed_root_does_not_switch_to_replacement`, `non_utf8_names_remain_distinct`, `missing_is_not_empty` tests. Core revocation assertion:

```rust
let (temp, authority, session, root) = support::granted_root();
std::fs::write(temp.path().join("draft"), b"before").unwrap();
let file = match session.lookup(&root, &runtime_fs::EntryName::new("draft".into()).unwrap()).unwrap() {
    runtime_fs::GrantedEntry::File(file) => file,
    _ => panic!("expected file"),
};
assert_eq!(session.read_at(&file, 0, 16).unwrap(), b"before");
authority.revoke_directory(&root).unwrap();
assert!(matches!(session.read_at(&file, 0, 16), Err(runtime_fs::FsError::Revoked)));
assert_eq!(std::fs::read(temp.path().join("draft")).unwrap(), b"before");
```

- [ ] Run focused failing tests with `cargo nextest run --locked --profile ci -p runtime-fs --test native_grants`; require failure for absent implementation, not setup/toolchain failure.
- [ ] Implement table-backed private grant leases, rights subsets, ancestor revocation and mount closure. Convert std opened directories with `cap_std::fs::Dir::from_std_file`; resolve below the pinned root rather than canonicalize-then-use absolute paths. Record native object identity for entries; stale action rows cannot silently address replacement objects.
- [ ] Implement directory snapshots at at most 256 rows/page with explicit revision. Build/retain at most a bounded snapshot budget; if a directory exceeds the budget, use a bounded continuation scan or return explicit Busy/refresh state, never an apparently complete truncated listing. Revalidate cursor/root before returning each page. Metadata/read failures stay typed. Limit byte chunks to MAX_READ_CHUNK.
- [ ] Test in-root relative symlink, escaping relative symlink, absolute symlink, dangling link, parent symlink swapped before lookup, root rename/replacement, absent file, revoked descendant, read-only owner, session close and fresh reopen. No test reads the real HOME or relies on chmod failure under a privileged test runner.
- [ ] Run Task 1 native checks/tests below, inspect diff, commit `feat(runtime-fs): add scoped native file and directory grants`; controller verifies exact SHA on Fedora before accepting.

## Task 2: Real mutation jobs, conflicts, partial outcomes and cancellation

**Files:** runtime-fs `operations.rs`, `recovery.rs`, `native.rs`, `grants.rs`, `tests/native_operations.rs`; extract only pure name/suffix logic from Files `fileops.rs` if useful, keeping localization in Files.

**Consumes:** Task 1 grants/rights/anchored resolution.

**Produces:** These typed operations; UI maps them to existing qos-actions protocol in Task 4.

```rust
pub enum ConflictPolicy { Fail, KeepBoth }
pub enum FsOperation {
    CreateDirectory { parent: DirectoryGrant, name: EntryName, conflict: ConflictPolicy },
    CreateFile { parent: DirectoryGrant, name: EntryName, bytes: Vec<u8>, conflict: ConflictPolicy },
    Rename { entry: GrantedEntry, name: EntryName },
    Copy { entries: Vec<GrantedEntry>, into: DirectoryGrant, conflict: ConflictPolicy },
    Move { entries: Vec<GrantedEntry>, into: DirectoryGrant, conflict: ConflictPolicy },
    Trash { entries: Vec<GrantedEntry> },
}
pub enum ItemState { Completed, Failed, Cancelled, CopiedSourceRetained, RecoveryRequired, OutcomeUnknown }
pub struct ItemOutcome {
    pub item: usize, pub state: ItemState, pub destination: Option<GrantedEntry>,
    pub error: Option<FsError>, pub recoveries: Vec<RecoveryId>,
}
pub enum FsUpdate {
    Progress { sequence: u64, item: usize, phase: &'static str, bytes: u64 },
    Item(ItemOutcome),
    Finished { completed: usize, failed: usize, cancelled: usize },
}
pub struct FsJob { /* private bounded control/results queue and terminal report */ }
impl FsSession { pub fn start(&self, operation: FsOperation) -> Result<FsJob, FsError>; }
impl FsJob {
    pub fn operation_id(&self) -> OperationId;
    pub fn poll(&mut self) -> Result<Vec<FsUpdate>, FsError>;
    pub fn request_cancel(&self);
}
```

### Operation and recovery owner produced in this task

```rust
// runtime_fs::recovery. IDs have private fields; only the host store mints them.
#[derive(Clone, Debug, Eq, PartialEq)] pub struct OperationId(uuid::Uuid);
#[derive(Clone, Debug, Eq, PartialEq)] pub struct RecoveryId(uuid::Uuid);
pub enum RecoveryKind { KnownStaged, UnknownNativeCommit, PublishedSourceRetained }
pub struct RecoveryInfo {
    pub id: RecoveryId, pub operation: OperationId, pub kind: RecoveryKind,
    pub original_name: std::ffi::OsString, pub acknowledged: bool,
}
pub enum OperationState { Running, Finished, RecoveryRequired, OutcomeUnknown }
pub struct RecordedItemOutcome {
    pub item: usize, pub state: ItemState, pub recoveries: Vec<RecoveryId>,
    pub error_code: Option<String>, pub message: Option<String>,
}
pub struct OperationReport {
    pub id: OperationId, pub owner: FsOwner, pub originating_mount: uuid::Uuid,
    pub state: OperationState, pub items: Vec<RecordedItemOutcome>,
}
pub struct OperationOwner { /* explicit owned directory, writer lock, durable receipts and jobs */ }
pub enum RecoveryResolution {
    RestoreKnown { parent: DirectoryGrant, name: EntryName },
    // Records the human's uncertainty acknowledgement; never claims success or mutates files.
    AcknowledgeUnknown { note: String },
}
impl OperationOwner {
    pub fn open(opened_storage: std::fs::File, owner: FsOwner) -> Result<std::sync::Arc<Self>, FsError>;
    pub fn report(&self, id: &OperationId) -> Result<OperationReport, FsError>;
    pub fn recoveries(&self) -> Result<Vec<RecoveryInfo>, FsError>;
    pub fn resolve(&self, id: &RecoveryId, current: &FsSession,
        resolution: RecoveryResolution) -> Result<FsJob, FsError>;
    pub fn acknowledge_report(&self, id: &OperationId) -> Result<(), FsError>;
}
impl NativeGrantAuthority {
    pub fn attach_operations(&self, owner: std::sync::Arc<OperationOwner>) -> Result<(), FsError>;
}
```

`OperationOwner` is retained by the trusted host's concrete Files service owner, not exposed as a general app storage client. `attach_operations` binds its private namespace to FsOwner once; `FsSession::start` returns StoreUnavailable before effects if its owner lacks this store. Each FsJob retains the same owner and its OperationId; widget drop never deletes the report. Job polling exposes only that job; host recovery/report queries operate on its explicitly scoped owner. Grant-bearing runtime destinations are not serialized: durable reports hold metadata and RecoveryId, not recreated path grants. Host owns native locators privately, opens/revalidates stored object identities against freshly admitted roots on restart, and cannot regain access from a receipt path alone.

Store at most 4,096 unacknowledged/active reports, 64 KiB metadata per report and 64 MiB aggregate metadata; reserve room before admitting an effect, returning Busy/LimitExceeded before modification. Retain unresolved recovery receipts indefinitely until explicit resolution; never evict them for TTL. Acknowledged resolved reports may be pruned after seven days. Staged **user bytes** are separate from these metadata limits and never garbage-collected because metadata quota is full. Retain staging locations, lengths and content/object identity in bounded receipts, not their payload bytes in the metadata file.

Persist an Admitted receipt before the first effect and persist each transition before its associated irreversible boundary: Staged, PublishStarted/Published, SourceRemovalStarted/Completed, and NativeTrashSubmitted/Completed. fsync journal data and containing directory before advertising that checkpoint. A failed checkpoint stops subsequent effects and retains the last known phase; do not silently continue without recovery storage. Restart is reconciliation only: verify private staged entry/identity under reauthorized descriptors; known bytes become KnownStaged, published destination with source retained becomes PublishedSourceRetained, and ambiguous native submission becomes UnknownNativeCommit. No move source removal, trash submission, overwrite or purge is automatically replayed.

`resolve(RestoreKnown)` rechecks current session/owner/read and destination Create authority, matches the exact RecoveryId's staged identity and performs a new journaled no-replace restore. Missing/replaced stage or revoked rights yields a typed failure and retains the receipt. `AcknowledgeUnknown` applies only to UnknownNativeCommit, stores a bounded 1,024-byte note, leaves outcome unknown and performs no filesystem mutation. It is not a retry of native trash. PublishedSourceRetained is resolved by a fresh explicitly authorized user operation if desired; no recovery API silently removes its source. `acknowledge_report` refuses an active job or unresolved known staging. Close/reopen tests query `OperationOwner::report` after all widget/job handles are dropped; host shutdown keeps failed reports and refuses to claim all effects settled.

- [ ] Add `recovery_storage_failure_prevents_first_effect`, `closed_widget_report_remains_queryable`, `restart_known_stage_requires_new_grant`, `uncertain_native_trash_is_not_replayed`, and `recovery_quota_never_deletes_staged_user_bytes`. Build the OperationOwner with an explicitly opened temp directory and inspect actual receipt bytes and staged files. Use module-private failure injection only to stop at named checkpoints. Test no-replace restoration collision and retry after removing the conflict without another destructive action.

- [ ] Add failing real-disk tests named `rename_conflict_preserves_both_bytes`, `copy_tree_keeps_source`, `copy_preserves_dangling_link`, `move_same_directory_is_not_copy`, `recursive_destination_rejected_before_creation`, `batch_reports_success_and_missing_item`, `cancel_after_first_item_retains_committed_item`, `revocation_during_copy_does_not_remove_source`, `cross_device_copy_remove_stages`, `non_exdev_rename_failure_never_copies`, `remove_failure_reports_copied_source_retained`. Assert exact bytes, source existence, destination existence and update ordering, not only returned strings.
- [ ] In each operation test, open a second explicit temporary metadata directory, call `OperationOwner::open(File::open(metadata_dir)?, session.owner().clone())`, and attach that Arc to the authority before FsSession::start. Keep the metadata TempDir and Arc owner for report/restart assertions. The negative missing-store test intentionally omits attachment and requires StoreUnavailable with unchanged source/destination.
- [ ] Run `cargo nextest run --locked --profile ci -p runtime-fs --test native_operations` and confirm meaningful failure.
- [ ] Implement a bounded worker admission queue (32 jobs), bounded item count (256/request), 1 MiB copy chunks, and at most one mutation job active per Files session. Coalesce replaceable progress; never drop Item or Finished. Preserve final outcome until consumed. Exhaustion returns Busy before admitting work. Cancellation is checked before each item/chunk and before publish/source removal; close requests cancellation but leaves terminal effects observable by the host operation owner.
- [ ] Use anchored parent descriptors and `rustix::fs::renameat_with(..., RenameFlags::NOREPLACE)` for publication. Rename always refuses collisions. KeepBoth must be a selected policy shown by Files (matching existing “ 2” naming), uses exclusive attempts for each generated candidate and reports the actual destination; bound attempts and return Conflict at exhaustion. Do not silently select overwrite, even on retries or dangling-link collisions.
- [ ] Copy directory/file/link into an owned staging entry on the destination filesystem. Stream ordinary bytes, preserve link contents rather than follow them, close/flush destination before publishing, and report failure without publishing a complete-looking destination. Remove only the provider's own staging artifacts; do not clean up preexisting names. Refuse copying a directory into itself/descendants using object ancestry under grants, including symlink aliases.
- [ ] Move uses no-replace rename on the same filesystem. Only EXDEV selects copy → publish → remove-source. Recheck source identity before removal; if replaced/revoked or removal fails, retain source and destination and return CopiedSourceRetained. Emit phases `copy`, `publish`, `remove-source`. A cancelled/failed operation reports existing committed destinations and never promises rollback. A retry is a new user decision; it must not blindly repeat a completed side effect.
- [ ] Keep syscall-bound fault injection private under cfg(test) in `operations.rs::tests`; integration-test binaries cannot access a dependency's cfg(test) items. Put the forced EXDEV/EIO/removal/barrier cases in those unit tests and keep ordinary real filesystem cases in `tests/native_operations.rs`. Run `cargo nextest run --locked --profile ci -p runtime-fs --lib -E 'test(operations::tests)'` for these additional cases. Inject only at syscall boundaries: force EXDEV before rename, an EIO after one real chunk, failure just before source removal, or a barrier before item two. This supplements real source/destination files and byte assertions; it is not a fake production backend. Add one real cross-volume native check when two disposable volumes exist, record explicit prerequisite absence separately.
- [ ] Write every ItemOutcome (including its RecoveryId list) into the OperationOwner before exposing a terminal report. FsJob::operation_id is the host query key after widget closure. Use the exact recovery state machine and storage bounds above; no second ephemeral result cache is the sole outcome owner. Test that polling a closed UI is unnecessary for durable report completion.
- [ ] Run Task 2 gates, commit `feat(runtime-fs): implement grant-backed file mutation jobs`; controller exact-SHA Fedora verification.

## Task 3: Recoverable trash, change observation and host-owned transfers

**Files:** runtime-fs `trash.rs`, `observe.rs`, `transfer.rs`, tests `native_trash.rs`, `native_observe_transfer.rs`, exports/dependencies.

**Consumes:** Task 1 authority and Task 2 worker/outcome/anchored publication.

**Produces:** Concrete trash availability, directory invalidations and clipboard/drop ownership. No automatic native dialog or external-app launch.

```rust
pub enum ObservationMode { Polling { interval_ms: u64 }, RefreshOnActivation }
pub enum DirectoryChange { Invalidated { generation: u64 }, Revoked, Unavailable }
pub struct DirectorySubscription { /* private bounded coalescing channel */ }
impl FsSession {
    pub fn observe(&self, directory: &DirectoryGrant) -> Result<DirectorySubscription, FsError>;
    pub fn observation_mode(&self, directory: &DirectoryGrant) -> Result<ObservationMode, FsError>;
}
impl DirectorySubscription { pub fn poll(&mut self) -> Option<DirectoryChange>; }
#[derive(Clone, Copy, Debug, Eq, PartialEq)] pub enum TransferIntent { Copy, Cut }
#[derive(Clone, Debug, Eq, PartialEq)] pub struct TransferId(uuid::Uuid);
#[derive(Clone, Debug, Eq, PartialEq)] pub struct GrantIdentity { /* authority/slot/generation, not authority */ }
pub struct TransferRequest {
    pub transfer: TransferId, pub source: FsOwner, pub recipient: FsOwner,
    pub recipient_mount: uuid::Uuid, pub destination: GrantIdentity,
    pub sources: Vec<GrantIdentity>, pub intent: TransferIntent,
}
pub trait TransferAuthorizer: Send + Sync {
    // Trusted host checks its live policy; strings in TransferRequest are bound facts, not proof.
    fn authorize(&self, request: &TransferRequest) -> Result<(), FsError>;
}
pub struct FileTransfers { /* host-owned payloads, live authorizer and private bound permits */ }
#[derive(Clone)] pub struct FileTransferClient { /* one bound FsSession; no authorizer/admission setters */ }
impl FileTransfers {
    pub fn new(authority: std::sync::Arc<NativeGrantAuthority>,
        authorizer: std::sync::Arc<dyn TransferAuthorizer>) -> Self;
    pub fn client(&self, session: &FsSession) -> Result<FileTransferClient, FsError>;
    pub fn revoke(&self, id: &TransferId) -> Result<(), FsError>;
}
impl FileTransferClient {
    pub fn is_bound_to(&self, session: &FsSession) -> bool;
    pub fn capture(&self, entries: Vec<GrantedEntry>, intent: TransferIntent)
        -> Result<TransferId, FsError>;
    pub fn paste(&self, id: &TransferId, into: DirectoryGrant,
        conflict: ConflictPolicy) -> Result<FsJob, FsError>;
}
```

- [ ] Add failing tests `trash_is_recoverable_not_permanent_delete`, `trash_unavailable_keeps_source`, `trash_failure_after_staging_exposes_recovery`, `trash_restore_refuses_collision`, `purge_requires_separate_authority`, `external_edit_invalidates_current_directory`, `stale_page_is_not_published_after_navigation`, `closing_observer_stops_delivery`, `copy_payload_survives_source_mount_close`, `cut_partial_success_retains_only_unmoved_items`, `revoked_clipboard_fails_without_source_loss`. Automated trash tests use explicit temporary files/info/staging directories and the real same-volume filesystem implementation; never the logged-in user's native Trash.
- [ ] Run `cargo nextest run --locked --profile ci -p runtime-fs --test native_trash --test native_observe_transfer`; confirm failure.
- [ ] Add exact **per-volume** registration, not a singleton claiming multiple volumes:

```rust
// Opaque IDs are derived from native fstat in this authority session, not caller strings.
#[derive(Clone, Debug, Eq, PartialEq)] pub struct NativeVolumeId { /* session + native device identity */ }
pub enum TrashConfiguration {
    Freedesktop { files: std::fs::File, info: std::fs::File, staging: std::fs::File },
    MacNative { staging: std::fs::File, trusted_staging_url: std::path::PathBuf },
}
impl NativeGrantAuthority {
    pub fn volume_of(&self, directory: &DirectoryGrant) -> Result<NativeVolumeId, FsError>;
    pub fn configure_trash(&self, volume: NativeVolumeId, configuration: TrashConfiguration)
        -> Result<(), FsError>;
}
```

The trusted host opens/adopts descriptors and chooses one configuration for each actual native volume. Derive the configuration descriptors' volume from fstat and reject mismatch or a duplicate registration. Volume IDs are rederived/revalidated each host startup, never trusted from persisted numeric device IDs. Missing configuration is Unsupported before staging. Reject non-directory/unsafe staging roots and require private host ownership. App FsSession clients cannot configure adapters; capabilities distinguish trash/list/restore/purge. A Freedesktop configuration is an actual directory-trash provider used on Linux; exercising it on Mac with temporary files is a **test adapter**, not native Mac Trash qualification.
- [ ] Allocate a private mode-0700 `staging/<operation-id>/` parent on the source volume. Keep the selected entry's exact native leaf name inside it (`staging/<operation-id>/original-name`), including extension/non-UTF8 bytes; never send an opaque operation UUID as the leaf name to native Trash. Obtain parent descriptors, persist the Admitted receipt containing original parent identity/name, grant generation, source object identity, volume and private staging locator, and fsync it before rename. Perform anchored no-replace rename into the private parent; verify staged identity; persist/fsync Staged. Then persist/fsync NativeTrashSubmitted before the native call, and Completed only after native success. Paths in receipts are private locators, not authority. If a rename/call/checkpoint fails, use the Task 2 OperationOwner RecoveryId and no-replace restore; a restoration conflict retains KnownStaged and its bytes. On restart a matching present stage is KnownStaged; an absent stage after NativeTrashSubmitted with no Completed acknowledgement is UnknownNativeCommit. Never infer native success from source absence or resubmit the uncertain call.
- [ ] The Freedesktop adapter writes real `.trashinfo` and publishes actual files under the explicitly injected files/info directories; record original location and time. Do not infer HOME. Advertise native Linux trash only for each volume that has a validated registered configuration; unconfigured volumes fail before staging. Restore needs a reauthorized original parent; purge needs separate host authorization and existing irreversible-confirmation flow. “Empty Trash” is limited to the host's explicitly granted trash scope, never an implicit purge of all user trash.
- [ ] The Mac adapter explicitly selects `trash::macos::DeleteMethod::NsFileManager` on a `TrashContext`, then deletes one staged item per reported outcome. Do not invoke Finder/osascript, claim an automation permission was granted, or equate a queue acceptance with completion. The pinned adapter does not expose a recoverable resulting URL/Put Back contract: mark native list/restore/purge unavailable and surface explanatory disabled actions. Keep native trash separate from permanent deletion. A crash after native success but before receipt completion is `outcome unknown`, retained for host recovery; never report source deletion failure as successful rollback.
- [ ] Implement an anchored polling observer (no native-event claim): every two seconds, one bounded page per worker turn computes directory membership and metadata signatures; after a complete cycle emit one revision invalidation. Subscribe to the currently viewed directory grant; navigation changes the subscription. No recursive walk through links. Revocation closes the subscription; unreadable roots emit Unavailable and move to explicit refresh-on-activation mode. User Refresh and host activation both re-enumerate. Tag replies with grant generation, navigation generation and mount UUID; discard stale pages and old callbacks before touching UI.
- [ ] FileTransfers retains its own host-owned transfer leases, independent of source mount closure and dependent on the underlying grants. Each host broker stores at most 32 payloads, at most 256 entries per payload, with a 120-second monotonic TTL; capture over quota returns Busy, oversized input returns LimitExceeded. Replacing/expiring a clipboard releases that payload, never moves/deletes a source. In-flight leases stay owned for report completion but no new chunk/commit may proceed after TTL/revocation; cancellation reports partial outcomes.
- [ ] The trusted shell constructs FileTransfers with its **actual live** TransferAuthorizer. `client(session)` verifies the authority and binds the client to that exact mount and allowed roots. Capture validates source grants, scope and requested intent. Retain an authority-owned restricted transfer lease, not a clone of the original FsSession/lifetime callback; source mount close alone therefore cannot revoke the payload. Underlying grant revocation still invalidates this derived lease. Paste builds TransferRequest from the stored payload and bound recipient/destination (never caller-provided owner strings), calls the authorizer and privately mints a permit bound to transfer generation, exact source identities, recipient/mount, destination identity/generation and Copy/Cut intent. Every actual chunk/publish/remove-source rechecks that same request against the live authorizer plus TTL, explicit revoke state and grant generations. Cross-owner and cross-workspace transfers require an affirmative current host decision; matching app/workspace strings alone do not admit them. A FileTransferClient cannot implement/swap the authorizer or mint a permit.
- [ ] Task 6's in-process authorizer reads the trusted shell's current workspace membership and app/root transfer policy through an Arc-backed policy owner updated by the shell; it returns Denied when a concrete delegation is absent. The future standalone route must adapt its accepted authenticated session decision door, and remains blocked until that producer exists. For isolated tests, install an explicit exact-request allow-set controlled by the test host (initially empty), then revoke it between copy and commit; do not use a production allow-all fallback.
- [ ] Keep outcome consumption inside the host-owned operation observer: remove each cut item only after its ItemState::Completed is durably reported. CopiedSourceRetained/error/cancel retain that item. The widget's polling cannot be required to update the host clipboard. Add `cross_workspace_needs_live_host_decision`, `delegation_cannot_change_destination_or_intent`, `delegation_revoked_before_commit_keeps_source`, `transfer_expiry_after_source_close`, and exact payload/TTL boundary tests. Native source mount closure is not grant revocation, while explicit underlying grant revocation still denies paste.
- [ ] Native URL drops must pass through trusted grant admission before capture; internal drag payloads use TransferId. Export to an external application remains an explicit host capability; unavailable export fails visibly and does not mark the internal move completed.
- [ ] Run Task 3 gates, commit `feat(runtime-fs): add scoped trash observation and file transfers`; controller exact-SHA Fedora verification. Actual Mac native-trash and real cross-volume checks remain named native qualification gates, not replaced by injected-backend success.

## Task 4: Wire the real Files widget through explicit typed inputs

**Files:** runtime-fs `app_local_state.rs`, `tests/app_local_state.rs`; `ui/src/application_files.rs`, application context/reexports; Files `hosted.rs`, files_app/model/command/config/favorites/templates/external_drag/fileops/lib; `tests/hosted_provider.rs` registered in `tests/it.rs`.

**Consumes:** Common framework ApplicationDefinition/HostContext/AppComponent/LaunchTarget/RestoreInput/WakeScope; Tasks 1–3 runtime provider.

**Produces:** `FileExplorerWidget::from_host(inputs: HostedFilesInputs) -> Result<Self, ui::launch::LaunchError>` and common typed file context. There is one real widget implementation.

### Fixed-key app-local atomic state producer

No general filesystem Write/replace right is added. Favorites and zoom stay **device-local**, as required by `actions.rs` and config/favorites source, rather than becoming collaborative document fields. Selection/navigation/caret stay in the common local View. Templates/downloads create a new granted file through Task 2 CreateFile/Copy with explicit conflict policy; they do not replace existing arbitrary files. Bound CreateFile to 16 MiB and return LimitExceeded above it; ordinary large file copies stream. An absent large network-download adapter is visibly unavailable.

```rust
// runtime_fs::app_local_state. Only these fixed keys exist; there is no path parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)] pub enum AppStateKey { Favorites, Zoom }
#[derive(Clone, Copy, Debug, Eq, PartialEq)] pub struct StateRevision(u64);
#[derive(Clone, Debug, Eq, PartialEq)] pub struct StateSaveId(uuid::Uuid);
pub struct AppStateNamespace { pub owner: FsOwner, pub device_id: String }
pub struct AppStateSnapshot { pub revision: StateRevision, pub bytes: Vec<u8> }
pub enum StateSaveStatus {
    Pending,
    Durable { revision: StateRevision },
    Failed { code: String, message: String },
}
pub struct StateShutdownReport { pub unsettled: Vec<StateSaveId> }
pub struct AppLocalStateOwner { /* owned private directory, exclusive writer, retained saves */ }
#[derive(Clone)] pub struct AppLocalStateClient { /* fixed namespace, retained owner, no raw path access */ }
impl AppLocalStateOwner {
    pub fn open(opened: std::fs::File, namespace: AppStateNamespace)
        -> Result<std::sync::Arc<Self>, FsError>;
    pub fn client(self: &std::sync::Arc<Self>) -> AppLocalStateClient;
    pub fn pending(&self) -> Vec<StateSaveId>;
    pub fn shutdown_report(&self) -> StateShutdownReport;
}
impl AppLocalStateClient {
    pub fn namespace(&self) -> &AppStateNamespace;
    pub fn load(&self, key: AppStateKey) -> Result<Option<AppStateSnapshot>, FsError>;
    pub fn submit(&self, key: AppStateKey, expected: Option<StateRevision>, bytes: Vec<u8>)
        -> Result<StateSaveId, FsError>;
    pub fn status(&self, id: &StateSaveId) -> Result<StateSaveStatus, FsError>;
    pub fn retry(&self, id: &StateSaveId) -> Result<(), FsError>;
    pub fn discard_failed(&self, id: &StateSaveId) -> Result<(), FsError>;
}
```

The trusted host opens one explicit device/app/workspace private directory, validates its namespace header, claims an exclusive writer lock and retains the Arc owner independently of widgets. Empty newly provisioned directory may initialize its namespace; corrupt/mismatched existing header or contested writer fails without replacing bytes. Two Files instances share this owner. Submit bounds blobs to 1 MiB, unsettled saves to 32, and retained bytes to 32 MiB; over-limit submission fails before acceptance. The owner retains exact desired bytes, expected revision and save ID, never a callback into a widget.

The worker writes a private pending intent plus bytes using exclusive temp creation, file sync, atomic rename and parent sync. Under the exclusive owner it compare-and-swap checks the expected fixed-key revision, writes a complete new snapshot with the same durable sequence, then durably settles the intent. The key-to-filename map is fixed in this module. Missing key is None; corrupt/unknown-version data is an error, not default state. Stale expected revision returns Conflict, preserving current snapshot and desired bytes. Initial publication is no-replace; replacing an existing **private state snapshot** occurs only after this exclusive owner's successful revision check. Reuse atomic-file's durable ordering against the private anchored directory; arbitrary paths never enter the update API.

Only the completed snapshot+directory acknowledgement yields Durable. Queued or intent-only work stays Pending; failure is sticky and retains exact bytes. Retry(id) retries that complete snapshot without a new edit, treats an already published matching save ID as idempotent, and refuses any different intervening revision. Before intent persistence, process crash can lose an unacknowledged request: it must never be labelled Saved. Widget close cannot lose it because the host retains the owner. Before host termination, shutdown_report must be empty; if desired bytes could not be journaled/saved, retain the retry surface and do not claim successful safe shutdown. Restart loads durable pending intents for explicit retry and never silently overwrites newer snapshots. `discard_failed(id)` is permitted only for a settled Failed save after an explicit Keep Saved user choice; durably acknowledge/remove its pending intent, preserve current snapshot bytes, and never discard a running request. Retry and Keep Saved are actual recovery actions, not automatic eviction when quotas fill.

- [ ] Add failing `existing_zoom_update_waits_for_durable_ack`, `favorites_update_is_not_general_path_write`, `state_save_survives_widget_close`, `failed_save_retries_without_new_edit`, `concurrent_stale_revision_preserves_newer_bytes`, `pending_intent_restart_recovers_exact_bytes`, and `state_quota_rejects_before_acceptance`. Inspect actual fixed-key files/versions. Injected post-intent/pre-publication/post-publication barrier cases live in `app_local_state.rs::tests`, not integration code that cannot access cfg(test) hooks.
- [ ] Run `cargo nextest run --locked --profile ci -p runtime-fs --test app_local_state` and `cargo nextest run --locked --profile ci -p runtime-fs --lib -E 'test(app_local_state::tests)'`; implement the exact owner/client protocol and rerun. Extract pure favorite/zoom encoding/decoding from current helpers; hosted code never invokes their swallowed-error saves.

### Exact FilesContext and common View lifetime consumption

```rust
// ui::application_files, Task 4. Task 5 adds optional record/open clients.
#[derive(Clone)] pub struct GrantedPlace { pub label: String, pub directory: runtime_fs::DirectoryGrant }
#[derive(Clone)] pub struct FilesContext { /* private validated local provider/client fields */ }
impl FilesContext {
    pub fn new(session: runtime_fs::FsSession, initial: runtime_fs::DirectoryGrant,
        roots: Vec<GrantedPlace>, state: runtime_fs::AppLocalStateClient,
        transfers: runtime_fs::FileTransferClient) -> Result<Self, crate::launch::LaunchError>;
    pub fn session(&self) -> &runtime_fs::FsSession;
    pub fn initial(&self) -> &runtime_fs::DirectoryGrant;
    pub fn roots(&self) -> &[GrantedPlace];
    pub fn state(&self) -> &runtime_fs::AppLocalStateClient;
    pub fn transfers(&self) -> &runtime_fs::FileTransferClient;
}
impl crate::application::HostContext {
    pub fn with_files(self, files: FilesContext) -> Result<Self, crate::launch::LaunchError>;
    pub fn files(&self) -> Result<&FilesContext, crate::launch::LaunchError>;
}
```

`with_files` is once-only before instantiate and checks FsSession::owner and AppLocalStateClient::namespace against the context's authenticated app/workspace/device. It attaches the exact context WakeHandle with `session.attach_lifetime(Arc::new(move || wake.is_active()))`; that attachment is once-only and rejects a session already attached to another component. Existing WakeScope has no UUID getter: the fresh runtime session mount UUID is a correlation identity, not a claim of equality with a WakeScope token. Close makes both the callback and session admission fail; a replacement component needs a new bound session. Tests using runtime-fs without ui close their explicit sessions directly. `FilesContext::new` uses check_directory and is_bound_to to validate its roots/transfer client without filesystem I/O. Read-only accessors supply exact typed fields; factory clones these handles into HostedFilesInputs while AppComponent retains the original context, so both share one lifetime rather than rebinding a second session. Missing required providers use new `LaunchError::RequiredProvider(&'static str)`. Record catalogue/opener are explicitly unavailable in Task 4 and become real optional clients in Task 5; Task 4 does not depend on yet undefined Task 5 types.

Consume authoritative `HostContext::prepare(definition, records, Rc<InstanceViewStore>, device_id, instance_id, permissions, author, action_context, &target)`. Preparation makes the explicit View store once, validates namespace and owns it in context. Invoke `AppComponent::close()` / `suspend()` with **no store argument**. Host retains `ComponentCloseReport { recovery, view_error, view_retry, record_error, record_retry, teardown_error }` independently of the widget. Its `retry_view()` uses the exact retained store/envelope; it never rediscovers a profile or reopens a mount. Keep OperationOwner/AppLocalStateOwner reports alongside this framework report in concrete Files service-owner fields; neither is a second View store.

- [ ] Add `files_preparer_local_view_store_survives_close`, `files_failed_view_retry_uses_original_namespace`, and `close_retains_both_view_and_file_operation_failures`. Drop preparer-local store handles before close, fail save, drop the component and retry through ComponentCloseReport. Assert only the originally supplied namespace changes and a replacement mount receives no stale wake.
- [ ] Implement the typed context/SDK reexports and add `LaunchTarget::Directory(DirectoryGrant)` / `LaunchTarget::File(FileGrant)` with no PathBuf variant. Files' framework record binding is separate from its directory and opened Notes record. Until Task 5, open/catalogue requests return RequiredProvider before effects; no thread-local or external fallback.
- [ ] Define app-local `HostedFilesInputs { files: ui::application_files::FilesContext, wake: ui::WakeHandle, presentation: StartupInputs }`; construct presentation labels from grants/host snapshots. Keep StartupInputs fields as display/migration data. Do not use presentation.initial_dir/record_roots to issue I/O on the hosted route. Add explicit injected remote-service state `Unavailable(reason)` or an existing connected typed client; a vector of account labels/health is not permission to read credentials or connect.
- [ ] Add failing tests constructing two actual widgets with different temporary grants, different state clients and different mount scopes; prove listing/rename/paste cannot cross owners, initial read errors remain visible, navigation cannot escape via pathbar/breadcrumb/favorite payloads, closing one does not change the other's service owner, and future activation refreshes external edits. Add a real headless resource invocation test verifying files.rename changes disk, emits one effect result and changes no Automerge domain fields.
- [ ] Run `cargo nextest run --locked --profile ci -p file-explorer --test it -E 'test(hosted_provider)'`; require nonzero tests and expected failing assertions.
- [ ] Refactor the existing constructor's common field initialization once. Hosted construction starts with pending/empty/error state from a provider job, not `read_dir_sync(...).unwrap_or_default()`. Legacy `new/with_persistence_dirs/with_startup_inputs` remain compatibility adapters until standalone and shell migrate; they do not prove production grants. Keep `NavShell`, toolbar, row/grid widgets, selection, locale formatting, sorting and status UI intact.
- [ ] Retain display `Entry` fields, adding a separate runtime mapping from resource identity to GrantedEntry. Resolve each selected entry again at dispatch; raw action payload paths never mint grants. Directory navigation/history stores granted directory references plus display paths; Up stops at the grant root. Config/favorites use AppLocalStateClient fixed-key snapshots and save IDs; templates create new files through granted Copy/CreateFile and explicit conflicts; thumbnail generation consumes provider-read bytes or an explicitly granted private cache staging resource. Until a thumbnail path adapter is complete, show the existing honest badge, not ambient reads of the display path.
- [ ] Route toolbar, inline rename, keyboard, context menus, pointer drag/drop and semantic actions into the same provider operation helper. New-folder/rename/copy/move/trash/template-save and downloaded-byte publication all use this helper. Preserve existing confirmation for purge/empty-trash. Disable unsupported capabilities with a reason and recheck on execution. Do not advertise unavailable DAV/history/share controls as working; keep existing Linux service route only when explicitly bound to its real authority.
- [ ] Keep preference/config/template save status pending until the actual asynchronous atomic-write/durable acknowledgement arrives. A queued request or absence of a cached error is not Saved. Retain dirty state and typed failure for retry, including a failure received after the widget closes; retry without a new edit attempts the exact retained desired snapshot and acknowledges actual durable success; an intervening revision remains Conflict until an explicit recovery choice. Use the common record owner's actual durable status for record-backed state, never a second Files writer. Add `save_failure_remains_dirty_and_retry_acknowledges` against an isolated real invalid destination.
- [ ] Replace hosted clipboard vectors and global drag calls with FileTransferClient. Remove hosted thread-local DocumentOpener fallback; until Task 5 provides OpenTargetClient, opening returns explicit RequiredProvider. Retain the compatibility exports temporarily for unmigrated Linux package callers; production Files never falls through to thread-local routing, `open::that`, subprocess terminal launch, accounts::load/find, DAV credential discovery or FUSE xattr discovery.
- [ ] Bridge FsJob updates into existing `ResourceOperationStart::Accepted` and `qos_actions::InvocationUpdate`: Progress retains phases/counters; Item serializes per-item state/resources; Finished yields Succeeded only when every requested effect completed, otherwise Failed with structured summary or Cancelled after all item outcomes. Surface copied-source-retained and recovery-required distinctly. Handle `_cancellation_requested` and retain operation owner/recovery reports after mount close; closed WakeHandle cannot publish UI updates into a replacement tile.
- [ ] Audit direct I/O with `rg -n 'std::fs|read_dir_sync|fileops::|xattr::|accounts::|DOCUMENT_OPENER|open::|std::process' apps/file-explorer/src`; classify every production-hosted reachable call as provider-backed, explicit metadata projection or disabled capability. Fixture/legacy code may remain, but no active hosted bypass. Add targeted tests for actual calls, not a brittle grep-only pass.
- [ ] Run Task 4 gates and existing app resource/parity/drag/drop regressions, commit `feat(files): consume explicit host file services in the real widget`; controller exact-SHA Fedora verification.

## Task 5: Authorized app-record index and exact Notes open intent

**Files:** `ui/src/record_index.rs`, record/application_files/application reexports; Files `model.rs`, search_provider/files_app/hosted/lib; shell host_apps/doc_open/placement; Files `tests/hosted_record_open.rs`, UI tests registration.

**Consumes:** Canonical `RecordOwners`, `RecordStore`, existing `RecordCheckout`, common catalogue/bound dispatch, real Notes exported definition. No Notes schema or storage rewrite here.

**Produces:** Metadata-only authorized app-record rows and instance-bound open dispatch. Proposed concrete types:

```rust
// ui::record_index; checkout is the existing ui::record::RecordCheckout.
#[derive(Clone)]
pub struct AppRecordRef {
    pub app_id: String, pub workspace_id: String,
    pub checkout: crate::record::RecordCheckout,
}
pub struct AppRecordRow { pub binding: AppRecordRef, pub title: String }
pub struct RecordIndexPage {
    pub rows: Vec<AppRecordRow>, pub revision: u64,
    pub next: Option<String>, pub reset: bool,
}
#[derive(Clone)]
pub struct RecordIndexClient { /* owner-scoped host client; no writer access */ }
impl RecordIndexClient {
    pub fn page(&self, cursor: Option<&str>) -> Result<RecordIndexPage, crate::launch::LaunchError>;
    pub fn resolve(&self, reference: &AppRecordRef) -> Result<AppRecordRef, crate::launch::LaunchError>;
}
// ui::application_files
pub enum OpenTarget {
    File(runtime_fs::FileGrant),
    Record { reference: AppRecordRef, reveal: Option<qos_actions::ActionAddress> },
}
pub enum OpenDisposition { FocusOrOpen, NewInstance }
pub enum OpenOutcome { Focused { instance_id: u64 }, Mounted { instance_id: u64 }, ExternalAccepted }
#[derive(Clone)]
pub struct OpenTargetClient { /* bound requesting owner, instance, mount and host queue */ }
impl OpenTargetClient {
    pub fn request(&self, target: OpenTarget, disposition: OpenDisposition)
        -> Result<qos_actions::InvocationId, crate::launch::LaunchError>;
    pub fn poll(&self, invocation: qos_actions::InvocationId)
        -> Option<Result<OpenOutcome, crate::launch::LaunchError>>;
}
```

- [ ] Extend Task 4 FilesContext with private `record_index: Option<RecordIndexClient>` and `opener: Option<OpenTargetClient>` fields, both initially None. Add `with_record_services(self, index: RecordIndexClient, opener: OpenTargetClient) -> Result<Self, LaunchError>` and `record_index(&self) -> Result<&RecordIndexClient, LaunchError>` / `opener(&self) -> Result<&OpenTargetClient, LaunchError>`. Binding is once-only; validate their requesting owner and runtime mount match the existing FilesContext. None returns RequiredProvider. Task 5 supplies real host owner implementations below; no fabricated empty successful client is introduced in Task 4.
- [ ] Add failing `notes_row_opens_exact_checkout`, `same_record_new_instance_preserves_two_views`, `cross_workspace_row_is_denied`, `record_revoked_after_search_is_denied`, `closed_files_cannot_redirect_open`, `record_index_revision_resets_search`, `record_index_survives_host_restart`, `unknown_application_is_visible_failure` tests. Use the **real** Notes factory with isolated canonical store, not a stub Notes widget. Assert opened app ID, workspace, RecordId and kind equal the original checkout, one shared writer, unchanged Notes record bytes/identity, and independently bound local Views. No `.notes` file, manufactured file UUID or Files-owned Notes writer is created.
- [ ] Run focused tests with `cargo nextest run --locked --profile ci -p file-explorer --test it -E 'test(hosted_record_open)'` and the registered UI record-index tests; confirm expected failures.
- [ ] Add a metadata index adjacent to canonical record owners, keyed by `(workspace, owning app ID, RecordId)`. Publish only Ready records after successful canonical initialization; never index a reservation or opaque filesystem path as authority. Persist/rebuild the index under the explicit host profile root from validated owner records; retain app-package storage partitioning. Treat index metadata as a hint: resolve performs live scope/read authorization and exact canonical open via the owning RecordStore. Corrupt/stale rows return typed errors without creating/replacing documents. Record titles come from owner/app metadata, not filename extension guesses.
- [ ] Keep pages at 256 rows with opaque revision-bound cursors; reset on revision change and invalidate cached rows on revocation. Index clients carry a read scope, never a blanket root path or direct arbitrary RecordStore access. If service backing is absent, Files shows “Record catalogue unavailable” and preserves its granted local files.
- [ ] In Files add `CatalogRecord::Filesystem(model::WorkspaceRecord)` and `CatalogRecord::Application(ui::record_index::AppRecordRow)` to the retained catalogue/search projection. Reuse current paging cadence and search snapshots; preserve existing filesystem record UUID resource IDs. Application-record resource IDs encode the complete app/workspace/RecordId tuple injectively (for example a versioned, length-delimited serde encoding), with a separate namespace; never split RecordId at a colon or truncate it. Keep filesystem Entry rows separate rather than stuffing an app record into Entry.path.
- [ ] Activation of an app-record row enqueues OpenTarget::Record with the exact reference/reveal address and caller's requested disposition. Host reauthorizes at dequeue, rejects closed request mount, resolves the canonical catalogue definition, opens the owning package's RecordStore, and passes `LaunchTarget::Record { checkout, reveal }`. Report Mounted only after factory/restore/mount success, Focused only for an existing matching instance. Queue admission alone produces Accepted. Unknown/unsupported types produce visible errors; external file opening is allowed only through an explicit host-default provider and reports ExternalAccepted, never in-shell Mounted.
- [ ] Preserve `AppHostIntent::ShowWorkspaceActivity { resource_id, version_id, entry }` for exact history/activity. Route it under the source instance workspace/mount; pass the exact record resource/version without substituting current shell selection. This intent does not grant record access.
- [ ] Export one `file_explorer::application_definition()` with ID `dev.qos.file-explorer`; its factory consumes HostContext/LaunchTarget/RestoreInput, constructs `FileExplorerWidget::from_host`, returns AppComponent with existing resource actions and an empty document-action registry as required by Files. Do not create fake Document operations to populate the registry. Common shell catalogue and library conformance consume this same definition; runner conformance uses the framework's `PreparedComponentInputs` and explicit `ComponentHostPreparer` through `LaunchSpec::component_host`. `LaunchSpec::from_definition` alone returns `MissingComponentHost` before factory/window/profile discovery. Test preparers receive registered `allowed_live_options`/`LaunchArgs::value` inputs and isolated explicit services; do not assume a production record/profile flag, permissive author, or ambient default host. Production main.rs cutover is deferred to Task 7 only after the separate SDK standalone-host bootstrap supplies the real preparer and exact CLI policy. Keep specialized shots fixtures separately named and do not use them as factory proof.
- [ ] Run Task 5 gates, commit `feat(files): open canonical app records through the shared catalogue`; controller exact-SHA Fedora verification.

## Task 6: Concrete shell/library integration and semantic headless conformance

**Files:** final `shell/src/shell/{hosted_inputs,host_apps,doc_open,placement,mod}.rs`, `shell/src/shell/real_files_host_tests.rs` registered in the library's `shell/mod.rs`, capability-map Files row. **Do not change the Files production binary in this task.**

**Consumes:** Tasks 1–5, accepted common framework, real Notes factory and direct core cutover. Uses the already trusted **in-process** shell owner with explicit services; no standalone issuer or native main-thread window adapter is assumed.

**Produces:** Real catalogue/library component construction with grant-backed Files effects, exact Notes record open and retained close recovery; real definition semantic headless conformance on Mac/Fedora. This task can complete without a native window launch, and its result must be named accordingly.

This task is also the sole producer of the concrete shell service container below, in `shell/src/shell/hosted_inputs.rs`. Native-window Task 5 imports this existing type into PreparedShellHost and requires Files Tasks 1–6; it does not redeclare it. The Files library tests require neither the native window nor native main, keeping the dependency order acyclic.

```rust
pub(crate) struct FilesServiceOwner {
    pub(crate) grants: std::sync::Arc<runtime_fs::NativeGrantAuthority>,
    pub(crate) operations: Vec<std::sync::Arc<runtime_fs::recovery::OperationOwner>>,
    pub(crate) app_local_state: Vec<std::sync::Arc<runtime_fs::app_local_state::AppLocalStateOwner>>,
    pub(crate) transfers: runtime_fs::transfer::FileTransfers,
    // Host operation IDs/reports and the live transfer-policy owner also live here.
}
```

- [ ] Add failing `real_files_host` integration cases with explicit temporary profile, canonical record owners, `Rc<InstanceViewStore>`, granted opened directories, OperationOwner, AppLocalStateOwner, FileTransfers and the actual catalogue. Construct real Files; run enumeration/create/rename/copy/move/trash through resources; verify disk/outcomes. Construct real Notes, find and open its exact checkout from Files, close/reopen Files and verify fresh mount scope and retained local View under reauthorized grants.
- [ ] Register `#[cfg(test)] mod real_files_host_tests` in `shell/src/shell/mod.rs`; run `cargo nextest run --locked --profile ci -p shell --lib -E 'test(real_files_host_tests)'` and require nonzero discovered/executed cases and expected failing behavior before implementing. These tests instantiate actual shell/catalogue/resources; library registration does not replace them with a model.
- [ ] Implement the one FilesServiceOwner above. Make the actual in-process TransferAuthorizer use current shell workspace/app/root transfer policy, with explicit exact-request decisions and revocation. Owner strings, root labels or a PickerScope path never substitute for that policy. Missing policy/service produces RequiredProvider or Denied before effects.
- [ ] Supply roots through trusted host configuration/admission and the exact FilesContext constructors. Keep `HostedTileInputs::fixture` under its named isolated fixture route. Production typed construction never falls back from missing grants to fixture(), new(), home, cwd, `/var/workspace` or PickerScope paths. Before constructing the component, attach FilesContext to an explicitly prepared HostContext with its one Rc<InstanceViewStore>.
- [ ] Close through `AppComponent::close()`/`suspend()` with no View-store argument. Persist/retain ComponentCloseReport and its view_retry/recovery/record_retry beside file-effect and app-state reports until resolved. Test View save failure plus one staged file effect and one failed preference save at the same close: dropping the widget loses none; retries write only the originally bound namespaces and cannot wake a replacement mount. No close-time profile discovery.
- [ ] Drive the same ApplicationDefinition through `FrameHarness::from_component` and `App::from_component` without starting a window, and through embedded TileHost. Use explicit PreparedComponentInputs/test adapters to compare resources, effect outputs, scope denials, stale wake rejection, and local View. Do not call the compatibility CLI and count it as the production definition.
- [ ] Run native Mac focused checks/tests and commit `feat(shell): bind real Files services to common components`; controller verifies that exact SHA/lockfile on Fedora before accepting. Linux package/compositor regressions remain required. Mark capability evidence specifically as library/shell-construction/semantic-headless conformance; native presentation and production standalone remain Task 7.

## Task 7: Separately gated native and standalone product qualification

**Files:** `apps/file-explorer/src/main.rs` only for accepted standalone cutover, app/runtime integration tests, and capability-map qualification evidence. Native host API implementation belongs to the separately accepted native-host slice; this task consumes it, never fabricates a UI test result.

**Prerequisites:** Task 6 plus the actual Mac native window/event-loop adapter for the native walkthrough. Standalone additionally requires the stable `.superpowers/sdd/2026-09-12-macos-shell-build-foundation/standalone-host-bootstrap-plan-draft.md` implementation **and** its separately accepted `docs/superpowers/plans/2026-09-13-authenticated-component-session.md` producer. The latter plan/implementation is absent at this revision: production standalone cannot yet be qualified. Do not mark Task 7 complete because Task 6 passed. Its native and standalone sub-gates are recorded separately and both are required for this task's full completion.

**Exact SDK consumer:** public `qos_app::standalone::launch_spec(definition)` installs `qos_app::standalone::prepare`, which produces PreparedComponentInputs only after authenticated admission. Files' Cargo manifest aliases qos-app as `ui`, so its actual main expression is `ui::launch::run(&ui::standalone::launch_spec(file_explorer::application_definition()))?;`. Keep existing compatibility main.rs until both producers are accepted; no app-local `real_preparer` placeholder or permissive bootstrap.

- [ ] After authenticated component sessions and SDK bootstrap land, add a real issuer-to-binary integration test using the actual shell-issued connected session descriptor. Register/accept the SDK's exact options `--profile`, `--workspace`, `--instance`, `--record`, `--host-session-fd` and flag `--new`; require exactly one of New/Existing and **zero positionals**. No `--root`, path positional, `--author`, unregistered record option or ambient identity fallback. Initial Files directory grants arrive through admitted descriptors, not CLI path authority. Existing received-share stdin behavior remains the compatibility route until its own authenticated adapter is accepted; do not silently add unsupported SDK flags.
- [ ] Verify wrong app/workspace/profile/record/instance, revoked authority, missing/disconnected issuer and absent required providers fail before factory or canonical record I/O. Verify real Files and Notes definition startup, exact record open, local state durable acknowledgement and framework close-report retry on both hosts. Test SDK definition-without-component_host still returns MissingComponentHost and cannot read HOME.
- [ ] Cut over main.rs through the exact SDK registrar only after the preceding real tests pass; commit `feat(files): use authenticated standalone component bootstrap`. Controller reviews and verifies exact SHA on Fedora. This is a distinct commit/delivery from Task 6. Shared authority doors must recheck effect/chunk/commit and record-index/open execution; initial fd admission alone does not make a grant irrevocable.
- [ ] After the real native Mac host adapter is available, run the actual shell window walkthrough with explicitly granted disposable files: pointer/keyboard, resize, refresh after external edit, create/rename/copy/move/trash, per-item outcome/error, exact Notes open and two independent Views. Verify Metal/IME/accessibility and applicable Linux compositor behavior in the main-spec matrix. Semantic headless construction does not fulfill this native gate.
- [ ] Native grant dialogs/file URLs use the accepted main-thread adapter, which owns native URL/security-scope lifetime and passes only granted resources inward. Until it exists, show native acquisition unavailable; trusted startup grants can support native provider execution but do not prove a chooser. Direct double-click first-run onboarding remains separately unqualified until native secret/unlock, identity/workspace admission and grant acquisition are implemented.
- [ ] Native Mac trash qualification uses one newly created disposable file and verifies the **original filename** reached native Trash, not just that the source disappeared. Never enumerate/empty unrelated user Trash. Automated Freedesktop temp-directory tests on Mac remain separately labelled test-adapter evidence. Real two-volume move/trash checks require two disposable configured volumes or record missing prerequisites explicitly, never a pass.
- [ ] Update the capability map with measured library, native and standalone evidence separately. Keep unavailable dialog acquisition, external native drag/export, unbound DAV/sharing/history and Mac native Trash list/restore/purge visible until independently implemented/tested. This plan alone does not complete Notes collaboration or the whole first milestone.

## Verification protocol and commands

The implementation author runs native Mac focused tests/checks before each task commit; controller reviews that SHA and verifies Fedora at exactly the same SHA/lockfile before accepting. Corrections receive follow-up commits and repeat affected gates. Verification below is a future execution obligation, not evidence that this planning revision ran it.

For Mac runtime/ui/app-only Cargo commands use:

```sh
CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py
```

For **every Mac shell-dependent command**, including Task 6 checks, nextest and native qualification builds, use:

```sh
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py
```

On Fedora the controller uses its explicit checkout target directory, same helper and Cargo payload, with that host's established ffmpeg environment; it does not reuse the Mac path. The helper clears QOS/QUICK/XDG and makes fresh XDG directories but preserves HOME/toolchain. Every new test therefore injects grants/stores and avoids ambient Mac home accesses. Record git SHA, rustc -vV, lockfile identity, test counts and exit codes. Zero selected tests is failure.

**Test registration checked against source:** runtime-fs/Cargo.toml has no `autotests=false` and no explicit test restriction, so each top-level `tests/native_*.rs` and `tests/app_local_state.rs` auto-registers as its own integration target; shared `tests/support/mod.rs` does not. Files uses `autotests=false`, so its new integration files must be modules of `tests/it.rs`. UI also uses its explicit `it` target. The Task 6 shell cases are library modules under `shell/src/shell/mod.rs`, independently of the existing Linux integration target. cfg(test) fault-injection cases stay in each library's internal tests.

| Task | Required nextest command/selection | Required check | Required clippy |
| --- | --- | --- | --- |
| 1 | `cargo nextest run --locked --profile ci -p runtime-fs --test native_grants` plus existing runtime-fs tests | `cargo check --locked -p runtime-fs --all-targets` | `cargo clippy --locked -p runtime-fs --all-targets -- -D warnings` |
| 2 | `cargo nextest run --locked --profile ci -p runtime-fs --test native_operations`; `cargo nextest run --locked --profile ci -p runtime-fs --lib -E 'test(operations::tests) or test(recovery::tests)'`; Task 1 regressions | `cargo check --locked -p runtime-fs --all-targets` | `cargo clippy --locked -p runtime-fs --all-targets -- -D warnings` |
| 3 | `cargo nextest run --locked --profile ci -p runtime-fs --test native_trash --test native_observe_transfer` plus provider regressions | `cargo check --locked -p runtime-fs --all-targets` | `cargo clippy --locked -p runtime-fs --all-targets -- -D warnings` |
| 4 | `cargo nextest run --locked --profile ci -p runtime-fs --test app_local_state`; `cargo nextest run --locked --profile ci -p runtime-fs --lib -E 'test(app_local_state::tests)'`; `cargo nextest run --locked --profile ci -p file-explorer --test it`; Files library resource/action regressions | `cargo check --locked -p runtime-fs -p ui -p qos-app -p file-explorer --all-targets` | `cargo clippy --locked -p runtime-fs -p ui -p qos-app -p file-explorer --all-targets -- -D warnings` |
| 5 | `cargo nextest run --locked --profile ci -p file-explorer --test it`; `cargo nextest run --locked --profile ci -p ui --test it -E 'test(record_index) or test(application_contract)'`; actual Notes affected tests | `cargo check --locked -p ui -p qos-app -p file-explorer -p notes --all-targets` | `cargo clippy --locked -p ui -p qos-app -p file-explorer -p notes --all-targets -- -D warnings` |
| 6 | `cargo nextest run --locked --profile ci -p shell --lib -E 'test(real_files_host_tests)'`; actual app semantic headless tests; Linux package/compositor regressions | Mac: `cargo check --locked -p shell --lib` and `cargo check --locked -p file-explorer -p notes --all-targets`; Fedora: `cargo check --locked -p shell -p file-explorer -p notes --all-targets` | Mac: `cargo clippy --locked -p shell --lib -- -D warnings` and `cargo clippy --locked -p file-explorer -p notes --all-targets -- -D warnings`; Fedora: `cargo clippy --locked -p shell -p file-explorer -p notes --all-targets -- -D warnings` |
| 7 | `cargo nextest run --locked --profile ci -p ui --test it -E 'test(standalone_prepare) or test(standalone_lifecycle)'` plus accepted issuer-to-binary tests and Task 6 regressions; native walkthrough is an additional gate | `cargo check --locked -p shell -p ui -p qos-app -p file-explorer -p notes --all-targets` | `cargo clippy --locked -p shell -p ui -p qos-app -p file-explorer -p notes --all-targets -- -D warnings` |

Run `cargo fmt --all --check` on every task's final diff on both hosts. Preserve existing Files `it::{double_click_open,drag_drop_move,parity,scroll,shots_smoke}`, library action/results and Linux live collaborative mount checks with explicit prerequisites. A native dependency failure needs exact parent-SHA evidence; it is not a portable pass. The final controller runs the authoritative full workspace matrix, isolated golden tail and required native/host tests from the main spec. No fixture suite or absence of a runtime error replaces these gates.

After a successful normal git push, final CI is automatically delivered to the task. Wait for relevant final CI before merging; investigate failures with `fgj actions`. The task commits do not by themselves require a push or merge.

## Preflight resolutions

| Finding | Resolution in this revision |
| --- | --- |
| 1 — authority source/header | Main spec/capability map are authoritative; framework dependency is `docs/superpowers/plans/2026-09-13-common-real-app-framework.md`. Header uses subagent-driven-development. Historical worker instructions removed. |
| 2 — mutable local state | Task 4 produces AppLocalStateOwner/Client with fixed Favorites/Zoom keys, CAS snapshots, StateSaveId/status/retry and shutdown ownership. General arbitrary-file replacement remains unavailable. Templates/downloads use bounded new-file effects. |
| 3 — recovery producer | Task 2 produces OperationOwner, OperationId, RecoveryId, durable reports and explicit host storage/admission/query/resolve interfaces. ItemOutcome carries recovery references; UnknownNativeCommit is distinct and never destructively replayed. |
| 4 — delegation decision | Task 3 FileTransfers constructor requires a live TransferAuthorizer; private permits bind exact recipient/mount/destination/action and are checked at paste/chunk/commit. Explicit revoke, 32 payloads, 256 items, 120-second TTL. |
| 5 — trash volume/name | Per-NativeVolumeId configuration verifies descriptor volume and refuses missing/duplicate config. Private operation parent preserves original leaf. Receipt checkpoint ordering is explicit. Mac native versus temp Freedesktop test evidence is separated. |
| 6 — delivery prerequisites | Task 6 is concrete in-process shell/library/headless integration. Task 7 separately consumes native host plus, for binary cutover, authenticated-component-session and SDK bootstrap. Exact public registrar/preparer and zero-positional CLI named. |
| 7 — commands | Mac shell commands carry ffmpeg@8 PKG_CONFIG_PATH; each clippy package set is literal. Runtime-fs auto-discovery versus UI/Files/shell it-module registration verified against manifests. |
| 8 — View ownership | Task 4/6 consume HostContext::prepare's Rc<InstanceViewStore>, no-argument close/suspend and retained ComponentCloseReport::view_retry/recovery/retry_view. No second profile lookup. |

## Task and shared-interface self-review

| Boundary | Producer → consumer | Concrete verification / remaining qualification |
| --- | --- | --- |
| Task 1 | NativeGrantAuthority, opaque scoped DirectoryGrant/FileGrant/FsSession → operations | Real files, disjoint same-owner roots, symlink escape, non-UTF8, revoke/close/reopen; no GUI dependency. |
| Task 2 | Anchored FsOperation/FsJob + OperationOwner/RecoveryId → trash, transfer, Files outcome UI | No-replace/copy/move/cancel/partial byte assertions; closed widget reports and restart reconciliation; real two-volume qualification remains explicit. |
| Task 3 | Per-volume trash, polling observer and FileTransferClient/live authorizer → Files | Original names, known-versus-unknown recovery, quotas/TTL, live delegation revoke, source close; Mac native Trash remains a separate Task 7 gate. |
| Task 4 | AppLocalStateOwner/client and FilesContext → actual FileExplorerWidget | Actual config update/durable acknowledgement/retry, real resource effects, no ambient calls, retained View/effect/state close failures; opener/index explicitly unavailable until Task 5. |
| Task 5 | Canonical owner index + OpenTargetClient + one real definition → shell and SDK consumers | Actual Notes checkout/app/workspace identity, one writer/two Views, stale/revoked row failures, registered factory; no `.notes` path. |
| Task 6 | Concrete in-process host services/catalogue → real shell/library and FrameHarness/App constructors | No native event loop or production standalone claim; exact-SHA native-toolchain/Fedora semantic checks complete this task independently. |
| Task 7 | Accepted native host + accepted authenticated session + SDK registrar → actual window/binary | Explicitly blocked while producers absent. Native and standalone evidence recorded separately; no early completion from Task 6. |
| Grant → operations | Task 1 allow-set/generation/parent descriptors + Task 2 attached OperationOwner → FsSession::start | Store absence fails before effect; every syscall/chunk/commit rechecks scope; receipt locators never reconstruct authority. |
| Operations → recovery/trash/transfer | FsJob::operation_id, ItemOutcome::recoveries and durable owner report → host query/resolution and payload bookkeeping | KnownStaged/PublishedSourceRetained/UnknownNativeCommit distinct; private staging bytes retained; cut items removed only from durable Completed outcomes. |
| Provider → Files | Typed FilesContext + AppLocalStateClient + FileTransferClient → from_host and existing v2 resource dispatch | Same actual widget/model/actions; no broad Write or generic service map; Pending is not Saved; effects remain Effects. |
| Record owner/index → opener | Ready canonical records → authorized pages/resolve → exact OpenTarget::Record → real catalogue definition | Reauthorize at open; current owning package/workspace writer; accepted queue is not Mounted; closed mount cannot redirect completion. |
| Definition → shell/standalone | Task 5 factory + explicit HostContext/View inputs → Task 6; accepted qos_app::standalone::launch_spec/prepare → Task 7 | MissingComponentHost/issuer/required services fail honestly. Stable bootstrap still has an unimplemented authenticated-session predecessor. |

## Remaining product dependencies (not missing provider interfaces)

- The controller assigns a sole author after the direct core's reviewed cutover. This ignored revision changes no production source.
- Common-framework implementation and real Notes factory/storage must land for their named consumers; they are specified in their own accepted plans.
- Authenticated component sessions are absent; the stable standalone bootstrap's public interfaces do not prove their producer exists. Files production main.rs stays on its compatibility path until both land. This candidate renumbers that cutover to **Task 7**; update the sibling bootstrap's old Files Task 6 reference when it is adopted.
- Native window/Metal/IME/accessibility, native chooser/security-scope acquisition, direct double-click setup, native external transfer, portable DAV/sharing/history and actual Mac↔Fedora collaboration retain their separate qualification obligations. No app-fleet rewrite is included.
- Mac native Trash list/restore/purge and unconfigured-volume trash remain Unsupported. Trash right never grants permanent purge or emptying unrelated user Trash.

Controller preflight: the eight original findings are resolved. WakeHandle::is_active exists and checks an open route independently of bind/mount, so the supplied context handle permits pre-mount work and closes with its scope; never substitute a detached handle. RecordIndexClient and OpenTargetClient clones retain the same bound owner/mount and queues, as required by FilesContext cloning. Tasks 1–6 are sequenced implementation slices after their named prerequisites; Task 7 remains explicitly contingent on real native and standalone producers. No production or native qualification has occurred from adopting this plan.
