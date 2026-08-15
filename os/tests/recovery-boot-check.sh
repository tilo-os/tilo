#!/usr/bin/bash
# Boot-side evidence for the recovery story. Run IN a booted guest, as root.
#
#   sudo /usr/libexec/qos-recovery-boot-check
#
# The host cannot establish any of this. Whether the repair entry is really on the
# ESP, whether its UKI really carries `qos.mode=repair` in its sealed command
# line, whether sd-boot really offers it, and whether the try counters really
# landed are all facts about an installed machine.
#
# ## A check that cannot look must FAIL
#
# Every assertion below distinguishes "I looked and it was wrong" from "I could
# not look". The second is a failure here, not a skip. This project has been
# caught three times by a verification script that passed because it lacked the
# privilege or the log source to see its subject, and a recovery mechanism that
# is reported working because nothing checked it is worse than one that is
# reported missing.
set -uo pipefail

fail=0
ok()   { printf 'ok    %s\n' "$*"; }
bad()  { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
info() { printf '      %s\n' "$*"; }

[ "$(id -u)" = 0 ] || { printf 'FAIL  needs root: the ESP is mode 0700 and EFI variables are root-only.\n' >&2; exit 1; }

MODE="$(tr ' \t' '\n\n' < /proc/cmdline | sed -n 's/^qos\.mode=//p' | head -1)"
GEN="$(tr ' \t' '\n\n' < /proc/cmdline | sed -n 's/^qos\.generation=//p' | head -1)"
CFS="$(tr ' \t' '\n\n' < /proc/cmdline | sed -n 's/^composefs=//p' | head -1)"

printf '== qOS recovery, boot-side ==\n'
info "booted mode=${MODE:-<none>} generation=${GEN:-<none>} composefs=${CFS:0:16}"

# --- can this check observe anything at all? ---------------------------------
command -v bootctl >/dev/null 2>&1 \
  || bad "bootctl is not installed, so nothing here can read what the boot loader
      will do. Reporting that as 'no problems found' is the failure this header is
      about."
ESP=""
if [ -d /boot/loader/entries ]; then ESP=/boot
elif [ -d /boot/efi/loader/entries ]; then ESP=/boot/efi
fi
[ -n "$ESP" ] && ok "found the systemd-boot layout at $ESP" \
               || bad "no loader/entries under /boot or /boot/efi — either this machine does
      not boot from loader entries, or the ESP is not mounted. Either way this
      check cannot see its subject."
[ "$fail" = 0 ] || exit 1

if [ -e /sys/firmware/efi/efivars/LoaderEntrySelected-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f ]; then
  ok "the boot loader reported which entry it booted (LoaderEntrySelected is set)"
else
  bad "no LoaderEntrySelected EFI variable. Either systemd-boot did not boot this
      machine, or efivarfs is not mounted — and without it nothing below can tell
      the running entry from any other."
fi
CUR="$(bootctl 2>/dev/null | sed -n 's/^ *Current Entry: *//p' | head -1)"
[ -n "$CUR" ] && ok "current entry: $CUR" || bad "bootctl could not name the current entry"

# --- the repair entry ---------------------------------------------------------
CONF="$ESP/loader/entries/qos-repair.conf"
if [ -r "$CONF" ]; then
  ok "the repair loader entry is on the ESP"
else
  bad "no $CONF. qos-boot-entries.service is what makes it; its journal says why
      it did not:  journalctl -u qos-boot-entries"
fi

REPAIR_UKI=""
if [ -r "$CONF" ]; then
  rel="$(sed -n 's/^uki *//p' "$CONF" | head -1)"
  REPAIR_UKI="$ESP/${rel#/}"
  if [ -f "$REPAIR_UKI" ]; then
    ok "it names a UKI that exists: $rel ($(stat -c %s "$REPAIR_UKI") bytes)"
  else
    bad "the repair entry names $rel, which is not on the ESP. sd-boot would offer
        a menu entry that cannot boot."
  fi
  sk="$(sed -n 's/^sort-key *//p' "$CONF" | head -1)"
  if [ -n "$sk" ] && [ "$sk" \> "bootc-zzzzzzzz-9" ]; then
    ok "its sort key ($sk) puts it below every deployment entry"
  else
    bad "the repair entry's sort key is '${sk:-<none>}', which does not sort after
        bootc's. sd-boot would offer repair mode ahead of a working system."
  fi
  case "$(basename "$CONF")" in
    *+*) bad "the repair entry carries a boot counter — a fallback that expires" ;;
    *)   ok "it carries no boot counter, so it never expires" ;;
  esac
