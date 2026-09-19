### Spec Compliance

- ✅ Spec compliant for the bounded Unix increment. The frozen 12-file diff covers the requested production consumer migration, shared existing-only reconciliation, owned leases, distinct existing-file commit, typed publication failures, and actual worker-frontier tests. Manifest and lock changes remove only the app-state libc edge and add the scoped test-support dev feature (`crates/system/app-state/Cargo.toml:24`, `Cargo.lock:606`). Full Task 5 is not complete.
- ✅ Canonical construction installs configuration before source admission, the first real chain commit, and actor I/O (`crates/system/app-state/src/lib.rs:581`). Existing-file acceptance compares owner generation, bytes and identity, commits the file/name and ancestors, then checks canonical source/gate before acknowledging heads (`crates/system/app-state/src/lib.rs:767`). This follows the approved ordering and cooperative writer-gate/trusted-namespace premise; it does not promise hostile concurrent child-mutation immunity.
- ✅ The non-Unix inventory matches the bounded exception: canonical gate refusal, legacy deny-sharing writer, ambient append/read-only existing-file fallback, ordinary ancestor sync, and the old room-claim implementation remain transitional. The diff does not claim these establish Windows durability (`crates/system/app-state/src/lib.rs:1321`, `crates/system/app-state/src/lib.rs:1373`, `crates/system/app-state/src/lib.rs:1454`).
- ⚠️ Cannot verify native Linux/Windows behavior, mount/firmlink profiles, storage interruption or power-loss durability from this diff or the supplied macOS runs. Windows evidence is compile-only. The controller must retain these qualification items and full Task 5 as open; the final shell integration check is separately controller-owned.

### Strengths

- Existing-only directory reconciliation shares the real retained-chain validation and leaf-to-root barrier engine; it does not call the creation path. Complete retry repeats the barriers, while partial admission cannot silently create or finish missing names (`crates/system/atomic-file/src/provision.rs:469`, `crates/system/atomic-file/src/provision.rs:357`, `crates/system/atomic-file/src/provision.rs:63`).
- Existing-file commit is a focused separate operation with retained file/parent custody, native error preservation, identity/privacy revalidation and data/name barriers. Its errors never invent publication phases or delete evidence (`crates/system/atomic-file/src/existing.rs:36`, `crates/system/atomic-file/src/existing.rs:98`).
- Canonical actor/document operations use retained directory authority; publication errors survive StoreError::Io wrapping. Normal save does not silently retry a final publication failure into success (`crates/system/app-state/src/lib.rs:133`, `crates/system/app-state/src/lib.rs:1191`, `crates/system/app-state/src/lib.rs:1400`).
- Room claims preserve the alias order, newline payload and recovered authority policy. Matching immutable winners execute existing-file and ancestor barriers, followed by identity/byte revalidation; uncertain publication is not treated as permission to delete or regenerate (`crates/system/app-state/src/lib.rs:1589`).
- Tests exercise the actual autosave worker, retained native commit retry, fresh-process claim recovery, and real lease/flock interoperability rather than synthetic acknowledgements (`crates/system/app-state/src/autosave.rs:584`, `crates/system/atomic-file/tests/native_publication.rs:1020`, `crates/system/atomic-file/tests/existing_commit.rs:242`).

### Issues

#### Critical

- None found within the bounded task.

#### Important

- None found within the bounded task.

#### Minor

- The successful app-state and Windows logs are not warning-free: they contain 24 vendored Automerge warnings, including the log macro's unused import (`vendor/automerge/src/lib.rs:238`), unused assignments (`vendor/automerge/src/columnar/column_range/value.rs:186`) and dead code (`vendor/automerge/src/columnar/column_range/generic.rs:71`). These locations are outside this task's diff; no new owned-source warnings appear in the inspected final logs. Track the vendor noise separately rather than changing unrelated source in this increment.

### Focused Checks and Evidence

- Read the frozen review diff once and compared it against the binding brief, approved app-state migration design and Task 5/global constraints. Did not reconstruct a diff from moving HEAD, mutate product/index/HEAD, or dispatch subagents.
- Concrete risk checked outside diff context: the new DirectoryReconcileError delegates retry to ProvisionError, whose body was absent from the diff. Inspected `crates/system/atomic-file/src/provision.rs:63`; only ReadyToCommit calls the shared commit engine, and Partial returns unchanged. No creation or cleanup is hidden in that delegation.
- Inspected existing final logs rather than rerunning suites: `/tmp/task5-final-app.log:384` reports 60 passed; `/tmp/task5-final-atomic.log:35`, `:46`, `:62`, `:100`, `:129` total 86 top-level passes, excluding the two subprocess subruns; `/tmp/task5-final-windows.log:316` records successful compile-only completion. No failures or ignored tests are reported. No additional test execution was needed for an unresolved in-scope code doubt.

### Assessment

**Task quality: Approved.**

The bounded Unix production migration satisfies the requested operation ordering, custody, error propagation and acknowledgement contracts with meaningful real-path regressions. Approval does not close the explicitly deferred Windows reconciliation or native storage qualification work.
