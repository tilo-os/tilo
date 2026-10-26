#!/usr/bin/bash
# Tests for the shell helpers the tilo-rollback systemd units use.
#
# These wrappers decide whether a machine discards its rollback fallback, so
# "it looked right" is not good enough. What is testable without a booted machine
# is the LUKS reasoning and the argument handling; the parts that need a real root
# filesystem (device-mapper discovery) and a real boot (the health gate) are called
# out at the end rather than faked.
#
#   ./os/tests/rollback-wrappers.sh [path-to-tilo-rollback-binary]
#
# Needs cryptsetup and a tilo-rollback binary. Exits non-zero on the first failure.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
TILO_ROLLBACK_BIN="${1:-$REPO/target/debug/tilo-rollback}"

pass=0
fail=0
ok()   { printf '  ok   %s\n' "$*"; pass=$((pass+1)); }
bad()  { printf '  FAIL %s\n' "$*"; fail=$((fail+1)); }
check(){ if [ "$1" = "$2" ]; then ok "$3"; else bad "$3 (got '$1', wanted '$2')"; fi; }

for tool in cryptsetup; do
  command -v "$tool" >/dev/null || { echo "SKIP: $tool is not installed"; exit 0; }
done
[ -x "$TILO_ROLLBACK_BIN" ] || { echo "SKIP: no tilo-rollback binary at $TILO_ROLLBACK_BIN"; exit 0; }
TILO_ROLLBACK_BIN="$(realpath "$TILO_ROLLBACK_BIN")"

# The helpers call `tilo-rollback` by name, so put the binary under test on PATH.
BINDIR="$(mktemp -d)"
ln -sf "$TILO_ROLLBACK_BIN" "$BINDIR/tilo-rollback"
PATH="$BINDIR:$PATH"
export PATH
# The units run it from /usr/libexec/tilo; the helpers take this path instead.
export TILO_ROLLBACK_BIN

# shellcheck source=/dev/null
. "$REPO/os/usr/libexec/tilo-rollback-common"

W="$(mktemp -d)"
cleanup() { rm -rf "$W" "$BINDIR"; }
trap cleanup EXIT

luks_image() { # $1 = image path, $2 = key file
  truncate -s 32M "$1"
  cryptsetup luksFormat --type luks2 --batch-mode --pbkdf pbkdf2 \
    --pbkdf-force-iterations 1000 --key-file "$2" "$1"
}

echo "== rollback_has_independent_keyslot =="

printf 'recovery-key-for-the-wrapper-test' > "$W/rec.key"
luks_image "$W/one.img" "$W/rec.key"

# One keyslot, no tilo-rollback token: that slot is not ours, so it counts.
if rollback_has_independent_keyslot "$W/one.img"; then
  ok "a lone recovery keyslot counts as an independent unlock path"
else
  bad "a lone recovery keyslot should count as an independent unlock path"
fi

# Add a second keyslot and claim it with a token, the way an enrolled generation
# does. The recovery slot is still independent.
printf 'a-generation-secret' > "$W/gen.key"
cryptsetup luksAddKey --batch-mode --pbkdf pbkdf2 --pbkdf-force-iterations 1000 \
  --key-file "$W/rec.key" "$W/one.img" "$W/gen.key"
cat > "$W/token.json" <<'JSON'
{"type":"tilo-rollback","keyslots":["1"],"version":2,"generation":7,
 "nv_index":31002880,"nv_operand":7,"nv_base":0,"pcr_bank":"sha256","pcrs":[11],
 "platform_pcrs":[7],
 "pin":false,"sealed_public":"AA==","sealed_private":"AA==","key_name":"AA==",
 "policy_digest":"AA==",
 "predictions":[{"approved_policy":"AA==","signature":"AA==","label":""}]}
JSON
cryptsetup token import --token-id 0 --json-file "$W/token.json" "$W/one.img"
if rollback_has_independent_keyslot "$W/one.img"; then
  ok "the recovery keyslot is still independent once a generation is enrolled"
else
  bad "enrolling a generation must not hide the recovery keyslot"
fi

# Now the pathological case the gate exists for: every keyslot claimed by a
# tilo-rollback token, so there is nothing left that does not need the TPM.
luks_image "$W/two.img" "$W/gen.key"
cat > "$W/token0.json" <<'JSON'
{"type":"tilo-rollback","keyslots":["0"],"version":2,"generation":7,
 "nv_index":31002880,"nv_operand":7,"nv_base":0,"pcr_bank":"sha256","pcrs":[11],
 "platform_pcrs":[7],
 "pin":false,"sealed_public":"AA==","sealed_private":"AA==","key_name":"AA==",
 "policy_digest":"AA==",
 "predictions":[{"approved_policy":"AA==","signature":"AA==","label":""}]}
JSON
cryptsetup token import --token-id 0 --json-file "$W/token0.json" "$W/two.img"
if rollback_has_independent_keyslot "$W/two.img"; then
  bad "a disk whose only keyslot is TPM-sealed must NOT pass the gate"
else
  ok "a disk whose only keyslot is TPM-sealed is refused"
fi

# A device that is not LUKS at all must not pass.
truncate -s 1M "$W/plain.img"
if rollback_has_independent_keyslot "$W/plain.img"; then
  bad "a non-LUKS file must not pass the gate"
else
  ok "a non-LUKS file is refused"
fi

echo "== rollback_generation_from =="
check "$(rollback_generation_from 'root=x tilo.generation=42 quiet')" "42" "reads the generation from a command line"
check "$(rollback_generation_from 'root=x quiet')" "" "absent generation reads as empty"
check "$(rollback_generation_from 'tilo.generationx=9')" "" "a similar key is not mistaken for it"
check "$(rollback_generation_from 'a=1 tilo.generation=abc b=2')" "" "a non-numeric value is not accepted"
check "$(rollback_generation_from 'tilo.generation=0')" "0" "generation zero is a real generation"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
if [ "$fail" -gt 0 ]; then exit 1; fi

cat <<'NOTE'

Not covered here, and not pretended to be:
  * rollback_luks_device: os/tests/rollback_luks_device.sh, against a fake
    sysfs; on a real kernel, the sign-in end-to-end check.
  * rollback_system_is_healthy — needs a real boot; it reads systemctl state.
  * the units firing, and early userspace writing /run/cryptsetup-keys.d/<vol>.key
    before systemd-cryptsetup — needs a VM boot with a generation number on the
    sealed command line.
NOTE
