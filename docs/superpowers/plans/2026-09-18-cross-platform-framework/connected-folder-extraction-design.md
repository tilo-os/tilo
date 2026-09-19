# N03 connected-folder boundary report

Date: 2026-09-18. Read-only source mapping; no product edits, Cargo commands, native tests, devices or other tasks' SDD workspaces. References and SHA-256 inventory below describe this checkout, not the historical audit snapshot. The requested `audit05-imager.md` resolves to `docs/audits/platform-extraction-2026-09-18/05-imager.md` (A05-06–09). This is an extraction proposal, not an executable implementation plan or security certification.

## Decision

**Physical framework ownership can move now, without waiting for all S01/I01/C01 implementation. Move the existing native protocols intact; do not replace them with partially equivalent generic primitives.** N03 remains semantically incomplete until scoped host composition, remaining native qualification and installed startup evidence are supplied. The roadmap's prerequisites are acceptance dependencies, not a reason to retain app-owned mechanisms.

Use a focused, non-rendering `crates/system/folder-service` (`tilo-folder-service`) for connected-folder and verified-copy operations. Keep transaction/store/recovery policy as portable service modules; native implementations are their private children. Do not put this 60,000+ line closure in `ui`, `atomic-file`, `bounded-command`, or the NTFS parser crate. Preserve the existing `runtime-connected` no-I/O exchange core and `tilo-connected-protocol` candidate codec as separate layers (verified below), reuse `qos-ntfs-protocol` unchanged, and reuse the existing cap-std/cap-primitives/cap-fs-ext libraries. Evaluate `atomic-file`/`runtime-fs`/`bounded-command` operation-by-operation before later consolidation.

**A bare `connection/` move is insufficient.** Its direct shared dependencies are small, but currently expose native pinned state to the adjacent copy engine. Widening those internals to public raw-file APIs to keep the engine in the app violates the boundary. Recommended coherent first cut includes the adjacent local/native copy engine and its storage/worker modules, plus narrowly split native helpers used by the app. Private download cache is a separate cut, not a transitive requirement for connection extraction.

## Existing connected crates: destination check

**Keep `runtime-connected` and `tilo-connected-protocol` separate; retain the new focused service recommendation.** Both are already workspace members (`Cargo.toml:163,175`), but neither currently owns a folder operation, authenticated transport, native backend or durable store. This judgment follows their implemented contracts, not their names. No changes to these existing crates are required for the physical relocation.

| Existing owner | Verified contract and boundary | Consequence for N03 |
| --- | --- | --- |
| `crates/runtime/runtime-connected` | `src/lib.rs:1–5` explicitly promises serialized **no-I/O** exchange state and says a future driver must own deadlines/cancellation/physical closure. `Cargo.toml:12–14` depends only on thiserror and tilo-connected-protocol. `src/exchange.rs:138–197` gives one in-flight request/correlation; `:201–247` checks structural matching only; `:252–291` classifies cancellation/failure/EOF as poisoned/unknown, without stopping native work. | Preserve the narrow client exchange layer. Putting 60,000+ lines of native mutation/recovery here would change its dependency and authority contract and make protocol-only consumers pull in filesystem/process backends. A future host-selected client driver should reuse `ClientCore`; a server/service implementation owns native operation validation and effects separately. Neither `FeedResponse::Complete` nor a JSON `success` field can become a transaction acknowledgement. |
| `crates/session/tilo-connected-protocol` | `src/lib.rs:1–5,16–20` provides structural framing only, 262144-byte body bound/depth 64. `src/envelope.rs:7–8,21–28,44–78` implements `tilo.connected` version 1 candidate envelopes with opaque JSON payloads. `src/codec.rs:25–33,54–107` is a four-byte big-endian length-prefixed decoder. `src/native_string.rs:10–17` explicitly permits NUL and unpaired UTF-16 as inert data. | Keep parser/codec independent of service and OS libraries. It is not the existing NTFS schema-3 seqpacket protocol, lease-receiver protocol, journal or mutation API. Do not change existing on-disk or child/broker wire bytes to force reuse. NativeString cannot replace validated RelativePath or persisted native encodings without explicit semantic validation and compatibility design. |

Proposed future layering: app SDK → host-created semantic client/driver → `runtime-connected` + `tilo-connected-protocol` for the **new candidate client channel**, with the driver handling authenticated transport and physical closure; server dispatch → `tilo-folder-service` → existing native mechanisms/NTFS protocol. Do not add that channel or dependency merely to complete a source move. In-process service composition can remain in-process while preserving its existing grants/refusals. A later approved driver might also live in runtime-connected, but the no-I/O core must remain usable without the native server closure.

