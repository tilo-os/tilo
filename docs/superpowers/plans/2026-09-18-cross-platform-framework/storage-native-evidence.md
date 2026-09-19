# Native storage implementation evidence

Status: partial implementation evidence; native storage qualification is incomplete.

This record separates accepted source/test evidence from cross-target compilation and native qualification. It does not close S01, S02, or any cross-host audit finding. Tests recorded for an earlier source snapshot do not establish later changes passed.

## Host observed on 2026-09-18

- Native host: macOS 27.0, build 26A428, Darwin arm64 (`sw_vers`, `uname -sm`).
- Compiler: rustc 1.97.1 (8bab26f4f 2026-07-14).
- Filesystem/storage profile: not established by this observation. `diskutil info .` could not access DiskManagement in this environment. No APFS or crash-durability certification is inferred.
- Native Linux and Windows execution: no evidence recorded here. Installed cross targets are compilation tools, not native hosts.

## Accepted Unix publication foundation (Task 1)

The independently reviewed Task 1 implementation and its admission fixes are described in [publication-unix-review.md](publication-unix-review.md). This is historical evidence for that accepted snapshot; Task 2 is now changing identity/privacy sources.

| Verification | Recorded result | Scope and limits |
| --- | --- | --- |
| `cargo test -p atomic-file --features test-support --locked --offline` | 16 unit, 10 lease, 13 publication tests passed before review fixes | Native macOS; does not cover subsequent fixes or Task 2 |
| `cargo test -p atomic-file --test native_publication --features test-support --locked --offline` | 14 passed after review fixes | Native macOS; includes FIFO nonblocking admission regression |
| `cargo check -p atomic-file --test native_publication --target aarch64-unknown-linux-gnu --features test-support --locked --offline` | Passed after review fixes | Compilation only; Linux O_PATH rejection still needs execution |
| `cargo check -p atomic-file --tests --target x86_64-pc-windows-msvc --locked --offline` | Passed before review fixes | Compilation of temporary legacy Windows route; no Windows publication claim |
| `FFMPEG_DIR=/opt/homebrew/opt/ffmpeg@8 cargo check -p shell --tests --locked --offline` | Exit 0 after review fixes, 28.60s | Integration compilation with existing warnings; no native device or installed-bundle claim |

Task 1 final Unix backend SHA-256: `fe0d231aa7531c0019d111588bbd3a20880638e4fad0326b0485835246a1c1ce`. Later edits require fresh evidence; this hash is a snapshot anchor, not an assertion about current source.

A native temporary-file probe demonstrated an inherited allow-read ACL on a mode0600 file. Task 1 consequently rejects every extended ACL for semantic OwnerOnly admission. Task 2 must accept proven benign ACLs while retaining other-principal rejection. Neither a mode check nor same-principal ACL inspection qualifies distinct-principal denial.

## Required evidence still outstanding

| Obligation | Required observation | Current status |
| --- | --- | --- |
| Windows identity and private creation | Full file ID/volume identity, native creation ACL, read admission, impersonation and reparse/link rejection | Task 2 code accepted; native execution unproven |
| macOS ACL policy | Benign owner ACL accepted; additional principal access rejected before bytes and on reads | Task 2 native admission tests passed; distinct-principal qualification pending |
| Distinct-principal privacy | Actual separate principal cannot read/write the created private file | Not executed; no skipped test counts as proof |
| Linux native conformance | Production operations including O_PATH rejection and locking execute on supported host | Not executed in this record |
| Windows publication | Local NTFS admission, retained-handle replace/IfAbsent, phase errors and retry | Task 3 source accepted; native Windows execution unproven |
| First launch | New ancestor durability, partial failure and retry on native hosts | Task 4 pending; existing mkdir behavior is not qualification |
| Consumer recovery | Real app-state, identity cleanup, vault winner and receipt call paths | Task 6 identity recovery source accepted with native macOS tests; Tasks 5, 7, 8 pending |
| Power-loss durability | External harness drops volatile writes at acknowledged production checkpoints; verify after reboot | Task 9 pending; process kill and fault injection are insufficient |
| Distribution | Native installed bundles work without checkout/developer PATH | Pending broader platform roadmap |

