# Native storage test fixture requirements

Status: preparation for native qualification; no remote job or fixture has been provisioned or executed by this task.

## Existing runner infrastructure

The current release workflow declares native Windows x86_64/ARM64, Linux and macOS labels. `docs/imager-release-operations.md` specifies disposable one-job repository-scoped runners, outside-checkout trusted launchers, exact native architecture validation and destruction after execution. These are deployment instructions, not proof a runner is available. The Windows launcher validates private registration metadata and runner binary ancestry; do not relax it to run storage tests.

`.forgejo/workflows/release-imager.yml` Windows jobs depend on installer catalog construction, invoke Imager package builds and include signing/release concerns. Do not dispatch that workflow merely to obtain atomic-file results. Ordinary PR checks do not allocate those native hosts. A separate storage-only workflow/entry point must be prepared without signing credentials or installer publication, using an exact source revision and the same native architecture validation. The declared repository scope in operations documentation must be reconciled with the actual checkout remote before any registration; never guess or request tokens in logs.

## Required ordinary-user fixtures

| Fixture | Actual test requirement | Missing fixture behavior |
| --- | --- | --- |
| Local NTFS root | Writable temporary namespace, native disk/locality query succeeds, NTFS with persistent ACL and extended rename capability; capture OS/build/architecture/storage profile | Fail admission/qualification; no fallback path |
| Unsupported volume root | `TILO_ATOMIC_UNSUPPORTED_VOLUME_ROOT` references an explicitly provisioned writable nonlocal or non-NTFS test namespace | Test fails; never substitute the normal root or skip |
| Distinct unprivileged principal | `TILO_ATOMIC_PRIVATE_SECOND_USER_RUNNER` executes the test binary arguments as another actual native account and preserves child-path/owner-SID fixture variables; child must demonstrate distinct SID, directory access, denied private-file open and observed witness | Test fails without the runner/witness; no same-account simulation |
| Reparse creation | Disposable namespace where the intended reparse/symlink test can actually create its fixture | Fail qualification if privilege/developer-mode prerequisite absent; do not bypass reparse validation |
| Linux native lane | Execute actual test binaries on the claimed architecture/filesystem, including O_PATH rejection and cross-process leases | Cross-compilation is recorded separately |
| macOS distinct account | Separate native account exercises actual access to private files; existing ACL inspection tests are insufficient | Remains explicitly unqualified until executed |

Privileged harness setup, if needed, is outside production app/service operations. Tests themselves must record and run with the intended ordinary-user authority. Use only disposable fixture accounts/disks/directories. No production credentials, physical user disks, or global permission relaxation belongs in the fixture.

## Commands and artifacts

For each exact source revision run the complete native atomic-file test-support suite and the required caller tests as they migrate. Capture terminal exit, full logs, actual test counts, prerequisites, OS/build/architecture, token/UID authority and queried filesystem/storage characteristics. A passed executable that ran zero relevant tests is not evidence. Preserve native error codes and full identity observations without exposing secrets.

Windows ARM64 must execute natively and cannot be certified by x86_64 emulation or the locally installed x86_64 cross target. Build and package checks remain separate from filesystem behavior. `Cargo.lock` remains pinned; no re-resolution or shared target directory.

Task4 additionally needs its bounded directory-create/flush probe and explicit durable-anchor/restart contract. Task9 needs an external disposable-VM/storage controller that drops volatile writes at production checkpoints and inspects after reboot. Process kill, injected I/O error, or graceful shutdown cannot substitute for abrupt storage-loss qualification.

Local Linux fixture discovery: command -v found no docker, podman, colima, limactl, qemu-system-aarch64 or qemu-system-x86_64 on this macOS task PATH. Repository os/test-vm.sh expects qemu-system-x86_64 and DEVELOPMENT.md describes rootless podman, but these references are not an accessible running Linux host. No VM/container installation, image pull or remote connection attempted. Native Linux fixture availability remains unproven.
