#!/usr/bin/env bash
# Assert the invariants repair mode and boot assessment rest on, from the source
# tree — the ones that would otherwise be discovered on a machine that will not
# boot.
#
#   ./os/tests/repair_mode.sh
#
# Everything here is decidable without a VM. What needs a booted machine
# (does the entry appear, does the counter count down, does the fallback fire) is
# os/tests/recovery-boot-check.sh, which runs in the guest.
#
# Exit codes: 0 all assertions hold; 1 at least one failed.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
OS="$REPO/os"

fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

need_file() {
  if [ -r "$1" ]; then ok "$2"; else bad "$2 — missing $1"; fi
}

# --- the files exist at all ---------------------------------------------------
need_file "$OS/usr/libexec/tilo-deployment"                       "tilo-deployment ships"
need_file "$OS/usr/libexec/tilo-repair"                           "the repair console ships"
need_file "$OS/usr/libexec/tilo-boot-health"                      "the boot health check ships"
need_file "$OS/usr/lib/systemd/system-generators/tilo-boot-mode-generator" "the boot-mode generator ships"
need_file "$OS/usr/lib/systemd/system/tilo-repair.target"         "tilo-repair.target ships"
need_file "$OS/usr/lib/systemd/system/tilo-repair@.service"       "the repair console unit ships"
need_file "$OS/usr/lib/systemd/system/tilo-boot-entries.service"  "the boot-entry unit ships"
need_file "$OS/usr/lib/systemd/system/tilo-boot-health.service"   "the boot-health unit ships"
need_file "$OS/usr/lib/bootc/kargs.d/04-mode.toml"               "the mode karg ships"
need_file "$OS/dev/uki-cmdline.py"                               "the cmdline rewriter ships"
[ "$fail" = 0 ] || exit 1

# --- THE load-bearing one: the two mode values must be the same length --------
# The repair UKI is the installed one with these bytes swapped in place. A
# length-changing edit would move every following PE section and produce an image
# the firmware loads as garbage — uki-cmdline.py refuses it, so the failure would
# be "no repair entry, silently" on every machine. Assert it here, where it is one
# line of arithmetic.
normal="$(sed -n 's/^kargs = \["\(tilo\.mode=[a-z]*\)"\]$/\1/p' "$OS/usr/lib/bootc/kargs.d/04-mode.toml" | head -1)"
# The invocation names the tool through a variable now, so match on the verb and
# the token rather than on the path. If this ever finds nothing the check FAILS
# below rather than quietly asserting about an empty string.
repair="$(sed -n 's/.*set \(tilo\.mode=[a-z]*\).*/\1/p' "$OS/usr/libexec/tilo-deployment" | head -1)"
if [ -z "$normal" ]; then
  bad "could not read the normal-mode karg out of 04-mode.toml — the declaration moved"
elif [ -z "$repair" ]; then
  bad "could not find the tilo-uki-cmdline set invocation in tilo-deployment — the
      derivation moved, and this check can no longer see what it asserts"
elif [ "${#normal}" = "${#repair}" ]; then
  ok "'$normal' and '$repair' are both ${#normal} bytes, so the in-place rewrite fits"
else
  bad "'$normal' (${#normal} bytes) and '$repair' (${#repair} bytes) are different lengths.
      The repair UKI is derived by overwriting one with the other inside the
      sealed .cmdline section; a length change would move every following PE
      section. uki-cmdline.py refuses it, so every machine would silently end up
      with no repair entry."
fi

# The generator has to be looking for the value tilo-deployment writes.
if [ -n "$repair" ] && grep -q "mode=repair\|\"repair\"\|) mode=\|repair)" "$OS/usr/lib/systemd/system-generators/tilo-boot-mode-generator"; then
  gen_val="$(sed -n 's/.*tilo\.mode=\}\"$//p;s/^ *\(repair\)) ;;.*/\1/p' "$OS/usr/lib/systemd/system-generators/tilo-boot-mode-generator" | head -1)"
  if [ "${repair#tilo.mode=}" = "${gen_val:-repair}" ]; then
    ok "the generator matches on the same value tilo-deployment writes (${repair#tilo.mode=})"
  else
    bad "tilo-deployment writes '$repair' but the generator matches '$gen_val'"
  fi
else
  bad "the generator does not appear to match on a repair value at all"
fi

# --- the fallback ladder ------------------------------------------------------
# sd-boot sorts by sort-key ascending, puts entries with no tries left last, and
# takes the first as the default (established on a booted guest). So the repair
# entry's sort key has to be greater than anything bootc emits — bootc uses
# `bootc-<stateroot>-<n>`.
sk="$(sed -n 's/^REPAIR_SORT_KEY="\(.*\)"$/\1/p' "$OS/usr/libexec/tilo-deployment" | head -1)"
if [ -z "$sk" ]; then
  bad "could not read REPAIR_SORT_KEY out of tilo-deployment"
elif [ "$sk" \> "bootc-zzzzzzzz-9" ]; then
  ok "the repair entry's sort key ($sk) sorts after every bootc entry, so a healthy
      deployment always wins the default"