Each new entry must identify exact source snapshot, command, host/filesystem/architecture, terminal result and limitations. Missing observation remains outstanding work; do not convert it into a pass or silently narrow the supported contract.

## Accepted identity/privacy implementation (Task 2)

The scoped [independent review](publication-identity-review.md) approved specification and code quality without findings. Accepted source snapshot hashes are recorded in [storage-task2-source-sha256.json](storage-task2-source-sha256.json); later Windows publication work is separate.

- Native macOS: `cargo test -p atomic-file --features test-support --locked --offline` exited0:16 unit+10 lease+17 publication tests,43 total, no failures/ignored. FIFO and umask child processes each actually ran one test.
- Windows compile only: `cargo check -p atomic-file --tests --features test-support --target x86_64-pc-windows-msvc --locked --offline` exited0.
- Linux compile only: `cargo check -p atomic-file --tests --features test-support --target aarch64-unknown-linux-gnu --locked --offline` exited0.
- Focused native macOS admission: owner/deny benign ACLs accepted, inherited Everyone-read allow rejected on creation and reads. These four tests prove ACL admission behavior on this host, not another account's actual denial.

Windows helper tests are written and compiled but unexecuted, including the actual separate-principal runner prerequisite. Windows publication and durable new-parent creation are not delivered by Task2. Required native Windows, Linux, distinct-principal and crash evidence remains outstanding.

## Accepted Windows publication source (Task 3)

[Independent review](publication-windows-review.md) approved the source implementation with no findings; [source hashes](storage-task3-source-sha256.json) identify that checkpoint. Complete Windows facade/legacy file writes reach the local-NTFS retained-handle implementation. Parent-creating wrappers still have transitional ancestor preparation; Task4 remains required.

- Final Windows MSVC test-target and default-feature library compilation passed, with no warnings.16 Windows-specific integration tests are compiled but unexecuted.
- Linux ARM64 test-target compilation passed. Native macOS suite43/43 passed before final Windows-only ownership/assertion changes and API documentation.
- Shared frozen-checkpoint `cargo check -p shell --tests --offline --locked` passed in7.36s with compatible FFmpeg8 pkg-config path and existing warnings. Parent verified exact command/log/exit, receipt SHA2567a6869dbd0ef1e43f0477385cfd7be642f410a117597cbf5dae616a1c2cabf20 and35 tracked source input snapshots. This is macOS integration evidence, not native Windows qualification.

Native Windows execution, ARM64 Windows checks, ordinary-user privacy and power-loss qualification remain pending. [Fixture requirements](storage-native-fixtures.md) specify the missing observations. First-launch bootstrap/ancestor durability remains an explicit Task4 contract and native-probe gate.

## Accepted identity consumer migration (Task 6)

[Independent review](publication-identity-consumer-review.md) approved the writer, both compensation guards and preferred-provider fallback guard with no findings. [Source hashes](storage-task6-source-sha256.json) identify the accepted owned files. Unrelated shared-checkout lockfile additions are preserved and excluded from this task; the owned dependency edge is runtime-identity → atomic-file.

Native macOS focused tests passed: publication_cleanup8/8, secure_record2/2, imported_secrets_and_selector_are_private1/1; atomic-file test_support7/7 and publication_commit_failure_can_be_retried_without_republishing1/1. Runtime-identity production compilation passed. Three temporary guard-removal mutations each caused the corresponding recovery test to fail; source was restored and the eight cleanup tests passed again. The production dependency graph does not enable test-support.

The import regressions target the second real metadata publication, inspect the persisted record and provider value, and reconstruct an identity to sign and verify. They establish phase-aware recovery on the tested host, not native Windows, SecretService transport, first-launch ancestry or power-loss durability. Those obligations remain open.

