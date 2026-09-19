# Task 2 — worker-owned preference persistence

Status: **DONE_WITH_CONCERNS** for this source slice. Product source is frozen; no staging, commits, stash, dependency installation, alternate target directory, atomic-file implementation changes, UI core changes, or folder-service source edits were performed. **No owned live process/tool handles remain.** Parent owns independent review and the shared shell check.

## Implemented

- Added private `PreferenceStore` at the unchanged production location `dirs::data_local_dir()/qos/setup/internal-download-selection.json`. The service accepts opaque values up to 4096 bytes, rejects relative/unavailable roots before effects, reads through a held `StorageDirectory` and no-follow child open, checks metadata bounds and uses `take(4097)`. Missing and oversized old values are absent; actual read errors retain typed failure. The app still decides whether JSON is malformed.
- Unix publication uses existing `provision_directory(Inherit)`, held namespace-control validation, and held `publish(Replace, OwnerOnly)`. Provisioning and publication failure enums are mapped verbatim to the facade's knowledge records. There is no ignored sync result or compensating deletion. Windows uses the existing wrapper with 0o600; success reports `NamespaceUnproven`, never `Durable`; errors retain embedded publication knowledge and unproven namespace knowledge.
- The existing owned worker resolves/loads once and retains one bounded coalesced `PreferenceState`. Final `Client::start(selection, Vec<u8>)` validates the byte bound synchronously and enqueues the save. `resume(selection)` and discard never save. Saving precedes authentication and cache acquisition. Save failure stops those effects; the terminal status retains kind/error/uncertainty and `PreferenceState` retains exact publication/namespace facts. A successful commit remains in all later statuses for that token, including cancellation and later network failure.
- Exact dev fixture methods `client_with_preference_fault` and `client_with_preference_hold` configure the actual private store. Observation, acknowledgement and injected failure execute on the worker's thread around its real save. Holds require release within ten seconds. Test receivers use bounded waits and require actual checkpoint acknowledgement.
- Imager no longer owns preference file I/O. It starts its client before polling preference state, gates commands until loading settles, decodes the unchanged `SelectionKey`, and keeps `(Token, SelectionKey)` pending until that matching token reports a commit. Coalescing cannot lose the acknowledgement. Resume re-resolves the saved key against the current catalog. NamespaceUnproven adds a retained power-loss warning. Save errors remain visible. Review gating, unsupported selection refusals, and protected-staging refusal remain.
- SDK exports already contained all final preference DTOs, so no unnecessary SDK source edit was made. Positive API tests now require the final start/resume/preference signatures. Existing coordinator fixtures received only the new private store argument; one additional cancellation/failure test covers preservation of exact facts.

## Baseline, scope and preservation

Read the dispatched brief/context first, then the accepted plan/design and relevant project instructions. Before the first write, rehashed all **36** baseline entries (34 present and two explicitly absent): **zero drift**. Baseline bytes are in `task-2-before/`, with exact identities in `task-2-before-sha256.json`. The final map is `task-2-final-sha256.json`; `task-2-current.patch` is relative to those exact shared-WIP baseline bytes, **not HEAD**.

13 paths changed: 11 existing files and two additions. No path was deleted. `task-2-changes.json` records changed/new/deleted/unchanged lists. All Task 1 catalog, transport, bundle, cache provider, checkpoint, native platform and fixture-platform sources outside the private coordinator seam remain byte-identical to this Task 2 baseline. All changed Rust files retain zero added ignore attributes. `SelectionKey`'s schema, fields and complete implementation are byte-identical to the baseline (`task-2-preservation-checks.json` records the exact hash).

The parent-authorized finite dependency window changed only the service manifest and one Cargo.lock edge, adding the already-existing `atomic-file` package to `tilo-install-artifacts`. Normal dependency uses its existing path; service `test-support` forwards `atomic-file/test-support`; the atomic-file dev dependency enables that feature. `cargo metadata --locked --offline --format-version 1 --no-deps` exited 0. Parsed comparison preserves all **1626** package identities, versions, checksums and nondependency data; sole edge addition is service → atomic-file. Root/SDK/Imager/atomic-file manifests were unchanged. Exact hashes are in `task-2-dependency-checkpoint.json`; parent independently accepted and released peer builds before implementation continued. Cargo.lock was never regenerated.

## Behavioral RED and other failed attempts

