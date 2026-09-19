# Task 4 Unix production increment — implementation brief

2026-09-18. Approved bounded implementation contract; Unix source implementation accepted after independent review and one typed-error compatibility fix. Scoped caller/native tests and the coordinated exact-checkpoint shell integration passed; cross-host qualification remains pending. This design is not native cross-platform durability qualification.

## Selected change

On Unix, route both existing `write_creating_parent*` wrappers through a new held-directory provisioner. It **opens/creates the complete chain first, then fsyncs every retained directory from leaf to `/`**. Direct Unix `mkdirat` needs no staged-directory rename, so there is no reason to import the Windows candidate's empty-component-before-descendants order. This deliberately matches app-state's existing full-tree/leaf-to-root obligation.

After successful provisioning the wrapper publishes the file relative to the returned held leaf; it must not resolve the ambient parent again. `settings-store::write_atomic` already calls the first wrapper, so its actual production save path benefits without adding a parallel storage service or changing the settings schema/location.

Keep app-state source and its existing regression unchanged in this increment. Add real provisioner tests for those same obligations, including loss of the prior error/handles. This is usable Unix progress; it is not Windows first-launch completion or the later Task 5 caller migration.

## APIs and state, intentionally small

Add these exports only under `cfg(unix)`; do not create a Windows Unsupported stub:

```rust
pub enum DirectoryState {
    Unchanged,
    CreatedDurabilityUnknown,
    NamespaceUnknown,
}

pub struct ProvisionError { /* native source + state + owned attempt */ }

pub fn provision_directory(path: &Path, privacy: Privacy)
    -> Result<StorageDirectory, ProvisionError>;

impl ProvisionError {
    pub fn state(&self) -> DirectoryState;
    pub fn io_error(&self) -> &std::io::Error;
    pub fn into_io_error(self) -> std::io::Error;
    pub fn retry_commit(self) -> Result<StorageDirectory, Self>;
}

pub fn directory_state(error: &std::io::Error) -> Option<DirectoryState>;
```

Derive Copy/Debug/Eq for the state. Implement Error/Display/source and original-kind I/O wrapping as for `PublicationError`. A parent-preparation error stays a typed `ProvisionError`; do not disguise it as file NotPublished or return a PublicationError with no directory state.

Internally store an explicit sum type, not a Boolean that can accidentally certify a prefix:

```text
Attempt::Partial {
    requested absolute path, resolved existing-prefix binding,
    held directory prefix, any just-created but not-yet-openable child name,
    privacy, created_any
}

Attempt::ReadyToCommit {
    requested absolute path, resolved existing-prefix binding,
    complete held chain root -> requested leaf,
    per-edge names/identities, per-node creation/privacy obligations
}
```

`ReadyToCommit` is constructed only after the **requested leaf** is opened, every required privacy check succeeds, and the entire chain/ambient binding is validated. The full-chain commit returns its actual last handle. Never return `last_opened` from a Partial attempt as the answer to the full request.

- `retry_commit` on Partial returns the same error unchanged. It does not create more components or claim that flushing a prefix completed the request. The caller restarts `provision_directory` to resume construction.
- `retry_commit` on ReadyToCommit revalidates its bindings/privacy, repeats the **whole leaf-to-root barrier sequence**, and validates bindings again. Only then may it return the requested StorageDirectory. Repeating the whole sequence avoids a fragile per-edge completed cursor.
- Any retry failure returns the owned attempt with the latest source error. No destructor deletes directories or restores permissions. A partial mkdir failure can leave real directory side effects; retain their handles/names where available.
- `Unchanged` means this call is proved to have created no directory, **not** that existing directories are durable. An existing-chain fsync failure can be Unchanged with a ReadyToCommit guard. If a successful mkdir occurred and there is no unresolved submitted operation, use CreatedDurabilityUnknown until the entire operation succeeds.

Classify mkdir errors at the submission boundary, conservatively and explicitly:

