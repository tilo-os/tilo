# ARM capsule validation under the Tilo kernel

The disposable VM harness in [`kernel-arm-capsule-smoke.py`](../tests/kernel-arm-capsule-smoke.py)
executes console fixtures against the built Tilo kernel instead of accepting
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
handoff, private namespaces, environment, Wine's Tilo JIT seccomp filter and
Proton's existing nested-runtime policy. They require Landlock ABI >= 5 and a
file written by Windows inside the retained capsule prefix. Exact
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
   `FEX_SERVERSOCKETPATH=/tmp/tilo-fex-server.sock` to both server and clients.
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
| `kernel-arm-capsule-transport-03` | Signed module loading, read-only 9p, RAM overlay, native execution and fully enforced launcher shim passed. |
| `kernel-arm-capsule-console-04` | Wine64 reached fully enforced Landlock, then failed with a glibc `sysmalloc` assertion and exit 139. Wine32 and Proton were not reached. |
| `kernel-arm-capsule-namespace-diagnostic-01` | Identical copied binaries and generated namespace reproduced the same assertion without the Landlock shim. |
| `kernel-arm-capsule-fex-diagnostic-01` | Direct FEX guest `true` passed; guest `getconf _NPROCESSORS_ONLN` returned 2 and passed. |
| `kernel-arm-capsule-wine-diagnostic-01` | Direct Wine with a fresh correctly mapped C drive reproduced the assertion without the capsule or Tilo seccomp. |
| `kernel-arm-capsule-wine-qemu10-01` | The same direct Wine control reproduced the assertion under QEMU 10.2.2. |
| `kernel-arm-capsule-wine-squashfs-02` | The complete original guest root mounted as read-only SquashFS over virtio-block reproduced the same direct-Wine assertion under QEMU 10.2.2. Native userspace still used 9p. |

The newer emulator came from the existing native libvirt runtime image
`bb391b2cad8818d2e92a84357d0a45c0b432c21cccf060f40cf3439013fa1132`.
Its QEMU executable SHA-256 is
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

### Allocator assertion origin

A separate ignored native `ptrace` supervisor followed the direct-Wine control
and captured registers, mapped files and the saved guest frame chain at the
first fault. It did not change FEX, the guest filesystem, or capsule policy.
The complete SquashFS guest, QEMU 10.2.2 and copied pre-DXVK helper inputs were
retained. This remains a diagnostic, with `accepted: false`.

In `kernel-arm-capsule-wine-first-fault-03`, the native fault PC was in FEX's
anonymous executable JIT region. The saved guest frames, resolved against the
unchanged x86 ELF symbols, were:

```text
__libc_assert_fail+0x5e
sysmalloc+0x9c1
_int_malloc+0xb08
__libc_malloc2+0xef
_dl_new_object+0x341
_dl_map_object_from_fd+0xe0
_dl_map_new_object+0x119
dl_open_worker_begin+0x781
```

This identifies the assertion as the guest x86 glibc allocator checking its
heap while the guest dynamic loader loads an object. It does not identify what
violated the heap invariant. The saved guest instruction pointer was
`mmap64+0x2c`; FEX documents that this saved value can lag during JIT execution,
so it is not treated as the precise faulting guest instruction.

`serial.log` retains the raw snapshots, and `decoded-guest-frames.json` retains
the object-relative addresses and symbol resolution. The tracer compiled with
`-Wall -Wextra -Werror`; its three bounded runs completed and their owned image
views were released. FEX's `DEBUG_STRACE` is a compile-time option in this
pinned source, not an available runtime environment toggle.

### Page-size comparison

The exact staged Tilo kernel configuration has `CONFIG_ARM64_4K_PAGES=y` and
`CONFIG_ARM64_VA_BITS=48`; 16 KiB, 64 KiB and 52-bit configurations are disabled.
The native WSL kernel configuration also selects 4 KiB pages and 48-bit virtual
addresses, and its native `getconf PAGE_SIZE` reports 4096. There is therefore
no configured page-size or virtual-address-width difference between these
controls. The bounded `kernel-arm-capsule-cpu-page-01` VM probe also measured
native and guest `getconf PAGE_SIZE` as 4096, and both online/configured CPU
counts as 2. Native `/sys/devices/system/cpu/online` bytes were
`30 2d 31 0a` (`0-1` followed by newline).

The guest `/usr/bin/od -An -tx1 /sys/devices/system/cpu/online` unexpectedly
reproduced the same `sysmalloc` assertion and exit 139 before printing bytes.
Its guest CPU-list contents were therefore not measured in this run, and the
script stopped before the `present` comparison. This supplies a smaller
reproducer independent of Wine. It does not establish the cause or whether
that program reached its CPU-list read. Individual guest mapping correctness
remains unproven.