fi

# --- the sealed command line of the repair UKI, read out of the PE ------------
# This is the assertion the whole design rests on, and it is read from the bytes
# rather than inferred from the fact that a file was written.
if [ -n "$REPAIR_UKI" ] && [ -f "$REPAIR_UKI" ]; then
  if ! command -v /usr/libexec/qos-uki-cmdline >/dev/null 2>&1; then
    bad "qos-uki-cmdline is not on this machine, so the repair UKI's sealed command
        line cannot be read and nothing below it can be established."
  else
    rc="$(/usr/libexec/qos-uki-cmdline show "$REPAIR_UKI" 2>/dev/null)"
    if [ -z "$rc" ]; then
      bad "could not read a .cmdline section out of $REPAIR_UKI"
    else
      case " $rc " in
        *" qos.mode=repair "*) ok "the repair UKI's SEALED command line carries qos.mode=repair" ;;
        *) bad "the repair UKI does not carry qos.mode=repair. It would boot the
        desktop from a menu entry that says Repair Mode.
        Its command line: $rc" ;;
      esac
      # Derived, not built: it has to name the same rootfs and the same generation
      # as the deployment it came from, or it is a different machine.
      rcfs="$(printf '%s' "$rc" | tr ' ' '\n' | sed -n 's/^composefs=//p' | head -1)"
      if [ -n "$CFS" ] && [ "$rcfs" = "$CFS" ]; then
        ok "it seals the same composefs digest as the running deployment"
      else
        bad "the repair UKI seals composefs=${rcfs:0:16}… but this deployment is
        ${CFS:0:16}… — it would mount a different rootfs, or none."
      fi
      rgen="$(printf '%s' "$rc" | tr ' ' '\n' | sed -n 's/^qos\.generation=//p' | head -1)"
      if [ "$rgen" = "$GEN" ]; then
        ok "and the same generation ($rgen)"
      else
        bad "the repair UKI claims generation '${rgen:-<none>}', this deployment is '${GEN:-<none>}'"
      fi
      case " $rc " in
        *" qos.mode=normal "*) bad "it carries BOTH modes — the in-place rewrite hit the wrong token" ;;
      esac
    fi
  fi
fi

# --- sd-boot actually offers it ----------------------------------------------
if bootctl list 2>/dev/null | grep -q 'id: *qos-repair.conf'; then
  ok "sd-boot lists the repair entry"
else
  bad "bootctl list does not show qos-repair.conf. The file may be there and
      unparseable — bootctl list output was:"
  bootctl list 2>&1 | sed 's/^/      /' >&2
fi

# The default has to be a deployment, not repair mode, on a healthy machine.
# Parsed as blocks: `bootctl` only appends the entry id to a title when two
# would otherwise read the same, so a single regex over the title line finds
# nothing on a machine whose titles are distinct — and reports "no default",
# which is not the same statement at all.
DEFAULT="$(bootctl list 2>/dev/null | awk '
  /^[[:space:]]*title:/ { want = ($0 ~ /\(default\)/) }
  /^[[:space:]]*id:/    { if (want) { sub(/^[[:space:]]*id:[[:space:]]*/, ""); print; exit } }
')"
if [ -z "$DEFAULT" ]; then
  bad "bootctl list named no default entry, so what the next boot would pick is
      unknown and the fallback order cannot be checked."
elif [ "$DEFAULT" = "qos-repair.conf" ]; then
  if bootctl list 2>/dev/null | grep -q 'id: *bootc'; then
    bad "repair mode is the DEFAULT entry while deployment entries exist. Every
        boot would come up in repair mode."
  else
    ok "repair mode is the default, and it is the only entry there is"
  fi
else
  ok "the default entry is a deployment ($DEFAULT), not repair mode"
