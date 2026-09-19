# Task 1 extraction report — implementation frozen for review

Task 1 Parts A/B are one implementation/review unit. Task 2 preference storage and Task 3 qualification/baseline retirement are not implemented here.

## Input identity and ownership

The 25-file shared-WIP baseline in task-1-before-sha256.json was independently rehashed twice, with zero drift, including immediately before authorized existing-file changes. HEAD is not the input baseline. task-1-inventory-before.json records 101 cache, 49 transport and 26 bundle test attributes and all five cache ignore strings. task-1-lock-before.json records parsed package identities and dependency edges. task-1-moves.json records the exact file relocation map. The authoritative catalog bytes retain SHA256 `3aa2c12d568e82557b278a17f9488f55f45e16f40e0c0e3159b369bad44e9f41`.

No folder-service or ui source was edited. The independently authorized folder-service peer and History UI changes are separate inputs. task-1-folder-service-before-build.json records folder-service source identities without copying its implementation. Parent owns peer receipts and coordinated Cargo lanes. Actual build checkpoints and final artifacts are bound by the receipts and manifests listed below.

## Implementation residuals

- Metadata/parser/private InternalRelease and catalog tests split from release.rs; Imager retains media projection/downloading and the existing private validate_digest helper required by that downloader. Plain catalog metadata has Clone/Eq and public data fields, but selections have no public builder/Deserialize/URL getter.
- Bundle algorithms and 26 tests retained; pure synthetic manifest helpers move to the restricted test-support module. Native verifier stays in private bundle; format exports metadata/parsing only.
- Cache/transport module roots change explicitly. `pub(in crate::setup)` becomes private service crate visibility; this visibility change is NOT normalized away.
- Shared DTOs and one NEXT_OPERATION move to download.rs. Coordinator is instantiated by explicit native/fixture wrappers. Ordinary-file provider and interleaved FreshDisk/DiskProvider helpers move to fixture_platform.rs/fixture_storage.rs; all cache test bodies remain #[test], and no fixture is selected by normal construction.
- Public Error has only its specified getters and Display/Error. Private Failure(Error) retains internal diagnostic From<String/&str> conversions; parent explicitly approved this implementation detail. Native-origin category constructors preserve messages. Held-reader postchecks all execute and concatenate all diagnostics, retaining uncertainty.
- App flow/view use semantic Client/Token/Phase/Status; full-OS review guards retained. App preference I/O/schema remains unchanged until Task 2. Dev Server supplies explicit fixture clients with opaque fields; removed callbacks are replaced by zero-consumption assertions.
- Exactly four workflow catalog paths move. Catalog wire/asset naming unchanged. App native dependencies remain because desktop_download and media/host code still use them (task-1-app-native-residuals.txt).

## Commands and current results

1. Initial `cargo test -p tilo-install-artifacts --lib catalog --offline --locked`: terminal 101, workspace membership placement typo. This was corrected; it is NOT a behavioral RED result.
2. `cargo metadata --offline --locked --no-deps --format-version 1`: terminal 0, task-1-metadata-checkpoint.json. Parent independently confirmed no existing package identity/version/checksum changes; only the new local package and Imager/SDK dependency edges.
3. `python3 -m unittest discover -s apps/imager/packaging -p test_release_catalog.py`: terminal 0, 9 passed, task-1-python.log.
4. Explicit-file `rustfmt --edition 2024 --config skip_children=true --emit stdout` syntax reads: 27 files parse; no formatter writes.

## Open gates

Portable catalog (13), host bundle (24) and catalog surface (2) tests passed; lib-test builds had 4 warnings and catalog_surface normal-library build had 10 warnings. These commands/log hashes are bound in task-1-portable-checkpoint.json. Full cache/transport/facade checks have passed as detailed below; app verification passed in the coherent shared-input window recorded below. Source-only test count preservation is not test execution. Current host macOS has no production persistent internal cache implementation; fixture-worker results will not certify native macOS support. Native Linux/Windows tests, ignored external Windows ACL/quota provisioning, cross-target evidence, signed distribution evidence, G02 HostContext integration, L02 migration, S01/S02 consolidation, N02 completion, Task 2 persistence and Task 3 privacy/package qualification remain explicit gates.