### Smaller guest-program control

`kernel-arm-capsule-od-control-01` ran existing native and guest `od` binaries
with `LC_ALL=C` and at most 64 input bytes. Native controls all passed.

| Guest input | Result |
| --- | --- |
| `/dev/null` | Exit 0. |
| `/etc/os-release` | Printed the expected 64 bytes, then `corrupted size vs. prev_size`, exit 134. |
| CPU `online` | Same `sysmalloc` assertion before byte output, exit 139. |
| CPU `present` | Same `sysmalloc` assertion before byte output, exit 139. |

Guest `true` and CPU-count `getconf` continued to pass. Native CPU `online` and
`present` both contained `30 2d 31 0a`. The smaller failure is therefore not
limited to Wine, CPU-list input, or a non-C locale. Empty-input startup succeeds;
ordinary nonempty input also exposes a heap invariant failure. These controls
do not establish where the corruption originates.

The diagnostic-only `kernel-arm-capsule-maxinst-one-01` repeated the regular-file
`od` and direct Wine controls with the verified `FEX_MAXINST=1` option. Both
failures persisted with the same allocator messages and exit statuses. This
block-size control is not a production setting or an accepted mitigation, and
does not reverse the earlier consecutive-`brk` regression result.

Read-only inspection separately confirms that pinned FEX emits CPU-list ranges
without a trailing newline. Earlier native-WSL seeded-stack probes recorded a
guest count of 119 for 12 CPUs. In this guest libc, the allocator's direct
`get_nprocs` call computes the arena-count limit; the VM's unseeded count of 2
does not prove that malformed-list parsing is harmless. Neither this known
formatting defect nor the already-fixed consecutive-`brk` regression is treated
as the explanation for the current failure.

## Read-only block-filesystem control

`--guest-squashfs PATH` substitutes a regular SquashFS artifact file for the
guest-root 9p share. The harness refuses device paths, attaches the file as a
read-only virtio block device, loads the matching signed `squashfs.ko`, and
mounts it read-only at the same guest-root path. The default remains 9p. Native
userspace and the other artifact shares are unchanged.

The complete 6.4 GiB original guest directory was compressed without exclusions
or source permission changes using existing offline `squashfs-tools` 4.6.1:

```sh
mksquashfs /guest-root /output/guest.squashfs \
  -noappend -exit-on-error -comp zstd -Xcompression-level 3 \
  -processors 2 -mem 256M -no-progress
```

The builder had only `DAC_READ_SEARCH` added to its otherwise empty capability
set, read-only source access, and a 7 GiB output limit. Output is about 2.3 GiB;
SHA-256 is
`730d904addefc98b44231cf1984b714d222c828a5d8ca4cc51e4c8f0c2eae66e`.

An earlier build without that read capability silently replaced seven
restricted-mode files with empty files despite exiting zero. It was rejected;
its hash, rejection reason and logs were retained, and only the invalid image
file was removed after verifying the replacement. The replacement treats read
errors as fatal, has no omission warnings, and all seven affected files were
byte-compared against their unchanged source. The rejected image's VM run is
not acceptance evidence.

The clean block-filesystem control still failed before the Windows marker.
Therefore the three full Landlock cases were not rerun with it. This rules out
guest-root 9p as a necessary condition for this failure; it does not rule out
effects from the remaining native-userland 9p share or establish a root cause.

## Reproduction and evidence

### Native executable transport control

`kernel-arm-capsule-native-ram-03` copied the native FEX, FEXServer and helper
executables, their resolved ELF interpreter, and all `ldd` dependencies into
VM tmpfs (40,368 KiB). Each copy was byte-compared with its source, hashed, and
bound read-only over the original path before running the unchanged `od`
matrix. The guest filesystem remained the verified read-only SquashFS image.
No source image or host disk was modified.

The same failures persisted: guest `/dev/null` succeeded, the regular-file
read printed its bytes then failed with exit 134, and both CPU-list reads
failed with the allocator assertion and exit 139. Native controls, guest
`true`, and guest CPU-count `getconf` still passed. Thus directly mapping these
native executables and their startup libraries from 9p is not necessary for
the observed failure. Other native data paths remain on 9p; this does not
prove every transport effect absent or establish a root cause.

The first two RAM-control attempts stopped during dependency-list setup
because `ldd` headings were mistaken for paths. Neither reached the guest
matrix and neither is runtime evidence. The completed third run retains its
shell trace, copy hashes, input identities, and diagnostic result.

### SVE vector-length controls

