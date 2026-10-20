#!/usr/bin/env bash
# The live installer unpacks a setup handoff stored ahead of the profile block.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
extractor="$repo_root/os/usr/libexec/tilo-installer-profile-extract"
# The extractor unpacks the handoff with the compiled installer helper; run
# this checkout's build of it, or the one TILO_STAGING_HANDOFF_BIN names.
if [ -n "${TILO_STAGING_HANDOFF_BIN:-}" ]; then
  export TILO_STAGING_HANDOFF="$TILO_STAGING_HANDOFF_BIN"
else
  cargo build --quiet --manifest-path "$repo_root/Cargo.toml" -p tilo-install-helpers --bin tilo-staging-handoff
  export TILO_STAGING_HANDOFF="${CARGO_TARGET_DIR:-$repo_root/target}/debug/tilo-staging-handoff"
fi
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
media="$work/media.img"
out="$work/out"

python3 - "$media" <<'PY'
import hashlib, sys
key = b"k" * 32
wifi = b"ssid=studio\n"
parts = [
    b"etc/marker\n4\nwifi",
    b"var/lib/tilo/device-private/keypair.bin\n" + str(len(key)).encode() + b"\n" + key,
    b"etc/NetworkManager/system-connections/tilo-setup.nmconnection\n"
    + str(len(wifi)).encode()
    + b"\n"
    + wifi,
]
payload = b"".join(parts)
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
test "$(stat -c %a "$out/setup-handoff/var/lib/tilo/device-private/keypair.bin")" = 640
test "$(stat -c %a "$out/setup-handoff/etc/NetworkManager/system-connections/tilo-setup.nmconnection")" = 600
python3 - "$media" <<'PY'
import sys
data = open(sys.argv[1], "rb").read()
assert data[:1000] == b"\xab" * 1000
PY
# A payload carrying the human root secret is refused whole.
secret_media="$work/secret.img"
python3 - "$secret_media" <<'PY'
import hashlib, sys
secret = b"human-root-secret"
wifi = b"ssid=studio\n"
payload = (
    b"etc/NetworkManager/system-connections/tilo-setup.nmconnection\n"
    + str(len(wifi)).encode() + b"\n" + wifi
    + b"var/lib/tilo/user-identity/privkey.bin\n" + str(len(secret)).encode() + b"\n" + secret
)
digest = hashlib.sha256(payload).hexdigest().encode()
header = b"TILO_SETUP_HANDOFF_V1\n" + digest + b"\n" + str(len(payload)).encode() + b"\n"
header = header.ljust(4096, b"\0")
open(sys.argv[1], "wb").write(b"\xab" * 1000 + payload + header + b"\0" * 65536)
PY
TILO_INSTALLER_PROFILE_DEVICE="$secret_media" \
TILO_INSTALLER_PROFILE_ALLOW_FILE=1 \
TILO_INSTALLER_PROFILE_OUT_DIR="$work/secret-out" \
  "$extractor"
test ! -e "$work/secret-out/setup-handoff"
grep -q "human root secret" "$work/secret-out/profile.error"

# A record naming a path outside the handoff is refused whole.
for escaping in ../escape /etc/escape; do
  escape_media="$work/escape.img"
  python3 - "$escape_media" "$escaping" <<'PY'
import hashlib, sys
payload = b"etc/marker\n4\nwifi" + sys.argv[2].encode() + b"\n4\nevil"
digest = hashlib.sha256(payload).hexdigest().encode()
header = b"TILO_SETUP_HANDOFF_V1\n" + digest + b"\n" + str(len(payload)).encode() + b"\n"
open(sys.argv[1], "wb").write(b"\xab" * 1000 + payload + header.ljust(4096, b"\0") + b"\0" * 65536)
PY
  rm -rf "$work/escape-out"
  TILO_INSTALLER_PROFILE_DEVICE="$escape_media" \
  TILO_INSTALLER_PROFILE_ALLOW_FILE=1 \
  TILO_INSTALLER_PROFILE_OUT_DIR="$work/escape-out" \
    "$extractor"
  test ! -e "$work/escape-out/setup-handoff"
done

# Automatic discovery on a non-removable composefs installer must find the
# parent of /sysroot's partition. Model lsblk's real tree-vs-raw behaviour;
# no explicit profile device or removable-media fallback can hide a failure.
mkdir "$work/bin"
cat > "$work/bin/findmnt" <<'SH'
#!/bin/sh
printf '/dev/tilo-test-partition /sysroot\n'
SH
cat > "$work/bin/lsblk" <<'SH'
#!/bin/sh
case "$*" in
  *NAME,TYPE,RM,TRAN*) exit 0 ;;
esac
case "$1" in
  *r*) printf '%s disk\n' "$TILO_TEST_MEDIA" ;;
  *) printf '└─%s disk\n' "$TILO_TEST_MEDIA" ;;
esac
SH
# Only the synthetic mount source is treated as a block device. The actual
# tail reader still consumes the real fixture and verifies its checksum.
cat > "$work/bash-env" <<'SH'
function [() {
  if builtin [ "$#" -eq 3 ] && builtin [ "$1" = -b ] && builtin [ "$2" = /dev/tilo-test-partition ]; then
    return 0
  fi
  builtin [ "$@"
}
SH
chmod +x "$work/bin/findmnt" "$work/bin/lsblk"
PATH="$work/bin:$PATH" BASH_ENV="$work/bash-env" TILO_TEST_MEDIA="$media" \
TILO_INSTALLER_PROFILE_ALLOW_FILE=1 TILO_INSTALLER_PROFILE_OUT_DIR="$work/discovered" \
  bash "$extractor"
test "$(cat "$work/discovered/setup-handoff/etc/marker")" = wifi
test "$(stat -c %a "$work/discovered/setup-handoff/etc/NetworkManager/system-connections/tilo-setup.nmconnection")" = 600

echo "setup handoff transport ok"
