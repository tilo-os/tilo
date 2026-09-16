# Task 1: verified fresh-destination folder copy

Status: implemented and locally verified; native Fedora verification passed. Native Windows runtime verification remains a release gate.

## Scope and implementation

Implemented `copy_folder(source, destination, cancelled, progress)` in `apps/imager/src/setup/migration.rs`, exported from `setup/mod.rs`.

- Explicit native absolute grants are independently opened. Every path component is pinned with a no-follow handle and observed identity; source/destination ancestry is revalidated before output creation and publication. Symlink components and overlapping source/destination grants are refused. Destination creation is exclusive and never merges into existing content.
- Moved the scanner's unchanged `metadata_nofollow`, `open_directory_nofollow`, and identity comparison into focused `setup/nofollow.rs`. Existing scanner callsites and race tests still use those exact checks. No native-installation or view changes.
- Recursively handles ordinary files and empty directories through pinned parent capabilities. Access failures, changed entries, links, special entries, unsupported Windows reparse/cloud-placeholder attributes, and destination name errors become explicit relative-path outcomes. `OsString` names are preserved without lossy conversion.
- Copies through a bounded 64 KiB buffer to an exclusive operation-owned temporary file. Flushes/syncs, rereads the destination, compares SHA-256, rereads source content, and revalidates source identity, length, modification time, permissions and relevant platform metadata before publication. Source handles are read-only.
- Publishes with an atomic hard link that cannot clobber an existing destination. A conflicting entry is retained unchanged; the source remains unchanged. A filesystem without hard links fails explicitly.
- Cancellation is checked between entries and stream chunks and before publication. Verified output remains in place. Temporary cleanup first checks the pinned parent and exact temporary-file identity and never deliberately unlinks a replacement or recursively removes output.
- Copies access/modification times for files and directories and verifies exact timestamp preservation. Linux O_PATH directory handles and Windows read-only scanner handles require a separate destination-only no-follow open with identity checking; Windows requests FILE_WRITE_ATTRIBUTES only for this destination handle.
- New Unix directories/files use private 0700/0600 permissions, subject to the process umask. Windows uses destination security inheritance. Foreign ownership and original permission modes are never restored.
- `CopyReport` lists verified paths and bytes, skipped/failed relative paths, interruption/enumeration state and fidelity disclosures. `CopyProgress` reports only verified counts/bytes plus the current relative path. General metadata limitations are disclosed separately; timestamp failures make the report incomplete.

## Verification

All fixtures use real native absolute temporary paths; no personal files were imported.

### Red/green evidence

- Initial `cargo test -p imager --lib setup::migration`: compiling stub, 0 passed / 5 expected failures. Log: `/tmp/imager-migration-red.log`.
- Expanded initial tests: 0 passed / 7 expected failures before implementation. Log: `/tmp/imager-migration-red-2.log`.
- Implemented engine: 7 passed / 0 failed. Log: `/tmp/imager-migration-green-1.log`.
- Actual copy/stage/publication boundary tests: 15 passed / 0 failed. Log: `/tmp/imager-migration-boundaries.log`.
- Mutation check disabling destination digest rejection: corruption test failed on unsafe successful publication (14 passed / 1 failed), then production checks restored. Log: `/tmp/imager-migration-mutation-red.log`.
- Mutation check disabling source digest/metadata rejection: same-length content edit with restored mtime failed on unsafe successful publication (0 passed / 1 failed), then production checks restored. Log: `/tmp/imager-migration-source-red.log`.

### Final results

- macOS ARM64: `cargo test -p imager --lib setup::`: **59 passed, 0 failed**, including all 15 migration tests and the scanner tests after extraction. Log: `/tmp/imager-migration-setup-green.log`.
- Fedora ARM64, native VM verification performed by root after syncing the stable four source files: `cargo test --locked -p imager --lib setup::`: **58 passed, 0 failed**, including all 15 migration tests and directory timestamp preservation; no warnings. Root session: `48855`. The different total reflects platform-gated existing setup tests / workspace snapshot.
- `cargo clippy -p imager --lib --no-deps`: exit 0; no migration/scanner errors or warnings. Existing unrelated `apps/imager/src/model.rs:285` needless-return warning remains. Log: `/tmp/imager-migration-clippy-lib.log`.
- `cargo clippy -p imager --lib --tests --no-deps -- -A clippy::panic_in_result_fn -A clippy::float_cmp`: exit 0. Exact command-level allowances are for unrelated test errors in `setup/view.rs:1288` (panic worker spy returns Result) and `main.rs:293–294` (screenshot-theme float assertions). Existing main.rs needless-borrow/model.rs needless-return warnings remain. No migration lint suppressions beyond the justified non-Windows Result-return helper cfg allowance. Log: `/tmp/imager-migration-clippy-tests.log`.
- Unsuppressed test clippy identified the unrelated issues above; root has been informed and owns the follow-up. No unrelated files were edited to address them.
- `git diff --check -- apps/imager/src/setup`: clean.

Tests cover copied bytes, empty directories, unchanged originals, overlap/existing destinations, cancelled initial grants, symlink roots, replaced destination parent, unsupported link/socket entries, private Unix modes, timestamp/fidelity reporting, corrupt staged bytes, changed source content despite restored mtime, publication conflicts, replaced temporary files, retention of verified output after cancellation, cancellation after staging, Unicode names / multiple chunks, and a descendant swapped to a link. Windows has a platform-specific parent-pinning test: cap directory handles deny rename rather than allowing the Unix replacement fixture.

## Limits and follow-up

- Task 1 is a fresh-destination engine only. There is no durable journal/receipt, restart protocol, merge/resume entry point, UI integration, or installation/handoff change. Task 2 must revalidate retained content and preserve fidelity warnings rather than trusting progress as authority.
- Native Windows tests have not run; no Windows VM/runner was available. Windows uses capability-library no-follow/reparse protection, pinned rename-denying directory handles and destination-only attribute-write access, but runtime/NTFS validation is still required before an advertised support claim.
- The source is not a filesystem snapshot or locked against writers. A detected change is incomplete/retryable; copying does not establish atomic consistency across multiple files or prevent later external edits. Filesystem-managed access-time updates can occur when reading originals.
- Ownership, original permissions, ACLs, extended attributes, alternate streams, creation times, sparse allocation and hard-link relationships are not preserved, and this is explicitly disclosed in every report. Supported timestamp failures are not silently downgraded.
- Known Windows reparse/offline/recall placeholder flags are refused; there is no general provider-specific POSIX cloud-placeholder adapter. Ordinary readable files are verified as exposed by the host filesystem.
- Hard-link support and exact access/modification timestamp preservation are required for success. Filesystems that lack these semantics return explicit failures. Nesting beyond 128 traversed levels is explicitly incomplete; file descriptors track active ancestors rather than all discovered descendants.
- Cleanup conservatively leaves a temporary entry if its parent/path identity can no longer be trusted or removal fails. It never cleans an entire partially copied destination; already verified files remain for an explicitly authorized later resume.
- This is unprivileged copying into user-controlled fresh directories. Source/destination validation does not grant collaboration sharing, cloud upload, synchronization, execution or privilege escalation.

## Repository hygiene

Initial base was `b7c998714`; resumed after independent view fixes at `2fdba6dbc`. Unrelated working-tree changes and concurrent commits were preserved. Only `migration.rs`, `nofollow.rs`, `folders.rs`, `setup/mod.rs`, and this report belong to the scoped commit. Cargo manifests/lockfile were not staged or changed for this task.
