# Connected-folder framework ownership Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Relocate Imager's connected-folder and adjacent verified-copy native mechanisms to a focused headless framework service, migrating every consumer of the relocated implementation through semantic APIs while preserving existing protocols and refusal behavior.

**Architecture:** `tilo-folder-service` owns the complete private connection/migration/security closure. The SDK exposes only curated copy, review, scan, readiness and early-host-dispatch operations; native endpoint/store/transaction internals remain private. Existing `runtime-connected` and `tilo-connected-protocol` remain the separate no-I/O exchange/codec layer and are unchanged.

**Tech Stack:** Rust edition 2024, existing cap-std/cap-primitives/cap-fs-ext, serde/serde_json, sha2, dirs, qos-ntfs-protocol, target-specific libc/windows-sys, existing tests and Python boundary checker.

**Spec:** `docs/superpowers/plans/2026-09-18-cross-platform-framework/connected-folder-extraction-design.md`; binding ownership/capability contract: `docs/superpowers/plans/2026-09-18-cross-platform-framework/framework-contract.md`. The public extraction design contains 100 source fingerprints. Read the public spec and this plan; do not inspect another plan's SDD workspace.

## Global Constraints

- Rust edition 2024; use the pinned toolchain and plain cargo.
- Never set CARGO_TARGET_DIR or share targets between worktrees.
- Do not regenerate Cargo.lock; review only necessary dependency-edge changes.
- Preserve existing checkout changes; implementation gets an isolated base that includes the intended current work, not an accidental older clean revision.
- Keep authorization, signed/wire formats, native names, history and CRDT schemas.
- No public unsafe evidence constructors, path-reopen shortcuts or unsandboxed fallback when a required protection cannot be provided.
- Actions/resources remain the public automation/accessibility surface.
- Unsupported optional/inherently host-specific capability is legitimate; unsupported required functionality on a claimed host is unfinished work.
- No ignored tests, weaker assertions or successful unobserved native checks.
- No publishing, signing, physical disk writes or user-desktop screenshots as part of planning. Native implementation tests use explicitly provisioned fixtures.

Additional execution constraints: one product writer at a time; coordinate exclusive ownership of the exact files below before editing. Do not edit atomic-file, app-state, runtime-connected or tilo-connected-protocol. Parent may finish its vault unit before this plan executes; take fresh source hashes and a fresh base at execution. Do not invoke native coordinator/device operations without separately provisioned and authorized fixtures. This draft has not executed Cargo or product modifications.

The existing source has fixture-gated ignored tests. The no-ignore covenant prohibits introducing ignores or hiding failures; retain those pre-existing declarations and explicitly run them in their native lanes. Ordinary `cargo test` alone does not qualify those tests. The native suite must fail missing prerequisites, not turn absence into a pass.

## Deliverable boundaries

This plan completes **physical ownership and the moved callers' API migration**, not all framework host certification. Keep open G02 scoped grants/revocation and owner lifetime; S01/S02 equivalent storage/lease consolidation; I01 authenticated transport integration; C01 generalized child ownership; N02 independent cache; N04 readiness service consolidation; required native/distribution qualification that cannot be run. Do not report full N03 acceptance or a required host feature implemented while those gates remain unmet. Preserve existing Windows copy refusal/Linux-only transaction execution; do not add a permanent “unsupported” implementation and call the host complete.

No new connected-folder UI/action or transaction API is required: outside `setup/connection`, the only current production caller is main's early receiver dispatch. Keep that complete connection subsystem private. Existing copy/import resources/actions continue to invoke their same production handlers.

## File structure and move manifest

Create `crates/system/folder-service/{Cargo.toml,src/lib.rs,src/copy.rs,src/review.rs,src/scan.rs,src/session.rs,src/startup.rs,src/test_support.rs,examples/early-host.rs}`. `copy.rs` is a curated export list, not another engine. `review.rs` wraps actual pinned validation; `scan.rs` contains the existing no-follow scanner; `session.rs` owns the legacy authenticated-readiness adapter; `startup.rs` owns both early administrative dispatch modes. Test support contains only narrowly gated test hooks and a wrapper around the existing reviewed-copy engine, never a capability constructor.

The exact full-file moves are enumerated in Appendix A. For every row move **all contents, including embedded tests**, to the destination, with only the explicit path/visibility normalization below. Do not break up the large native algorithm files in this extraction.

Additional edits:

- Root `Cargo.toml`/`Cargo.lock`: service member/dependency edges only.
- `crates/app/tilo-app/{Cargo.toml,src/lib.rs}`: semantic `folder-copy` feature and curated `folders` export.
- `apps/imager/Cargo.toml`: enable `folder-copy` in both existing ui dependency declarations; service test-support dev dependency. Preserve remaining native dependencies used by unrelated Imager mechanisms.
- `apps/imager/src/main.rs`: one unconditional early framework dispatch replaces the two Linux-specific connection/native-copy calls.
- `apps/imager/src/setup/mod.rs`: remove moved modules, retain policy/view/picker declarations. Do not `pub use` entire framework internals to emulate the old setup tree.
- `apps/imager/src/setup/{desktop_continuation,import_view,folders}.rs`: semantic imports/calls; retain presentation, continuation schema, choices, mode overrides, approval actions, workflow state and picker implementation.
- `apps/imager/src/setup/{controller,view}.rs`: only scanner DTO import adjustments if required; retaining `folders` scanner re-exports avoids these changes.
- `tools/app_platform_baseline.json`: remove/shrink only extracted sites, never add allowances.
- `.forgejo/workflows/release-imager.yml`: add moved service's library/doc test commands beside existing native Imager test steps, without running release/signing locally.
- `crates/session/qos-ntfs-helper/tests/native_copy_coordinator.py`: retain CLI compatibility; only change if its supplied test-binary discovery is codified. Its test filter is a substring, not the old full module name, so no forced filter rename is needed.
- `dist/nix/package.nix`: normally no edit. Verify exported `QOS_DESKTOP_TRUSTED_SHELL_PATH` still reaches compilation of folder-service; do not move it to a runtime override.

## Exact semantic surface

These are proposed signatures; the existing source implementations named below remain the implementation. Preserve `Result<_, String>`/`io::Result` mappings and diagnostic text during this ownership move, rather than adding a broad error taxonomy redesign.

The service root has private implementation modules and these public modules only:

```rust
mod connection;
mod destination_security;
mod nofollow;
mod migration;
mod migration_durability;
mod migration_journal;
mod migration_journal_log;
#[cfg(any(target_os = "linux", test))]
mod migration_native;
#[cfg(any(target_os = "linux", test))]
mod migration_native_journal;
mod migration_review;
mod migration_source;
mod migration_store;
mod migration_worker;

pub mod copy;
pub mod review;
pub mod scan;
pub mod session;
pub mod startup;
#[cfg(feature = "test-support")]
pub mod test_support;
```

