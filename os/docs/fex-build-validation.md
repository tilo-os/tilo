# FEX native ARM build validation

FEX translates x86 instructions on ARM hosts. Its native executable alone does
not provide Wine/Proton support: the x86 userspace filesystem, graphics thunks,
and application sandbox integration must also be built and validated.

## Source and toolchain inputs

`os/external-assets/fex-source.env` pins FEX and its recursive gitlinks.
`os/dev/fetch-fex-source.sh DESTINATION` acquires that checkout or verifies an
existing clean checkout without resetting caller changes.

`os/Containerfile.fex-buildroot` builds the native compiler/tool dependencies.
Pass the digest-pinned `BASE_REF` from `os/base-snapshot.env` and the external
asset cache as the named `github-assets` build context. The cache must contain
`nasm-2.16.03.tar.xz`; its official HTTPS source and SHA-256 are recorded in
`os/external-assets/github-assets.tsv`. Production consumers must pin the
resulting buildroot image by digest. No production FEX buildroot digest has
been promoted yet.

The buildroot compiles NASM natively into `/usr/local/bin`. NASM emits x86 test
programs; it is not an x86 executable and does not require emulation to run.
Keep `/usr/local/bin` before `/usr/bin` when configuring and building FEX.

## Assembler regression

On the ARM Fedora 44 buildroot, NASM 3.02 assembled `smsw rax` as `0f 01 e0`
instead of `48 0f 01 e0`, and dropped the `66` prefix from `smsw cx`. Disabling
NASM optimization did not restore those encodings. Consequently all three
execution variants of each upstream 32-bit and 64-bit `07_XX_04.asm` test
failed. Reassembling the unchanged sources with native NASM 2.16.03 made all
six cases pass. Neither FEX instruction semantics nor expected register values
were modified.

`bash os/tests/fex-assembler.sh [NASM_PATH]` checks exact operand-size bytes for
both execution modes. The buildroot runs this preflight after installing its
pinned assembler. Keep the preflight when upgrading NASM; changing expected
bytes to accept a different instruction would invalidate the test.

## Runtime test prerequisites and acceptance limits

FEX's dynamically linked x86 tests need a compatible x86 root filesystem.
Mount a verified filesystem read-only into the test container and set
`FEX_ROOTFS` to that container path. A missing root filesystem produced exit
248 failures in the initial run; these are failed tests, not accepted skips.
Preserve the full test log and rerun the complete suite after supplying it.

The first full run had 3,740 failures among 12,074 tests. Six were the assembler
cases above; missing-rootfs diagnostics occurred in the remaining failures and
the corrected full run is still required. Passing the six isolated cases does
not establish full FEX acceptance, Wine/Proton compatibility, or graphics
acceleration. Graphics thunk builds additionally require the upstream
`X86_DEV_ROOTFS` development inputs and their own runtime checks.

## Recorded native validation

Rebuilding all assembly fixtures with the pinned NASM produced 7,779 passing
instruction/emitter tests, 24 upstream skips, and zero failures. The upstream
disabled-test lists and expected CPU results were unchanged.

The old OS base digest was unavailable from the public Fedora registry and the
private mirror required renewed authentication. A separate test filesystem was
therefore acquired from `quay.io/fedora/fedora-bootc:44` after resolving it to
index digest
`sha256:5fca5bb111aa1aa7ea40a2501d78bdacbe40ccee7250e9c50a4242e0c6110098`.
Its amd64 child is
`sha256:4eb463c47f595ff98a0d98e5fdd43a126ed32bf428cc166e6bacfd389b878f7f`.
This is a test input, not a change to `os/base-snapshot.env`. Fedora may garbage
collect these manifests; preserve the acquired artifact for repeated testing.

Native ARM FEX executed x86-64 `uname` and nested x86-64 shell/child processes
with that filesystem mounted read-only in a container, without global binfmt
registration. The full 12,074-case rerun then produced:

- 10,041 passes;
- 458 upstream skips;
- 1,525 failures from missing 32-bit runtime support in the test filesystem;
- 38 structure-layout failures with missing cross-architecture C++ headers;
- 11 other failures and one timeout still requiring investigation.

The instruction-only success is not full-suite acceptance. The next run needs
the 32-bit runtime and complete target development headers. Preserve normal RPM
installation scripts when constructing that environment; simply ignoring the
failed checks or using incomplete header parsing cannot validate ABI layouts.

## Server lifecycle during tests

