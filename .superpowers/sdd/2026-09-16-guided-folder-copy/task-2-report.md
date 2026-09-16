# Task 2 implementation report

Status: DONE_WITH_CONCERNS. Durable journal/resume is implemented and verified on native macOS and Fedora Linux. Windows journaled start explicitly refuses before mutation; native Windows directory-entry persistence implementation and runtime verification remain release work, as approved by the controller. The existing nonjournal `copy_folder` API remains available on Windows.

## Implementation

- Added `copy_folder_with_journal` and `resume_folder`, exported both from `setup::migration_journal` and `setup::migration`. Existing `copy_folder` API is preserved.
- One exclusively created private external journal contains an exact version-1 serde schema with operation identifier, lossless native root/component paths, native source/destination identities, verified file sizes/SHA-256 digests/source snapshots, directory identities/original timestamps/completion flags, and final per-path outcomes/metadata disclosures. No access grant or secret is serialized.
- Journal creation uses an exclusively created private temporary, file persistence, no-replace hard-link publication, and parent persistence. Updates persist a private replacement before atomic rename and then persist the parent. Every mutation validates the pinned private namespace and current journal identity. Existing or replaced operation receipts are not silently adopted or overwritten.
- Native paths use Unix bytes or Windows UTF-16 plus platform marker. Decode rejects platform mismatch, NUL, empty/non-normal relative components, parent/root traversal, multiple components within one name, duplicates, excessive depth/length, unknown fields/schema versions, and oversized journals. Journal files are bounded to 16 MiB. Resume refuses a journal relocated into either copied tree.
- Engine integration records receipts only after verified file publication and destination directory persistence. Created directories, including the root and empty directories, have durable identity receipts before traversal. Their original timestamps survive interruption. Completed directories and final outcomes are durably recorded.
- Resume pins current roots, rechecks root and destination namespace security, verifies identities/source facts, and independently hashes existing source and output before counting a file. Metadata-preserving rereads restore original supported destination timestamps. Modified/missing/replaced files, changed roots, unrecorded output, and uncertain identities become explicit incomplete results/errors requiring review. Receipts never permit replacing output contents. Original source files are never opened for writing; ordinary filesystem access-time effects of reads remain possible.
- Resume uses no-follow, nonblocking opens and checks opened identity against observed metadata, including a test for a regular source swapped to a FIFO between stat and open.
- macOS uses descriptor-based volume UUID (`fgetattrlist`) plus native inode, `fsync`, and `F_FULLFSYNC` for persistent files/directories. Linux uses descriptor-based `FS_IOC_GETFSUUID` when available and file/directory `fsync`. Unsupported generic UUID lookup has an explicitly boot-bound device fallback that refuses cross-boot automatic resume. Btrfs uses native FS_INFO filesystem UUID plus GET_SUBVOL_INFO tree ID/subvolume UUID and inode, distinguishing snapshot inode aliases. Unsupported Btrfs identity evidence is refused rather than ambiguously downgraded.

## TDD evidence

Harness manifest: `/var/folders/p6/fg2jh4md76b0fd2s6m_qcpv40000gn/T/qos-copy-windows-check-fu19as05/Cargo.toml`. It imports the exact repository source files, with repository-compatible dependency versions.

1. Initial RED: `cargo test --manifest-path <harness> migration_journal::tests::restarts -- --nocapture` failed with `durable resume is not implemented` after a real first file was copied and cancellation prevented the second. GREEN: same test passed after journal integration, with both outputs and originals verified.
2. Timestamp RED: `... migration_journal::tests::resumed_files_and_directories -- --nocapture` failed with destination access time `1600000060` instead of original `1600000000`, simulating access-time advancement on mounts where reads update atime. GREEN after persisting and restoring receipt timestamps.
3. Publication-boundary RED: `... migration_journal::tests::crash_after_publication -- --nocapture` initially found child exit code 0 instead of required abrupt exit 73. After wiring test-only exits at the actual durable-publication and receipt-replacement boundaries, all boundary tests passed. The subprocess exits bypass destructors; tests reopen the actual filesystem/journal state. Unrecorded output is preserved as a conflict, and a replaced receipt is revalidated.
4. Cancellation RED: `... migration_journal::tests::cancelled_start -- --nocapture` failed because an already-cancelled start created a journal. GREEN after early cancellation handling without mutation.
5. Relocated-journal RED: `... migration_journal::tests::resume_refuses -- --nocapture` failed because resume accepted a journal moved inside output. GREEN after pinned containment validation before traversal or journal update.
6. FIFO race RED: `... migration_journal::tests::source_swapped_to_fifo -- --nocapture` failed after a bounded 3-second child timeout: `resume blocked opening a swapped FIFO`. GREEN after nonblocking no-follow opens. Test fixture is parent-owned so child termination cannot prevent fixture cleanup.
7. Btrfs RED (controller-run native Fedora): strengthened `migration_durability::tests::distinguishes_btrfs_subvolumes_and_snapshot_file_identities` failed with `Btrfs must have persistent identity, not boot/device fallback`, left/right `Some(2)`, 0 passed/1 failed. GREEN after Btrfs native identity implementation: 1 passed/0 failed. The earlier weaker identity-inequality test passed because the same-boot fallback had different `st_dev` values; it was NOT valid RED evidence. Native probes confirmed generic FS_IOC_GETFSUUID returned ENOTTY for Btrfs while working on Fedora XFS.
8. Full-app integration initially exposed four subprocess-test failures because hardcoded harness module names selected zero tests in the imager binary. Worker names now derive their actual module prefix, and the FIFO worker leaves an asserted completion marker. Full native app suites pass.