else
  bad "the repair entry's sort key ($sk) does not sort after bootc's ('bootc-...').
      sd-boot would make repair mode the default on a perfectly good machine."
fi

if grep -q 'case "\$id" in bootc\*)' "$OS/usr/libexec/tilo-deployment"; then
  ok "only bootc's own entries get a boot counter"
else
  bad "tilo-deployment no longer restricts boot counters to bootc entries. A counted
      repair entry is a fallback that runs out, which is not one."
fi

# The repair image is a Type #2 entry: a bare .efi at the top of EFI/Linux, no
# .conf. bootc parses every .conf in loader/entries as one of its own and fails
# outright on a foreign one, taking `bootc status` and `bootc upgrade` with it.
if grep -q 'REPAIR_EFI_NAME="tilo-repair.efi"' "$OS/usr/libexec/tilo-deployment" \
   && grep -q 'dest_dir="\$esp/EFI/Linux"' "$OS/usr/libexec/tilo-deployment"; then
  ok "the repair image goes at the top of EFI/Linux, where sd-boot discovers it"
else
  bad "the repair image is no longer installed as a Type #2 entry at the top of
      EFI/Linux. See docs/security/repair-mode.md — a .conf in loader/entries
      breaks bootc, and a .efi one directory deeper is never discovered."
fi
if grep -q 'cat > "\$conf"' "$OS/usr/libexec/tilo-deployment"; then
  bad "tilo-deployment writes a loader entry again. bootc fails to parse a foreign
      one: 'efi does not start with custom prefix'."
else
  ok "and no loader entry is written, which is what keeps bootc able to read its own"
fi
if grep -q 'set-osrel' "$OS/usr/libexec/tilo-deployment"; then
  ok "the image is renamed in its own .osrel, which is where a Type #2 title comes from"
else
  bad "nothing sets the repair image's PRETTY_NAME. A Type #2 entry takes its menu
      title from .osrel, so it would appear under the deployment's own name."
fi
if grep -q 'remove_stale_repair_entry' "$OS/usr/libexec/tilo-deployment"; then
  ok "and an entry left by the earlier shape is cleared away on upgrade"
else
  bad "nothing removes a tilo-repair.conf left by an older version of this script.
      On a machine that has one, every later boot has a broken bootc."
fi
if grep -Fq '[ -f /var/lib/tilo/secureboot/db.key.cred ]' \
      "$OS/usr/libexec/tilo-deployment" \
   && grep -Fq 'systemctl start --wait tilo-secureboot-sign.service' \
      "$OS/usr/libexec/tilo-deployment" \
   && grep -Fq 'tilo-crypto verify-efi /var/lib/tilo/secureboot/db.crt "$dest"' \
      "$OS/usr/libexec/tilo-deployment"; then
  ok "TPM-sealed machines synchronously sign and verify a repair UKI before retaining it"
else
  bad "TPM-sealed machines can publish an unsigned repair UKI and rely on a later path hook"
fi
if grep -Fq 'exec 7<"$repair_lock_dir"' "$OS/usr/libexec/tilo-deployment" \
   && grep -Fq 'exec 6<"$esp"' "$OS/usr/libexec/tilo-deployment" \
   && grep -Fq 'exec 8<"$esp"' "$OS/usr/libexec/tilo-deployment" \
   && grep -Fq 'flock -u 8' "$OS/usr/libexec/tilo-deployment" \
   && grep -Fq 'exec 8<"$ESP"' "$OS/usr/libexec/tilo-secureboot"; then
  ok "repair publication and Secure Boot signing serialize ESP replacement without deadlocking sealed-key activation"
else
  bad "repair publication and Secure Boot signing do not share a stable ESP-inode lock"
fi

# --- the health gate is wired the way systemd expects -------------------------
h="$OS/usr/lib/systemd/system/tilo-boot-health.service"
if grep -q '^Before=boot-complete.target' "$h" && grep -q '^RequiredBy=boot-complete.target' "$h"; then
  ok "the health check is ordered before boot-complete.target and required by it"
else
  bad "tilo-boot-health.service is not Before= + RequiredBy= boot-complete.target.
      systemd-bless-boot.service Requires that target; without both, a failed
      health check would still bless the boot and no rollback would ever happen."
fi
# Blessing renames the booted entry on the ESP, which stays mounted read-only.
# Without a writable view of its own, systemd-bless-boot fails with EROFS and a
# deployment that boots fine keeps spending tries until it is abandoned. The
# remount has to be in the same Exec line as the blessing: each Exec line gets
# a mount namespace of its own.
b="$OS/usr/lib/systemd/system/systemd-bless-boot.service.d/10-tilo-writable-esp.conf"
if [ -r "$b" ] && grep -qx 'PrivateMounts=yes' "$b" && grep -qx 'ExecStart=' "$b" \
   && grep -E '^ExecStart=/bin/sh .*mount -o remount,rw .*exec /usr/lib/systemd/systemd-bless-boot good' "$b" >/dev/null \
   && grep -Fq 'systemd-bless-boot.service.d/10-tilo-writable-esp.conf' "$OS/Containerfile"; then
  ok "systemd-bless-boot remounts the ESP writable in its own namespace, in the blessing's own process, and the image ships it"