`copy.rs` exports the following existing types; promote the named definitions/methods to `pub` within private implementation modules, preserving private fields:

```rust
pub use crate::migration::{CopyProgress, CopyReport};
pub use crate::migration_store::{
    IssuePage, MigrationStore, NativePath, OperationRecord, OperationState, StoreWriter,
};
pub use crate::migration_worker::{
    MigrationJob, MigrationWorker, WorkerEvent, WorkerOutcome, durable_copy_available,
};
```

`NativePath` is inert serde-compatible persisted path data, not authority. Keep its Unix bytes/Windows UTF-16 tagged format exactly. Export `encode(&Path)->Self` and `decode(&self)->Result<PathBuf,String>`; do not convert it to `tilo-connected-protocol::NativeString`. That codec permits NUL and has a different wire representation.

| Type | Exact public methods needed by current callers |
| --- | --- |
| `MigrationJob` | `fresh(operation_id: impl Into<String>, source: PathBuf, destination: PathBuf, journal: PathBuf) -> Result<Self,String>`; `with_reviewed_identities(self, source: DirectoryIdentity, parent: DirectoryIdentity, revision: [u8;32]) -> Self`; `operation_id(&self)->&str`; `source(&self)->&Path`; `destination(&self)->&Path`; `journal(&self)->&Path`; `retry(&self)->Result<Self,String>` |
| `MigrationWorker` | `start(job: MigrationJob)->Result<Self,String>`; `start_persisted(job: MigrationJob, writer: StoreWriter, operations: Vec<OperationRecord>, index: usize)->Result<Self,String>`; `cancel(&self)->bool`; `try_next(&mut self)->Option<WorkerEvent>` |
| `MigrationStore` | `open_default()->Result<Self,String>`; `open_at(path:&Path)->Result<Self,String>`; `root(&self)->&Path`; `load_continuation_bytes(&self)->Result<Option<Vec<u8>>,String>`; `save_continuation_bytes(&self, bytes:&[u8])->Result<(),String>`; `load(&self)->Result<Vec<OperationRecord>,String>`; `issue_page(&self, operation:&OperationRecord, offset:u64)->Result<IssuePage,String>` |
| `StoreWriter` | `new(store:Arc<MigrationStore>)->Result<Self,String>`; `save(&self, operations:Vec<OperationRecord>)->Result<mpsc::Receiver<Result<Vec<OperationRecord>,String>>,String>`; `checkpoint(&self, operations:Vec<OperationRecord>)->Result<Vec<OperationRecord>,String>` |
| `OperationRecord` | Preserve the existing signatures for `new`, `issue_count`, `retain_detail_reference`, `compact`, `persistence_failed`, `id`, `source`, `destination`, `journal`, `state`, `verified_bytes`, `verified_file_count`, `current_relative_path`, `issues`, `metadata_limitations`, `failure`, `approve`, `start_job`, `retry_job`, `mark_running`, `apply_progress`, `apply_outcome` from `migration_store.rs:183–403`. All are semantic DTO transitions/status access; no fields become public. See Appendix B for extracted exact signatures. |

The transitive DTO closure is explicitly public through the curated modules; preserve these exact shapes and existing derives/serde attributes:

```rust
// copy module exports the owning definitions; do not duplicate definitions.
pub struct CopyProgress {
    pub verified_files: u64,
    pub verified_bytes: u64,
    pub current_relative_path: PathBuf,
}
pub struct CopyReport {
    pub verified_files: Vec<PathBuf>,
    pub verified_bytes: u64,
    pub skipped: Vec<(PathBuf, String)>,
    pub failed: Vec<(PathBuf, String)>,
    pub cancelled: bool,
    pub scan_completed: bool,
    pub metadata_limitations: Vec<String>,
}
// Preserve CopyReport::complete(&self) -> bool.
pub enum WorkerOutcome {
    Complete(CopyReport), Incomplete(CopyReport), Failed(String), Interrupted,
}
pub enum WorkerEvent {
    Progress(CopyProgress), Finished(WorkerOutcome), Stored(Box<OperationRecord>),
}
pub enum OperationState {
    AwaitingReview, Ready, Running, Interrupted, Cancelled, Incomplete, Failed, Complete,
}
pub struct IssuePage {
    pub issues: Vec<(PathBuf, String)>,
    pub next: Option<u64>,
}
```

FolderScan/SkippedEntry/SkipReason are defined below. OperationRecord, MigrationJob, MigrationStore and MigrationWorker remain opaque; only their curated methods are exposed. StoredProgress, StoredIssue, StoredReport, StoreDocument and SaveRequest stay private. Public result signatures use standard `Path`, `PathBuf`, `String`, `Vec`, `Arc`, `mpsc::Receiver` and these exported DTOs, so no app must import a private implementation module to name a returned type.

`IssuePage::{issues,next}` become public unchanged. CopyProgress/CopyReport fields and WorkerEvent/WorkerOutcome/OperationState variants remain the existing portable definitions, with no OS variant selection added. Job paths/review identities are expectations revalidated by the worker; neither `fresh` nor `with_reviewed_identities` may bypass the existing checked roots, source inventory or journal comparisons. `open_at` preserves explicit fixture/caller-supplied roots but performs the same validation; it does not mint raw authority for the app. G02 later supplies scoped roots without changing this ownership prerequisite.

Add these semantic operations, implemented in Task 1 and moved in Task 2:

```rust
// review.rs; existing Identity fields remain private, serde bytes unchanged.
pub use crate::migration_durability::Identity as DirectoryIdentity;
pub struct CopyReview {
    parent_identity: DirectoryIdentity,
    source_revision: [u8; 32],
}
impl CopyReview {
    pub fn into_expectations(self) -> (DirectoryIdentity, [u8; 32]);
}
pub fn directory_identity(path: &Path) -> Result<DirectoryIdentity, String>;
pub fn path_is_absent(path: &Path) -> Result<bool, String>;
pub fn inspect_copy_review(source: &Path, destination: &Path) -> Result<CopyReview, String>;
pub fn prepare_managed_import_parent() -> Result<PathBuf, String>;

// scan.rs: these are existing folder-estimate data and scanner, moved intact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason { SymbolicLink, ReadError(String), UnsupportedFileType }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedEntry { pub path: PathBuf, pub reason: SkipReason }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderScan {
    pub source: PathBuf,
    pub file_count: u64,
    pub bytes: u64,
    pub empty_directories: Vec<PathBuf>,
    pub skipped: Vec<SkippedEntry>,
}
pub fn scan_folder(root: &Path) -> Result<FolderScan, String>;

// session.rs: implementation-selected internally, no caller backend parameter.
pub fn trusted_identity_complete() -> Result<(), String>;

// startup.rs: all targets; dispatches native modes only where implemented.
pub fn dispatch_early(args: &[String]) -> std::io::Result<Option<i32>>;
```

