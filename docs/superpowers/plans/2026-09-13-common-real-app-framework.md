# Common Real-App Framework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task, with one implementation author at a time. Steps use checkbox (`- [ ]`) syntax for tracking. Controller preflight is recorded in this plan's SDD ledger. Continue the already-authorized slices after each review and exact-SHA host verification.

**Goal:** Establish one application factory, canonical record owner, per-instance local presentation/undo context, and fully bound widget/action dispatch shared by standalone, embedded, and headless hosts.

**Architecture:** Extend `LaunchSpec`, `AppWidget`, `RecordStore`, `UndoLog`, `View`, and the existing qos-actions dispatcher in `ui`, reexported by `qos-app`. A component owns its real widget and fresh `WakeScope`; a scoped record owner owns the canonical Automerge document and writer. `App` and `headless::Driver` consume the same prepared component, and `TileHost` presents it without changing the destination of descendant callbacks or operations.

**Tech Stack:** Existing Rust workspace, Automerge, app-state/atomic-file, qos-actions/collab-types permissions, retained Widget/Taffy, WakeScope, serde/postcard, existing nextest and isolated test helper. No new runtime, wire ABI, widget system, service locator, or dependency on shell from ui/apps.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md` §§4, 6, 7, 8, 11; `docs/superpowers/specs/2026-09-12-macos-app-capability-map.md`, particularly Notes and Files acceptance.

## Global Constraints

- “The first milestone is **real Notes and Files, with a complete capability map for every other app**.” This plan supplies their framework dependency, not milestone completion or substitute factories.
- “Apps and `ui` must not depend on `shell` to request a service.”
- “Two tiles opening one record share its document, but keep independent Views and operation contexts.”
- “A construction or restoration failure leaves a retryable error tile and retains the original persisted data.” The framework returns a retained typed error; the later shell integration renders that tile.
- “Headless execution remains a first-class runner of this same host contract.”
- “Benchmarks and tests must not load the user's real state or secrets.”
- “Keep Linux defaults and HDR behavior.”
- Reviewed wake/LocalState/App baseline is `f14221783`, including completed Task 3 conformance. Do not replace it. There are no concurrent UI fixture edits; the direct core cutover is the sole source author until the controller assigns this plan's next implementation author.
- The direct shell core cutover plan can move frame/render/shared-shell modules. This plan has no dependency on their current location and edits no shell module.
- Same codebase and current lockfile/toolchain on both hosts; no Notes migration, Files provider, collaboration transport, package bridge, native event loop, or 47-app rewrite in this plan.

---

## Execution and review protocol

The controller assigns one implementation author at a time. For each task: write the meaningful failing behavior tests, implement, self-review the full diff, run native macOS focused tests and affected checks, then create the explicit source commit named at the end of that task. Report the commit SHA and native evidence. The controller reviews that concrete task commit, synchronizes the exact SHA to Fedora and verifies the focused tests/affected checks there before accepting the task and proceeding. Corrections repeat this cycle with an explicit follow-up commit. Do not make the source commit depend on a prior controller review of a nonexistent commit. These already-authorized slices require no additional user permission.

Native commands below use `CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell`; record `rustc -vV` host/target and use the already configured Apple Silicon toolchain. Keep `--locked --profile ci` on nextest runs, and require nonzero discovered/executed counts. On Fedora the controller uses that host's established explicit target directory and the same helper/lockfile/selection at the exact source SHA; do not reuse the Mac filesystem path. A baseline Mac dependency failure needs concrete parent-SHA evidence; it is not a portable success or grounds for excluding Fedora checks. No GUI launch is needed for these focused gates. After any normal git push, await the automatic final CI notification and use `fgj actions` for failures; keep relevant CI green before merging. No merge/push is required merely to produce this plan's task commits.

The following check/clippy gates are mandatory for each task on **both hosts**, in addition to its focused tests. Run the native commands before its source commit; the controller runs the identical Cargo payload at that exact commit on Fedora. Prefix each command on Mac with `CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py`; on Fedora use `CARGO_TARGET_DIR=/home/parallels/qos-target python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py` from the controller's isolated checkout. These explicit target directories preserve HOME/toolchain and prevent cross-host cache assumptions. Formatting uses `cargo fmt --all --check` on both hosts. A failed native gate must be fixed or established as an exact parent-SHA blocker; the affected Fedora gate remains required and no failure counts as a pass.

| Task | Required compile command (both hosts) | Required lint command (both hosts) |
| --- | --- | --- |
| 1 — ownership/storage | `cargo check --locked -p app-state -p ui --all-targets` | `cargo clippy --locked -p app-state -p ui --all-targets -- -D warnings` |
| 2 — factory/View contract | `cargo check --locked -p ui -p qos-app --all-targets` | `cargo clippy --locked -p ui -p qos-app --all-targets -- -D warnings` |
| 3 — bound callbacks | `cargo check --locked -p ui -p qos-app --all-targets` | `cargo clippy --locked -p ui -p qos-app --all-targets -- -D warnings` |
| 4 — bound actions/history | `cargo check --locked -p ui -p qos-app --all-targets` | `cargo clippy --locked -p ui -p qos-app --all-targets -- -D warnings` |
| 5 — all runners | `cargo check --locked -p app-state -p ui -p qos-app --all-targets` | `cargo clippy --locked -p app-state -p ui -p qos-app --all-targets -- -D warnings` |

The complete workspace/real-app host matrix remains a later controller gate; this table is the mandatory minimum for every common-framework task and cannot be replaced by integration-test success.

## Source evidence and decisions for controller preflight

| Source inspected | Consequence |
| --- | --- |
| `ui/src/launch.rs:462` and `:495` | Headless calls `live_root` and opens state independently; only native calls `configure_app`. Definition-aware preparation must happen before choosing the runner. Keep old launch entrypoints for unmigrated apps. |
| `ui/src/headless.rs:424` | `Driver` is a type alias for `FrameHarness`, not a separate module. Modify `headless.rs`. |
| `ui/Cargo.toml:313`, `ui/tests/it.rs` | Integration tests are modules of the single `it` binary (`autotests=false`); merely adding a test file does not run it. |
| `ui/src/record.rs:569,766,858` | Current package/workspace resolution reads environment, `persist_for` converts load errors to absence and tries legacy adoption, and `open_record -> bool` conflates failure/already-open. Canonical opening needs a separate fallible explicit path with no implicit import. |
| `crates/system/app-state/src/lib.rs:423,470,483` | Lock loss currently selects a random actor but permits writing. Empty existing document bytes currently mean absence. Canonical opening must refuse both ambiguous emptiness and contested writers. |
| `crates/system/app-state/src/lib.rs:483,572,693` | Current load uses partial-load Ignore; save/write_full publish immediately. Keep recovery bytes in a CanonicalLoad until UI identity validation explicitly accepts publication through write_full. |
| `crates/system/app-state/src/autosave.rs:175,197` | `flush` returns unit; `last_error` is a string. Reuse the writer and expose a fallible acknowledgement/status above it; do not equate a queued request with durable success. |
| `ui/src/widget/tile_host.rs:561,615,751` | First sync calls `UndoLog::rebase`, layout inserts raw descendant pointers, and bound input uses the record but ambient author. Root forwarding alone is insufficient. |
| `ui/src/state/mod.rs:20,101,116,260` | Undo entries have real author and explicit heads; rebase only changes `last_heads`, not stacks. Add instance filtering and explicit action boundaries without rewriting actor/user identity. |
| `ui/src/app.rs:2449`, `ui/src/headless.rs:1621` | Resource resolution and Document execution currently select the runner's document. Extract the existing implementation and select the component binding before authority checks/executor/history. |
| `ui/src/undo.rs:220` | Global raw-pointer draining combines commit notifications into the outer `State`. Partition by binding and drain once at the record boundary. |
| `ui/src/wake.rs:60,127`, `ui/src/app.rs:5229` | `WakeScope::enter` can enter a closed route; App Drop closes, enters, then explicitly drops the root. Reuse that teardown ordering; detached handles still authorize publication. |
| `ui/src/view.rs:65,141` | Existing `View` is reusable; ordinary hosted restoration must not invoke `FrozenApp.state` replacement. `RecordCheckout` is not yet a persisted envelope. |
| `ui/src/permissions/mod.rs:124`, `crates/collab/actions/src/context.rs:17` | Reuse `PermissionContext` and `ActionContext`. The latter is provenance, not a user/instance identity or capability. |
| `crates/runtime/runtime-fs/src/lib.rs` | Existing `FsClient`/`FileHandle` do not supply the complete portable directory-grant/operation contract. Do not invent a path-bearing “grant” in this framework slice. |
| `shell/src/shell/tab_documents.rs:26` and `chrome_state.rs` | Durable tile IDs are `u64`. Reuse that representation below shell; scope it by explicit workspace/device rather than import a shell type. |

**Writer decision:** Keep legacy advisory multiwriter behavior only while every participant is legacy-mode in this revised version. Add one shared/exclusive ownership gate on the existing store directory: legacy writable stores hold a shared gate, canonical stores an exclusive gate. A canonical reservation/owner retains its Arc<DocStore> outside worker lifetime, and the gate remains held until the owner/recovery handle and its last worker have both released it; legacy stores hold the shared gate for their writer lifetime. Keep the existing actor lock for legacy behavior. This prevents a revised legacy entrypoint from writing alongside a canonical owner and prevents canonical opening of an active legacy import destination. Older/uncooperative binaries do not acquire this gate; no advisory mechanism stops their arbitrary writes. Concurrent use of one profile by those binaries remains unsupported and must be stated in the API docs. Do not describe this as runtime-qualified.

**Service boundary decision:** The initial context has an explicit record service, current `PermissionContext`, authenticated `UserId`, `ActionContext`, and mount handle. Required/optional capability declarations use existing `PermissionScope` requests and existing service availability identifiers only when backed by a concrete implementation. The Files and collaboration plans add their actual typed handles to this context; this slice does not publish an `Any` map, raw-path authority, or success-shaped fake service.

**Schema decision:** Use a new narrowly versioned local instance envelope containing existing `View` and record-reference fields, separate from `FrozenApp`. Version 1 has no document bytes and no effects to replay. App-specific local View contributions beyond today's fields belong to the Notes plan after its exact selected-page/caret needs are traced.

## File responsibility map

- `crates/system/app-state/src/lib.rs`: explicit canonical opener, absence/error distinction, writer ownership gate, typed store errors.
- `crates/system/app-state/src/autosave.rs`: fallible flush acknowledgement while preserving existing `flush()` compatibility.
- `ui/src/record.rs`: explicit owner registry and fallible record access, canonical identity checks, initialization before history, status/flush and action ownership.
- New `ui/src/application.rs`: definition/catalogue, context, target, component construction and lifecycle. No native window code.
- New `ui/src/application_view.rs`: validated local instance envelope and explicit local store; reuse `View` and `atomic-file`.
- New `ui/src/bound_dispatch.rs`: callback binding/node routing and extracted resource-operation execution over the selected document.
- `ui/src/widget/{mod,app_widget,tile_host}.rs`: default binding hook, component presentation/lifecycle and full existing widget contract.
- `ui/src/{app,headless,launch,undo,view,wake,a11y_tree,accesskit_bridge}.rs`: consume the new bound owner/helpers; preserve legacy APIs where specified.
- `ui/src/state/mod.rs`: instance-aware entries/filtering and explicit mutation boundaries in existing UndoLog.
- `ui/src/lib.rs`, `crates/app/qos-app/src/lib.rs`: public module/prelude exports.
- New `ui/tests/{application_contract,canonical_record_owner,bound_component_dispatch,instance_undo}.rs` plus `ui/tests/it.rs`: meaningful behavior gates; tests share small probes in new `ui/tests/support/application.rs` registered in existing `support/mod.rs`.

## API vocabulary to keep consistent

These are proposed additions, not assertions that they already exist. Concrete service extensions noted in the final section are intentionally not part of these signatures.

```rust
// ui::application. u64 is the existing durable tile ID representation.
pub type InstanceId = u64;
pub type ComponentFactory = fn(HostContext, LaunchTarget, RestoreInput)
    -> Result<AppComponent, crate::launch::LaunchError>;

pub struct ApplicationDefinition {
    pub app_id: &'static str,
    pub title: &'static str,
    pub icon: crate::widget::IconDescriptor,
    pub record_kind: crate::record::RecordKind,
    pub required: Vec<crate::permissions::PermissionScope>,
    pub optional: Vec<crate::permissions::PermissionScope>,
    pub create: ComponentFactory, // not Option: supported entries cannot lack a factory
}

#[non_exhaustive]
pub enum LaunchTarget {
    New,
    Record {
        checkout: crate::record::RecordCheckout,
        reveal: Option<qos_actions::ActionAddress>,
    },
}

pub enum RestoreInput {
    None,
    Local(crate::application_view::SavedInstanceView),
    // Explicit recovery input only; decode/validate, never adopt state on mount.
    LegacyRecovery(crate::view::FrozenApp),
}

// Private fields; constructed by a runner/host, never inferred by app code.
pub struct HostContext {
    app_id: String,
    instance_id: InstanceId,
    device_id: String,
    records: crate::record::RecordStore,
    views: std::rc::Rc<crate::application_view::InstanceViewStore>,
    record: crate::record::RecordCheckout,
    reservation: Option<crate::record::RecordReservation>, // unique New construction owner
    permissions: std::rc::Rc<std::cell::RefCell<crate::permissions::PermissionContext>>,
    author: crate::permissions::UserId,
    action_context: qos_actions::ActionContext,
    wake_scope: crate::wake::WakeScope,
}

