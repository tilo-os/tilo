# Task 5 bounded Unix app-state increment

Status: source implemented for independent review; full Task 5 remains open. Windows existing-name and ancestor barrier composition is unresolved. This report does not certify crash/power-loss durability or native Windows/Linux execution.

## Scope and baseline

Compared against `.superpowers/sdd/2026-09-18-platform-durable-publication/task-5-unix-before`, not moving HEAD. Product changes are confined to app-state and atomic-file. No other consumers, accounts, UI, dependency versions, staging, commits, stash, lock regeneration, CARGO_TARGET_DIR, or workspace cargo fmt. Explicit-file rustfmt used edition 2024 and skip_children=true.

Changed files:

- `crates/system/app-state/Cargo.toml`: default-off atomic-file test-support dev edge; remove final unused Unix libc dependency.
- `crates/system/app-state/src/lib.rs`: actual canonical/claim/save paths and production regressions.
- `crates/system/app-state/src/autosave.rs`: actual worker durability-frontier regression (scheduling/status production code unchanged).
- `crates/system/atomic-file/src/{lib,directory,provision,test_support}.rs`: consumed existing-only chain, sentinel/append/existing-commit methods and native-loop checkpoints.
- `crates/system/atomic-file/src/existing.rs` (new): distinct existing-object commit custody/error/retry.
- `crates/system/atomic-file/src/native/unix.rs`: native regular no-follow writable opens, initial/retry commit checkpoint placement.
- `crates/system/atomic-file/tests/existing_commit.rs` (new): existing commit/chain, privacy/no-follow/append/lease and real legacy-flock process interoperability.
- `crates/system/atomic-file/tests/native_publication.rs`: retained retry persistent-failure regression.
- `Cargo.lock`: exactly one removed `"libc"` dependency line in the app-state package compared with the frozen snapshot. Other lock changes preserved.

## Real-path behavior and custody

Canonical initial construction retains create_dir_all as setup. The configure closure runs before source admission, before the first actual full-chain commit, and before actor read/publication. A temporary in-memory actor does not escape. The original exact directory retry visit-vector and missing/replaced source/gate tests are retained without weakening. A one-shot test action runs in the shared engine's real BeforeSync observer to replace a directory during reconciliation; it cannot cause actor publication in either original or replacement.

Unix gates/writers own exactly one FileLease sentinel handle; independently opened comparison handles do not carry or clone the lease. Gate Busy remains WriterBusy; real acquisition errors remain Io. Legacy writer failure still falls back to no writer/fresh actor. Canonical StorageDirectory authority drives actor reads/publication, document reads/publication and existing append; initial admission is the only gate-creation path. Normal canonical save/reconcile cannot mkdir/recreate the source or gate.

Existing-only reconciliation uses the accepted forward-edge Chain preparation/validation/leaf-to-root native commit engine. It never enters missing-suffix creation. Complete errors retain ProvisionError/chain custody and retry every barrier after validation. Source mismatches/disappearance have a separate discriminator; true barrier errors retain their I/O source. Binding messages carry the affected path. Provisioning's ordinary missing-path native error behavior remains separate from existing-only classification.

ExistingCommitError is not PublicationError and invents no publication phase. It retains the admitted file, parent, expected full identity, name and privacy across file-data and parent-name flushes, with binding/privacy checks. Ready retries never publish/delete/create; dropping errors deletes nothing. No-repair canonical acceptance validates owner generation, exact bytes and native identity, commits that existing file/name and the full chain, and only then advances accepted/persisted. Equal-byte replacement is refused. Repair continues through typed Replace publication.

StoreError::Io preserves owned PublicationError via into_io_error; publication_state delegates only that source. Full-save prepublication faults preserve old bytes; final-commit faults leave complete visible bytes but unchanged persisted heads. A native commit retry alone does not advance application heads. The consumed worker-local TestFaults publication request installs with_failure inside write_full on the thread doing publication. The actual autosave worker keeps prior durable_heads and sticky error after failure and only a completed explicit checkpoint clears/advances them.

