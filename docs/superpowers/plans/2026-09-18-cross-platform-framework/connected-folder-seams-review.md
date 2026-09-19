# Task 1 independent review

## Spec compliance

- **PASS for the bounded four-file seam preparation.** The implementation matches the Task 1 brief and dispatch context. Review base is the frozen `task-1-before/` snapshot; `6e5721c6ce73460a282f40b3a265828f5dd28072` is informational. Read `task-1-review.diff` once; the parent supplied and verified the corresponding four current hashes in `task-1-review-sha256.json`.
- `apps/imager/src/setup/migration.rs:30` moves directory identity sampling with the exact absolute/no-traversal validation first, then pin open, pin validation, readable-directory conversion and identity sampling. `migration.rs:47` preserves absence-check ordering, including destination validation both before and after no-follow metadata lookup and the NotFound-only absence result.
- `apps/imager/src/setup/migration.rs:63` gives `CopyReview` private fields and exposes only consumed identity/revision expectations. `migration.rs:74` preserves parent discovery, parent pin open, destination validation, parent identity, source pin open, inventory capture and revision ordering. No live pin or new authority escapes.
- `apps/imager/src/setup/desktop_continuation.rs:217` consumes those expectations and preserves `Some(source_revision)` in the existing resolved record. The unchanged overlap/collision checks precede review. Identity and absence call-site replacements retain their original refusal messages and short-circuit order (`desktop_continuation.rs:60`, `:259`, `:278`, `:299`, `:344`). No serde field/schema change appears in the diff.
- `apps/imager/src/setup/migration_store.rs:873` implements the exact requested managed-parent operation. `apps/imager/src/setup/import_view.rs:675` uses it in the existing unselected-parent branch, preserving the data-local `qos/files` path, error text and private-root preparation.
- `apps/imager/src/setup/desktop_continuation.rs:447` and `:527` merely qualify the two remaining `PinnedDirectory` names in existing Linux-only readiness bodies. No native dependency, Cargo ownership, cfg/allow exception, test assertion or unrelated policy hunk is introduced.
- **Verification limits:** exclusive-writer history, process custody and unchanged files outside the supplied four-file diff cannot be proven from that diff. They are recorded in `task-1-report.md:9`, `:11`, `:25`, `:55`; `task-1-implementer-preflight.json:9` records 100 checked fingerprints with no drift. Parent independently verified snapshot/current hashes. Native Linux/Windows qualification and the later framework relocation remain explicitly outside this verdict (`task-1-report.md:57`).

## Strengths

- `migration.rs:63` keeps the seam narrow: plain review expectations, private representation and no authority-bearing return type. `desktop_continuation.rs:217` keeps app choice policy at the caller.
- `migration.rs:47` retains both validations around metadata inspection, avoiding a subtle weakening during relocation. `migration_store.rs:873` removes the app's discarded-pin use without changing when private-root preparation occurs.
- Existing behavior tests remain unchanged in the supplied diff. Saved admitted baseline and final logs provide both positive runs and the required source-swap, destination-swap, post-inventory-edit and unpublished-retry refusal evidence.

## Issues

### Critical

- None found.

### Important

- **Plan-mandated duplicate validation — `apps/imager/src/setup/migration.rs:31` and `apps/imager/src/setup/desktop_continuation.rs:80`.** The absolute/no-traversal predicate and its error string now have two verbatim implementations. They currently agree and both boundaries need validation; this is not a demonstrated behavior or authority defect. However, future changes can make continuation decoding and directory review accept different representations or emit different failures. The assigned reviewer rubric explicitly classifies plan-mandated verbatim logic duplication as Important, so this finding remains visible despite being required by the brief. Resolve the design/rubric conflict before calling the quality gate unqualifiedly approved: either consolidate the pure representation validator in an appropriately neutral shared location while retaining validation at both boundaries, or explicitly accept independent defensive validation as a scoped design exception. Do not remove either boundary check or introduce a framework-to-app dependency merely to deduplicate it.

### Minor

- **Existing warning noise — `apps/imager/src/setup/connection/store.rs:199`**, evidenced by `task-1-final-check-qualified.log:386`. The final check succeeds but is not warning-clean: Automerge 24, UI 8, Imager library 328, Imager library tests 21 warnings (`:313`, `:384`, `:3316`, `:3516`). The test warning count returns to its admitted baseline of 21 (`task-1-baseline-continuation-admitted.log:618`); the temporary unused `PinnedDirectory` import is removed in the final diff. Track existing warnings separately; no warning cleanup is required in this bounded task.
- **Existing platform-contract debt — `apps/imager/src/setup/connection/store.rs:361`**, evidenced by `task-1-boundary-after.log:1`. The 28-violation platform gate remains failing (`:29`), and the before/after log bytes are identical. This is not a new Task 1 regression or a passing full-extraction gate.

## Checks performed

- Read the task brief first, dispatch context second, then reviewer rubric, implementation report, hash manifest and snapshot-relative diff. No product or Git mutation, subagent, broad source search or test rerun was performed.
- Inspected saved baseline/final logs rather than repeating tests: continuation baseline 10/10 (`task-1-baseline-continuation-admitted.log:634`) and final 10/10 (`task-1-final-continuation.log:641`); import baseline 15/15 (`task-1-baseline-import-admitted.log:639`) and final 15/15 (`task-1-final-import.log:645`). All have zero failed/ignored tests.
- Required refusal tests are individually present and passing in baseline (`task-1-baseline-continuation-admitted.log:626`, `:627`, `:629`, `:632`) and final (`task-1-final-continuation.log:633`, `:634`, `:636`, `:639`).
- Confirmed final qualified-import tests-target check completed (`task-1-final-check-qualified.log:3517`). Regression runs precede the final import qualification; that qualification is visible in the reviewed diff and the final compile covers it. No specific unanswered behavioral doubt warranted a rerun.
- Compared the saved boundary log bytes directly: identical. The initially verbose evidence output was truncated; recovered the relevant final check/warning summaries and boundary totals with a focused log read. No source outside the diff was inspected.

## Assessment

- **Spec compliance: PASS.** All four requested file changes are present and the bounded semantics-preserving extraction is supported by the diff and saved evidence.
- **Task quality: NEEDS RESOLUTION under the supplied rubric.** No behavioral regression or Critical issue was found. The sole Important item is the explicitly plan-mandated duplication above; controller adjudication is required before treating this as an unqualified quality approval. Existing warnings and platform debt remain nonblocking for Task 1 and do not constitute platform qualification.
