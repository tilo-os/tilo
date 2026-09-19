# Task 4 Unix fix 1 scoped rereview

## Finding disposition

**ADDRESSED — Preserve typed NotPublished errors during early filename validation.**

`crates/system/atomic-file/src/lib.rs:165–175` now wraps both missing-final-name errors and native component-validation errors with `PublicationError::new(PublicationState::NotPublished, source).into_io_error()`. Both validations still precede parent calculation and `provision_directory`, so the fix preserves the required pre-effects ordering. The native validation error is moved directly into the existing typed error; the unchanged constructor retains its source and `into_io_error` retains its I/O kind. No `ProvisionError` or directory-state classification is introduced.

The two focused regressions in `crates/system/atomic-file/tests/parent_provision.rs` each exercise both public parent-creating wrappers. They cover NUL and absent final filenames, assert InvalidInput, recover and downcast PublicationError(NotPublished), inspect the I/O source, require absent directory state, reject provisioning/publication checkpoints, and verify the fixture remains empty with no missing parent created. These assertions observe the production wrapper path.

## New breakage within the fix

**Critical: None. Important: None.** The localized wrapping change resolves the finding without changing provisioning, retry, directory barriers, file mode handling, or publication delegation. Unrelated observations are deferred.

## Evidence and boundary

- Read `task-4-unix-fix-1-review.diff` once (two files, supplied 3,647-byte diff), with the corresponding `task-4-unix-fix-1-before` source snapshot as baseline, not moving HEAD.
- Read the original Important finding, appended fix report, task brief, and relevant binding Unix design requirements for early filename validation and unchanged file-publication errors.
- Narrow outside-diff check: inspected the existing PublicationError constructor, I/O wrapper, and Error source implementation solely to verify original-source/kind preservation. Accepted the parent's final eleven-source hash verification and two-fix-file scope confirmation.
- Accepted supplied focused execution evidence for the identical `final_component_fails_before_parent_creation` filter: RED 0 passed / 2 failed, then GREEN 2 passed / 0 failed, with both wrappers covered in each test. No tests rerun; prior full-suite results remain pre-fix evidence.
- No product edits, git mutations, subagents, or broad review. This report is the only file written. Parent retains shell compilation and source-stability ownership.

**Assessment:** The reported filename typed-error finding is resolved; the bounded Unix fix is accepted. Native Linux ACL, supplied mount/firmlink, Windows bootstrap/barrier, and crash/power-loss qualification remain open. This does not close full Task 4 or cross-platform acceptance.
