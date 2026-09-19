# Task 3 privacy, package wiring, and source milestone report

Status: **DONE_WITH_CONCERNS for the Task 3 source milestone.** The bounded implementation and its source checks are complete and frozen. There are no live process handles. Parent-owned shell verification also passed after the freeze; independent review follows. This report does not close N02, G02, L02/L03, S01/S02, native Linux/Windows runtime qualification, signed-distribution provenance, or installed-package execution.

## Frozen source result

`task-3-before-sha256.json` was rehashed before the first edit and contained 52 planned paths: 40 present and 12 explicitly absent. `prepare-task3-artifacts.py` now proves 5 changed paths, 12 additions, 35 byte-identical paths, and no Task 3 deletion. Exact path classifications and hashes are in `task-3-changes.json` and `task-3-final-sha256.json`; the snapshot-relative patch is `task-3-current.patch`. The source archive is `task-3-source.tar.gz`, SHA-256 `db431c0bafb04ddbb6dd64a34beb9534483663c8ab884ff9d4101dcd668ae0bb`.

The five changed paths are:

- `.forgejo/workflows/release-imager.yml`
- `apps/imager/src/setup/view.rs`
- `crates/app/tilo-app/src/install_artifacts.rs`
- `crates/packaging/install-artifacts/src/lib.rs`
- `tools/app_platform_baseline.json`

The 12 additions are `tests/privacy_probes.py` and the eleven files under `tests/consumer_probes/` listed in `task-3-changes.json`. Cargo workspace/root/lock and all relevant app/service manifests stayed byte-identical to the Task 3 baseline. All cache, transport, preference, format, catalog, fixture, and native-provider implementation files stayed byte-identical; Task 3 changed API documentation, external boundary probes, workflow enforcement, exact obsolete checker entries, and the authorized view-test grouping only. Folder-service inputs in the conservative causal closure remained unchanged.

## External privacy boundary

The probe runner rebuilds the requested service or SDK artifact immediately before each group using Cargo JSON. It selects the emitted hashed `.rmeta` and matching `.rlib`, records both hashes and feature lists, runs the positive control first, passes an optional target to Cargo and rustc, and rejects E0463, E0464, E0514, and E0786. Each group had 59 local crates and 1,951 conservative causal inputs before and after with zero drift. Final outputs are under `task-3-final-probes/`:

| Group | Positive controls | Exact negative diagnostics |
| --- | --- | --- |
| service-normal-2 | public API | cache E0603; transport E0603; forged selection E0451; private field E0616; no Deserialize E0277; no consume E0599; backend E0432+E0603; no fixture facade E0432 |
| service-support-2 | public API, Unix fixture selection/client | same semantic/private results; backend E0432+E0603; Server::transport E0624 |
| sdk-normal-2 | SDK public API | cache E0433; transport E0432; forged selection E0451; private field E0616; no Deserialize E0277; no consume E0599; backend E0433; no fixture facade E0432 |
| sdk-support | SDK public API | same SDK negatives, including no SDK fixture facade E0432 |

The original service-support attempt expected the plan's E0599 and failed with E0624. Inspection showed `Server::transport` is an intentionally retained `pub(crate)` helper for moved internal tests. The parent approved exact E0624 as the stronger external private-method proof without renaming the helper or changing the access attempt. The failed assumption log remains preserved; the final group accepts only E0624 after both positive controls and still rejects all environment codes.

Normal service receipts contain only `default`; support receipts contain `default,test-support`. SDK receipts contain `default,install-artifacts` and never add an SDK `test-support` export. In the SDK support group, only the direct service dependency enables its bounded fixture feature. `task-3-feature-inverse-{normal,support}.log` and `task-3-feature-inverse-results.json` record the corresponding inverse atomic-file feature trees with 1,951 stable inputs. The normal production edge enables only atomic-file default; atomic-file `test-support` remains a dev dependency. The explicit service support group deliberately enables it through the service's test-support feature.

These probes prove there is no public cache/backend/provider/transport/receipt/consume/raw-handle bypass through either service or SDK facade, and that `ReleaseSelection` cannot be externally constructed, read, or deserialized. They are compile-boundary evidence, not native runtime evidence.

## Automated release-lane enforcement

Each existing Linux and macOS Imager release lane now runs the service lib tests, service doctest, SDK install-artifacts doctest, and all four external probe groups before the existing Imager tests. Each Windows lane command preserves `$env:TILO_WINDOWS_ARCH-pc-windows-msvc`; the normal service and SDK probe groups pass that exact target to Cargo and rustc. Unix fixture-support groups are deliberately absent on Windows because that facade is Unix-only. Existing Imager and folder-service tests remain in place. No release, signing, packaging, push, or privileged fixture action was run.

