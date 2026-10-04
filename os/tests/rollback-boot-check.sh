#!/usr/bin/bash
# Boot-side checks for bounded rollback. Run INSIDE a booted Tilo guest.
#
#   ./os/dev/vm.sh boot --tpm          # then, in the guest:
#   sudo /usr/libexec/tilo-rollback-boot-check
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
GEN="$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^tilo\.generation=\([0-9][0-9]*\)$/\1/p' | head -1)"
if [ -n "$GEN" ]; then
  ok "tilo.generation=$GEN is on the kernel command line"
else
  no "no tilo.generation= on the kernel command line — rollback protection is not bound to anything"
fi

# It has to come from the UKI, not from a bootloader config. The strongest
# evidence available inside the guest is that the stub measured a command line and
# that the number is in the image too.
if [ -r /usr/lib/tilo/generation ]; then
  FILE_GEN="$(cat /usr/lib/tilo/generation)"
  if [ "$FILE_GEN" = "$GEN" ]; then
    ok "/usr/lib/tilo/generation agrees with the command line ($FILE_GEN)"
  else
    no "/usr/lib/tilo/generation says $FILE_GEN but the command line says ${GEN:-<absent>}"
  fi
else
  no "/usr/lib/tilo/generation is missing — a staged deployment's generation cannot be read"
fi

if [ -d /sys/firmware/efi ]; then
  ok "booted via UEFI"
else
  na "not a UEFI boot; measured boot does not apply"
fi

# --- the TPM and its measurements --------------------------------------------
if [ -e /dev/tpmrm0 ] || [ -e /dev/tpm0 ]; then
  ok "a TPM is present ($( [ -e /dev/tpmrm0 ] && echo /dev/tpmrm0 || echo /dev/tpm0 ))"

  # The firmware's own event log. Without it the firmware never exposed a working
  # TCG2 protocol, and then every PCR below reads zero no matter what the kernel
  # did — so this is checked first and named, rather than letting a firmware
  # problem surface as "the UKI did not measure".
  evlog=/sys/kernel/security/tpm0/binary_bios_measurements
  if [ -r "$evlog" ] && [ "$(head -c 1 "$evlog" 2>/dev/null | wc -c)" -gt 0 ]; then
    ok "the firmware produced a TPM event log ($evlog)"
  else
    no "no firmware TPM event log at $evlog — the firmware exposed no working TCG2,
         so nothing was measured and the PCR values below cannot mean anything.
         On a VM this is the 2 MB OVMF build (no TPM support) or a software TPM
         that was never sent TPM2_Startup; see os/dev/_common.sh."
  fi

  if command -v tpm2_pcrread >/dev/null 2>&1; then
    for pcr in 7 11; do
      # tpm2_pcrread pads the index column, so a single-digit PCR prints as
      # "  7 : 0x…" while a two-digit one prints as " 11: 0x…". Matching a field
      # against "$pcr:" therefore misses every single-digit PCR and reports a
      # perfectly good measurement as unreadable. Match the punctuation instead of
      # assuming the layout, and keep the digits: deleting the "0x" prefix with
      # `tr -d '0x'` deletes every 0 and every x in the value too, which turns an
      # all-zero PCR into an empty string — i.e. reports "could not be read" for
      # the one state this check exists to catch.
      val="$(tpm2_pcrread "sha256:$pcr" 2>/dev/null \
             | sed -n "s/^[[:space:]]*$pcr[[:space:]]*:[[:space:]]*0[xX]\([0-9a-fA-F]\{16,\}\).*/\1/p" \
             | head -1 | tr 'A-F' 'a-f')"
      if [ -z "$val" ]; then
        no "PCR $pcr could not be read (no sha256:$pcr value in tpm2_pcrread output)"
      elif [ -z "${val//0/}" ]; then
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

  # systemd's own verdict on the same question, which is the one the units use.
  # Asking it directly beats inferring it from the variables above: this is the
  # condition a unit gated on a measured boot will be evaluated against.
  if command -v systemd-analyze >/dev/null 2>&1; then
    if systemd-analyze condition 'ConditionSecurity=measured-uki' >/dev/null 2>&1; then
      ok "systemd agrees this is a measured UKI (ConditionSecurity=measured-uki)"
    else
      no "systemd says ConditionSecurity=measured-uki is unmet — anything gated on a
         measured boot will be skipped, and a seal to PCR 11 is not bound to this
         command line"
    fi
  else
    no "systemd-analyze is missing; systemd's own measured-UKI verdict cannot be read"
  fi
else
  na "no TPM on this machine; rollback binding is unavailable by design"
fi

# --- the encrypted root and the unlock hand-off ------------------------------
# shellcheck source=/dev/null
if [ -r /usr/libexec/tilo-rollback-common ]; then
  . /usr/libexec/tilo-rollback-common
  DEV="$(rollback_luks_device)"
else
  DEV=""
  no "/usr/libexec/tilo-rollback-common is missing"
