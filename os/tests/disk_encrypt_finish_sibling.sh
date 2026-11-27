#!/usr/bin/env bash
# The install-time decision behind the sibling slot, against fakes:
# `tilo-disk-encrypt finish --unlock … --var …` keeps the throwaway install key
# inside the encrypted root exactly when the planned unlock cannot be opened
# by this machine alone — an owner-device plan, or a tpm (or open) plan on a
# machine with no usable TPM. Every other plan drops it as before.
#
#   bash os/tests/disk_encrypt_finish_sibling.sh
#
# Needs no root, no block device and no real cryptsetup.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
finish="$repo/os/usr/libexec/tilo-disk-encrypt"
fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
bin="$work/bin"
mnt="$work/target"
var="$mnt/state/os/var"
dev="$work/volume"
log="$work/calls"
uuid=01234567-89ab-cdef-0123-456789abcdef
mkdir -p "$bin" "$var"

cat > "$bin/cryptsetup" <<'EOF'
#!/bin/sh
printf 'cryptsetup %s\n' "$*" >> "$FAKE_LOG"
case "$1" in
  luksDump) printf 'Keyslots:\n  0: luks2\n  1: luks2\n' ;;
  luksUUID) printf '%s\n' "$FAKE_UUID" ;;
  luksRemoveKey|close) : ;;
  *) printf 'unexpected cryptsetup call\n' >&2; exit 9 ;;
esac
EOF
chmod +x "$bin/cryptsetup"
: > "$dev"

fresh() {
  rm -rf "$var" "$mnt"
  mkdir -p "$var"
  ( umask 0077; head -c 64 /dev/urandom > "$work/luks.key" )
  : > "$log"
}

tpm_device="$work/no-tpm"
attempt() {
  PATH="$bin:$PATH" FAKE_LOG="$log" FAKE_UUID="$uuid" \
    TILO_TPM_DEVICE="$tpm_device" \
    bash "$finish" finish "$dev" --key-file "$work/luks.key" --mnt "$mnt" "$@" \
    > "$work/stdout" 2> "$work/stderr"
}

sibling_dir="$var/lib/tilo/install-key"

# --- an owner-device plan keeps the key inside the root ---------------------
fresh
if attempt --unlock owner-device --var "$var"; then ok "an owner-device plan finishes"; else
  bad "an owner-device plan failed: $(cat "$work/stderr")"
fi
flagged_key="$(ls -l "$sibling_dir/luks.key" 2>/dev/null | cut -c1-10)"
[ "$flagged_key" = "-rw-------" ] && ok "the kept key is root-only" \
  || bad "the kept key is not mode 0600 (got '$flagged_key')"
[ "$(stat -c '%a' "$sibling_dir")" = 700 ] && ok "its directory is 0700" \
  || bad "the kept key's directory is not 0700"
grep -qx "luks_uuid=$uuid" "$sibling_dir/volume.conf" \
  && ok "the enrolment file names the volume" \
  || { bad "the enrolment file is wrong"; cat "$sibling_dir/volume.conf" >&2; }
grep -qx "key_file=$sibling_dir/luks.key" "$sibling_dir/volume.conf" \
  && ok "the enrolment file names the kept key" || bad "the enrolment file names another key"
[ "$(stat -c '%a' "$sibling_dir/volume.conf")" = 600 ] \
  && ok "the enrolment file is root-only" || bad "the enrolment file is not 0600"
[ -f "$var/lib/tilo/state/sibling-unlock-pending" ] \
  && ok "the installed status starts out pending" || bad "no pending marker was written"
grep -q luksRemoveKey "$log" && bad "an owner-device plan removed the install key slot" \
  || ok "the install key slot stays until the sibling slot exists"
[ -e "$work/luks.key" ] && bad "the installer's copy of the key survived" \
  || ok "only the copy inside the root remains"
grep -q -- "$(od -An -tx1 "$sibling_dir/luks.key" | tr -d ' \n')" "$work/stdout" "$work/stderr" \
  && bad "the key's bytes reached a log" || ok "the key's value never reached a log"

# --- tpm with a usable TPM drops it ----------------------------------------
fresh
tpm_device=/dev/null
attempt --unlock tpm --var "$var" \
  && ok "a tpm plan on a machine with a TPM finishes" \
  || bad "a tpm plan with a TPM failed: $(cat "$work/stderr")"
tpm_device="$work/no-tpm"
grep -q luksRemoveKey "$log" && ok "a machine that can seal an unattended slot drops the key" \
  || { bad "a TPM machine kept an install key it does not need"; cat "$log" >&2; }
[ ! -e "$sibling_dir" ] && ok "nothing is left inside the root" || bad "the kept key survived"

# --- tpm with no usable TPM keeps it ---------------------------------------
fresh
attempt --unlock tpm --var "$var" && ok "a tpm plan without a TPM finishes" \
  || bad "a tpm plan without a TPM failed: $(cat "$work/stderr")"
[ -f "$sibling_dir/luks.key" ] && ok "a machine with no TPM keeps the key for the sibling slot" \
  || bad "a machine with no TPM dropped its only unattended way in"
grep -q luksRemoveKey "$log" && bad "a machine with no TPM removed the install key slot" \
  || ok "the install key slot stays"

# --- an open plan with no usable TPM keeps it ------------------------------
fresh
attempt --var "$var" && ok "an unplanned unlock without a TPM finishes" \
  || bad "an unplanned unlock without a TPM failed: $(cat "$work/stderr")"
[ -f "$sibling_dir/luks.key" ] && ok "an open plan falls back to the sibling slot" \
  || bad "an open plan with no TPM dropped the key"

# --- a passphrase plan drops it, TPM or not --------------------------------
fresh
attempt --unlock passphrase --var "$var" && ok "a passphrase plan finishes" \
  || bad "a passphrase plan failed: $(cat "$work/stderr")"
grep -q luksRemoveKey "$log" && ok "a passphrase plan keeps its promise and drops the key" \
  || bad "a passphrase plan kept the install key"
[ ! -e "$sibling_dir" ] && ok "a passphrase plan leaves nothing inside the root" \
  || bad "a passphrase plan left the key"

# --- without an installed /var (a desktop) nothing is kept -----------------
fresh
attempt --unlock owner-device && ok "a caller with no installed /var finishes" \
  || bad "a caller without /var failed: $(cat "$work/stderr")"
grep -q luksRemoveKey "$log" && ok "a desktop's install drops the install key" \
  || bad "a desktop install kept the key"

# --- refusals ---------------------------------------------------------------
fresh
attempt --unlock password --var "$var" && bad "an unknown unlock was accepted" \
  || ok "an unknown unlock is refused"
fresh
attempt --unlock owner-device --var "$work/elsewhere" && bad "a /var outside the root was accepted" \
  || ok "a /var outside the encrypted root is refused"
[ -e "$work/luks.key" ] && ok "a refusal leaves the installer's key alone" \
  || bad "a refusal removed the key"

if [ "$fail" -ne 0 ]; then
  printf '\nfinish sibling decision: FAILURES\n' >&2
  exit 1
fi
printf '\nfinish sibling decision: every check passed\n'
