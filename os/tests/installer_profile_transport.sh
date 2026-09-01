#!/usr/bin/env bash
# Exercise the exact fixed-block format across the imager/live shell boundary.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
extractor="$repo_root/os/usr/libexec/qos-installer-profile-extract"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
media="$work/media.img"
out="$work/out"

python3 - "$media" <<'PY'
import hashlib, json, sys
payload = json.dumps({
    "schema": 1,
    "hostname": "studio",
    "username": "ada",
    "full_name": "ada Example",
    "locale": "en_US.UTF-8",
    "keyboard": "de",
    "timezone": "Europe/Berlin",
    "filesystem": "btrfs",
}, separators=(",", ":")).encode()
header = b"QOS_INSTALL_PROFILE_V1\n" + hashlib.sha256(payload).hexdigest().encode() + b"\n" + str(len(payload)).encode() + b"\n"
block = (header + payload).ljust(65536, b"\0")
open(sys.argv[1], "wb").write(bytes(65536) + block)
PY

QOS_INSTALLER_PROFILE_DEVICE="$media" \
QOS_INSTALLER_PROFILE_ALLOW_FILE=1 \
QOS_INSTALLER_PROFILE_OUT_DIR="$out" \
  "$extractor"
python3 - "$out/profile.json" <<'PY'
import json, sys
profile = json.load(open(sys.argv[1], encoding="utf-8"))
assert profile["hostname"] == "studio"
assert profile["keyboard"] == "de"
assert "password" not in profile
PY

printf X | dd of="$media" bs=1 seek="$((65536 + 100))" conv=notrunc status=none
rm -rf "$out"
QOS_INSTALLER_PROFILE_DEVICE="$media" \
QOS_INSTALLER_PROFILE_ALLOW_FILE=1 \
QOS_INSTALLER_PROFILE_OUT_DIR="$out" \
  "$extractor"
[ ! -e "$out/profile.json" ]
grep -q 'damaged' "$out/profile.error"
printf 'ok    embedded installer profile crosses the media boundary and corruption fails closed\n'