- A failure **before submission** (path validation, privacy preflight, handle acquisition, or BeforeCreate injection) has no effect from that mkdir. Use Unchanged if no earlier mkdir succeeded; otherwise preserve CreatedDurabilityUnknown. Do not erase an already unresolved operation's NamespaceUnknown state.
- A successful mkdir is a known namespace effect. A later open, privacy, identity, or fsync failure preserves CreatedDurabilityUnknown and the owned evidence; it does not authorize deletion.
- EEXIST is a collision result, not permission to infer durability or identity. Open the competing entry without following links, validate the actual winner, and include it in the full barrier sequence. If it cannot be admitted, fail with the earlier effects preserved; no usable leaf escapes.
- Any other **submitted** mkdir failure defaults to NamespaceUnknown unless the implementation has an explicit, justified proof of no effect for that native result. In particular, arbitrary EIO, interruption, or an unclassified native failure must not become Unchanged merely because the syscall returned an error. NamespaceUnknown takes precedence over known earlier-prefix creation in the public state; keep both facts internally.

For NamespaceUnknown retain the held parent, submitted child name, previously held prefix, and any child identity actually obtained; never fabricate identity when opening/querying fails. The attempt remains Partial. `retry_commit` returns its error unchanged even if flushing the known prefix could succeed. It must not repeat mkdir, delete the submitted name, or return a directory as completed. A fresh `provision_directory` call performs normal no-follow admission/reconciliation of the actual namespace and complete-chain barriers; it must not delete/replace an existing object or interpret the earlier error as proof that the name is absent. If a name is now proven absent, ordinary fresh provisioning may create it under the admitted parent. This conservative contract is required now, rather than deferred until an implementer encounters an ambiguous native failure.

No `ProvisioningRoot`, generic evidence constructor, serialized handle/identity, global registry, or persistent obligation journal is needed in this increment. Every fresh call rebuilds and flushes the entire chain, so dropped errors and process restart cannot erase the obligation merely because names now exist.

## Path admission and retained authority

These APIs are trusted-host ambient admission, like current `StorageDirectory::open`; they do not replace granted-user-file confinement. The host owns the namespace and mount topology, and cooperating storage users maintain exclusion against hostile child-name/ACL changes. Do not advertise race-free hostile-path confinement.

1. Capture an absolute request once (`current_dir` plus a relative path where necessary). Empty provision-directory input is InvalidInput. Wrappers use `.` for a leaf filename whose parent is empty. Validate the file's final component before any parent creation.
2. Locate the longest existing **directory** prefix without making changes. Canonicalize that prefix using ordinary host path resolution, so established symlink aliases such as `/var -> private/var` and `/tmp -> private/tmp` are admitted. Only NotFound permits peeling another component; permission/query/loop/not-directory failures propagate. A dangling link is not an absent directory to replace.
3. The uncreated remainder must consist of normal components. Existing-prefix `..` traversal can be resolved by canonicalization, so ordinary `../existing/new` works. Reject an unresolved suffix containing `..` before mutation rather than inventing semantics for creating `missing/../other`. This is an explicit narrow validation change from unconstrained `create_dir_all`, and must be documented/tested; do not silently perform lexical `..` normalization across symlinks. Root-only requests and `.` resolving to an existing directory are valid.
4. Open `/` as an ordinary read-capable directory. Walk the canonical prefix **forward**, using `openat(parent, component, RDONLY|DIRECTORY|CLOEXEC|NOFOLLOW)` and retain every parent/child handle plus full FileIdentity. Do not use a canonical string itself as authority, and do not reconstruct ancestry by opening `..` from a child.
5. Reopen the original existing-prefix path read-only for a native identity comparison with its held counterpart. This ambient lookup is a check, never the handle used for creation. A mismatch fails before creating descendants. From this point, all creation/opening is relative to the retained chain.
6. For each remaining normal component, attempt no-follow directory open. On proven absence, `mkdirat` the single component with the selected mode, then open/validate it relative to the same parent. On AlreadyExists from mkdir, reopen and validate the actual winner; neither EEXIST nor successful open counts as committed. A new symlink in this suffix is rejected. Do not delete or replace a conflicting object.
7. Once the leaf is held, re-open each recorded edge relative to its held parent with no-follow directory flags and require complete identity equality. Check that the original requested path still resolves to the held leaf and that the original existing-prefix binding still matches. Repeat these checks after the complete barrier pass. A changed/missing edge fails; do not rebase onto a replacement or recreate it during retry.

Read-only comparison opens may follow aliases already admitted in the existing prefix. All actual mkdir and later file publication uses held handles. If a trusted owner changes an alias to a different object, binding validation detects it; if an alias still resolves to the same object, the rule agrees with app-state's existing handle-identity source policy.

### Mounts, firmlinks, and the structural root