## Full-service first compile/checkpoint

- task-1-cache-1.log: terminal 101 before tests, six module-root residual errors. Corrected two mechanical `super::crate` imports and explicitly bound the shared driver test module to `coordinator/tests.rs` (the wrapper `#[path]` otherwise found cache/tests.rs).
- task-1-cache-2.log: terminal 101 before tests, six fixture alias/TcpListener/private-Server-field residual errors; corrected imports and replaced field references with read-only requests().
- task-1-cache-3.log: compiled; 33 passed and 27 failed only at localhost TcpListener::bind with OS PermissionDenied/Operation not permitted. This is sandbox environment refusal, NOT algorithm failure or TDD behavioral RED. The exact escalated rerun passed (cache-4 below); no fixture fallback, weakened assertion or ignore added.
- Parent approved the source/design cfg correction: one private HTTP Server implementation remains cross-platform for original Windows unit transport tests. Public Server alias and fixture client are Unix-only; Windows unit-only alias is pub(crate). Exactly these Imager view tests gain Unix cfg because their prior Worker fixture constructor was Unix-only: `full_os_download_command_routes_to_the_real_internal_worker_only`, `full_os_download_command_is_refused_outside_full_os_review`, `duplicate_full_os_start_command_is_rejected_without_an_extra_request`. Their test attributes/bodies remain; all 28 view and 16 flow source test attributes/names are retained.
- The original app Worker fixture constructor installed a worker-thread 1024-byte checkpoint interval. Explicit Server::client retains that override through test_support::with_fixture_interval; production construction never calls that setter and retains the production interval. This source distinction does not qualify native host behavior.

## Full-service successful checkpoints

All commands below used `--offline --locked`; the exact command, source inputs before/after, terminal result, log SHA256 and warning summaries are bound in corresponding `*-receipt.json`. The owned/config hash maps have zero drift during each command. Folder-service peer hashes are recorded separately and are not part of this headless service's causal dependency closure.

| Check | Log stem | Result | Warnings |
| --- | --- | --- | --- |
| `cargo test -p tilo-install-artifacts --lib cache` (scoped localhost escalation) | task-1-cache-4 | 60 passed, 0 failed/ignored | 18 lib-test |
| `cargo test -p tilo-install-artifacts --lib transport` (scoped localhost escalation) | task-1-transport | 49 passed | 18 lib-test |
| `cargo test -p tilo-install-artifacts --test api_surface` | task-1-api-surface | 2 passed | 89 normal-lib |
| `cargo test -p tilo-install-artifacts --test api_surface --features test-support` (scoped localhost escalation) | task-1-api-support | 3 passed | 21 lib |
| `cargo check -p tilo-install-artifacts --all-targets --features test-support` | task-1-service-alltargets | terminal 0 | 21 lib; 10 lib-test, 4 duplicates |
| `cargo test -p tilo-install-artifacts --lib error::tests` | task-1-error-unit | 1 passed | 12 lib-test |
| `cargo test -p tilo-install-artifacts --lib facade_tests` (scoped localhost escalation) | task-1-private-facade | 1 passed | 12 lib-test |

The 89 warnings are retained unsupported-host/dead-code/unused diagnostics, not a warning-free build claim. No blanket allow attributes were added to suppress them. MacOS fixture/common-protocol tests do not run Linux/Windows persistent cache providers.

After the API checkpoint, `src/lib.rs` and `src/cache/coordinator.rs` changed intentionally: both native construction and the explicit fixture callback now call the same private `refuse_protected_staging` function. The new private regression `facade_tests::private_consumer_refuses_protected_staging_and_retains_finals` drives that callback through the actual worker, asserts Failed and the exact original refusal diagnostic, both finals/residue, exactly one internal consumption, and preserved bytes after close. No public consume method was added. The all-target check plus focused regression cover this residual; prior cache/transport passes remain evidence for their exact recorded checkpoint and were not redundantly repeated. The error unit test confirms structural native error-kind classification and unchanged diagnostics.