DirectoryIdentity is a serializable comparison hint (the existing volume/inode record), not a grant, proof or public handle. Do not export any `identity(&File)`, field constructor, native timestamp/metadata object or `PinnedDirectory` accessor. CopyReview has private fields and no deserializer; it holds observations only, and consumption supplies expectations that the engine rechecks.

`prepare_managed_import_parent` preserves current default `dirs::data_local_dir()?.join("qos/files")`, existing `ensure_private_root`, and diagnostics; returns the path only after checks. Canonical stores must remain exactly `dirs::data_local_dir()/qos-imager/migrations` and `dirs::data_local_dir()/qos-imager/connections-v1`, with the same lock files, cross-process exclusive ownership and existing live worker lifetime. Merely discovering old stores while opening a new namespace is not acceptable. No L02 path migration is introduced. Existing portable UI path-existence checks are presentation hints only; they must never replace the service's revalidation/absence checks.

SDK feature/export:

```toml
# crates/app/tilo-app/Cargo.toml
[features]
folder-copy = ["dep:tilo-folder-service"]

[dependencies]
tilo-folder-service = { path = "../../system/folder-service", optional = true }
```

```rust
// crates/app/tilo-app/src/lib.rs
#[cfg(feature = "folder-copy")]
pub mod folders {
    pub use tilo_folder_service::{copy, review, scan, session, startup};
}
```

App imports are `ui::folders::...` on every host; no backend-named Cargo features. Add `folder-copy` alongside existing `portal` on Linux and to existing non-Linux ui declaration; removing other portal cfg wiring is not this plan's work.

## Private visibility and wire invariants

Mechanical mapping is exactly:

```text
crate::setup::connection       -> crate::connection
crate::setup::migration*       -> crate::migration*
crate::setup::destination_security -> crate::destination_security
pub(in crate::setup)           -> pub(crate)  [only moved internal sites]
```

Use explicit longest-prefix replacements, including docs and string test filters where intended, and review each remaining `crate::setup` occurrence. Ordinary sibling `super::` references stay valid because all support modules move as root siblings. Preserve narrower `pub(in crate::connection::transaction...)` scopes; never widen these to `pub(crate)` just to compile. Only the listed copy/review exports gain public reachability. `ConnectionStore`, EndpointContext native registration, native ReadEndpoint variants, transaction/session/evidence constructors, NtfsReadEndpoint native client methods, source traits, staged files, logs and filesystem descriptors remain inaccessible from service/SDK public APIs.

No new `File`, RawFd/OwnedFd, Windows HANDLE, libc/windows-sys type, platform callback or public backend factory crosses `ui::folders`. Preserve all native/persisted bytes: store CURRENT/pages/log/namespace descriptors; QOSJNL3 native journal; migration index/continuation tagged native paths; lease frames; NTFS schema 3; exact CLI flags. Public model data such as `VerifiedBaseline::from_verified` remains under private connection; no need to change its existing data-only semantics.

## Startup and packaging contract

Keep production self-reexecution; no new deployed helper filename or PATH search is needed. `Receiver::start` continues to use current_exe, whose binary must call `dispatch_early` before constructing UI, runtime or threads. Imager is the current production host. An example host validates headless reuse. The service documents this host composition requirement and provides the dispatcher; do not imply arbitrary binaries can safely use receiver execution without that setup.

```rust
// startup.rs
pub fn dispatch_early(args: &[String]) -> std::io::Result<Option<i32>> {
    #[cfg(target_os = "linux")]
    {
        if let Some(code) = crate::connection::maybe_run_lease_receiver(args) {
            return Ok(Some(code));
        }
        return crate::migration_native::maybe_run_command(args);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(None)
    }
}
```

The non-Linux None is **early Linux administrative-mode dispatch**, not a false implementation of a required filesystem feature. Preserve live capability refusals in the actual service.

```rust
// main.rs, immediately after collecting args, replaces only the two old blocks
if let Some(code) = ui::folders::startup::dispatch_early(&args)? {
    std::process::exit(code);
}
```

```rust
// examples/early-host.rs: test host, never a production package fallback
fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    std::process::exit(tilo_folder_service::startup::dispatch_early(&args)?.unwrap_or(2));
}
```

Keep receiver flag `--qos-connected-lease-receiver`, exclusive fd 3 adoption, exact argument count, parent PID/UID checks, close_range CLOEXEC refusal, no-new-privileges, signal isolation, epoch request matching, unique reservation/deferred reaping and all timeouts. Keep `QOS_LEASE_HELPER` test-only. Neither app nor public caller can supply an executable/command or configure weaker confinement. Retain native copy CLI `--native-copy-selected`/`--native-resume-selected` and its bounded descriptor, identity, cancellation and diagnostic-only reporting.

NTFS parser stays installed at `/usr/libexec/qos-ntfs-probe` by its current owner. Do not add root account/service/policy activation or parser dependencies to folder-service. `QOS_DESKTOP_TRUSTED_SHELL_PATH` remains compile-time package input from `dist/nix/package.nix:73`, now read when compiling the service readiness module. FHS sibling-shell logic and peer/executable authentication remain unchanged. Do not convert this to a caller-selected trusted executable API.

---

### Task 1: Isolate semantic review calls inside the current Imager tree

**Files:** Modify `apps/imager/src/setup/{migration.rs,migration_store.rs,desktop_continuation.rs,import_view.rs}`. Tests remain in their existing modules. This task changes no Cargo ownership and leaves a buildable app for its own review.

**Interfaces:** Consumes the current PinnedDirectory, SourceInventory, Identity, ensure_private_root. Produces the four review functions and CopyReview above at `pub(super)` scope, temporarily in `migration.rs`, plus `prepare_managed_import_parent` in `migration_store.rs`. The next task moves these exact bodies to service review.rs.

- [ ] Record pre-task hashes, exact base commit and relevant uncommitted WIP hashes in this plan's SDD ledger. Verify no simultaneous writer in these files. Recheck that native connection subtree hashes still agree with the accepted source manifest.
- [ ] Record existing production-path regression evidence before changes: `cargo test -p imager --lib setup::desktop_continuation::tests --locked` and `cargo test -p imager --lib setup::import_view::tests --locked` on an admitted host. Do not call a failing baseline known-red without fresh evidence.
- [ ] Move the native bodies of continuation `path_identity` and `absent` to the review helpers in `migration.rs` with unchanged validation/error order. Retain the small portable `absolute` representation validator in the app for continuation decoding, and include that exact validation at the service directory-identity boundary; it does not perform native I/O or mint authority. Add `CopyReview` and `inspect_copy_review` using the exact native portion of `Continuation::resolve`:

```rust
pub(super) struct CopyReview {
    parent_identity: migration_durability::Identity,
    source_revision: [u8; 32],
}
impl CopyReview {
    pub(super) fn into_expectations(self) -> (migration_durability::Identity, [u8; 32]) {
        (self.parent_identity, self.source_revision)
    }
}
pub(super) fn inspect_copy_review(source: &Path, destination: &Path) -> Result<CopyReview, String> {
    let parent = destination.parent().ok_or("destination has no parent")?;
    let pin = PinnedDirectory::open_absolute(parent).map_err(|e| e.to_string())?;
    pin.validate_destination().map_err(|e| e.to_string())?;
    Ok(CopyReview {
        parent_identity: directory_identity(parent)?,
        source_revision: super::migration_review::SourceInventory::capture(
            &PinnedDirectory::open_absolute(source).map_err(|e| e.to_string())?,
        ).and_then(|inventory| inventory.revision()).map_err(|e| e.to_string())?,
    })
}
```

- [ ] Keep `Continuation::resolve` overlap/collision/message policy unchanged; replace its native pin/inventory block with `let (parent_identity, source_revision) = inspect_copy_review(&source, &destination)?.into_expectations();`, then store `Some(source_revision)` in the same serde fields. Replace all `path_identity` and `absent` usages with semantic imports/aliases. Do not turn deserialized hints into live authority.
- [ ] Add and use the managed-parent operation, removing the discarded-pin call from import_view:

```rust
pub(super) fn prepare_managed_import_parent() -> Result<PathBuf, String> {
    let parent = dirs::data_local_dir()
        .ok_or("No local user data directory")?
        .join("qos/files");
    ensure_private_root(&parent).map_err(|e| e.to_string())?;
    Ok(parent)
}
```

- [ ] Rerun the two scoped test commands and `cargo check -p imager --tests --locked`. Existing tests `source_swapped_after_review_is_rejected_by_worker_before_journal_or_output`, `destination_swapped_after_review_is_rejected_before_output`, `edits_after_inventory_validation_cannot_become_the_copy_baseline`, and `unpublished_retry_refuses_existing_names_and_unavailable_parents` are mandatory preserved evidence. This is semantics-preserving seam extraction; do not add dummy function-existence tests.
- [ ] Review semantic diff separately from moved function bodies. Finish with independently reviewed WIP and a source manifest. Do not stage or commit any files: parent policy currently prohibits commits.

### Task 2: Move the invariant closure and migrate all callers atomically

**Files:** All Appendix A moves, new service files listed above, root/SDK/Imager manifests, `main.rs`, setup declarations/consumers, exact baseline entries. `session.rs` includes the two native readiness tests moved from continuation. `scan.rs` includes scanner tests from folders; picker stdout test stays app-side.

**Interfaces:** Consumes Task 1 review helpers and existing engines. Produces the curated SDK surface, production early dispatcher, service test-support feature and headless example. This is intentionally one coherent buildable increment: splitting it across a public raw-handle bridge is prohibited.

- [ ] Before moving, create `relocation-before.json` in this plan's SDD directory using Appendix A and SHA-256 each source. Record tests/filter strings and existing ignored reasons. Prepare regression probes for the public boundary as service `lib.rs` rustdoc compile-fail blocks:

```rust,compile_fail
use tilo_folder_service::connection::transaction::native::NativeRestore;
```

```rust,compile_fail
use tilo_folder_service::migration::PinnedDirectory;
```

```rust,compile_fail
use tilo_folder_service::review::DirectoryIdentity;
let _ = DirectoryIdentity { volume: vec![], inode: 0 };
```

These rustdoc tests compile as external consumer crates. They must fail for privacy, not because the crate is missing. Inspect compiler diagnostics during review. Add this positive service doc snippet so a broken export graph cannot pass only compile-fail examples:

```rust
use std::path::PathBuf;
use tilo_folder_service::{copy::{MigrationJob, MigrationStore}, review::DirectoryIdentity, scan::FolderScan};
fn describe(source: PathBuf, destination: PathBuf, journal: PathBuf) -> Result<MigrationJob, String> {
    MigrationJob::fresh("review-fixture", source, destination, journal)
}
let _ = describe as fn(PathBuf, PathBuf, PathBuf) -> Result<MigrationJob, String>;
```

Add the corresponding SDK public-module docs (compiled with `cargo test -p tilo-app --doc --features folder-copy --locked`) with a positive import `use tilo_app::folders::{copy::MigrationJob, review::DirectoryIdentity, scan::FolderScan};` and these failures:

```rust,compile_fail
use tilo_app::folders::connection::endpoint::EndpointContext;
```

```rust,compile_fail
use tilo_app::folders::copy::MigrationStore;
fn steal(store: MigrationStore) { let _ = store.root.file; }
```

Capture actual negative-compile diagnostics in addition to rustdoc status. After building the positive API and native-host artifacts, save Cargo JSON and run the following external consumer probe script in this plan's SDD directory; no new lockfile or app execution is needed:

```sh
cargo build -p tilo-folder-service --lib --message-format=json --locked > .superpowers/sdd/2026-09-18-connected-folder-extraction/service-artifacts.jsonl
cargo build -p tilo-app --lib --features folder-copy --message-format=json --locked > .superpowers/sdd/2026-09-18-connected-folder-extraction/sdk-artifacts.jsonl
```

```python
from pathlib import Path
import json, subprocess
w = Path('.superpowers/sdd/2026-09-18-connected-folder-extraction').resolve()
def artifact(log, target, required_features=(), forbidden_features=('test-support',)):
    found = []
    for line in (w / log).read_text().splitlines():
        obj = json.loads(line)
        if obj.get('reason') == 'compiler-artifact' and obj['target']['name'] == target:
            features = set(obj['features'])
            assert set(required_features) <= features, (target, features)
            assert not features.intersection(forbidden_features), (target, features)
            (w / f'{target}-{log}.features.json').write_text(json.dumps(sorted(features)))
            found += [Path(p) for p in obj['filenames'] if p.endswith('.rlib')]
    assert len(set(found)) == 1, found
    return found[0]
libs = {
    'tilo_folder_service': artifact('service-artifacts.jsonl', 'tilo_folder_service'),
    'tilo_app': artifact('sdk-artifacts.jsonl', 'tilo_app', required_features=('folder-copy',)),
}
cases = [
 ('positive_service', 'tilo_folder_service', 'use tilo_folder_service::{copy::MigrationJob, review::DirectoryIdentity, scan::FolderScan};', None),
 ('private_connection', 'tilo_folder_service', 'use tilo_folder_service::connection::transaction::native::NativeRestore;', 'E0603'),
 ('private_pin', 'tilo_folder_service', 'use tilo_folder_service::migration::PinnedDirectory;', 'E0603'),
 ('identity_fields', 'tilo_folder_service', 'fn f() { let _ = tilo_folder_service::review::DirectoryIdentity { volume: vec![], inode: 0 }; }', 'E0451'),
 ('positive_sdk', 'tilo_app', 'use tilo_app::folders::{copy::MigrationJob, review::DirectoryIdentity, scan::FolderScan};', None),
 ('sdk_no_native', 'tilo_app', 'use tilo_app::folders::connection::endpoint::EndpointContext;', 'E0433'),
 ('sdk_no_handle', 'tilo_app', 'fn f(s: tilo_app::folders::copy::MigrationStore) { let _ = s.root.file; }', 'E0616'),
]
for name, crate, source, expected in cases:
    src = w / f'{name}.rs'
    src.write_text(source + '\n')
    lib = libs[crate]
    result = subprocess.run(['rustc', '--edition=2024', '--crate-type=lib',
        '--emit=metadata', '--error-format=json', '--extern', f'{crate}={lib}',
        '-L', f'dependency={lib.parent}', str(src), '-o', str(w / f'{name}.rmeta')],
        text=True, capture_output=True)
    (w / f'{name}.diagnostics.jsonl').write_text(result.stderr)
    diagnostics = [json.loads(line) for line in result.stderr.splitlines() if line.startswith('{')]
    codes = {d['code']['code'] for d in diagnostics if d.get('code')}
    assert 'E0463' not in codes, (name, result.stderr)
    if expected is None:
        assert result.returncode == 0, (name, result.stderr)
    else:
        assert result.returncode != 0 and expected in codes, (name, result.stderr)
```

