#!/usr/bin/env bash
# Whether this node's kernel can enforce the PDF helper's sandbox: prints the
# value for TILO_REQUIRE_PDF_SANDBOX, `1` where it can and nothing where not.
#
#   required=$(bash tools/ci/require-pdf-sandbox.sh)
#   export TILO_REQUIRE_PDF_SANDBOX=$required
#
# The helper refuses to parse PDFs without a fully enforced Landlock ruleset
# at the ABI Tilo targets (crates/system/sandbox-core: TARGET_ABI). Where the
# kernel offers it, TILO_REQUIRE_PDF_SANDBOX makes an unconfined helper a test
# failure instead of the development build's fallback to the in-process engine
# (crates/platform/tilo-pdf/src/lib.rs, "When the sandbox cannot be
# enforced"), so a regression that leaves the helper unconfined fails here
# instead of passing as a host without Landlock. A node without it still runs
# every PDF test that does not need the sandbox, and says on stderr that the
# sandbox went unverified.
set -euo pipefail

required_abi=5

# landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION) returns the
# kernel's Landlock ABI; the syscall number is 444 on x86_64 and aarch64.
abi=$(python3 - <<'PY'
import ctypes
libc = ctypes.CDLL(None, use_errno=True)
libc.syscall.restype = ctypes.c_long
abi = libc.syscall(444, None, ctypes.c_size_t(0), ctypes.c_uint32(1))
print(max(abi, 0))
PY
)

if [ "$abi" -ge "$required_abi" ]; then
    echo "Landlock ABI $abi: PDF tests require the helper's sandbox" >&2
    echo 1
else
    echo "warning: Landlock ABI $abi (< $required_abi) on this node: the PDF helper cannot confine itself, so its sandbox tests stand down and the viewer tests use the in-process engine" >&2
fi