Run `python3 os/tests/run-fex-tests.py --build-dir /build --rootfs /rootfs` inside
the isolated test container. Optional CTest arguments follow `--`, for example
`-- -R gcc-target-32`. The runner starts FEXServer in the foreground, waits for
its readiness pipe, writes server output to `Testing/Temporary/FEXServer.log`,
and terminates only that server when CTest finishes. It preserves test failures
and does not change the upstream disabled-test lists or expected results.

The upstream auto-started daemon inherits standard output. With a capturing
test wrapper this can keep the first test waiting for pipe EOF even after its
guest process exits. Both observed first-case timeouts (`clock-2-1` and the
32-bit `20000609-1` case) passed in 0.10 seconds when the server was managed
explicitly. This resolves the test-process lifecycle issue; it does not waive
the remaining ABI, permissions, or runtime checks.

## Target headers for ABI verification

Put `os/tests/fex-cross-clang` on the test container's `PATH` as `clang` and set
`QOS_FEX_X86_SYSROOT` to the mounted x86 development filesystem. The wrapper
passes its sysroot and GCC toolchain to native Clang for the two x86 targets
requested by upstream `StructPackVerifier.py`. Other targets use native headers.
The upstream verifier and its expected layouts remain unchanged.

The SDK needs both `glibc-devel.x86_64` and `glibc-devel.i686`, plus both
`libstdc++-devel.x86_64` and `libstdc++-devel.i686`. Installing `gcc-c++` alongside
only the i686 C++ development package can satisfy RPM dependencies while leaving
the x86-64 C++ configuration headers absent. With both header sets present, all
40 upstream structure verification tests passed on the native ARM build.

The local SDK is a diagnostic input assembled from Fedora RPMs using the host's
pre-existing QEMU binfmt handler. An earlier container recipe's `SHELL` setting
was ignored for OCI output; its successful package transaction therefore did
not prove execution through FEX. Explicit FEX execution subsequently exposed a
crash in Fedora's static PIE `ldconfig`, resolved by the upstream revision
validated below. Preserve
normal RPM scriptlets and distinguish SDK construction from translator runtime
acceptance. Release SDK production still requires a pinned, reproducible build
and its own acceptance evidence.

## Runtime filesystem and syscall test fixture