Room migration admits one held parent across `.qos-` then `.tilo-`. Valid different owners deny; malformed base uses `.recovered`; malformed recovered authority fails closed. New immutable claims use IfAbsent with exact room+newline bytes. Known published final failures retry retained custody once; persistent failures propagate the owned typed error. Unknown publication is returned without deleting evidence. A proven NotPublished result alone never establishes a collision: a real winner must be read. Matching winners execute existing-file data/name barriers, then full ancestry and final identity/byte revalidation before positive ownership. They are never rewritten/replaced. Fresh-process recovery after losing an AfterPublish error fails under either actual existing barrier fault, and succeeds only after the complete protocol. Two processes claiming different owners produce one positive winner; same-owner restart resumes.

BeforeCommit moved from the initial-only orchestration site into Unix CommitGuard::commit immediately before its real parent sync. Initial publication still visits it once. Retained retry now visits the same real checkpoint on every attempt. This was driven by an observed RED where a persistent BeforeCommit fault incorrectly let retry succeed. The complete affected publication suite passes after the move.

## TDD and verification evidence

Actual observed RED commands (all `--locked --offline`, expected assertion failures, no ignored tests):

1. `cargo test -p app-state --lib --locked --offline canonical_configuration_rejects_source_before_actor_effects`: 0 passed, 1 failed; actor already existed in moved original before configuration rejected source. Initial tool session 53549 terminal, exit101.
2. `cargo test -p app-state --lib --locked --offline canonical_accept_rejects_equal_bytes_with_replaced_identity`: 0/1; old acceptance accepted replacement with equal bytes. `/tmp/task5-red-identity.log`.
3. `cargo test -p app-state --lib --locked --offline existing_barriers`: 0/1; matching winner bypassed real existing-data barrier. `/tmp/task5-red-existing.log` (session51953).
4. `cargo test -p app-state --lib --locked --offline canonical_accept_existing_runs_data_and_namespace_barriers`: 0/1; existing acceptance bypassed data/name protocol. `/tmp/task5-red-accept.log`.
5. `cargo test -p atomic-file --features test-support --test native_publication --locked --offline retained_publication_retry_observes_real_commit_failure`: 0/1; retained retry bypassed persistent real commit checkpoint. `/tmp/task5-red-retry.log` (session66236).
6. `cargo test -p app-state --lib --locked --offline canonical_shared_reconciliation_source_change_precedes_actor`: 0/1 while one-shot real-loop test bridge was not consumed. `/tmp/task5-red-shared-source.log` (session72800). This RED demonstrates missing test-seam consumption, not a newly discovered source-validation bug; GREEN executes the replacement at real BeforeSync and verifies production refusal.

The initial source/identity and existing-winner/acceptance regressions preceded their consumer behavior changes. Shared primitive operations and supplemental privacy/process/frontier tests were subsequently added and verified; this is not a claim that every supplemental test or primitive had a separate missing-API RED run. A supplemental test initially had a byte-array type error, corrected before verification; it is not counted as behavioral RED.

Final native GREEN commands and exact results are recorded below after terminal output is inspected. Earlier checkpoints: canonical filter18 passed; full app52 then58 then59 passed; initial atomic84 passed. Existing vendor/automerge warnings remain (24); no new owned-source warnings in successful final logs.

## Platform and transitional inventory

Native execution here: macOS27.0 build26A428, arm64, local APFS `/System/Volumes/Data`; pinned Rust1.97.1. Native tests cover this development filesystem/profile only. No native Linux, Windows/NTFS, real mount/firmlink variation, storage interruption, filter/hardware, or power-loss qualification was performed. Process death/restart and injected failures are not power-loss proof.

Windows/non-Unix transitional helpers remain explicitly visible in app-state:

- StoreLease is still File off Unix; non-Unix canonical acquire_gate retains its existing refusal, legacy gate retains its existing ordinary open behavior.
- Windows legacy writer retains deny-sharing `share_mode(0)` ownership; no new Windows FileLease/sentinel interoperability claim.
- Non-Unix gate_matches retains false, as the previous same_file fallback did. Canonical ownership is not newly advertised.
- Existing directory reconciliation retains its old ordinary File::open/sync_all ancestor sequence off Unix; this is a transition, not the unresolved native Windows answer.
- No-repair existing acceptance retains its old read-only File::open/sync_all fallback; canonical append retains its old ambient existing append helper off Unix. Both await real Windows reconciliation/access semantics and native qualification.
- Legacy parent preparation retains create_dir_all off Unix; no durability proof is claimed for it.
- Non-Unix room claims retain the old hard-link/temp/equality protocol, including its recursive absent-collision retry; it is not counted as migrated or durable existing-winner reconciliation.
- Portable common canonical held read/publish/identity code and legacy full publication still compile with current Windows atomic-file APIs, but the above refusal/transitional boundaries remain. No new permanent Unsupported stub and no Windows native backend edits were added.