fi

# --- boot counters ------------------------------------------------------------
counted=0; uncounted=0; total=0
for f in "$ESP"/loader/entries/bootc*.conf; do
  [ -e "$f" ] || continue
  total=$((total+1))
  case "${f##*/}" in *+*) counted=$((counted+1)) ;; *) uncounted=$((uncounted+1)) ;; esac
done
if [ "$total" = 0 ]; then
  bad "no bootc loader entries at all — this machine was not installed by bootc,
      so the boot-assessment half of this check has no subject."
elif [ "$total" = 1 ]; then
  ok "one deployment, and it is the one booted — nothing to count down yet"
  info "(a counter is added when a second deployment is staged; the fallback"
  info " ladder has nowhere to fall until then)"
else
  if [ "$counted" -ge 1 ]; then
    ok "$counted of $total deployment entries carry a try counter"
    for f in "$ESP"/loader/entries/bootc*+*.conf; do
      [ -e "$f" ] || continue
      info "$(basename "$f")"
    done
  else
    bad "$total deployment entries and not one has a try counter. A boot that
        fails would be retried for ever. qos-boot-entries.service is what adds
        them: journalctl -u qos-boot-entries"
  fi
fi

# --- the health gate ----------------------------------------------------------
if systemctl is-enabled qos-boot-health.service >/dev/null 2>&1; then
  ok "qos-boot-health.service is enabled"
else
  bad "qos-boot-health.service is not enabled, so nothing decides whether a boot
      succeeded and systemd-bless-boot would bless every boot unconditionally."
fi
if systemctl show -p Before --value boot-complete.target 2>/dev/null | grep -q systemd-bless-boot \
   || systemctl show -p Requires --value systemd-bless-boot.service 2>/dev/null | grep -q boot-complete; then
  ok "systemd-bless-boot.service requires boot-complete.target"
else
  bad "systemd-bless-boot.service does not require boot-complete.target on this
      machine, so a failed health check would not stop a boot being blessed."
fi
if [ "$MODE" != repair ]; then
  wants="$(systemctl show -p RequiredBy --value qos-boot-health.service 2>/dev/null)"
  case "$wants" in
    *boot-complete.target*) ok "and the health check is required by it" ;;
    *) bad "qos-boot-health.service is not RequiredBy boot-complete.target
        (RequiredBy=$wants). Its verdict would be ignored." ;;
  esac
fi

# --- repair mode, when that is what booted -----------------------------------
if [ "$MODE" = repair ]; then
  printf '\n-- booted IN repair mode --\n'
  d="$(systemctl get-default 2>/dev/null)"
  if [ "$d" = "qos-repair.target" ]; then
    ok "the generator redirected default.target to qos-repair.target"
  else
    bad "systemctl get-default says '$d'. The generator did not take, and this boot
        is not the mode it claims to be."
  fi
  for u in greetd.service fs.service bootc-fetch-apply-updates.timer sshd.service; do
    systemctl cat "$u" >/dev/null 2>&1 || { info "$u is not installed here"; continue; }
    if [ "$(systemctl is-enabled "$u" 2>/dev/null)" = masked ]; then
      ok "$u is masked"
    else
      bad "$u is NOT masked in repair mode (is-enabled: $(systemctl is-enabled "$u" 2>/dev/null))"
    fi
  done
  if systemctl is-active --quiet 'qos-repair@ttyS0.service' \
     || systemctl is-active --quiet 'qos-repair@tty1.service'; then
    ok "a repair console is running"
  else
    bad "no repair console is running on tty1 or ttyS0"
    systemctl status --no-pager 'qos-repair@*' 2>&1 | sed 's/^/      /' >&2
  fi
else
  if [ "$(systemctl get-default 2>/dev/null)" = "qos-repair.target" ]; then
    bad "this is a normal boot but default.target is qos-repair.target — the
        generator fired without qos.mode=repair on the command line."
  else
    ok "a normal boot is not diverted to the repair target"
  fi
fi

printf '\n'
[ "$fail" = 0 ] && printf 'ALL CHECKS PASSED\n' || printf 'SOME CHECKS FAILED\n' >&2
exit "$fail"
