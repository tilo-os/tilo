# Task 7 — bounded Unix vault migration report

2026-09-18. Source implemented and locally verified; ready for independent spec/quality review. **Full Task 7/S03 remain open.** Native Windows implementation/qualification, native Linux ACL/profile evidence, cross-principal qualification and power-loss evidence are not established by this increment.

## Scope and frozen baseline

Comparison is against `.superpowers/sdd/2026-09-18-platform-durable-publication/task-7-unix-before` and `task-7-unix-before-sha256.json`, never moving HEAD. All original 34 hashes matched before work. Parent later authorized the narrow Settings caller regression and captured its unchanged file, extending the baseline to 35 files. Parent then refined its trigger from orphaned ciphertext to a malformed existing local key so the app test remains platform-neutral; Unix orphaned-key admission remains tested in accounts. Only the following nine source/manifest files differ or were added:

- `crates/system/accounts/Cargo.toml`: dev-only atomic-file `test-support` feature edge; ordinary production dependency unchanged.
- `crates/system/accounts/src/vault.rs`: Unix dispatch to private storage composition; old non-Unix implementation retained behind explicit cfg. Original round-trip/key-mode assertions remain.
- `crates/system/accounts/src/vault/unix_storage.rs` (new): held root/key admission, bounded zeroizing reads, existing/new directory composition, immutable key publication/winner reconciliation, typed failures, read-only load and shared Secret Service migration reader.
- `crates/system/accounts/src/vault/unix_tests.rs` (new): production-path/fault/process regressions and scoped test-only key operation hooks.
- `crates/system/atomic-file/src/directory.rs`: consumed Unix `validate_namespace_control` semantic method.
- `crates/system/atomic-file/src/native/unix.rs`: held effective-owner/no-group-or-other-write directory predicate, reusing supported permission profiles and conservative macOS ACL validation. No change to OwnerOnly, private staging, provisioning, existing commit, native durability, or publication retry engines.
- `crates/system/atomic-file/tests/namespace_control.rs` (new): policy separation, native wrong-owner directory refusal, macOS ACL and Linux ACL fixture source.
- `crates/system/settings-store/src/lib.rs`: tests only, filling missing file-publication phase/visible-format coverage through the existing public facade.
- `apps/settings/src/panes/accounts.rs`: tests only, parent-authorized actual `AccountsPane::save_account` vault-failure/metadata-order regression. Production X01 transaction logic unchanged.

No Imager, UI, app-state, other app, accounts status, dependency-version, lockfile or native Windows changes. No commits, staging, stash, worktree cleanup, cargo fmt, CARGO_TARGET_DIR, lock regeneration or subagents. Explicit-file rustfmt uses edition2024 and skip_children=true. Cargo.lock remains exactly SHA256 `8011893fde82c925ab031c68709a7b168ffdf07fe8e42ff89b585ca2234cd908`.

Diff and final hash artifacts: `task-7-unix-diff.patch`, `task-7-unix-final.sha256` in this SDD directory.

## Resulting production behavior

Existing effective-owner namespace-safe roots are admitted without chmod or relocation. Unix modes0755/0750/0711 are independent of OwnerOnly directory privacy; group/other writes and unsupported permission profiles fail. macOS reuses the conservative ACL validator. Directories do not inherit regular-file single-link admission.

Existing save roots run existing-only `reconcile_directory(Inherit)` across the complete physical ancestry. Only an initial proven NotFound root enters `provision_directory(OwnerOnly)`. Root native identity/canonical path binding and namespace are revalidated around effects and owned retry. No failed attempt repairs permissions, rebases onto a replacement, or cleans unknown directory effects.

The only Unix local-key reader uses held `open_read(OwnerOnly)`, retaining the file and full FileIdentity. It reads into a fixed zeroizing33-byte buffer, requires exactly32 bytes plus EOF, and propagates open/identity/read/privacy/type/link failures. Load uses an existing held root and reads ciphertext from it; it performs no mkdir, key generation/publication or durability commit. A failed public load's empty view supplies no initialization evidence. A missing local key plus existing ciphertext is a recovery stop; uncertain ciphertext absence is also refusal.

