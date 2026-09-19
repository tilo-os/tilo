# Task 4 provisioning contract refinement

2026-09-18. Design recommendation for parent adjudication, not an implementation or native qualification report. No product source edits, builds, commits, or Task 3 review were performed.

## Decisions to carry into the executable plan

1. `provision_directory(path, Privacy)` cannot, by its arguments alone, establish a durable stopping point. Keep it only as a compatibility facade over a framework-owned anchor resolver and a held, anchor-relative provisioning operation. Do not implement “first existing ancestor is the anchor.”
2. Every fresh attempt must reconcile **every edge below the established anchor**, including already-existing directories. This is the restart mechanism. It avoids needing a persistent obligation journal, and survives callers dropping an error or terminating between creation and barrier completion.
3. In-process failures own the opened child, its parent commit authority, component identity, and remaining barrier obligations. Retry repeats barriers against those objects; it does not repeat creation or rename. An error destructor never removes possibly published directories.
4. Ordinary `StorageDirectory` admission is not anchor evidence. Neither an opened handle, native identity, local NTFS admission, Known Folder lookup, an old-looking creation time, nor a marker inside the proposed root proves durable ancestry.
5. Production Windows anchor establishment remains an explicit unsolved native/bootstrap requirement. Moving it into a new type or asking an application to supply a “trusted” directory does not solve it. No inspected source or evidence supplies the initial production proof today.
6. Advance a bounded Task 4a now: owned obligations, exhaustive replay, the Unix production path, and a real Windows probe with a separately established **test** anchor. Do not route successful Windows first launch through an unqualified directory protocol or mark first launch complete. Task 4b must supply the production anchor mechanism and qualified Windows directory barriers before the wrappers/consumers can close.

## Inspected evidence and scope

The implementation plan's global contracts and Task 4 require private component-wise creation, uncertain-creation retry, working parent-creating wrappers, and no permanent Unsupported substitute. Task 5 explicitly requires retaining the existing app-state retry regression. The directory evidence report separately identifies native operational/crash qualification and anchor admission as open requirements.

`app-state/src/lib.rs::sync_directory_chain` checks canonical source identity, canonicalizes the directory path, flushes **every physical ancestor through the root**, and checks canonical source identity again. Its caller does not remember only directories created on the latest attempt:

- `open_canonical_with` first performs `create_dir_all`, opens the canonical directory, acquires the gate, and loads/creates the actor; it then calls `sync_directory_chain`.
- `write_full` publishes document bytes and then calls `sync_directory_chain`. Thus directory failure after bytes become visible still fails the save.
- `accept_canonical` flushes the existing document and calls `sync_directory_chain` before advancing persisted heads when no repair is needed.

`canonical_directory_sync_failure_is_retried_even_when_ancestors_exist` fails the first open at the `profile` ancestor, asserts the complete descendant path already exists, drops the failed attempt, and requires a new open to visit exactly the complete leaf-to-root chain. It separately injects directory failure after document publication and verifies that bytes are visible despite the error. This is stronger than testing `ProvisionError::retry_commit` alone: the error and its handles need not survive.

Current `StorageDirectory::{open,from_file,identity}` supply opened directory authority and native identity, not a durable-ancestry certificate. The Windows `open` access mask shown by the current API requests list/read-attributes/read-control/synchronize rights; it is not evidence that it holds the candidate directory commit rights. `from_file` must not silently obtain stronger authority by resolving a display path. These are input constraints for Task 4, not findings about Task 3 correctness.

The current parent-creating wrappers explicitly document transitional creation without durable ancestor reachability. That limitation must stay visible until Task 4 actually closes it.

## Why a path-only implementation is insufficient

Consider `anchor/a/b`: the first call creates `a`, reports a commit failure, and is dropped. The second call sees `a` present and creates `b`. Treating `a` as durable because it exists loses the failed first call's obligation. The same failure occurs after process restart. A marker in `a` merely moves the circular proof: the marker can be observed while `a`'s parent entry is still uncertain.

An algorithm can conservatively repeat barriers to a structural filesystem root, as app-state does on Unix. The proposed Windows child-plus-parent flush protocol may require writable directory handles. Recursing to `C:\` cannot assume a standard user has those rights; acquiring privileges, modifying ancestor ACLs, or requesting a writable volume handle is outside the authorized storage boundary. Starting at the nearest writable directory also does not prove that directory's link is durable.

Thus the original function can remain useful syntactic sugar, but an unconditional promise for arbitrary paths is unsupported without additional native evidence or an explicit host-established anchor contract. Changing a signature does not create that evidence.

## Proposed implementable API division

The names below are recommendations, not an assertion that they are already fixed APIs:

```rust
// No public fields, raw-handle escape, Serialize, or "assume durable" constructor.
pub struct ProvisioningRoot { /* held anchor and commit authority; evidence */ }
pub struct ProvisionedDirectory { /* held leaf; admitted chain/replay scope */ }

