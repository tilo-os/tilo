# Task 4 Unix production provisioning increment

2026-09-18. Production source stable for independent review. This report covers the bounded Unix increment only. Full Task 4, Windows bootstrap, and the cross-platform goal remain open. Parent owns independent review and the shared `cargo check -p shell --tests` requirement.

## Snapshot and owned files

Compared against the dispatch snapshot `task-4-unix-before` / `task-4-unix-before-sha256.json`, never moving HEAD. Final owned-file hashes are in `task-4-unix-final-sha256.json`.

Modified:

- `crates/system/atomic-file/src/lib.rs`: Unix exports and both parent-creating wrappers delegate through held provisioning; Windows transitional branches/docs retained.
- `crates/system/atomic-file/src/native/mod.rs`: crate-private Unix component-validation reexport only.
- `crates/system/atomic-file/src/native/unix.rs`: no-follow directory open, mkdirat, held fsync, extraction/reuse of accepted permission-profile helper and existing macOS security-only ACL preflight. Existing file publication protocol is untouched.
- `crates/system/atomic-file/src/test_support.rs`: default-off Unix scoped provision checkpoint/path observers/failures and synthetic submitted mkdir-result seam.
- `crates/system/settings-store/Cargo.toml`: dev-only dependency on the existing atomic-file path with `test-support`; no normal dependency or version change.
- `crates/system/settings-store/src/lib.rs`: one Unix test module exercising public `update_section_at`; production serialization/save code unchanged.

Added:

- `crates/system/atomic-file/src/provision.rs`.
- `crates/system/atomic-file/tests/parent_provision.rs`.

Cargo.lock has **zero changes** relative to dispatch (SHA256 `5b8f36b9c5698482f5fbd072cc98fc68b957f2ca115ceec66c71061755fd3890`). No lock dependency edges changed; the sole manifest addition enables an already-present dev feature. Entire app-state source/tests/manifests, atomic-file manifest, directory/privacy public facade, publication module, native Windows code, legacy implementation, and diagnostic probe match dispatch hashes. Unrelated shared UI/history files were not edited. No staging, commits, stash, cargo fmt, target-directory overrides, or lock regeneration.

## Implemented production protocol and error authority

The Unix API captures one absolute requested path, locates/canonicalizes the longest existing directory prefix, rejects unresolved non-normal suffixes before mutation, opens `/`, and follows the canonical prefix forward with retained no-follow read-capable directory handles. It compares the ambient admitted prefix to the held identity before descendant creation. Dangling links/non-directory/permission failures do not become missing directories.

The complete missing suffix is created/admitted before any directory barrier. Inherit uses native `0777`/umask; OwnerOnly uses `0700`, shared profile/ACL preflight before mkdir, and the accepted held-directory privacy validation after open. Existing non-leaf traversal ancestors are never required to be private. Created/adopted suffix privacy obligations and the requested leaf are revalidated before/after commit and on owned retry. No chmod/ACL repair occurs.

All recorded forward edges and the original prefix/requested binding are identity-validated. The complete chain is then fsynced with the actual native fsync for every node from leaf through `/`, including existing ancestors. `Synced` is emitted only after native success. The same bindings/privacy are checked afterward. Both wrappers publish through the returned held leaf with existing file publication/mode semantics; no ambient parent reopen is used for publication.

`Attempt::Partial` owns the requested path, resolved prefix binding, held prefix, privacy, known-creation fact, and submitted child name when available. `ReadyToCommit` is constructed only after the requested leaf and complete chain are admitted. Partial retry returns the same error without mkdir, cleanup, barrier work or a prefix-as-leaf result. Complete retry repeats full validation and the whole real leaf-to-root sequence, preserving authority on repeated error. Fresh calls reconstruct/replay the full chain even after dropping the prior error or a process exit.

States distinguish proven no creation (`Unchanged`, which does not certify existing durability), successful mkdir with failed completion (`CreatedDurabilityUnknown`), and unclassified submitted mkdir error (`NamespaceUnknown`). EEXIST opens and validates the actual winner. Other submitted failures conservatively become NamespaceUnknown and retain the parent/name/prefix; no cleanup or retry mkdir occurs. Original native io::Error/kind/source are retained through typed wrapping/downcast.

## Executed RED/GREEN evidence

Initial RED command:

`cargo test -p atomic-file --test parent_provision --features test-support --locked --offline`

- First run: **0 passed, 2 failed**. The original wrapper returned success for missing/../other and created a parent before rejecting a NUL final filename.
- After adding independent checkpoint scaffolding and barrier tests but before production provisioning: **0 passed, 4 failed**. The original wrapper staged despite a directory barrier fault and recorded no complete-chain barriers before staging. The original two assertions still failed.
- After the production change: **4 passed**, then expanded adversarial coverage **21 passed**, and final hook-restoration coverage **22 passed, 0 failed, 0 ignored**. Additional adversarial tests were first executed green; they are not claimed as separately observed pre-implementation REDs.

Final parent suite covers both wrappers; complete tree before initial fsync; exact BeforeSync/Synced leaf-to-root vector before StageCreated and on all-existing calls; early profile failure/no file staging; fresh retry and a two-process restart without prior authority; complete retry/repeated failure/exact leaf identity; Partial retry; pre-submission earlier-effect preservation; synthetic submitted EIO with and without real mkdir effect, typed source and no cleanup; EEXIST actual-winner privacy; ancestor replacement and alias retargeting; injected suffix symlink/dangling link/non-directory; private new/existing leaf, public traversal ancestor, no chmod; privacy revalidation on retry; legacy 0600/0644 file modes versus inherited directory modes; root/dot/existing .. paths; macOS foreign inheritable ACL rejection before mkdir plus owner allows/benign denies; native /tmp alias; scoped observer/failure restoration after panic. The `restart_child` harness entry is included in the 22 total; its parent test invokes both actual child phases.