Existing tests reinforce this separation: `runtime-connected/tests/exchange.rs:73–98` preserves `{"approved":true,"success":true}` as unvalidated payload; `:253–270` checks unknown outcome on terminal events rather than native cancellation. `tilo-connected-protocol/tests/codec.rs:330–359` round-trips NUL/non-Unicode/unpaired units. Keep these tests at their current owners. They are useful future client/protocol regression evidence, **not substitutes** for N03 leases, auth, recovery or native filesystem qualification. No tests were run for this check.

## Exact relocation proposal

Paths below are relative to repository root. Preserve filenames/module nesting initially, to minimize changes to private visibility and self-spawning test names.

| Existing source | Proposed destination / treatment |
| --- | --- |
| All 31 files under `apps/imager/src/setup/connection/` (enumerated in hash inventory) | `crates/system/folder-service/src/connection/`, same suffix. Move complete model, reconciliation, store codecs/recovery, Windows storage, native descriptors, witnesses, lease receiver, and all embedded/separate tests together. |
| `apps/imager/src/setup/destination_security/{mod.rs,unix.rs,macos_acl.rs,windows.rs}` | `crates/system/folder-service/src/destination_security/`, same suffix, private module. Preserve Darwin ACL and Windows protected DACL/handle policy. |
| `apps/imager/src/setup/nofollow.rs` | `crates/system/folder-service/src/nofollow.rs`, private. |
| `apps/imager/src/setup/{migration.rs,migration_durability.rs,migration_source.rs,migration_journal.rs,migration_journal_log.rs,migration_native.rs,migration_native_journal.rs,migration_review.rs}` | Same basenames under `crates/system/folder-service/src/`; keep sealed source/transport, pinned-directory/temp implementations, receipts, recovery, and serialization together. Expose only semantic copy/review results and operations. |
| `apps/imager/src/setup/{migration_store.rs,migration_worker.rs}` | Same basenames under service `src/` for first physical extraction. Storage implementation and owned worker lifecycle belong here. Their portable records/status DTOs remain portable service code, not native backend code; app still decides when to approve/start/retry/show them. This avoids exporting `PinnedDirectory.file`, `OwnedTemporary`, log handles, or a public `CopyFileSource` implementation point. |
| `apps/imager/src/setup/folders.rs:14–205` | Split scanner DTOs and `root_parent_and_name`, `pin_selected_root`, `open_descendant`, `empty_scan`, `scan_folder` into service `src/scan.rs`. Move scanner-only tests. Leave picker/presentation policy app-owned pending D01; do not move UI imports into service. |
| `apps/imager/src/setup/desktop_continuation.rs:90–117`, native work in `resolve` around `:244–257` | Service `src/review.rs`: checked directory identity, destination absence/eligibility and source inventory revision, returning data/opaque review results, never files. Continuation serialization/mode/destination choices remain app-owned. Preserve exact source/parent/revision comparison semantics. |
| `apps/imager/src/setup/desktop_continuation.rs:450–642` | Framework-owned `src/legacy_session_readiness.rs` as a narrowly named temporary backend, with N04/I01 owner recorded. Move complete lookup/authentication/connect sequence and native tests; expose only a semantic readiness result. This is required to remove its dependency on the now-private `PinnedDirectory`; do not move the whole continuation/UI module or expose the descriptor. Later relocate to runtime-portal when its exact peer/executable-attestation contract is available. |
| `apps/imager/src/setup/import_view.rs:678` | Replace `ensure_private_root` returning a pin with a service operation returning `Result<(), ...>` (caller currently discards the pin). |
| `apps/imager/src/{main.rs,setup/mod.rs}` | Wire framework-owned early command dispatcher before graphics/portal/runtime startup; remove moved module declarations or temporarily re-export semantic framework modules. App→framework compatibility imports are permissible; framework→app calls/path-includes are not. |
| Root `Cargo.toml`, `apps/imager/Cargo.toml`, new service `Cargo.toml`, `crates/app/tilo-app/{Cargo.toml,src/lib.rs}` | Add narrow service membership/dependency and SDK semantic export/feature. Service owns target dependencies. Preserve edition 2024 and review only necessary lock dependency-edge changes. Do not prematurely remove Imager libc/windows/cap dependencies still used by unmigrated N01/N04/D01/cache code. |

This is the lowest-risk **whole-file** closure. A smaller alternative is to split `migration_store` into native store engine vs portable records/worker policy first, but that needs an explicit new private-store contract and test split. Moving the whole existing non-UI module first is less semantic change. Neither alternative requires moving all Imager setup/release code.

## Why this closure is real