1. `cargo test -p tilo-install-artifacts --lib preference --locked` against the moved legacy tempfile/file-sync/persist/ignored-directory-sync writer selected six tests: **2 passed, 4 failed**. `task-2-red.log` captures failures at `production publication checkpoint must be observed` and `production commit checkpoint must be observed`, as well as the old writer accepting 4097 bytes and following a preference symlink. These are actual behavior assertions against the initial writer, not missing APIs or fabricated native failures.
2. Before worker integration, `cargo test -p tilo-install-artifacts --test preference_publication --features test-support --locked` was first denied the localhost bind by the filesystem/network sandbox (`task-2-worker-red.log`). This is **environment failure, not RED**. The explicitly escalated bounded localhost fixture run (`task-2-worker-red-localhost.log`) then selected two tests, with the real 404 authentication path failing `save acknowledgement must survive later failure` (**1 passed, 1 failed**). This demonstrates the missing worker save/retained acknowledgement.
3. Corrected store passed the original six tests (`task-2-store-green.log`); expanded store mapping suite passed nine (`task-2-store-final.log`). Worker integration passed six (`task-2-worker-green.log`). Later consumer and cancellation assertions extend passing coverage; no separate preimplementation RED is claimed for every later added assertion.
4. One attempt to start existing coordinator tests was not executed because automatic approval review ran out of workspace credits. It was not an unsafe-action verdict. After the environment changed to full access, the command ran normally and passed 29/29 (`task-2-coordinator.log`). No assertion or fixture gate was bypassed.
5. The first final run failed compilation only because the new cancellation/failure test lacked two manifest-fixture imports (E0425). It never ran tests and is **not behavioral RED**. The exact failed log, source snapshots and receipt remain under `task-2-final-preference-unit-compile-error*`. The imports were corrected, then the complete final sequence passed.

## Final verification

Each final command has its exact argv, start/end timestamps, terminal exit, raw-log SHA256, warning lines and before/after source snapshot names in `task-2-final-<name>-receipt.json`. The capture helper follows every target/optional/dev/build local dependency and **all eight workspace patch.path roots**, recursively follows literal Rust include paths, and includes files under every local crate plus root Cargo/lock/toolchain/.cargo configuration. It captures a conservative **59-local-crate, 1939-file** closure. Every final command has **zero changed/added/deleted inputs** across its execution. The parent independently checked all eight receipts and raw-log hashes. Registry identities remain fixed by Cargo.lock. This bounds source coherence; it does not claim crash/native qualification.

| Receipt name | Exact command | Terminal result |
| --- | --- | --- |
| preference-unit | `cargo test -p tilo-install-artifacts --lib preference --locked` | 10 passed, 0 failed, 0 ignored |
| preference-normal | `cargo test -p tilo-install-artifacts --test preference_publication --locked` | 1 portable positive surface test passed |
| preference-support | `cargo test -p tilo-install-artifacts --test preference_publication --features test-support --locked` | 6 passed, including all real worker fixture behaviors |
| api-normal | `cargo test -p tilo-install-artifacts --test api_surface --locked` | 2 passed |
| api-support | `cargo test -p tilo-install-artifacts --test api_surface --features test-support --locked` | 3 passed |
| app-flow | `cargo test -p imager --lib internal_download_flow --locked` | 20 passed |
| app-view | `cargo test -p imager --lib --locked -- full_os_download_command duplicate_full_os_start_command` | 3 passed |
| app-check | `cargo check -p imager --tests --locked` | exit 0 |

The plain preference integration command does **not** exercise feature-gated worker fixtures: its one test is distinguished from the full explicit test-support run. No feature widening was used to make the count look larger. Existing coordinator suite separately passed 29/29 before the final added cancellation regression; that new test runs in final `preference-unit` (10 = nine private store tests + one actual-worker cancellation/commit-failure test).

Coverage includes absent/read-only load, 0/4096/4097 bounds, opaque malformed bytes, symlink refusal, relative path refusal, before-publication failure, known publication/failed commit, lost publication acknowledgement, exact directory provisioning states, independent reread, save before network/cache effects, cancellation after publication with success and with commit failure, commit retention through early network failure, no-save resume, once-only load, malformed JSON app policy, loading gates, coalesced acknowledgement, foreign-token refusal, changed/duplicate catalog refusal, and retained warning projection.

**Warnings are not pristine.** Final service unit log reports 15 compiler warnings (16 `warning:` lines including the summary); normal service integration/API reports 100 (101 lines), predominantly unused/cfg-gated service items on this unsupported normal macOS cache host; support builds report 21 (22 lines). Each final app check/test log has 488 warning lines: compiler summaries report tilo-install-artifacts 21, folder-service 431, ui 8 and automerge 24. These include existing warning debt and host-cfg unused preference plumbing. The one introduced unused Imager fixture local was removed; final logs contain no Imager-specific warning summary. No warning suppression or unrelated cleanup was added.

## Qualification gates and concerns

- This is the accepted legacy unprivileged source slice. G02, authenticated HostContext construction, removal of `Client::open_legacy_imager`, broader framework migration and Task 3 API/packaging gates remain open.
- Preference tests execute the real atomic-file production path on this macOS host. Unix cache-worker tests use the explicit existing host fixture; they do not qualify Linux or Windows native cache protocols and do not implement the missing macOS production cache.
- Windows success is explicitly NamespaceUnproven in source and has a Windows-conditioned assertion in the cross-platform private store test. This host did not execute it. The app warning test projects the public DTO using a real token; it is consumer presentation evidence, **not Windows native evidence**. Required Windows namespace/profile support and installed native qualification remain open; no Durable success is claimed for Windows.
- Previously documented Linux/Windows cross-build barriers in native C toolchain/headers were not retried unchanged. No missing-target or environmental failure counts as privacy/native proof.
- The parent owns `cargo check -p shell --tests --locked`. Its first run compiled with exit 0 but was invalidated when unrelated `ui/src/control_custody.rs` changed during the 2536-input observation; that is not a stable final-shell acceptance. A fresh parent observation is pending. Task 2's own app receipts remain valid at their separately unchanged snapshots.
- Existing Imager flow source is large and remains in its planned file; no unrelated restructuring was attempted.

