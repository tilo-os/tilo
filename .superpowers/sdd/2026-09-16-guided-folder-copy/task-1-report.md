# Task 1: verified fresh-destination folder copy

Status: implemented, including review fix round 1; native macOS/Fedora verification passed. Ready for namespace-security re-review. Native Windows runtime verification remains a release gate.

## Scope and implementation

Implemented `copy_folder(source, destination, cancelled, progress)` in `apps/imager/src/setup/migration.rs`, exported from `setup/mod.rs`.

- Explicit native absolute grants are independently opened. Every path component is pinned with a no-follow handle and observed identity; source/destination ancestry is revalidated before output creation and publication. Symlink components and overlapping source/destination grants are refused. Destination creation is exclusive and never merges into existing content.
- Moved the scanner's unchanged `metadata_nofollow`, `open_directory_nofollow`, and identity comparison into focused `setup/nofollow.rs`. Existing scanner callsites and race tests still use those exact checks. No native-installation or view changes.
- Recursively handles ordinary files and empty directories through pinned parent capabilities. Access failures, changed entries, links, special entries, unsupported Windows reparse/cloud-placeholder attributes, and destination name errors become explicit relative-path outcomes. `OsString` names are preserved without lossy conversion.
- Copies through a bounded 64 KiB buffer to an exclusive operation-owned temporary file. Flushes/syncs, rereads the destination, compares SHA-256, rereads source content, and revalidates source identity, length, modification time, permissions and relevant platform metadata before publication. Source handles are read-only.
- Publishes with an atomic hard link that cannot clobber an existing destination. A conflicting entry is retained unchanged; the source remains unchanged. A filesystem without hard links fails explicitly.
- Cancellation is checked between entries and stream chunks and before publication. Verified output remains in place. Temporary cleanup first checks the pinned parent and exact temporary-file identity and never deliberately unlinks a replacement or recursively removes output.
- Copies access/modification times for files and directories and verifies exact timestamp preservation. Linux O_PATH directory handles and Windows read-only scanner handles require a separate destination-only no-follow open with identity checking; Windows requests FILE_WRITE_ATTRIBUTES only for this destination handle.
- New Unix directories/files use private 0700/0600 permissions, subject to the process umask. Windows directories receive an explicit protected private DACL at creation; their children inherit only that private policy. Foreign ownership and original permission modes are never restored.
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

### Initial implementation results

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


## Review fix round 1: protected destination namespace (latest)

Both P1 findings are addressed by establishing the destination namespace's permissions before the first creation and revalidating that protection before subsequent name-based writes. A post-mkdir identity observation alone was not sufficient: another account with parent write access could swap the child before observation; the same access could race link/unlink after validation. These findings were reproduced and are superseded by the controls below.

### Protection and trust boundary

- New focused `setup/destination_security/{mod,unix,macos_acl,windows}.rs` modules hold platform permission logic. Source permissions, source compatibility and the reviewed scanner primitives are unchanged.
- `PinnedDirectory::validate_destination` examines the full pinned ancestor chain, including the protected next child's actual observed metadata. It runs before mkdir, before temporary creation, before verification/publication/cleanup, and before destination timestamp writes. Created directories are marked as requiring private security, with additional checks on every new temporary file.
- Unix requires root/current-effective-user ownership throughout the destination ancestry. Group/other-writable ancestors are refused unless sticky semantics protect a root/current-user-owned child. Sticky bits alone do not authorize foreign-owned descendants. Every fresh directory/file must be current-user-owned with no group/other mode permissions.
- Linux explicitly supports ext-family, XFS, Btrfs, tmpfs and overlay filesystem permission semantics, where POSIX ACL masks limit inherited grants to the 0700/0600 creation mode. Unknown/network/FUSE/FAT semantics are refused rather than treated as mode-enforced privacy.
- macOS requires local APFS/HFS with ownership enforcement enabled (`MNT_IGNORE_OWNERSHIP` is refused). Native handle-based Darwin extended ACL inspection rejects foreign mutation grants, inherit-only mutation grants, inheritable foreign disclosure grants, unknown allow types/permission bits, and any nontrusted access on new private objects. Harmless non-inheritable read or deny ACLs remain compatible. Native APFS fixtures established that an open descriptor without an extended ACL returns NULL/ENOENT; that specific no-ACL result is accepted only after supported-filesystem verification.
- Windows queries owner/DACL using `GetSecurityInfo` on pinned handles. It trusts the current user, SYSTEM, Administrators and the known privileged TrustedInstaller owner for OS ancestry; NULL/unavailable/unknown/conditional ACL forms fail closed. Nontrusted mutation rights (including DELETE_CHILD, WRITE_DAC, WRITE_OWNER, data/attribute/EA writes) are refused. Ordinary ADD_SUBDIRECTORY-only ancestor rights remain allowed because they cannot replace a protected existing child, and exclusive creation refuses collisions.
- Windows creation uses `CreateDirectoryW` with an explicit protected SDDL descriptor granting current user/SYSTEM/Administrators with child inheritance. There is no permissive inherited-ACL window before a later chmod/ACL fix. The spelling is derived from the pinned parent handle while the whole cap directory chain denies rename/delete; the result is reopened no-follow and checked for expected ownership, private DACL and protected status. Only local fixed/removable volumes reporting persistent ACL support are considered; remote/unknown volume types are refused.
- **Threat boundary:** malicious processes already running as the same account or with administrator privileges are excluded. Such principals can alter final output regardless of this protocol. Identity checks still detect controlled same-account replacements at observable boundaries, but this implementation does not claim unconditional immutability against those actors. Other-account writable namespaces are refused before adoption, publication or cleanup.
- No source or pre-existing parent permission mutations were introduced. Unsupported security returns an explicit error, leaving unrelated entries untouched. Once a destination loses trusted permissions, publication and cleanup stop; owned temporary entries may remain rather than risk an unsafe unlink.