Additional cases cover changed output/source, digest changes with restored mtime and matching size, missing source, replaced roots, existing/replaced journals, malformed schema/traversal, journal permissions/symlinks, platform/boot mismatch, nested/empty directories and persisted outcomes, and Linux non-UTF-8 filename round trips. APFS rejects invalid UTF-8 filenames at fixture creation, so the real raw-byte filename test is Linux-specific. Windows has a cfg-compiled refusal-before-mutation test.

## Final verification

All results below are terminal successful tool results, not inferred from compilation in another environment.

- macOS ARM64 repository: `cargo test -p imager setup:: --lib`: **91 passed, 0 failed, 1 ignored**, 133 filtered. Ignored case is the preexisting opt-in foreign-owned sticky-child fixture requiring root.
- macOS focused journal suite: `cargo test -p imager setup::migration_journal --lib`: **22 passed, 0 failed** (includes two subprocess helper entrypoints).
- macOS exact-source scoped lint: `cargo clippy --manifest-path <harness> --all-targets -- -D warnings`: **exit 0**, no warnings.
- Windows x86_64 MSVC exact-source cfg check: `cargo check --tests --target x86_64-pc-windows-msvc --manifest-path <harness>`: **exit 0**, no warnings. This is compilation evidence only; there is no native Windows runtime evidence. The full repo Windows build's missing SDK header issue was preexisting and is not represented as resolved.
- Native Fedora ARM64 repository (controller-run): `/home/parallels/.cargo/bin/cargo test -p imager setup:: --lib` with the configured guest target cache: **88 passed, 0 failed, 2 explicitly ignored**, 135 filtered; exit 0, no warnings. Actual imager subprocess crash/FIFO cases passed. The extra ignored case is the opt-in Btrfs fixture, run separately below.
- Native Fedora exact-source full copy/journal suite on disposable Btrfs (controller-run): **42 passed, 0 failed, 2 explicitly ignored**, exit 0, no warnings.
- Native Fedora opt-in Btrfs persistent identity test (controller-run): **1 passed, 0 failed**, 43 filtered; exit 0. Fixtures include distinct subvolume roots with inode 256 and an original/snapshot ordinary file sharing inode number. Test runs as ordinary `parallels`; elevated controller work only provisions disposable loop fixtures. Controller verified no fixture directory or loop mappings remain.
- Native ABI probe compiled against Fedora headers: BTRFS_IOC_FS_INFO size 1024, fsid offset 16, ioctl `0x8400941f`; GET_SUBVOL_INFO size 504, UUID offset 296, ioctl `0x81f8943c`. Linux fsuuid2 is 17 bytes (`u8 len; u8 uuid[16]`), ioctl `0x80111500`.
- `rustfmt --edition 2024` applied to changed implementation files; scoped `git diff --check` clean.

Native controller scripts/evidence: `.superpowers/sdd/2026-09-16-guided-folder-copy/test-native-btrfs-identity.sh`, `probe-btrfs-identity.py`, `probe-native-fsuuid.py`, plus controller terminal sessions. These files are controller-owned and are not included in this implementation commit.

## Self-review and limits

- Fixed independently observed timestamp, cancellation, misplaced-journal, FIFO-race and subprocess-prefix defects before handoff. Source/destination mutation and journal paths use the reviewed pinned primitives; namespace protection continues to exclude hostile same-account/administrator processes.
- Journal storage retains unrelated/unrecorded output and never recursively cleans destinations. Abrupt exits can leave exclusive temporary files; they are intentionally not removed based on receipts.
- Process interruption is tested at actual publication and receipt replacement points. Tests do not simulate sudden physical media power loss or prove storage hardware honors flushes; implementation invokes supported native persistence operations and returns errors when they fail. Actual reboot/resume has not been exercised; stable native identity semantics and boot-mismatch refusal are verified independently.
- Windows durable copying is unfinished release work, explicitly refused before mutation. Windows native runtime testing is unavailable; cfg checks do not substitute for it.
- Entire versioned JSON snapshots are rewritten per receipt, with a 16 MiB safety cap. This is simple and auditable but has quadratic total serialization/I/O as receipt count grows; exceeding capacity produces an explicit incomplete operation and preserves output. No scale/performance claim is made.
- `migration_journal.rs` is roughly 1,200 lines, about half tests. Its production portion remains one journal/recovery responsibility. The controller approved retaining this organization rather than splitting solely by line count. No UI/install/sync behavior was added.