impl ProvisioningRoot {
    pub fn provision(
        &self,
        relative: &Path,
        privacy: Privacy,
    ) -> Result<ProvisionedDirectory, ProvisionError>;
}

impl ProvisionedDirectory {
    pub fn directory(&self) -> &StorageDirectory;
    pub fn reconcile_reachability(&mut self) -> Result<(), ProvisionError>;
}

impl ProvisionError {
    pub fn state(&self) -> DirectoryState;
    pub fn io_error(&self) -> &std::io::Error;
    pub fn into_io_error(self) -> std::io::Error;
    pub fn retry_commit(self) -> Result<ProvisionedDirectory, Self>;
}

// Internal native/framework-host operation. Its Windows implementation is a gate,
// not a caller-provided assertion or a completed contract today.
fn establish_provisioning_root(path: &Path)
    -> Result<(ProvisioningRoot, /* relative remainder */ PathBuf), ProvisionError>;

// Compatibility surface; cannot bypass establish_provisioning_root.
pub fn provision_directory(path: &Path, privacy: Privacy)
    -> Result<StorageDirectory, ProvisionError>;
```

The host admission operation owns ambient resolution and the storage-location policy. Applications continue to pass their existing SDK/location requests. They do not supply principal strings, native handles, native OS logic, or attestations of durable ancestry. This is not a second grant registry: `ProvisioningRoot` is retained state for already-authorized framework storage, not a mechanism for granting new access.

The core operation accepts validated relative components only, rejects traversal/reparse/symlink substitution according to the admitted profile, and operates relative to held parents. Root and intermediate replacement are errors, not permission to rebase onto newly resolved objects. Initial capabilities must be acquired through authorized host admission; duplicating a handle does not upgrade its rights. Where separate authority/commit handles are required, admission must acquire both and compare full native identity before effects.

`ProvisionedDirectory` preserves the replay scope for services such as DocStore that need later reachability reconciliation. Returning only `StorageDirectory` is acceptable at the legacy facade boundary **only because** any subsequent path-only call reconstructs and replays the entire scope. It is not an implicit conversion from arbitrary `StorageDirectory` to a durable anchor.

The service retains its existing exclusion/lease while operating. Provisioning does not fabricate a lease or protect a child namespace from hostile mutation. Concurrent cooperative creation may produce AlreadyExists, but the winner must still be admitted and committed; it is never automatically a durable winner.

## Obligation and retry state machine

An obligation is for an edge, not merely a directory path:

```text
held parent identity + validated child component + held child identity
    + outstanding child barrier / parent barrier
    + namespace publication phase