else
  bad "systemd-bless-boot has no private writable view of the read-only ESP (or the image
      does not ship $b): blessing a good boot fails with EROFS, so a working
      deployment spends a try on every boot and the boot loader abandons it."
fi
# greetd has Restart=, so it is active between the restarts of a greeter that
# cannot draw. The verdict has to rest on the greeter presenting and holding,
# or an update that breaks the GPU stack is blessed on its first boot and never
# rolled back. os/tests/boot_health_greeter.sh exercises the behaviour.
hs="$OS/usr/libexec/tilo-boot-health"
if grep -Fq 'systemctl show -p NRestarts' "$hs" \
   && grep -Fq '"first-frame-presented"' "$hs" \
   && grep -Fq '/run/tilo-greeter/startup-metrics.json' "$hs" \
   && grep -Fq 'SETTLE=' "$hs" \
   && grep -Fq '/run/tilo-greeter' "$OS/usr/libexec/tilo-cage-session"; then
  ok "the health check requires a greeter that presented a frame and holds without greetd restarting"
else
  bad "tilo-boot-health no longer checks greetd's restarts and the greeter's presented
      frame: a greeter that crash-loops while greetd stays active would bless the boot."
fi
if grep -q 'ConditionKernelCommandLine=!tilo.mode=repair' "$h"; then
  ok "the health check does not run in repair mode"
else
  bad "tilo-boot-health.service would run in repair mode, where no session is
      started — it would condemn the boot that is trying to fix the machine."
fi

# --- repair mode must not start the desktop -----------------------------------
g="$OS/usr/lib/systemd/system-generators/tilo-boot-mode-generator"
for u in greetd.service fs.service fs-identity.path bootc-fetch-apply-updates.timer sshd.service; do
  if grep -q "$u" "$g"; then
    ok "repair mode masks $u"
  else
    bad "repair mode does not mask $u — see docs/security/repair-mode.md for why
        each of these is on the list"
  fi
done
if grep -q 'EARLY' "$g" && grep -q 'default.target' "$g"; then
  ok "the generator writes default.target into the EARLY generator directory"
else
  bad "the generator does not write default.target into \$2 (the early directory).
      /etc/systemd/system/default.target beats the normal directory, so the
      machine would boot the desktop while reporting that it had not."
fi

# --- the repair console gates on a real secret --------------------------------
r="$OS/usr/libexec/tilo-repair"
if grep -q 'cryptsetup open --test-passphrase' "$r"; then
  ok "the repair console proves a LUKS passphrase rather than trusting an open disk"
else
  bad "the repair console no longer runs 'cryptsetup open --test-passphrase'.
      The TPM keyslot is sealed to PCR 7, which repair mode does not change, so
      the disk is already open and 'the disk is open' is not authentication."
fi
for verb in act_rollback act_update act_shell act_logs act_var_rw act_boot_once act_recover_files; do
  if sed -n "/^$verb()/,/^}/p" "$r" | grep -q 'require_auth'; then
    ok "$verb is behind the authentication gate"
  else
    bad "$verb does not call require_auth"
  fi
done

# --- the caveats are printed by every rollback path ---------------------------
d="$OS/usr/libexec/tilo-deployment"
if sed -n '/^cmd_rollback()/,/^}/p' "$d" | grep -q 'print_rollback_caveats'; then
  ok "rollback prints what it does and does not undo before changing anything"
else
  bad "cmd_rollback no longer prints the /var and .tapp caveats. Discovering them
      afterwards is the failure this exists to prevent."
fi
for phrase in '/var does NOT' 'packages do NOT'; do
  if grep -qF "$phrase" "$d"; then
    ok "the caveats still say: $phrase"
  else
    bad "the caveats no longer mention: $phrase"
  fi
done

if sed '/^[[:space:]]*#/d' "$d" | grep -Fq '/proc/cmdline'; then
  bad "tilo-deployment reads /proc/cmdline even though boot-entry maintenance runs with ProcSubset=pid"
elif grep -Fq 'cmdline="$($UKI_CMDLINE show "$esp/${uki_rel#/}"' "$d"; then
  ok "running generation comes from the bootloader-selected signed UKI inside the service sandbox"
else
  bad "running generation is not bound to the bootloader-selected signed UKI"
fi

# --- shell syntax -------------------------------------------------------------
for s in "$OS/usr/libexec/tilo-deployment" "$OS/usr/libexec/tilo-repair" \
         "$OS/usr/libexec/tilo-boot-health" "$g"; do
  if bash -n "$s" 2>/dev/null; then ok "$(basename "$s") parses"
  else bad "$(basename "$s") is not valid bash"; bash -n "$s"; fi
done

exit "$fail"
