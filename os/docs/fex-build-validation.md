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
