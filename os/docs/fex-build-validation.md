# FEX native ARM build validation

FEX translates x86 instructions on ARM hosts. Its native executable alone does
not provide Wine/Proton support: the x86 userspace filesystem, graphics thunks,
and application sandbox integration must also be built and validated.

Current complete-suite result: 11,726 passes, 460 unchanged upstream skips,
and two failures across 12,188 tests. Vulkan thunk callback handling and an
upstream expected-failure mismatch remain unresolved. Independent CPU-list
and GLX pbuffer probes also remain failing. The native ARM and both x86 guest
architectures pass the window pixel-rendering comparison. This is not release
acceptance; detailed inputs and limits follow.

## Source and toolchain inputs

`os/external-assets/fex-source.env` pins FEX and its recursive gitlinks.
`os/dev/fetch-fex-source.sh DESTINATION` acquires that checkout or verifies an
existing clean checkout without resetting caller changes.

Local downstream fixes are carried separately under
`os/external-assets/fex-patches/`. Their ordered manifest binds each patch's
SHA-256 to the unchanged upstream revision. The payload build copies the input
into a disposable writable source directory, verifies that copy is pristine,
then checks and applies that series. It never patches the caller's source or
accepts an arbitrary dirty checkout. The patch files and an application record
ship with the payload; the OS consumer requires the same reviewed series.
Patch application does not establish execution acceptance. The local fixes
require the regression and capsule evidence described below before release.

`os/Containerfile.fex-buildroot` builds the native compiler/tool dependencies.
Pass the digest-pinned `BASE_REF` from `os/base-snapshot.env` and the external
asset cache as the named `github-assets` build context. The cache must contain
`nasm-2.16.03.tar.xz`; its official HTTPS source and SHA-256 are recorded in
`os/external-assets/github-assets.tsv`. Production consumers must pin the
resulting buildroot image by digest. No production FEX buildroot digest has
been promoted yet.

Local preparation has verified these existing immutable inputs:

- ARM buildroot: `localhost/tilo-fex-buildroot@sha256:a0af121da48ed8a9a73dea795200b5b8535dc2905633c70ab8df164909c6d7a7`.
- x86 SDK: `localhost/tilo-fex-thunk-sdk@sha256:ef722888334908045f0dc84489c654476b2ff1fb4b95132daaa638c3f3ef93ef`.

A read-only, network-disabled buildroot probe matched all 1,157 RPM identities
to its recorded inventory and checked Clang 21.1.8, NASM 2.16.03 (including
the SMSW regression), build tools and CMake configuration. Its installed LLD
is 22.1.8. The downstream incremental builder used this exact image ID.
The retained SDK extraction's package inventory matches the original image's
record; complete extracted-file equality has not been established. Production
builds mount the immutable SDK image read-only during compilation instead of
copying it into another layer. This preserves the full SDK without redundant
storage. These local identities do not establish release signing/promotion or
reproducibility of the earlier live-repository dependency resolution.

### Offline payload build contract

`os/Containerfile.fex-payload` consumes a native ARM buildroot and a separate
x86 development SDK by full OCI digest. Its named `fex-source` context must
retain the pinned checkout's Git metadata and recursive submodules. The build
verifies that clean source again, then configures and compiles without network
access. It does not resolve packages or fetch dependencies during compilation.

The recipe uses Clang/LLVM 21, both checked-in guest toolchains, all upstream
supported thunk builds, and LTO. It explicitly sets `TUNE_CPU=none` and
`TUNE_ARCH=generic`: the resulting package must not inherit the build machine's
CPU extensions. It installs the main project and both guest subprojects,
preserves upstream and bundled license/notice files, and records source,
buildroot packages, input digests, and CMake configuration in the payload.

`os/dev/validate-fex-payload.py STAGING_ROOT` verifies required executables,
host and guest thunk families, ELF classes and architectures, the thunk
database, and license-file presence. Its JSON inventory records content
hashes. Validation rejects escaping file symlinks and malformed or missing
artifacts. It is a packaging check, not dependency-closure or license review,
and it explicitly reports the upstream lack of an i386 Vulkan guest thunk.

Run its negative-fixture tests with:

```sh
python3 -m unittest discover -s os/tests -p test_fex_payload.py
```

On 2026-09-16 the exact recipe's CMake configuration completed offline against
the existing native buildroot and complete guest SDK with portable CPU settings.
Mutable input references were rejected before compilation. The complete new
validator suite passed nine tests, and 36 staged files passed validation
in a separate fixture with the upstream license added. The original stage was
unchanged and correctly failed its missing-license check. A production buildroot/SDK
digest, separately frozen shipping guest filesystem, dependency-closure checks,
and passing runtime acceptance remain required before final image integration.

