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
