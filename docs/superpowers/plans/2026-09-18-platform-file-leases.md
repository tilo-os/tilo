# Framework file leases implementation plan

> **For agentic workers:** Use subagent-driven-development for implementation and independent task review.

**Goal:** Provide native process-lifetime locks in the shared framework storage layer and remove the catalogue/moderation non-Unix synchronization downgrade (audit A08-16).

**Architecture:** Extend the std-only `atomic-file` owner with a private-handle lease type over stable Rust file locking. Domain services retain their sentinel paths, transaction logic and error vocabulary; framework owns lock acquisition/lifetime. This is the first S02 slice, not complete storage or Windows persistence certification.

**Tech stack:** Rust 1.97.1, edition 2024, std::fs file locks (stable since 1.89), existing tempfile tests.

**Spec:** `docs/superpowers/plans/2026-09-18-cross-platform-framework/framework-contract.md`; audit A08-16 and A09-02.

## Global constraints

- Preserve all pre-existing checkout edits; do not stage or commit unrelated work.
- Do not regenerate Cargo.lock, add dependencies, set CARGO_TARGET_DIR or share target directories.
- Lock acquisition failure must not be reported as success or silently replaced by a process-local mutex.
- Keep already-open file ownership and never reopen a security-bearing handle by path inside the lease API.
- Lock files are persistent dedicated sentinels; do not delete/recreate them on release, truncate them, or lock the data file that atomic replacement replaces.
- Preserve Linux/macOS compatibility with existing flock callers and persistent formats.
- Run native tests where available; unavailable Windows/Linux execution remains outstanding, not passed.
- No raw native descriptors, public unlock method, File clone or mutable native evidence constructor.

### Task 1: Owned shared and exclusive leases

**Files:** create `crates/system/atomic-file/src/lease.rs`, export it from `src/lib.rs`, create `crates/system/atomic-file/tests/file_lease.rs`. No manifest/dependency changes.

**Interfaces:**

```rust
pub enum LeaseMode { Shared, Exclusive }
pub enum AcquireError { Busy, Io(std::io::Error) }
pub struct FileLease { file: std::fs::File }
impl FileLease {
    pub fn acquire(file: std::fs::File, mode: LeaseMode) -> std::io::Result<Self>;
    pub fn try_acquire(file: std::fs::File, mode: LeaseMode) -> Result<Self, AcquireError>;
    pub fn metadata(&self) -> std::io::Result<std::fs::Metadata>;
}
```

`LeaseMode` is Copy/Debug/Eq; errors implement Debug/Display/Error with underlying
I/O source preserved. `FileLease` is not Clone. Use `File::lock/lock_shared` and
`File::try_lock/try_lock_shared`; map std `TryLockError::WouldBlock` to Busy and
`Error(io)` to Io. The constructor consumes the File; after successful native
acquisition it returns the owning guard. Metadata reads from that handle. Drop
closes the private file and releases the native lock. No explicit unlock is
necessary. Document dedicated sentinel use, native/network-filesystem limitations,
and that callers must not retain duplicated handles to the consumed open file.
This API promises locking, not private creation, identity attestation or durable
directory publication.

- [ ] **Step 1: Write tests against production FileLease before implementation.**

```rust
#[test]
fn exclusive_lease_excludes_a_second_open_until_drop() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lease");
    let open = || std::fs::OpenOptions::new().read(true).write(true)
        .create(true).truncate(false).open(&path).unwrap();
    let first = FileLease::try_acquire(open(), LeaseMode::Exclusive).unwrap();
    assert!(matches!(FileLease::try_acquire(open(), LeaseMode::Exclusive),
        Err(AcquireError::Busy)));
    drop(first);
    let _second = FileLease::try_acquire(open(), LeaseMode::Exclusive).unwrap();
}
```

Also test two shared leases coexist, shared rejects exclusive and vice versa,
metadata uses held file, failure is distinguishable from contention, normal
release and abrupt child death release the lock. Use real child processes through
the integration test binary (test-only helper entry selected by exact test name
and dedicated environment marker). Parent/child coordinate readiness over pipes
or files with bounded deadlines, not an arbitrary sleep as proof of ownership.
Always kill/reap a test child on every failure path. The child must use the public
FileLease API, not a mock lock implementation. Assert the final acquired result.
Test blocking acquisition with another process releasing its held lock.