The portable configuration subsequently completed all 8,165 build steps. Main,
Guest and Guest_32 installation into a separate staging directory, with the
recipe's full license-copy step, passed the payload validator: 74 inventoried
files in a 29 MB stage. The unchanged full upstream suite then produced 11,726
passes, 460 skips and the same two failures across 12,188 tests in 497.52 seconds.
The Vulkan thunk `vulkaninfo` SIGILL and `syscalls_efault.64.jit.flt` failure
remain; portable CPU tuning introduced no additional failures in this suite.

Evidence is retained in `fex-portable-validation-summary.json`,
`fex-portable-payload-inventory.json`, `fex-portable-stage.log`, and
`fex-portable-complete-graphics-validation.log` in the preparation tree.
The diagnostic image is `localhost/tilo-fex-portable-runtime:arm64-guests`, ID
`0e5d405552f06a46515cdd3ce49862718d73f03a056023acd0f38209b9089299`.
Its installed FEX SHA-256 is
`fd1ff65e292759a60d56bffaf0df522e5068ff780272ee23bea99e7da1ca505b`;
FEXServer is
`86451ad92754d0f229c06848ead93f3427ab9ba8b87650b43831f725bc204d1b`.
These checks used existing local diagnostic input images. They do not constitute
a build of the complete new Containerfile against promoted immutable inputs,
or release acceptance.

### Existing frozen RPM input audit

After registry authentication was restored on 2026-09-16, the exact tracked
`RPMS_REF` was pulled and inspected without executing it or installing packages.
The image is amd64, ID
`69d60f03794d94b7ced9ab5c401f777ecc539e4da7acf4bd45202e57ca2a0a2f`.
Its 1,240 RPM headers report 860 x86_64 entries (including the kernel source RPM)
and 380 noarch entries, with no AArch64 or i686 entries. Its direct-spec manifest
matches all 173 current x86-64 requirements. Similar package names on ARM do not
make these binary packages usable there; the image preflight now rejects an
architecture mismatch before comparing package manifests.

The x86 Wine-core package supplies `wine64`, `wineserver64` and both Windows PE
DLL trees, but not the `wine32`/`wineserver32` executables required by the current
separate 32-bit runner. The snapshot is an installation transaction delta against
the pinned OS base, not a complete guest-rootfs repository. Missing Python,
shell or coreutils RPMs in this delta do not alone prove a broken OS transaction.
It cannot substitute for the separately frozen complete guest filesystem.

The mounted inspection was unmounted afterward. Package headers, direct specs,
entrypoint listings and the gap report are preserved as `rpms-pinned-*` files in
the preparation tree. The pinned Fedora base does resolve for ARM64; the tracked
kernel artifact returned `manifest unknown`, so the separately validated local
ARM kernel remains a preparation artifact rather than that missing published pin.

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
installation scripts when constructing that environment; ignoring the
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
`TILO_FEX_X86_SYSROOT` to the mounted x86 development filesystem. The wrapper
passes its sysroot and GCC toolchain to native Clang for the two x86 targets
requested by upstream `StructPackVerifier.py`. Other targets use native headers.
The upstream verifier and its expected layouts remain unchanged.

When using the versioned Clang 21 buildroot, also set
`TILO_FEX_CLANG=/usr/lib64/llvm21/bin/clang`. Its loader selects libclang 21 for
the Python verifier; discovering headers with the default Clang 22 driver
mixes resource headers from different versions. This caused all 20 AArch64
checks to fail with missing `int8_t` and related types. Matching the driver to
the library and using the complete SDK restored all 40 ABI checks (41.29
seconds, zero failures). Preserve this selection separately from the compiler
used to build guest binaries.

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
passed the regression and Fedora `ldconfig -p` with normal JIT settings.
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
`localhost/tilo-fex-x86-sdk:fedora44-fex-validated`, image ID
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
export TILO_FEX_X86_SYSROOT=/x86-sdk
cmake -S /src -B /build \
  -DCMAKE_C_COMPILER=/usr/lib64/llvm21/bin/clang \
  -DCMAKE_CXX_COMPILER=/usr/lib64/llvm21/bin/clang++ \
  -DClang_DIR=/usr/lib64/llvm21/lib64/cmake/clang \
  -DLLVM_DIR=/usr/lib64/llvm21/lib64/cmake/llvm \
  -DBUILD_THUNKS=ON -DBUILD_FEX_LINUX_TESTS=ON -DENABLE_CLANG_THUNKS=ON \
  -DX86_DEV_ROOTFS="$TILO_FEX_X86_SYSROOT" \
  -DX86_64_TOOLCHAIN_FILE=/tilo/os/dev/toolchains/fex-cross-x86_64.cmake \
  -DX86_32_TOOLCHAIN_FILE=/tilo/os/dev/toolchains/fex-cross-i686.cmake
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
`localhost/tilo-fex-wine-runtime:fedora44-dev` completed its package transaction
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
python3 /tilo/os/tests/fex-wine-smoke.py \
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
  /tilo/os/tests/fixtures/fex-vulkan-buffer.c -o /tmp/native-probe \
  -l:libvulkan.so.1
