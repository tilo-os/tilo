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

The complete rerun of FEX-2609 with this fixture finished in 590.61 seconds:
11,616 passes, 458 upstream skips, and zero failures across 12,074 cases.
This baseline does not cover the separate Fedora `ldconfig` crash.

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
Its complete native ARM suite then passed in 480.47 seconds: 11,616 passes,
458 upstream skips, and zero failures across 12,074 cases. The source manifest
now pins this revision and its recursive gitlinks. No downstream FEX patch or
production block-size override is required.

The upstream suite passing by itself did not expose this application startup
failure. Keep the additional regression and real-program checks when updating
FEX. A successful `ldconfig -p` check also does not replace validation of an
entire RPM transaction through FEX, graphics thunks, or Wine/Proton integration.