Live handles at this service checkpoint: none. Drained terminal handles include 97979 (cache), 64083 (transport), 84441 (support API), 25558 (all-targets), and 40514 (error unit); private facade completed synchronously. Imager/SDK integration subsequently passed as recorded below; independent parent specification/quality review remains the next gate.

## Review artifact map and preserved test identity

- `task-1-current-manifest.json`: exact 53 owned product paths, baseline/current SHA256 per path, null before for new files and null after for deletion tombstones.
- `task-1-current.patch`: complete diff against the accepted 25-file shared-WIP snapshot, including new untracked source and deleted implementation files. This is not a HEAD-relative diff.
- `task-1-source.tar.gz`: all current owned product files plus snapshot-manifest.json; no UI or folder-service peer implementation is copied.
- `task-1-moves.json`, `task-1-normalization.json`, `task-1-normalized-residual.patch`: twelve original full-file comparisons. Normalization changes only exact module-root paths on eligible lines, excluding visibility, cfg/test/ignore attributes, constants/statics and literal-bearing lines. **No entire destination is normalized-equal**: every residual remains visible. Split destinations account for moved DTOs, fixture providers/storage, private test server and catalog. No algorithm preservation is inferred merely from a rename.
- `task-1-body-identity.json`: 439 of 544 original function bodies byte-identical at a named service destination. Remaining 105 bodies are not claimed equal; explicit imports, typed diagnostics, fixture accessor conversions and facade/lifecycle wiring appear in the residual diff. The function matcher does not normalize bodies and does not hide signature/visibility/cfg differences.
- `task-1-inventory-before.json` and `task-1-inventory-after.json`: exact 176 original test names retained: 101 cache, 49 transport, 26 bundle. Five original ignore attributes/reasons remain unchanged. MacOS selected only 60 cache and 24 bundle tests; counts of source attributes are not execution claims.
- `task-1-dependency-changes.json`: parsed lock comparison proves all old package identities, versions and checksums unchanged; one new local package, plus exactly one new dependency edge in each of Imager and tilo-app. Root workspace membership and the optional SDK feature are visible in the source diff. Service dependencies contain no app/UI/rendering edge; libc is Unix-scoped and cap/windows-sys are Windows-scoped. Other Imager native consumers retain required native dependencies.
- `task-1-packaging-identity.json`: exact catalog byte equality and the workflow's exact four path-only substitutions; downloaded asset name/directories and generator wire format unchanged.

Literal child-process filters are now exactly `cache::linux::tests::native::lock_probe` and `cache::windows::tests::native::lock_probe`. Both parent tests inspect captured stdout for `running 1 test` and the exact filter name before their original exit-status assertions. Existing `--ignored` use remains only for the previously ignored child fixture. The MacOS run does not execute those native subprocess tests, so their real execution remains a host-specific gate.

Split mapping: catalog metadata/parser moves from Imager release.rs to catalog_data.rs, private authenticated selection logic to catalog.rs, and catalog tests follow it. Media URL/download behavior remains in release.rs. Bundle's synthetic manifest constructors move to test_support.rs. Coordinator DTOs/cancellation/global token allocator move to download.rs, the old coordinator fixture provider block to cache/fixture_platform.rs, and original cache test storage helpers to cache/fixture_storage.rs. The helper module compiles only for tests or test-support; original #[test] functions remain in test-only modules. The single coordinator algorithm is instantiated by native_coordinator.rs and fixture_coordinator.rs. Private Server's portable implementation supplies original Windows transport unit tests, while its public alias/client stays Unix-only.

## Additional cross-target attempts

`rustup target list --installed` confirmed `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-gnu` and `x86_64-pc-windows-msvc` alongside the host/wasm targets. The following all-target/test-support checks used existing offline dependencies/toolchain without environment overrides or provisioning:

- `cargo check -p tilo-install-artifacts --all-targets --features test-support --target aarch64-unknown-linux-gnu --offline --locked`: handle 89979 terminal 101; cc-rs cannot find aarch64-linux-gnu-gcc.
- Same command with `--target x86_64-pc-windows-gnu`: handle 46958 terminal 101; cc-rs cannot find x86_64-w64-mingw32-gcc.
- Same command with `--target x86_64-pc-windows-msvc`: handle 24702 terminal 101; ring 0.17.14's native C build cannot find Windows-target assert.h.

