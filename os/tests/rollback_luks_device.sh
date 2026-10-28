#!/usr/bin/bash
# rollback_luks_device (os/usr/libexec/tilo-rollback-common) finds the LUKS2
# partition under the root by content, through sysfs, as the TPM lockout
# hardening, generation confirmation and repair mode need it.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
fail=0
check() {
  if [ "$1" = "$2" ]; then
    printf 'ok: %s\n' "$3"
  else
    printf 'FAIL: %s (got %s, wanted %s)\n' "$3" "'$1'" "'$2'"
    fail=1
  fi
}
# shellcheck source=/dev/null
. "$REPO/os/usr/libexec/tilo-rollback-common"

# A booted composefs root: / is composefs (no block device), /sysroot is the
# open LUKS2 mapping root (dm-0) on vda3. The kernel's record of that lives in
# sysfs; a fake one stands in for it. cryptsetup isLuks is answered from the
# LUKS magic at the start of the device, so the suite needs no cryptsetup.
cryptsetup() {
  [ "$1" = isLuks ] && [ "$(head -c 6 "$2" | od -An -tx1 | tr -d ' \n')" = 4c554b53babe ]
}
fake="$W/fake-root"
mkdir -p "$fake/sys/dev/block/253:0/dm" "$fake/sys/dev/block/253:0/slaves" "$fake/sys/dev/block/252:3" "$fake/dev"
printf 'CRYPT-LUKS2-0123456789abcdef0123456789abcdef-root\n' >"$fake/sys/dev/block/253:0/dm/uuid"
ln -s ../../252:3 "$fake/sys/dev/block/253:0/slaves/vda3"
printf 'LUKS\272\276\000\002' >"$fake/dev/vda3"
mounts() { # findmnt -nro MAJ:MIN MOUNT, from $FAKE_SYSROOT and $FAKE_ROOT
  case "$3" in /sysroot) [ -n "$FAKE_SYSROOT" ] && echo "$FAKE_SYSROOT" ;; /) echo "$FAKE_ROOT" ;; esac
}
discover() {
  ( findmnt() { mounts "$@"; }
    TILO_ROLLBACK_SYSFS="$fake/sys" TILO_ROLLBACK_DEVROOT="$fake/dev" \
      TILO_ROLLBACK_ROOT_LINK="$fake/no-link" rollback_luks_device )
}
check "$(FAKE_SYSROOT=253:0 FAKE_ROOT=0:31 discover)" "$fake/dev/vda3" \
  "a composefs root over a LUKS /sysroot finds the LUKS partition through sysfs"
check "$(FAKE_SYSROOT='' FAKE_ROOT=253:0 discover)" "$fake/dev/vda3" \
  "a LUKS root mounted at / is found the same way"
check "$(FAKE_SYSROOT=252:3 FAKE_ROOT=0:31 discover)" "" "a plain partition is not encrypted"
check "$(FAKE_SYSROOT='' FAKE_ROOT=0:31 discover)" "" "a root no block device backs is not encrypted"
printf 'CRYPT-PLAIN-root\n' >"$fake/sys/dev/block/253:0/dm/uuid"
check "$(FAKE_SYSROOT=253:0 FAKE_ROOT=0:31 discover)" "" "a plain dm-crypt mapping is not LUKS"
printf 'CRYPT-LUKS2-0123456789abcdef0123456789abcdef-root\n' >"$fake/sys/dev/block/253:0/dm/uuid"
truncate -s 0 "$fake/dev/vda3"; truncate -s 1M "$fake/dev/vda3"
check "$(FAKE_SYSROOT=253:0 FAKE_ROOT=0:31 discover)" "" "a device without a LUKS header is never handed on"

exit "$fail"
