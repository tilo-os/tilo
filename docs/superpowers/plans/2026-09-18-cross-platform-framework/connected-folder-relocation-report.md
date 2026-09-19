# Task 2 relocation report — frozen for independent review

## Scope and ownership

This uncommitted WIP unit physically relocates the complete 46-file Imager connected-folder/native copy closure to `crates/system/folder-service`. No source is staged or committed. No atomic-file, app-state, runtime-connected, protocol, native wire/schema, canonical namespace, or production authorization algorithm was changed. Original inputs are the accepted shared WIP bytes, not reconstructed HEAD versions; `task-2-before/`, `task-2-before-sha256.json`, and `before-source.tar` retain them.

`relocation-before.json` records all 46 source hashes, logical module paths, test/ignore attributes, and literal old filter strings. `normalization-rules.json` records actual per-file rewrites. Mechanical normalization permits only the three specified root paths and excludes every `pub(...)` visibility token; all visibility/cfg/test/constant/code-motion changes remain in `relocation-residual.diff`. There are 29 byte-equal normalized files and 17 residual files. `semantic.diff` adds complete new sources and complete diffs for partial files, callers, SDK, manifests, baseline and workflow. `git-diff-find-renames.patch` is supplemental, not the proof of relocation.

`handoff-paths.json` contains 113 explicit source/destination/caller paths. `snapshot.tar` includes every present path and `relocation-after.json` records SHA-256 hashes and deleted-source tombstones. `baseline.json` lists unrelated shared WIP separately. The final freeze runs `prepare-handoff.py`; independently rehash `relocation-after.json` at ownership transfer. Any mismatch reopens affected validation.

## Module and API mapping

- `crate::setup::connection` → private `crate::connection`; all native endpoints, transactions, restore/proof construction, handles and stores remain behind this private module.
- `crate::setup::migration*`, `destination_security`, `nofollow` → private same-named service root siblings. Original embedded tests move with every file.
- `migration::{directory_identity,path_is_absent,CopyReview,inspect_copy_review}` and `migration_store::prepare_managed_import_parent` → public semantic `review`; `Identity` is re-exported only as `review::DirectoryIdentity` with private fields. Both independent absolute/no-traversal predicates remain intact.
- The existing scanner DTOs/implementation and five scanner tests move from `setup/folders.rs` to `scan.rs`; Imager retains picker code, its stdout test, and the permitted named scanner re-export.
- Both OS-selected readiness implementations and the Linux authenticated peer/connect helpers plus two Linux tests move from continuation into `session.rs`. Continuation retains the old restricted name as a semantic readiness re-export. Compile-time `QOS_DESKTOP_TRUSTED_SHELL_PATH` is unchanged; Nix `preBuild` exports it for the whole Cargo compilation.
- `copy.rs` exports exactly `CopyProgress`, `CopyReport`, `IssuePage`, `MigrationStore`, `NativePath`, `OperationRecord`, `OperationState`, `StoreWriter`, `MigrationJob`, `MigrationWorker`, `WorkerEvent`, `WorkerOutcome`, `durable_copy_available`. Only the contract-listed methods and existing DTO fields are promoted. Jobs/stores/workers and native identity fields remain private.
- The SDK `folder-copy` feature publishes only `{copy,review,scan,session,startup}` under `ui::folders`; existing SDK exports are preserved. No broad facade, app dependency, raw handle bridge or native endpoint export was introduced.
- Imager calls `startup::dispatch_early` immediately after collecting arguments, replacing the two previous Linux-only dispatch blocks. The Linux flags/current-executable self-reexecution remain unchanged. `early-host` demonstrates the documented before-runtime/thread/UI composition contract.
- The service's empty `test-support` feature exposes only save hold/snapshot hooks, direct reviewed-copy regression wrapper and resume operation. Imager enables it as a dev dependency only. Normal-build public APIs do not expose test hooks. Existing `MigrationWorker::start` remains public for production shots.

## Literal subprocess filters

The five full names change only by deleting the `setup::` prefix:

- `connection::store::tests::two_processes_cannot_own_store`
- `connection::store::tests::killed_owner_leaves_durable_pending_intent`
- `connection::store::storage::windows::tests::native_windows_lock_child`
- `connection::transaction::native::linux::tests::native_executor_process_restart_boundaries`
- `connection::transaction::native::linux::lease_process::tests::native_isolated_receiver_regressions`

No `setup::` occurrence remains in the service. Migration journal/store helpers already compute their filter from `module_path!()` and need no literal edits. The external native-copy coordinator accepts a caller-supplied `--native-test`; it does not discover or hardcode Imager's executable, so its source is unchanged. A future authorized invocation must supply the **service** unit-test executable. Its `native_selected_copy_command_worker` substring is unchanged. No coordinator/device/native ignored test was run.

## Dependency and platform-checker changes

`dependency-edges.diff` compares Cargo.lock to the actual Task 2 WIP baseline: Imager removes `qos-ntfs-protocol`, adds dev `tilo-folder-service`; `tilo-app` adds optional `tilo-folder-service`; the new service depends on existing cap-fs-ext/cap-primitives/cap-std 3.4.6, dirs, libc, qos-ntfs-protocol, serde, serde_json, sha2, tempfile(dev), windows-sys 0.61.2 with the existing feature set. No version, registry, checksum or unrelated edge changes. Imager retains cap dependencies because internal_bundle and desktop_download still use them.

`service-tree.txt` records the normal headless graph, with no ui/winit/vello/app/native parser/helper/reqwest edge. Both Linux and non-Linux Imager SDK declarations enable `folder-copy`, preserving portal and unrelated features.

