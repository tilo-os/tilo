#!/usr/bin/bash
# Boot-side checks for bounded rollback. Run INSIDE a booted qOS guest.
#
#   ./os/dev/vm.sh boot --tpm          # then, in the guest:
#   sudo /usr/libexec/qos-rollback-boot-check
#
# Everything here needs a real boot and cannot be established from the host:
# whether the generation number arrived on the sealed command line, whether the
# measurements it is bound to are meaningful, whether early userspace produced the
# key file before systemd-cryptsetup wanted it, and whether the units fired.
#
# Prints one line per check and exits non-zero if any REQUIRED check failed.
# Checks that are "not applicable" (no TPM, no encryption) say so and do not fail:
# a machine without the hardware is supposed to keep working.
set -uo pipefail

pass=0; fail=0; skip=0
ok()   { printf '[ OK ]   %s\n' "$*"; pass=$((pass+1)); }
no()   { printf '[FAIL]   %s\n' "$*"; fail=$((fail+1)); }
na()   { printf '[ n/a ]  %s\n' "$*"; skip=$((skip+1)); }
info() { printf '[info]   %s\n' "$*"; }

echo "=== bounded rollback: boot-side checks ==="

# --- the generation number ---------------------------------------------------
GEN="$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^qos\.generation=\([0-9][0-9]*\)$/\1/p' | head -1)"
if [ -n "$GEN" ]; then
  ok "qos.generation=$GEN is on the kernel command line"
else
  no "no qos.generation= on the kernel command line — rollback protection is not bound to anything"
fi

# It has to come from the UKI, not from a bootloader config. The strongest
# evidence available inside the guest is that the stub measured a command line and
# that the number is in the image too.
if [ -r /usr/lib/qos/generation ]; then
  FILE_GEN="$(cat /usr/lib/qos/generation)"
  if [ "$FILE_GEN" = "$GEN" ]; then
    ok "/usr/lib/qos/generation agrees with the command line ($FILE_GEN)"
  else
    no "/usr/lib/qos/generation says $FILE_GEN but the command line says ${GEN:-<absent>}"
  fi
else
  no "/usr/lib/qos/generation is missing — a staged deployment's generation cannot be read"
fi

if [ -d /sys/firmware/efi ]; then
  ok "booted via UEFI"
else
  na "not a UEFI boot; measured boot does not apply"
fi

# --- the TPM and its measurements --------------------------------------------
if [ -e /dev/tpmrm0 ] || [ -e /dev/tpm0 ]; then
  ok "a TPM is present ($( [ -e /dev/tpmrm0 ] && echo /dev/tpmrm0 || echo /dev/tpm0 ))"

  if command -v tpm2_pcrread >/dev/null 2>&1; then
    zero='0000000000000000000000000000000000000000000000000000000000000000'
    for pcr in 7 11; do
      val="$(tpm2_pcrread "sha256:$pcr" 2>/dev/null | awk -v p="$pcr:" '$1==p{print tolower($2)}' | tr -d '0x')"
      if [ -z "$val" ]; then
        no "PCR $pcr could not be read"
      elif [ "$val" = "$zero" ]; then
        no "PCR $pcr is all zeroes — nothing measured into it, so sealing to it means nothing"
      else
        ok "PCR $pcr holds a non-zero measurement"
      fi
    done
  else
    no "tpm2_pcrread is missing; PCR contents cannot be checked"
  fi

  # PCR 11 is systemd-stub's UKI measurement. Without it the generation number on
  # the command line is signed but not measured, and the seal is not bound to it.
  if [ -d /sys/firmware/efi/efivars ] && \
     ls /sys/firmware/efi/efivars/StubPcrKernelImage-* >/dev/null 2>&1; then
    ok "systemd-stub reports it measured the kernel image (StubPcrKernelImage)"
  else
    no "no StubPcrKernelImage EFI variable — this was probably not booted as a UKI, so the command line is not measured"
  fi
else
  na "no TPM on this machine; rollback binding is unavailable by design"
fi

# --- the encrypted root and the unlock hand-off ------------------------------
# shellcheck source=/dev/null
if [ -r /usr/libexec/qos-rollback-common ]; then
  . /usr/libexec/qos-rollback-common
  DEV="$(rollback_luks_device)"