clang --target=x86_64-linux-gnu --sysroot=/sdk --gcc-toolchain=/sdk/usr \
  -fuse-ld=lld -std=c11 -Wall -Wextra -Werror -I/headers \
  /tilo/os/tests/fixtures/fex-vulkan-buffer.c -o /tmp/guest-probe \
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
`TILO_VULKAN_BUFFER_PASS 4096 bytes` using Mesa 26.1.8 llvmpipe / LLVM 22.1.8.
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
printed `TILO_VULKAN_CALLBACK_PASS`; FEX terminated with SIGILL even when using
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

### CPU-list text truncation affects both guest architectures

The `cpu_count.32` failure is not confined to i686. A controlled stack-content
probe reproduced it in x86-64 too. Native ARM reads five bytes (`30 2d 31 31 0a`,
`0-11` plus newline) from both CPU `online` and `present` sysfs files; the pinned
FEX implementation returns four bytes (`30 2d 31 31`) without the newline.
After initializing prior stack contents to an alternating digit/non-digit
pattern, both translated libc readers report 119 CPUs instead of 12. The native
control still reports 12. Without controlled stack contents, the same small
program can report 12 under FEX, so a single successful query is insufficient.

`os/tests/fixtures/fex-cpu-list.c` checks line termination, independently parses
the CPU range using a safely terminated local copy, and compares the seeded
libc online-CPU query with that count. It passes natively and exits 1 under both
x86-64 and i686 FEX. Build it with the existing native compiler and explicit guest
SDK targets, then run each guest executable through FEX. The evidence is in
`fex-cpu-list-validation.log`; this is a failing compatibility contract, not an
expected-failure waiver.