444 exact app platform baseline entries were removed or shrunk only at changed Imager sites (`retired-platform-entries.json`); none were added/increased and unrelated allowances remain unchanged. The first checker run caught one unnecessary extra import cfg guard; combining the same guarded imports restored the existing count. Final platform checker passes and all 32 checker unit tests pass. Release workflow adds service lib/docs and SDK folder-copy docs beside each existing Linux/macOS/Windows native test lane. No release/sign/package operation was run.

## Validation and limits

Final validation results are recorded below. `validation-results.json`, `service-validation-results.json`, compiler artifact JSONL, direct probe diagnostics, and named command logs contain actual output. Native platform labels are explicit; cross-target `cargo check` is not native execution.

Completed: macOS service all-targets; Linux aarch64 and Windows x86_64 all-targets cross-checks; model9/reconcile26/scan5 tests; service docs4; headless example build; normal and test-support service privacy probes. Source inventory preserves all405 full-file test annotations and every existing ignored reason. Host executable inventory has316 tests, with3 pre-existing ignored tests explicitly listed. Linux-only native tests remain present but cannot be qualified by a macOS run.

The first SDK-doc and Imager continuation attempts were blocked by contemporaneous unrelated history/UI writer compile errors in action_palette.rs (E0432/E0599), after the first Imager tests check had passed. The final controlled reruns below passed. The initial direct rustc probe correctly rejected E0463 from Cargo's top-level artifact alias; the probe was corrected to choose the immutable dependency `.rlib` paired with the exact JSON-emitted `.rmeta`, preventing feature-build alias overwrite. Feature claims come only from Cargo artifact messages. Positive controls and required privacy diagnostic codes are mandatory; arbitrary compiler failure is never accepted.

This is an ownership-transfer increment, not full N03 or cross-platform native qualification. Required Linux/Windows fixture, native receiver/confinement, actual NTFS diagnostic/coordinator and distribution evidence remain open. No ignored annotation/assertion/fixture gate was added or weakened. G02, S01/S02, I01, C01, N02, N04 and host/distribution gates remain open as specified in the binding contract.


## Final controlled validation

All extraction product paths remain byte-identical to the reviewed snapshot inputs used by the service checks. The finite UI hold additionally recorded 434 source inputs, including every `ui/src` Rust file and `ui/Cargo.toml`, with identical start/end/final hashes. `ui-five-source-hold.json` records the parent-coordinated hold; `frozen-sdk-start-hashes.json`, `frozen-sdk-end-hashes.json`, and `frozen-terminal-input-hashes.json` establish no source drift through the final SDK probes, Imager test-target check and both runtime regression groups.

| Check | Target / result |
| --- | --- |
| service `cargo check --all-targets --offline --locked` | native aarch64-apple-darwin PASS |
| service all-targets cross-check | aarch64-unknown-linux-gnu PASS, compile only |
| service all-targets cross-check | x86_64-pc-windows-msvc PASS, compile only |
| service model / reconcile / scan | native macOS 9 / 26 / 5 passed |
| service doctests | native macOS 4 passed, positive + three privacy failures |
| SDK `--doc --features folder-copy` | native macOS 3 passed, positive + two privacy failures |
| service library test inventory / ignored inventory | 316 / 3 nonzero names; original ignore annotations unchanged |
| service `early-host` build | native macOS PASS; no host execution |
| Imager binary build | native macOS PASS; no GUI/device execution |
| final Imager `cargo check --tests` | native macOS PASS under frozen UI inputs |
| final continuation regression group | native macOS 10 passed, 0 ignored |
| final import-view regression group | native macOS 15 passed, 0 ignored |
| normal + support direct consumer probes | 14/14 expectations PASS, four positive compilations, ten intended privacy failures |
| checker / checker tests | PASS, 55 apps / 32 tests |
| pulled-forward Task 3 native service library suite | native macOS 313 passed, 0 failed, 3 existing ignored, 241.65 seconds |

The native service suite is separately authorized, pulled-forward **macOS** evidence in `task3-macos-service-evidence.json`, tied to source and log hashes. It exercised the actual same-path store/process lock and journal subprocess regressions. It did not run the three ignored root/image fixture tests. Linux/Windows cross-checks do not qualify their native behavior.

The SDK positive-control E0463 during concurrent UI editing was diagnosed rather than waived: `sdk-locator.log` records the SDK expecting UI metadata hash `14768d2e8a15befa3b30772c2bf6c4da` while the replaced dependency artifact had `89d5beb11c9bc2fd7eb00c30635d8ab4`. The controlled sequence rebuilt each SDK configuration and immediately ran its probes before the next build. `frozen-sdk-results.json` and `privacy-results.json` now record all intended successes with unchanged inputs. Negative diagnostics are actual E0603 (private connection/pin), E0451 (identity fields), E0433 (SDK native module absent), and E0616 (store root private), never E0463. The connection probe also reports E0432 for the unavailable `NativeRestore` name, but its independent E0603 diagnoses the private connection module, and positive imports compile. Normal SDK dependency artifact features exclude service test-support; support SDK dependency features include it; the SDK's unrelated test-support feature stays disabled. JSON feature receipts are saved beside each artifact log.

`split-test-proof.json` verifies the five scanner and two readiness test bodies are byte-identical across their old and new files. `moved-test-inventory.json` verifies all 405 full-file test annotations and identical ignore text. No unobserved native check is described as passing.

The writer is frozen for independent specification and code-quality review. Parent owns final shell gate and ownership transfer. First review fixes remain with this implementer; any product change requires refreshed affected checks and snapshot hashes. There are no remaining live build/test handles.
