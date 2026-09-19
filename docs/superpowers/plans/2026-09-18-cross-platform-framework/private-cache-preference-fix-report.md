# Task 2 fix round 1 — loading schedule and explicit retry

**Status: DONE_WITH_CONCERNS; ready for scoped independent rereview, not an acceptance claim.** Both Important review findings are addressed. The Minor host-cfg warning finding remains documented. No commits, staging, stash, dependency edits, alternate target directories, or live owned handles.

## Findings verified and changes made

Read the complete `task-2-review.md` and `task-2-fix-1-context.md`, then verified the production call chain. `SetupApp::needs_idle_sync` delegates to `InternalDownloadFlow::has_pending_operation`, whose previous predicate omitted `!preference_loaded`. A newly constructed flow therefore gated its commands without requesting another idle sync. `can_start` also required no token unconditionally, stranding a terminal failed/cancelled save even though its pending key had never been committed and the saved key remained unchanged.

Only two product files changed:

- `apps/imager/src/setup/internal_download_flow.rs`: unsettled preference loading participates in the existing pending-operation/idle predicate. Start admission also permits a matching pending token/key after Failed, Cancelled, or Paused. The pending key exists only until its matching commit is acknowledged, so a committed download does not acquire this retry path. This predicate change does not clear or rewrite the prior token, presentation, saved key, failure facts, or publication/namespace knowledge. Explicit `start_selected` retains its existing capability/catalog resolution before submitting a fresh token. Two real-facade regressions cover failed publication followed by successful retry, and cancellation before the target save followed by successful retry.
- `apps/imager/src/setup/view.rs`: one regression exercises the actual `Widget::needs_idle_sync` and `Widget::sync` path. It first proves other SetupApp idle reasons are absent, installs the unpolled preference-loading flow without pre-waiting it, and calls sync only while the production predicate requests it. It verifies loading settles, Start becomes available, idle demand ends, and no preference parents/cache/network effects occur.

The publication-refusal regression uses a real directory collision at the temporary preference path after loading a valid older key. It asserts NotPublished/Durable, original error, unchanged old saved key, matching pending token/key, and no cache/network effects. Repeated polling preserves those facts. Only explicit repair and retry replace the failed token; a stale failed status is rejected, the new matching commit advances saved, and a deliberate later 404 proves the successful preference commit survived authentication failure.

The cancellation regression uses the existing observed BeforeCommit hold on an older real operation. It queues and cancels the target operation before its save can run, releases the older hold, then verifies the target's Cancelled status has no preference commit and no cache/network effects. Its token, pending key, presentation and preference facts remain until retry. The retry is held at its own observed commit checkpoint, where saved is still absent; releasing that hold produces the matching commit and saved-key acknowledgement. No new fixture hook or storage API was required.

## TDD and final checks

Each command ran through `run-task2-fix1-check.py` with full before/after local dependency snapshots. The closure includes all local target/optional/dev/build dependencies, all eight workspace patch.path roots, literal includes, root Cargo/lock/toolchain and .cargo configuration: **59 crates, 1939 files**. Every check below has zero changed, added or deleted inputs. Exact argv, timestamps, terminal exit, result lines, raw-log SHA256 and warning lines are in its `task-2-fix-1-<name>-receipt.json`; full output is in the adjacent `.log`.

| Name | Exact command | Result |
| --- | --- | --- |
| red | `cargo test -p imager --lib --locked -- preference_loading_keeps_setup_idle_sync terminal_uncommitted_preference` | exit 101; 3 selected, 0 passed, 3 expected behavioral failures |
| green | Same command | exit 0; 3 passed |
| coverage | `cargo test -p imager --lib --locked -- internal_download_flow preference_loading_keeps_setup_idle_sync full_os_download_command duplicate_full_os_start_command` | exit 0; 26 passed, 0 failed, 0 ignored |
| check | `cargo check -p imager --tests --locked` | exit 0 |

The RED failures were exactly:

- `unsettled preference must request the next idle sync` from the actual SetupApp predicate;
- `a terminal uncommitted save must allow an explicit retry` after the real publication refusal;
- `cancellation without a commit must allow an explicit retry` after the real queued cancellation.

No compile error, sandbox failure, absent checkpoint or zero-match filter is counted as RED. The unchanged service implementation was not rebuilt merely to repeat old suites; the affected full app flow and view union covers existing duplicate/foreign/stale/catalog/cancellation/acknowledgement behavior alongside the three new regressions.

All four logs contain **488 warning lines** including summaries: automerge 24 compiler warnings, ui 8, folder-service 431 and tilo-install-artifacts 21. There is no new Imager warning summary. The previously documented normal macOS unused parameter/preference plumbing warnings remain in the unchanged service, as requested by the Minor review finding; this fix adds no suppressions or host-cfg alterations and does not claim pristine output.

## Shell and qualification boundaries

Parent-owned evidence `task-2-shell-checkpoint-retry1/receipt.json` records the successful `cargo check --offline --locked -p shell --tests` with 2536 unchanged inputs. `task-2-fix-1-shell-revalidation.json` and `task-2-fix-1-shell-revalidation-receipt.json` freshly establish that the complete 73-crate/2536-input shell dependency map still equals that successful checkpoint, including path-patch membership. There are no Imager inputs in that shell closure. This is continued applicability of the earlier compile plus fresh source equivalence, **not a newly run shell compile**. The earlier drift-invalidated shell run remains excluded.

This round does not close native Linux/Windows qualification, the missing production macOS cache, G02/legacy-constructor removal, or Task 3 API/packaging/boundary cleanup. The boundary guard's reported diagnostics remain assigned to Task 3. No unchanged native cross-build/environment failures were retried.

## Exact scope, immutable evidence and frozen artifacts

Before editing, all 36 product hashes matched the reviewed Task 2 freeze. Copied those exact bytes to `task-2-fix-1-before/` and hashes to `task-2-fix-1-before-sha256.json`. Final hashes are `task-2-fix-1-final-sha256.json`. Exactly **2 existing paths changed, 0 added, 0 deleted**; all 34 other tracked task inputs remain byte-identical, including every service source, SDK export, manifest and Cargo.lock. No atomic-file, peer UI/folder-service, packaging, or baseline files were edited.

The original 109 evidence artifact hashes in `task-2-review-base/` were validated before edits and again at final freeze. The original report, diff, review and logs remain available unchanged. `task-2-fix-1-preservation.json` records that check and the exact scope result.

- `task-2-fix-1.patch`: fix-only diff relative to the exact reviewed source snapshot, never HEAD.
- `task-2-fix-1-{before,final}-sha256.json` and `task-2-fix-1-changes.json`: full 36-path identities and exact dispositions.
- `task-2-fix-1-source.tar.gz`: 36 final source files plus `snapshot-manifest.json`; every archived file was verified against the final manifest. No archive is on the product build path.
- `task-2-fix-1-artifact-sha256.json`: report, diff, archive, snapshots, logs, receipts and fix tooling hashes, excluding the index itself.
- `run-task2-fix1-check.py`, `prepare-task2-fix1-artifacts.py`: bounded verification and reproducible artifact preparation.

Product source is frozen. All fix-round command handles are terminal; owned live handles: **none**. No further source change is planned before rereview.

| Changed path | Reviewed SHA-256 | Fix SHA-256 |
| --- | --- | --- |
| `apps/imager/src/setup/internal_download_flow.rs` | `e5ff7bcba054454da940b246661a8c9655aa46a5dc36174fda35c97875c2c6ef` | `7f5eb0b885d57669802a7a47347e53067c523645a70fd126100d9fde3c7eb496` |
| `apps/imager/src/setup/view.rs` | `20cb2a9d20cde45c2d899c33e72a0c8b024a6b40651c1ec7e0eab689fe9d0a20` | `23e80ab751307b0cdee70218259926e8d7a3c1fc7aab9334f604cee159d4fec6` |
