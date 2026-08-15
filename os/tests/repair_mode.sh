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
need_file "$OS/usr/libexec/qos-deployment"                       "qos-deployment ships"
need_file "$OS/usr/libexec/qos-repair"                           "the repair console ships"
need_file "$OS/usr/libexec/qos-boot-health"                      "the boot health check ships"
need_file "$OS/usr/lib/systemd/system-generators/qos-boot-mode-generator" "the boot-mode generator ships"
need_file "$OS/usr/lib/systemd/system/qos-repair.target"         "qos-repair.target ships"
need_file "$OS/usr/lib/systemd/system/qos-repair@.service"       "the repair console unit ships"
need_file "$OS/usr/lib/systemd/system/qos-boot-entries.service"  "the boot-entry unit ships"
need_file "$OS/usr/lib/systemd/system/qos-boot-health.service"   "the boot-health unit ships"
need_file "$OS/usr/lib/bootc/kargs.d/04-mode.toml"               "the mode karg ships"
need_file "$OS/dev/uki-cmdline.py"                               "the cmdline rewriter ships"
[ "$fail" = 0 ] || exit 1

# --- THE load-bearing one: the two mode values must be the same length --------
# The repair UKI is the installed one with these bytes swapped in place. A
# length-changing edit would move every following PE section and produce an image
# the firmware loads as garbage — uki-cmdline.py refuses it, so the failure would
# be "no repair entry, silently" on every machine. Assert it here, where it is one
# line of arithmetic.
normal="$(sed -n 's/^kargs = \["\(qos\.mode=[a-z]*\)"\]$/\1/p' "$OS/usr/lib/bootc/kargs.d/04-mode.toml" | head -1)"
repair="$(sed -n "s/.*qos-uki-cmdline set \(qos\.mode=[a-z]*\).*/\1/p" "$OS/usr/libexec/qos-deployment" | head -1)"
if [ -z "$normal" ]; then
  bad "could not read the normal-mode karg out of 04-mode.toml — the declaration moved"
elif [ -z "$repair" ]; then
  bad "could not find the qos-uki-cmdline set invocation in qos-deployment — the
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

# The generator has to be looking for the value qos-deployment writes.
if [ -n "$repair" ] && grep -q "mode=repair\|\"repair\"\|) mode=\|repair)" "$OS/usr/lib/systemd/system-generators/qos-boot-mode-generator"; then
  gen_val="$(sed -n 's/.*qos\.mode=\}\"$//p;s/^ *\(repair\)) ;;.*/\1/p' "$OS/usr/lib/systemd/system-generators/qos-boot-mode-generator" | head -1)"
  if [ "${repair#qos.mode=}" = "${gen_val:-repair}" ]; then
    ok "the generator matches on the same value qos-deployment writes (${repair#qos.mode=})"
  else
    bad "qos-deployment writes '$repair' but the generator matches '$gen_val'"
  fi
else
  bad "the generator does not appear to match on a repair value at all"
fi

# --- the fallback ladder ------------------------------------------------------
# sd-boot sorts by sort-key ascending, puts entries with no tries left last, and
# takes the first as the default (established on a booted guest). So the repair
# entry's sort key has to be greater than anything bootc emits — bootc uses
# `bootc-<stateroot>-<n>`.
sk="$(sed -n 's/^REPAIR_SORT_KEY="\(.*\)"$/\1/p' "$OS/usr/libexec/qos-deployment" | head -1)"
if [ -z "$sk" ]; then
  bad "could not read REPAIR_SORT_KEY out of qos-deployment"
elif [ "$sk" \> "bootc-zzzzzzzz-9" ]; then
  ok "the repair entry's sort key ($sk) sorts after every bootc entry, so a healthy
      deployment always wins the default"
else
  bad "the repair entry's sort key ($sk) does not sort after bootc's ('bootc-...').
      sd-boot would make repair mode the default on a perfectly good machine."
fi

if grep -q 'REPAIR_ENTRY_ID' "$OS/usr/libexec/qos-deployment" \
   && grep -q 'case "\$id" in bootc\*)' "$OS/usr/libexec/qos-deployment"; then
  ok "only bootc's own entries get a boot counter — the repair entry never expires"
else
  bad "qos-deployment no longer restricts boot counters to bootc entries. A counted
      repair entry is a fallback that runs out, which is not one."
fi

# --- the health gate is wired the way systemd expects -------------------------
h="$OS/usr/lib/systemd/system/qos-boot-health.service"
if grep -q '^Before=boot-complete.target' "$h" && grep -q '^RequiredBy=boot-complete.target' "$h"; then
  ok "the health check is ordered before boot-complete.target and required by it"
else
  bad "qos-boot-health.service is not Before= + RequiredBy= boot-complete.target.
      systemd-bless-boot.service Requires that target; without both, a failed
      health check would still bless the boot and no rollback would ever happen."
fi
if grep -q 'ConditionKernelCommandLine=!qos.mode=repair' "$h"; then
  ok "the health check does not run in repair mode"
else
  bad "qos-boot-health.service would run in repair mode, where no session is
      started — it would condemn the boot that is trying to fix the machine."
fi

# --- repair mode must not start the desktop -----------------------------------
g="$OS/usr/lib/systemd/system-generators/qos-boot-mode-generator"
for u in greetd.service fs.service bootc-fetch-apply-updates.timer sshd.service; do
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
r="$OS/usr/libexec/qos-repair"
if grep -q 'cryptsetup open --test-passphrase' "$r"; then
  ok "the repair console proves a LUKS passphrase rather than trusting an open disk"
else
  bad "the repair console no longer runs 'cryptsetup open --test-passphrase'.
      The TPM keyslot is sealed to PCR 7, which repair mode does not change, so
      the disk is already open and 'the disk is open' is not authentication."
fi
for verb in act_rollback act_update act_shell act_logs act_var_rw act_boot_once; do
  if sed -n "/^$verb()/,/^}/p" "$r" | grep -q 'require_auth'; then
    ok "$verb is behind the authentication gate"
  else
    bad "$verb does not call require_auth"
  fi
done

# --- the caveats are printed by every rollback path ---------------------------
d="$OS/usr/libexec/qos-deployment"
if sed -n '/^cmd_rollback()/,/^}/p' "$d" | grep -q 'print_rollback_caveats'; then
  ok "rollback prints what it does and does not undo before changing anything"
else
  bad "cmd_rollback no longer prints the /var and .qpkg caveats. Discovering them
      afterwards is the failure this exists to prevent."
fi
for phrase in '/var does NOT' 'packages do NOT'; do
  if grep -qF "$phrase" "$d"; then
    ok "the caveats still say: $phrase"
  else
    bad "the caveats no longer mention: $phrase"
  fi
done

# --- shell syntax -------------------------------------------------------------
for s in "$OS/usr/libexec/qos-deployment" "$OS/usr/libexec/qos-repair" \
         "$OS/usr/libexec/qos-boot-health" "$g"; do
  if bash -n "$s" 2>/dev/null; then ok "$(basename "$s") parses"
  else bad "$(basename "$s") is not valid bash"; bash -n "$s"; fi
done

exit "$fail"