- `connection/store/storage.rs:2–9,117,304,1359` uses destination security, durability, no-follow metadata, `migration::{PinnedDirectory,OwnedTemporary}`, `migration_store::ensure_private_root`, journal framing and `same_snapshot`; `store/recovery.rs:3,37` shares the same primitives. Windows storage also uses `migration::same_snapshot` (`storage/windows.rs:35`). Endpoint and Linux transactions use destination security (`endpoint.rs:5`, `windows_native.rs:74`, `transaction/native/linux.rs:13`). There is **no direct internal_cache or migration_native dependency in connection**.
- Moving `PinnedDirectory`/`OwnedTemporary` alone reaches `migration.rs:19–24,71,295,371`; staging is generic over the sealed `CopyFileSource`. That source returns `migration_journal::FileReceipt` (`migration_source.rs:2–5,16–43`). Journal → source inventory → `migration_store::NativePath` is another edge (`migration_journal.rs:345,397`; `migration_review.rs:3–6`). `migration_journal_log.rs:267` calls the journal's typed persistence-result mapper. These are protocol/receipt dependencies, not merely utility imports.
- Native copy (`migration_native.rs:2–8,25`) implements a sealed transport over the existing authenticated NTFS client and consumes the same staged-copy source interface. Its journal imports `Plan`/`PlanEntry` back from native copy (`migration_native_journal.rs:2–6`) and explicitly excludes session tokens from persisted facts (`migration_native.rs:52–78`). Keep the cycle inside the service crate.
- `migration_store.rs:873–912` is the actual `ensure_private_root` implementation. Do not drag unrelated product flow into the service just because of this one helper: its body can become a private service helper; the complete store/worker recommendation above is to retain private typed storage/worker invariants, not because the connection needs product operation records.
- `migration_store.rs:16–20` and `migration_worker.rs:8–10` form a non-rendering cycle. Public promotion should be selective: opaque store/job/worker types and semantic methods/status records; keep native fields, test hooks and low-level entry points private.

## Constructor and visibility obligations

Keep the whole connection hierarchy under one crate-private boundary. Rewrite `pub(in crate::setup::connection...)` to the equivalent new ancestor, never blanket `pub`. `store.rs:12` keeps errors/operation IDs/admission machinery scoped; `transaction.rs:17–115` binds accepted preview to exact record, weak lifetime and digest. Native `NativeRestore`, `NeverBegun`, selected proofs and `NativeSafeBoundary` retain private fields and borrowed lifetimes (`transaction/native/mod.rs:24–103`). Directory admissions/continuations are likewise mintable only by native/store code. No public constructor from a receipt, raw handle, boolean or caller-supplied identity.

Distinguish *data assertions* from authority: `model.rs:350–389` explicitly documents public `VerifiedBaseline::from_verified` as a bounded store-adapter assertion, not a mutation grant. `ObservedTree::new` and path/identity DTOs are also data. Do not claim these existing constructors are native authority or “fix” them by redesigning the algorithm in a move. Keep them off any new authority-bearing SDK API unless their descriptive status is explicit.

Existing endpoint surface itself needs curation: `endpoint.rs:85` exposes a Linux-only `ReadEndpoint::Ntfs` variant, and `:206` exposes `register_ntfs_session(Arc<Mutex<qos_ntfs_protocol::client::Client>>, ...)`. They may remain framework-internal backend contracts; do not wholesale re-export `endpoint` as a portable SDK. The SDK needs an opaque read-session wrapper and native selection inside framework composition. The current transaction executor/store are not public app capabilities; no GUI transaction caller was found. Do not create a new UI feature merely to exercise extraction.

## Receiver startup, process and authentication

Production `Receiver::start` re-execs `current_exe`; only unit tests use `QOS_LEASE_HELPER` (`lease_process.rs:219–226`). Preserve `--qos-connected-lease-receiver`, fd 3, seqpacket frame format, deadlines, unique receiver reservation, bounded revoke/kill/reap and deferred reaping. `:238–257` sets null stdio and CLOEXEC on unrelated descriptors using close_range, with refusal rather than a weaker fallback. `:464–505` verifies socket type, spawning-parent PID, effective UID, real/effective UID agreement and no-new-privileges. `:623–636` consumes fd 3 only in exclusive early mode. Signals/signalfd remain confined to the child.

**Do not leave current_exe implicit in a reusable service.** Preserve it for an initial compatible self-reexec host *only with* one framework-owned early dispatcher called unconditionally by every host that enables the service, before threads/UI initialization; non-Linux dispatch returns None internally. Imager's current Linux cfg dispatch is `main.rs:65–73`. This removes app-side backend selection without changing the process protocol. A later dedicated framework helper binary is optional, but switching to it requires framework-owned installed-path resolution and package inclusion tests; do not add developer PATH/environment fallbacks or Cargo-on-launch. `bounded-command` owns general child lifecycle but its current public command/result APIs (`src/lib.rs:22–108,157–255`) do not prove equivalence to this bidirectional leased-FD protocol. Integrate only after an explicit protocol/lifetime comparison.

The second early administrative path is `migration_native::maybe_run_command` (`migration_native.rs:763–851`): preserve `--native-copy-selected`/`--native-resume-selected`, bounded no-follow descriptor read, independently expected identity check, signal cancellation and diagnostic-only output. Hide Linux client types from ordinary SDK consumers. Administrative integration tests currently invoke the Imager executable; update their host binary path together with any startup change.