The upstream emulated-file implementation writes exactly `cpus_online.size()`
bytes, while constructing that string without a newline. Glibc's
[`read_sysfs_file` and `next_line`](https://github.com/bminor/glibc/blob/master/sysdeps/unix/sysv/linux/getsysstats.c)
parse CPU ranges with `strtoul`, making that omitted terminator significant.
The runtime used glibc 2.43-8.fc44. Restoring the Linux text-file contract is
required; changing the expected CPU count or forcing a thread-count setting
would not repair the underlying issue.

### Validated compiler API for thunk generation

An independent Clang 22.1.8 API probe reproduced the variadic-generator failure:
`ASTContext::getSizeType().getAsString()` returns `__size_t` for ARM64, x86-64,
and i686. Its canonical type is `unsigned long` (64 bits) for ARM64/x86-64 and
`unsigned int` (32 bits) for i686. The pinned FEX generator emits that internal
`__size_t` name into C++ without a declaration, causing its existing
`VariadicFunction.ThunkGen` test to fail. This is distinct from a missing guest
header package or an ARM ABI mismatch.

The native buildroot now includes Fedora's versioned Clang/LLVM 21 packages.
Select both compiler executables and both CMake package directories explicitly
as shown above; installing the packages alone does not change CMake's default
Clang 22 selection. The default compiler remains available to the existing
explicit x86 SDK toolchains. Use a fresh FEX build directory when changing the
native compiler.

The unmodified pinned FEX source configured and built `thunkgentest` with Clang
21.1.8, and all 15 `.ThunkGen` tests passed in 4.58 seconds. Build and test output
is retained in `fex-clang21-generator-validation.log`. The validation disabled
compiler caching after an unrelated cache-directory failure; optimization and
LTO remained enabled. A nested dependency still invoked the cached launcher,
so `CCACHE_DISABLE=1` and a writable isolated `CCACHE_DIR` were used.

This resolves the generator test in the validated toolchain configuration.
The complete emulator and guest libraries were subsequently rebuilt successfully
with this compiler selection. Full runtime validation is still required; the
compiler change does not repair the separately reproduced CPU-list or Vulkan
callback defects.

The exact updated `os/Containerfile.fex-buildroot` also built successfully as
`localhost/tilo-fex-buildroot:arm64-clang21`, image ID
`2b7fdf294cd47de7465f68c96bc9ab91399950b4c63a83a052c0832158054d36`.
It records the selected generator compiler version and refreshes the complete
RPM inventory after installing the versioned toolchain.

The complete build finished successfully in `fex-clang21-full-build.log`, using
the checked-in guest toolchains and the unmodified pinned FEX source. Its
installed validation image is `localhost/tilo-fex-clang21-runtime:arm64-dev`,
image ID `7f006db926f9213e38c7ba09ce4665aa5aa4951be7ff87f6a5ac959c77c7a157`.
Keep this build evidence separate from test-suite and application acceptance.

### Fresh-build test home and server supervision

The first complete Clang 21 suite attempt had an invalid fixture: `/build/home`
did not exist, FEX fell back to `/`, and its server could not create its state
directory. The readiness pipe closed during failure before the process exit
was observed, allowing CTest to start and report client-connection failures.
Those results are not emulator acceptance evidence. The stopped run is retained
as `fex-clang21-invalid-home-validation.log` and
`fex-clang21-invalid-home-server.log`.

`run-fex-tests.py` now creates its own home below the build directory and sets
HOME and XDG state paths explicitly. It also supervises the server throughout
CTest execution and terminates the test process group if the server exits.
The two formerly failing clock tests passed with the rebuilt server in
1.66 seconds. A separate process-lifecycle check verified that simulated server
exit is detected and its sleeping test child is terminated within five seconds.
The complete suite must still finish with this corrected fixture.

### Complete Clang 21 suite audit

The corrected server fixture completed all 12,188 configured tests in 481.59
seconds: 11,700 passed, 460 were skipped by upstream, and 28 failed. The log is
`fex-clang21-full-validation.log`. This is not a passing acceptance run:

- Twenty failures came from mismatched Clang resource headers. All 40 ABI
  checks subsequently passed with the matched driver described above.
- Two `glxinfo` tests resolved an ARM executable because the guest rootfs did
  not contain the requested x86 executable.
- Untranslated `vulkaninfo` found no usable guest driver.
- Translated `vulkaninfo`, `vulkan_procaddr.64`, and both `thunk_testlib`
  architectures lacked their installed guest libraries.
- `syscalls_efault.64` passed all 20 assertions in three cases, but upstream
  marks it as an expected failure. Its expectation remains unchanged.

The complete build produced guest libraries under both `Guest` and `Guest_32`,
but the initial installed image lacked them. Verify staged artifacts as well
as the top-level install command's exit status. Both guest subprojects have
their own install manifests:

```sh
DESTDIR=/stage cmake --install /build/Guest
DESTDIR=/stage cmake --install /build/Guest_32
file /stage/usr/share/fex-emu/GuestThunks/libvulkan-guest.so
file /stage/usr/share/fex-emu/GuestThunks_32/libfex_thunk_test-guest.so
```

The resulting files were verified as x86-64 and i386 ELF libraries respectively.
The complete local validation image is
`localhost/tilo-fex-clang21-runtime:arm64-guests`, ID
`6213032510a9da2cb348976c9900228ac8b8c7f8bed5c88c3bcf2a41f7483a2b`.
All 19 targeted generator and installed-thunk tests passed against this image
in 4.90 seconds, including `thunk_testlib.64`, `thunk_testlib.32`, and
`vulkan_procaddr.64`. The log is `fex-clang21-installed-thunks-validation.log`.
Graphics application acceptance and a full corrected run remain required.
A passing unseeded
CPU-count test in the full suite does not supersede the failing controlled
CPU-list regression fixture documented above.

### Graphics application comparison with complete drivers

The guest diagnostic runtime was extended with `glx-utils.x86_64` and both
x86-64/i686 Mesa DRI and Vulkan drivers, using normal RPM scriptlets through
explicit FEX execution. Its image is
`localhost/tilo-fex-graphics-guest:fedora44-dev`, ID
`a2e62822ac3d9498f75d38c1005fa09c089460cb1f3de9c4fee2364e8a75db14`.
The exported package inventory contains 1,070 entries. These diagnostic
packages came from Fedora repositories; they are not promoted frozen release
inputs.

The native ARM graphics host image adds Mesa drivers, GL/Vulkan tools, and
Xvfb to the complete installed FEX runtime. Its ID is
`5819150611346d43c72daa5a733e207b093ef89645d727d545c4f289ce0ff8a7`.
The comparison ran as UID 1000 with all capabilities dropped, an init process,
no external network, and Podman's default seccomp filter. Xvfb provided a
1280×800 display; Mesa software rendering was selected. Build artifacts and
the guest filesystem were read-only, with test output in a private temporary
directory.

Native ARM `glxinfo -B` reported OpenGL 4.6 with llvmpipe. Native
`vulkaninfo --summary` reported Vulkan 1.4.354 with llvmpipe. The four unchanged
upstream graphics tests then finished in 2.10 seconds:

| Guest application | Native graphics thunks | Result |
| --- | --- | --- |
| `glxinfo` | Disabled | Passed |
| `glxinfo` | Enabled | Passed |
| `vulkaninfo` | Disabled | Passed |
| `vulkaninfo` | Enabled | Illegal instruction |

The log is `fex-clang21-graphics-functional-validation.log`. Driver selection
was automatic; no Vulkan ICD override hid the thunk failure. These results
resolve the missing tools/drivers fixture problems and reproduce the remaining
Vulkan bridge failure with the complete Clang 21 build. They do not establish
physical GPU, shader workload, DXVK, or Proton acceptance.

### OpenGL pixel rendering on both guest architectures

`os/tests/fixtures/fex-gl-pixels.c` creates a GLX context, clears a 16×16 RGBA
surface, and verifies all 1,024 returned color channels (one unit of rounding
tolerance). It synchronizes OpenGL and X11 and completes context/display
cleanup before printing `TILO_GL_PIXELS_PASS`. It can exercise a window with
`--window`, or a pbuffer with no argument; retain both cases.

Build the native control with `clang -std=c11 -Wall -Wextra -Werror` and
`-lGL -lX11`. Cross-build with the existing SDK, explicit x86-64/i686 targets,
and lld. Link the guest rootfs's `usr/lib64/libGL.so.1` and `libX11.so.6` for
x86-64, and their `usr/lib` counterparts for i686. Verify each ELF architecture.
Run against Xvfb with software rendering, the complete rootfs, and the installed
FEX runtime. For thunk comparisons, select upstream `Data/CI/GLThunks.json` with
`FEX_THUNKCONFIG`; leave it unset for instruction translation alone.

All five window cases passed: native ARM, x86-64 with/without thunks, and i686
with/without thunks. The native and thunked renderer reported llvmpipe with
128-bit vectors; the translated guest renderer reported 256-bit vectors. These
tests used an ordinary UID, no capabilities, no external network, and the
default container seccomp filter. Results are retained in
`fex-gl-window-pixels-validation.log`.

The pbuffer comparison remains failing. Native ARM and all four guest cases
reported X11 `BadMatch` in `X_ShmPutImage`, despite matching 24-bit visual/display
depth and RGBA8 framebuffer selection. Native ARM, untranslated-graphics x86-64,
and both i686 cases exited 1; thunked x86-64 exited 139 after the X error.
`fex-gl-pbuffer-validation.log` preserves those results. Because the native
control also fails, this is not evidence of a translation-only defect. Do not
count the earlier successful local pixel readback as complete pbuffer acceptance
or hide the error by ignoring X11 failures.

A diagnostic rerun with Xvfb's MIT-SHM extension disabled failed in all five
cases with `BadMatch` in ordinary `X_PutImage`, preserving the same exit codes.
The failure therefore is not specific to shared-memory transport.
`fex-gl-pbuffer-no-shm-validation.log` records this comparison; disabling the
extension is not an accepted runtime workaround.

### Full suite with corrected graphics and compiler inputs

`fex-clang21-complete-graphics-validation.log` records the full, unfiltered
12,188-case run against the complete graphics images described above. The
run used the matched Clang 21 ABI driver, complete guest SDK, installed guest
thunks, both guest Mesa architectures, and Xvfb. It finished in 495.41 seconds:

- 11,726 passed;
- 460 retained upstream skips;
- two failures: `ThunkFunctionalTest-Thunks-/usr/bin/vulkaninfo` (illegal
  instruction) and `syscalls_efault.64.jit.flt` (unexpected pass).

No expected results or disabled-test lists were changed. The full-suite fixture
retains the earlier permission/device setup and diagnostic outer seccomp
override; the separate graphics and window-pixel comparisons passed their
documented cases with the default filter. The two full-suite failures and the
independent CPU-list/pbuffer failures remain open. A green generator or ABI
subset does not supersede these results.