The unchanged native-RAM/guest-SquashFS reproduction now distinguishes CPU
feature configurations. All rows use QEMU 10.2.2, the same kernel, native FEX
image, guest bytes and read-only `od` matrix. These are diagnostics, not full
capsule acceptance:

| QEMU CPU | FEX override | Result |
| --- | --- | --- |
| `max` | none | Heap corruption persists |
| `max,sme=off` | none | Heap corruption persists |
| `max,sve=off,sme=off` | none | All four guest reads, `true` and `getconf` pass |
| `max,sve-max-vq=2,sme=off` | none | All checks pass with SVE retained at at most 256 bits |
| `max` | `FEX_HOSTFEATURES=disablesve,disablesvebitperm`, `FEX_FORCESVEWIDTH=0` | All checks pass |

Results are retained under `kernel-arm-capsule-native-ram-{nosme,nosve,sve256,fexnosve}-01`.
The FEX-only control leaves native ARM userspace's CPU capabilities unchanged
and retains FEX's guest AVX advertisement. Thus changing native libc dispatch
is not necessary to avoid this reproduction. No override was installed in the
production runtime.

At FEX commit `91bbce5a96b9e1eaa32f95f440ea8b46bbfbdf20`, host feature detection
reads the active SVE width with `RDVL` and enables its 256-bit path at widths
of at least 256 bits. `GenerateSVEMemOperand` in `JIT/MemoryOps.cpp` scales
constant offsets by 32 bytes before emitting a `MUL VL` address. The failing
VM's kernel reports a default vector length of 64 bytes. This is a concrete
candidate for incorrect addresses at wider lengths; the observed failing
guest instruction has not yet been traced through that helper, so this is
not a confirmed root-cause attribution to a particular instruction.

The unchanged upstream `unittests/ASM/VEX/vmovups_mem.asm` subsequently gave
a smaller semantic reproduction. The existing portable `TestHarnessRunner`
and upstream-generated binary/config were run with `FEX_MAXINST=1`,
`FEX_MULTIBLOCK=0`, `FEX_TSOENABLED=0` and no host-feature override. With
`max,sme=off`, the kernel's default vector length was 64 bytes: the test's
scalar reads after a 256-bit store at offset 32 returned `eeeeeeeeeeeeeeee`
and `ffffffffffffffff` instead of `6162636465666768` and `7172737475767778`.
The runner reported `I Faulted? No` and `I Passed? No`. The same artifacts
passed with `max,sve-max-vq=2,sme=off` and a 32-byte default vector length.
This independently proves incorrect emulated memory results at the wider
length; it is not just an inference from Wine or guest libc failing.

The two runs are `kernel-upstream-vmovups-{sve512,sve256}-01`. Artifact SHA-256:

- Runner: `cb815c11adcf4c605144b36ec9d875263b8afc8d1149d3ded47e2770cb2b17c0`
- ASM binary: `d4a8f74c2e44fafa5cbb3ce50695a40176887992ba0116208929da27c9e6ce19`
- ASM config: `a461e65f91de8bcd420e588379c6f478fbecad24353904c17a4e0b412f27e08a`

A launch-only vector-length clamp is insufficient: this FEX pin forwards
unrecognized guest `prctl` options to the host, which can change a thread's
SVE length and exec-inheritance flags. It also has other fixed-width address
calculations. No such workaround has been installed in Tilo.

### Current-helper initialization follow-up

At 256-bit maximum SVE, `kernel-arm-capsule-console-sve256-01` used the current
managed Wine/DXVK helper and reached fully enforced Landlock ABI 10. Wine64
then hit its existing preparation deadline; Wine32 and Proton were not reached.
The same binaries and guest payload in `kernel-arm-capsule-namespace-sve256-01`
also timed out without the executable shim. These are failures, not capsule
acceptance. The original heap assertion was absent, but that does not establish
why initialization timed out or prove every SVE operation correct at 256 bits.

The follow-up controls retain the immutable binaries copied into
`kernel-arm-capsule-console-sve256-01/inputs` and the diagnostic new-WoW guest
payload. They do not use subsequently rebuilt traditional-Wine provider tests.
The full Landlock control `kernel-arm-capsule-console-nosve-01`, with
`max,sve=off,sme=off`, also timed out during Wine64 preparation after 138.84
seconds. Wine32 and Proton were not reached. Disabling SVE therefore does not
resolve this initialization failure.

Private namespace observers captured process state at approximately 30, 60,
and 100 seconds without changing production deadlines, seccomp or executable
bytes. `kernel-arm-capsule-namespace-observer-01` showed that the initial
`wine wineboot.exe -i` loader remained in `anon_pipe_read` after both its Wineboot
child and Wineserver exited. The private prefix acquired neither registry
files nor Windows DLLs. This is not evidence of initialization merely taking
longer under TCG.