NTFS is already correctly separated into `crates/session/qos-ntfs-protocol` and `qos-ntfs-helper`. Retain those owners; do not pull parser/build.rs/native libraries into folder-service. Client checks root-owned ancestry, root peer UID, hello identity/client UID, nonce and monotonic sequence (`protocol/src/client.rs:36–115,130–160`); broker checks selected client UID (`helper/src/broker/ipc.rs:95–120`) and retains exclusive claims (`broker/session.rs`). Keep existing socket frame/ancillary-FD refusal. A generic I01 socket connect cannot substitute for these checks.

Packaging remains `/usr/libexec/qos-ntfs-probe` via `qos-ntfs-helper/stage_install.py`; `README.md:181–205` states a dedicated pre-existing nonzero parser account, one authenticated session/no reconnect, and kill/reap before claims release. No new account/service/polkit activation is authorized by this move. Preserve native confinement and licenses/source shipping. Review `tests/native_copy_coordinator.py`'s `--imager` parameter and hardcoded copy command flags when moving entry points.

## N02 cache is separate

Do **not** move `internal_cache` merely to satisfy an N03 prerequisite row. Its native providers depend on `internal_download::SelectedManifest` (`internal_cache/{checkpoint.rs:1,linux.rs:42,windows.rs:118}`), and coordinator depends on `release::InternalRelease` plus `InternalTransport`, `PersistentOutcome`, `Progress`, `SelectedManifest` (`coordinator.rs:5–8`). Its tests also depend on bundle/release fixtures. It has no connection dependency.

A later N02 cache plan must move `internal_cache.rs` and all eight child files with their sealed `PreparationComplete`, provider/session and held-reader contracts, or split those contracts coherently. Keep download transport/range/hash and bundle schema portable; move native component-open/identity portions of `internal_bundle.rs:423–894` with providers. Do not invent a caller-constructible “selected manifest”/“ownership” proof to break this cycle. `internal_download_flow` and product release choices remain app-owned. Linux cache explicitly documents cooperative same-UID limitations; Windows explicitly disclaims guaranteed power-loss directory-entry persistence. Replacing either with “durable cache” after extraction would overclaim.

## Existing framework reuse and remaining work

`atomic-file` already exports `StorageDirectory`, identity types, `Privacy`, `PublicationState`, `replace` and `publish_if_absent` (`src/lib.rs:49–87`), plus leases. Its documented trusted namespace and durably provisioned parent preconditions (`:18–24`) do not automatically match connected-folder rename/capture/quarantine/no-replace protocols. Do not replace connection acknowledgements or legacy receipt formats with atomic-file outcomes without a reviewed mapping. Existing publication work has another writer; make no concurrent edits there.

`runtime-fs` already has opaque live `DirectoryGrant`/`FileGrant`, rights and revocation (`grants.rs:1–51`), but its current native-name implementation uses Unix byte APIs (`:54–76`) and runtime attachment brings Cap'n Proto/runtime-portal. It is not a demonstrated portable drop-in for the Windows UTF-16/volume identity and lease-backed transaction engine. Future G02/W02 integration should consume live grants and pass host-bound scoped clients, preserving per-operation revocation. Existing path inputs remain compatibility inputs that are revalidated, not proof that HostContext authorization is complete.

Keep defaults `qos-imager/migrations` and connected-store legacy path (`migration_store.rs:421–426`, `connection/store.rs:572`) discoverable until L02 supplies explicit migration. No journal schema, native name byte encoding, checksum, namespace, durable acknowledgement, recovery budget, retained conflict version, descriptor or wire change belongs in the physical move. Windows endpoint/store availability remains distinct from Linux transaction execution; macOS unsupported endpoint remains an explicit refusal. Full same-app cross-platform support is still open.

## Callers, test relocation and bounded sequence

Caller edits: `apps/imager/src/main.rs`, `setup/mod.rs`, `setup/{import_view,desktop_continuation,folders}.rs`, and indirect scanner users `setup/{controller,view}.rs` if DTO imports change. Retain action/resource dispatch and UI tests. No other production direct connection user was found outside its subtree and main's helper startup. Keep product `FolderChoice`, `FolderMode`, continuation serialization/revision, review UI, approval selection, installation flow and strings under the app.