Do not require all ancestors to have the same device ID. That would reject ordinary mounted home/data directories, and it would mistake filesystem presentation details for an authority boundary. Do not classify a macOS firmlink as an ordinary symlink merely because it crosses a volume or resolves differently through `..`.

Instead, the authoritative edge is `held_parent + component -> held_child identity`, acquired and checked by **forward native lookup**. Retain every crossed directory handle, including directories on each side of a mount/firmlink boundary; fsync the complete retained chain leaf-to-root. No cross-filesystem rename is performed. A changed edge is an error, even if bytes or path spelling look the same. Do not omit a node because it is a mount root, reports the same device as its parent, is read-only, or appears already old.

`/` is the process's structural namespace root; the algorithm has no ordinary parent-entry obligation above it. Still execute the root fsync, matching the existing consumer's sequence. The new operation does not create mounts, persist mount configuration, or promise that a differently configured namespace after reboot resolves the same path. Its host contract requires the admitted mount/firmlink topology to remain stable during the operation and be restored by the host. An inaccessible or unflushable ancestor is a real error, never a skipped barrier. A sandbox namespace root is the boundary of that host namespace, not proof that mounts outside it are durable.

This is the same Unix filesystem-barrier level of guarantee as the existing file engine and app-state sequence. Hardware power-loss behavior and each persistent filesystem/storage profile still require native qualification; fsync success on volatile storage does not turn it into persistent storage. Task 8's permission-profile allowlist must not be reused as a durability certificate.

Read-only local inspection confirmed actual `/var` and `/tmp` symlink aliases on this Mac. Their resolved paths and `/Users` were readable, but those metadata observations are **not** fsync, mount-persistence, or power-loss evidence.

## Directory privacy and modes

Use Task 8's accepted `StorageDirectory::validate_privacy`/native counterpart for held **directory** validation; it correctly separates directory policy from regular-file single-link rules. Wait for its final source rather than copying another owner/mode/ACL implementation.

| Requested policy/object | Required behavior |
| --- | --- |
| Inherit, newly created directory | `mkdirat(..., 0777)` with ordinary umask/native inheritance; no permission rewrite |
| OwnerOnly, newly created directory | private creation policy below; validate held directory before creating descendants or returning it |
| OwnerOnly, requested leaf already exists | run the held OwnerOnly directory validator; fail rather than chmod/ACL-repair |
| Existing non-leaf traversal ancestors | retain/open/identity-check/fsync them; do **not** require effective-user ownership or private mode |
| AlreadyExists winner in the formerly missing suffix | validate OwnerOnly when requested, then include it in the complete barrier pass |

OwnerOnly's promise is about every directory this call creates and the requested leaf. It is not a recursive permission assertion over `/`, `/private`, `/var`, a home directory, or every previously existing ancestor. That scope is stable/documented even though retries find more existing names. Trusted namespace/exclusion remains a separate premise. Retain privacy validation obligations for directories created/adopted in this attempt and revalidate them on owned retry; a fresh call re-admits its leaf and all newly created/adopted components.

For private creation use `mkdirat(..., 0700)`, respecting umask, with no later chmod-to-pass. Extremely restrictive owner bits can cause open/search to fail; return the real error. Before mkdir, establish that native inheritance cannot widen the requested privacy:

- On admitted Linux POSIX-DAC profiles, the requested mode bounds inherited effective access; reuse Task 8's **profile** helper before creating in that parent without imposing its parent-owner/mode check. The native ACL project documents mode-bounded default-ACL inheritance in [acl(5)](https://man7.org/linux/man-pages/man5/acl.5.html). Unknown permission profiles fail before private creation, not after exposing a new directory.
- On macOS, mode alone does not establish the existing extended-ACL contract. Reuse the existing security-only held-FD `macos_acl::validate(parent)` before mkdir. This already conservatively accepts effective-owner allows/benign denies and rejects other-principal or uninterpretable allows, including inheritable ones. It does **not** require the parent's owner UID or mode to be private. Validate the newly opened child's full directory privacy afterward. Maintain the trusted-host ACL-stability premise between check and creation. Do not create broadly, then strip an inherited ACL and call that private-from-creation.
- Other Unix permission profiles get the accepted Task 8 privacy policy; do not silently claim universal mode-only ACL enforcement. The Inherit route remains separate from requesting OwnerOnly.

This macOS preflight intentionally inherits the current conservative ACL policy, including possible rejection of a harmless non-inheritable foreign allow. Do not invent a new ACL evaluator in this increment. A later wider supported ACL profile requires its own explicit evidence/tests.

