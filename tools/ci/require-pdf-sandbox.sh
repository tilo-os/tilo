#!/usr/bin/env bash
# Require the PDF helper's sandbox on runners whose kernel can enforce it.
#
# The helper refuses to parse PDFs without a fully enforced Landlock ruleset
# at the ABI Tilo targets (crates/system/sandbox-core: TARGET_ABI). On such a
# runner, TILO_REQUIRE_PDF_SANDBOX makes an unconfined helper a test failure
# instead of the development-build fallback to the in-process engine (see
# crates/platform/tilo-pdf/src/lib.rs, "When the sandbox cannot be enforced"). A runner
# without it still runs every PDF test that does not need the sandbox, and
# says here that the sandbox went unverified.
#
# Usage: require-pdf-sandbox.sh [ENV_FILE]   (default: $GITHUB_ENV)
set -euo pipefail

env_file=${1:-${GITHUB_ENV:?GITHUB_ENV is not set}}
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
  printf 'TILO_REQUIRE_PDF_SANDBOX=1\n' >>"$env_file"
  echo "Landlock ABI $abi: PDF tests require the helper's sandbox"
else
  echo "::warning::Landlock ABI $abi (< $required_abi) on this runner: the PDF helper cannot confine itself, so its sandbox tests stand down and the viewer tests use the in-process engine"
fi