The expected SDK missing-module diagnostic is E0433 for the qualified path as written; if the pinned compiler classifies it differently, reviewer must inspect the full message and correct the probe expectation only when it still proves the native module is inaccessible and the positive SDK import succeeds. Never treat an arbitrary compile error as privacy proof. These direct rustc probes are host-compiled; cross-target builds need matching target artifacts/compiler flags, not host rlibs.

Record feature configuration from Cargo's artifact messages, never infer it from an rlib filename. Verify the service artifact in the normal SDK build also lacks `test-support`. Then repeat the same positive and negative probes against artifacts built with `cargo build -p tilo-folder-service --lib --features test-support --message-format=json --locked` and `cargo build -p tilo-app --lib --features folder-copy,tilo-folder-service/test-support --message-format=json --locked`, saving separate artifact and diagnostic files. For this second pass require `test-support` on the service artifact and clear that forbidden feature for its selection; keep the SDK's existing unrelated `test-support` feature disabled. Verify the SDK build's dependency artifact carries the requested service feature. All native proof/pin and private-field probes must still fail for the intended reason with dev support enabled. This establishes both feature configurations without changing the normal public build or relying solely on the absence of a named re-export.

The lexical app checker is not an existing API/graph checker. These are new focused external-consumer privacy probes; `cargo tree` plus explicit review provides graph evidence. Do not reference a nonexistent ui::platform module or phantom fixture gate.

- [ ] Create service Cargo.toml with package name `tilo-folder-service`, edition 2024, workspace license/publish/lints, `test-support=[]`. Copy exact existing cap dependencies (3.4.6), dirs/serde/serde_json/sha2 workspace declarations and qos-ntfs-protocol path. Target Unix libc and Windows windows-sys version `0.61.2`/feature set come from Imager unchanged initially. Put tempfile under dev-dependencies. No ui/winit/vello, reqwest, NTFS helper or native parser dependency. Add root workspace membership. Do not add atomic-file merely because it exists; the existing mechanisms are not yet proven interchangeable.
- [ ] Apply Appendix A full-file moves using explicit paths, then path/visibility mapping. Transfer Task 1 functions into `review.rs`, referencing root siblings. Keep private helper `ensure_private_root` in migration_store. Publicize exactly the semantic types/methods inventory above; keep every native field private.
- [ ] Extract scanner types/functions/helpers from `folders.rs:14–205` to scan.rs, and replace their app definitions with `pub use ui::folders::scan::{FolderScan,SkippedEntry,SkipReason,scan_folder};`. Keep picker source/UI dependency in the app. Move tests `nested_files_and_empty_directories_are_enumerated`, `missing_and_non_directory_roots_are_errors`, `symbolic_links_are_skipped_and_never_traversed`, `a_scan_is_only_a_point_in_time_estimate`, `nofollow_open_rejects_a_directory_replaced_by_a_symlink` with backend; leave `dialog_stdout_preserves_spaces_and_rejects_invalid_utf8` beside picker.
- [ ] Extract readiness functions `trusted_identity_complete`, `authenticated_session_ready`, `authenticate_session_peer`, `connect_readiness` into session.rs together, including both OS-selected definitions. Move `direct_invocation_without_shell_and_fake_ready_peer_are_refused` and `readiness_connection_refuses_a_full_backlog_without_hanging` into its local tests; copy only their direct fixture imports. In continuation, replace native imports/implementations with `use ui::folders::session::trusted_identity_complete;` or re-export with existing `pub(super)` visibility for its app caller. Preserve compile-time trusted shell path.
- [ ] Create startup.rs/example exactly as specified; replace both old Linux cfg dispatch blocks in main. Remove old connection/migration/security/nofollow module declarations. Ordinary app consumers now import `ui::folders::{copy,review}`; no broad `pub use tilo_folder_service::*` or native endpoint exports.
- [ ] Keep cross-crate app test hooks usable with **dev-only service feature**:

```toml
# apps/imager/Cargo.toml
[dev-dependencies]
tilo-folder-service = { path = "../../crates/system/folder-service", features = ["test-support"] }
```

Change migration_store's test-hook module and before_save injection to `#[cfg(any(test,feature="test-support"))]`, leaving hook implementations private. Service `test_support.rs` provides:

```rust
pub fn hold_save(path: &std::path::Path, nth: usize)
    -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
    crate::migration_store::test_support::hold_save(path, nth)
}
pub fn save_snapshot(store: &crate::copy::MigrationStore,
                     operations: &[crate::copy::OperationRecord]) -> Result<(), String> {
    store.save_snapshot(operations).map(drop)
}
```

Make `save_snapshot` crate-visible (not public) for this wrapper. Change app test-only `store.save(...)` calls to `tilo_folder_service::test_support::save_snapshot(...)`; change `hold_save` imports similarly. Production has no test-support feature.

The one continuation test that calls `migration_journal::copy_reviewed` keeps production-path injection through a gated wrapper with the **same** signature, substituting the public `DirectoryIdentity` alias in the tuple. Name this wrapper `run_reviewed_copy_for_test`, use it only in the existing source-edit regression, and keep its implementation a direct call to `crate::migration_journal::copy_reviewed`. Also expose the existing `resume_folder` function only in test_support for that test. Do not expose native chain admissions, historical proof construction, mutable store journals, NativeEpoch, unrestricted callback-bearing native drivers or endpoint factories through test support. Do not expose CopyFileSource or raw metadata. Keep `MigrationWorker::start` public because production shots use it; do not mistake that caller for a test-only method.