Every admitted existing key or actual winner must pass shared `commit_existing(OwnerOnly)` file-data and name barriers before encryption. Bytes alone never establish authority; equal-byte inode replacement fails. New keys use IfAbsent+OwnerOnly. Losing candidates are zeroized/discarded, and an actual held validated winner is committed. AlreadyExists from staging exhaustion does not establish a winner. PublicationUnknown returns its typed failure with no same-call reconciliation, regeneration or cleanup. Known PublishedDurabilityUnknown receives at most one retained commit retry, with namespace/binding validation. A namespace refusal before that retry retains both the original PublicationError and the admission error in `PublicationAdmission`, preserving the already-published phase at the String boundary.

Only after durable key acquisition does save serialize/seal and publish held ciphertext. `try_save: Result<(), String>` remains unchanged, with distinct directory, key admission/commit, publication and recovery diagnostics. QASV/version1, 24-byte nonce, XChaCha20-Poly1305, 32-byte keys, paths, entry representation and custody choice remain unchanged. Ciphertext final-commit failure honestly reports failure even though new complete bytes can be visible.

Secret Service lookup failure still refuses local fallback. Read-only load may read an existing validated local key when the selected service has no key, but never generates or provisions. Save's already-selected migration route uses exactly the same held bounded local reader and existing-key commit. Store/lookup verification and ambient local-file removal remain the prior transport/removal protocol; this increment does not qualify or redesign it.

### Private creation source clarification

The approved compatibility prose initially described private-creation preflight too broadly. Actual existing Unix directory provisioning calls `creation_preflight`, conservatively rejecting even noninheritable foreign allows. File Stage::create instead validates the actual newly created private child before writing bytes; a noninheritable foreign-read parent ACL need not be inherited by that child. Namespace admission independently rejects that parent in this vault path. Tests preserve these distinct existing obligations. No production preflight/ACL relaxation or new ACL engine was introduced. Parent acknowledged this source correction and owns the design wording update.

## Evidence by required test group

