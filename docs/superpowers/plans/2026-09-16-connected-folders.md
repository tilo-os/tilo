# Connected Folders Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Keep connected maintain usable local copies on Windows and Linux, with verified two-way reconciliation, preserved conflicts, recoverable deletions and reliable disconnect.

**Architecture:** A device-local connection service compares two pinned endpoints against a durable verified baseline. A pure reconciler proposes operations; platform adapters revalidate mount and file identity before executing journaled changes. The setup, first-login and Files surfaces use the same service and never implement synchronization themselves.

**Tech Stack:** Rust, existing capability-based filesystem handles and migration primitives, bounded durable journals, platform-native filesystem/volume APIs, existing Imager UI and worker patterns.

**Spec:** `docs/superpowers/specs/2026-09-16-guided-migration-design.md`

## Global Constraints

- Connected folders are synchronized local copies, not symlinks or direct access to original files.
- They consume local storage and remain usable while the source is unavailable.
- Dual-boot synchronization occurs between sessions when the source can be accessed; no background agent is assumed to run in the inactive OS.
- Use a verified baseline to detect changes on both sides. Preserve both versions when changes conflict, including edit/delete conflicts.
- Never resolve conflicts solely by wall-clock timestamps.
- Normal deletions propagate into recoverable storage with a proposed 30-day retention period, disclosed in connection settings.
- If recovery space is insufficient, pause deletion propagation rather than discard recoverable data.
- Disconnecting stops synchronization and retains both the qOS copy and originals.
- An unavailable source or permission failure must never be interpreted as deletion.
- Folder migration grants no collaboration sharing or cloud-upload permission.

## Existing boundaries and sequencing

Execute after the reviewed one-time copy service and Desktop continuation integration. Desktop Task 4 may introduce continuation interfaces; read its final implementation before choosing those call sites. This plan does not authorize disk partitioning, mounting an unsafe volume writable, identity provisioning or enabling an unproved OS-install route.

The current `setup/migration.rs` owns pinned directories, verified byte streaming and no-clobber one-time copying. `migration_journal.rs` and `migration_store.rs` provide restart-safe receipts and locked local state. Reuse narrow filesystem primitives where their contracts fit; do not add overwrite/deletion authority to the one-time Copy API. Source deletion must remain impossible in that API.

`migration_durability.rs` currently refuses Windows durability and can use a Linux boot-bound fallback identity. Neither refusal nor a same-boot identity proves a persistent cross-boot connection. Implement the missing native guarantees or keep that exact configuration unavailable. The `runtime-fs` workspace service is not already a two-way folder synchronizer; do not mistake document replication for this service.

Create focused modules under `apps/imager/src/setup/connection/`: `model.rs`, `reconcile.rs`, `endpoint.rs`, `store.rs`, `transaction.rs`, `worker.rs`, and `mod.rs`; platform-specific endpoint files may be added when native APIs warrant them. A connection UI belongs in `setup/connection_view.rs`. Shared migration primitive extraction must preserve existing Copy tests and behavior.

## Task 1: Baseline model and pure reconciliation

**Files:** Create connection/mod.rs, model.rs and reconcile.rs; add setup/mod.rs declaration.

**Interfaces:** `ConnectionId`, `EndpointIdentity`, `RelativePath`, `EntryState`, `VerifiedBaseline`, `ObservedTree`, `ReconcilePlan`, `Conflict` and `ConnectionStatus`. `reconcile(&VerifiedBaseline, &ObservedTree, &ObservedTree) -> ReconcilePlan` performs no filesystem writes. A complete observation and an unavailable/incomplete observation are distinct types/states. File state includes verified content digest, kind and relevant metadata; timestamps alone never establish equality. Paths preserve native bytes/UTF-16 without lossy conversion.

- [ ] Write failing cases for initial comparison, source-only edit, destination-only edit, equal independent edits, divergent edits, deletion versus unchanged, edit/delete, file/directory replacement, unknown rename, incomplete subtree, case collision and unrepresentable names.
- [ ] Implement bounded typed state and deterministic reconciliation. Missing entries become deletion candidates only within a successfully enumerated, unchanged endpoint scope and against a verified baseline. Incomplete observations produce pending/error outcomes, never destructive proposals.
- [ ] Keep an absent baseline explicit. Connecting an existing copied folder produces a comparison preview; it does not assume both sides match or overwrite either side. Preserve both divergent versions and request resolution.
- [ ] Model Up to date only after a complete successful comparison and required operations are durably verified. Keep Source unavailable and Changes pending distinct, with useful per-path reasons.
- [ ] Run focused tests and commit only these modules/declaration. Reviewer can reject reconciliation independently of native execution.