Task6 integration: `env PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig cargo check -p shell --tests --offline --locked` passed in1m22s, with existing warnings. Parent independently verified the raw handoff checksums, exit0 and27 selected identical start/end source hashes against unchanged sources or preserved pre-probe snapshots. Receipt SHA256: `72d1cbd16db65d28a3680ff3a17bd0a15f16a4ff8819cab48a7e1f74d64142a0`. This covers that checkpoint, not subsequent diagnostic probe changes.

## Accepted directory diagnostic source (bounded Task4 probe)

[Implementation report](directory-probe-report.md), [initial review](directory-probe-review.md), and [scoped fix approval](directory-probe-fix-review.md) preserve the source evidence. [Snapshot hashes](storage-directory-probe-source-sha256.json) identify the accepted artifact. Host tests passed23 library+3 CLI cases; five subsequent observation-classification regressions passed after demonstrated failures. Windows default library and feature/test/example compilation passed. The probe implements native ordinary-user fixture admission, real directory requests, repeated barriers and structured observations; it does not implement or certify production first launch.

Native Windows runs, separate-principal privacy, all candidate runtime/access profiles, abrupt storage loss and production anchor establishment remain outstanding. Source acceptance cannot close Task4 or S01. Receipt consumer Task8 is now in implementation independently of that native provisioning gate.

## Receipt publication source accepted (Task8)

[Report](receipt-publication-report.md), [independent review](receipt-publication-review.md), and [source hashes](storage-task8-source-sha256.json) identify the accepted12-file checkpoint. Native macOS runtime-fs54 library tests and16 native integration tests passed; one unchanged ignored two-volume qualification remains unexecuted. Shared atomic-file native publication19 tests passed. Final scoped --tests compilation and Windows x64/Linux ARM64 shared-validator test compilation passed without warnings. Cross-compilation is not native execution. The v1 fixed-byte fixture and real worker16-case fault matrix preserve format/error custody evidence.

Native Linux/Windows execution, wrong-owner/distinct-principal evidence, malformed/unreadable ACL fixtures, crash/storage qualification, actual production journal provisioning and coordinated current shell --tests remain unfinished. Source review has one nonblocking public error-documentation suggestion recorded in the ledger; no runtime blocker. Task4 Unix provisioning proceeds independently of native Windows bootstrap qualification.

## Unix production parent provisioning source accepted

[Implementation report](unix-provisioning-report.md), [initial review](unix-provisioning-review.md), [fix approval](unix-provisioning-fix-review.md), and [final hashes](storage-unix-provision-source-sha256.json) identify the accepted bounded Task4 increment. Both real parent-creating wrappers and settings update use held-leaf publication after complete real leaf-to-root barriers. Pre-fix native macOS checks passed22 parent,52 settings and57 existing atomic-file tests plus two unchanged app-state regressions. The localized final error-typing correction passed2 focused regressions after demonstrated2 failures; earlier full-suite counts are not a claim of a fresh final full-suite run. Windows/Linux test compilation passed before that Unix-only wrapper fix. Production normal/build settings graph excludes test-support.

Current shared shell integration is pending under a finite source checkpoint. Actual Linux default ACL, mount/firmlink fixtures, Windows native directory/bootstrap and power-loss evidence remain unqualified; process restart/synthetic EIO tests do not replace those. Full Task4 and the cross-platform goal remain open.

Task4 Unix + Task8 current shell integration verified: History finite hold RELEASED, all7peercommands reported terminal. Parent independently verified exact cargo check --offline --locked -p shell --tests, FFmpeg8 PKG_CONFIG_PATH, exit0 and rawlog checksum;1.24s build completion with existing36shell-lib-test warnings.84peer input identities (83files+1explicit absent legacy_runtime.rs) unchanged/start-end/current; our44file checkpoint unchanged additionally covers10 build/schema/test/example files absent from peer capture. Neutral evidence preserved task-4-unix-shell-handoff; receipt SHA2561bb3e907a47f08656ac2184c6739cba4f0d61405b51717ebb69a8b7f31c1c3e9. Own verification initially assumed all identities were files; explicit null absence was then inspected and correctly verified, no skipped missing subject. This is exact checkpoint integration, not native host/crash qualification. Task5 baseline24files reverified unchanged before dispatch.