### Fix-round verification

- Added native filesystem regressions before implementation. The ordinary repository RED command initially hit unrelated shared `ui/src/app.rs` compilation errors; the exact-source isolated harness then produced **1 passed / 5 failed**, catching unsafe direct parent, unsafe ancestor, changed permissions during staging, macOS effective write ACL and macOS inherit-only write ACL. Log: `/tmp/imager-namespace-isolated-red.log`. The initially passing case proved safe sticky paths remained usable.
- Final macOS repository `cargo test -p imager --lib setup::`: **69 passed / 0 failed / 1 explicitly ignored**. The ignored test requires root solely to create a disposable foreign-owned child. Log: `/tmp/imager-namespace-repo-test-final.log`.
- Exact-source macOS isolated suite: **22 passed / 0 failed** before adding the opt-in root test, covering all migration cases plus namespace refusals and harmless read-only ACL compatibility. Log: `/tmp/imager-namespace-unix-green.log`.
- Root's native Fedora full repository setup suite: **65 passed / 0 failed**, no warnings, session `99904`. This was before adding the explicit ignored root-only fixture; no Linux production behavior changed afterward.
- Root's native Fedora isolated source suite on XFS: **19 passed / 0 failed**, no warnings, session `78542`.
- Root repeated those same **19 passing** tests with TMPDIR on a freshly formatted disposable 256 MiB Btrfs loop volume. The volume was unmounted/detached/removed afterward; no retained-OS/host disks were used. Repeatability script: `.superpowers/sdd/2026-09-16-guided-folder-copy/test-native-btrfs.sh`.
- Root built the opt-in sticky/foreign-owner test as the ordinary guest user and ran only its compiled executable as root: `--exact migration::tests::namespace_sticky_parent_does_not_trust_a_foreign_owned_child --ignored`: **1 passed / 0 failed**. This changes ownership only on the newly created empty fixture child. Repeatability script: `.superpowers/sdd/2026-09-16-guided-folder-copy/test-native-sticky.py`.
- Exact repository sources and all Windows tests compile with `cargo check --manifest-path /var/folders/p6/fg2jh4md76b0fd2s6m_qcpv40000gn/T/qos-copy-windows-check-fu19as05/Cargo.toml --target x86_64-pc-windows-msvc --tests`: exit 0. All **77** harness dependency name/version/source/checksum identities match the repository lock. Log: `/tmp/imager-namespace-windows-check-final.log`.
- Three native Windows ACL tests are included and compile: unsafe DELETE_CHILD parent rejection before mkdir; safe ADD_SUBDIRECTORY-only ancestor compatibility; and protected child/private copied-file ACLs despite permissive inherit-only parent rules. **These Windows tests have not been executed natively.** The full Windows application cross-check remains separately blocked by native ring/Windows SDK headers; isolated compilation does not substitute for native runtime evidence.
- Exact-source isolated clippy passes on both macOS and the Windows target with `--lib -- -D clippy::pedantic -A clippy::must_use_candidate -D clippy::arithmetic_side_effects -D clippy::indexing_slicing`. The must-use allowance matches the repository configuration. Logs: `/tmp/imager-namespace-clippy-final.log`, `/tmp/imager-namespace-windows-clippy.log`.
- Repository `cargo clippy -p imager --lib --tests --no-deps -- -A clippy::float_cmp`: exit 0. The sole allowance is for unrelated `main.rs:293–294` screenshot-theme float assertions. Existing unrelated model.rs needless-return and main.rs needless-borrow warnings remain; the previous view panic-spy lint was fixed by its owning task. No new migration/security lint warnings. Log: `/tmp/imager-namespace-repo-clippy-final.log`.
- Scoped `git diff --check`: clean.

### Remaining scope and repository hygiene

This closes the reviewed other-account destination replacement windows under the stated trust boundary. It does not add durable resume, UI integration, or installation changes. Native Windows user-temp/NTFS execution and representative permission configurations remain a release gate; unsupported ACL/filesystem configurations must remain explicit refusals until separately validated.

The fix round adds only a narrow Windows-target `windows-sys 0.61.2` dependency with Foundation/Security/Authorization/FileSystem/Threading features, and its single `imager` dependency entry in Cargo.lock. Existing registry package identities are reused. The unrelated scratchpad/root manifest/lockfile work is preserved and not staged. Source changes are limited to migration integration, the private setup module declaration, and the focused destination-security modules, plus this report. Independent commits that appeared during implementation were preserved.