- [ ] Rewrite hardcoded reexec filters, not just Rust use paths:

```text
setup::connection::store::tests::two_processes_cannot_own_store
 -> connection::store::tests::two_processes_cannot_own_store
setup::connection::store::tests::killed_owner_leaves_durable_pending_intent
 -> connection::store::tests::killed_owner_leaves_durable_pending_intent
setup::connection::store::storage::windows::tests::native_windows_lock_child
 -> connection::store::storage::windows::tests::native_windows_lock_child
setup::connection::transaction::native::linux::tests::native_executor_process_restart_boundaries
 -> connection::transaction::native::linux::tests::native_executor_process_restart_boundaries
setup::connection::transaction::native::linux::lease_process::tests::native_isolated_receiver_regressions
 -> connection::transaction::native::linux::lease_process::tests::native_isolated_receiver_regressions
```

Search moved files for any additional `setup::` literal and explain each remaining use. Native-copy coordinator's `native_selected_copy_command_worker` substring remains correct; its `--native-test` argument must reference the service unit-test executable now, not Imager's.

- [ ] Delete exact baseline entries whose full source files moved. For partial files/main/manifest, run `python3 tools/check_app_platform_contract.py --inventory` and shrink/remove only sites actually gone; never increase count or add entries. Preserve unrelated allowances. Verify `python3 tools/check_app_platform_contract.py` and `python3 -m unittest discover -s tools/tests -p test_app_platform_contract.py`. Keep the existing same-path multi-process tests `two_processes_cannot_own_store` and the migration-store lock tests pointed at the canonical namespace; no alternate namespace or process-local mutex can satisfy them. Original 28 violations were all connection sites; any fresh unrelated violation is attributed separately, never silently absorbed.
- [ ] Run portable/host-appropriate checks:

```sh
cargo check -p tilo-folder-service --all-targets --locked
cargo test -p tilo-folder-service --lib connection::model --locked
cargo test -p tilo-folder-service --lib connection::reconcile --locked
cargo test -p tilo-folder-service --lib scan::tests --locked
cargo test -p tilo-folder-service --doc --locked
cargo test -p tilo-app --doc --features folder-copy --locked
cargo check -p imager --tests --locked
cargo test -p imager --lib setup::desktop_continuation::tests --locked
cargo test -p imager --lib setup::import_view::tests --locked
cargo build -p tilo-folder-service --example early-host --locked
cargo build -p imager --bin imager --locked
```

Run migration/store/native backend tests only in their admitted host lane as specified in Task 3; unavailable host evidence is not green. Document target for every result.

- [ ] Produce relocation-after manifest and residual semantic diff (review procedure below). Independent reviewer receives original/final hashes, normalized move proof, the **complete** residual diff, ABI/export surface and actual test outputs. Resolve privacy/caller/startup/test regressions before completing this WIP increment. Do not stage or commit. This is an ownership-change review gate, not full host certification.

### Task 3: Qualify moved native paths, installed startup and distribution test ownership

**Files:** `.forgejo/workflows/release-imager.yml`; `dist/verify-payload.sh`, `dist/ci/verify-nix-result.sh`, and `dist/ci/run-payload-contract-tests.sh` for the installed receiver smoke and its harness regressions; narrowly required shared test helper or explicit test-runtime provisioning in `dist/ci/install-build-deps.sh`/`dist/ci/run-package-job.sh`; spec/execution evidence documents; service/app test files only if the moved tests reveal a genuine extraction bug. No transaction algorithm redesign or release publishing. The parent's attempted Linux app-state cross-check was blocked by missing `aarch64-linux-gnu-gcc` in blake3; atomic-file Linux tests compiled. This is toolchain evidence, not evidence for this new service. Before target checks, provision the target Rust standard library, actual target linker/C compiler and native libraries reported by `tools/check-build-env.sh`/`DEVELOPMENT.md`; never disable native features or substitute a host artifact to make checks green.

**Interfaces:** Uses Task 2 production dispatcher in Imager and the headless example, and unchanged NTFS coordinator command interface. Produces native and installed-host evidence or explicit outstanding gate records; cannot fabricate fixture availability.

- [ ] Run the complete service nonignored library tests on each native runner where their existing preconditions hold, and add `cargo test -p tilo-folder-service --lib --locked` plus `cargo test -p tilo-folder-service --doc --locked` alongside existing Imager commands in each release workflow host lane. Do not remove existing Imager/action tests. The ordinary affected-package graph should already include the new service; verify from CI's actual package selection rather than hardcoding a second graph implementation.
- [ ] On a separately provisioned Linux endpoint fixture, run exact production-path native groups. Require both environment values at shell entry; absence fails:

```sh
: "${QOS_ENDPOINT_FIXTURE:?authorized disposable native filesystem root required}"
: "${QOS_LEASE_HELPER:?absolute dispatcher-bearing helper executable required}"
cargo test -p tilo-folder-service --lib connection::endpoint --locked -- --ignored --nocapture
cargo test -p tilo-folder-service --lib connection::transaction::native --locked -- --ignored --nocapture
```

Set QOS_LEASE_HELPER to the absolute built `target/debug/examples/early-host` for headless engine qualification, then rerun the receiver/lease group with the installed Imager executable to prove packaged startup. A filter returning zero tests fails this gate; capture listed selected test names/count before run. Do not run every ignored migration test blindly: some require different administrative/image fixtures.

- [ ] On Windows's provisioned endpoint root, run moved endpoint/storage native suites and their child-process lock tests; preserve existing lack of transaction execution separately:

```sh
: "${QOS_ENDPOINT_FIXTURE:?authorized Windows native endpoint root required}"
cargo test -p tilo-folder-service --lib connection::endpoint::native --locked -- --ignored --nocapture
cargo test -p tilo-folder-service --lib connection::store::storage::windows --locked
```

Use the native runner's shell equivalent for required environment checks. Inventory cfg-selected names first; Windows endpoint is compiled as `endpoint::native`, not a literal `endpoint::windows` module. Never accept zero selected tests. On macOS run moved ACL/durability/migration tests; endpoint transaction refusal does not count as implementation proof.

- [ ] Retain `qos-ntfs-helper` tests with their owner. When authorized fixture coordinator data exist, provide current built probe/Imager paths and the service unit-test executable via existing `--native-test`, keeping coordinator authorization `QOS_NTFS_NATIVE_AUTHORIZATION=DISPOSABLE_FIXTURE_ONLY` and externally provisioned device/client UID. Do not create or claim a device from this plan. Run existing positive/resume/cancellation/death/crash modes selected by the native qualification owner; each missing mode remains outstanding. Preserve root peer checks, nonce/request sequencing, no ancillary FDs and worker kill/reap/confinement. Only `--native-test` discovery changes after extraction.
- [ ] Native Linux installed startup smoke must sanitize inherited descriptors. From a clean installed package (no checkout/PATH dependency), invoke its executable with the receiver flag after closing fd 3 and assert exit 2; invoke `--tilo-package-smoke` and assert 0; invoke ordinary arguments in the existing package smoke harness, not the user's desktop. Passing the invalid-fd test proves early dispatch only, not leases. Full installed lease test uses QOS_LEASE_HELPER pointed at that installed executable in the real native suite above.

