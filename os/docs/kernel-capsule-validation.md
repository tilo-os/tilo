# ARM capsule validation under the qOS kernel

The disposable VM harness in [`kernel-arm-capsule-smoke.py`](../tests/kernel-arm-capsule-smoke.py)
executes console fixtures against the built qOS kernel instead of accepting
WSL's insufficient Landlock ABI. Full capsule acceptance is **not complete**.

## Test boundary

The harness loads the kernel's matching signed `netfs`, `9pnet`,
`9pnet_virtio`, `9p`, and `overlay` modules. QEMU exports only prepared artifact
directories through read-only virtio 9p. A mounted native OCI image supplies
userspace without exporting another multi-gigabyte rootfs copy. Its image view
is released after the owned VM exits. Guest writes stay in tmpfs and a RAM
overlay; no physical disk or writable host directory is attached.

The guest switches its root mount before dropping to UID/GID 1000. A plain
`chroot` is insufficient: Linux intentionally rejects creation of an
unprivileged user namespace from a chroot. Networking is disabled.

The three `generated_arm_*_landlock_executes_windows_console` tests retain the
entire generated production plan, including the launcher shim, descriptor
handoff, private namespaces, environment, Wine's qOS JIT seccomp filter and
Proton's existing nested-runtime policy. They require Landlock ABI >= 5 and a
file actually written by Windows inside the retained capsule prefix. Exact
test names are checked before execution, preventing an empty test selection
from appearing successful. Each console case has a 600-second deadline.

Wayland and render-node sources are deliberately inert console fixtures.
Results do not establish graphical, physical-hardware, KVM, or installation
acceptance. The kernel reports ABI 10; the launcher reports its required
ruleset `FullyEnforced` with effective ABI 7.

## Findings and fixes

1. FEX's default filesystem socket follows `XDG_RUNTIME_DIR`. The capsule grants
   access to its brokered Wayland socket, not arbitrary writes to that directory.
   The native supervisor now supplies the upstream-supported absolute
   `FEX_SERVERSOCKETPATH=/tmp/qos-fex-server.sock` to both server and clients.
   `/tmp` is already the capsule's private writable tmpfs. Landlock rules were
   not expanded for this fix; a child-environment regression checks that
   inherited socket/rootfs overrides are replaced.
2. FEXServer enumerates `/proc/self/fd` in its own process. ARM translated plans
   now grant read-only access to their private procfs. A rule anchored to the
   launcher's resolved `/proc/self` would not cover forked children. The
   regression checks `--unshare-all`, `--proc /proc`, absence of host proc binds
   and proc write grants, and unchanged native x86 plan behavior.

## Recorded results

The native image is portable FEX image
`0e5d405552f06a46515cdd3ce49862718d73f03a056023acd0f38209b9089299`
(`TUNE_CPU=none`, `TUNE_ARCH=generic`). Initial VM controls used QEMU 9.2.4
with two TCG vCPUs and 4 GiB RAM. Its executable SHA-256 is
`6741cce11c64818065ab667b560d5bab5c0781b583a119c103b89d68012021f4`.

| Run directory under the install-preparation workspace | Result |
| --- | --- |
| `kernel-arm-capsule-transport-03` | Signed module loading, read-only 9p, RAM overlay, native execution and actual fully enforced launcher shim passed. |
| `kernel-arm-capsule-console-04` | Wine64 reached fully enforced Landlock, then failed with a glibc `sysmalloc` assertion and exit 139. Wine32 and Proton were not reached. |
| `kernel-arm-capsule-namespace-diagnostic-01` | Identical copied binaries and generated namespace reproduced the same assertion without the Landlock shim. |
| `kernel-arm-capsule-fex-diagnostic-01` | Direct FEX guest `true` passed; guest `getconf _NPROCESSORS_ONLN` returned 2 and passed. |
| `kernel-arm-capsule-wine-diagnostic-01` | Direct Wine with a fresh correctly mapped C drive reproduced the assertion without the capsule or qOS seccomp. |
| `kernel-arm-capsule-wine-qemu10-01` | The same direct Wine control reproduced the assertion under QEMU 10.2.2. |

The newer emulator came from the existing native libvirt runtime image
`bb391b2cad8818d2e92a84357d0a45c0b432c21cccf060f40cf3439013fa1132`.
Its actual QEMU executable SHA-256 is
`2c8f9b5fc52a151cf77e97792fea0401718309788dd4c8af29f21a28834ba79e`.
It ran through that image's dynamic loader and libraries from an owned mounted
view; neither emulator image was copied. Both image views were released.
The namespace, direct-FEX and direct-Wine controls reuse the helper and test
binaries copied into `console-04`, before the later DXVK provisioning changes.

The current allocator failure is therefore not evidence that Landlock needs
further relaxation. Its root cause remains unproven; tiny guest-program success
does not exclude a Wine-specific FEX, packaging, kernel, or TCG defect. Native
runtime comparisons are separate controls; reproducing on two QEMU versions
does not establish that physical hardware would fail.
No upstream FEX source or expected-result lists were changed for these tests.

## Reproduction and evidence

Use `python3 os/tests/kernel-arm-capsule-smoke.py --help` for the explicit kernel,
module archive, native image, guest rootfs, pinned runtimes and binary inputs.
Start with `--mode probe`, then use a new output directory for `--mode console`.
An optional `--qemu` selects an explicitly recorded emulator or loader wrapper.

Every run retains `inputs.json`, `qemu-command.json`, `serial.log` and
`result.json`. Inputs include kernel/module hashes and executable identities.
Diagnostic modes (`namespace-diagnostic`, `fex-diagnostic`, `wine-diagnostic`)
always report `accepted: false`; `diagnostic_passed` records their separate
outcome. Only all three real Landlock console cases passing may establish the
console capsule result.