All stopped in dependencies before compiling this service. No fake headers, host fallback, replacement dependency, compiler installation or target override was used. DEVELOPMENT.md has no configured Linux cross-C/sysroot for this machine; the public execution history records the same Linux cross-compiler limitation. Exact commands/log hashes and zero owned/config drift are in task-1-cross-linux-receipt.json, task-1-cross-windows-receipt.json and task-1-cross-windows-msvc-receipt.json. These attempts provide no cross-target compile pass and no native runtime qualification. All handles are terminal.

The app integration receipt preparation resolves a conservative 51-local-crate closure: every target/optional/dev/build path edge recursively, all source/assets under each crate, literal external Rust include paths, and root Cargo/lock/toolchain/.cargo configuration. The prepared map has 1329 files; it was recaptured at the coherent app window. It is broader than a selected feature graph and does not rely on historical peer receipts.

## Migrated app integration and final freeze

Parent coordinated finite holds for the current UI closure and peer folder-service source. Each app receipt records the independently captured conservative 51-crate/1329-file closure, before/after receipt SHA256, actual command/result and raw log SHA256. Every app check's before/after map is equal. No prior peer/shell pass is used as proof of this extraction.

- First `cargo check -p imager --tests --offline --locked`, handle 38733: terminal 101, exactly two E0382 compile errors in migrated flow tests. Their zero-consumption assertions accessed Server after drop(server). This is a compile residual, not behavioral RED. Parent approved moving each assertion immediately after drop(flow), which joins the worker, and before drop(server). Both reopened-server assertions remain. Failed checkpoint: task-1-imager-check-receipt.json.
- Retry same command, handle 42570: terminal 0, task-1-imager-check-2.log/receipt. This compiles SDK integration, normal Imager code and test targets. One Imager lib-test unused Worker alias warning remains, plus dependency warnings fully retained in the log/receipt. No source-wide formatter or warning suppression was run.
- `cargo test -p imager --lib internal_download --offline --locked -- --list`, handle 87706: terminal 0, exactly 16 names, every name in setup::internal_download_flow::tests. task-1-imager-flow-selection.json proves both requested substring filters select this identical set, so the union was executed once per parent ruling.
- `cargo test -p imager --lib internal_download --offline --locked`: terminal 0, 16 passed, 0 failed/ignored. Names include real held-ready retention, pause/reconstruct with a new token, duplicate start, cancellation/discard, stale-start refusal, dropping during authentication and held-ready shutdown. task-1-imager-flow.log/receipt.
- `cargo test -p imager --lib setup::view::tests::full_os_download_command_routes_to_the_real_internal_worker_only --offline --locked -- --exact`: terminal 0, 1 passed, task-1-imager-view-route.log/receipt.
- Same exact command form for `full_os_download_command_is_refused_outside_full_os_review`: terminal 0, 1 passed, task-1-imager-view-refuse.log/receipt.
- Same exact command form for `duplicate_full_os_start_command_is_rejected_without_an_extra_request`: terminal 0, 1 passed, task-1-imager-view-duplicate.log/receipt.
- `cargo test -p imager --lib release::tests --offline --locked`: terminal 0, 21 passed, task-1-imager-release.log/receipt. Media projection, catalog parsing, profile/provenance validation and range/resume behavior remain covered.

All five behavioral app commands used exact scoped localhost sandbox escalation, not network access to external services or fixture weakening. They completed synchronously without outstanding handles. The successful flow/view/release logs retain the single Imager unused import warning; dependency warning counts remain per-compilation in each receipt, not falsely collapsed to a single task-wide number.

Final evidence totals: 155 successful service Rust test invocations across the portable/full checkpoints; 40 successful Imager test invocations; 9 Python packaging tests. The all-targets service check and Imager test-target check also passed. Earlier compile/environment failures remain separately preserved above. These totals do not imply additional native-host execution.