Other completed commands:

- `cargo test -p settings-store --lib --locked --offline`: **52 passed, 0 failed**. Actual public update reaches provisioning failure, preserves typed directory state and no payload/stage, then fresh retry visits the full chain and writes the exact existing pretty-JSON-plus-newline bytes.
- `cargo test -p atomic-file --lib --test native_publication --test file_lease --features test-support --locked --offline`: **28 lib + 19 native publication + 10 lease passed, 0 failed**. Existing file/privacy behavior and macOS ACL tests remain green.
- `cargo test -p app-state --lib canonical_directory_sync_failure_is_retried_even_when_ancestors_exist --locked --offline`: **1 passed, 47 filtered** unchanged.
- `cargo test -p app-state --lib canonical_missing_or_replaced_directory_and_gate_never_recreated --locked --offline`: **1 passed, 47 filtered** unchanged. Final output also in `/tmp/task4-app-state-source-check.log`.
- `cargo check -p atomic-file --tests --features test-support --target x86_64-pc-windows-msvc --locked --offline`: **exit 0**. Cfg/legacy facade compilation only, not Windows execution.
- `cargo check -p atomic-file --tests --features test-support --target aarch64-unknown-linux-gnu --locked --offline`: **exit 0**. Linux profile/default-ACL test compilation only, not Linux execution.

Existing vendor/automerge warnings appeared during app-state compilation; no owned-source warnings in the reported scoped commands. Explicit rustfmt used only listed owned Rust files with `--edition 2024 --config skip_children=true`.

## Source self-review and qualification limits

Reviewed state transitions, full retained-chain/leaf invariants, original-error propagation, no-effect versus submitted-result boundary, privacy helper reuse, pre/post identity checks, wrapper final-name validation before creation, and Windows cfg isolation. Confirmed no uncertain-directory destructor cleanup, prefix-only success, omitted root/mount barriers, create_dir_all Unix shortcut, ambient publication reopen, or duplicated ACL evaluator. The Linux profile helper is permission admission only. Other Unix permission profiles still reject OwnerOnly through the shared helper; Inherit remains available. No unused public reconciliation API was added for Task 5.

Native execution here is macOS on the host's temporary filesystem and aliases. Actual directory fsync completion and native macOS ACL enforcement were observed. This is **not** a power-loss, durable mount-configuration, or persistent hardware-storage certificate. The process-exit replay test and synthetic EIO seam establish replay/classification behavior only.

No supplied native mount/firmlink qualification fixture was available. Mount/firmlink qualification remains **unqualified**, with no skipped fixture counted as a passing mount test. The ordinary /tmp alias test is not mount/firmlink evidence. Linux default-ACL native execution remains outstanding (the test requires working setfacl and fails if unavailable; it does not skip). Windows native directory barriers/bootstrap/crash execution remain outstanding; production Windows wrappers/probe/native file engine are preserved with their documented transitional ancestor-durability limitation. No permanent Unsupported replacement was introduced.

All owned cargo sessions reached terminal completion: 71321, 18215, 12724, 33804, 40099, 14369, 88059. **No live owned build sessions or spawned background processes remain.**

## Independent review fix 1 — early filename error compatibility

The Important review finding in `task-4-unix-review.md` was confirmed: the new early filename validation retained InvalidInput but lost the existing typed PublicationError(NotPublished). Against the finite `task-4-unix-fix-1-before` snapshot, only `crates/system/atomic-file/src/lib.rs` and `crates/system/atomic-file/tests/parent_provision.rs` changed. `write_provisioned` now wraps both missing-final-name errors and native component-validation errors using `PublicationError::new(PublicationState::NotPublished, source).into_io_error()` before any provisioning operation. The directory protocol is unchanged.

The existing invalid-final-component test was extended and a missing-final-component test added. Each covers both ordinary and explicit-mode parent-creating wrappers and asserts the original InvalidInput kind, recoverable NotPublished classification, owned PublicationError and native I/O source, absent directory state, no provisioning/publication checkpoint, an empty fixture directory, and no created parent.

Focused RED and GREEN used the identical command:

`cargo test -p atomic-file --test parent_provision --features test-support --locked --offline final_component_fails_before_parent_creation`

- RED session 40512: exit 101, **0 passed, 2 failed, 21 filtered**. Both failures were `publication_state` returning None instead of Some(NotPublished).
- GREEN session 77630: exit 0, **2 passed, 0 failed, 21 filtered**. Both wrappers and both filename cases execute in these two tests. This command also recompiles the owned library and integration target.

No broad successful suite was rerun for this localized error-wrapping change. No additional compile check is needed for the unchanged Windows/Linux cfg/native code; the focused native command compiled the edited Unix path. Previously reported full-suite results remain the pre-fix evidence and are not represented as fresh full-suite execution. Explicit rustfmt touched only the two edited files with `--edition 2024 --config skip_children=true`.

Self-review of the fix confirms unchanged early ordering, original source preservation, and no directory-state misclassification. All other reviewed source hashes, Cargo.lock and app-state remain unchanged. Current hashes are recorded in both `task-4-unix-final-sha256.json` and `task-4-unix-fix-1-final-sha256.json`. Source is coherent and held for the parent's finite shared verification checkpoint. Both fix-round cargo sessions are terminal; **no live owned sessions remain**. Qualification limits and full Task 4's open Windows/bootstrap obligations are unchanged.
