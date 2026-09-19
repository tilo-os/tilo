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