Product source is frozen at `task-1-final-frozen-inputs.json` and `task-1-current-manifest.json`. `task-1-source.tar.gz` contains 41 current product files plus embedded snapshot-manifest.json; the manifest records 12 deletion tombstones, 53 total paths. Final source archive, complete snapshot-relative diff, normalization/residual, inventories, receipts and report hashes are indexed in `task-1-artifact-sha256.json`. Parent may now perform independent specification/quality review of cohesive Parts A/B and the shared shell integration check. No staging, commit, stash, cleanup, CARGO_TARGET_DIR, package installation or peer source edit was performed. Live tool/process handles: **none**.

Status: **DONE_WITH_CONCERNS for Task 1 implementation; independent review pending.** Concerns are the recorded warning debt and unqualified native/cross-target environments, not hidden passing claims. Task 2 preference persistence and Task 3 baseline retirement/privacy/package qualification remain unimplemented. G02 HostContext binding/legacy entrance removal, L02 root migration, L03 emulated-host/native evidence, S01/S02 consolidation, N02 native completion, signed/installed distribution and native Linux/Windows cache/subprocess/ACL/quota execution remain open. MacOS normal construction remains Unsupported by design; explicit fixture successes do not implement its persistent cache.

## Parent-owned shared shell checkpoint

After the successful app suite, parent ran the required shared `cargo check -p shell --tests` checkpoint. Parent reports handle 71659 terminal 0, 21.16s, 3531 input identities unchanged, no live handles; 60 shell lib warnings and 36 lib-test warnings remain. Neutral evidence is `/private/tmp/tilo-private-cache-task1-shell-checkpoint`; raw log SHA256 `1b69ee19a933f845b759084f61ae8cf361750946209ca31a2c43d928885a3dad`. Exact command/environment and input maps are owned by that parent receipt; this report does not relabel it as a locally run command or native/distribution qualification. Peer/UI holds were released afterward; this task's product source remains frozen for review.


---

# Task 1 fix round 1 — frozen for scoped rereview

Addressed Important findings 1 and 2 and the small unused-import finding from task-1-review.md. No other product scope changed. Status: DONE_WITH_CONCERNS, awaiting independent scoped rereview; native compilation/runtime evidence remains limited as below.

## Exact changes

Five paths changed relative to the reviewed 53-path product snapshot:

- `crates/packaging/install-artifacts/src/cache/linux.rs`: the journal and final publication error closures now explicitly accept `crate::error::Failure`, retain the original diagnostic strings, and return the original typed error with `with_message(message).with_uncertainty()` in the uncertain branch. The other branch still returns `e` directly. Journal provider uncertainty state assignments and native operation/check order are unchanged.
- `crates/packaging/install-artifacts/src/cache/windows.rs`: the matching journal publication error closure receives the same typed adaptation. It does not reconstruct a generic Failed error or lose the originating native category.
- `crates/packaging/install-artifacts/src/cache.rs`: only private fixture_storage's module gate changes to `any(test, all(unix, feature = "test-support"))`. All unit-test hosts can therefore resolve the existing fault/read_event hooks. Actual DiskProvider/FreshDisk/file-id helper definitions remain Unix-gated in the unchanged fixture_storage.rs. No Windows test/import was gated away, no public fixture API widened, and native/fixture constructor selection stays unchanged.
- `crates/packaging/install-artifacts/src/error.rs`: added `contextual_failure_preserves_kind_diagnostic_and_uncertainty`, exercising the actual existing Failure I/O classification, with_message, with_uncertainty and into_error helpers. It covers four native categories, both incoming uncertainty states and both exact native context strings. Assertions check category and original diagnostic retention, uncertainty retention across message replacement and uncertainty marking afterward. No replacement helper or copied protocol algorithm was introduced. This is a helper regression/characterization check, not a claimed native-branch behavioral RED or native compilation proof.
- `apps/imager/src/setup/internal_download_flow.rs`: removes only the unused `Client as Worker` test import, retaining Phase. No production caller or test body changes.

No manifest, lockfile, peer UI/folder-service source, API surface, constant/encoding, native permission check or fixture behavior changed.

## Checks and receipts