else
  DEV=""
  no "/usr/libexec/qos-rollback-common is missing"
fi

if [ -n "$DEV" ]; then
  ok "root is on LUKS at $DEV"
  VOL="$(findmnt -nro SOURCE /sysroot 2>/dev/null || findmnt -nro SOURCE / 2>/dev/null)"
  VOL="$(basename "${VOL%%[*}")"
  info "device-mapper volume: $VOL"

  # The hand-off: early userspace writes the passphrase where systemd-cryptsetup
  # looks for it. It is removed after use, so the evidence is in the journal.
  if journalctl -b --no-pager -o cat 2>/dev/null | grep -q "qos-rollback: generation .* unsealed to /run/cryptsetup-keys.d/"; then
    ok "early userspace unsealed a generation into /run/cryptsetup-keys.d/"
  else
    no "no journal record of qos-rollback unlocking into /run/cryptsetup-keys.d/"
  fi

  # Ordering: our unlock has to finish before systemd-cryptsetup starts, or
  # systemd-cryptsetup falls back to prompting.
  unlock_at="$(journalctl -b --no-pager -o short-unix -u qos-rollback-unlock.service 2>/dev/null | head -1 | cut -d. -f1)"
  crypt_at="$(journalctl -b --no-pager -o short-unix "-u" "systemd-cryptsetup@${VOL}.service" 2>/dev/null | head -1 | cut -d. -f1)"
  if [ -n "$unlock_at" ] && [ -n "$crypt_at" ]; then
    if [ "$unlock_at" -le "$crypt_at" ]; then
      ok "the unlock unit ran before systemd-cryptsetup@$VOL ($unlock_at <= $crypt_at)"
    else
      no "the unlock unit ran AFTER systemd-cryptsetup@$VOL ($unlock_at > $crypt_at)"
    fi
  else
    no "could not establish the ordering between the unlock unit and systemd-cryptsetup@$VOL"
  fi

  # Nothing may be left behind holding a working disk passphrase on a real fs.
  if [ -e /run/cryptsetup-keys.d/"$VOL".key ]; then
    fs="$(stat -f -c %T /run 2>/dev/null)"
    if [ "$fs" = "tmpfs" ] || [ "$fs" = "ramfs" ]; then
      info "the key file is still present on $fs (removed at the end of boot on a normal system)"
    else
      no "/run is $fs, not a tmpfs — a disk passphrase is being written to persistent storage"
    fi
  else
    ok "no leftover key file in /run/cryptsetup-keys.d/"
  fi

  if [ -e /var/lib/qos/rollback/state.json ]; then
    ok "rollback enrollment state is present"
    command -v qos-rollback >/dev/null 2>&1 && qos-rollback status --device "$DEV" | sed 's/^/         /'
  else
    na "no rollback enrollment on this device yet"
  fi

  # An independent unlock path must exist. This is the invariant that matters most.
  if rollback_has_independent_keyslot "$DEV"; then
    ok "the disk has a keyslot outside the TPM-sealed ones"
  else
    no "the disk has NO keyslot outside the TPM-sealed ones — a cleared TPM would mean data loss"
  fi
else
  na "root is not on LUKS; the unlock hand-off does not apply"
fi

# --- the units ---------------------------------------------------------------
for unit in qos-rollback-harden.service qos-rollback-confirm.timer; do
  if systemctl list-unit-files "$unit" >/dev/null 2>&1 && \
     [ "$(systemctl is-enabled "$unit" 2>/dev/null)" = "enabled" ]; then
    ok "$unit is enabled"
  else
    no "$unit is not enabled"
  fi
done

state="$(systemctl show -p Result --value qos-rollback-harden.service 2>/dev/null)"
case "$state" in
  success) ok "qos-rollback-harden ran successfully" ;;
  "")      na "qos-rollback-harden has not run" ;;
  *)       no "qos-rollback-harden result: $state"
           journalctl -b --no-pager -u qos-rollback-harden.service | tail -10 | sed 's/^/         /' ;;
esac

if command -v qos-rollback >/dev/null 2>&1; then
  ok "qos-rollback is installed"
  qos-rollback tpm-status 2>&1 | sed 's/^/         /'
else
  no "qos-rollback is not installed"
fi

printf '\n%s passed, %s failed, %s not applicable\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
