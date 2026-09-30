#!/usr/bin/env bash
# The live installer unpacks a setup handoff stored ahead of the profile block.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
extractor="$repo_root/os/usr/libexec/tilo-installer-profile-extract"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
media="$work/media.img"
out="$work/out"

python3 - "$media" <<'PY'
import hashlib, sys
payload = b"etc/marker\n4\nwifi"
digest = hashlib.sha256(payload).hexdigest().encode()
header = b"TILO_SETUP_HANDOFF_V1\n" + digest + b"\n" + str(len(payload)).encode() + b"\n"
header = header.ljust(4096, b"\0")
profile = b"\0" * 65536
image = b"\xab" * 1000
open(sys.argv[1], "wb").write(image + payload + header + profile)
PY

TILO_INSTALLER_PROFILE_DEVICE="$media" \
TILO_INSTALLER_PROFILE_ALLOW_FILE=1 \
TILO_INSTALLER_PROFILE_OUT_DIR="$out" \
  "$extractor"
test "$(cat "$out/setup-handoff/etc/marker")" = wifi
python3 - "$media" <<'PY'
import sys
data = open(sys.argv[1], "rb").read()
assert data[:1000] == b"\xab" * 1000
PY
echo "setup handoff transport ok"