1. Freeze/refingerprint mapped source; obtain exclusive move ownership. Write the executable plan with exact public semantic methods and review/native helper splits above. Do not change atomic-file/app-state concurrently.
2. Add service crate and move full connection + migration closure without algorithm changes. Update absolute visibility paths, fixture imports and **literal test executable filters** (not only Rust imports). Keep existing portable model/reconcile/format tests and native fault hooks in the owning crate. Selectively expose app-required semantic methods; add SDK wrapper only for actual callers.
3. Split scanner/review/readiness native leaves and migrate callers, keeping app policy. Native readiness tests move with backend; continuation/approval/action tests stay in app and call production service. Cross-crate app tests currently call `migration_store::test_support::hold_save` (`import_view.rs:2276,2311,2333`): provide narrowly feature-gated service test support/dev dependency, never production evidence constructors. Preserve all assertions.
4. Move startup implementation, maintain compatible early dispatcher, update helper test binary discovery and distribution checks. If a separate binary is selected, package it in the same unit and verify installed path without checkout/PATH; do not defer that prerequisite after deleting app receiver entry.
5. Retire exact moved-site `tools/app_platform_baseline.json` entries; leave only still-present N01/N02/N04/D01 sites with their owners. Never add blanket allowances or regenerate the baseline to absorb leaks. Re-run the boundary checker and dependency/API fixture gates. Parent-provided fresh boundary output is `.superpowers/sdd/2026-09-18-platform-durable-publication/boundary-current.log`: exit 1, 28 violations/55 apps, SHA-256 `dbf3addc6f28400003fa4af10502c6598d36f45f46526f358e63893be623441c`; all reported sites are in the connection closure, including stale exception entries. This is source-boundary evidence, not native correctness evidence.
6. Compile/test the moved library and all affected Imager test targets on applicable hosts; repository finishing gate remains `cargo check -p shell --tests --locked`. Run portable and native suites separately. Preserve checkpoint/journal golden/roundtrip tests, privacy/no-follow/ACL/native-name/collision/resume/space/persistence tests. Do not count missing fixture tests as passing.

All 31 connection files carry their tests unchanged, especially `store/{tests,directory_tests}.rs`, `transaction/tests.rs`, `transaction/native/linux/directory/tests.rs`, embedded Windows storage/endpoint tests, `transaction/native/linux.rs` production-path race/recovery suites, `linux/witness.rs:798+`, and `linux/lease_process.rs:640+`. Preserve overflow/lost witness, lease break, helper death/parent EOF, signal isolation, bounded shutdown and crash recovery qualification. `QOS_ENDPOINT_FIXTURE` and `QOS_LEASE_HELPER` must be provisioned; helpers contain exact `setup::connection::...` test filter strings that need corresponding updates. Move all migration/destination-security/nofollow/durability tests with implementations. `qos-ntfs-helper/tests/{native_session.rs,fd_boundary.rs,native_copy_coordinator.py,native_session_coordinator.py}` stay with broker ownership; update only migrated consumer/runner names and run the existing safe fixture suites.

Outstanding after physical extraction: G02 authorization/lifetime client integration; S01/S02 semantic storage/lease consolidation where equivalent; I01 authentication transport integration; C01 common process ownership; N02 cache; N04 final readiness provider ownership; native Linux/macOS/Windows evidence and installed artifact qualification. These are tracked remaining work, not reasons to weaken current refusals or certify unavailable operations.

## SHA-256 source inventory

Hashes identify every proposed moved source and named caller, plus directly inspected adjacent contracts. Recompute before implementation; a mismatch requires refreshed mapping. Inventory generated at report completion. Framework files may change independently under their existing owner; hashes are not a lock.

