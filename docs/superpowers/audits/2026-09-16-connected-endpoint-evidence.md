# Connected endpoint evidence — implementation remains incomplete

This audit records the interim native endpoint work for the approved Windows/Linux migration design. It does not enable Connected folders, certify Windows support, or complete the migration goal.

Source commit: `087d5a62c9b94b97489de331919277d02224b0e6`. Its independent source review found four Important defects; a fix round is in progress. The defects concern Windows capability promotion, unresolved Linux clone queries, serial namespace requirements, and unsupported Windows metadata. Do not treat the implementation report's original Windows capability claim as correct: the source contradicted it.

## Evidence scope

Native execution used only the disposable Fedora 44 ARM64 fixture, UUID `8fdddf6a-6dae-4f9d-a236-321a1d223e3a`, as ordinary user `parallels`. The host root filesystem is XFS; separate 256 MiB ext4 and Btrfs loop images were created for tests. Frozen source snapshots and logs reside in `.superpowers/sdd/2026-09-16-connected-folders/` and are local, ignored evidence, not distributed test artifacts.

| Native snapshot | Filesystem | Observed result | Limit |
| --- | --- | --- | --- |
| native-red-harness | Btrfs | 1 failed: fresh volume incorrectly ambiguous | Genuine behavioral RED |
| native-probe2-harness | ext4, XFS, Btrfs | 1 identity/observation test passed on each | Predates final source |
| native-probe2-harness | XFS | Selected-write trial failed Unverified | Genuine missing-implementation RED |
| native-proof3-harness | ext4, XFS | 4 tests passed on each | Observation, cancellation/link scope, replaced/offline root, private write trial |
| native-proof3-harness | Btrfs | 4 failed: pseudo-device lookup | Not passing evidence |
| native-proof4-harness | Btrfs | Three test commands returned exit 0 | Full guest logs not retrieved before suspension; empty local copies are not evidence |
| native-proof4-harness | ext4 | Guarded detach/remount coordinator returned exit 0 | Old session unavailable; saved identity reopens in fresh context; guest log pending retrieval |
| native-proof4-harness | Btrfs | Coordinator returned PRL_ERR_IO_STOPPED | Mount-cycle proof absent |

The proof4 outcomes above were observed as command/coordinator results, not subsequently re-read guest logs. They are neither reboot nor power-loss proof. Later source changes and current review fixes require a new native snapshot run.

The Windows actual-backend `cargo +1.97.1 check --tests --target x86_64-pc-windows-msvc --offline` check completed in 4.20 seconds. It compiled real Copy/continuation/endpoint modules with a compile-only portal adapter and omitted UI/TLS. Its ten excluded-UI dead-code warnings were existing harness noise; there were no endpoint warnings. This is compile evidence only. The implementer's 38 host tests and strict host/Linux/Windows source lint results likewise do not substitute for native Windows execution.

## Outstanding delivery gates

- Resolve and independently review all four source findings.
- Re-run final source on ext4, XFS and Btrfs, retaining complete outputs.
- Execute read-only, duplicate-volume, mount replacement, reconnect, concurrent-writer and resource-limit fixtures. Prepared controller scripts have only local syntax checks.
- Implement Linux-side access to retained Windows NTFS folders, including native persistent identity, health/hibernation refusal and metadata handling. This is missing implementation, not merely missing tests.
- Execute Windows native NTFS cases, including locks, placeholders, reparse points, hard links, directory streams and ACL policies; establish actual write/persistence support.
- Complete durable connection storage, transaction execution, conflict/recovery behavior and shipped UI/service integration with native evidence. A final observation sweep is not an atomic filesystem snapshot; transaction-time preimage validation remains required.
- Complete separate Desktop installed lifecycle and Full OS internal staging/dual-boot gates. Production catalogs remain disabled pending authentic signed artifacts and supported native configurations.

Both Fedora VMs were authoritatively listed as suspended during this audit. No resume, UI input, package installation or session changes were performed. Native testing is paused; the original Fedora VM is outside fixture mutation scope.

## Retained local evidence fingerprints

These hashes identify the locally retained artifacts; they do not imply the files are committed or prove unexecuted scenarios.

| Artifact under the local evidence directory | SHA-256 |
| --- | --- |
| `task-2-btrfs-red.log` | `33a47968bab9b2dbb90359bd8654f27c593a3c4e4dc39113d9908dd074c60ece` |
| `task-2-ext4-probe2.log` | `36bca68f443a87d82fe66c298300499c312966fe1cba534b3f44110917b61c3b` |
| `task-2-xfs-probe2.log` | `951b8c166a467ebaf304e506def6ba2850bcbf0514d4217b076cc58434330c8e` |
| `task-2-btrfs-probe2.log` | `2f7e1bcf76bd430cfab6cce91227ad03312e6772911bb71795571d4372538a84` |
| `task-2-write-red.log` | `f458f991b9398859834a1c93ba2b307a37fe1d27bd985dc6037fcaa7d5fd1414` |
| `task-2-ext4-proof3.log` | `d540a985ee3acf4e215a951b945fa0563894e01ea885de76f8cec93327f196b5` |
| `task-2-xfs-proof3.log` | `7b3825e4b837b672d46218b6a0e8a7570a5b3e86683b47d5115784a6f5cae702` |
| `task-2-btrfs-proof3.log` | `fb4eb971041faeee89b5bb87ada8077f813372cf839106ac469c6ad26e03a997` |
| `task-2-windows-backend-check.log` | `e923dfbe12df1eba8cdc705ec4a9c28b790b69972671b146054e3ef9305ddefb` |
| `native-proof3-harness/source-sha256.json` | `3de419c93f25377c8f2a7583182e83bb140f3bbdc21a57aabc2582a0e81b0e30` |
| `native-proof4-harness/source-sha256.json` | `0aaa61c465910c3f5970e0c07e6432df43a46baff01065ca3199e4e1bea1f533` |