## Review artifacts

- `task-2-current.patch`: complete exact-baseline-relative diff, including additions.
- `task-2-before-sha256.json`, `task-2-final-sha256.json`, `task-2-changes.json`: exact complete 36-path identities and disposition.
- `task-2-source.tar.gz`: all 36 final snapshot files plus embedded `snapshot-manifest.json` (source, not a replacement build path).
- `task-2-preservation-checks.json`: lock identity/edge proof, unchanged app schema check, ignore inventory, final receipts and no-handles record.
- `task-2-artifact-sha256.json`: hashes of the report, diff, source archive, scripts, snapshots, metadata and all retained command logs/receipts (excluding the index itself).
- `capture-task2-app-closure.py`, `run-task2-final-checks.py`, `prepare-task2-artifacts.py`: reproducible observation/artifact tooling outside product source.

## Exact changed-file identities

| Path | Baseline SHA-256 | Final SHA-256 |
| --- | --- | --- |
| `Cargo.lock` | `4ea03c4be01006d0079381240a24e1dce3d25182942ea1656603c6efa1ab0497` | `33139a9401f14c7499a315cfcfe31ee6c9d27b064647352b7c4304f3c884e560` |
| `apps/imager/src/setup/internal_download_flow.rs` | `fca44de65229ef8cf07c805b0176a9a2516225f4ea56e618de1227c2324673fb` | `e5ff7bcba054454da940b246661a8c9655aa46a5dc36174fda35c97875c2c6ef` |
| `apps/imager/src/setup/view.rs` | `8155ff1ec94e189c2c3ad33f2a53451b1e865c0065172d39e75b72d078d4489f` | `20cb2a9d20cde45c2d899c33e72a0c8b024a6b40651c1ec7e0eab689fe9d0a20` |
| `crates/packaging/install-artifacts/Cargo.toml` | `dfbdfcac7b258c9572210d64ed452655bbbc9bcd6b525cc4846ec6a9b9062ef0` | `1211990603b548017bed6090fbf3f3c35e3d51b9dafd682923439695e9cdb252` |
| `crates/packaging/install-artifacts/src/cache/coordinator.rs` | `c2edd0c11b5f1b3cdc17010ae7f0084330ea68666d53f45a79f2307130a74e72` | `e8a3ab8a31d7b111d4245326d68b226a7edb94d76ef5f8cdd9a2e7ff50bfab22` |
| `crates/packaging/install-artifacts/src/cache/coordinator/tests.rs` | `65e0a561846b8b658b42d57e87eda43e3a8ae02e636f7accc6aa6d68a68636bd` | `2cb0a720077f2917b12f086adaf37b19177f6d71d561b0dd7b4791d10f082ec2` |
| `crates/packaging/install-artifacts/src/download.rs` | `8b5e742d464d6249bd5f8ae8f2175dbeee91a62408f08f2d8cfa416ecd77924d` | `18dd3ca1248b19104d18f6da01c3b1f975e0b7997573dc1229f4384038894992` |
| `crates/packaging/install-artifacts/src/error.rs` | `3847f18ad2774bfb9e8026298a108a2128c66ae8c51ca98906be55f0d72c058c` | `8f93dfbba238da24ae7595f5b2fb6bb728cb4eb352cb9c7c2e6d272938f5475e` |
| `crates/packaging/install-artifacts/src/lib.rs` | `3f132923fe71334456113bcf62e4822d8ced93ddd94d2355b2e53e873f04c98d` | `5ddfe698a326cdfb4acfd16f31f16a62c7d082870298f2e3f44674eb550fed04` |
| `crates/packaging/install-artifacts/src/test_support.rs` | `24ff060e07bcd83894731e238204846a41c5fbdb00e4635b87fb3975974535c6` | `2d7f903c3857a14bf31be06c7ee8aa02d4b33d084cb004c09c7110f8fdf99f2a` |
| `crates/packaging/install-artifacts/tests/api_surface.rs` | `22bb9f4de706090b4cd307e43dd646fa562fa53cacbadf4b865182d2e018c6ba` | `acc7027f96ef56f6be1311a212fb41d4e07efbe3db34e80187a19c740e5dbbd8` |
| `crates/packaging/install-artifacts/src/preference.rs` | `ABSENT` | `05560cf6314d0cf310c006f13daa1a3dc7fa188a86ff26fe49e10826d2bb961b` |
| `crates/packaging/install-artifacts/tests/preference_publication.rs` | `ABSENT` | `9d8a7feecdd3640a308aa542a910bbe03063a4e717c8e0d1c2e51127cf68267a` |