The workflow probes compile target artifacts only; they do not claim cross-host runtime behavior. SDK and service documentation now state the semantic API, preference timing, fixed-catalog authority limit, and private cache/transport/native boundary. Both doctests compile against `Client::start(selection, Vec<u8>)`, `preference()`, and `resume(selection)`.

## Exact app-boundary retirement and test preservation

The pre-retirement checker reported exactly 84 stale diagnostics after the authorized view grouping. `retire-task3-baseline.py` removed exactly those 84 entries from 512 to 428, added nothing, and did not expand or regenerate an allowance. Before/after hashes are `cf3b7e7bc85cacdba9364fb792268904dcc6b48ad6ed39c3d69ae892e2513e68` and `7bf9c4773c917607cd3471d7bcdba7bdded1dbcfa4b1ed3108dd62805c425202`; every removed identity is in `task-3-baseline-retirement.json`. The final checker passed all 55 app packages and its 32 unit tests passed.

Five existing Unix-dependent view tests were grouped under `tests::unix_tests`; the other 24 remain in the portable parent module. `task-3-test-inventory.json` proves all 29 bodies are byte-identical and maps each old qualified name to its final qualified name. The final scoped run selected and passed all 29, with 305 unrelated tests filtered. The original moved inventory remains 26 bundle + 49 transport + 101 cache = 176 names with the exact five ignore reasons. The current cache count is 102 only because Task 2 already added `cancelled_preference_commit_failure_keeps_publication_and_namespace_facts`; Task 3 added or removed none.

The fixed service catalog remains byte-identical at SHA-256 `3aa2c12d568e82557b278a17f9488f55f45e16f40e0c0e3159b369bad44e9f41`. The emitted service dep-info names `src/../release-catalog.json` and never the removed app catalog. The old app cache/download implementation files are absent; the remaining app bundle module is a narrow `ui::install_artifacts` compatibility forwarder with no native API mechanism.

## Final source checks

`run-task3-final-checks.py` completed terminal 0. Every group captured the full conservative closure before and after: 59 local crates, all eight workspace `patch.path` roots, all local target/optional/dev/build edges, includes, Cargo.lock, toolchain, and root config; all 1,951 inputs were unchanged. `task-3-final-results.json` binds commands, exits, log hashes, warning counts, and input identities.

- service lib: 159 passed
- service API: 2 normal and 3 support passed
- preference publication: 6 passed
- service and SDK doctests: 1 each passed
- service `--all-targets` check and Imager `--tests` check passed
- view selection: 29 passed, 0 failed, 0 ignored, 305 filtered
- app boundary checker: 55 packages passed; checker unit tests: 32 passed
- normal/support and target-resolved Linux/Windows dependency trees completed

The normal service graph has no app, UI, winit, or vello edge. Target-resolved trees are dependency-resolution evidence only. Existing warnings were retained in the raw logs rather than suppressed: 16 warning lines for the service lib, 101 for normal API/service docs, 22 for support API/preference, 135 for SDK docs, 116 for service all-targets, and 488 for Imager/view compilation.

Task 1's installed cross-target attempts were not repeated because their prerequisites did not change: aarch64 Linux stopped before this crate for missing `aarch64-linux-gnu-gcc`, Windows GNU for missing `x86_64-w64-mingw32-gcc`, and Windows MSVC in ring's native C build for missing target `assert.h`. Their exact logs and receipts remain `task-1-cross-{linux,windows,windows-msvc}*`. They establish no native or cross-target compile pass. This macOS host's normal production cache is UnsupportedHost; Unix fixture tests do not qualify a macOS production backend. Native Linux/Windows execution, Windows ACL/quota fixtures, L03 emulated-host evidence, signed catalog provenance, and installed package execution remain open.

## Remaining gates and handoff

The parent-owned post-freeze `cargo check -p shell --tests --locked` completed terminal 0 with 73 local crates, all eight workspace path patches, and 2,548 before/after inputs identical. Its receipt, maps, and raw log are under `task-3-shell-checkpoint/`; the log SHA-256 is `c87fdf806956cb1d2716ed966a6da8aea10cb4c70e249c2e06bcb59803edb6c3`. The check retained 60 library and 36 library-test warnings.

Parent owns public milestone/report/register updates and independent specification/code review. G02 HostContext authority and removal of the compatibility entrance, L02 root policy migration, L03 native/emulated host qualification, S01/S02 consolidation, N02 completion, native Linux/Windows backend execution, ignored external fixtures, signed distribution provenance, package/sign/release execution, and the full 194-finding program goal remain open. This Task 3 source milestone does not claim any of them complete.

No commit, staging, stash, alternate target directory, Cargo.lock regeneration, manifest edit, release/sign/package action, fake dependency error, test skip, or weakened checker assertion was used.

`task-3-artifact-sha256.json` binds the final report, source archive/manifests, scripts, probe diagnostics and receipts, raw check logs, causal snapshots, retirement evidence, and parent shell checkpoint.