```text
b8a268632d53a1f7de4c6ac59d3a236624665db676414415ee62ad288b6578f6  Cargo.toml
880d37dfe911935a7891eed1dea9f28cd2b70e30bb5637b88060477d14056993  apps/imager/Cargo.toml
a071fb9d887e9036bf1407816cd38421284ddb5e347a2d1bd25c7018777317c4  apps/imager/src/main.rs
5651e897a0cf2d46645b895b30d70534097248c76064c42d1bf2a29795158894  apps/imager/src/release.rs
2c010c1be378a214da137979956c5e0d7c7ce425d5807cb9d2300cfaf0d10238  apps/imager/src/setup/connection/endpoint/linux/ntfs.rs
d3a3837b0b05937c4c3e2c54134e4ecebbdbe6cecc1ae6823ff9681cc1c00b04  apps/imager/src/setup/connection/endpoint/linux.rs
24a14508642479ae1c44392755f8e0c021f9e35a38920feb1c047c493d9b9525  apps/imager/src/setup/connection/endpoint/unsupported.rs
643e417c392b377472163e6873ad016e1ea3f6f156c6ac20fdf409ed9a566a00  apps/imager/src/setup/connection/endpoint/windows.rs
b490924d29c80146e4cc221a4b5869983dbb5904ec734886c805b309d053b4b0  apps/imager/src/setup/connection/endpoint.rs
81f231ae685e2f2f94195df23051f3a51c649de06ded2bd2950dc5420393eb23  apps/imager/src/setup/connection/mod.rs
80388aa479228d22867f1186db0e9d2d94c76137858060e180400c7f1e566779  apps/imager/src/setup/connection/model.rs
d3551297d0e9ea774cc2aff76ea78536cec24cab6bc06f87bc6d8e0cef66510e  apps/imager/src/setup/connection/reconcile.rs
e448dce8557a29f8ba03301a6d7969e14661bd579e704731363394bd34666876  apps/imager/src/setup/connection/store/directory.rs
fbf2b07e2181b7a3fbbcaca58ab82b12cd4a1347df1a1a74dbe1c3bdf4c2e0cf  apps/imager/src/setup/connection/store/directory_tests.rs
b95ccf87768f57891b827cd88353f5cb4a3e2d1e4c75c68626072d989b1f2789  apps/imager/src/setup/connection/store/format.rs
19581bf06751785a226f4995b2a98251427aa612aeaddcc9deee43384a87e4b0  apps/imager/src/setup/connection/store/recovery/directory.rs
38ce4086bcaee9cec7e0e5538ad83486eedbed4b32e55213136834beb0d9694c  apps/imager/src/setup/connection/store/recovery.rs
92a0c4d5695b22c0ce984431a5b821c801a3952e4876882e207e689bf8577eb5  apps/imager/src/setup/connection/store/storage/windows.rs
9f47fbe9084a878827ec4cb72f3be64f8b0c096a6cf6f17ef9e675f31e8c1bf0  apps/imager/src/setup/connection/store/storage.rs
4caf23b975a5716e5e3d9751a9b86c75d16e5280ae1ea3b935697d20f490e0e9  apps/imager/src/setup/connection/store/tests.rs
f9f58e007aaa12af97f0831c7e16879bf7db2cb7dec13fea74106b5442fdbd66  apps/imager/src/setup/connection/store.rs
a98a7beb3ba4ca13366462fcfa0038860b6dae76a5746b839caaba72c72628b4  apps/imager/src/setup/connection/transaction/directory.rs
ec56daa1eb872a94772074ddc59c6dcc2076052378bb1465c50fc3638a37cdb5  apps/imager/src/setup/connection/transaction/native/descriptor/directory.rs
158f4f419c373787e06350ed84f202b5c0c3148d64b26523b6ca8438025122a0  apps/imager/src/setup/connection/transaction/native/descriptor.rs
73c58853baee714285bd91da7c38c44995955a10ff0743100240d865ab85836c  apps/imager/src/setup/connection/transaction/native/directory.rs
d25c276c67b58e934ac90ab6f2130a29df864a4b1d36dbd46131d8754e3e4bdd  apps/imager/src/setup/connection/transaction/native/linux/directory/tests.rs
c0601a93442bcda26c42893959e4fa4be533530941d99c4683c8b86b5da90100  apps/imager/src/setup/connection/transaction/native/linux/directory.rs
d3086710985bbd65afdda0765a590839f3c5411e91131ad669be15fcd7084fff  apps/imager/src/setup/connection/transaction/native/linux/lease_process.rs
c96feee4e0ba5d049cd669d652e3879b0abc77079b3406b92c7453703be711c5  apps/imager/src/setup/connection/transaction/native/linux/witness.rs
b5ba1f39e6d4a018ddfd5128a9e8d5146f68fc51836772bfa3a2b750e50d35b0  apps/imager/src/setup/connection/transaction/native/linux.rs
7e6693174c69a3ebe2162ed7295e2fe7c08c52c9e3732acc1a84a4ee0bcc123a  apps/imager/src/setup/connection/transaction/native/mod.rs
edacd53ce7e352a217c864a23cbee42409bc0a13318e01e90d884c41baaa0fbd  apps/imager/src/setup/connection/transaction/native/witness.rs
0ab48be2b6054bf71af21fdc0fc74f3a6a0293d045416df02b62bf864626fa4c  apps/imager/src/setup/connection/transaction/tests.rs
489dd0bcfa29678ab11a17fcd7704d112eff9c2c9d38605057f89523972ffe69  apps/imager/src/setup/connection/transaction.rs
e86478ae74c4e8aec7009e8419b0061945dc83517799e4530436bfb4634ed4a6  apps/imager/src/setup/connection/windows_native.rs
dd99b7b9889a2c9418f0912f730f639a755ca8e8f0e5843e3b27a931cb1c61e8  apps/imager/src/setup/controller.rs
8702fbfe7929e1e96253baa232af389546aec1472cf6aba0a6df4b078486d9e6  apps/imager/src/setup/desktop_continuation.rs
b0952c2d83ab3ea330f59b37502233c30c893f27aef010afa34ba17d68a37722  apps/imager/src/setup/destination_security/macos_acl.rs
5721734b264a3afefd6fc78293393e09d9204fba8100a4df9d61c2f84ecf924a  apps/imager/src/setup/destination_security/mod.rs
64182b55f6a52fd7eb7445296b64604153a3d4a0c90223a5d437cc5c56164fcb  apps/imager/src/setup/destination_security/unix.rs
cb7a5d739b9dcb2a2a0b435ee361aa03f0a88fcb9e3ebbbde799d8abb222dbf0  apps/imager/src/setup/destination_security/windows.rs
e038647b0b6e091db921d4c7c18258beb97d0ace4430392c505fca263a5cc987  apps/imager/src/setup/folders.rs
c2b39a610c204814bd2827fb661cd457194f21e035685c000b9bdafad5741bca  apps/imager/src/setup/import_view.rs
a9f8714b338a849268b3d55db414deeadd2b6fc309a8854bc162f7c2c55d9849  apps/imager/src/setup/internal_bundle.rs
c57079d3794fe555d8afe3daa203c3796066a4291dd76f33bc9540420a7814e6  apps/imager/src/setup/internal_cache/checkpoint.rs
f31be5b25c8780e944c84c82caff39d58d7d33c902846497afb1f9d7686037e3  apps/imager/src/setup/internal_cache/coordinator/tests.rs
79de5d11a5f49ea463e032e55b01e2086ddd9337fe8bb9431d626498f3f4bb05  apps/imager/src/setup/internal_cache/coordinator.rs
4fc8c70539685cef47467cb24c3fca4af747533e16ee38a922c2fc73f1883faf  apps/imager/src/setup/internal_cache/linux/tests.rs
2884d0a794c76437a60ba13bbdd9546d3a7e186db813d80befaae60f10bff07a  apps/imager/src/setup/internal_cache/linux.rs
f1ac8b0acd595475ba49547d66d4dcc2f7b7084a4191051dae06b449511663c4  apps/imager/src/setup/internal_cache/tests.rs
1fa2937618dbd35e8b2f1546bb62caf292ce89a00882a7652acbfd15708953b5  apps/imager/src/setup/internal_cache/windows/tests.rs
a800f5f5ec58e535b672fa9a90f0481182327d6d2478eaddec16d31c8cc450f6  apps/imager/src/setup/internal_cache/windows.rs
c18f0ba0f193adbe3173e3bc2cab114a34f338058fd5d0edbd68875cf37e8ce3  apps/imager/src/setup/internal_cache.rs
32aaf6328302d74bffa36b2d4bb8e701a7818f198bbd79046b8a0b9efacb328b  apps/imager/src/setup/internal_download.rs
202512b9b4568a056e608f33de9b263b1da1903b79d49551fe67fb7c12495621  apps/imager/src/setup/internal_download_flow.rs
feeacde3a33eb7cfe14d94a87ec2e2ed2225c85856c891316c82d4ed58a2cea8  apps/imager/src/setup/migration.rs
f6a92aca885e1f7a3ae047515238bdf0a89ea37ca9fe9f6c82c16db9885750c0  apps/imager/src/setup/migration_durability.rs
07eb2adf07fc822db40ca698f0c4062bc142b4515da9b48b089cb6d2c9ce1be8  apps/imager/src/setup/migration_journal.rs
eb2d1079d1566cdec9c18089a6e2b5b236249028b129d633209bdce06f9b41c9  apps/imager/src/setup/migration_journal_log.rs
0e08833cfe9c43f7dc0f758bf1df65c3c70e9e976fcea004f74dcf035857a99f  apps/imager/src/setup/migration_native.rs
3aba01c70215d368cf29642a96987d9c085e7f1f3e4668cb9fb640f296999b67  apps/imager/src/setup/migration_native_journal.rs
29f3ec6ed0bd21ba9e5d9cfd8558f5c6af48ca07de65be6685531099a8e61654  apps/imager/src/setup/migration_review.rs
8e54bea34bb3717bf4c5dbd80a91bb0384fdde22a806ae7853b515551ff2e0d6  apps/imager/src/setup/migration_source.rs
ff7809b0bcbea7c46d40b375595a603fa3a3788f98226abee2d6fc6215922ba0  apps/imager/src/setup/migration_store.rs
265d1c4372f0a7eac1a3f099e1cb1a4003d5b4b104b239594cff40c23bcf6e5d  apps/imager/src/setup/migration_worker.rs
272ccd67395150d189611dbf23e249fa4dad131d7757c628e5c6d0d39d21a470  apps/imager/src/setup/mod.rs
7b6dd829edf3094f3769557d8e5cac83fd3a3f830c795691629b3e4faf51e9e1  apps/imager/src/setup/nofollow.rs
742e3bcf97094cabbbaff8a44c9af4552842c576d81662fb058d6c36c87f15f3  apps/imager/src/setup/view.rs
1b2daf80386032b32bf5a91de737712defaaefb0aaa39b2c24d5470e34d6b8e3  crates/app/tilo-app/Cargo.toml
76ecaaad07ee0fd90dbf67d702851edc765a2b8f0eea6f0011aa5feb31b82477  crates/app/tilo-app/src/lib.rs
c881a3eeb3255cefc0850d557a0edd93d7015efe2a6785e8c88ac904a9a811d0  crates/runtime/runtime-fs/Cargo.toml
d3e261ecef11bf59f4f199ca3a8ad1fd0bbeaac67a6de2e540a22cee0b554a78  crates/runtime/runtime-fs/src/grants.rs
cf5fca0154c2a618b14a14a2709f405626710dcf06b043cf5c29f97e884a6caa  crates/runtime/runtime-fs/src/lib.rs
212d75aa6bbd4c0a226185024478125148dc44894b87e60e6d8832d603e90ba0  crates/session/qos-ntfs-helper/Cargo.toml
27b2d0a434fe75caa27a556e01241cd63b15257829b3e28b988027183a89593f  crates/session/qos-ntfs-helper/README.md
461114e9f0e9825e3ccd954cf26278e7faac60370a107e7b927735413b80335b  crates/session/qos-ntfs-helper/src/broker/ipc.rs
c127775074877d204ebfdfc5d61e7a89c6d3c2f72526f283023c28c7ac417f02  crates/session/qos-ntfs-helper/src/broker/linux.rs
9812748f854679723bf92035fdf0ebbbe40a916b07d76541aba3a2c134ae4549  crates/session/qos-ntfs-helper/src/broker/session.rs
9444d1e3f2e472d37d43db2de85f58c494d084aeb93403149f28950045022ef8  crates/session/qos-ntfs-helper/stage_install.py
d534b10a8b93a48d84b83ce701c20ef772b5188b8b90aeb2f8a592a895eeb761  crates/session/qos-ntfs-helper/tests/native_copy_coordinator.py
5aa6e92820632bfcff9baa21ef58d9d2d615efe18334edc295d626eafd618c8a  crates/session/qos-ntfs-helper/tests/native_session_coordinator.py
1e79dda4adea5da60e7c31dbb3892e91d54adcd27728b97426de3c0a9e684d49  crates/session/qos-ntfs-protocol/Cargo.toml
57bae0fac92f0aa82929296b14f04dab22e106b5476af57dadf345fe5f435f26  crates/session/qos-ntfs-protocol/src/client.rs
3fae6e983bab4b605a4d5adc960ff87bedb796cb2b90baccccc314bf8fac68b8  crates/session/qos-ntfs-protocol/src/socket.rs
0bdb0e222a2c7f68c4ea7ed52d1d2fbc9bf138b3c91f1645c7695144a161406b  crates/system/atomic-file/src/lib.rs
3c3e9151d6093695ba077988cde26373028db608ca78752611588954f106e9fe  crates/system/bounded-command/src/lib.rs
97449f0c158b23006c188871e16439b0b649e4d1821cd72abf645c00ac07815f  docs/audits/platform-extraction-2026-09-18/05-imager.md
35ec1840fef12d8331c2da305b53cdf86cf936e4eb8132fbc95b719924f8bad2  docs/superpowers/plans/2026-09-18-cross-platform-framework/README.md
2b78a7c7789a1d72f290442650750f5a53e58a991849f15f6f8bfaad18b5eabf  docs/superpowers/plans/2026-09-18-cross-platform-framework/framework-contract.md
28d0fae06ee55822bd99d1e58503d038eab8df68ba12ea2cd1c7315995566b56  tools/app_platform_baseline.json
66b1e38d86319fa30bad4e8720204cdde32168ae5c7d3b1383c037ba0e0be6a4  crates/runtime/runtime-connected/Cargo.toml
80ae4b029ff6887b6dd2fe336ce088a338b62400ef1be163d839c0845d342891  crates/runtime/runtime-connected/src/exchange.rs
5a8576ed0d13110d7cd4260bf2b38c0fee24fee7fe679191cc9c724045fd6e62  crates/runtime/runtime-connected/src/lib.rs
4bc67a2a7c762a94cca4305de7757c4ccc5009a082fdf4f5971bd0a6ee171f1c  crates/runtime/runtime-connected/tests/exchange.rs
570863cfd3883c3df5ea856920c89858a211546065d23e90f56cb8f8b27e131e  crates/session/tilo-connected-protocol/Cargo.toml
ddf161262a8d430fb3fce084427f22022e51d592b0e14749ec7ee93d29ed22d7  crates/session/tilo-connected-protocol/src/codec.rs
ac7dea0965336259886988723879004a4890d4323f352c0530dc1bc261de2599  crates/session/tilo-connected-protocol/src/envelope.rs
50468d0b51f6a6721538b67bcf8dc90c40c4ffa6602e3ffb14a491593138e4af  crates/session/tilo-connected-protocol/src/lib.rs
31cad29c6e6f2c2428766b510ddbbca5b3528008fa38be5ab24dd6a61ccfc7f1  crates/session/tilo-connected-protocol/src/native_string.rs
8491ed0ab08eddbad40fac968c6da453615fdfa31bfea896681e5a2438104dd6  crates/session/tilo-connected-protocol/tests/codec.rs
```