## Files in implementation commit

- `apps/imager/src/setup/migration_journal.rs`
- `apps/imager/src/setup/migration_durability.rs`
- narrow integration in `apps/imager/src/setup/migration.rs` and `mod.rs`
- this report

## Review fix round 1 — stop after persistence failure

Addressed the Important finding in `task-2-findings.md`. The original implementation treated receipt failure like an ordinary entry failure, could publish later entries, and could discard its accumulated report when final persistence failed.

Changes:

- Added typed internal persistence-boundary errors and explicit poisoned journal state. There is no error-string matching. Receipt construction/encoding/capacity, journal writes/replacement/flush, and output publication/directory flush failures propagate as fatal persistence errors through every recursive traversal level.
- A poisoned operation stops before the next entry and never retries the final checkpoint. Already published/unrecorded output is retained; unsaved receipts are not included in verified counters. Initial root receipt failures and final outcome-save failures return an explicit incomplete `CopyReport` rather than losing accumulated results through a top-level error.
- Diagnostics distinguish a retained earlier checkpoint, visible replacement with uncertain persistence, unpublished output, published output without a receipt, uncertain output metadata/publication persistence, and an unavailable/replaced journal whose prior checkpoint must not be trusted. The last distinction avoids claiming a checkpoint was retained when journal validation itself failed.
- Existing conflict behavior remains: a later explicit resume revalidates any receipt actually present, and an output without a saved receipt remains an existing conflict. No file/directory cleanup or overwrite authority was added.

TDD evidence:

- RED command: `cargo test --manifest-path /var/folders/p6/fg2jh4md76b0fd2s6m_qcpv40000gn/T/qos-copy-windows-check-fu19as05/Cargo.toml migration_journal::tests::failed_ -- --nocapture`. Result: **0 passed, 4 failed**. Failures were `later file published after before_encode failure`, `parent traversal continued after directory checkpoint failure`, and missing retained reports for root/final persistence errors. These were real copies and journal updates with deterministic failure hooks at actual persistence boundaries.
- GREEN: same initial command **4 passed, 0 failed** after fatal propagation and report retention. Expanded final journal suite: `cargo test --manifest-path <same harness> migration_journal -- --nocapture`: **27 passed, 0 failed**. Five failure regression tests exercise matrices of file receipt encoding, before/after journal replacement, post-publication directory flush, receipt construction, root publication, directory receipt/completion, and final outcome persistence. They assert no later file/directory publication, unchanged source contents, retained output/report, valid prior checkpoint or explicit uncertainty, and safe resumed conflicts.
- Diagnostic-strengthening RED: `... migration_journal::tests::replaced_journal_is_never -- --nocapture`: **0 passed, 1 failed**, because a replaced journal incorrectly had a `previous journal checkpoint retained` prefix. GREEN after a distinct `JournalUnavailable` typed boundary: **1 passed, 0 failed**. Existing replaced-journal behavior now returns the retained incomplete report, which is the requested contract, while preserving replacement bytes.

Verification:

- macOS actual repository amended component suite: `cargo test -p imager setup::migration --lib`: **49 passed, 0 failed, 1 ignored**, 180 filtered; exit 0. This covered engine + journal changes before the final diagnostic-only delta; the targeted replaced-journal regression then passed on the final delta.
- Native Fedora/Btrfs amended exact-source copy+journal suite, controller-run session 77925: **47 passed, 0 failed, 2 explicitly ignored**, exit 0, no warnings. Includes all five new persistence failure tests.
- Final native Fedora replaced-journal diagnostic regression, controller-run session 99802: **1 passed, 0 failed**, 48 filtered, exit 0, no warnings.
- Final scoped `cargo clippy --manifest-path <same harness> --all-targets -- -D warnings`: exit 0, no warnings.
- Final Windows `cargo check --tests --target x86_64-pc-windows-msvc --manifest-path <same harness>`: exit 0, no warnings. Test guard types are cfg-limited to supported runtime platforms; this remains compilation-only evidence.
- `rustfmt` applied and scoped `git diff --check` clean. No change to durability platform support, schema, or public function signatures.

Deferred scale finding: controller's separate native Fedora XFS release-build probe (session 9757) copied 100 × 1 KiB files in 182 ms with an 85,814-byte journal, and 1,000 × 1 KiB files in 2,181 ms with an 842,713-byte journal. Complete reports were asserted and temporary fixtures cleaned. These are bounded local measurements only, not unrestricted-scale support or physical-storage guarantees. The 16 MiB cap and quadratic snapshot/index cost remain for final component review and workload validation, not accepted as permanent product limitations.

Scoped fix files: `migration.rs`, `migration_journal.rs`, and this report. No subagents or unrelated files were changed by this fix.