Example exact subprocess assertions for a Linux package test harness:

```python
import os, subprocess

def close_channel():
    try:
        os.close(3)
    except OSError:
        pass

r = subprocess.run([installed_imager, '--qos-connected-lease-receiver'],
                   close_fds=True, preexec_fn=close_channel,
                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
assert r.returncode == 2, (r.returncode, r.stdout, r.stderr)
r = subprocess.run([installed_imager, '--tilo-package-smoke'],
                   close_fds=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
```

The existing FHS and Nix verifiers currently invoke only `--tilo-package-smoke`; add the bounded invalid-fd receiver check to both existing owners. Their regression script currently installs native `true` as the import executable. Replace only that test fixture with a receiver-aware native fixture or equally meaningful fixture arrangement; a stub returning zero for the receiver flag must now fail. Preserve ELF, architecture and shared-library checks. Add wrong-exit and timeout negative controls without bypassing checks in fixture mode. If using the Python example, explicitly provide its test runtime in the minimal distro/Nix harnesses; do not assume Python happens to be installed or add it as a shipped application dependency. Fixture-harness success is not installed production-dispatch evidence.

`installed_imager` is the package harness's actual installed absolute path, not a new executable locator. Keep this in the existing packaging test owner/harness when implementing, without introducing a release/signing action. No new deployed binary needs packaging because production uses the same installed Imager executable.

- [ ] Verify service dependency graph without executing native code: `cargo tree -p tilo-folder-service --edges normal --locked` must exclude rendering/UI/app crates and qos-ntfs-helper. SDK consumers with folder-copy receive the same semantic source API on each host. Public API review confirms no connection/native endpoint exports; doc privacy tests pass. Native backend dependencies are legitimate inside the service.
- [ ] Run final repository-required `cargo check -p shell --tests --locked` after coordinated build slot availability. Record any failure with fresh cause and exact scope; do not blame unrelated baseline without evidence.
- [ ] Update execution evidence: original source sites deleted, moved implementation paths, migrated caller/test lists, exact boundary-check result, host-specific test names/results, installed distribution results and remaining G02/S01/I01/C01/N02/N04/native gates. Do not mark required hosts complete with preserved refusal. Independent final review covers the entire range, including Task 1 seam changes, Task 2 visibility/startup/test hooks, Task 3 workflow/docs. Hand reviewed WIP and its hashes to the parent; no staging, commit, push, merge or publish is permitted without a parent policy change.

## Review procedure for the large mechanical move

The mechanical scope is roughly 70,000 lines including tests, so do not ask reviewers to infer semantic changes from rename detection alone.

1. At Task 2 base, record `git rev-parse HEAD` and SHA-256 source bytes for every Appendix A row. Record source logical module path and intended destination. Current no-commit policy means the baseline is HEAD plus explicitly recorded WIP bytes, not HEAD alone; do not reconstruct inputs from git show if it would discard shared accepted WIP.
2. Generate normalized expected destination bytes by applying the explicit path rewrites above to each source. Keep a machine-readable list of replacements per file. Do not normalize away `pub` changes, cfg changes, assertions, constants, code motion, error strings, field changes or test annotations. Any of those remains semantic residual.
3. Compare final destination bytes to normalized expected bytes. Save complete unified residual diffs and before/after hashes, including each differing file. List byte-identical normalized files separately. Full `git diff --find-renames` is still supplied; mechanical proof supplements rather than hides it.
4. Review non-full-file moves (scanner/readiness/review helper split), every app caller/manifests/baseline/workflow modification, test support feature and startup dispatch completely. Include all deleted function bodies and their new locations so omission is visible.
5. Verify original-to-destination inventory is one-to-one: all 46 full-file rows exist at destination and no original implementation remains. Confirm no path includes back to app, no app dependency in service graph, no implementation duplicate left as “compatibility”. App semantic re-exports are allowed only for scanner names; no broad migrated module facade.
6. Count tests and preserve every existing ignored reason/annotation; account for each renamed filter and relocated test. Run `cargo test -p tilo-folder-service --lib -- --list` and matched native `--ignored --list` inventories; absence/zero matches fails. Assertions or production paths are never weakened to make the move green.
7. Task reviewer verdicts both spec compliance and code quality. Any unexplained residual/native-authority change blocks acceptance and is returned to the implementer; do not silently declare the 18,000-line Linux backend mechanical if it differs beyond audited paths.

## Plan self-review and concrete open gates

- Closure: Appendix A includes complete connection, migration source/native journals, destination security and nofollow; scanner/readiness direct consumers are explicitly migrated. Cache and runtime-connected/protocol remain separate.
- Source/API: native evidence remains private; DirectoryIdentity/NativePath are documented serialized hints; copy engines revalidate them. No new endpoint execution UI feature is invented.
- Startup: framework owns dispatch, Imager invokes it before runtime, self-reexec contract preserved, example validates headless use. Installed receiver/native-copy executable paths stay compatible.
- Data: no schema/path/wire/namespace migration; test-support does not change production authorities; Nix trusted-shell compile input retained.
- Validation: preserved tests plus meaningful boundary probes; exact native test-filter relocation; no zero-test/absent-fixture pass; boundary exceptions only retired; final shell tests compile.
- Remaining concrete execution decisions are environmental, not new API guesses: which authorized Linux/Windows native fixtures/runners and installed package artifact are available, and who grants the exclusive source/build slot. Missing evidence is recorded, not filled with a stub. The implementation/API choices above are settled for this draft. At handoff supply the Imager owner with baseline HEAD, relevant before/after WIP hashes, complete old→new module/API mapping, literal test-filter changes, and actual affected-target evidence so its seven-file chain brief can rebase without semantic changes. Use parent-supplied coordination, not another task's private SDD files.


## Frozen WIP handoff protocol

No staging or commits. Freeze the relocation writer before handing off to the Imager chain writer, and allow only one of those writers to run at a time. Parent coordinates transfer; independent readers are allowed. Handoff consists of `baseline.json`, `relocation-before.json`, `relocation-after.json`, `semantic.diff`, complete `snapshot.tar`, and validation logs tied to the exact after hashes, all in this plan's SDD directory. Include newly created files and deletion tombstones; a plain git diff is insufficient for untracked files.

Build the snapshot from the explicit union of Appendix A source/destination paths and this plan's modified/created caller, manifest, SDK, baseline and workflow paths. Save the complete array as `handoff-paths.json` before snapshot creation. Exclude unrelated shared WIP from the artifact, but list its paths separately in baseline.json so it is not accidentally attributed to this unit. Record HEAD and the affected files' actual bytes at both task start and finish. Rehash the snapshot list at ownership transfer; a mismatch reopens verification for those inputs before the peer chain starts.

