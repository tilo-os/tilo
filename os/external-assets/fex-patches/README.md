# Local FEX compatibility patches

`series.json` pins these downstream changes to upstream commit
`91bbce5a96b9e1eaa32f95f440ea8b46bbfbdf20`. The upstream checkout and recursive
submodules remain separately pinned and must be clean before application.

1. **SVE byte addressing:** form the complete guest byte address before using
   a zero SVE immediate. This avoids host-vector-length scaling, malformed
   signed/large immediate encoding, and element-size scaling in masked loads
   and stores. The existing address helper preserves extension and IR scale;
   both non-temporal widths also use explicit byte addresses. Four ASM tests
   cover these cases. SVE remains enabled.
2. **x86 `prctl` ABI:** return `EINVAL` for the four ARM SVE/SME vector-length
   controls. Native x86 Linux rejects these options. Forwarding SVE changes
   to the ARM host could invalidate FEX's cached vector-width assumptions.
   SME controls a separate streaming vector length, but is likewise outside
   the x86 ABI. The regression is built for both guest widths and includes
   ordinary `prctl` behavior.
3. **Thunk identifier hashing:** use LLVM's SHA-256 implementation already
   linked into the generator instead of requiring OpenSSL headers and libcrypto
   in the FEX cross-build toolchain. The input bytes, 32-byte output, and
   generated thunk identifiers remain unchanged.

The payload builder applies the checksum-verified series in a disposable source
copy and ships the patch files plus its application record. The OS consumer
requires exactly this series. Neither a patch record nor an image digest proves
runtime acceptance.

## Validation boundary

All patches apply to a clean checkout of the pinned commit and reproduce the
source files used by the incremental build. The compiler completed both native
executables and the new 32/64-bit syscall tests. The full native upstream suite was started against the previously recorded
baseline, but its final result has not been collected. The validation agent was
stopped by a service content filter; the suite must not be reported as passed.

The unchanged binary reproduces the SVE address failures. A preliminary address
fix passed its original cases at 128-, 256- and 512-bit SVE, but review then added
the masked-element correction. Those earlier successes do **not** validate the
revised patch or the syscall guard. The final series still requires SVE-enabled
execution, the complete capsule tests and graphics acceptance. The local WSL
CPU has no SVE, so native test results cannot substitute for those checks.