## Task 2: Native endpoint identity and safe access

**Files:** Create connection/endpoint.rs and platform modules as required; narrowly reuse migration/nofollow/destination_security/durability APIs where valid. Add native fixture tests.

**Interfaces:** `probe_endpoint(&Path) -> Result<EndpointProbe, EndpointError>` is read-only; `open_endpoint(&EndpointIdentity, Access) -> Result<PinnedEndpoint, EndpointError>` validates current identity and access. `observe(&PinnedEndpoint, &AtomicBool) -> ObservedTree` computes stable hashes with bounded memory and explicit per-path failures. `EndpointProbe` reports safe-read, safe-write and durable-transaction capabilities separately.

- [ ] Write failing fixtures for directory replacement, symlink/reparse traversal, mount substitution, offline endpoint, locked/encrypted volume, read-only mount, hibernated/unsafe source, placeholder files, permission denial and concurrent writes.
- [ ] Implement Windows and Linux probes using native evidence, not filesystem names or platform identity alone. Resolve stable volume/root identities across boots. A path or drive letter alone is not an identity; duplicate/cloned identity ambiguity requires re-selection.
- [ ] Pin roots and descendant handles without following substituted links. Bound enumeration and hashing work; report unstable files and unsupported metadata. Do not execute files, hydrate cloud placeholders silently, unlock encryption or clear hibernation state.
- [ ] Verify actual read/write semantics in private disposable files only after the user selected connection; keep initial capability discovery read-only. Establish supported atomic replacement, recoverable rename and persistence behavior on each claimed filesystem. Read-only Copy remains separately available when proven safe.
- [ ] Recheck mount state and identity for every synchronization session and before mutations, with retained handles and explicit invalidation. Handle unmount/media removal as unavailable, never deletion.
- [ ] Run native Linux and Windows tests against disposable volumes and record the exact support matrix. Cross-compilation alone cannot enable Windows connections. Commit the adapter and evidence.

## Task 3: Durable connection store, conflicts and recovery retention

**Files:** Create connection/store.rs; add bounded persistence tests. Reuse or extract reviewed journal framing only if it reduces duplication without changing Copy semantics.

**Interfaces:** `ConnectionStore` holds an exclusive service lock; `ConnectionRecord` binds both endpoints and policy; `OperationReceipt` records intent, preserved objects, publication and verification; `RecoveryEntry` records recoverable versions and retention eligibility. Store methods publish explicit durable acknowledgements. No user-editable record grants privileged filesystem authority.

- [ ] Write failing tests for torn final frames, corrupted prior frames, oversized input, stale sequence, conflicting writers, endpoint identity mismatch, restart during each receipt boundary and large connection inventories.
- [ ] Store state privately and locally outside collaborative documents. Bound frame size and memory; use immutable pages/checkpoints rather than one unbounded JSON snapshot. Keep credentials out of records and diagnostics.
- [ ] Persist operation intent before mutation and verified receipts before advancing baseline. A successful copy or rename without durable acknowledgement remains pending/recovery work on restart.
- [ ] Store conflicting and deleted versions with collision-proof identities, original paths and verified content. Do not overwrite a recovery entry or follow user-created replacement paths. Recovery storage must preserve actual bytes before destructive publication is allowed.
- [ ] Implement 30-day retention policy and explicit recovery/resolution state. A clock jump or uncertain elapsed time cannot justify early purge; preserve items when expiry cannot be established. Unresolved conflicts never expire as routine deletion history. Recovery-space exhaustion pauses destructive work.
- [ ] Implement disconnect as a durable fence that prevents new operations, waits for an in-flight safe boundary, and retains both endpoint copies and existing recovery data. Restart cannot reactivate a disconnected connection.
- [ ] Run focused persistence/crash-injection tests and native directory-entry durability checks; commit.

## Task 4: Journaled two-way transaction execution

**Files:** Create connection/transaction.rs and worker.rs; extend connection/mod.rs exports. Keep copy_folder one-way API unchanged.