The Unix shared methods are intentionally cfg(unix) in framework modules. Future accounts/private-key consumers can use commit_existing with OwnerOnly, but this change does not migrate accounts or add namespace-control/legacy-key policy.

Parent owns independent review and the required final `cargo check -p shell --tests`; not duplicated here. Full Task5 stays open until Windows composition, implementation, and required native evidence exist.

## Final inspected results and terminal custody

- `cargo test -p atomic-file --features test-support --locked --offline`: exit0, 86 top-level tests: 28 unit +5 existing-operation/interoperability +10 lease +20 native-publication +23 parent-provision. Two one-test subprocess subruns are not double-counted. Zero failures/ignored. Final session38490 terminal. Log: `/tmp/task5-final-atomic.log`.
- `cargo test -p app-state --lib --locked --offline`: exit0, 60 passed, zero failed/ignored. Exact final session70585 terminal after shared build-lock wait. Log: `/tmp/task5-final-app.log`.
- `cargo check -p app-state --tests --target x86_64-pc-windows-msvc --locked --offline`: exit0, compile-only, final session18624 terminal. Log: `/tmp/task5-final-windows.log`. The later provision.rs message-only path refinement is Unix-gated and excluded from that target.

All issued exec sessions terminal: 53549, 51953, 17499, 66236, 89729, 57800, 64853, 28833, 72800, 47312, 2953, 28510, 18624, 38490, 70585. No live cargo/build handles owned by this worker remain. Child processes were waited/reaped; native interoperability child guards kill/wait on parent unwind. No compositor/runtime directories were spawned. A read-only process-list command was sandbox-denied; it was not used as evidence of tool-session completion (all listed sessions returned terminal exits).

Source diff saved against frozen snapshot at `task-5-unix-diff.patch`; final source hash manifest at `task-5-unix-final.sha256`. No parent shell-check or independent-review result is claimed by this report.

```text
5ec5e033bdfdf0d30b0cb5475dd9080150a07e39a00c447d03dd4443ce83bbe5  crates/system/app-state/Cargo.toml
498fd0d23ac61b4aa60c237ad3f356acb308b1aad22aed6c0787b120b0aef0ab  crates/system/app-state/src/autosave.rs
cdcac14b6d30b6cb7638f70f18f7119a1111f32d9a29d8d922b0c32d2ae80e9c  crates/system/app-state/src/lib.rs
937999987eeec178e5fa1470cc38bc28d5bbdbaa885b77aae7796151d5a2d278  crates/system/atomic-file/src/directory.rs
c2bfaaf793bac202ae9cf9c9d0937dbca3419a01f65fe5447779150775a99b68  crates/system/atomic-file/src/existing.rs
0bdb0e222a2c7f68c4ea7ed52d1d2fbc9bf138b3c91f1645c7695144a161406b  crates/system/atomic-file/src/lib.rs
358046d15b8768d5d87e42b06a9c066e623363153d5442eef364bc814ae56e11  crates/system/atomic-file/src/native/unix.rs
3e74af81af46814913e73f253a9b5542245bbac75f1406bea42a37375b24e0d6  crates/system/atomic-file/src/provision.rs
4feac83a62ec8e83c6f1c61969961075bd892cc55abab6635a18146bf962524d  crates/system/atomic-file/src/test_support.rs
1f12f828c58300714d916315fb3a4acb5b40987c351f2c5c0e289a41500908bb  crates/system/atomic-file/tests/existing_commit.rs
e96f63bd0504a21a99bfd8ffd16149fe728b562f70f92c593d7ea08a5ea432ca  crates/system/atomic-file/tests/native_publication.rs
8011893fde82c925ab031c68709a7b168ffdf07fe8e42ff89b585ca2234cd908  Cargo.lock
```

Final explicit-file `rustfmt --check --edition 2024 --config skip_children=true` returned exit0; all12 final source hashes reverified unchanged before handoff.
