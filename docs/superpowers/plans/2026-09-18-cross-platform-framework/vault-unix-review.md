# Task 7 Unix source review

## Spec compliance

- ✅ **Spec compliant for the bounded Unix increment.** No critical or important source issue found. This is not acceptance of full Task 7/S03 or cross-platform qualification.
- Checked all nine files against the frozen-snapshot review package and its identity manifest, not HEAD. The production changes are the Unix dispatch (`crates/system/accounts/src/vault.rs:176`), private storage composition (`crates/system/accounts/src/vault/unix_storage.rs:1`), and framework namespace predicate (`crates/system/atomic-file/src/directory.rs:45`, `crates/system/atomic-file/src/native/unix.rs:93`). The remaining changes are the authorized dev-test edge (`crates/system/accounts/Cargo.toml:43`) and tests. No unrequested product behavior was added.
- ⚠️ Native Linux ACL/profile enforcement, second-principal exclusion, foreign-owned key fixtures, Windows namespace/durability implementation and execution, distribution qualification, and power-loss evidence remain unfinished. The Linux fixture at `crates/system/atomic-file/tests/namespace_control.rs:110` and successful cross-compilation do not establish native qualification. Non-Unix behavior remains expressly transitional at `crates/system/accounts/src/vault.rs:180`.
- ⚠️ Secret Service transport/removal remains the existing, separately unqualified protocol (`crates/system/accounts/src/vault/unix_storage.rs:252`). X01 account-journal recovery/concurrency and the Windows status-lock blocker are separate work. The Settings test at `apps/settings/src/panes/accounts.rs:2047` proves the positive vault-before-metadata ordering only. The parent still owns required shell integration validation.

## Strengths

- Namespace control is distinct from OwnerOnly privacy: the native predicate checks supported profiles, effective ownership, write exclusion, and the existing macOS ACL policy without chmod or a directory single-link rule (`crates/system/atomic-file/src/native/unix.rs:93`). Direct tests preserve the distinction and existing provisioning refusal (`crates/system/atomic-file/tests/namespace_control.rs:9`).
- Existing and missing roots take separate reconciliation/provisioning branches (`crates/system/accounts/src/vault/unix_storage.rs:100`). The shared key reader retains identity and a live handle, uses a fixed zeroizing 33-byte buffer, and admits exactly 32 bytes plus EOF (`crates/system/accounts/src/vault/unix_storage.rs:125`). Read-only load never enters provisioning, generation, or commit (`crates/system/accounts/src/vault/unix_storage.rs:303`; observer coverage at `crates/system/accounts/src/vault/unix_tests.rs:418`).
- Existing keys and actual collision winners pass `commit_existing` before encryption. Candidates are discarded before winner admission, staging exhaustion is not assumed to identify a winner, PublicationUnknown returns failure, and known commit failure has one bounded retained retry (`crates/system/accounts/src/vault/unix_storage.rs:176`, `:207`). Namespace refusal before retry preserves the original publication phase; the regression verifies it (`crates/system/accounts/src/vault/unix_tests.rs:594`).
- Tests exercise genuine old-layout subprocess creation and unchanged key/directory identity (`crates/system/accounts/src/vault/unix_tests.rs:54`, `:202`), two-process candidate competition (`:658`), equal-byte inode replacement (`:779`), and a persistent real commit checkpoint (`:914`). They exercise production save paths rather than a synthetic durability helper.
- Settings coverage uses the actual caller and verifies the vault-save error plus exact unchanged key, ciphertext, and metadata bytes (`apps/settings/src/panes/accounts.rs:2047`). Settings-store coverage checks typed publication phases and visible pretty JSON/newline behavior through its public facade (`crates/system/settings-store/src/lib.rs:506`). Existing formats, crypto, custody selection and original baseline assertions are preserved by the diff.

## Issues

### Critical

- None found.

### Important

- None found for this bounded source increment.

### Minor

- Recorded Settings validation output is not pristine: warnings include `apps/settings/src/region.rs:55`, `apps/settings/src/sidebar.rs:629`, `apps/settings/src/settings_app.rs:72`, and `apps/settings/src/sidebar.rs:189`, plus dependency/UI warnings (`/tmp/task7-final-settings-caller2.log:472`, `/tmp/task7-final-settings-positive.log:468`). These locations are outside this diff; do not expand this task to fix them, but retain them as separate cleanup debt rather than describing the run as warning-free.
- Concurrent inherited child output makes parts of the accounts log difficult to read (`crates/system/accounts/src/vault/unix_tests.rs:32`, subprocess invocations at `:658`; `/tmp/task7-final-accounts2.log:126`). The final parent result is intact. Capturing each child's output and attaching it on failure would improve future diagnostics without changing test assertions.

## Checks and evidence

- Read the binding brief, complete implementer report, approved compatibility design, review identity manifest, and the full 1,819-line diff in consecutive segments. Used the diff only for changed-source review; extracted line locations mechanically afterward. No git operation, product edit, test run, suite repeat, or subagent dispatch occurred.
- Focused outside-diff check, named risk: a symlink/nonregular/read error might be mistaken for absent key/ciphertext. Inspected the shared reader at `crates/system/atomic-file/src/native/unix.rs:189`: relative NOFOLLOW/NONBLOCK open, regular-file validation, privacy validation, and original errors confirm the consumer's narrow NotFound handling.
- Focused outside-diff check, named risk: existing-key commit might acknowledge another inode or omit a barrier. Inspected `crates/system/atomic-file/src/existing.rs:86`: retained/named identities are validated around file and parent synchronization; opening the commit handle is existing-only. This supports the new consumer composition at `crates/system/accounts/src/vault/unix_storage.rs:176`.
- Inspected existing logs rather than rerunning tests: accounts 47 passed (`/tmp/task7-final-accounts2.log:372`), atomic-file 89 top-level tests across its reported suites (`/tmp/task7-final-atomic2.log`), settings-store 53 passed (`/tmp/task7-final-settings-store.log:61`), and both Settings caller regressions passed (`/tmp/task7-final-settings-caller2.log:518`, `/tmp/task7-final-settings-positive.log:508`). No reported ignored tests.
- Inspected successful Linux compile-only output and Windows atomic-file/settings-store compile-only output. The combined Windows log records the eight accounts status.rs errors (`/tmp/task7-final-windows.log:71` onward); no native Windows success is inferred. No source issue discovered here warrants another focused test.

## Assessment

**Task quality: Approved for the bounded Unix increment.** The composition uses shared native authority and durability mechanisms, preserves ambiguous effects, and has meaningful adversarial coverage. Full Task 7/S03 remains open for the explicitly inventoried platform and qualification work.