An exported distribution image is not directly a FEX runtime overlay. Follow
the upstream [custom RootFS instructions](https://wiki.fex-emu.com/index.php/Development:Setting_up_RootFS):
exclude runtime directories such as `/tmp`, `/dev`, `/proc`, `/sys`, and `/home`,
and the host-specific configuration files listed there. Keep the complete SDK
separately for package maintenance. Preserve the source export and build a
separate runtime view; exclusions applied during tar extraction can break
hardlinks in Fedora bootc's OSTree objects. Copying the complete filesystem
before moving excluded paths aside preserves those objects.

The native ARM syscall diagnostics identified three fixture requirements:

- PID 1 must belong to root, as the upstream `sched_setparam` and `sigqueue`
  permission tests explicitly assume. Run the tests as an ordinary user with
  all capabilities dropped, under a root-owned supervisor that waits for them.
- Make `/dev/fuse` and `/dev/net/tun` available in the isolated test container.
  Device existence is tested even when privileged operations are skipped by
  upstream. No network administration capability is required for this run.
- The outer container's default seccomp filter changes invalid `clock_settime`
  calls from `EINVAL` to `EPERM`. An equivalent native ARM control reproduced
  this failure and passed with the outer filter disabled. Disable that filter
  only for the isolated diagnostic fixture; this does not change the product
  application sandbox or grant clock-setting capabilities.

With a read-only runtime view, a root-owned supervisor, unprivileged test
processes, the two devices, and the diagnostic seccomp setting, all 11 previously
failing syscall groups passed. The complete suite must still pass with this
fixture; targeted success alone is not full acceptance.

The complete rerun of the configured FEX-2609 suite with this fixture finished in 590.61 seconds:
11,616 passes, 458 upstream skips, and zero failures across 12,074 cases.
This baseline had `BUILD_THUNKS=OFF` and `BUILD_FEX_LINUX_TESTS=OFF`; it does not
cover those additional targets or the separate Fedora `ldconfig` crash.

## Consecutive syscall regression

`os/tests/fixtures/fex-consecutive-brk.asm` reduces the static PIE startup
failure to querying and extending the program break, then writing into the
allocated page. On FEX-2609 the second `brk` never reaches its handler and the
program exits unsuccessfully. A diagnostic run with `FEX_MAXINST=1` passes;
disabling optimization passes alone does not fix the failure. Do not use that
diagnostic block-size override as a production workaround.

The emitted IR reuses the original syscall-number constant after the first
syscall overwrites its allocated register. Upstream
[commit 91bbce5a96b9e1eaa32f95f440ea8b46bbfbdf20](https://github.com/FEX-Emu/FEX/commit/91bbce5a96b9e1eaa32f95f440ea8b46bbfbdf20)
fixes Linux syscall block boundaries and advances the saved instruction
pointer, including cache format invalidation. That exact, unmodified revision
passed the regression and actual Fedora `ldconfig -p` with normal JIT settings.
The same configured native ARM suite then passed in 480.47 seconds: 11,616 passes,
458 upstream skips, and zero failures across 12,074 cases. The source manifest
now pins this revision and its recursive gitlinks. No downstream FEX patch or
production block-size override is required.

The upstream suite passing by itself did not expose this application startup
failure. Keep the additional regression and real-program checks when updating
FEX. A successful `ldconfig -p` check also does not replace validation of an
entire RPM transaction through FEX, graphics thunks, or Wine/Proton integration.

The subsequent diagnostic SDK build explicitly invoked the fixed native FEX
executable as the OCI `RUN` command and completed the 21-package transaction,
including both x86 C++ development header sets and normal RPM scriptlets.
The resulting local image is
`localhost/qos-fex-x86-sdk:fedora44-fex-validated`, image ID
`864eb1cb6f5f1cf6132681aae58f34489a8518c1dab6286f73c6208ac713c9ab`.
Unlike the earlier ignored `SHELL` recipe, this build did not rely on OCI shell
selection to launch FEX. This remains a diagnostic SDK assembled from live
Fedora repositories, not the frozen production runtime or a release artifact.

## Graphics bridge build inputs

The native buildroot also supplies Clang's development library and the native
ALSA, DRM, OpenGL/EGL, X11, XCB, Wayland, and xshmfence development packages.
FEX uses them to build its native thunk generator and ARM host libraries.
The separate x86 SDK needs matching development packages for both x86-64 and
i686, including the C/C++ libraries described above. Preserve normal package
scripts when constructing that SDK.

Use the explicit guest toolchains in `os/dev/toolchains/` when enabling the
upstream targets:

```sh
export QOS_FEX_X86_SYSROOT=/x86-sdk
cmake -S /src -B /build \
  -DBUILD_THUNKS=ON -DBUILD_FEX_LINUX_TESTS=ON -DENABLE_CLANG_THUNKS=ON \
  -DX86_DEV_ROOTFS="$QOS_FEX_X86_SYSROOT" \
  -DX86_64_TOOLCHAIN_FILE=/qos/os/dev/toolchains/fex-cross-x86_64.cmake \
  -DX86_32_TOOLCHAIN_FILE=/qos/os/dev/toolchains/fex-cross-i686.cmake
cmake --build /build --parallel 2
```

The environment variable is also required by upstream's nested Linux-test
projects, which do not forward `X86_DEV_ROOTFS` themselves. The toolchains use
native Clang with explicit guest targets, the SDK's GCC runtime, and target
pkg-config directories. They reject an absent SDK or `/` as the SDK; guest
library discovery cannot silently fall back to native ARM libraries. The
outer build still compiles the generator and host libraries natively.

Building thunks is not runtime acceptance. Run the added `.ThunkGen`, Linux
`.jit.flt`, and `ThunkFunctionalTest` cases with the installed libraries and
the required guest applications. Report those results separately from the
12,074-case baseline. The pinned source currently emits a 64-bit Vulkan guest
thunk but no 32-bit Vulkan guest thunk; this must be accounted for when
validating 32-bit DirectX applications, not hidden by the existence of a
32-bit-interface host library.

The native ARM buildroot recipe completed successfully with these dependencies
(image ID `e877d2c6ca51003b1fd8fd45670eaccde172df49370a0ea331d2b098c3add959`).
Both checked-in guest toolchains compiled and linked a C++ program using vectors
and exceptions; ELF inspection confirmed the requested architectures, and FEX
ran both successfully (`CROSS_SDK_PASS`, 64-bit and 32-bit, sum 42).

The first expanded test run completed 110 cases: 103 passes, two upstream skips,
and five failures. These comprise `VariadicFunction.ThunkGen` (generated
`__size_t` is undeclared), both `smc-shared-1` variants (executable shared-memory
mapping), `cpu_count.32` (12 CPUID IDs versus 119 reported CPUs), and
`syscalls_efault.64` (all 20 assertions pass, but upstream marks it an expected
failure). These results do not constitute runtime acceptance; preserve the
failures and investigate the fixture and implementation separately.

A rerun with `--tmpfs /dev/shm:rw,exec,nosuid,nodev,size=64m` passed both
`smc-shared-1` variants: these cases explicitly request `PROT_EXEC` on a
`shm_open` mapping. The dedicated validation container needs an executable
shared-memory mount for that test contract. This does not change the product's
sandbox configuration. The expanded result is now 105 passes, two upstream
skips, and the three other failures above across 110 cases (10.69 seconds).

## Initial Wine runtime check

The diagnostic Fedora 44 runtime image
`localhost/qos-fex-wine-runtime:fedora44-dev` completed its package transaction
and full filesystem export, followed by the upstream host-directory exclusions.
Its image ID is
`2c94250fbe81422bf0c3854958d67c1eea5baf39a20c0d932dbf06174143dace`.
Both `/usr/bin/wine64 --version` and `/usr/bin/wine32 --version` report Wine 11.0
(Staging) when explicitly launched through native ARM FEX.

The initial console execution checks failed. With separate user-owned temporary
homes and win64/win32 prefixes, both `cmd /c echo` checks exited 53 because
`kernel32.dll` cannot be loaded (`c0000135`). The packaged DLLs exist in Fedora's
`wine-wow64/wine/{x86_64,i386}-windows` directories. Explicit `WINEDLLPATH` did
not resolve the failure. Preserve this distinction between version startup and
working Windows applications; prefix creation, DLL loading, graphics, and
sandbox integration remain acceptance requirements. The resource-path diagnosis
and subsequent console validation are recorded below.

Run the repeatable console check inside a dedicated native ARM container with
an init process, no network, a private executable `/tmp`, a read-only guest
rootfs, and a writable results directory:

```sh
python3 /qos/os/tests/fex-wine-smoke.py \
  --rootfs /rootfs --output-dir /results
```

It starts FEXServer with the readiness handshake, creates independent temporary
win64 and win32 prefixes, and requires both a successful exit and the expected
Windows `cmd.exe` output for each architecture. It records version, console,
cleanup, and server logs plus `result.json`, and returns nonzero on failure.
Run with normal FEX settings; diagnostic JIT overrides are not acceptance.

### Wine resource paths must be visible to Windows path resolution

The initial DLL failure was reproduced under a file-operation trace. Wine
resolved its built-in DLL through
`.../dosdevices/z:/usr/lib64/wine-wow64/wine/x86_64-windows/kernel32.dll`.
That prefix is outside the guest rootfs and its `z:` symlink points to the
container root. FEX's guest-library lookup alone does not make this Windows
path resolve into `/rootfs`. The DLL was present, but not at the path Wine used.
Neither an explicit guest `WINEDLLPATH`, single-instruction JIT blocks, nor
disabling the outer container seccomp filter fixed this failure.

For the Fedora diagnostic runtime, expose its packaged resource trees at the
same paths in the isolated container, read-only, alongside the guest rootfs:

```sh
-v "$rootfs/usr/lib64/wine-wow64:/usr/lib64/wine-wow64:ro" \
-v "$rootfs/usr/lib/wine-wow64:/usr/lib/wine-wow64:ro" \
-v "$rootfs/usr/share/wine:/usr/share/wine:ro" \
-v "$rootfs:/rootfs:ro"
```

This preserves the original DLLs, package layout, and normal FEX settings.
Do not expose the host filesystem wholesale or populate prefixes by manually
copying DLLs. Product sandbox integration must make its packaged Wine resources
visible to both Unix and Windows path resolution; these Fedora paths are a
validation input, not a portable assumption about every Wine distribution.

The smoke test accepts `wineserver -k` returning 1 when no server remains and
then separately requires `wineserver -w` to finish successfully. Upstream
[`kill_lock_owner`](https://github.com/wine-mirror/wine/blob/wine-11.0/server/request.c)
and the [command-line handler](https://github.com/wine-mirror/wine/blob/wine-11.0/server/main.c)
explain why requiring a zero kill status after normal server exit was incorrect.
Console execution still requires exit zero and the exact output marker.

With these read-only resource mounts, the tracked smoke test passed for both
64-bit and 32-bit Wine. Each created a fresh prefix, ran `cmd.exe /c echo`,
returned zero with the exact marker, and completed the independent server wait.
The run used the default Podman seccomp policy, `--cap-drop=ALL`, no network,
`--init`, a private executable `/tmp`, and normal FEX JIT settings. The recorded
result is `fex-wine-resource-mount-results/result.json` in the preparation
workspace. This is console and prefix acceptance only; graphics, representative
applications, and integration with the product sandbox remain unverified.

## Vulkan command execution

`os/tests/fixtures/fex-vulkan-buffer.c` creates a Vulkan device and a 4 KiB
host-visible buffer, submits `vkCmdFillBuffer`, applies a transfer-to-host memory
barrier, waits for the queue, invalidates the mapped range, and checks all 1,024
words. It exits unsuccessfully on a Vulkan error or data mismatch. This exercises
handles, command buffers, synchronization, memory mapping, and readback across
the bridge; it does not claim shader rendering or presentation coverage.

Build the same source natively and for the x86-64 guest inside the native FEX
build environment. `/headers` is the pinned FEX Vulkan-Headers include directory,
`/sdk` is the complete x86 development SDK, and `/rootfs` is the Wine runtime:

```sh
clang -std=c11 -Wall -Wextra -Werror -I/headers \
  /qos/os/tests/fixtures/fex-vulkan-buffer.c -o /tmp/native-probe \
  -l:libvulkan.so.1
clang --target=x86_64-linux-gnu --sysroot=/sdk --gcc-toolchain=/sdk/usr \
  -fuse-ld=lld -std=c11 -Wall -Wextra -Werror -I/headers \
  /qos/os/tests/fixtures/fex-vulkan-buffer.c -o /tmp/guest-probe \
  /rootfs/usr/lib64/libvulkan.so.1
```

Link only the guest Vulkan loader from the runtime; adding the runtime's entire
library directory to the linker search path can select its libc linker script
outside the development sysroot. Keep the SDK's C runtime and linker inputs.

With an explicitly managed FEXServer, set `FEX_ROOTFS=/rootfs` and
`FEX_THUNKCONFIG` to the pinned `Data/CI/VulkanThunks.json`, then run the native
probe and `FEX /tmp/guest-probe`. Both passed on ARM with the normal JIT, no added
capabilities, default Podman seccomp policy, and no network. ELF inspection
confirmed AArch64 and x86-64 respectively. Both reported
`QOS_VULKAN_BUFFER_PASS 4096 bytes` using Mesa 26.1.8 llvmpipe / LLVM 22.1.8.
A separate `LD_DEBUG=libs` run confirmed the translated process loaded
`/usr/lib64/fex-emu/HostThunks/libvulkan-host.so` and the native
`/usr/lib64/libvulkan_lvp.so`, and passed again.

The transfer probe passed both with automatic driver selection and with
`VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.aarch64.json`. Guest
`vulkaninfo --summary` also passed with that explicit software driver. Its
initial automatic-driver run terminated with SIGILL, which remains unresolved;
the narrower transfer success does not clear that failure. These are software
Vulkan results, not physical GPU acceptance, Windows DXVK acceptance, or proof
of a 32-bit Vulkan guest thunk.

### Vulkan debug callback failure

A native ARM `vulkaninfo --summary` control passed with automatic driver
selection in the same diagnostic container where FEX exited 132 (SIGILL).
The container used native `vulkan-tools-1.4.341.0-1.fc44.aarch64` and the existing
native loader/Mesa stack. A debugger run passed FEX's handled SIGBUS and SIGSEGV
signals to reach the unhandled SIGILL. The native stack returned from
`libvulkan_powervr_mesa.so` to address `0x5555555756a0`, where instruction bytes
were `f3 0f 1e fa`: an x86 ENDBR64 entry being executed as ARM instructions.
This is evidence of an untranslated guest callback reaching native code.

`os/tests/fixtures/fex-vulkan-callback.c` provides a deterministic additional
contract. It enables `VK_EXT_debug_utils`, creates a messenger with a guest
callback and user-data pointer, submits an explicit message, and requires that
callback to receive the message and user data exactly once. Compile it using
the same native/guest commands as the buffer fixture. The native ARM control
printed `QOS_VULKAN_CALLBACK_PASS`; FEX terminated with SIGILL even when using
only the native llvmpipe driver. Explicit software-driver selection therefore
does not solve callback support.

The pinned upstream implementation strips the older debug-report structure
from instance creation, leaves the debug-utils callback structure on the chain,
and substitutes dummy callbacks in separate custom messenger implementations.
The latest upstream `main` inspected on 2026-09-16 retained the same
[`vkCreateInstance` handling](https://github.com/FEX-Emu/FEX/blob/main/ThunkLibs/libvulkan/Host.cpp).
A complete repair must preserve callback delivery, argument conversion, user
data, and lifetime for both direct entry points and functions returned by
`vkGetInstanceProcAddr`; merely suppressing messages or forcing one driver is
not acceptance. Keep this fixture failing until that contract is satisfied.

Evidence logs in the preparation workspace:
`fex-vulkan-native-comparison.log`, `fex-vulkan-backtrace.log`, and
`fex-vulkan-callback-validation.log`. The callback defect remains a release
blocker for this translated Vulkan path. Passing transfer/readback tests do not
waive it or establish Windows DXVK compatibility.