1. Genuine old-layout fixtures are produced in isolated subprocesses with umask022, ordinary create_dir_all, old private-file writer and version1 known credential. Parent asserts0755 before new code runs. Reopen/decrypt/save/reopen preserves old/new secrets, key bytes/full native identity, directory identity/mode, location and QASV1 framing. Original round-trip/key-private baselines were run before production edits and retained.
2. Direct framework tests separate namespace control from OwnerOnly across0755/0750/0711/0700 and group/other-write modes. Existing nonprivate leaves still fail OwnerOnly provisioning without mode changes. A directory child ensures directory link count is not treated as file link count. A real foreign-owned `/` is rejected under this non-root test principal without mutation.
3. Native second-principal ability to list names but not read/create/rename/unlink was **not executed**. No unavailable fixture was treated as passing. Linux named-user read/search and write-ACL fixture source was added and cross-compiled; native ACL/mask/profile enforcement remains unqualified.
4. macOS native owner allow, benign deny, foreign read allow, and inheritable foreign allow directory fixtures ran. Namespace and directory-preflight results are independent; ACL entries and mode remain unchanged. The existing full atomic-file suite also ran its inherited-file-ACL-before-writing and benign ACL private-file tests. Harmless foreign-read directory ACL compatibility beyond the conservative accepted profile remains open.
5. Local public load/save cover lengths0/31/33/oversized, symlink, FIFO, directory, hardlink, privacy-invalid and real mode000 unreadable keys, plus valid32-byte keys. Scoped test-only hooks inject failures at actual key-open, post-open identity-query, and bounded-read call sites; tests prohibit reaching RNG/encryption/publication and verify preservation. Secret Service migration tests use an isolated fake `secret-tool` transport subprocess with the real bounded-command route; they cover valid key migration, private-invalid/short key, the same three injected I/O failures, and lookup-error refusal. Native foreign-owned-key/cross-principal read denial beyond these owner-mode fixtures remains qualification work. Injected query/read errors are consumer-policy evidence, not hardware/native EIO certification.
6. Read-only load observers forbid all real publication/provision/existing-commit visits. Orphan ciphertext never causes generation on load/save. Uncertain ciphertext query and nonregular ciphertext prevent initialization. Truly absent roots initialize with0700 directories and0600 key and reopen successfully.
7. Existing-file data/name faults and ancestor-sync faults prevent ciphertext publication. Positive ancestry observations match the complete leaf-to-root chain. Real fresh subprocess restart after dropped key-publication errors executes existing data/name barriers again; faults still refuse ciphertext, and successful restart preserves key identity/bytes. No durable authority is inferred from load success.
8. Two actual processes rendezvous at their real first BeforePublish, forcing competing key candidates. They race key publication and then serialize the unrelated ciphertext replacements; both successful saves/reopens use the immutable winning key. This does not claim concurrent whole-vault transaction semantics. A fresh process fills exactly32 actual staging names and proves AlreadyExists exhaustion cannot invent a key winner or trigger repeated RNG. Persistent final key-commit failure visits initial commit plus exactly one retry and never encrypts. One-shot failure succeeds via retained retry; ambiguous publication blocks until a later fresh reconciliation. Equal-byte replacement both during the shared commit and between bounded read and commit is refused.
9. Root replacement, alias retargeting during real ancestry commit, and namespace mode changes during actual key commit fail without rebasing or rewriting permissions. Namespace change at the owned retry boundary refuses retry and preserves the PublishedDurabilityUnknown diagnostic and visible key. Partial private provision effects survive; a fresh call enters existing-only admission and repeats barriers before initialization can succeed.

The added Settings test runs the actual AccountsPane::save_account in a subprocess with empty PATH to guarantee local custody without touching a desktop credential service. Existing ciphertext with a deliberately malformed31-byte key reaches the exact `could not save credential:` error, not an intent/preparation error. Existing store.json bytes, ciphertext and malformed key bytes remain identical, the old account remains and the new account is absent. The test has no OS conditional, native API or permission setup. This proves the positive vault-before-dependent-metadata ordering only; X01 compensation/recovery/concurrent multi-file semantics remain separate required work.

## RED and baseline chronology

All cargo commands below use `--locked --offline`; expected failing runs returned exit101 and no ignored tests.

- Original baseline: `cargo test -p accounts --lib --locked --offline vault` — exit0, original6 tests, including `vault_put_save_load_round_trips` and `key_file_is_owner_only`; session59723 terminal.
- Behavioral RED: `cargo test -p accounts --lib --locked --offline vault::unix_tests` — 2 passed/4 failed. Failures demonstrate load generated a key with existing ciphertext, private-invalid key accepted, existing key bypassed real existing commit hook, and existing root bypassed real ancestor hook. Legacy0755 compatibility already passed old code. Log `/tmp/task7-red.log`, session64890 terminal. These tests preceded the Unix migration.
- Missing API compiler RED: `cargo test -p atomic-file --test namespace_control --locked --offline` — E0599 before namespace-control API was introduced. This is compile-only missing-API evidence, **not** a behavioral RED. `/tmp/task7-red-namespace.log`, session81552 terminal.
- Read fault seam RED: `cargo test -p accounts --lib --locked --offline vault_injected_open_query_read_failures` — actual save still published because new test hooks were not consumed. `/tmp/task7-red-read-hook.log`, session3024 terminal. This proves missing seam consumption, not a newly discovered native EIO bug.
- Final typed-phase RED: `cargo test -p accounts --lib --locked --offline vault_owned_key_retry_revalidates_namespace_before_retry` — namespace refusal originally discarded PublishedDurabilityUnknown. Strengthened assertion failed before `PublicationAdmission` was implemented. `/tmp/task7-red-retained-phase.log`, session31977 terminal.