**Interfaces:** `start_sync(ConnectionId) -> Result<ConnectionWorker, ConnectionError>` publishes bounded progress and terminal status. A worker consumes a fresh plan plus pinned endpoints and locked store; it never trusts an old plan's hashes as current authority. Conflict resolution consumes explicit selected version and expected conflict revision.

- [ ] Write RED integration tests using real temporary trees: propagate edits in both directions, matching edits, edit/edit and edit/delete conflicts, deletes with recovery, restore, interrupted publication, permission loss, source disappearance and disconnect during work.
- [ ] Verify baseline preconditions again before every operation. Stream into private exclusive temporaries, verify bytes/metadata and durable publication. Preserve the displaced object before replacement/deletion; record its actual identity and content, not merely the planned version.
- [ ] Handle concurrent editors conservatively: validate moved/replaced objects and keep unexpected versions as conflicts. Never delete displaced bytes merely because a pre-operation hash matched. Platform operations must not overwrite a raced-in object without preserving it. If a platform cannot supply the required guarantee, refuse that operation with an actionable pending outcome.
- [ ] Propagate normal deletion by recoverable relocation, never immediate unlink of the only surviving version. Persist recovery and transaction receipts before updating baseline. Recover interrupted intent from actual endpoint/recovery identities; do not retry based solely on status labels.
- [ ] Drain/finish begun atomic operations when UI disappears; cancellation pauses at a durable boundary. Coalesce progress and bound UI queues. Leave in-flight outcome discoverable by a reopened app.
- [ ] Update a baseline only after both expected sides and recovery receipts are verified. Run a final comparison before Up to date. Errors remain visible even if other files succeeded.
- [ ] Test large trees, long native names, disk-full injection, concurrent writers and repeated process restarts on native Linux and Windows supported filesystems. Explicitly distinguish process-interruption tests from power-loss evidence. Commit with exact evidence.

## Task 5: Setup, first-login, Files and session scheduling

**Files:** Create setup/connection_view.rs; modify setup/view.rs, import_view.rs and final Desktop continuation integration. Modify Files/settings and packaging/session integration only at existing supported entry points identified from their current implementations; record concrete touched files in the task brief before dispatch.

**Interfaces:** UI consumes ConnectionStore snapshots and ConnectionWorker events; scheduling invokes the same start_sync entry point. No independent synchronization code in views, Files or boot scripts.

- [ ] Add controller tests for capability-gated Keep connected, per-folder overrides, comparison preview when converting Copy, conflict choices, source unavailable, pause/retry, recovery browser and disconnect. Preserve pending setup choices across restart without auto-authorizing changed endpoints.
- [ ] Show local storage cost and explain when changes carry over. Keep Copy recommended for uncertainty. Show the actual source/destination and 30-day deletion policy before connection approval.
- [ ] Integrate first-login connections only after trusted identity completion and source/target revalidation. Never assume a service runs in the inactive OS. Start a bounded session scan when the active OS can safely access both endpoints; expose manual Sync now and pause controls.
- [ ] Ship the real service/UI in supported packages and install the reviewed user-session scheduling integration. Withhold write scheduling for unsafe/offline mounts; watchers are hints to rescan, not proof of change or deletion. Avoid concurrent service instances using the durable store lock.
- [ ] Add Files Add folders and connection settings using the shared service. Disconnect keeps both copies. Conflict resolution previews both versions and checks revision before applying an explicit choice; restoring a deleted file preserves any current collision.
- [ ] Inspect compact and normal screenshots, keyboard navigation, accessible names, progress/error recovery and explanatory copy. Test actual setup-to-first-login continuation, later connection conversion and reopen-after-disconnect.
- [ ] Prove end-to-end native scenarios on Windows and Linux, including both sides edited, source unavailable, hibernation refusal where applicable, recovery after deletion and dual-boot between-session sync. Confirm originals/alternate versions remain recoverable. Enable only combinations with completed evidence and update regression docs.

## Completion boundary

Pure-model tests, cross-compilation and supported-looking filesystem labels do not complete this plan. Connected must work through the actual shipped UI/service on both requested operating systems, preserve conflicts and recoverable deletions across restart, and refuse unsafe mounts without pretending they are empty folders. Full OS internal boot and signed-release availability retain their separate delivery gates.
