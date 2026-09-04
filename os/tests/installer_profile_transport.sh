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

# A common live layout mounts a squashfs through /dev/loop0. findmnt therefore
# yields a non-empty candidate list, but that loop's end is not the physical
# media tail where the imager stored the profile. Prove that discovery also
# merges the actual removable disk instead of treating the loop as sufficient.
discovered_media="$work/discovered-media.img"
cp "$media" "$discovered_media"
python3 - "$discovered_media" <<'PY'
import hashlib, json, sys
payload = json.dumps({
    "schema": 1, "hostname": "loop-backed", "username": "ada",
    "full_name": "ada Example", "locale": "en_US.UTF-8",
    "keyboard": "de", "timezone": "Europe/Berlin", "filesystem": "btrfs",
}, separators=(",", ":")).encode()
header = b"QOS_INSTALL_PROFILE_V1\n" + hashlib.sha256(payload).hexdigest().encode() + b"\n" + str(len(payload)).encode() + b"\n"
block = (header + payload).ljust(65536, b"\0")
with open(sys.argv[1], "r+b") as target:
    target.seek(-65536, 2)
    target.write(block)
PY
fake_bin="$work/fake-bin"
mkdir "$fake_bin"
loop_candidate="$work/mounted-loop.img"
truncate -s 65536 "$loop_candidate"
cat > "$fake_bin/findmnt" <<'SH'
#!/bin/sh
printf '/dev/loop0 /run/initramfs/live\n'
SH
cat > "$fake_bin/lsblk" <<SH
#!/bin/sh
case "\$*" in
  *'/dev/loop0') printf '/dev/loop0 loop\n' ;;
  *'NAME,TYPE,RM,TRAN'*)
    printf '%s disk 0 usb\n' '$discovered_media'
    [ -z "\${QOS_TEST_SECOND_PROFILE:-}" ] \
      || printf '%s disk 1 mmc\n' "\$QOS_TEST_SECOND_PROFILE"
    printf '%s disk 0 sata\n' '$work/internal.img'
    ;;
esac
SH
chmod +x "$fake_bin/findmnt" "$fake_bin/lsblk"
rm -rf "$out"
PATH="$fake_bin:$PATH" \
QOS_INSTALLER_PROFILE_DEVICE="$loop_candidate" \
QOS_INSTALLER_PROFILE_ALLOW_FILE=1 \
QOS_INSTALLER_PROFILE_OUT_DIR="$out" \
  "$extractor"
python3 - "$out/profile.json" <<'PY'
import json, sys
profile = json.load(open(sys.argv[1], encoding="utf-8"))
assert profile["hostname"] == "loop-backed"
PY

# Broadening discovery must not make an arbitrary choice when two attached
# removable devices both carry profiles. Preserve the exactly-one safety gate.
second_media="$work/second-media.img"
cp "$discovered_media" "$second_media"
rm -rf "$out"
PATH="$fake_bin:$PATH" \
QOS_INSTALLER_PROFILE_DEVICE="$loop_candidate" \
QOS_INSTALLER_PROFILE_ALLOW_FILE=1 \
QOS_INSTALLER_PROFILE_OUT_DIR="$out" \
QOS_TEST_SECOND_PROFILE="$second_media" \
  "$extractor"
[ ! -e "$out/profile.json" ]
grep -q 'More than one attached device' "$out/profile.error"

make_malformed_header() {
  local mode="$1"
  python3 - "$media" "$mode" <<'PY'
import hashlib, json, sys
payload = json.dumps({
    "schema": 1, "hostname": "studio", "username": "ada",
    "full_name": "ada", "locale": "en_US.UTF-8", "keyboard": "de",
    "timezone": "Europe/Berlin", "filesystem": "btrfs",
}, separators=(",", ":")).encode()
digest = hashlib.sha256(payload).hexdigest().encode()
length = str(len(payload)).encode()
if sys.argv[2] == "digest":
    digest = b"ab"
elif sys.argv[2] == "length":
    length = b"not-a-number"
elif sys.argv[2] == "oversize":
    length = b"65536"
header = b"QOS_INSTALL_PROFILE_V1\n" + digest + b"\n" + length + b"\n"
open(sys.argv[1], "wb").write(bytes(65536) + (header + payload).ljust(65536, b"\0"))
PY
}

for case in digest length oversize; do
  make_malformed_header "$case"
  rm -rf "$out"
  QOS_INSTALLER_PROFILE_DEVICE="$media" \
  QOS_INSTALLER_PROFILE_ALLOW_FILE=1 \
  QOS_INSTALLER_PROFILE_OUT_DIR="$out" \
    "$extractor"
  [ ! -e "$out/profile.json" ]
  grep -Eq 'malformed|too large' "$out/profile.error"
done
printf 'ok    embedded installer profile crosses the media boundary and corruption fails closed\n'