Supplemental process/adversarial/native/Settings tests were added and verified after the core behavioral fix. No claim is made that every supplemental test independently saw a preimplementation RED.

Two new macOS ACL test fixture mistakes were diagnosed and corrected without changing production: the initial observation included ls directory size/link metadata that naturally changes with children; later, a test incorrectly conflated directory preflight with staged-file privacy. The corrected test compares actual ACL entries/mode and invokes the existing directory preflight for that obligation. Failed logs: `/tmp/task7-green-namespace.log` (32709) and `/tmp/task7-final-atomic.log` (75846). They are not reported as production ACL failures or hidden by ignored tests.

## Final inspected verification

Native execution: macOS27.0 build26A428, arm64, pinned Rust1.97.1. These results describe this development host/filesystem only; no native Linux/Windows execution or power-loss experiment occurred.

- `cargo test -p atomic-file --features test-support --locked --offline` — exit0, **89 top-level tests**:28 unit +5 existing commit +10 lease +3 namespace +20 publication +23 parent provision; zero failed/ignored. Child subruns not double-counted. `/tmp/task7-final-atomic2.log`, session72388 terminal.
- `cargo test -p accounts --lib --locked --offline vault` — exit0, **29 tests**, zero failed/ignored. `/tmp/task7-final-vault2.log`, session19072 terminal. The later strengthened typed-phase assertion/fix was covered in the final full47-test run below.
- `cargo test -p accounts --lib --locked --offline` — final exit0, **47 tests**, zero failed/ignored, including all29 vault tests after final typed-phase fix. `/tmp/task7-final-accounts2.log`, session25363 terminal. No accounts-owned warnings.
- `cargo test -p settings-store --lib --locked --offline` — exit0, **53 tests**, zero failed/ignored. `/tmp/task7-final-settings-store.log`, session27417 terminal.
- `env PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig cargo test -p settings --lib --offline --locked vault_save_failure_preserves_existing_account_metadata` — exit0, one top-level test plus its one-test child, zero failed/ignored. `/tmp/task7-final-settings-caller2.log`, session19321 terminal. Earlier Unix-only orphan-key version passed in38787, then parent boundary review required the final platform-neutral malformed-key fixture.
- Same Settings command with filter `edit_preserves_envelope_unknown_config_and_service_toggles` — exit0, one passed, zero failed/ignored. `/tmp/task7-final-settings-positive.log`; command returned terminal directly. Existing vendor/dependency/UI/Settings warnings remain; no production app source was changed.
- `cargo check -p accounts -p atomic-file -p settings-store --tests --target aarch64-unknown-linux-gnu --locked --offline` — final exit0 after typed-phase fix. **Compile only.** `/tmp/task7-final-linux2.log`; terminal direct result. Prior check32422 also terminal.
- `cargo check -p accounts -p atomic-file -p settings-store --tests --target x86_64-pc-windows-msvc --locked --offline` — exit101, eight errors in unchanged accounts/src/status.rs (unconditional Unix imports, OpenOptionsExt::mode, AsRawFd/libc::flock). `/tmp/task7-final-windows.log`, session23171 terminal. Parent identifies this separately inventoried A09-02/S02 status-lock work. File is byte-identical to frozen baseline; this is the exact observed blocker, not a claimed independently reproduced baseline build failure.
- `cargo check -p atomic-file -p settings-store --tests --target x86_64-pc-windows-msvc --locked --offline` — exit0. **Compile only**, not native namespace/durability qualification. `/tmp/task7-final-windows-framework.log`, session28780 terminal. Final later accounts-only Unix error variant is excluded from Windows.
- Explicit eight-owned-Rust-file `rustfmt --check --edition 2024 --config skip_children=true` — exit0. No workspace cargo fmt.

