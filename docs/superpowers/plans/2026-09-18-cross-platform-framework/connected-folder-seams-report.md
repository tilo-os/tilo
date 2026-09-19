# Task 1 implementation report

Status: implementation and local verification complete; ready for the parent's independent review. No source writer remains active and every child Cargo handle is terminal. No commit/stage performed.

## Ownership and baseline

Exact base commit: `6e5721c6ce73460a282f40b3a265828f5dd28072`.

Only four product files were edited: `apps/imager/src/setup/{migration.rs,migration_store.rs,desktop_continuation.rs,import_view.rs}`. Parent confirmed exclusive four-file ownership and native connection source hold before dispatch. Before edits, the implementer independently recomputed all 100 accepted public-design fingerprints and the four prepared snapshot hashes; all matched. `task-1-implementer-preflight.json` records this result. `task-1-relevant-wip-sha256.json` records Cargo.lock, workspace/app manifests, existing Imager lib WIP, and ui lib WIP; each remains unchanged at completion.

`task-1-before-sha256.json` and `task-1-before/` are the review baseline. `task-1-snapshot-relative.diff` is the exact final diff against those bytes. `task-1-final-sha256.json` records all four final hashes. `task-1-final-source-manifest.json` records all 100 public inventory paths at completion; exactly the four owned files differ from the accepted inventory, and all native connection sources remain identical.

## Semantic changes, separate from relocated bodies

- Continuation source/destination identity and absence policy now calls `directory_identity` and `path_is_absent` from migration. Its overlap/collision/error-message policy, persisted identity/revision schema, and worker expectation constraints remain unchanged.
- `Continuation::resolve` consumes `inspect_copy_review(...).into_expectations()` and writes the same parent identity and `Some(source_revision)` fields. `CopyReview` is `pub(super)` with private fields; it exports only inert expectation values, no live pin or authority.
- Import review invokes `prepare_managed_import_parent`; the existing default path `dirs::data_local_dir()/qos/files`, error, private-root preparation, and discarded pin lifetime remain equivalent.
- The pre-existing portable `absolute` validator remains byte-identical app-side. Its exact validation is also performed first at the new directory-identity boundary.
- After removing native review from continuation, its remaining two `PinnedDirectory` references occur in already-Linux-gated readiness function bodies. Those references were qualified and the unused root import removed. No cfg/allow/site exception was added. This final lexical cleanup was followed by a fresh compile check; unaffected regression tests were not repeated.

## Moved-body fidelity

Scripted comparison against the frozen snapshot confirmed: directory-identity native body is byte-identical after function rename, return-type qualification and replacement of `absolute(path)?` with the unchanged validation body; absence body is byte-identical after rename and visibility. `inspect_copy_review` preserves the brief's exact ordering: parent discovery, parent pin open, destination validation, parent directory identity, source pin open, source inventory capture, revision. Managed-parent operation matches the brief. Manual diff review found no unrelated policy changes.

All four existing `#[cfg(test)] mod tests` suffixes are byte-for-byte identical to the snapshot; no assertions, ignored status, fixture, or test names changed. `git diff --check` on the four owned paths passed. No formatter was run, preserving pre-existing unrelated formatting.

## Commands and results

All Cargo commands used the existing pinned toolchain and the prefix `env PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig`. No CARGO_TARGET_DIR or HOME override was set. Offline and locked resolution were preserved throughout.

| Phase | Exact Cargo command (after environment prefix) | Result | Log |
| --- | --- | --- | --- |
| Untouched sandbox baseline | `cargo test -p imager --lib setup::desktop_continuation::tests --offline --locked` | Exit 101; 0 passed, 10 failed because existing home-directory fixture creation was denied | `task-1-baseline-continuation.log` |
| Untouched sandbox baseline | `cargo test -p imager --lib setup::import_view::tests --offline --locked` | Exit 101; 0 passed, 15 failed with denied fixtures/unavailable derived shot state | `task-1-baseline-import.log` |
| Untouched admitted baseline | `cargo test -p imager --lib setup::desktop_continuation::tests --offline --locked` | Exit 0; 10 passed, 0 failed/ignored | `task-1-baseline-continuation-admitted.log` |
| Untouched admitted baseline | `cargo test -p imager --lib setup::import_view::tests --offline --locked` | Exit 0; 15 passed, 0 failed/ignored | `task-1-baseline-import-admitted.log` |
| After extraction, admitted | `cargo test -p imager --lib setup::desktop_continuation::tests --offline --locked` | Exit 0; 10 passed, 0 failed/ignored | `task-1-final-continuation.log` |
| After extraction, admitted | `cargo test -p imager --lib setup::import_view::tests --offline --locked` | Exit 0; 15 passed, 0 failed/ignored | `task-1-final-import.log` |
| After extraction | `cargo check -p imager --tests --offline --locked` | Exit 0; 14.45 seconds | `task-1-final-check.log` |
| Final qualified-import source | `cargo check -p imager --tests --offline --locked` | Exit 0; 4.82 seconds; Imager test warning count restored to baseline 21 | `task-1-final-check-qualified.log` |

The admitted test invocations used require_escalated for the unchanged self-cleaning native home-directory fixtures, following observed sandbox denial. No approval rejection occurred. Both admitted baselines preceded every product edit; all subsequent tests passed. No baseline was classified known-red.

Mandatory production-path evidence passed in both baseline and final continuation runs:

- `source_swapped_after_review_is_rejected_by_worker_before_journal_or_output`
- `destination_swapped_after_review_is_rejected_before_output`
- `edits_after_inventory_validation_cannot_become_the_copy_baseline`
- `unpublished_retry_refuses_existing_names_and_unavailable_parents`

`python3 tools/check_app_platform_contract.py` was run before and after (also after the final import cleanup): exit 1, 28 violations across 55 app packages each time. `task-1-boundary-before.log` and `task-1-boundary-after.log` are byte-identical. This is fresh, unchanged boundary debt, not a passing boundary gate. No exception was added.

## Process custody and limitations

Cargo sessions owned and collected by this implementer: 22918 (initial sandbox continuation, exit 101), 95309 (admitted baseline import, exit 0), 4422 (final continuation, exit 0), 21378 (final import, exit 0), 56380 (initial final check, exit 0), 56540 (qualified-import final check, exit 0). Other commands returned terminal results directly. No process was cancelled/restarted due to lock or observation timeout. Cargo was yielded before starting baselines while parent completed shell check session 32431; that parent-owned check was reported exit 0 and is not claimed as child-owned evidence. The child did not run a broad shell check.

This verifies the intermediate four-file preparation on the current macOS host, not full framework extraction, Linux/Windows native qualification, installed artifacts, endpoint admission, or cross-platform support. No devices, screenshots, coordinator operations, dependency ownership changes, lock regeneration, staging or commits occurred. Parent must finish independent review before advancing Task 2; the implementation remains uncommitted WIP.