`kernel-arm-capsule-namespace-pipe-observer-01` identified the exit statuses and
pipe ownership from outside the capsule, inside the disposable VM:

- Wineserver PID 183 was a zombie with `/proc/PID/stat` field 52 equal to 4:
  termination by `SIGILL`. Wineboot PID 185 exited with status 0.
- Wine loader PID 181 was blocked reading 16 bytes from fd 11. That descriptor
  and its own write-only fd 12 both referenced `pipe:[1456]`. After Wineserver
  exited, the cross-VM descriptor scan found only this loader holding the pipe;
  no retained writer in the native supervisor or FEXServer was observed.
- The retained write end prevents end-of-file on that pipe, explaining the
  loader's continued wait until the existing preparation timeout. The cause
  of Wineserver's `SIGILL` remains unknown.

Wine also logged `prctl(PR_SET_SECCOMP, ...): Invalid argument` and missing
kernel32 lookup warnings. Their causal role is unproven; the immutable guest
does contain its builtin kernel32 and ntdll payloads. Kernel logs captured at
exit supplied no fault address. FEX can synthesize guest `SIGILL` using a native
`hlt` trampoline, so the signal number alone cannot distinguish a guest fault,
a native JIT instruction fault, or a forced FEX assertion. First-signal
registers, instruction bytes and mapped guest state are the next discriminator.
All diagnostic results remain failures, and both owned image mounts and the
VM processes were released after each run. No workaround was installed.

### First Wineserver illegal instruction

The existing GDB 17.2 from an additional read-only tools image ran outside the
capsule in `kernel-arm-capsule-namespace-gdb-observer-02`. It attached only after
the persistent Wineserver was adopted by the retained native helper, caught
the first `SIGILL`, recorded registers and memory, then passed the signal.
The earlier `-01` attempt selected the short-lived launcher and reported
`ptrace: No such process`; it captured no fault evidence.

The first captured signal has `si_code=1` (`ILL_ILLOPC`) and fault address/native
PC `0x80006a62f7f0`, inside anonymous executable JIT memory
`0x80006a584000..0x80006b583000`. Its instruction word is `0xfffffc40`
(bytes `40 fc ff ff`), an undefined native instruction. The following sequence
was read directly from that process:

```text
add  x20, x10, x7
.inst 0xfffffc40
ld1b {z17.b}, p7/z, [x20, x0]
add  x20, x11, x7
.inst 0xfffffc40
st1b {z17.b}, p7, [x20, x0]
```

This first fault is an undefined instruction in generated native code; its
sampled word is not the documented guest-SIGILL `hlt(0)` trampoline. Whether
the word was emitted incorrectly or altered later remains unproven. The
captured `vg=4` corresponds to the unchanged 256-bit SVE control.

The saved guest RIP, `0x423700`, maps to an ordinary function prologue in the
stripped x86-64 Wineserver ELF, whose SHA-256 is
`ddf8f1d7b1df04b7688358d81fce66148ba44d67fbb7942dd7645f059db4c968`.
It can lag the native fault and is not asserted to be the faulting guest
instruction; no function name was recovered. The run retains
`gdb-first-sigill.txt`, `guest-wineserver-disassembly.txt` and
`first-sigill-evidence.json` beside its raw serial log. Kernel, modules, native
image and all saved runtime input hashes matched the previous control.
Preparation still failed after 138.51 seconds. All three owned image mounts
and the VM were released; no production code, deadline or policy changed.

### Running the harness

Use `python3 os/tests/kernel-arm-capsule-smoke.py --help` for the explicit kernel,
module archive, native image, guest rootfs, pinned runtimes and binary inputs.
Start with `--mode probe`, then use a new output directory for `--mode console`.
An optional `--qemu` selects an explicitly recorded emulator or loader wrapper.
`--cpu` selects a QEMU CPU model/features and defaults to `max`. The selected
value is retained in both input and result metadata, and the guest logs its
native CPU features. A passing restricted-feature run does not establish
acceptance for the default or for hardware exposing different vector widths.

Every run retains `inputs.json`, `qemu-command.json`, `serial.log` and
`result.json`. Inputs include kernel/module hashes and executable identities.
Diagnostic modes (`namespace-diagnostic`, `fex-diagnostic`, `wine-diagnostic`)
always report `accepted: false`; `diagnostic_passed` records their separate
outcome. Only all three Landlock console cases passing may establish the
console capsule result.