`python3 tools/check_app_platform_contract.py` — exit1 with exactly the existing28 Imager violations and **zero Settings entries**; final log `task-7-boundary-check-after.log`, session75138 terminal. Parent had detected a29th violation from the initial Unix cfg on the new caller test; that cfg was removed, without adding a baseline allowance. The whole boundary guard remains red on the separately owned Imager work.

Parent owns independent review and required final `cargo check -p shell --tests` integration; neither is claimed here.

## Transitional inventory and unresolved gates

- Non-Unix vault try_save retains ordinary create_dir_all plus legacy key acquisition/write; load_entries retains exists/ambient reads/create-capable selection; load_or_create_key, local_file_key, secret_service_key and write_key_file retain their prior non-Unix implementations. In particular prior ambient reads and error-driven key regeneration/replacement are not migrated or certified. No new permanent Unsupported substitute was added.
- Windows still requires a distinct framework namespace-control predicate for real directory mutation/security rights, not the private-DACL predicate; existing-only ancestry/key winner commit composition and native legacy ACL artifacts remain outstanding. Compilation does not supply these contracts.
- accounts/src/status.rs remains the separately owned blocking Unix flock portability issue; this increment does not change status locking or whole read-modify-write ownership.
- Unix Secret Service lookup/store/verification/ambient removal remains separately unqualified transport/removal custody. Fake subprocess fixtures prove storage branch behavior, not a native Secret Service, Keychain or Windows credential service port.
- X01 Settings account-mutation journal/metadata-error compensation/recovery and concurrent multi-file transactions remain required separate work. The caller regression only verifies that vault failure blocks subsequent metadata publication.
- Native Linux supported-filesystem/access/default-ACL mask enforcement, real second-principal reads/name visibility/create/rename/unlink exclusion, native foreign-owned key fixtures, broader macOS harmless foreign-read ACL support, native Windows/NTFS/filter/ACL/bootstrap/restart results, distribution qualification, storage interruption and power-loss durability remain open. An absent test subject was never skipped and counted as passing.

## Terminal custody and final hashes

Every issued long-running session is terminal:59723,64890,81552,88561,3024,70003,32709,99598,75846,43414,72388,13541,27417,38787,19072,23171,32422,28780,31977,25363,75138,19321. No live cargo/build handle remains. Spawned test children are waited/reaped; the two-process guard kills/waits on unwind. No compositor/runtime directories or persistent services were spawned.

Final source hashes (also `task-7-unix-final.sha256`):

```text
91e49a559451d61736d9569cea5cbdc89c3fb56b3e00a42a9177479db84cb92b  apps/settings/src/panes/accounts.rs
f9567e59387de3b7f1abc314a2d872bc4e4a61588431fb4bbece0f9ca78b4d79  crates/system/accounts/Cargo.toml
131ac513729b19237b3619fab5373edbed43a2a4374bf80f826ae90c192c8b22  crates/system/accounts/src/vault.rs
d20657551931759f2c0a69f4a4201c4a7abb909b1026c68cbb9ca67dd06d9199  crates/system/accounts/src/vault/unix_storage.rs
1dc36211306004ae1c8b70b58c365951eddb29a5d122559a51b61b1e0f4f2e9a  crates/system/accounts/src/vault/unix_tests.rs
fa01cd05b1095041e6733d2536b9fb41425e6f72a4b26d887719f8dd81dbccbb  crates/system/atomic-file/src/directory.rs
38e0908201a4c91d10be0f03fe9262b602e02e9afda43aef9c66b1248d3dc4e5  crates/system/atomic-file/src/native/unix.rs
432ffbf40245ed584a7d320a6b6d0344d8378006365c2e89875d996bb584a4fe  crates/system/atomic-file/tests/namespace_control.rs
3a358338891c3c38d9ba8fc05b4f5fb6941a7a1f08c15c7e8f2ccb5bbc234bff  crates/system/settings-store/src/lib.rs
```