pub struct AppComponent {
    // Own real content and instance lifetime together; expose no unbound root extraction.
    context: HostContext,
    root: Option<Box<dyn crate::widget::AppWidget>>, // taken/dropped explicitly under CLOSED child entry
    registry: Option<qos_actions::ActionRegistry>, // callback captures dropped under same CLOSED entry
    local_view: crate::view::View,
    close_report: Option<ComponentCloseReport>,
    // Runtime-only mount state and existing per-invocation lifecycle bookkeeping.
    // Custom Drop closes context.wake_scope before cancelling/dropping root;
    // field declaration order is not used as the lifecycle mechanism.
}
```

The full construction signatures are:

```rust
impl HostContext {
    pub fn prepare(definition: &ApplicationDefinition, records: RecordStore,
        views: std::rc::Rc<InstanceViewStore>,
        device_id: String, instance_id: InstanceId,
        permissions: std::rc::Rc<std::cell::RefCell<PermissionContext>>,
        author: UserId, action_context: qos_actions::ActionContext,
        target: &LaunchTarget) -> Result<Self, LaunchError>;
    pub fn record(&self) -> &RecordCheckout;
    pub fn author(&self) -> &UserId;
    pub fn permissions(&self) -> PermissionContext; // fresh snapshot, no authority cache
    pub fn action_context(&self) -> &qos_actions::ActionContext;
    pub fn wake_handle(&self) -> crate::wake::WakeHandle;
    pub fn read_record<R>(&self, read: impl FnOnce(&AutoCommit) -> R)
        -> Result<R, RecordOpenError>;
    pub fn initialize_new_record(&mut self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>)
        -> Result<(), RecordOpenError>;
}
impl ApplicationDefinition {
    pub fn instantiate(&self, context: HostContext, target: LaunchTarget,
        restore: RestoreInput) -> Result<AppComponent, LaunchError>;
}
impl AppComponent {
    pub fn new(context: HostContext, root: Box<dyn AppWidget>,
        registry: qos_actions::ActionRegistry) -> Self;
    pub fn record(&self) -> &RecordCheckout;
    pub fn mount(&mut self, sink: impl Fn(crate::wake::WakeEvent) + Send + Sync + 'static)
        -> Result<(), LaunchError>;
    // Suspension in this first slice unmounts through close; restoring constructs
    // a fresh component/lifetime from the saved binding/View. No scope is reopened.
    pub fn suspend(&mut self) -> ComponentCloseReport;
}
pub struct ApplicationCatalog {
    definitions: std::collections::HashMap<&'static str, &'static ApplicationDefinition>,
}
impl ApplicationCatalog {
    pub fn new() -> Self;
    pub fn register(&mut self, definition: &'static ApplicationDefinition)
        -> Result<(), LaunchError>;
    pub fn resolve(&self, app_id: &str) -> Result<&'static ApplicationDefinition, LaunchError>;
}
```

These snippets omit imports; `AutoCommit`, `PermissionContext`, `UserId`, record types, AppWidget, LaunchError and SavedInstanceView are the existing or explicitly defined types above. Mutable authority handles are supplied by the host; the public `permissions()` snapshot is informational and execution rereads the private live handle. HostContext constructors take explicit values; neither package/workspace nor user identity comes from environment in this API. `RecordStore` fixes package/workspace; validate it matches definition and restore binding. Runner preparation opens an existing record or reserves a new record identity and creates HostContext before factory invocation. A New reservation is not yet a live record, has no UndoLog/autosave, and cannot be opened by another component until the one owner initialization succeeds. The factory uses `HostContext::initialize_new_record` for its schema initialization, gets `record()/author()/permissions()/action_context()/wake_handle()` accessors, and consumes the context in `AppComponent::new(context, root, registry)`. Construction exposes read/project access to an existing record; document mutation during factory setup must be an explicit schema migration through the owner, not an untracked `sync` seed. Required scope failure is typed and prevents factory invocation. Current permission handles are consulted again at action execution.

`ApplicationDefinition::instantiate(context, target, restore)` is the shared checked wrapper around its one `create` function: validate identity/target/restore, enter scope, invoke once, require the returned record to already be Ready and validate its binding, project/restore local View without document mutation, retain data on failure, and return a prepared component. Only RecordReservation::initialize transitions Reserved→Ready; instantiate never rebases, constructs a second UndoLog or starts autosave. A runner supplies context through `HostContext::prepare` (which resolves New vs existing before construction). No second native-specific root factory is introduced.

## Task 1: Make canonical record opening explicit and fail closed

**Files:** `app-state/src/{lib,autosave}.rs` at the paths above; `ui/src/record.rs`; new `ui/tests/canonical_record_owner.rs`; `ui/tests/it.rs`.

**Consumes:** `DocStore`, `Autosave`, `RecordSet`, `RecordStore`, `RecordCheckout`, `UndoLog`.

**Produces:**

```rust
// app_state; owner identity/path resolution is explicit and validated.
impl DocStore {
    pub fn open_canonical_at(package_id: &str, directory: &std::path::Path)
        -> Result<Self, StoreError>;
    // Build the private New candidate without I/O or store mutation.
    pub fn new_canonical_document(&self)
        -> Result<automerge::AutoCommit, StoreError>;
    pub fn validate_canonical_schema(&self, doc: &automerge::AutoCommit)
        -> Result<(), StoreError>;
    pub fn load_canonical(&mut self) -> Result<Option<CanonicalLoad>, StoreError>;
    pub fn accept_canonical(&mut self, loaded: CanonicalLoad)
        -> Result<automerge::AutoCommit, StoreError>;
    pub fn room_record_dir_at(profile_state_root: &std::path::Path,
        package_id: &str, room_key: &str, record_key: &str)
        -> Result<std::path::PathBuf, StoreError>;
}
// CanonicalLoad has private fields; load performs no document publication.
pub struct CanonicalLoad {
    doc: automerge::AutoCommit,
    source_bytes: Vec<u8>,
    repair_bytes: Option<Vec<u8>>, // valid recovered prefix, serialized in memory
}
impl CanonicalLoad {
    pub fn document(&self) -> &automerge::AutoCommit;
}
// New StoreError variants: WriterBusy { path }, EmptyExistingDocument { path },
// SourceChanged { path } (source/directory/gate identity no longer matches).
// InvalidSchema { path: std::path::PathBuf, reason: &'static str }.
// Keep the existing NewerSchema variant's u32 fields unchanged.
impl app_state::Autosave {
    // Borrows the retained canonical store; failure cannot consume its owner.
    pub fn try_spawn_canonical(store: &std::sync::Arc<DocStore>,
        debounce: std::time::Duration) -> Result<Self, StoreError>;
    // true: ended worker joined and successor started; false: live usable worker.
    // Never clears a durability error or acknowledges heads.
    pub fn try_restart_canonical(&mut self) -> Result<bool, String>;
    pub fn try_flush(&self) -> Result<(), String>;
    pub fn request_checkpoint(&mut self, doc: &mut automerge::AutoCommit);
}
// app_state; reexport DurabilitySnapshot from the private autosave module.
// One coherent read-only worker acknowledgement snapshot.
#[derive(Clone)]
pub struct DurabilitySnapshot {
    pub durable_heads: Option<Vec<automerge::ChangeHash>>,
    pub error: Option<String>,
}
impl app_state::Autosave {
    pub fn durability_snapshot(&self) -> DurabilitySnapshot;
}
// ui::record; MemoryOnly is explicit and must never be labelled durably saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordSaveStatus { Saved, Saving, Failed(String), MemoryOnly }
impl RecordStore {
    pub fn save_status(&self, id: &RecordId)
        -> Result<RecordSaveStatus, RecordOpenError>;
}

// ui::record; holds one RecordStore per package/workspace under this profile root.
pub struct RecordOwners { /* explicit root and weak owner map */ }
impl RecordOwners {
    pub fn new(profile_state_root: std::path::PathBuf) -> Self;
    pub fn open(&mut self, package_id: &str, workspace_id: &str)
        -> Result<RecordStore, RecordOpenError>;
}
pub enum RecordOpenOutcome { Loaded, AlreadyOpen }
// Not Clone; private owner generation prevents a shared store handle from
// initializing or cancelling another construction. Drop cancels only its
// own still-Reserved slot; after successful initialization Drop is inert.
pub struct RecordReservation { /* private store, checkout, generation */ }
impl RecordReservation {
    pub fn checkout(&self) -> &RecordCheckout;
    pub fn initialize(&mut self,
        initialize: impl FnOnce(&mut automerge::AutoCommit) -> Result<(), String>)
        -> Result<(), RecordOpenError>;
}
pub enum RecordOpenError {
    Store(app_state::StoreError),
    IdentityMismatch,
    MissingRecord,
    Busy,
    Initialization(String),
}
impl RecordStore {
    pub fn try_open_record(&self, want: &RecordCheckout)
        -> Result<RecordOpenOutcome, RecordOpenError>; // Existing only
    pub fn reserve_record(&self, want: &RecordCheckout)
        -> Result<RecordReservation, RecordOpenError>; // explicit New only
    pub fn is_ready(&self, id: &RecordId) -> bool;
    pub fn durability_error(&self, id: &RecordId) -> Option<String>;
    pub fn try_flush(&self) -> Result<(), String>;
}
```

- [ ] Add regression tests first. Construct `RecordOwners::new(tempdir.path().join("state"))`, never change HOME or process env. Two `open("dev.qos.test", "room-a")` calls share mutations and a single writer; the same record ID in room-b does not share data. Existing target with no file yields `MissingRecord`, never `Reserved`. Assert corrupt, empty-existing, newer-schema, wrong-record and wrong-workspace files retain byte-identical contents after failed open and attempted close; the initializer's call counter remains zero. A fresh explicit New path reserves an ID without creating document bytes, history or autosave; initialization runs once and changes Reserved→Ready, while reopening does not initialize again. A second opener sees Busy while the reservation is initializing. An initializer error retains no partially initialized live document, publishes no document bytes and leaves the reservation owned by its construction context for retry/cancellation; a second initializer after success is rejected without running its closure.

```rust
// Core assertion sequence in canonical_record_owner::corrupt_is_not_a_new_record.
let original = b"not an automerge document".to_vec();
std::fs::write(&document_path, &original).unwrap();
let result = store.try_open_record(&checkout);
assert!(matches!(result, Err(RecordOpenError::Store(_))));
assert_eq!(std::fs::read(&document_path).unwrap(), original);
assert!(store.read(&checkout.id, |_| ()).is_none());
```

Test path construction uses the production explicit resolver, not duplicated layout constants. In app-state unit tests use its private constants and explicit test directory. Add child-process lock tests by launching the current test executable with `--exact` on a dedicated lock-holder test and a unique path argument/environment variable scoped only to that child. A pipe/file handshake proves the holder acquired the gate; no timing sleeps. Cover canonical→canonical, canonical→legacy, legacy→canonical refusals, legacy→legacy permitted behavior, release after orderly drop and process termination. Both refused opens leave actor and document bytes unchanged.

- [ ] Run focused failing tests through the helper:

```sh
CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run --locked --profile ci -p app-state -E 'test(canonical_)'
CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run --locked --profile ci -p ui --test it -E 'test(canonical_record_owner::)'
```

- [ ] Implement shared/exclusive gate acquisition before actor creation/loading or autosave. Extend existing lock acquisition code to return errors and support shared mode; hold its file in DocStore until the canonical owner/recovery handle and its last worker have both released it, including through startup/restart failure and unexpected worker exit. Every revised writable opener takes this gate; canonical uses exclusive and legacy uses shared, while legacy's actor lock behavior remains intact. `open_canonical_at` validates package and the absolute directory path, rejects a non-directory existing path without `ensure_state_dir` legacy migration, and never resolves package/workspace via environment. Add path derivation under RecordOwners using app-state's existing validated package/room/record layout helpers; expose a shared explicit-root resolver rather than reproduce layout in ui.

- [ ] Implement `load_canonical` as read/decode only: nonexistent file is absence; empty-existing, read and unparseable data are errors. First load with `LoadOptions::new().on_partial_load(OnPartialLoad::Error)` (the pinned Automerge default); on a strict decoding error retry the existing `OnPartialLoad::Ignore` behavior and accept a recovery candidate only when it yields a valid document with the required schema/identity. The pinned loader does not expose a distinct public PartialLoad error variant, so do not match an invented one; a failure in both modes is unrecoverable. Validate every complete/recovered candidate with `validate_canonical_schema` as specified below; schema rejection is not a decoding error and never triggers another fallback. If a valid prefix is recovered, retain original `source_bytes` plus `doc.save()` repair bytes in CanonicalLoad; no save/append/atomic replacement occurs here. Failure to obtain a valid document remains an error with bytes preserved. `CanonicalLoad::document()` is immutable so UI validation cannot accidentally mutate the proposed recovery document.

- [ ] In `RecordStore::try_open_record`, validate record ID/kind against `want` and room_scope against the explicit store using the existing readers and a nonmutating counterpart to `init_scoped`; require a valid nonempty home but preserve its original minting room, which may differ from the current replica workspace. Do not require home == workspace or rewrite home; canonical existing data with missing/mismatched identity is an error, not an identity-stamping opportunity. Only after app-state schema and these UI record checks succeed call `DocStore::accept_canonical`. That method consumes the exact CanonicalLoad from this opener, verifies that the source bytes still match, then publishes pending repair through the strict canonical `DocStore::write_full` branch using `atomic_file::write` plus the physical ancestor synchronization barrier below, sets the actor/persisted heads only after success, and returns the document. Repair/publication-barrier failure returns an error and starts no autosave; a failure after rename may leave complete repaired bytes visible and does not promise byte rollback. For a complete valid file it performs no document write, but syncs the existing document and physical directory chain before seeding acknowledged heads. No legacy fallback/migration-claim helper is called. Keep `CanonicalLoad` tied to its opening path/gate with a private source identity, so a load from store A cannot be accepted by store B.

- [ ] Test recovered-prefix files carrying wrong record, wrong room, wrong kind or newer schema: compare original bytes before failed open, after dropping the failed open and after attempted flush; all remain identical and no writer starts. Test valid partial-tail recovery is published only after validation, survives a new edit/flush/reopen, and a changed source before acceptance yields SourceChanged without overwriting it. This is publication after validation, not publication during load.

- [ ] Add RecordOwners weak registry keyed by validated `(package_id, workspace_id)` under its fixed profile root, and canonical mode in RecordSet. `try_open_record` distinguishes outcomes and checks already-open identity/kind. An explicit existing target never creates; New mints a RecordId, rejects collisions and installs a construction-owned Reserved slot holding Arc<DocStore>, its opened exclusive gate and identity, with no published document, UndoLog or Autosave. `RecordReservation::initialize` is the sole once-only phase transition: build the new document privately with `new_canonical_document`, stamp identity/home/room, run the app schema closure, validate schema and UI identity, and commit setup. Only successful fallible `try_spawn_canonical` permits creating one UndoLog baseline and installing Ready. The worker receives no document message before Ready; then queue the initial full snapshot without seeding durable heads. On a pre-Ready initializer/validation/startup error discard the private candidate, retaining the same reservation generation, Arc and original error for its owner; startup failure returns `RecordOpenError::Store`, never a disabled canonical Ready record. The same token may retry initialization because no Ready transition occurred. Release the RecordSet RefCell borrow while invoking the app closure so another handle observes Busy rather than a borrow panic. A queue/write failure after Ready is a retained durability failure, not rollback or another initialization. On constructor cancellation before Ready, release the reservation without publishing it. Keep canonical reservations in a separate pending map so legacy `Parked.doc == None` retains its checked-out meaning; reservations are not discoverable through read/with/open_records. Bind the reservation to its construction context and cancel only that owner's still-Reserved slot on failure/drop. `reserve_record` returns the unique non-Clone RecordReservation shown above; it carries a private owner generation and owns a strong store handle. Only this token can initialize/cancel that reservation. `HostContext` owns it in an Option and its mutable `initialize_new_record` accessor delegates to the token. A second RecordStore handle can neither initialize by public record ID nor cancel another reservation. Test drop of the owning token releases an uninitialized reservation, drop after Ready preserves the record, and a stale token cannot cancel a newer generation. `try_open_record` has no create boolean: it is Existing-only; New always uses the explicit reservation operation. A factory that needs no domain fields still calls this seam with `|_| Ok(())`. Only this method can finalize New; later projection, View restoration and mount do not finalize/rebase it. For the first valid Existing open, accept the load before wrapping the store in Arc, then start its canonical Autosave fallibly and create one UndoLog only after startup succeeds; a startup failure installs no Ready slot, and an already accepted valid repair remains reopenable; AlreadyOpen only borrows the existing Ready slot and never takes a new baseline. Failed later factory/View setup retains any successfully initialized record for retry through its exact record ID and never reinitializes it. Existing `open/open_record/checkout` retain their signatures and legacy behavior outside canonical mode. Never move the canonical document out into an outer `State` via checkout.

- [ ] Add `Autosave::try_flush` acknowledgement containing the actual writer result and a failure if the worker/ack channel closed. Preserve `flush()` as its legacy wrapper. `RecordStore::try_flush` requests current record changes once then awaits each writer and returns record-specific errors; `durability_error` reflects `last_error`. Test actual file failure (replace the document path with a directory inside the temp fixture) and worker error propagation. The current writer takes pending bytes before I/O and advances `sent` when enqueueing, so adding an acknowledgement alone cannot make retry durable. Implement an explicit failed-writer state: any failed Full/Append remains a sticky failure, ordinary Append messages cannot clear it or extend a possibly partial file, and an empty Flush returns that failure. `Autosave::request_checkpoint` always serializes a complete current document snapshot independent of `sent`; it advances `sent` only after successful enqueue, and an enqueue failure records a shared failed/unavailable status. Only successful strict atomic publication of an explicit Full checkpoint clears the failure and acknowledges durability. On a failed-record retry, `RecordStore::try_flush` first calls `try_restart_canonical`, then requests this checkpoint from its retained canonical document and waits for the actual writer result; a live usable worker is not restarted. If a failure races with the first flush, return it; the next explicit retry sends the checkpoint. No error is cleared because there were no pending bytes. Keep ordinary healthy incremental writes unchanged. A failed append may have partially reached disk, so never blindly append its bytes again. Add a deterministic writer-unit regression using the existing commit boundary with a test-only injected write failure after a prefix, plus a filesystem regression: fail flush, repair destination without a new document edit, call owner try_flush again, drop all owners and reopen, and require all latest edits and healthy status. Also test an edit queued after failure is preserved by the full retry and the retry does not duplicate any Automerge change. No public test-only fault service is introduced. Test normal save/reopen and valid-prefix crash recovery using existing app-state fixtures. Prove repeated store handles do not schedule duplicate writes by existing full_writes/appends counters in module tests.

- [ ] Add the read-only durability contract above while implementing the actual writer acknowledgement. Store durable heads and sticky error in one shared mutex snapshot. Advance heads and clear an error together only after the corresponding strict write/publication result succeeds; worker exit and channel/ack failures also set a sticky error but never advance heads. Full/Append queue messages carry the snapshot's heads; coalescing preserves the newest included heads, and successful filesystem publication acknowledges exactly those heads. An older checkpoint completing while newer edits exist must not label them saved. Keep `last_error` compatible by reading this same snapshot. Seed acknowledged heads only for a validated existing on-disk document after accept_canonical; a New private initialization must queue its initial full document and must not call seed as if those bytes were already durable. A first initialization can atomically publish before a later explicit flush, but success of enqueue alone never proves durability.

  `RecordStore::save_status` is read-only: Failed takes precedence, an explicit in-memory store reports MemoryOnly, any pending ops or mismatch between current committed base heads and acknowledged heads reports Saving, and only matching acknowledged heads with no pending ops reports Saved. Use pinned Automerge0.8 `Transactable::pending_ops(&self)` and `Transactable::base_heads(&self)`; its `AutoCommit::get_heads(&mut self)` closes a transaction and must not be called merely to poll status. Missing/Reserved records return MissingRecord/Busy. Do not flush or mutate from this accessor. Add real writer tests with a narrow private completion barrier: queued full write is Saving, actual background acknowledgement becomes Saved without another edit, newer pending edits remain Saving after an older acknowledgement, failure stays Failed until successful full checkpoint, and repeated status polling leaves a real pending typing transaction's operation count/base heads/history unchanged. Cover MemoryOnly explicitly. Export through existing ui/qos-app facades. Notes later consumes this status through a bound widget hook; it does not implement another writer.

### Canonical writer, publication, and schema contracts

These controller-reviewed corrections use only the existing Task 1 files and dependencies. Keep legacy `Autosave::spawn`, `DocStore::autosave`, `flush`, `schema_version_of`, legacy `load`, and legacy `atomic_file` behavior unchanged. Canonical mode is explicit; it does not silently fall back to legacy or MemoryOnly.

- [ ] `new_canonical_document(&self)` returns a fresh actor/current-schema candidate without changing persisted heads; propagate a schema-stamp error. It exists because legacy `new_document(&mut self)` resets store bookkeeping. `validate_canonical_schema` also runs after the New app initializer, before Ready. These DocStore methods reject legacy-mode misuse via `StoreError::Io` / `InvalidInput`; UI record identity validation remains in ui::record. Reexport `DurabilitySnapshot` from app-state's root because `autosave` is private; module-qualified record exports already pass through qos-app. Keep the assigned file list unchanged.

- [ ] Retain a canonical Arc<DocStore> in Autosave outside its worker, and give the existing writer loop a clone; no AutoCommit crosses threads and the loop continues using `write_full(&self)`/`append_bytes(&self)`. A private atomic one-handle claim/lease on DocStore prevents two `try_spawn_canonical` calls on cloned Arcs from starting concurrent writers under one gate. The lease stays with the handle through restart and releases on handle drop after joining its worker; failed initial spawn releases the claim without consuming the caller's Arc. Use WriterBusy for duplicate-handle refusal and StoreError::Io for thread creation errors. A private worker-exit guard marks unexpected return/unwind unavailable in the shared status; channel/ack failures do likewise. Known worker death must not leave Saved. Expected Drop retains Stop/join behavior.

- [ ] `try_restart_canonical` returns `Ok(false)` for a live usable worker, including an I/O-failed worker able to accept a checkpoint. If unavailable but its JoinHandle is not `is_finished()`, return a retryable stopping error without starting another worker or waiting indefinitely. Once finished, take and join the old handle, discard its sender, and only then spawn one successor with the retained Arc/lease. A failed successor spawn keeps the same owner/lease and sticky error; with the old worker already joined, a later explicit retry may spawn again. No automatic supervisor, timer, or replacement opener is added. The successor retains acknowledged heads/error and requires an explicit full checkpoint; neither thread startup nor an empty queue clears failure.

- [ ] Distinguish an explicit checkpoint from ordinary compacting Full using a private message flag. While failed/needs-checkpoint, ordinary frontend `request` neither enqueues nor advances `sent`, and the failed worker rejects already queued ordinary Full/Append without acknowledging them. A checkpoint always captures the full current document. A failed send sets the shared error; the existing unit-returning method is followed by `try_flush`. After successful strict checkpoint publication, healthy requests derive deltas from its sent heads. An edit arriving after that snapshot must remain unsent/Saving until a subsequent request/flush. Keep healthy Full/Append coalescing, associating bytes with the newest included heads. RecordStore::try_flush owns this explicit restart/checkpoint/ack sequence; a failure racing its first healthy flush is returned for the next explicit retry. Preserve current document/history and never reinitialize or rebase on recovery.

- [ ] Canonical Saved means successful platform `File::sync_all` of the published document and synchronization of the directory entries needed to reach it; it is an OS acknowledgement, not a hardware-failure guarantee. Propagate every barrier error, including Unsupported/InvalidInput. Canonical open validates/creates directories without legacy stale-file migration, acquires the gate before actor creation, retains opened directory/gate identity, and syncs the physical directory chain after actor setup before returning. Resolve the now-existing physical directory for traversal only; keep the caller's explicit layout/path identity. Sync leaf-to-root through the filesystem root: the existing opener supplies no narrower trusted durability anchor. No directory contents are enumerated, and no failed directory chain is removed.

- [ ] Each canonical full publication calls existing `atomic_file::write`, then explicitly syncs that complete physical ancestor chain before acknowledging heads/clearing errors. The helper's ignored directory-sync result cannot substitute for this barrier. Repeat the chain on every canonical open/full publication, including retry when directories already exist, so ancestors left visible after failed creation remain covered. A post-rename sync error may leave complete new bytes visible; retain Failed and retry a full checkpoint without claiming rollback. Complete Existing acceptance performs no document write, but syncs its existing file and required directory chain after validation/source comparison and before acknowledged-head seeding. Valid-prefix repair uses the same strict full-publication branch. Private store helpers suffice; no dependency or new filesystem service is added.

- [ ] Canonical writes must not inherit `write_full`'s unconditional `create_dir_all` once a gate is held. Missing/replaced owned directory or gate identity is SourceChanged/failure; do not recreate a path under a stale gate. Check retained directory/gate metadata on the supported Unix hosts; the cooperative advisory-lock limitation remains. Healthy canonical append uses write_all/file-sync but does not create a missing document file: fail sticky and repair only by a full checkpoint. It need not repeat the established ancestor barrier per append. A filesystem unable to meet the strict contract fails canonical open/publication rather than silently downgrading it.

- [ ] Strict schema validation reads every live `qos:schema_version` value via pinned `ReadDoc::get_all`. Absence keeps legacy compatibility without stamping/mutation. Every present value must be scalar Uint or nonnegative Int within u32; checked normalization happens before comparison. Any newer in-range value returns existing NewerSchema, even if a supported value wins `get`; any malformed/out-of-range value returns InvalidSchema. Reject negative integers, overflowing integers, floats (even integral-looking), strings, bools, nulls, timestamps, counters, bytes and objects without numeric coercion. Read errors become StoreError::Automerge, never absence. If all live assignments normalize to the same supported value, accept nonmutatingly, including Int/Uint representations; identical concurrent assignments are legitimate. Differing valid supported values return InvalidSchema with reason `conflicting schema versions`. Values at/below the understood version keep the current comparison behavior; no general conflict resolver/migration is introduced. Use this validator for complete loads, recovered candidates before publication and New after initialization. Schema rejection never triggers the decoding-recovery fallback. The earlier reject-all-multiplicity preflight suggestion is superseded.

- [ ] Add the controller's bounded private regressions alongside the original tests. Use per-test/per-store injection or a private spawn/write helper, real channels/barriers and child-process lock handshakes; no public fault API, process-wide failure flag, sleeps or dependencies:

  1. Initial spawn failure preserves the same Reserved token/generation/Arc, blocks second canonical and legacy openers, creates no Ready/history/document bytes, and permits one later successful initialization. A second successful initialization remains forbidden. Two startup calls on one Arc cannot create two handles.
  2. Unexpected exit/unwind after receipt but before acknowledgement reports Failed and retains exclusivity. Restart refuses while teardown is unfinished, joins before one successor, and retains owner/lease if successor startup fails. Full checkpoint/reopen recovers all latest edits with no duplicate Automerge changes or history rebase; private active-worker count never exceeds one.
  3. Block recovery checkpoint completion, edit/request again, then acknowledge the older snapshot: status remains Saving, and the next healthy request/flush saves the later edit, proving failed-state skips did not advance sent.
  4. Inject explicit leaf/ancestor sync failure after real atomic replacement: bytes may be visible but no head acknowledgement/error clearance occurs. Repair the fault and retry without a new edit, then reopen. Cover valid-prefix repair and reopening an earlier visible but unacknowledged replacement.
  5. Start with missing profile/package/room/record ancestors; record actual barrier visits while calling real sync, fail an ancestor obligation, then retry after the directories already exist and require the same obligation. A missing document refuses append until full retry; an absent/replaced owned directory is never recreated under the old gate.
  6. Table-test malformed, negative, noninteger, out-of-range, supported, newer and absent schema values in complete/recovered files with unchanged bytes/no writer on refusal. Build a real conflict with a supported winner and unsupported loser and require refusal; swap actor assignments if needed rather than invent a winner order. Also require differing supported versions to fail and independently assigned identical supported normalized Int/Uint values to succeed without resolving/mutating assignments. Preserve legacy tolerant results.

The controller's current-filesystem syscall probes support API availability only: native ordinary fsync and Rust File::sync_all's actual Apple F_FULLFSYNC passed on the owned file/physical ancestors, as did the matching Fedora fsync probe. They do not replace any Task 1 tests, private failure cases or exact-SHA gates. Receipts remain in the SDD ledger (`native-directory-sync-preflight.json`, `native-rust-sync-all-preflight.json`, `fedora-directory-sync-preflight.json`). These syscall probes do not establish implementation or runtime qualification.

- [ ] Rerun these selections plus existing app-state and record unit regressions; execute mandatory Task 1 check/clippy/format gates above on native. After self-review and native gates, commit only this task's explicit file set; the controller then reviews and executes those gates on Fedora at that SHA: `fix(state): enforce canonical record ownership and fallible opens`.

## Task 2: Add the canonical definition, explicit construction and local restore envelope

**Files:** new `ui/src/{application,application_view}.rs`; `ui/src/{lib,launch,view}.rs`; `ui/src/app.rs` for existing FreezeThaw View delegation and the private explicit construction-input seam below; `ui/src/collaboration.rs` only for a crate-private disabled-manager constructor reused by existing failure branches; `ui/src/record.rs` only for the crate-private read-only canonical namespace seam described below; `crates/app/qos-app/src/lib.rs`; new `ui/tests/application_contract.rs`; shared test probe and module registrations.

**Consumes:** Task 1 RecordOwners/RecordStore and all API vocabulary above.

**Produces:** `ApplicationDefinition`, `HostContext`, `LaunchTarget`, `RestoreInput`, `AppComponent`, `ApplicationCatalog`; `SavedInstanceView`/`InstanceViewStore`; definition-based preparation independent of a native runner.

```rust
// application_view; existing record identifiers serialize as validated strings.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SavedInstanceView {
    pub version: u32,                  // exactly 1 in this slice
    pub app_id: String,
    pub device_id: String,
    pub workspace_id: String,
    pub instance_id: u64,
    pub record_id: String,
    pub record_kind: String,
    pub view: crate::view::View,
}
pub struct InstanceViewStore {
    // Private validated profile/device/workspace namespace and resolved directory.
    directory: std::path::PathBuf,
    device_id: String,
    workspace_id: String,
}
impl InstanceViewStore {
    pub fn new(profile_state_root: std::path::PathBuf, device_id: &str, workspace_id: &str)
        -> Result<Self, crate::view::ThawError>;
    pub fn load(&self, instance_id: u64) -> Result<Option<SavedInstanceView>, crate::view::ThawError>;
    pub fn save(&self, instance: &SavedInstanceView) -> Result<(), crate::view::ThawError>;
}
// HostContext::prepare takes definition ID/kind, explicit store, device/instance,
// author/current permissions/provenance and target; it allocates fresh WakeScope.
// AppComponent::new consumes the prepared context and real AppWidget+registry.
impl AppComponent {
    pub fn capture_local_view(&self) -> Result<crate::view::View, crate::view::ThawError>;
    pub fn restore_local_view(&mut self, view: &crate::view::View) -> Result<(), crate::view::ThawError>;
    pub fn close(&mut self) -> ComponentCloseReport;
    pub fn close_report(&self) -> Option<&ComponentCloseReport>;
}
#[derive(Clone)]
pub struct ComponentCloseReport {
    // Retained whether local save or record flush succeeds or fails.
    pub recovery: Option<SavedInstanceView>,
    pub view_error: Option<String>,
    pub view_retry: Option<std::rc::Rc<InstanceViewStore>>, // exact store while a local View save failed
    pub record_error: Option<String>,
    pub record_retry: Option<crate::record::RecordStore>, // strong owner while a dirty flush failed
    pub teardown_error: Option<String>,
}
impl ComponentCloseReport {
    pub fn retry_view(&mut self) -> Result<(), crate::view::ThawError>;
}
```

`HostContext::prepare` owns the supplied `Rc<InstanceViewStore>` for the full component lifetime and validates its profile/device/workspace namespace against the RecordStore and explicit device. `close()` and `suspend()` use that exact store; their callers supply no replacement store and no runner discovers a profile after construction. The store may be shared by sibling components within the same namespace; each envelope still has its own instance key. On View-save failure the report retains both `recovery` and `view_retry`; `retry_view` atomically saves that envelope to that same store and clears the error/retained store only after success. It never reconstructs a store from the current environment or reopens a closed mount. In Task 2, test a preparer-local store whose other handles are dropped, then call the common component close directly and retry a failed save after that component is dropped. The original explicit namespace must receive the View, and a wrong-profile/device/workspace store must fail before factory invocation. Task 5 supplies the actual native/headless/embedded runner close and retained-report conformance tests after its runner adapters exist; Task 2 must not substitute simulated runner labels for that later integration evidence.

Expose a crate-private, read-only canonical namespace snapshot/accessor from `RecordStore` for this validation: explicit profile root, package ID and current workspace, with no canonical namespace for legacy/in-memory stores. It must not expose a mutable owner, resolve environment values, open another writer, mutate a document or publish a path-bearing service to apps. Retain profile identity directly in `InstanceViewStore` rather than infer it by walking its derived directory. Use the final Task 1 owner layout; the existing wrong-profile/device/workspace tests must exercise this real seam before factory invocation.

- [ ] Write definition tests with one deliberately small **test-only** AppWidget probe, exported nowhere in production catalogue. Its factory increments a counter, reads the selected record, registers a real test action, and creates `LocalState` during construction. Invoke checked construction with New twice and assert distinct record IDs; invoke existing twice and assert same record/different local View. Duplicate catalogue registration returns `LaunchError::DuplicateApplication`; supported entries always contain a factory by type. Unknown ID returns `UnknownApplication` rather than fabricated content. Required permission denial prevents factory execution. Optional denial is discoverable but not a construction failure.

```rust
let a = instantiate_probe(&mut host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
let b = instantiate_probe(&mut host, 12, LaunchTarget::New, RestoreInput::None).unwrap();
assert_ne!(a.record().id, b.record().id);
assert_eq!(factory_calls.get(), 2);
assert!(catalog.register(probe_definition()).is_err()); // second same package ID
```

Define `instantiate_probe`, `probe_definition` and counter in `support/application.rs` using explicit temp RecordOwners, explicit `PermissionContext::from_capabilities`, and explicit `UserId`; do not call `local_user_id()` or `PermissionContext::permissive()` as a production default.

- [ ] Write restore tests before implementation. Save View A, mutate the canonical record through B, close/reopen A with its old envelope, and assert B's changes remain and A's local scroll/focus restores independently. Foreign app/workspace/record/instance binding, malformed bytes, unknown version and nonfinite View geometry return typed errors with source bytes unchanged. A LegacyRecovery input validates `FrozenApp.version`/state but returns `RecoveryRequired` and never replaces canonical state. A failed factory keeps existing record and View bytes; it closes its lifetime and exposes no mounted callback target. For device D/workspaces A and B, save the same instance ID 11 with distinct app/record/View values, reload both scoped stores and prove neither overwrote the other; the same checks cover distinct devices within one profile. A wrong-device/workspace envelope at a valid key fails without replacement. App/record/kind/instance mismatches against HostContext also fail without modifying either file.

- [ ] Run `cargo nextest run --locked --profile ci -p ui --test it -E 'test(application_contract::)'` via the isolated helper with the native CARGO_TARGET_DIR above and observe intended failures.

- [ ] Implement the API vocabulary and `ApplicationCatalog::{register,resolve}` using package identity, not `AppKind`. Factory is a function pointer; catalogue only stores real supported definitions. Missing/unavailable fleet metadata remains in the later shell catalogue/capability map, with no manufactured GUI factory for Agent/OSK. Add typed LaunchError variants `Record(RecordOpenError)`, `Restore(ThawError)`, `DuplicateApplication(String)`, `UnknownApplication(String)`, `CapabilityDenied(PermissionScope)`, `BindingMismatch`, `RecoveryRequired`; preserve existing CLI error displays/exits. Remove the legacy Clone/PartialEq/Eq derives because typed retained errors cannot satisfy them; replace local CLI equality assertions with exact variant/payload matches, without stringifying errors or manufacturing equality. Add a retained construction failure variant `ComponentSetup { source: Box<LaunchError>, record: RecordCheckout, records: RecordStore }` for failure after a record reached Ready. The checked wrapper retains a store handle and checkout before consuming HostContext into the factory; on factory/restore failure after initialization it wraps the original typed error with that exact binding and strong owner. The host uses it for retry through Existing and must retain it independently of an error tile. Test New initialization succeeds then factory/restore fails, drop all other handles, and recover the exact minted record and content from the error; no second initializer runs. Before Ready, drop the unique reservation and return the original failure without publishing an empty record. Existing persisted View bytes remain untouched; a failed supplied restore input is retained by the caller and is not saved over them.

- [ ] Put common construction order in one checked implementation. Validate required authority/target/restore envelope → open Existing or reserve New → allocate scope/context → enter scope for factory/action registration, with New factory calling the one RecordReservation::initialize transition before its read-only canonical projection through HostContext::read_record → require Ready and matching binding → restore validated local View → return prepared, unmounted component. No finalization follows View restore. `initialize_new_record` is unavailable after initialization or on an existing record and never rebases another mounted instance. Test a factory that omits initialization is rejected as unready, and count one initialization/UndoLog construction/autosave across prepare→factory→restore→mount and second-instance read-only mount. Faults after successful initialization retain Ready state and retry through Existing rather than rerunning the initializer. Scope closes on every failure path; pending construction wakes survive until mount. Factory registration travels in AppComponent, never via native `configure_app`.

- [ ] Extract the component-local parts of current `App` FreezeThaw capture/restore into reusable helpers in `view.rs`: map only this component's focus order, use existing root `view_state/restore_view_state/get_state/set_state` hooks, and resolve against the current component tree. The existing implementation is in `ui/src/app.rs`; adapt only those View methods to consume the helpers so this is a real extraction rather than copied logic. Preserve its wake-scope entry, focus ordering/restoration, dirty/sync effects and legacy whole-state FreezeThaw behavior. Runner component mounting and general App callback/dispatch changes remain Tasks 3–5. Default View does not invoke external effects. Use InstanceViewStore's explicit `(profile_state_root, device_id, workspace_id)` namespace and `atomic_file::write`. The directory is `profile_state_root/instance-views/<hex UTF-8 device ID>/<hex UTF-8 workspace ID>` and the filename is the decimal u64 instance ID plus `.postcard`; reject empty/invalid identity inputs and use reversible hex encoding so IDs cannot escape or collide as path segments. Envelope device/workspace must equal the store namespace; instance must equal the requested filename; construction separately validates app/record/kind against HostContext. Do not use an unscoped `load(instance_id)` beneath only a device root. Do not add an application-specific selected-page schema here. If app-local fields cannot round-trip through existing View, return a documented unsupported contribution for the Notes slice rather than serialize the whole document into the View.


- [ ] Preserve the two existing real-App FreezeThaw tests using one private explicit construction-input seam. Keep `App::with_parts` as the sole field initializer and preserve public constructor signatures, wake ownership, production dependency producers, discovery ordering and root-callback ordering. A private lazy provider or phased inputs may supply production values at their current points; do not eagerly move late presence/surface/scale lookups ahead of earlier callbacks just to populate a bundle. Add a crate-only `#[cfg(test)]` controlled App constructor which delegates to that initializer with `font_catalog::fixture_renderer()`, `Theme::dark()`, deterministic author/presence/surface identity, default accessibility and unit scales, disabled collaboration, no theme stamp/debug/profiler/floating client/TTS/continuation/startup presence scope. Add only `CollaborationManager::disabled()` and use it in existing disabled failure branches; no new transport or lifecycle mode. Change exactly the three constructor expressions in `view::tests::{freeze_thaw_round_trips_doc_and_view,thaw_rejects_bad_bytes_without_panicking}` to that controlled adapter; keep their actual App, init_layout, FreezeThaw, Drop and assertions. No parallel stripped-down initializer, public policy bypass, process-global environment toggle or HOME rewrite. Verify the controlled transitive closure before runtime. This retains required legacy regression coverage without system/home/network font discovery.

- [ ] Add exports and rerun focused tests plus existing FreezeThaw/launch parsing tests; execute mandatory Task 2 check/clippy/format gates above on native. After self-review/native success, create the source commit; the controller then reviews and executes those gates on Fedora at that SHA: `feat(ui): define shared application construction and local restore`.

## Task 3: Bind every widget callback, including raw descendants, to the instance

**Files:** `ui/src/{application,bound_dispatch,wake,app,headless,accesskit_bridge,a11y_tree,lib,undo,view}.rs`; `ui/src/record.rs` only for the crate-private owner-identity and optional checked-borrow seams below; `ui/src/widget/{mod,app_widget,tile_host,column,row,container}.rs` (the three transparent containers preserve mutable operation forwarding without repeating the legacy fallback); new `ui/tests/bound_component_dispatch.rs`; `ui/tests/{it,support/application}.rs`.

**Consumes:** Task 2 AppComponent/context with Task 1 canonical record handle.

**Produces:** `TileHost::from_component`, runtime-only CallbackBinding, bound node map/helper, complete lifecycle and callback routing used by App and Driver.

```rust
// ui::bound_dispatch; not serialized and not an owning widget reference.
#[derive(Clone)]
pub struct CallbackBinding {
    // Private: fresh lifetime handle, instance ID, record store/checkout,
    // current permission handle, authenticated author, operation context.
}
pub struct BoundNodeMap {
    // Existing NodeId -> *mut dyn Widget stays unchanged for Widget implementers.
    // Every admitted node has metadata: explicit unbound outer, or the nearest
    // component binding propagated through the matching Taffy topology.
}
// Public because FrameBuilder consumes this same implementation. Export
// `pub mod bound_dispatch` from ui/src/lib.rs; keep all map fields private.
impl BoundNodeMap {
    pub fn new() -> Self;
    // Safety: root, taffy, layout_root and nodes must be from the same live
    // build_layout result under one exclusive tree owner. No replacement,
    // rebuild or destruction may occur during this call.
    pub unsafe fn rebuild(&mut self, root: &dyn crate::widget::Widget,
        taffy: &taffy::TaffyTree<()>, layout_root: taffy::NodeId,
        nodes: &std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>);
    pub fn clear(&mut self);
}
#[derive(Debug)]
pub enum BoundDispatchError {
    MissingNode,
    ClosedLifetime,
    CrossRecordReentry,
    Record(crate::record::RecordOpenError),
}
// Safety: admitted open/unbound delivery requires exclusive ownership of the
// live matching tree/topology and no overlapping target/ancestor reference.
// Closed stale metadata may retain dead pointers: reject it before dereference.
// No replacement/rebuild/destruction during admitted delivery; nothing escapes.
pub unsafe fn invoke_bound_node<R>(
    bindings: &BoundNodeMap,
    nodes: &std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    node: taffy::NodeId, outer: &mut automerge::AutoCommit,
    outer_root: &automerge::ObjId,
    callback: impl FnOnce(&mut dyn crate::widget::Widget,
        &mut automerge::AutoCommit, &automerge::ObjId) -> R,
) -> Result<R, BoundDispatchError>;
// New default Widget hook; implemented by the bound component root/TileHost.
fn callback_binding(&self) -> Option<crate::bound_dispatch::CallbackBinding> { None }
// Internal WakeHandle method validates live route, enters it without owning it.
// Closed must return Err; it must not fall back to the outer/current scope.
// pub(crate) fn enter_if_open(&self) -> Result<WakeScopeGuard, WakeBindError>;
// Teardown uses the OWNED WakeScope: close(); enter(); then cancellation/drop.
// Existing WakeScope::enter preserves a closed route (unlike enter_detached).
// No public dispatch helper may enter closed routes to execute ordinary work.
```

This is the single producer contract for App, Driver and the later shell FrameBuilder consumer. `BoundNodeMap::rebuild` consumes the live Taffy topology and returned layout root from the same `build_layout` call; while that matching tree/topology is stable, it may inspect only the structural metadata needed to propagate the nearest binding to layout-only descendants, with nested bound roots overriding ancestors. It records every admitted raw node, including explicit unbound outer nodes. Use normal safe retained-reference traversal wherever it can represent the callback. An empty or cleared entry, or a NodeId whose current raw pointer differs from the admitted pointer, returns `MissingNode` before raw-pointer dereference; absence is never interpreted as unbound compatibility. `invoke_bound_node` cannot detect an otherwise changed topology because it receives no live Taffy tree or generation token; matching live topology and no replacement during admitted open/unbound delivery remain unsafe caller obligations. Refused closed metadata may retain actual old raw pointer values after replacement, provided its binding closed before destruction; those entries are checked and refused without dereference.

Same-record reentry reuses the active document borrow, never reacquiring its RefCell; it does not permit aliasing an already borrowed target widget or ancestor, and `invoke_bound_node` requires that no overlapping live widget reference exists. Synchronous reentry from an active mutation into another record returns `CrossRecordReentry` before executing the callback or borrowing the target; Task 3 does not queue an arbitrary borrowed `FnOnce`. A future owned typed intent API may schedule such work after the active borrow ends. A requested closed/missing component binding fails before callback and never falls back to the outer document. Unbound shell widgets use only metadata explicitly recorded as unbound and keep their supplied outer document. Synthetic accessibility/resource nodes without Taffy IDs retain their originating CallbackBinding through common guarded traversal; the raw-node helper does not reconstruct that origin.

`HostContext` is the only producer of a complete component binding: add one private/crate-private constructor method which passes the existing fresh lifetime, instance ID, exact record store/checkout, shared current permission handle, authenticated author and operation context without exposing those fields. The common entry owns the active-binding state and record borrow. If that entry cannot prove all initial store borrows are mediated, add only a crate-private checked `RecordStore` callback using `try_borrow_mut` and map contention to `RecordOpenError::Busy`; do not change persistence, writers or schema.


Task 3 implementation rulings: `RecordStore` may additionally expose only a crate-private `same_owner(&Self)` using `Rc::ptr_eq`; same-record reentry requires that exact owner and canonical record ID/kind, not namespace or instance strings. The existing widget lifecycle has Accepted invocation admission and `poll_resource_operation(id, cancellation_requested, ...)`, with no generic subscription hook. Track actual Accepted IDs, remove terminal ones, and request cancellation through that existing hook under the owning CLOSED route. Then drop all app-owned root/operation/handle captures under the same guard even on unwind. Cover concrete LatestRequest/LocalState and operation completion/cancellation captures; drop any concrete app-owned subscription handles there, but do not invent a subscription API or claim transport-level unsubscribe. Later service-owner plans remain responsible for real broker subscription cleanup.


The spec §6 explicitly excludes legacy `AppCommand::invoke` callbacks from portable hosting: commands are discovered from `Widget::resources()` and executed through the existing typed action door in Task4. Therefore the canonical bound adapter returns no legacy `AppCommand` contributions and does not call the inner legacy `commands()` producer. Keep `AppCommandInvoke`'s public `Send + Sync` signature and unmigrated legacy content unchanged. A focused probe with nonempty legacy contributions must show that the canonical adapter neither constructs, exports nor invokes those callbacks. Do not add forwarding-index registries, an alternate command schema or thread-affine values behind an unsafe Send wrapper. This is an explicit exception to blanket AppWidget forwarding, mandated by the spec; no legacy contribution exists in the currently inspected app fleet.

- [ ] Write callback probes that create a LocalState/LatestTask in each relevant callback. Build an ordinary container → TileHost → container → focusable leaf with both retained and Taffy descendants. Bind two instances to separate wake collectors, with a separate outer App scope. Drive root event plus **direct** leaf focus, IME, accessibility semantic action and measure paths; complete captured work and assert only the intended collector receives acceptable WakeEvents. Have the leaf read/mutate a distinct record marker on permitted document callbacks; the shell marker never changes. Test same durable instance ID closed/reopened and stale node/binding dispatch rejects the old lifetime. Add destructor/cancellation probes that capture a fresh LocalState, attempt publication through an old writer, and try a reentrant bound dispatch. All run under the CLOSED child route: publications/dispatch fail and no wake reaches the child, outer App or replacement. Exercise explicit close, content replacement, factory error after the component owns its root, restore failure, unwind of a constructed component and ordinary Drop. Consume Task 2's accepted private production cleanup helper and its generic root/registry-position Drop probes; this is ownership-position coverage, not a claim that public `ActionRegistry::register` captures application values. Task 3 separately adds real admitted widget-operation and subscription cancellation captures. A test-only catch_unwind observes the destructor probes; production does not catch arbitrary app panics and claim recovery. Do not add a public custom-decoder seam solely for this test.

- [ ] Build a table-driven test ledger of actual callbacks, not an assumption that one event covers all methods: construction; sync/update; build/apply/measure/draw; focused event/IME; focus/accessibility/synthetic accessibility; resource discovery/resolve/control drain; operation start/poll/cancel; search activation; native-view lifecycle hook invocation with existing harmless test host if available; host-intent drain; View capture/restore; suspend/close/drop. Native OS integration itself is deferred; every toolkit hook which invokes app code is still bound here. Update probe implementations as needed rather than adding public introspection only for tests.

- [ ] Add a doc-hidden integration-test-usable Driver constructor taking an explicit `TextRenderer` and `UserId`, with one shared field initializer. The focused fixture supplies `FontCatalog::controlled_fixture`/`TextRenderer::with_font_catalog` and a fixed author. Existing production constructors still perform `TextRenderer::new()` and `local_user_id()` at their current points and in their current order, then enter that initializer; use lazy/private inputs if needed to avoid moving discovery or root callbacks. Do not use production `Driver::new`, production `App::new`, process-global environment changes or HOME rewriting in this selection.

- [ ] Add private `#[cfg(test)]` controlled-input adapters for the existing successful `App::from_factory` and `App::from_scoped_parts` paths. They must enter/transfer the real owned WakeScope and execute the supplied factory/root callback exactly where the public path does, then delegate to the one `with_parts` initializer using Task 2's controlled lazy inputs. The detached `new_with_controlled_inputs` constructor is not a substitute for these lifetime-transfer regressions. Keep public production signatures, producers, discovery order and callback order unchanged. Adopt exactly the 44 existing regression entrypoints enumerated in `.superpowers/sdd/2026-09-13-common-real-app-framework/task-3-existing-regression-fixture-audit.md`: 21 run unchanged and 23 use only its 12 audited constructor-call substitutions; do not use a broad module selector. Keep the two public `App::try_from_factory`/`App::from_factory` FactoryError/Unwind entrypoints unchanged. Before authorizing the 23 substituted tests at runtime, trace the final implemented constructor closure to confirm that ambient font/identity/service discovery is bypassed, owned-scope and callback order are preserved, and the detached `.run()` panic guard still fires before `EventLoop::build`.

- [ ] Run `cargo nextest run --locked --profile ci -p ui --test it -E 'test(bound_component_dispatch::)'` via the isolated helper with the native CARGO_TARGET_DIR above; failures must demonstrate missing/outer destination, not a window/GPU prerequisite.

- [ ] Add `TileHost::from_component(component)` and keep existing `TileContent::App`/`TileHost::new`/`bind_record` compatibility for unmigrated callers. The component path owns AppComponent through a single internal bound AppWidget adapter. Checked component replacement marks Closing and closes the old lifetime immediately before root destruction. Because a TileHost cannot mutate its external App/Driver map synchronously, stale entries retain that closed binding and are refused before pointer dereference until the exclusive host clears both node maps on rebuild/destruction. Do not let `content_mut()` extract an unbound canonical root; if retained for compatibility, scope it to legacy content and expose a checked component replacement operation. Legacy bind failures must never use the parent document as fallback after a record binding was requested.

- [ ] Implement callback entry in two complementary places. The bound root adapter guards every existing Widget/AppWidget callback and rebinds document parameters to its record. After each `build_layout`, the exclusive App/Driver owner calls unsafe `BoundNodeMap::rebuild` with that same live root, Taffy tree, returned layout root and raw node map. The map associates each admitted node with explicit unbound outer metadata or the nearest component binding; nested components override the ancestor. Keep `Widget::build_layout(... HashMap<NodeId,*mut dyn Widget>)` intact to avoid rewriting every widget. Do not retain a second node-build delta, use a TLS build side channel, or assume `children()` exposes layout-only descendants. Do not keep widget references across callbacks, replacement or layout rebuild. Resolve synthetic accessibility targets through their originating bound root even where no Taffy node exists.

- [ ] Route ALL direct dereferences through the scoped helper: App/Driver focus/order/measure/event/IME/semantic dispatch, undo status/request sweeps, resource and operation polling; accesskit_bridge, a11y_tree and View focus capture/restore retained-tree walks enter child bindings too. Preserve old public helper signatures with explicit unbound compatibility wrappers and add bound internal variants; do not silently leave App/Driver calling the old wrappers. For callbacks with document arguments borrow the bound record once and pass ROOT; callbacks without document arguments still enter scope. Validate metadata presence, NodeId/raw-pointer identity and lifetime before dereference; topology stability remains the unsafe caller obligation. Avoid nested RefCell mutable borrow: root adapter and direct-node dispatcher share one entry function, and recursion under the same binding reuses the already borrowed document passed by the caller, never reopens it. Synchronous cross-record reentry returns `CrossRecordReentry`; it does not execute, fall back or manufacture a queued `FnOnce` payload.

- [ ] Expose the real content through `children/children_mut` so search/focus/semantics still discover it, while central resource traversal recognizes a bound boundary and cannot collect the same child's resources twice using the parent document. Forward existing theme/scale, metadata, host intents and local View hooks; no second command schema. Close sequence is mark component Closing/reject ordinary input → capture the latest local View while the tree is intact and cache its envelope in the component AND a host-owned close report → close the mount WakeScope immediately → enter that CLOSED child scope → save the captured envelope/request and flush recoverable record state → cancel admitted widget operations/subscriptions → detach node metadata/native children → take/drop the root under that same CLOSED entry. Do not defer scope.close until cancellation or drop, and never use enter_detached for teardown: detached publication is allowed, closed publication is not. Ordinary callback helpers reject Closing/Closed components and closed WakeHandles; only the owning lifecycle path may enter its closed route for cancellation/destruction. Record-service persistence may outlive the closed UI scope under its existing owner.

  `close` returns ComponentCloseReport rather than returning only an error which would lose the captured View. It attempts both View save and record flush, records both errors, caches the report and returns a clone to the host even on failure. Host retains this report independently of the tile until recovery is durably recorded or explicitly resolved. On record flush failure, record_retry retains a strong RecordStore handle; the weak RecordOwners registry alone is insufficient to preserve dirty in-memory state after the last tile drops. A retry flushes that same owner and clears the retained handle only after durability succeeds. The local envelope still contains no document bytes. Repeated close returns the retained report without recapturing a destroyed root; a retry uses report.recovery and the record owner, never reopens the old mount. If capture itself fails, preserve the previous saved envelope/cached View and record the failure. A local save failure must not overwrite the prior good View file. Tests inject independent and simultaneous View-save/record-flush failures, drop the tile, and assert host-held recovery/error details and the dirty canonical record remain usable; repairing the test destination and flushing report.record_retry must persist the unsaved edit after the last tile is gone. Dropping one tile does not drop a record still used by another.

- [ ] Implement explicit AppComponent::Drop in the existing App::Drop pattern (`ui/src/app.rs:5229`): close the owned scope, enter the closed child route, and use Task 2's accepted private production cleanup helper to take root, registry-position ownership and invocation-owned callback captures out of their Option/container fields and drop all app-owned values before leaving that guard. Declare the closed entry guard before the taken ownership locals so cancellation unwinding drops those values before the guard restores the outer scope; ordinary field drop only handles empty options and inert SDK metadata. Retain Task 2's generic root/registry-position Drop coverage and add Task 3 probes for actual operation/subscription cancellation captures; public `ActionRegistry::register` is not claimed to hold an application capture and gains no custom-decoder test API. Cancellation follows the same closed owner path. Use a scoped teardown guard so an unwind during cancellation still drops any remaining owned root under the closed child entry; do not rely on field declaration order or unwind into an outer scope. A component factory must place its root and any callback-owning app resources in this guarded component owner immediately after successful construction, before subsequent fallible setup. Factory-error and restore-error cleanup drops that owner; an unready/new reservation is cancelled without publication. HostContext::Drop closes its scope for a factory which never installs a root. The framework does not claim panic isolation for arbitrary unreturned constructor locals; any app-owned partially built object that invokes code on Drop must adopt the same closed-scope owner before such a fallible step, a concrete requirement for the later real-app factory migrations.

Task3 search boundary: bind `Widget::search_providers()` creation and `Widget::activate_search_result()` delivery. The returned owned `SearchProvider` is a separate callback surface consumed later by the palette and directory/resource paths; its subsequent method calls and Drop are explicitly deferred to Task4, and Task3 reports must not claim them as covered. Task4 must complete their component lifetime and disposal ownership before portable app qualification.

Task3 lifecycle transfer contract: add production `Widget::close_components() -> Vec<ComponentCloseReport>` and `Widget::take_component_close_reports() -> Vec<ComponentCloseReport>` hooks, with default retained recursion and owning bound-adapter/TileHost overrides. Report drain transfers inert ownership metadata and must not invoke ordinary app callbacks after close. App/Driver drain and retain reports before removing a subtree; TileHost replacement holds them until that host drain. Repeated close/drain does not duplicate pending host recovery or recapture destroyed widgets. Nested close captures the parent View while child data is still live, closes each child once and transfers every child report before destruction. Preserve original `view_retry`/`record_retry` strong owners. Explicit close/replacement uses this observable path; ordinary Drop only guarantees closed cleanup. Task5 consumes these hooks for runner/exit conformance; native host integration retains its existing close/retry UX contract.

- [ ] Audit with `rg -n 'node_map|unsafe|children\(|children_mut\(' ui/src/{application,app,headless,undo,view,accesskit_bridge,a11y_tree}.rs` and check every app-code call against the ledger. Rerun the probe selection and existing wake integration/focus/accessibility tests; execute mandatory Task 3 check/clippy/format gates above on native, then create the source commit for controller review and exact-SHA Fedora verification: `feat(ui): bind component callbacks through descendant dispatch`.

## Task 4: Execute actions and undo through the selected record and instance

**Files (final producer reconciliation):** `ui/src/{application,bound_dispatch,app,headless,undo,action_host,action_palette,a11y_tree}.rs`; `ui/src/{state,permissions}/mod.rs`; `ui/src/record.rs`; `ui/src/widget/{mod,app_widget,text_input,search_field}.rs`; `ui/src/search/provider.rs`; `ui/src/collaboration.rs` solely for the existing successful primary-route witness; `ui/tests/support/application.rs`; new `ui/tests/instance_undo.rs`; extend `bound_component_dispatch.rs` and test registrations. No TileHost or search registry source change is needed.

**Consumes:** Task 3 CallbackBinding/node map, final active-borrow entry and bound `SearchProvider` creation/search-result activation; application-owned root/registry/context; per-component current permissions/provenance; a checked RecordStore loan over document/history/pending owner; the accepted safe synthetic `KeyEvent` prerequisite before any keyboard regression runs.

**Produces:** one private operation execution body shared by checked bound and legacy App/Driver adapters, scoped resource addresses and search-provider handles with captured lifetime provenance, per-instance history selection, correctly partitioned commit notifications, and a UI-local typed action error. Keep the extracted function's existing argument semantics rather than simplify away actor, confirmation or broker evidence:

```rust
#[derive(Debug)]
pub(crate) enum BoundActionError {
    Dispatch(BoundDispatchError),
    Door(qos_actions::DoorError),
    AmbiguousTarget { address: qos_actions::ActionAddress },
}
```

```rust
// Private retained-tree front door. The explicit document is the same-record
// reentry witness; target retains the exact lifetime and registry-owner binding.
pub(crate) fn invoke_resource_action(
    root: &mut dyn crate::widget::Widget,
    document: &mut automerge::AutoCommit,
    target: &BoundActionTarget,
    transport: &mut dyn qos_actions::ActionTransport,
    argument: Option<&str>,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    actor: &str,
    confirmed: bool,
    broker_admitted: bool,
    invocation: qos_actions::InvocationId,
    ambient: Option<&qos_actions::ActionContext>,
) -> Result<crate::widget::ResourceOperationStart, BoundActionError>;

```

`invoke_resource_action` is the checked bound ownership/authority/history wrapper, not a bound-only copy of the operation engine. Extract one private execution body accepting the minimum existing `Widget` operation surface and explicit admitted document, registry, transport and action context. The bound wrapper calls it only inside the matching live component loan and checked record action-state loan. Legacy App and Driver callers have `Box<dyn Widget>` roots and outer `State` document/history, with no canonical `HostContext` or `CallbackBinding`; their adapter supplies those existing values to the same body and preserves current unbound history behavior. It must invoke mutable admission through the retained root's existing `start_resource_operation_with_document` route: a legacy `TileHost::bind_record`/current `record_intent` resolves through its checked record owner, executes against that record at `ROOT`, and returns missing, busy or identity failures without mutating or falling back to the parent document. `AppWidget` is viewed through its `Widget` supertrait only while the component loan is active. Do not construct a fake host context, reserve a canonical record for an unbound caller, weaken provenance to an optional unchecked value, downcast arbitrary widgets, or duplicate resolve/argument/confirmation/permission/execution/result validation. Leave the exact private helper name and full internal signature to the final Task 3 API reconciliation.

App's existing broker routing/admission surrounds the shared body and supplies its established transport; Driver supplies its explicitly configured transport. Host-specific broker admission, semantic target delivery, notification publication and scheduling remain with their existing owners. Preserve their supplied evidence/results, the mutable-document operation route and the declared replayable Document follow-up; do not move App broker effects into a generic transport or silently omit equivalent Driver behavior. The body receives either structured arguments or legacy argument text, rejects both being supplied inconsistently, re-resolves live resources, and retains existing confirmation/result validation. No newly invented broker or service implementation belongs here. `BoundActionError::Door` preserves the domain error unchanged. `BoundActionError::Dispatch(BoundDispatchError::CrossRecordReentry)` is the typed synchronous cross-record refusal and maps at every external App/Driver/broker boundary to the stable code `cross_record_reentry`; it must not become `DoorError::Failed`, outer-record execution or an arbitrary queued closure. `AmbiguousTarget` is returned only after current authority filtering finds more than one authorized live component destination and maps to `ambiguous_action_target`. Zero authorized targets retains the existing nondisclosing `DoorError::Unknown` behavior, regardless of hidden destinations. Keep domain `DoorError` and `ActionAddress` unchanged. Any future retry uses a separately specified owned typed action/intent after the active borrow ends.

Before Task 4 implementation begins, after Task 3 fix 1 and controller review, recheck the final lifecycle/report-drain and inherited mutable-operation traversal producers, then reconcile the private adapter and active-entry APIs against this ownership contract. Do not invent parallel method names in advance: `AppComponent` remains the sole owner of its root/registry/context and offers one crate-private closure loan of the matching live `CallbackBinding`, mutable `AppWidget` root and registry; the bound adapter/TileHost path gives App and Driver one private exact-instance resolver over that loan. It refuses Closing/Closed and compares the captured lifetime with the current component before lending. Separately, the binding's one checked record entry holds the exclusive canonical record loan and explicit document while temporarily moving its owned `UndoLog` and pending/generation state into a private scoped `Rc<RefCell<Option<ActionFrame>>>`. The frame is restored on normal return and unwind. Nested callbacks supply the explicit same document and reuse that owned frame only after exact record/lifetime checks; framework borrows end before arbitrary Widget or transport delivery. No history references are reconstructed from TLS, and cross-record entry returns `CrossRecordReentry`. Recheck the final `Widget`/`BoundAppWidget`/TileHost lifecycle drains and mutable operation reachability together with the `AppComponent`, App and FrameHarness close/report consumers before dispatch. No independent root/registry getters, downcast search, nested `RecordStore::with_history`, or unbound extraction is allowed.

```rust
// Runtime routing envelope; domain ActionAddress remains unmodified.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BoundActionAddress {
    pub instance_id: crate::application::InstanceId,
    pub address: qos_actions::ActionAddress,
}
// A discovered/pending target always retains this address together with the
// captured CallbackBinding. The numeric instance ID is not a lifetime token.
// Add to existing UndoEntry; never write instance into UserId/Automerge actor.
pub instance_id: Option<crate::application::InstanceId>;
// New UndoLog APIs; existing methods delegate with instance_id=None and keep
// legacy author-scoped/LIFO behavior. Bound callers require exact Some match.
pub fn next_undo_in(&self, author: &UserId, instance: InstanceId) -> Option<&UndoEntry>;
pub fn next_redo_in(&self, author: &UserId, instance: InstanceId) -> Option<&UndoEntry>;
pub fn undo_in(&mut self, doc: &mut AutoCommit, author: &UserId, instance: InstanceId,
    focus: Option<usize>, metadata: Option<String>) -> Option<UndoEntry>;
pub fn redo_in(&mut self, doc: &mut AutoCommit, author: &UserId, instance: InstanceId,
    focus: Option<usize>, metadata: Option<String>) -> Option<UndoEntry>;
pub fn record_committed_action_in(&mut self, doc: &mut AutoCommit,
    before: Vec<ChangeHash>, focus: Option<usize>, metadata: Option<String>,
    author: UserId, instance: InstanceId) -> bool;
```

- [ ] Add regression tests with two probe components sharing one canonical record and a third using another. Extend the one explicit integration fixture in `ui/tests/support/application.rs`: its canonical root is a test TempDir and its author, permissions, provenance, registry and transport are controlled; do not duplicate application construction. That shared fixture covers `AppComponent` and Driver through `ui/tests/instance_undo.rs` and `ui/tests/bound_component_dispatch.rs`. Put new real-App cases in the existing `ui/src/app.rs` library unit-test module and use its private `App::new_with_controlled_inputs`; do not add an integration-public App constructor solely for tests. Export identical domain resource IDs from the shared instances; invoke by BoundActionAddress and assert correct View destination and one document change/history entry. Run pointer/control invocation, keyboard shortcut, palette/address invocation, and accessibility semantic action across those real-App unit cases and Driver integration cases using their existing controlled constructors. Deny/revoke after discovery and verify invocation is rejected without mutation and without revealing the hidden destination. An unqualified address with two authorized destinations returns `BoundActionError::AmbiguousTarget`; a stale resource, captured closed lifetime or same-u64 replacement returns the typed lifetime refusal rather than routing to the current tile.

- [ ] Write interleaved undo tests over real Automerge changes: A writes `a1`, B writes `b1`, A writes `a2`; undo A removes only `a2`, undo B removes only `b1`, redo each restores its own action. Both instances use the same authenticated author, which remains present in each entry. Add a remote-authored change between local actions and show undo preserves it. Add A-edit→undo-A→B-edit→redo-A and require B's edit not to clear A's redo stack. Mount B while A has an unfinished typing burst; no mount/rebase may consume A's pending boundary. Initialize a new record before first undo, then verify first undo removes only the first user edit, never schema or imported setup.

```rust
assert_eq!(history.next_undo_in(&alice, 11).unwrap().instance_id, Some(11));
assert_eq!(history.next_undo_in(&alice, 12).unwrap().author, Some(alice.clone()));
// After A's undo, B's independently edited field and remote field are intact.
assert_eq!(read_string(doc, "b"), "b1");
assert_eq!(read_string(doc, "remote"), "kept");
```

- [ ] Adopt exactly the 31 existing regression entrypoints enumerated in `.superpowers/sdd/2026-09-13-common-real-app-framework/task-4-existing-regression-fixture-audit.md`: 12 pure tests run unchanged, 16 App/Driver tests use only the ten audited constructor-call substitutions, and three cache tests use only their three direct App substitutions. Preserve all assertions, including the existing unbound App generation-exhaustion behavior; the stronger bound-record exhaustion rule gets a separate new Task 4 fixture rather than changing that legacy test. Production `App::new` and `Driver::new` remain disallowed in this isolated selection. Before runtime, after Task 3 fix 1 and controller review, recheck the final lifecycle/report-drain and inherited mutable-operation producers, the doc-hidden public `Driver::new_with_controlled_inputs` signature and initializer closure, and the private `App::new_with_controlled_inputs` entry. Confirm both controlled entries preserve the real initializer, then trace the final implemented closure for fixture-only fonts, fixed author, disabled App collaboration, local transport, retained layout/settling order and no new Drop-time service work. Do not select whole App/headless modules.

- [ ] Consume the separate accepted safe synthetic `KeyEvent` prerequisite before running the eight existing keyboard regressions in that selection: the three palette tests keep their real `action_palette_handle_key` path, and five Driver tests keep production `Driver::type_text`/`Driver::key`. That prerequisite, outside Task 4 implementation scope, supplies one field-complete vendored constructor on every winit-supported backend with cfg-specific platform payload initialization, converts production `headless::key_event` and the palette `synth_enter` helper, uses `DeviceId::dummy()`, retains `is_synthetic: false`, and registers its downstream macOS/Linux constructor/supplement test in the existing workspace-locked `ui/tests/it.rs` target. Require its native macOS and Fedora/Linux contract gates to have passed before the keyboard selection. If the accepted constructor/test closure is unchanged, do not rerun that standalone contract solely because Task 4 starts; Task 4 runtime selection follows its own changed source closure. Then run isolated `cargo nextest run --locked --profile ci -p ui --test it -E 'test(instance_undo::) | test(bound_component_dispatch::)'` plus the 31 individually selected existing entrypoints and observe intended failures.

- [ ] Extract the actual resource resolve/argument/confirmation/permission/execution/result logic from App and Driver into bound_dispatch as the one private execution body described above. Preserve existing qos-actions `door`, registry/executor, `ActionTransport`, `InvocationId`, `ResourceOperationStart`, `InvocationUpdate`, cancellation and v2 result validation. Do not reimplement action schemas or downgrade `start_resource_operation_with_document` to the old immutable route. The application-owned live closure loan and private exact-instance resolver supply the bound adapter one matching binding/root/registry tuple without exposing fields or extending references beyond the closure. Enter the binding's checked document/history/pending-owner loan before discovery; retain that one loan through the second current-authority check, encoding/execution, immediate history recording and pending-owner update. Use that component's registry and authenticated author. The legacy adapter supplies its outer root/State/registry/transport to the same body and preserves unbound history behavior, while calling mutable admission through the retained root so `TileHost::bind_record` and current `record_intent` keep their checked record/document/`ROOT` route. A failed checked follow returns its error and must not announce/advance scope, execute against outer State, or fall back to the parent document. Preserve broker admission/confirmation evidence behavior: this slice accepts the existing injected transport/status but starts no new collaboration connection or ambient identity lookup.

- [ ] Cache resource discovery by component lifetime, a record-owned mutation/revision generation, current permission generation and existing widget-local resource invalidation, not just domain resource ID or shell State revision. Never call `get_heads` during read-only discovery: it closes an open Automerge transaction and can split a typing burst. Advance/evict the record generation at every admitted mutation source: bound widget update/event/semantic/search mutation and `needs_commit`; drained ActionHost or control Document action; local or broker-admitted Document dispatch; a replayable Document follow-up from `start_resource_operation_with_document`; undo, redo or revision restore; remote/incremental merge or loaded/imported change; and record initialization. Existing explicit local resource invalidation and permission-generation changes remain independent cache-key inputs. If the generation exhausts, disable caching permanently for that record owner and serve fresh discovery results until the owner is destroyed; never wrap, reset, or rematerialize under a reusable generation key.

- [ ] Bound discovery retains an internal snapshot pairing each resource subtree with its captured `CallbackBinding`, registry-owner identity and `BoundActionAddress`; it neither clones/moves the `ActionRegistry` nor retains root/registry references. Public qos-actions `Resource` and `ActionAddress` remain unchanged. Filter through each binding's current authority before counting candidates. Zero authorized destinations returns nondisclosing `Door(Unknown)`, one selects exactly that captured target, and more than one returns `AmbiguousTarget` unless the caller supplies the explicit instance. Immediately before execution, re-resolve lifetime, record, permissions, resource and action and borrow the matching root/registry only through the application-owned live component loan; those references cannot escape its closure. Direct control drains capture their originating binding instead of losing it through recursive `take_control_invocation`. Palette/search rows use instance-qualified internal keys and retain `BoundActionAddress` plus the captured binding across argument and confirmation flow; reopening the same numeric instance invalidates that pending target. Do not derive origin from domain IDs.

- [ ] Complete the Task 3 search boundary for every owned `SearchProvider` returned by a bound widget. Retain its originating `CallbackBinding` and registry-owner identity across `name`, `action_directory_generation`, `action_directory_cursor_is_current`, `search`, `search_limited`, `search_page` and `resolve_action_resource`; invoke live methods through the matching component route and current authority. `name() -> &str` may use inert copied metadata captured during bound construction, but no app/root/registry borrow escapes its guard. Closed-provider calls refuse without app delivery using each existing signature's empty/`None`/`false` result, and a stale provider neither queries nor wakes a replacement lifetime. Preserve result limits, cursor semantics and resource provenance. Dispose the actual provider payload under its owning component route: early provider removal while the app is live drops promptly, while teardown takes and drops all still-retained provider payloads under the CLOSED owner even if palette/directory proxy handles outlive the tile, including unwind. Do not weaken `Send`/`Sync`, extract a raw root, retain root/registry references, or add a generic capture registry for hypothetical callbacks. Add real method/wake/destructor probes for early removal, outliving close and unwind; no transport-service unsubscribe is implied. Reconcile `ui/src/search/{provider,registry}.rs` only if this concrete mechanism requires consumer changes.

- [ ] Extend UndoEntry/filtering as above, including every commit, inverse, undo-to-redo, redo-to-undo, restore/revision and revision-limit constructor/path. Preserve the exact `InstanceId` on moved entries and on status/summary selection. A new bound local edit clears redo entries for only `Some(instance)`; legacy `instance_id=None` APIs preserve their current author-scoped/LIFO and whole-stack redo behavior. Add record-owned pending action state containing `(instance_id, before_heads, authenticated_author, original_metadata, original_action_context)` beside the document history, initialized on every record construction/open path. The one checked action-state loan supplies document, history and pending owner atomically and maps borrow contention/missing records to `RecordOpenError`; no nested `with_history` call is permitted.

- [ ] Capture before heads before any mutation; drain explicit ActionHost boundaries once and record their before/after immediately. For widget typing bursts retain the logical original owner until its commit. At foreign-instance opaque mutable admission, the existing AutoCommit transaction may be physically committed under its saved context; this is necessary because Widget callbacks have no pre-write notification. A foreign no-op retains the original logical burst and redo state without publishing history. An actual foreign mutation finalizes the prior burst exactly once at pre-foreign after-heads, then starts the foreign segment. Nested owner transitions apply on both entry and return. Persistence may likewise stamp/commit a physical transaction without ending the logical burst. These extra physical commits require later benchmark qualification. Same-instance continuation does not call `get_heads` again. Read-only mount, projection, discovery and View restore neither finalize that burst nor call get_heads/rebase merely to establish a second baseline. Test A pending→mount/read B→continue A as one unchanged burst, then B's first actual mutation closes A once and starts a separate B boundary. Never reset a shared record baseline as a side effect of mounting another tile. No interleaved callbacks may use a shared `last_heads` to attribute another instance's edits.

- [ ] Partition `record_dispatched_action`, `needs_commit/on_commit`, UndoAccess publication/requests and shortcuts by CallbackBinding and run them through the same checked document/history/pending-owner loan. A bound child's notification is consumed at its record boundary with its binding's authenticated author/context and excluded from outer shell history, even if discovered through both retained traversal and node_map. Deduplicate raw pointer visits per sweep. Unbound shell changes continue using shell State. Pure View navigation and Effect progress/outcomes create no document entry. A checked owned operation scope consumes exact successful typed before/after spans from the existing ActionHost ordinary/contextual/optimistic producer and the shared Document executor; declared follow-ups from either View or Effect record exactly once under their captured instance. No-op/failed/submitted-without-change producers preserve the prior logical typing burst. Pure View does not call get_heads. Potentially mutating Effect admission may close the physical transaction while retaining logical pending ownership; a raw prefix or tail around declared spans is excluded from both declared history and later typing. Such a callback returns the UI-local typed `undeclared_document_mutation` post-delivery fault while retaining canonical bytes, generation and original ownership. Its Accepted handle remains in component custody for existing CLOSED cancellation/cleanup. Ordinary unbound rising-edge ActionHost semantics remain unchanged. A pre-delivery denied/refused action creates no entry; post-delivery contract faults do not erase valid declared entries or unrelated history/redo.

- [ ] Store the exact captured `CallbackBinding` with every accepted local/directed operation and poll it under that admitted component lifetime and same record binding; cancellation during close uses only the owning CLOSED-scope teardown path defined in Task 3; route terminal updates by `(lifetime, InvocationId)`. Remove an entry before terminal delivery as today. Closing rejects new UI delivery; do not retarget a result to a reopened instance with the same `InstanceId`. Durable service completion is an explicit later service contract and cannot be inferred from a UI effect handle. Preserve the local-private View guard against remote actors.

- [ ] Rerun the 31 exact existing entrypoints and the new Task 4 instance/dispatch/history/cache/provider-lifetime tests only; do not use broad App/headless module selectors. The eight keyboard regressions exercise the consumed safe-event path, but the unchanged prerequisite-only constructor/helper contract tests are not mandatory Task 4 reruns unless Task 4 changes their source closure. Verify shell outer document heads/history unchanged in all bound child cases, bound-record generation exhaustion remains permanently uncached until owner destruction, and stable external error codes preserve `cross_record_reentry`, `ambiguous_action_target` and existing nondisclosing unknown-action behavior. Execute mandatory Task 4 check/clippy/format gates above on native, then create the source commit for controller review and exact-SHA Fedora verification: `fix(ui): dispatch component actions and undo to their bound record`.

### Final private producer and compatibility decisions

`AppComponent::with_live_component` is the sole checked root/registry closure loan. The doc-hidden `Widget::visit_components` accepts an opaque crate-private request with a narrowly documented private-interface lint allowance. Public Widget mutation signatures are unchanged. The public Driver qualified-address entry maps the private UI action error to the unchanged domain DoorError envelope; routing codes remain in its Unavailable payload and external App failures preserve them.

Bound resource caches key exact owner lifetime/record generation, existing local invalidation, full PermissionContext equality including private expiry-evaluation time, and widget revisions. Exhausted record generations remain uncached. Scoped palette rows, argument/confirmation state, lazy and opaque provider results, and semantic resources retain their captured owner; semantic identity uses lifetime UUIDs without altering app domain IDs. Component-owned provider payloads are exposed as weak checked proxies, collected once at retained boundaries even through ordinary containers. Payload disposal occurs under its original live or CLOSED route, including unwind. Host control availability is independently projected for each component.

The transport publication namespace `qos-ui-bound:v1:` contains host/lifetime UUIDs and bounded reversible hexadecimal UTF-8 resource metadata (4096-byte maximum); no pointer address is serialized. Eager/paged exports, outbound qualified requests and directed reverse routing share it. Per-owner registries validate the original contracts, reserved/malformed/stale aliases never raw-fallback, and page limits apply after projection. The owner table does not grow per queried resource.

Aliases solve instance/resource ambiguity only within an admitted route. The existing collaboration Hello/PrimaryReady channel now acknowledges the successful RouteSpec. A private manager witness is invalidated before every primary Hello enqueue, consistency-checked on readiness and gated by current bridge health; a fresh epoch invalidates directory/publication state even on same-route reannouncement. Bound broker use requires exact canonical namespace app, parseable workspace UUID and checkout record equality. Missing/mismatched routes fail closed before publication, request ID allocation or directed mutation. Matching bound exports use the acknowledged app ID; legacy-only behavior remains. A private synchronous App capture seam tests request contents and selected contract separately from directed delivery; it is not a broker acceptance or service-roundtrip claim.

Mixed-app/workspace/record action routes, per-route bounded directories and directed-event provenance are a concrete downstream shell/canonical collaboration integration dependency. Task4 does not implement new connections, services, broker/wire changes or certify mixed hosted-app collaboration. Native/Fedora exact selected evidence and independent review remain acceptance gates.

Task4 fix1 review reconciliation (F1–F4 only): Driver outer unqualified controls now discover current-authority bound candidates and either select their captured owner, refuse ambiguity, or use only unbound declarations for legacy fallback. A foreign no-op `needs_commit` is acknowledged without changing the existing logical pending owner, metadata, context, redo or published history; actual foreign writes still split via the existing segment settlement. Bound provider hits resolve/filter under their owner before becoming visible; opaque hits require current app Read, and retained provider rows are pruned when authority changes. Preserve paging budgets and activation revalidation.

The controlled App palette carries explicit renderer and SearchField factories through the real initializer and every field reset. TextInput/SearchField share their production field initializers with a crate-private test-only explicit-language/no-warmup path; production defaults remain unchanged. The reviewed palette fixture closure supplies checked-in fonts with en-US and local input en_US without environment-language lookup or dictionary warmup. Retain the original 13 constructor substitutions and assertions. The exact closure/ruling lives in task-4-existing-regression-fixture-audit.md and task-4-fix1-palette-input-ruling.md. Task5 must reconcile its future owned renderer input with this palette factory seam; no Task5 implementation is included. The public Driver undo_status consistency finding and optional module organization remain deferred controller ledger items. Fix1 native selection is the unchanged original83 plus the three named F1–F3 regressions, with exact identities frozen in task-4-fix1-final-selection.json; independent review and exact-SHA Fedora remain required.

Task4 fix2 N1 reconciliation: retained bound palette rows carry their producing-provider index and exact permission validation snapshot. One finite queue validates at most32 retained rows per step, separately from ordinary provider paging; unvalidated, revoked or closed rows are excluded from visible results and the active row index. Permission changes invalidate and enqueue a previously validated row once, including after it was visited in the current pass. SearchAdvance.inspected accounts for retained row checks. An external completed-query advance may begin a new finite pass, whose final step returns pending=false without starting another pass. No unknown generation is treated as stable and no generation callbacks are needed. Per-kind ranking remains unchanged with no total result/group cap. Argument/confirmation input pauses advance with zero work and pending=false, preserving its empty result surface and any partial queue until the existing exit/open rebuild. The same controlled test exercises the existing strict Confirm contract and subsequent reset. See task-4-fix2-prompt-pause-fixture-audit.md for this incremental exercised path. The exact130-row controlled regression is app::bound_component_tests::task4_completed_bound_search_validation_is_bounded; final fix2 selection is the unchanged86 plus this one case. See task-4-fix2-bounded-validation-ruling.md and task-4-fix2-proposal-and-fixture-audit.md for the narrow design/fixture closure. Independent N1 review and exact-source Fedora remain required.


## Task 5: Use one prepared component in standalone, embedded and headless runners

**Files:** `ui/src/{launch,app,headless,application}.rs`; existing `launch.rs` unit tests; `ui/tests/application_contract.rs`, `bound_component_dispatch.rs`; `crates/app/qos-app/src/lib.rs`.

**Consumes:** Completed Tasks 1–4; no real-app construction or host-service side effects are implemented here.

**Produces:**

```rust
// Runner authority comes from an explicit adapter, never from app metadata.
pub struct PreparedComponentInputs {
    pub context: HostContext,
    pub target: LaunchTarget,
    pub restore: RestoreInput,
}
pub type ComponentHostPreparer = fn(&ApplicationDefinition, &LaunchArgs)
    -> Result<PreparedComponentInputs, LaunchError>;
impl LaunchSpec {
    pub fn from_definition(definition: &'static ApplicationDefinition) -> Self;
    pub fn component_host(self, prepare: ComponentHostPreparer) -> Self;
    // Runner setup supplies context/target/restore from validated CLI inputs.
    pub fn prepare_component(&self, args: &LaunchArgs, context: HostContext,
        target: LaunchTarget, restore: RestoreInput) -> Result<AppComponent, LaunchError>;
}
impl App {
    pub fn from_component(component: AppComponent) -> Result<Self, LaunchError>;
}
impl crate::headless::FrameHarness {
    pub fn from_component(component: AppComponent, viewport: (u32, u32))
        -> Result<Self, LaunchError>;
}
// headless::run_component consumes the prepared component. It reuses current
// drive_spec_actions/audit/output code and returns existing 0/1/3 outcomes.
pub fn run_component(spec: &crate::headless::Spec, component: AppComponent) -> i32;
```

- [ ] Write a three-runner conformance test using the same `probe_definition().create` pointer and same serialized starting record. Standalone uses `App::from_component` without `.run()`, embedded uses `TileHost::from_component` under a distinct outer App, headless uses `Driver::from_component`. Each runner invokes the factory exactly once, has the same registered test domain action, sees identical canonical initial state and restoration, and produces byte-equivalent domain result with one history entry. Test effect and View result parity, denied action parity, local operation cancellation and close/reopen stale result rejection. These probe tests establish framework behavior only, not real Notes/Files qualification.

- [ ] Add a launch unit test proving definition-based headless calls common preparation before choosing semantic-only/rendered behavior and never invokes `configure_app/open_state` as an independent second setup. Retain existing argv validation, custom commands/live handlers/shots behavior on legacy LaunchSpec. Explicitly reject incompatible definition plus legacy root/state override combinations with a LaunchError instead of calling two factories. Screenshot fixtures remain available but do not supply a production construction substitute.

- [ ] Run isolated `cargo nextest run --locked --profile ci -p ui --test it -E 'test(application_contract::) | test(bound_component_dispatch::) | test(instance_undo::)'` and launch unit selections; observe intended failures.

- [ ] Implement definition-aware dispatch inside LaunchSpec. Store the one ApplicationDefinition and an optional explicit ComponentHostPreparer callback. `from_definition` installs no ambient preparer; running a definition without one returns new typed `LaunchError::MissingComponentHost` before factory, window, profile discovery or service initialization. `component_host` supplies the runner's adapter. After ordinary argument validation, invoke it once to obtain PreparedComponentInputs, then call the existing checked prepare_component once before selecting native versus headless delivery. Validate returned context/target through the same ApplicationDefinition::instantiate wrapper; a preparer cannot bypass binding checks. Direct embedding can keep calling prepare_component with explicit inputs and needs no CLI callback. Resolve explicit standalone/test profile, workspace, author, scope and target in the adapter before app construction. Headless must provide isolated services/state before factory code runs, not mutate environment or discover state afterwards. Preserve public legacy startup behavior for unmigrated apps; definition-aware mode always follows the one prepared component path. Test adapters use the existing `allowed_live_options`/`LaunchArgs::value` seam with explicitly supplied temporary paths and fixture identity/permissions; do not invent a production `--author` flag or trust application-supplied IDs as authentication. Add tests that missing host preparation invokes no factory, denied/invalid preparation returns its typed failure, and native/headless call the same preparer exactly once. This framework slice defines no default profile/record CLI policy: the later SDK standalone-host bootstrap plan must supply a real preparer plus its exact accepted options before any production app main cuts over. Keep that dependency explicit in Notes/Files plans; neither an unregistered record flag nor an assumed default host makes a production binary runnable. Do not silently run a fresh empty profile for an explicit existing record whose profile is unavailable.

- [ ] Mount the prepared component in both App and FrameHarness through the same internal bound root adapter used by TileHost. Native App can own its own outer window scope, while each component keeps its separate mount scope bound to that host's wake destination. FrameHarness owns a nonnative receiver/dirty flag for component wakes and checks fresh lifetime before ticking; constructing it inside another App scope must never capture that outer destination. No event loop, window, GPU, compositor or collaboration initialization is required by semantic-only preparation. Runner record persistence delegates to the canonical owner; do not call `persist::open` or create a second Autosave for the component document. Outer shell State remains independently owned.

- [ ] Route explicit runner close and component replacement through `AppComponent::close()` before dropping the bound root. App, FrameHarness and the embedding host retain failed ComponentCloseReports independently of removed widgets, including the exact `view_retry` store and canonical `record_retry` owner. Headless driving includes close/save outcomes in its durability exit result; an in-memory component status remains explicitly MemoryOnly. Add three-runner tests with the preparer's other View-store handles dropped and a save failure during close: assert original scoped destination, retained envelope and retry without resurrecting the component. The Drop safety net invalidates callbacks during failure/unwind; it is not a substitute for this observable explicit close result.

- [ ] Refactor headless driving helpers to accept the prepared driver's persistence callback/status rather than assuming a separate global Autosave. Keep JSON action inputs, inspection/audit outputs, semantic-only versus rendered modes and success/action/durability exit codes (0/1/3). Write filesystem-output smoke tests using temporary explicit paths, with real new framework definition preparation rather than a shot-only fixture. Add failure-path tests for state-open error and flush error returning 3 with preserved original bytes.

- [ ] Run relevant existing launch/headless/App/widget/record/undo/resource/accessibility regressions using the isolated helper. Execute mandatory Task 5 check/clippy/format gates above on native before committing; after the source commit below, the controller reviews and runs those gates at that exact SHA on Fedora. Additional parent-plan workspace checks remain required in its qualified host/test windows. Exact final focused command:

```sh
CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run --locked --profile ci -p ui --test it -E 'test(application_contract::) | test(canonical_record_owner::) | test(bound_component_dispatch::) | test(instance_undo::)'
```

Record host/commit/selection and any prerequisite failure; no build/test execution was performed to author this candidate. After self-review and native gates, create the source commit for controller review and exact-SHA Fedora verification: `feat(ui): share component preparation across native embedded and headless hosts`.

## Seams handed to later plans

1. **Notes construction/migration:** exports its real ApplicationDefinition and AppWidget implementation; factory consumes explicit HostContext record, author, permission/provenance and wake handle, then returns AppComponent with Notes actions registered. `initialize_new_record` is the only initial-document write seam for a genuinely new destination; it returns `RecordOpenError::Initialization` without running the closure for a loaded/already initialized record. Notes import must extend that owner transaction with its atomic receipt/content strategy and explicit legacy-source handling; no framework implicit legacy fallback. Selected page/caret extension uses existing local View helpers first. Existing normal Markdown/saver ownership remains Notes plan work.
2. **Files providers and opener:** add a concrete typed granted-resource variant to non-exhaustive LaunchTarget once the actual runtime-fs provider contract defines file **and directory** handles, scope and revocation. Add those concrete handles/service fields to HostContext, not a generic service bag. Existing Record target already carries the exact Notes RecordCheckout/reveal address; Files can use the scoped record index/open intent without fabricating a `.notes` path. This framework plan does not claim FsClient/FileHandle is already sufficient.
3. **Collaboration/authority:** canonical RecordOwners/RecordStore own per-record worker and delta-pump attachment; component owns distinct per-instance presence/View subscription. Extend explicit context/service construction with authenticated broker connection and record route, retain ActionTransport and current PermissionContext at invocation, and reuse current App delta processing. No environment-driven `CollaborationManager::for_record` is introduced into AppComponent. Revocation updates the live permission handle; capability discovery never substitutes for execution checks.
4. **Shell catalogue/lifecycle:** link actual app-crate definitions in one package-ID catalogue shared by platforms, persist chosen app/record/instance binding, map u64 tile IDs directly, supply profile/device/workspace-scoped InstanceViewStore and explicit profile registry, bind wakes on mount, render typed retryable failures with data retained. Unsupported fleet entries stay capability-map metadata. The shell’s packaged Linux process route is separate and preserved.
5. **Standalone SDK host:** supplies ComponentHostPreparer and exact profile/workspace/record CLI policy, authenticated identity/current authority, explicit record/view stores and actual required services before any factory runs. Definition-only LaunchSpec fails with MissingComponentHost until this adapter is installed. Real Notes/Files production binary cutover depends on this host plan; library conformance tests already provide explicit test adapters. No permissive or ambient test fallback is inferred.
6. **Native host:** deliver input/IME/accessibility to existing App/Driver/common root and use component wake events. This plan needs no AppKit/Smithay types. Native lifecycle, Metal, VoiceOver, Fedora compositor/headless and actual Mac↔Fedora Notes/Files walkthrough remain subsequent qualification gates.

## Decisions and remaining integration boundaries

- Controller preflight accepted the shared/exclusive directory gate preserving legacy-with-legacy behavior while refusing canonical-with-legacy in this version. The exact gate file constant can live beside current LOCK_FILE; older binaries remain an explicit concurrent-profile limitation. This is not permission to migrate/import while an old writer is active.
- Controller preflight accepted that `LaunchTarget` initially exposes New and Record only, with Files' granted target added by its provider plan. No currently inspected portable directory-grant type can faithfully occupy that variant today; accepting `PathBuf` would make the desired authority promise false.
- Local restore schema v1 uses existing View only. The Notes plan must decide any additional page/drawer/caret contribution using its actual widgets; it must not overload shared document state or freeze bytes to bypass the local schema boundary.
- Pending text-burst ownership is the main implementation complexity: preserve current grouping within one instance, close it on an actual mutation-owner transition (never read-only mount), and use explicit before/after boundaries for domain actions. If behavior tests prove this cannot be done safely within current UndoLog, bring that concrete failing trace to the controller before choosing another history owner; do not create per-instance document copies.

## Resolution of controller preflight findings

1. **Lifecycle:** explicit root ownership and custom Drop close the child route before cancellation/detach/destruction, then enter that closed route; ordinary dispatch rejects it. Failure/unwind cleanup follows the same owned-root path, and host-held ComponentCloseReport retains local recovery, independent persistence errors and a strong canonical owner for retrying failed writes.
2. **Restore keys:** InstanceViewStore is constructed for explicit profile/device/workspace, uses collision-free namespace paths, and validates namespace plus app/record/instance binding; cross-workspace equal-ID tests are required.
3. **Pending history:** read-only mount/projection leaves another instance's typing burst untouched. Finalization occurs only before a different instance actually mutates, retaining original author/context/metadata/before heads.
4. **Recovery publication:** read-only CanonicalLoad holds source/repair bytes; accept_canonical publishes only after schema and immutable UI identity checks. Invalid recovered identities and newer schemas retain exact source bytes.
5. **Initialization:** New reserves a private construction-owned slot. RecordReservation::initialize alone initializes schema, commits setup, creates baseline/autosave and marks Ready; factory restore/mount only verify Ready and never finalize again.
6. **Baseline/gates:** baseline is reviewed f14221783 with UI Task 3 complete, direct core is the sole source author, and each task now requires explicit check/clippy/format gates on native and exact-SHA Fedora.

## Additional controller preflight resolutions

- Task 1 reservations now have a unique non-Clone RecordReservation token and Existing-only opener; shared handles cannot initialize or cancel another factory's slot. Task 2 stores that token explicitly and delegates initialization through a mutable HostContext.
- The writer's current `pending.take()` plus enqueue-time `sent` tracking makes error acknowledgement alone insufficient for recovery. Task 1 now specifies sticky failed-writer state and a complete atomic checkpoint retry; Task 3's retained owner is therefore useful after a failed close, including no-new-edit retries and partial append failures.
- Record home identifies its minting room, not its present replica scope (`ui/src/record.rs::init_scoped`). Validate nonempty home, preserve it and compare only room_scope to current workspace. Add a valid foreign-home/current-scope load regression alongside mismatch tests.
- A successfully initialized New record needs a retained strong owner and exact minted checkout if subsequent factory/View setup fails. Task 2 now specifies LaunchError::ComponentSetup and a last-owner failure/retry regression; returning only a message would lose the promised recovery route.

## Plan self-review

This bounded plan covers the reviewed design's common factory, explicit target/context, record writer/restore ownership, local View, per-instance operation/undo and full callback/action dispatch requirements. Notes storage/schema import, Files grants/providers, broker/presence transport, shell production cutover and native loop remain separately named consumers, not hidden completion claims. Existing types and domain action/resource IDs are retained. No task starts a native event loop or depends on a substitute Notes/Files implementation. Testing commands use the provided isolation helper and explicit temp paths, preserving HOME and toolchain. No production files, tracked design documents, git state, operator data, builds/tests, VMs or GUI surfaces were changed while producing this candidate. Controller preflight decisions above are routine design review points within the authorized goal, not new user approval requests.