Legacy `write_creating_parent_with_mode` retains **file** mode semantics, including 0600 and 0644. Provision its directories with Inherit, as prior `create_dir_all` did; do not map arbitrary file modes to directory search bits. Consumers that expressly need private directories use `provision_directory(..., OwnerOnly)` and the held publication API in their scheduled migration. The ordinary wrapper also provisions with Inherit.

Directory privacy success is not root reachability proof. Durability comes from the complete ordered barrier pass; no validator, existing name, or permission profile short-circuits it.

## Commit order and wrapper integration

After complete construction/admission, call native `fsync` on the held leaf, then each retained parent in reverse order through `/`. Flush every node, including existing ancestors, on every fresh attempt and every owned retry. Do not call `create_dir_all`, select the first existing ancestor as a stopping point, or return success after only flushing the first missing component.

After post-commit binding checks, transfer the held leaf out of the chain. The Unix wrappers call its `publish`/`publish_with_mode` using the already-validated final file name and current publication mode/privacy behavior. Keep file publication errors unchanged. Parent-provision failure prevents file staging and file publication altogether; a later file-commit error can still leave visible file bytes according to PublicationError.

Use explicit `cfg(unix)` and `cfg(windows)` wrapper branches. Windows continues its present transitional `create_parent` followed by the accepted native file engine, with its current mode validation before effects. Keep the Windows limitation in public docs: file commit does not establish newly created ancestor durability. Do not add a permanent always-Unsupported first-launch branch, and do not remove the accepted diagnostic probe. Windows production bootstrap, native directory barriers, and crash qualification remain open Task 4 work.

## App-state compatibility and later seam

No `app-state/src/lib.rs` product or test edit is required now. Run `canonical_directory_sync_failure_is_retried_even_when_ancestors_exist` unchanged, along with the canonical source-replacement regression. Its complete-path-exists, exact physical leaf-to-root vector, repeated-open visit, and post-publication visible-bytes/error assertions remain intact.

The selected Unix algorithm removes the previous conceptual conflict with that test: all components exist before the first directory fsync, and the real barriers run in the same order. The new atomic-file tests must independently fail at the same early `profile` ancestor **during the actual first barrier pass**, then drop the error and replay; never add a successful hidden provisioning pass before injecting the fault.

For Task 5, keep construction and reconciliation as distinct internal paths: existing-only reconciliation takes the canonical held leaf identity, opens/retains the chain, refuses missing/replaced sources, and runs this same barrier engine. It never calls mkdir. The current gate and before/after canonical source checks remain necessary. Its fault observer belongs immediately before each real fsync so the existing test can observe precisely the same production obligation. A future post-file-publication directory failure still leaves bytes visible, returns an error, and does not advance persisted heads. No new public unused reconciliation API is needed for this increment.

In particular, `open_canonical_with` currently installs its store fault configuration **after setup and before its first chain commit**. Task 5 must not insert this increment's all-in-one `provision_directory` before that configuration and then emulate failure during a redundant sweep. Its smallest compatible seam is to preserve initial construction/setup and delegate the first real existing-only chain commit after configuration. If Task 5 instead changes setup ownership, its configuration must move ahead of that first real commit while retaining the exact assertions and single visit vector. Both routes still check the canonical directory and gate before/after the commit and preserve SourceChanged on missing/replaced sources. Having authority to write to an old held directory after its rename is useful for general publication, but it does not satisfy canonical app-state source acceptance.

## Owned files and implementation sequence

| File | Change |
| --- | --- |
| `atomic-file/src/provision.rs` (new) | public Unix API/errors, Partial/ReadyToCommit ownership, admission/ordered chain orchestration |
| `atomic-file/src/native/unix.rs` | crate-private held no-follow child-directory open, mkdirat, directory fsync, permission-profile/inheritance preflight reuse; no new file writer |
| `atomic-file/src/lib.rs` | Unix exports and both wrappers' held-parent delegation; precise Unix/Windows documentation |
| `atomic-file/src/directory.rs` | only necessary crate-private integration; do not alter Task 8 public privacy behavior |
| `atomic-file/src/test_support.rs` | scoped production provisioning checkpoints below |
| `atomic-file/tests/parent_provision.rs` (new) | real provisioning/wrapper/child-process regressions |
| `settings-store/Cargo.toml` and `settings-store/src/lib.rs` tests only | default-off atomic-file test-support dev dependency if needed; real public save fault regression |