Before fixes, all 53 reviewed product identities matched and were saved in `task-1-fix-1-before/` plus `task-1-fix-1-before-manifest.json`. The immutable 96-artifact reviewed snapshot in task-1-review-base/ was verified before edits and again after artifact preparation. Original review/report and its evidence remain available there; the original review file is unchanged.

Each command below used the unchanged shared dependencies and normal Cargo target directory. Exact command/log SHA256, terminal outcome, warning summaries and 65 owned/config input hashes before/after are in its fix-specific receipt. The owned/config maps are equal during all checks. Folder-service peer identities were recorded separately; this service's headless causal dependency closure excludes that peer and the concurrently changing UI.

| Command | Result | Evidence | Warnings |
| --- | --- | --- | --- |
| `cargo test -p tilo-install-artifacts --lib error::tests --offline --locked` | handle 51177 terminal 0; 2 passed, 0 failed/ignored | task-1-fix-1-error.log / -receipt.json | 12 lib-test |
| `cargo test -p tilo-install-artifacts --lib cache --offline --locked` | handle 57585 terminal 0; 60 passed, 0 failed/ignored | task-1-fix-1-cache.log / -receipt.json | 12 lib-test |
| `cargo check -p tilo-install-artifacts --all-targets --features test-support --offline --locked` | synchronous terminal 0 | task-1-fix-1-alltargets.log / -receipt.json | 21 lib; 10 lib-test, 4 duplicates |

The cache command used scoped localhost fixture escalation, with no fixture fallback or weakening. No additional cross-target attempts were made: the previously recorded Linux/Windows GNU missing C compilers and Windows MSVC missing target C headers still prevent the dependency graph reaching this service. No installation, fake header, host fallback, toolchain substitution, test removal or alternate dependency was used. These host results do not establish that the changed Linux/Windows native cfg branches compile or run; those branches have the explicit minimal source correction for independent review and require real target checks in a suitable environment. Windows all-target compilation with and without test-support and native execution remain open.

Per parent ruling, no new app/shared integration window was requested solely for deleting the unused test import. Its exact one-line source-only removal is shown in the fix-relative patch. Previous app 16-flow + 3-view + 21-release passes and app test-target compilation apply to their earlier recorded checkpoint. No fresh app or shell compilation is claimed for this fix snapshot. Existing broad warning debt remains reported; no suppression or unrelated cleanup was performed.

## Preservation and artifacts

- `task-1-fix-1.patch`: fix-only diff against exact reviewed bytes, never HEAD.
- `task-1-fix-1-changed-manifest.json`: five changed paths with reviewed/final SHA256.
- `task-1-fix-1-manifest.json`: full 53-path reviewed/final identity map, including 12 existing deletion tombstones.
- `task-1-fix-1-complete.patch`: current complete Task 1 diff against original accepted shared-WIP baseline.
- `task-1-fix-1-source.tar.gz`: 41 current product files plus embedded snapshot-manifest.json, under a distinct archive name.
- `task-1-fix-1-inventory.json`: all 176 original moved test names and all five original ignore declarations/reasons are unchanged. The one added error helper test is separate from that original moved inventory.
- `task-1-fix-1-normalization.json` / `task-1-fix-1-normalized-residual.patch`: regenerated using the original exact permitted normalization rules, with no visibility/cfg/test/constant/literal normalization. None of the twelve whole moves is claimed equal.
- `task-1-fix-1-body-identity.json`: 436/544 original moved function bodies remain byte-identical at named service destinations; the decrease by three is exactly the three explicitly corrected native provider bodies. Their changes stay visible in the fix/full residual.
- `task-1-fix-1-artifact-sha256.json`: distinct final artifact index, including this report and the appended main report. Original reviewed artifact hashes remain preserved and verified under task-1-review-base/.

Source frozen after the three covering checks. No live tool/process handles. No staging, commit, stash, cleanup, new install, target override or broad formatting. Task 2 preference persistence, Task 3 final baseline/privacy/package qualification, G02/L02/L03/S01/S02/N02, native Linux/Windows/macOS completion and signed/installed host qualification remain open. The prior parent shell checkpoint remains historical for its own exact inputs.