This exact snapshot core handles new files and deletions without staging:

```python
from pathlib import Path
import hashlib, json, subprocess, tarfile

root = Path.cwd()
workspace = root / '.superpowers/sdd/2026-09-18-connected-folder-extraction'
paths = json.loads((workspace / 'handoff-paths.json').read_text())
rows = []
with tarfile.open(workspace / 'snapshot.tar', 'w') as archive:
    for relative in sorted(set(paths)):
        path = root / relative
        if not path.exists():
            rows.append({'path': relative, 'state': 'deleted'})
            continue
        if not path.is_file() or path.is_symlink():
            raise RuntimeError(f'Unexpected non-regular source: {relative}')
        data = path.read_bytes()
        rows.append({'path': relative, 'state': 'present',
                     'sha256': hashlib.sha256(data).hexdigest()})
        archive.add(path, arcname=relative, recursive=False)
(workspace / 'relocation-after.json').write_text(json.dumps({
    'head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
    'files': rows,
}, indent=2) + '\n')
```

The final handoff also states exact old→new module/API mapping and subprocess filter changes, known validation limitations and all modified source/Cargo.lock hashes. Validation against a different WIP snapshot is not transferable. If the source changes after a test, rerun the affected check; do not label the prior output as final evidence.

## Appendix A: exact full-file relocation manifest

All source paths begin `apps/imager/src/setup/`; all destination paths begin `crates/system/folder-service/src/`. Each row uses the exact same relative suffix below. This table contains 46 moves, including every test file. Original hashes are in the accepted design; execution refreshes them after Task 1.

| Source suffix | Destination suffix |
| --- | --- |
| `connection/endpoint/linux/ntfs.rs` | `connection/endpoint/linux/ntfs.rs` |
| `connection/endpoint/linux.rs` | `connection/endpoint/linux.rs` |
| `connection/endpoint/unsupported.rs` | `connection/endpoint/unsupported.rs` |
| `connection/endpoint/windows.rs` | `connection/endpoint/windows.rs` |
| `connection/endpoint.rs` | `connection/endpoint.rs` |
| `connection/mod.rs` | `connection/mod.rs` |
| `connection/model.rs` | `connection/model.rs` |
| `connection/reconcile.rs` | `connection/reconcile.rs` |
| `connection/store/directory.rs` | `connection/store/directory.rs` |
| `connection/store/directory_tests.rs` | `connection/store/directory_tests.rs` |
| `connection/store/format.rs` | `connection/store/format.rs` |
| `connection/store/recovery/directory.rs` | `connection/store/recovery/directory.rs` |
| `connection/store/recovery.rs` | `connection/store/recovery.rs` |
| `connection/store/storage/windows.rs` | `connection/store/storage/windows.rs` |
| `connection/store/storage.rs` | `connection/store/storage.rs` |
| `connection/store/tests.rs` | `connection/store/tests.rs` |
| `connection/store.rs` | `connection/store.rs` |
| `connection/transaction/directory.rs` | `connection/transaction/directory.rs` |
| `connection/transaction/native/descriptor/directory.rs` | `connection/transaction/native/descriptor/directory.rs` |
| `connection/transaction/native/descriptor.rs` | `connection/transaction/native/descriptor.rs` |
| `connection/transaction/native/directory.rs` | `connection/transaction/native/directory.rs` |
| `connection/transaction/native/linux/directory/tests.rs` | `connection/transaction/native/linux/directory/tests.rs` |
| `connection/transaction/native/linux/directory.rs` | `connection/transaction/native/linux/directory.rs` |
| `connection/transaction/native/linux/lease_process.rs` | `connection/transaction/native/linux/lease_process.rs` |
| `connection/transaction/native/linux/witness.rs` | `connection/transaction/native/linux/witness.rs` |
| `connection/transaction/native/linux.rs` | `connection/transaction/native/linux.rs` |
| `connection/transaction/native/mod.rs` | `connection/transaction/native/mod.rs` |
| `connection/transaction/native/witness.rs` | `connection/transaction/native/witness.rs` |
| `connection/transaction/tests.rs` | `connection/transaction/tests.rs` |
| `connection/transaction.rs` | `connection/transaction.rs` |
| `connection/windows_native.rs` | `connection/windows_native.rs` |
| `destination_security/macos_acl.rs` | `destination_security/macos_acl.rs` |
| `destination_security/mod.rs` | `destination_security/mod.rs` |
| `destination_security/unix.rs` | `destination_security/unix.rs` |
| `destination_security/windows.rs` | `destination_security/windows.rs` |
| `migration.rs` | `migration.rs` |
| `migration_durability.rs` | `migration_durability.rs` |
| `migration_journal.rs` | `migration_journal.rs` |
| `migration_journal_log.rs` | `migration_journal_log.rs` |
| `migration_native.rs` | `migration_native.rs` |
| `migration_native_journal.rs` | `migration_native_journal.rs` |
| `migration_review.rs` | `migration_review.rs` |
| `migration_source.rs` | `migration_source.rs` |
| `migration_store.rs` | `migration_store.rs` |
| `migration_worker.rs` | `migration_worker.rs` |
| `nofollow.rs` | `nofollow.rs` |

## Appendix B: exact OperationRecord method signatures

Copy existing bodies unchanged; `pub(super)` becomes `pub` only on these methods in the private implementation module. Other helper visibility remains restricted.

```rust
pub fn new(job: &MigrationJob) -> Self;
pub fn issue_count(&self) -> usize;
pub fn retain_detail_reference(&mut self, saved: &Self);
pub fn compact(&mut self);
pub fn persistence_failed(&mut self, error: &str);
pub fn id(&self) -> &str;
pub fn source(&self) -> Result<PathBuf, String>;
pub fn destination(&self) -> Result<PathBuf, String>;
pub fn journal(&self) -> Result<PathBuf, String>;
pub fn state(&self) -> OperationState;
pub fn verified_bytes(&self) -> u64;
pub fn verified_file_count(&self) -> usize;
pub fn current_relative_path(&self) -> Result<Option<PathBuf>, String>;
pub fn issues(&self) -> Result<Vec<(PathBuf, String)>, String>;
pub fn metadata_limitations(&self) -> &[String];
pub fn failure(&self) -> Option<&str>;
pub fn approve(&mut self);
pub fn start_job(&self) -> Result<MigrationJob, String>;
pub fn retry_job(&self) -> Result<MigrationJob, String>;
pub fn mark_running(&mut self) -> Result<(), String>;
pub fn apply_progress(&mut self, progress: &CopyProgress);
pub fn apply_outcome(&mut self, outcome: WorkerOutcome);
```