- [ ] **Step 2: Record red** with `cargo test -p atomic-file --test file_lease --locked --offline`; implement the module using only native std locks; run the same command to green.
- [ ] **Step 3: Run** `cargo test -p atomic-file --locked --offline` once after focused tests pass. Existing native macOS baseline is 12 passing library tests at start of goal; retain every assertion.
- [ ] **Step 4: Self-review and report** exact test outcomes and which hosts actually ran. Do not claim Windows persistence fixed by this lock API. Parent supplies task review; do not spawn reviewers or commit.

### Task 2: Catalogue checkpoint and moderation transactions use framework leases

**Files:** modify `crates/packaging/app-catalogue/src/client.rs` and `crates/packaging/registry-server/src/moderation.rs`; production-path process tests may live in their embedded test modules so private operations remain private. Remove the now-unused direct libc dependencies from both crate Cargo.toml files and only their libc dependency edges in Cargo.lock; preserve every other lockfile entry/version and concurrent edit, without regenerating it. Do not alter unrelated app-state identity/durability in this task.

**Consumes:** `atomic_file::lease::{FileLease, LeaseMode}` from Task 1.
**Produces:** existing public catalogue and moderation APIs with real cross-process
synchronization on every host supported by std native locking.

- [ ] **Step 1: Write regression tests for actual sentinel ownership.** Drive
`checkpoint_lock` and `ProcessGuard::acquire` using controlled helper processes,
and prove they contend on the exact persistent sentinel with a separately opened
framework lease. The guard must stay held across the corresponding production
read/validate/write transaction. Add a catalogue production checkpoint sequence
test with concurrent writers and assert monotonic rejection of older/same-sequence
forks; use explicit temporary roots rather than mutating global user locations.
Existing moderation transition tests continue to assert actual stored outcomes.
Test helpers must have bounded readiness, cleanup and nonzero child failures.

- [ ] **Step 2: Remove cfg-gated synchronization and native calls.**

```rust
// checkpoint_lock retains the existing sentinel open and error mapping:
FileLease::acquire(file, LeaseMode::Exclusive)
    .map_err(|error| RegistryError::Checkpoint(error.to_string()))
```

Return FileLease from `checkpoint_lock`, call it unconditionally before reading
the checkpoint, and retain it through publication. In moderation use one
ProcessGuard definition on all supported targets, owning a FileLease plus the
existing process mutex. Arrange field destruction so the native lease releases before
the process mutex, preserving the old explicit unlock-before-mutex-release order.
Preserve open and lock errors as ProcessLock, preserve
sentinel path and all transition policy, and remove the no-lock non-Unix branch
and explicit libc unlock. Remove native imports made obsolete by this change;
do not remove Cargo dependencies used elsewhere in a package.

- [ ] **Step 3: Run focused tests** with
`cargo test -p app-catalogue --lib client::tests --locked --offline` and
`cargo test -p registry-server --lib moderation --locked --offline`.
Compile affected tests using `cargo check -p app-catalogue -p registry-server --tests --locked --offline`.
- [ ] **Step 4: Review cross-host limitations honestly.** Required Windows native
contention/release evidence is separate from source portability. Windows durable
atomic publication and full storage privacy are later S01 work; do not mark
A08-16 fully certified until its native transaction acceptance evidence exists.
Record changed production paths and verification in the program register.

## Preflight relationships

| Task/pair | Interface or shared file | Resolution |
| --- | --- | --- |
| 1 | std-only leaf, owned File and error types | no dependency or lockfile changes needed |
| 2 | consumes FileLease blocking acquisition | sequential after Task 1 review |
| 1 / 2 | no native handle is exposed; both use persistent sentinels | compatible with atomic data replacement |
| 2 / S01 | atomic-file write may still lack Windows durable publication | lock fix progresses S02; required storage support remains open |
| 2 / future app-state migration | Windows deny-share writer protocol differs | do not change app-state in this slice |