Use pinned existing rustix/std APIs; no new native dependency is expected. No Windows native/probe edits, app-state edits, workspace formatting, lockfile regeneration, or identity/receipt schema change. The settings-store dev edge may enable a feature but must not enable faults in normal application builds.

Add a separate feature-gated `ProvisionCheckpoint::{BeforeCreate, Created, BeforeSync, Synced}` and owned event containing the physical path plus an optional native FileIdentity snapshot. Provide scoped observer and exact checkpoint/path fault hooks, restoring prior hooks on unwind and invoking observers before failure. Synced is observation-only; emit it only after a successful real fsync. Created is after real mkdir/open/privacy admission, not a fake creation callback. The ordinary file Checkpoint semantics stay unchanged. No raw handles, release fault environment variables, or directory-probe dependency.

### Required production-path tests

1. Two missing levels through `write_creating_parent`: observe complete chain and exact real leaf-to-root BeforeSync/Synced ordering before file StageCreated; read final bytes. Repeat with all directories already existing and require the same barrier vector.
2. First-attempt failure at the early `profile` ancestor after all descendants exist: wrapper fails, file staging is absent, every requested directory exists. Drop the error, retry with a new call, and prove that ancestor is flushed again. Add a child-process version that destroys all prior in-memory authority before retry; do not label process exit as power-loss evidence.
3. Complete-chain BeforeSync failure permits `retry_commit` to return the exact requested leaf after replay. Inject failure again on retry and retain all obligations. A BeforeCreate/Created failure with remaining components yields Partial: retry_commit must return Err without creating the remainder, publishing a file, or returning the prefix as the leaf. Cover the production mkdir-result classification with a narrowly scoped native-result fault seam: a submitted failure without no-effect proof yields NamespaceUnknown, retains prefix/parent/name evidence, and performs no retry mkdir or cleanup; pre-submission failure preserves the earlier known-effect state. A fresh invocation reconciles the actual namespace and can finish it. Synthetic result injection proves conservative error handling, not real EIO recovery or crash behavior.
4. Rename/replace an opened ancestor between admission and commit through the observer; require failure, no file publication into the replacement, and no repair/recreation on retry. Include alias retargeting and a symlink inserted into a missing suffix. Existing admitted symlink aliases, including the host's `/var` or `/tmp` path when present, must succeed via held physical traversal.
5. Private new components + existing private leaf succeed; existing public requested leaf fails without chmod or file effects. Root-owned/searchable traversal ancestors remain usable. Legacy explicit file modes remain exact while directory creation retains Inherit behavior.
6. macOS inherited other-principal allow on the immediate creation parent is rejected **before mkdir**; effective-owner allow and benign deny fixtures remain accepted. Linux default-ACL creation/privacy tests use the accepted Task 8 profile and verify the created child; unsupported privacy evidence cannot count as success.
7. A path crossing a real host mount/firmlink, when supplied by a native qualification fixture, must retain/check/flush each forward edge. Do not create privileged mounts or count absence of such a fixture as a passing mount test. The ordinary macOS alias test is not mount/firmlink qualification.
8. Settings-store public save/update on an absent parent reaches the shared checkpoints; a directory barrier fault propagates typed directory state and leaves settings payload uncreated. Fresh retry writes the existing pretty JSON + newline unchanged. This establishes a real production caller, not only a new unused facade.

Run scoped tests after red/green implementation: `cargo test -p atomic-file --test parent_provision --features test-support --locked --offline`, existing atomic-file native-publication/private-file tests affected by helper reuse, `cargo test -p settings-store --lib --locked --offline`, and the two named app-state canonical regressions unchanged. Check atomic-file tests on Windows for cfg compatibility; compile evidence is not native Windows execution. Parent coordinates the required shell test compilation rather than duplicating the current sole writer's build work.

## Acceptance boundary

This increment is complete when the Unix wrappers execute this production protocol, owned retry cannot certify a prefix, all fresh attempts replay the whole chain, actual directory/privacy/source-change failures propagate, and the real caller plus unchanged app-state regressions pass. Record Unix filesystem/hardware qualification separately. Task 8's validator supplies permission admission only. The full Task 4 and Tasks 5/7 Windows first-launch obligations remain incomplete until the native bootstrap/barrier gate is resolved; the accepted probe and its still-owed native runs remain part of that work.