fi

if [ -n "$DEV" ]; then
  ok "root is on LUKS at $DEV"
  VOL="$(findmnt -nro SOURCE /sysroot 2>/dev/null || findmnt -nro SOURCE / 2>/dev/null)"
  VOL="$(basename "${VOL%%[*}")"
  info "device-mapper volume: $VOL"

  # The hand-off: early userspace writes the passphrase where systemd-cryptsetup
  # looks for it. It is removed after use, so the evidence is in the journal.
  #
  # grep -c, not grep -q. This script runs under `set -o pipefail`, and a boot
  # journal is far larger than the 64 KB pipe buffer: `grep -q` exits at the first
  # match, journalctl dies of SIGPIPE, and pipefail makes the pipeline fail — so
  # finding the record would be reported as not finding it. -c consumes all input.
  unsealed="$(journalctl -b --no-pager -o cat 2>/dev/null \
              | grep -c "tilo-rollback: generation .* unsealed to /run/cryptsetup-keys.d/" || true)"
  if [ "${unsealed:-0}" -gt 0 ]; then
    ok "early userspace unsealed a generation into /run/cryptsetup-keys.d/"
  else
    no "no journal record of tilo-rollback unlocking into /run/cryptsetup-keys.d/"
  fi

  # Ordering: our unlock has to finish before systemd-cryptsetup starts, or
  # systemd-cryptsetup falls back to prompting.
  unlock_at="$(journalctl -b --no-pager -o short-unix -u tilo-rollback-unlock.service 2>/dev/null | head -1 | cut -d. -f1)"
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

  if [ -e /var/lib/tilo/rollback/state.json ]; then
    ok "rollback enrollment state is present"
    command -v tilo-rollback >/dev/null 2>&1 && tilo-rollback status --device "$DEV" | sed 's/^/         /'
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
for unit in tilo-rollback-harden.timer tilo-rollback-confirm.timer; do
  if systemctl list-unit-files "$unit" >/dev/null 2>&1 && \
     [ "$(systemctl is-enabled "$unit" 2>/dev/null)" = "enabled" ]; then
    ok "$unit is enabled"
  else
    no "$unit is not enabled"
  fi
done

# systemd answers Result=success for a unit it has never heard of, so a missing
# unit would otherwise be reported as one that ran and succeeded. Ask whether it
# exists first, and treat "not loaded" as its own answer.
if ! systemctl cat tilo-rollback-harden.service >/dev/null 2>&1; then
  no "tilo-rollback-harden.service does not exist in this image"
else
  state="$(systemctl show -p Result --value tilo-rollback-harden.service 2>/dev/null)"
  active="$(systemctl show -p ActiveState --value tilo-rollback-harden.service 2>/dev/null)"
  case "$state:$active" in
    success:active|success:activating) ok "tilo-rollback-harden ran successfully" ;;
    success:inactive)
      # Never started this boot. Its conditions may have skipped it, which is a
      # legitimate outcome; "it ran and worked" is not what that means.
      na "tilo-rollback-harden has not run on this boot" ;;
    *) no "tilo-rollback-harden result: $state (state: $active)"
       journalctl -b --no-pager -u tilo-rollback-harden.service | tail -10 | sed 's/^/         /' ;;
  esac
fi

# The confirmation's timer fires ten minutes into the boot. Before that it has
# not run, which says nothing; after it, anything but success means the floor
# never moves on this machine.
if ! systemctl cat tilo-rollback-confirm.service >/dev/null 2>&1; then
  no "tilo-rollback-confirm.service does not exist in this image"
else
  state="$(systemctl show -p Result --value tilo-rollback-confirm.service 2>/dev/null)"
  started="$(systemctl show -p ExecMainStartTimestampMonotonic --value tilo-rollback-confirm.service 2>/dev/null)"
  if [ -z "$started" ] || [ "$started" = 0 ]; then
    if [ "$state" = success ]; then
      na "tilo-rollback-confirm has not run on this boot yet (its timer fires ten minutes in)"
    else
      no "tilo-rollback-confirm could not start: $state"
      journalctl -b --no-pager -u tilo-rollback-confirm.service | tail -10 | sed 's/^/         /'
    fi
  elif [ "$state" = success ]; then
    ok "tilo-rollback-confirm ran successfully"
    journalctl -b --no-pager -o cat -u tilo-rollback-confirm.service | grep -F 'tilo-rollback:' \
      | tail -3 | sed 's/^/         /'
  else
    no "tilo-rollback-confirm result: $state"
    journalctl -b --no-pager -u tilo-rollback-confirm.service | tail -10 | sed 's/^/         /'
  fi
fi

if command -v tilo-rollback >/dev/null 2>&1; then
  ok "tilo-rollback is installed"
  tilo-rollback tpm-status 2>&1 | sed 's/^/         /'
else
  no "tilo-rollback is not installed"
fi

printf '\n%s passed, %s failed, %s not applicable\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