```

For a fresh attempt, enumerate the anchor-relative chain. Each existing descendant is initially unproven for this attempt. Validate/open it relative to its held parent and schedule the required barriers just as for a previously failed creation. The absence of an in-memory error or an obligation file changes nothing.

For new directories, set requested privacy at exclusive creation, validate before using them as parents, and retain their handles. Do not recursively chmod or rewrite ACLs on unrelated existing ancestors. Explicitly define the directory privacy policy in Task 4: recommended semantics are private creation of all new components, validation of existing application-owned descendants when OwnerOnly is requested, and separate anchor admission. Existing unacceptable privacy fails; it does not trigger implicit permission repair. The low-level native definition of acceptable directory ACLs remains subject to qualification.

With the current Windows candidate, finish an empty component's publication and child/parent barriers before creating descendants. An early failure therefore legitimately leaves a prefix, not necessarily the complete requested hierarchy. Success means every required edge barrier has completed in the supported profile. Visibility may precede success; the operation is not a transaction over the chain.

On a known successful create/rename followed by commit failure, retain the child and the parent's commit handle. Retry revalidates the relevant identities under the same service exclusion and repeats the required barrier sequence. Repeating both child and parent barriers is conservative; a cursor may only skip a step whose successful completion remains valid under the qualified protocol. Retry must not recreate, rename, overwrite, or delete the published object. A later creation failure can retain earlier committed-prefix state without claiming the remaining hierarchy exists.

On ambiguous namespace submission, retain evidence and require reconciliation of relative final/stage names against the held child's complete identity. `retry_commit` alone must not blindly turn ambiguous submission into known publication. A conflicting object or inaccessible evidence stays an error. After restart, there is no serialized native identity/capability to trust: reopen and validate the actual chain from the independently established anchor, replay all barriers, and apply the service's ordinary conflict policy. An abandoned stage need not be deleted to make progress; destructive orphan cleanup is outside this bounded task.

The current proposed `DirectoryState::{Unchanged, CreatedDurabilityUnknown}` needs refinement for ambiguous directory rename/create. Recommend adding `NamespaceUnknown`, or choosing direct final-name exclusive creation only after evidence permits a smaller state machine. Never label ambiguous effects `Unchanged`. `Unchanged` describes effects of the current attempt; it does **not** say that pre-existing descendants are durable. A failure while recommitting old descendants can be Unchanged and still carry mandatory obligations.

Keep native source errors and directory state downcastable through `io::Error`. A parent-preparation failure must not be represented solely as file `NotPublished`: the file may be untouched while new directories are visible. No deleting destructor is allowed after final-name publication or ambiguous submission. Closing the error loses convenient in-process retry handles, but exhaustive replay makes the next admitted attempt safe.

## Initial anchor: what can actually establish it

Only the framework/native owner may mint a `ProvisioningRoot`; its evidence must name the boundary whose own reachability is outside the replay range. There are two legitimate proof shapes, neither supplied merely by a path:

| Proof shape | Concrete establishment | Restart rule |
| --- | --- | --- |
| Reconcile from a structural filesystem root | Admit the filesystem profile and root, then execute the qualified edge barriers on the entire path below that root. The root itself has no ordinary parent directory entry to commit. | Repeat admission and edge barriers. Do not trust a remembered pathname. |
| Independently committed host bootstrap root | A framework/OS bootstrap producer establishes that root's ancestry using a qualified protocol, and its lifecycle contract guarantees that the admitted root is the established object. A trusted native lookup binds runtime authority to that root. | Repeat trusted lookup and the bootstrap contract's identity/lifecycle admission. A local marker alone is insufficient. |

The first shape is a concrete Unix implementation direction: retained traversal and directory barriers through the admitted filesystem root, preserving the current full-chain obligation. Mount/symlink boundaries need explicit admission policy; do not silently claim an arbitrary user-file namespace is the same supported framework profile.

**There is no currently established production Windows implementation of either shape in the inspected material.** The existing proposed directory protocol requires testing its access masks, flush support, retry, and namespace coverage. A standard user's profile or LocalAppData path is a plausible placement for a bootstrap boundary, but native Known Folder lookup establishes location, not the necessary crash-durability/lifecycle fact. This report does not claim Windows creates it with a qualified barrier on the application's behalf.

To close Windows, Task 4b must establish one of these exact propositions:

1. A supported unprivileged native barrier can establish the required existing-ancestor reachability while using authority available on the path through a structural root. Probe minimal-rights handles separately; do not infer sufficiency from one successful syscall.
2. A specifically identified OS/framework bootstrap producer supplies an independently durable per-user root, with an authoritative lifecycle contract and a native admission route. The application/framework may then replay all descendants using commit authority only at and below that boundary. This must support ordinary first launch without requiring applications or users to pre-create the directory or obtain administrator privileges.

If neither proposition is established, full Windows first launch remains unresolved. We cannot implement a verified constructor now merely by naming it `establish_provisioning_root`. A qualified native protocol might lead to a different bootstrap design; that is a protocol decision, not evidence already available.

An externally established **probe** root is concrete and usable now: an external disposable-VM controller prepares the test parent, abruptly restarts the guest/storage fixture, then independently confirms the expected root identity and private namespace before the experiment. Record how the fixture discarded volatile writes. The controller retains the expected identity/evidence outside the filesystem under test. This makes the tested anchor an explicit fixture premise; it does not implement production host bootstrap or make process termination into a power-loss experiment.

## Wrappers and app-state integration

`write_creating_parent*` should resolve/admit the framework root, replay/provision the file's parent relative to it, and only then invoke file publication through the returned held directory. Do not reopen the final ambient path between provisioning and publication. Preserve exact Unix file modes independently from the directory privacy policy; reject unsupported mode mappings before effects.

The path-only wrappers must perform fresh exhaustive replay every time, including after a previous `io::Error` was dropped and after restart. A process-global cache is at most an optimization; it is not persistent proof. No new on-disk journal is required with exhaustive replay, and a journal within an uncertain directory would not fix initial-anchor circularity anyway.

For production Windows these wrappers are waiting on the two propositions above and the selected native directory protocol. Keep the current documented transitional limitation until that implementation is supplied. Do not strengthen the docstring, report the task complete, or add a permanent Unsupported route in lieu of it. An out-of-profile or unauthorized path should ultimately produce a precise admission error before effects, but profile rejection does not excuse failure to supply the required ordinary first-launch profile.

DocStore should retain the held provisioning scope and pending obligations alongside its canonical ownership state, while continuing to own its lease and source-change policy. Canonical open/reopen and accept must reconcile before accepting durable heads. Full save must preserve the distinction between bytes visible and durability acknowledged, including failure of any required post-publication reachability barrier. Do not advance `persisted` on that failure. Keep the existing gate/source identity checks; the new directory type does not prove canonical ownership.

### Exact existing-test conflict that requires adjudication

The named existing regression encodes both the essential obligations and details of the old algorithm:

- it requires all descendants to exist after failure at an early ancestor;
- it requires the literal complete leaf-to-root visit vector;
- it requires failure after document publication while retaining the visible bytes.

The proposed empty-component commit-before-descendants algorithm intentionally stops at an early prefix. A Windows bounded bootstrap anchor intentionally avoids writable barriers above its boundary. Those designs cannot mechanically preserve both of the first two assertions for the same injected creation failure. Calling an identity-only visit a “sync,” or injecting the fault only after a successful independent provisioning pass, would make that old test pass while removing its evidence about uncertain creation. Do not do either.

Given the repository covenant, do not delete, ignore, gate away, or weaken this regression in Task 4a. Keep the current app-state implementation/regression intact while building focused new provisioning tests. The parent must resolve this migration contract before Task 5: either retain the exact existing Unix full-chain behavior where required and add the new Windows semantics as equally explicit production coverage, or obtain a deliberate specification/test-contract change for the early-prefix behavior and qualified anchor boundary. This is an explicit unresolved sequencing issue, not permission for the worker to silently edit assertions. The essential dropped-error retry and post-publication failure guarantees remain mandatory under either choice.

## Next bounded implementation/probe and sequencing

**Task 4a, executable without claiming Windows qualification:**

1. Finalize the owned error/replay types and production test checkpoints. Implement the Unix retained-handle primitive and exhaustive replay to an admitted structural root. Scope changes to atomic-file; leave app-state and the Windows successful first-launch route unchanged until their gates are met. Keep obligations and native handles out of serialized state.
2. Add production-path tests for two missing levels, private initial creation, partial prefix, dropped-error replay with all relevant names already present, repeated barrier failure, full identity replacement, ambiguity/conflict, and no file publication after parent error. Include a child-process restart test that discards all in-memory guards and then replays the same chain. Report this as process-recovery evidence only.
3. Implement the standalone Windows native probe already specified in the directory evidence report. Make `--root` and profile/evidence input mandatory; record requested rights, NTSTATUS/Win32 errors, full identities, DACLs, device/volume facts, and every checkpoint. Never report missing observation as a pass. Cross-compilation is useful but is compile evidence only.
4. Extend its matrix specifically for this gap: a controller-established writable per-user-like test root; existing descendant edges from a prior failed attempt; reopened-process replay; denied writable ancestors above the test boundary; no-follow/collision behavior; and minimal-access versus writable parent commit handles. Log whether a candidate needs writable ancestors that a standard user lacks. Do not enable backup/restore/manage-volume privileges or change ancestor ACLs.
5. Make the process emit externally observed checkpoints after create/rename, child barrier, parent barrier, and acknowledged success. The external native harness supplies abrupt guest/storage loss and verifies reachability/identity/privacy after restart. Implementing the checkpoints is bounded local work; executing the native and crash matrix remains explicitly owed.

**Task 4b, required for completion:** run the standard-user native matrix on the claimed Windows profiles, establish the initial production anchor proposition, choose the smallest justified directory protocol, implement it behind the shared facade, and run wrapper first-launch/retry tests plus crash qualification. Seek directory-specific authoritative clarification if the composition argument still does not establish namespace coverage. Native operation success and a finite set of crash samples have different evidentiary limits; record both.

**Dependent work:** Tasks 5 and 7 remain gated on Task 4b and the app-state test-contract decision. A bounded Task 6 existing-parent migration or Task 8 receipt migration may advance independently if its own admission and compensation requirements are satisfied; they do not close first launch. Task 9 records native evidence and cannot be used to relabel missing anchor/provisioning implementation as merely a test debt.

## Unresolved points, explicitly

- Exact standard-user Windows operation or bootstrap contract establishing the initial production anchor, including restart and first-ever user/profile launch.
- Operational support/access for the candidate directory options and flush, and authoritative/qualified namespace coverage of child-plus-parent barriers.
- Native retry behavior after real I/O failure; injected errors alone do not establish this.
- Supported filesystem/mount/reparse policy and directory-specific OwnerOnly admission details.
- Whether the error enum gains `NamespaceUnknown`, and how explicit ambiguity reconciliation is exposed without weakening `retry_commit`.
- Adjudication of the existing app-state test's exact full-chain and complete-descendant assertions before its production implementation migrates.

No native Windows execution or power-loss evidence was generated for this report. The primary references and exact native request matrix remain in `docs/superpowers/plans/2026-09-18-cross-platform-framework/windows-directory-publication.md`; this refinement relies on their stated limits and does not upgrade them into guarantees.
