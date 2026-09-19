### Spec Compliance

- ❌ One compatibility defect: final-filename validation in `crates/system/atomic-file/src/lib.rs:165–171` returns untyped I/O errors, losing the existing `PublicationError(NotPublished)` classification. The bounded Unix provisioning protocol otherwise matches the approved design.
- ✅ `crates/system/atomic-file/src/provision.rs:38–70` separates Partial from ReadyToCommit and makes Partial retry a no-op error. Submitted non-EEXIST mkdir failures become NamespaceUnknown at lines 267–270; retained prefix/name custody and known earlier creation remain owned.
- ✅ `crates/system/atomic-file/src/provision.rs:283–329` admits the complete leaf, validates bindings/privacy, executes real leaf-to-root fsync including `/`, and revalidates afterward. Fresh calls and complete retries replay the entire chain. No cleanup or prefix-as-leaf success path was found.
- ✅ `crates/system/atomic-file/src/lib.rs:176–186` passes the prepared held leaf directly to publication; `crates/system/settings-store/src/lib.rs:201–204` reaches that production wrapper and preserves pretty JSON plus newline.
- ⚠️ Native Linux ACL, supplied mount/firmlink, Windows bootstrap/barrier, and crash/power-loss qualification remain outstanding as declared in `task-4-unix-report.md:64–66`. This review does not close full Task 4 or cross-platform acceptance. Parent owns shell test compilation.

### Strengths

- `crates/system/atomic-file/src/provision.rs:202–285` validates the unresolved suffix before mutation, admits existing aliases once, walks forward with held no-follow authority, and keeps privacy obligations for created/adopted nodes separate from traversal ancestors.
- `crates/system/atomic-file/src/native/unix.rs:90–121` reuses the accepted native permission-profile and macOS ACL checks before private mkdir without requiring traversal-parent ownership or repairing permissions. The existing file security checks and exact-mode staging path remain unchanged.
- `crates/system/atomic-file/tests/parent_provision.rs:35–129,145–264` observes actual barrier/staging boundaries, checks full-chain replay, distinguishes complete/partial retry, and labels synthetic mkdir-result coverage accurately. Source replacement, alias changes, private modes, process restart, and hook unwinding have focused coverage.
- `crates/system/atomic-file/src/test_support.rs:283–350,388–433` keeps provision events owned and handle-free, restores nested hooks on unwind, invokes observers before injected failure, and reports Synced only after native success. Parent independently confirmed test-support is absent from the settings normal/build graph.

### Issues

#### Critical (Must Fix)

None.

#### Important (Should Fix)

- **Preserve typed NotPublished errors during early filename validation.** `crates/system/atomic-file/src/lib.rs:165–171`: both parent-creating wrappers now return a bare `io::Error` for a missing final name or invalid component such as `bad\0name`. Previously, `publication::publish_path` wrapped missing-name errors in `PublicationError(NotPublished)` (`publication.rs:198–206`), and native `publish` wrapped component-validation errors the same way (`native/unix.rs:342`). Consequently `publication_state(&error)` changes from `Some(NotPublished)` to `None`, violating the design's unchanged file-publication error contract. Keep validation before any parent effects, but wrap these validation failures using `PublicationError::new(PublicationState::NotPublished, source).into_io_error()`. Extend the existing invalid-component regression (`tests/parent_provision.rs:15`) to assert original I/O kind, typed publication state/source, no directory state, and no parent creation for both wrappers; also cover a path without a final filename.

#### Minor (Nice to Have)

- `task-4-unix-report.md:58` records pre-existing vendor/automerge build warnings. These are external validation noise, not an introduced owned-source defect; retain their explicit attribution rather than describing all command output as pristine.

### Assessment

**Spec compliance:** Needs fixes for the typed-error regression above.

**Task quality:** Needs fixes. The custody and durability implementation is coherent and well covered; the compatibility error regression is localized and does not require changing its protocol.

### Review checks and evidence boundary

- Read the final snapshot-relative eight-file diff once, the task/review briefs, implementation report, binding Unix design, and global/Task 4 contracts. Diff output cut off portions of the new provisioner/hooks/test functions; read those cut portions directly to complete the review. No git-state inspection or mutation, product edits, subagents, or test reruns.
- Named outside-diff risk: held publication could reopen ambient paths or change file modes/error classification. Inspected `StorageDirectory::{identity,validate_privacy,publish_with_mode}`, `publication::{replace_with_mode,publish_path}`, and native `Directory::{open,from_file}`, `Stage::create`, `publish`, and `validate_private_security`. Held delegation and mode/security policy are preserved; early validation classification is the finding above.
- Named outside-diff risk: settings could bypass preparation or lose typed errors/format. Inspected `update_section_at` and `write_atomic`; they propagate the wrapper error directly and preserve serialization.
- Accepted parent-confirmed execution evidence without rerunning: 22 parent tests, 52 settings tests, 28 atomic-file library tests, 19 native-publication tests, 10 lease tests, both unchanged app-state canonical regressions, and Windows/Linux compile checks. Parent supplied final-source hashes, unchanged app-state/lock/Windows/probe checks, and normal/build feature-graph verification. Native execution and hardware qualification remain distinct.
