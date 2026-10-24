#!/usr/bin/env bash
# A broken update rolls itself back, on the installed image, by booting it.
#
#   RUNNER='./os/dev/vm.sh ssh' SUDO_PASS=tilo \
#   RESET='./os/dev/vm.sh stop; ./os/dev/vm.sh boot --2d --disk "$TILO_DISK"' \
#   SERIAL='./os/dev/vm.sh serial' \
#   FAULT=greetd|greeter|unlock \
#     ./os/tests/boot_assessment_rollback.sh STAGE_IMAGE [OUT_DIR]
#
# RUNNER runs one shell command line in the booted guest (`vm.sh ssh`, an ssh
# wrapper, `tilo-vm vm ssh NAME --`). With SUDO_PASS the commands that need root
# go through `sudo -S`; without it RUNNER must already be root. RESET, if set,
# power-cycles a guest that stopped answering, since a broken boot may never
# bring its network up; without it such a boot fails the check. UNLOCK, if set,
# runs on the host every few seconds while a rebooted guest does not answer yet:
# an installed system's encrypted root waits for its passphrase at every boot
# (UNLOCK types it once the prompt is on screen, e.g. with `vm.sh type -`).
# SERIAL, if set, prints the guest's console log; each boot that never answers
# keeps it as evidence of where it stopped.
#
# STAGE_IMAGE is what `bootc switch` stages: any bootable image other than the
# booted one that this machine's policy accepts (an immutable tilo-vm digest on
# a dev disk, a signed release on a sealed one). Prefix `containers-storage:` to
# stage an image from root's local container storage.
#
# What is proved, on the production units and the production boot loader:
#
#   1. The booted deployment's own boot-entry maintenance succeeds
#      (tilo-boot-entries.service). A dev disk boots split kernel/initrd
#      entries, which this pass once refused outright.
#   2. Staging an update and running the same maintenance, as every update path
#      does, gives the staged deployment's entry, and only it, a try counter
#      BEFORE the reboot into it.
#   3. The staged deployment is made unable to reach the greeter, by a greetd
#      drop-in in /etc that acts only on the staged deployment's composefs
#      identity. /etc is carried into the staged deployment when it is
#      finalized, so the image itself is untouched and the fault is bounded to
#      that one deployment. FAULT picks how it breaks:
#        greetd   (default) greetd itself fails to start.
#        greeter  greetd starts and stays in service, but its greeter is the
#                 real one cut off 12 s after it starts, so the screen comes
#                 and goes while systemd restarts greetd every few seconds: the
#                 shape of an update that breaks the GPU stack, cage or the
#                 greeter, and the one a check of `greetd is active` blesses.
#        unlock   the staged deployment's boot stops in the initrd at a disk
#                 unlock prompt that nobody answers, and the machine is
#                 power-cycled there (RESET): the shape of an update whose
#                 keyboard is dead at the unlock prompt. On a kernel/initrd
#                 entry (a dev disk) the prompt is added to that entry's
#                 command line, a plain dm-crypt mapping of the root
#                 filesystem's device that is never opened; on a sealed UKI entry the
#                 fault is UNLOCK not being run for the staged deployment's
#                 boots, so its real encrypted root waits.
#      Each of its boots spends a try: tilo-boot-health.service fails (or, at
#      the unlock prompt, nothing runs at all), systemd-bless-boot leaves the
#      counter, and the boot loader decrements it. While a broken boot is up,
#      its own boot-entry pass puts the previous deployment on trial as well.
#   4. After exactly as many failed boots as it had tries, the boot loader
#      starts the previous deployment by itself. The used-up entry stays on the
#      ESP, and the deployment that came back reaches its greeter, passes its
#      health check and is blessed (systemd-bless-boot renames its entry on the read-only ESP).
#   5. On an image with tilo-boot-fallback.service, the deployment that came
#      back is bootc's default again (no rollbackQueued) and a report of the
#      failed deployment, with its spent tries, is under /var/lib/tilo/boot-fallback.
#
# Entries are matched to deployments by the composefs identity they boot, never
# by file name: bootc renumbers every entry when it stages one.
#
# Every boot leaves its record in OUT_DIR (entries, health journal, timings).
# A step that cannot observe its subject fails; nothing here is skipped.
set -uo pipefail

STAGE_IMAGE="${1:-}"
OUT="${2:-${TILO_RUN:-/tmp}/boot-assessment}"
RUNNER="${RUNNER:-}"
SUDO_PASS="${SUDO_PASS:-}"
RESET="${RESET:-}"
UNLOCK="${UNLOCK:-}"
SERIAL="${SERIAL:-}"
# How long a staged boot is left at its unlock prompt before the power-cycle.
HANG_TIMEOUT="${HANG_TIMEOUT:-150}"
BOOT_TIMEOUT="${BOOT_TIMEOUT:-420}"
# Above tilo-boot-health's own wait for a greeter (180 s, then a 20 s hold), so
# the verdict on a broken boot is the health check's and not this timeout's.
HEALTH_TIMEOUT="${HEALTH_TIMEOUT:-360}"
SWITCH_TIMEOUT="${SWITCH_TIMEOUT:-2400}"
DROPIN_DIR=/etc/systemd/system/greetd.service.d
DROPIN="$DROPIN_DIR/90-boot-assessment-fault.conf"
FAULT="${FAULT:-greetd}"
FAULT_CONFIG=/etc/greetd/boot-assessment-fault.toml

[ -n "$RUNNER" ] || { echo "set RUNNER to a command that runs a shell line in the guest" >&2; exit 2; }
[ -n "$STAGE_IMAGE" ] || { echo "usage: $0 STAGE_IMAGE [OUT_DIR]" >&2; exit 2; }
case "$FAULT" in greetd|greeter|unlock) ;; *) echo "FAULT must be greetd, greeter or unlock, not '$FAULT'" >&2; exit 2 ;; esac
[ "$FAULT" != unlock ] || [ -n "$RESET" ] || { echo "FAULT=unlock power-cycles boots that wait at their unlock prompt: set RESET" >&2; exit 2; }
mkdir -p "$OUT"

step() { printf '\n=== %s\n' "$*" >&2; }
ok()   { printf 'ok    %s\n' "$*" >&2; }
fail() { printf 'FAIL  %s\n' "$*" >&2; printf 'FAIL %s\n' "$*" > "$OUT/verdict.txt"; exit 1; }

guest() { $RUNNER "$1"; }
# The command line is quoted once for the guest's login shell.
as_root() {
  local line
  line="bash -c $(printf '%q' "$1")"
  if [ -n "$SUDO_PASS" ]; then
    guest "printf '%s\\n' $(printf '%q' "$SUDO_PASS") | sudo -S -p '' $line"
  else
    guest "$line"
  fi
}

boot_id() { guest 'cat /proc/sys/kernel/random/boot_id' 2>/dev/null | tr -d '\r\n'; }
booted_digest() {
  guest 'tr " " "\n" < /proc/cmdline | sed -n "s/^composefs=//p"' 2>/dev/null | tr -d '\r\n'
}
# The booted entry as the boot loader reported it, counter stripped.
current_entry() {
  as_root 'bootctl status --no-pager 2>/dev/null | sed -n "s/^ *Current Entry: *//p" | head -1' \
    | tr -d '\r\n' | sed -E 's/\+[0-9]+(-[0-9]+)?\.conf$/.conf/'
}
id_of() { sed -E 's/\+[0-9]+(-[0-9]+)?\.conf$/.conf/' <<<"$1"; }

# Every bootc entry as "DIR FILE DIGEST": DIR is `entries` (what boots now) or
# `entries.staged` (what bootc swaps in when the staged deployment is
# finalized), DIGEST the composefs identity the entry boots. A UKI entry's
# identity is in the UKI's sealed command line; a split kernel/initrd entry's
# is on its options line.
entry_table() {
  as_root '
    for d in /boot/loader/entries /boot/efi/loader/entries /boot/loader/entries.staged /boot/efi/loader/entries.staged; do
      [ -d "$d" ] || continue
      esp="${d%/loader/*}"
      for f in "$d"/bootc*.conf; do
        [ -e "$f" ] || continue
        uki="$(sed -n "s/^uki[[:space:]]\{1,\}//p" "$f" | head -1)"
        if [ -n "$uki" ]; then
          c="$(/usr/libexec/tilo-uki-cmdline show "$esp/${uki#/}" 2>/dev/null)"
        else
          c="$(sed -n "s/^options[[:space:]]//p" "$f")"
        fi
        dg="$(printf "%s\n" "$c" | tr " \t" "\n\n" | sed -n "s/^composefs=//p" | head -1)"
        printf "%s %s %s\n" "${d##*/}" "${f##*/}" "${dg:-none}"
      done
    done' | tr -d '\r'
}
# The file in DIR of TABLE that boots DIGEST; it fails unless there is exactly one.
entry_for() {
  local table="$1" dir="$2" digest="$3" hits
  hits="$(awk -v d="$dir" -v g="$digest" '$1 == d && $3 == g { print $2 }' <<<"$table")"
  [ "$(grep -c . <<<"$hits")" = 1 ] || return 1
  printf '%s\n' "$hits"
}

wait_reachable() {  # OLD-BOOT-ID [TIMEOUT [UNLOCK]]
  local old="$1" deadline=$(( $(date +%s) + ${2:-$BOOT_TIMEOUT} )) unlock="${3-$UNLOCK}" id
  while [ "$(date +%s)" -lt "$deadline" ]; do
    id="$(boot_id)"
    if [ -n "$id" ] && [ "$id" != "$old" ]; then printf '%s\n' "$id"; return 0; fi
    [ -z "$unlock" ] || bash -c "$unlock" >&2 || true
    sleep 3
  done
  return 1
}

reboot_guest() {
  local old="$1" new
  as_root 'systemctl reboot' >/dev/null 2>&1 || true
  if new="$(wait_reachable "$old")"; then printf '%s\n' "$new"; return 0; fi
  [ -n "$RESET" ] || return 1
  printf '      no answer within %ss; power-cycling with RESET\n' "$BOOT_TIMEOUT" >&2
  bash -c "$RESET" >&2 || return 1
  wait_reachable "$old"
}

# tilo-boot-health's verdict for this boot: good, failed, or not-assessed (the
# boot loader did not count this boot, so boot-complete.target never asked).
health_verdict() {
  local deadline=$(( $(date +%s) + HEALTH_TIMEOUT )) state result counted
  counted="$(as_root 'test -e /sys/firmware/efi/efivars/LoaderBootCountPath-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f && echo yes || echo no' | tr -d '\r\n')"
  while [ "$(date +%s)" -lt "$deadline" ]; do
    state="$(guest 'systemctl show -p ActiveState --value tilo-boot-health.service' 2>/dev/null | tr -d '\r\n')"
    result="$(guest 'systemctl show -p Result --value tilo-boot-health.service' 2>/dev/null | tr -d '\r\n')"
    case "$state" in
      active) echo good; return 0 ;;
      failed) echo failed; return 0 ;;
      inactive)
        if [ "$result" != success ]; then echo failed; return 0; fi
        if [ "$counted" = no ]; then echo not-assessed; return 0; fi ;;
    esac
    sleep 3
  done
  echo "undecided:$state/$result"
}

# systemd-bless-boot renames the entry after a good counted boot; give it the
# moment it takes before reading the entries.
wait_blessed() {
  local i state
  for i in $(seq 1 20); do
    state="$(guest 'systemctl show -p ActiveState --value systemd-bless-boot.service' 2>/dev/null | tr -d '\r\n')"
    case "$state" in active) return 0 ;; failed) return 1 ;; esac
    sleep 3
  done
  return 1
}

record_boot() {
  local name="$1"
  as_root 'echo "cmdline: $(cat /proc/cmdline)"; echo; bootctl status --no-pager 2>&1 | sed -n "/Current Entry/p;/Default Entry/p"; echo
           ls -l /boot/loader/entries /boot/loader/entries.staged /boot/efi/loader/entries /boot/efi/loader/entries.staged 2>&1; echo
           systemctl --no-pager status tilo-boot-health.service greetd.service systemd-bless-boot.service tilo-boot-entries.service 2>&1 | head -80; echo
           echo "greetd restarts: $(systemctl show -p NRestarts --value greetd.service)"
           cat /run/tilo-greeter/startup-metrics.json 2>&1; echo
           journalctl -b --no-pager -u tilo-boot-health -u tilo-boot-entries -u systemd-bless-boot 2>&1 | tail -60; echo
           systemd-analyze 2>&1 | head -2' > "$OUT/$name.txt" 2>&1
  entry_table >> "$OUT/$name.txt" 2>&1
}

# The boot's own boot-entry maintenance (tilo-boot-entries.timer) has run:
# prints its result once it has, fails if it never did.
await_boot_entries() {
  local i state
  for i in $(seq 1 60); do
    state="$(guest 'systemctl show -p ActiveState --value tilo-boot-entries.service' 2>/dev/null | tr -d '\r\n')"
    case "$state" in
      active|failed)
        guest 'systemctl show -p Result --value tilo-boot-entries.service' | tr -d '\r\n'
        return 0 ;;
    esac
    sleep 3
  done
  return 1
}

boot_entries_pass() {
  local log="$1" result
  as_root 'systemctl restart tilo-boot-entries.service' > "$log" 2>&1
  as_root 'journalctl -b --no-pager -u tilo-boot-entries | tail -20' >> "$log" 2>&1
  result="$(guest 'systemctl show -p Result --value tilo-boot-entries.service' | tr -d '\r\n')"
  [ "$result" = success ] || { printf '%s\n' "$result"; return 1; }
}

# --- 0. what is booted ----------------------------------------------------------
step "the booted deployment"
[ -n "$(boot_id)" ] || fail "RUNNER cannot reach the guest"
A_DIGEST="$(booted_digest)"
[[ "$A_DIGEST" =~ ^[[:xdigit:]]{128}$ ]] || fail "no composefs= identity on the kernel command line; this is not a bootc composefs boot"
A_ENTRY="$(current_entry)"
[ -n "$A_ENTRY" ] || fail "the boot loader did not report the booted entry (no LoaderEntrySelected); this check cannot see which deployment boots"
FLAVOUR="$(guest 'cat /usr/lib/tilo/image-flavour 2>/dev/null' | tr -d '\r\n')"
# Where the ESP is mounted on this layout: /boot on a sealed or dev disk,
# /boot/efi beside an installed system's separate /boot. Every boot tool looks
# only there, so an ESP they cannot find leaves each update without a try
# counter and each good boot unblessed; that fails here, before anything else.
as_root 'for m in /boot /boot/efi /efi; do ls "$m" >/dev/null 2>&1 || true
           findmnt -o TARGET,SOURCE,FSTYPE,OPTIONS --mountpoint "$m" || echo "$m: not a mount point"; done
         echo; bootctl status --no-pager 2>&1 | head -40' > "$OUT/00-esp.txt" 2>&1
ESP_PATH="$(as_root 'bootctl --print-esp-path 2>/dev/null' | tr -d '\r\n')"
[ -n "$ESP_PATH" ] || fail "systemd finds no mounted ESP (bootctl --print-esp-path), so no boot here can be blessed; see $OUT/00-esp.txt"
ok "the ESP is mounted at $ESP_PATH"
ok "booted ${A_ENTRY} (composefs ${A_DIGEST:0:16}, image flavour ${FLAVOUR:-sealed})"
# A boot still on trial is blessed (or not) by its own health check first; the
# booted entry has to be a settled one before another is staged beside it.
verdict="$(health_verdict)"
case "$verdict" in
  good) wait_blessed || fail "this boot was judged good but systemd-bless-boot never ran" ;;
  not-assessed) ;;
  *) fail "the booted deployment's own health check says '$verdict'; it is not a working starting point" ;;
esac
ok "this boot's own assessment: $verdict"
record_boot 00-booted
TABLE="$(entry_table)"
A_FILE="$(entry_for "$TABLE" entries "$A_DIGEST")" \
  || fail "no single loader entry boots the running composefs identity: $(tr '\n' ';' <<<"$TABLE")"
[ "$(id_of "$A_FILE")" = "$A_ENTRY" ] || fail "the boot loader reports entry $A_ENTRY, but the entry for this composefs identity is $A_FILE"
[[ "$A_FILE" != *+* ]] || fail "the booted entry is still on trial after its boot was settled ($A_FILE)"

# --- 1. the booted deployment's own boot-entry pass ---------------------------------
step "boot-entry maintenance on the booted deployment"
result="$(boot_entries_pass "$OUT/01-boot-entries.txt")" \
  || fail "tilo-boot-entries.service failed on the booted deployment ($result); see $OUT/01-boot-entries.txt"
ok "tilo-boot-entries.service succeeded"

# --- 2. stage, and the staged entry is counted before the reboot ------------------
step "stage $STAGE_IMAGE"
case "$STAGE_IMAGE" in
  containers-storage:*) switch="bootc switch --transport containers-storage $(printf '%q' "${STAGE_IMAGE#containers-storage:}")" ;;
  *) switch="bootc switch $(printf '%q' "$STAGE_IMAGE")" ;;
esac
start=$(date +%s)
as_root "timeout --kill-after=10s ${SWITCH_TIMEOUT}s $switch" > "$OUT/02-switch.txt" 2>&1 \
  || fail "bootc switch failed; see $OUT/02-switch.txt"
ok "staged in $(( $(date +%s) - start ))s"
as_root 'bootc status --json' > "$OUT/02-bootc-status.json" 2>/dev/null
B_DIGEST="$(python3 -c '
import json, sys
s = json.load(open(sys.argv[1]))["status"]["staged"] or {}
print(((s.get("composefs") or {}).get("verity")) or "")' "$OUT/02-bootc-status.json" 2>/dev/null)"
[[ "$B_DIGEST" =~ ^[[:xdigit:]]{128}$ ]] || fail "bootc status names no staged composefs identity; see $OUT/02-bootc-status.json"
[ "$B_DIGEST" != "$A_DIGEST" ] || fail "the staged deployment is the booted one; stage a different image"

# The production update paths (tilo-deployment update, bootc-fetch-apply-updates)
# run this same pass right after staging.
result="$(boot_entries_pass "$OUT/02-boot-entries.txt")" \
  || fail "tilo-boot-entries.service failed after staging ($result); see $OUT/02-boot-entries.txt"
TABLE="$(entry_table)"
printf '%s\n' "$TABLE" > "$OUT/02-entries.txt"
# The entries the next boot reads: bootc's staged set when it wrote one.
NEXT=entries
grep -q '^entries\.staged ' <<<"$TABLE" && NEXT=entries.staged
B_FILE="$(entry_for "$TABLE" "$NEXT" "$B_DIGEST")" \
  || fail "no single $NEXT entry boots the staged composefs identity ${B_DIGEST:0:16}; see $OUT/02-entries.txt"
A_NEXT="$(entry_for "$TABLE" "$NEXT" "$A_DIGEST")" \
  || fail "staging left no single $NEXT entry for the running deployment; see $OUT/02-entries.txt"
[[ "$A_NEXT" != *+* ]] || fail "the running deployment's entry was given a try counter ($A_NEXT); a boot that already succeeded would be put on trial"
[[ "$B_FILE" =~ \+([0-9]+)\.conf$ ]] || fail "the staged entry $B_FILE carries no try counter: a broken update would be retried for ever"
TRIES="${BASH_REMATCH[1]}"
[ "$TRIES" -ge 1 ] || fail "the staged entry has $TRIES tries"
B_ENTRY="$(id_of "$B_FILE")"
ok "the staged entry carries a try counter before the reboot: $NEXT/$B_FILE (the running one is $NEXT/$A_NEXT)"

# --- 3. the staged deployment cannot reach the greeter --------------------------------
step "make only the staged deployment unable to reach the greeter ($FAULT)"
UNLOCK_INJECTED=no
if [ "$FAULT" = unlock ]; then
  # The staged entry's own command line, edited in a private mount namespace
  # with the ESP's superblock writable only for the edit.
  fault_script='set -eu
esp="$1" file="$2"
if grep -q "^uki[[:space:]]" "$file" || ! grep -q "^options[[:space:]]" "$file"; then echo sealed; exit 0; fi
# The prompt is keyed to the UUID of the root filesystem, as the entry names it:
# systemd-cryptsetup takes per-device options only for a full 128-bit UUID,
# which a FAT serial is not, and without them it reads the device as LUKS,
# fails at once and lets the boot go on.
uuid="$(grep "^options[[:space:]]" "$file" | tr " \t" "\n\n" | sed -n "s/^root=UUID=//p" | head -1)"
printf "%s" "$uuid" | grep -Eqx "[0-9a-fA-F]{8}-([0-9a-fA-F]{4}-){3}[0-9a-fA-F]{12}" \
  || { echo "the staged entry names no root=UUID= to key the prompt to" >&2; exit 1; }
unshare -m sh -c "mount -o remount,rw \"\$1\" && sed -i \"/^options[[:space:]]/s/\\\$/ rd.luks.name=\$3=tilo-unlock-fault rd.luks.options=\$3=plain/\" \"\$2\"; rc=\$?; mount -o remount,ro \"\$1\"; exit \$rc" fault "$esp" "$file" "$uuid"
grep "^options" "$file"'
  shape="$(as_root "printf '%s' $(printf '%q' "$(printf '%s' "$fault_script" | base64 -w0)") | base64 -d > /run/tilo-unlock-fault.sh && bash /run/tilo-unlock-fault.sh $(printf '%q' "$ESP_PATH") $(printf '%q' "$ESP_PATH/loader/$NEXT/$B_FILE")" 2>"$OUT/03-fault.txt" | tr -d '\r')"
  printf '%s\n' "$shape" >> "$OUT/03-fault.txt"
  if [ "$shape" = sealed ]; then
    [ -n "$UNLOCK" ] || fail "the staged entry boots a sealed UKI whose command line cannot carry the fault, and this guest has no encrypted root (UNLOCK) whose prompt could be left waiting"
    ok "the staged entry is sealed; its boots will wait at the encrypted root's own prompt, which is not answered"
  else
    grep -q 'rd.luks.name=[^ ]*=tilo-unlock-fault' <<<"$shape" || fail "could not add the unlock prompt to $NEXT/$B_FILE; see $OUT/03-fault.txt"
    UNLOCK_INJECTED=yes
    ok "the staged entry's initrd now waits at an unlock prompt: $shape"
  fi
elif [ "$FAULT" = greetd ]; then
  as_root "install -d -m 0755 $DROPIN_DIR && cat > $DROPIN <<EOF
# Written by os/tests/boot_assessment_rollback.sh: greetd fails on the
# deployment under test (composefs $B_DIGEST) and nowhere else.
[Service]
ExecStartPre=/usr/bin/sh -c '! grep -q \"composefs=$B_DIGEST\" /proc/cmdline'
EOF
systemctl daemon-reload" > "$OUT/03-fault.txt" 2>&1 || fail "could not write $DROPIN"
else
  # greetd's own command line, so the deployment that is not under test runs
  # exactly what it ran before. The unit may name the program bare
  # (ExecStart=greetd, which systemd reports as path=greetd and looks up when
  # it starts it), so a bare name is resolved in the guest the same way.
  GREETD_EXEC="$(guest 'systemctl show -p ExecStart --value greetd.service' | tr -d '\r' | head -1)"
  greetd_path="$(sed -n 's/.*path=\([^ ;]*\).*/\1/p' <<<"$GREETD_EXEC")"
  if [[ -n "$greetd_path" && "$greetd_path" != /* ]]; then
    greetd_path="$(guest "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin command -v $(printf '%q' "$greetd_path")" | tr -d '\r\n')"
  fi
  greetd_args="$(sed -n 's/.*argv\[\]=\([^;]*[^; ]\) *;.*/\1/p' <<<"$GREETD_EXEC" | cut -s -d' ' -f2-)"
  GREETD_ARGV="$greetd_path${greetd_args:+ $greetd_args}"
  [[ "$greetd_path" == /* ]] || fail "could not read greetd's ExecStart to wrap it (got '$GREETD_EXEC')"
  as_root "install -d -m 0755 $DROPIN_DIR && cat > $FAULT_CONFIG <<EOF
# Written by os/tests/boot_assessment_rollback.sh: the real greeter, cut off
# 12 s after it starts, for the deployment under test only (see $DROPIN).
[terminal]
vt = 1

[default_session]
command = \"/usr/bin/timeout 12 /usr/libexec/tilo-cage-session /usr/libexec/tilo/tilo-greeter\"
user = \"greetd\"
EOF
cat > $DROPIN <<EOF
# Written by os/tests/boot_assessment_rollback.sh: on the deployment under test
# (composefs $B_DIGEST) greetd runs a greeter that exits 12 s after it starts,
# so greetd stays in service and systemd restarts it every few seconds.
[Service]
ExecStart=
ExecStart=/usr/bin/sh -c 'if grep -q \"composefs=$B_DIGEST\" /proc/cmdline; then exec $GREETD_ARGV --config $FAULT_CONFIG; fi; exec $GREETD_ARGV'
EOF
systemctl daemon-reload" > "$OUT/03-fault.txt" 2>&1 || fail "could not write $DROPIN"
fi
guest 'systemctl is-active --quiet greetd.service' || fail "greetd is not active on the booted deployment after the fault was written; it must only affect the staged one"
[ "$FAULT" = unlock ] || ok "greetd still runs here; its $FAULT fault acts on composefs ${B_DIGEST:0:16}"

has_fallback_unit() {
  guest 'systemctl cat tilo-boot-fallback.service >/dev/null 2>&1'
}

# tilo-boot-fallback settles bootc on the deployment that came back and
# reports the one that failed; images from before it leave bootc queuing the
# failed one, which is checked as such.
check_fallback_settled() {  # TRIES-SPENT
  local queued report
  local i state
  has_fallback_unit || return 0
  for i in $(seq 1 40); do
    state="$(guest 'systemctl show -p ActiveState --value tilo-boot-fallback.service' 2>/dev/null | tr -d '\r\n')"
    case "$state" in active|failed) break ;; esac
    sleep 3
  done
  as_root 'systemctl show -p Result --value tilo-boot-fallback.service; journalctl -b --no-pager -u tilo-boot-fallback; bootc status --json' \
    > "$OUT/05-fallback.txt" 2>&1
  queued="$(as_root 'bootc status --json' 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)["status"].get("rollbackQueued"))' 2>/dev/null)"
  [ "$queued" = False ] || fail "after the fallback bootc still queues the failed deployment (rollbackQueued $queued); see $OUT/05-fallback.txt"
  report="$(as_root "grep -l 'composefs=$B_DIGEST' /var/lib/tilo/boot-fallback/*/report" | tr -d '\r' | head -1)"
  [ -n "$report" ] || fail "no fallback report names composefs ${B_DIGEST:0:16}; see $OUT/05-fallback.txt"
  as_root "cat ${report%/report}/summary.txt ${report}" >> "$OUT/05-fallback.txt" 2>&1
  as_root "grep -qx 'tries=$1' $report && grep -qx 'settled=yes' $report" \
    || fail "the fallback report does not record $1 spent tries, settled; see $OUT/05-fallback.txt"
  ok "the deployment that came back is bootc's default again, and ${report%/report} reports the failed one"
}

if [ "$FAULT" = unlock ]; then
  # --- 4. each staged boot waits at its unlock prompt and is power-cycled -------
  current_boot="$(boot_id)"
  as_root 'systemctl reboot' >/dev/null 2>&1 || true
  for attempt in $(seq 1 "$TRIES"); do
    step "boot $attempt: the staged deployment, left at its unlock prompt"
    if new="$(wait_reachable "$current_boot" "$HANG_TIMEOUT" "")"; then
      record_boot "04-boot$attempt"
      # What became of the prompt, where the check's own log keeps it.
      as_root 'echo "cmdline: $(cat /proc/cmdline)"; systemctl status --no-pager --all "systemd-cryptsetup@*" 2>&1 | head -40
               journalctl -b --no-pager -o short-monotonic 2>/dev/null | grep -iE "cryptsetup|unlock-fault|luks|password" | head -80' >&2 || true
      fail "boot $attempt reached userspace on composefs $(booted_digest | cut -c1-16) instead of waiting at its unlock prompt; see $OUT/04-boot$attempt.txt"
    fi
    if [ -n "$SERIAL" ]; then
      bash -c "$SERIAL" > "$OUT/04-boot$attempt-console.txt" 2>&1 || true
      if [ "$UNLOCK_INJECTED" = yes ]; then
        grep -qi 'tilo-unlock-fault' "$OUT/04-boot$attempt-console.txt" \
          || fail "boot $attempt never answered, but its console shows no unlock prompt for the fault; see $OUT/04-boot$attempt-console.txt"
      fi
      grep -qiE 'passphrase|password' "$OUT/04-boot$attempt-console.txt" \
        && ok "boot $attempt waits at an unlock prompt (console: $OUT/04-boot$attempt-console.txt)" \
        || printf '      boot %s: no prompt text on the console to show for it\n' "$attempt" >&2
    fi
    printf '      no answer within %ss; power-cycling at the prompt\n' "$HANG_TIMEOUT" >&2
    bash -c "$RESET" >&2 || fail "RESET failed after boot $attempt"
  done
  step "boot $((TRIES + 1)): after $TRIES tries the boot loader must go back by itself"
  current_boot="$(wait_reachable "$current_boot")" || fail "after $TRIES boots left at the unlock prompt the machine never came back (see the VM's serial log)"
  digest="$(booted_digest)"
  [ "$digest" = "$A_DIGEST" ] || fail "boot $((TRIES + 1)) runs composefs ${digest:0:16}, not the previous deployment ${A_DIGEST:0:16}"
  entry="$(current_entry)"
  verdict="$(health_verdict)"
  [ "$verdict" = good ] && wait_blessed
  record_boot "04-boot$((TRIES + 1))"
  case "$verdict" in good|not-assessed) ;; *) fail "the deployment that came back was judged '$verdict'; see $OUT/04-boot$((TRIES + 1)).txt" ;; esac
  TABLE="$(entry_table)"
  a_now="$(entry_for "$TABLE" entries "$A_DIGEST")" || fail "no single entry boots the deployment that came back"
  [[ "$a_now" != *+* ]] || fail "the deployment that came back is still on trial ($a_now)"
  if has_fallback_unit; then
    # bootc's rollback renumbers the entries, so the names the boot loader
    # used this boot are gone; the report holds what the boot loader counted.
    check_fallback_settled "$TRIES"
  else
    [ "$entry" = "$(id_of "$a_now")" ] || fail "the previous deployment booted, but the boot loader reports entry $entry, not $a_now"
    b_now="$(entry_for "$TABLE" entries "$B_DIGEST")" || b_now=""
    [ "$b_now" = "${B_ENTRY%.conf}+0-$TRIES.conf" ] \
      || fail "the used-up entry is ${b_now:-<gone>}, expected ${B_ENTRY%.conf}+0-$TRIES.conf to stay on the ESP"
  fi
  ok "boot $((TRIES + 1)): the boot loader went back to $a_now by itself after $TRIES boots that hung at the unlock prompt"
  as_root '/usr/libexec/tilo-deployment list 2>&1 || true' > "$OUT/05-list.txt" 2>&1
  printf 'PASS staged=%s tries=%s fault=%s\n' "$B_ENTRY" "$TRIES" "$FAULT" > "$OUT/verdict.txt"
  printf '\nPASS: an update that hangs at its unlock prompt spent its %s tries and the boot loader went back by itself (evidence in %s)\n' "$TRIES" "$OUT"
  exit 0
fi

# --- 4. each boot of the staged deployment spends a try, then it falls back ----------
current_boot="$(boot_id)"
attempt=0
while :; do
  attempt=$((attempt + 1))
  [ "$attempt" -le $((TRIES + 1)) ] || fail "after $TRIES failed boots and one more the machine still boots the broken deployment"
  step "boot $attempt"
  current_boot="$(reboot_guest "$current_boot")" || fail "boot $attempt never answered (see the VM's serial log)"
  digest="$(booted_digest)"
  entry="$(current_entry)"
  verdict="$(health_verdict)"
  [ "$verdict" = good ] && wait_blessed
  record_boot "04-boot$attempt"
  TABLE="$(entry_table)"
  b_now="$(entry_for "$TABLE" entries "$B_DIGEST")" || b_now=""
  printf '      entry %s, composefs %.16s, health %s, broken entry now %s\n' \
    "$entry" "$digest" "$verdict" "${b_now:-<gone>}" >&2
  if [ "$digest" = "$B_DIGEST" ]; then
    [ "$entry" = "$B_ENTRY" ] || fail "boot $attempt runs composefs ${B_DIGEST:0:16} but the boot loader reports entry $entry, not $B_ENTRY"
    [ "$attempt" -le "$TRIES" ] || fail "boot $attempt chose the broken entry with no tries left"
    [ "$verdict" = failed ] || fail "boot $attempt of the broken deployment was judged '$verdict', not failed; see $OUT/04-boot$attempt.txt"
    if [ "$FAULT" = greeter ]; then
      # The fault this mode exists for: greetd never failed, systemd kept
      # restarting it. A greetd that failed outright proves the other mode.
      restarts="$(guest 'systemctl show -p NRestarts --value greetd.service' | tr -d '\r\n')"
      greetd_state="$(guest 'systemctl show -p ActiveState --value greetd.service' | tr -d '\r\n')"
      [[ "$restarts" =~ ^[0-9]+$ ]] && [ "$restarts" -ge 1 ] \
        || fail "boot $attempt: greetd was never restarted (NRestarts '$restarts'), so the greeter fault did not happen; see $OUT/04-boot$attempt.txt"
      [ "$greetd_state" != failed ] || fail "boot $attempt: greetd failed outright, which is the greetd fault, not a crash-looping greeter"
      ok "boot $attempt: greetd stayed in service ($greetd_state) and was restarted $restarts times, and the boot was still judged failed"
    fi
    left=$((TRIES - attempt))
    [ "$b_now" = "${B_ENTRY%.conf}+$left-$attempt.conf" ] \
      || fail "after boot $attempt the broken entry is ${b_now:-<gone>}, expected ${B_ENTRY%.conf}+$left-$attempt.conf (blessed, or not decremented)"
    # A broken machine stays up long enough for its own boot-entry pass, which
    # puts the deployment it would fall back to on trial too.
    result="$(await_boot_entries)" || fail "boot $attempt's tilo-boot-entries.timer never ran"
    [ "$result" = success ] || fail "tilo-boot-entries.service failed on the staged deployment ($result); see $OUT/04-boot$attempt.txt"
    TABLE="$(entry_table)"
    a_counted=no
    [[ "$(entry_for "$TABLE" entries "$A_DIGEST")" == *+* ]] && a_counted=yes
    ok "boot $attempt: the broken deployment failed its health check and keeps $left tries"
    continue
  fi
  [ "$digest" = "$A_DIGEST" ] || fail "boot $attempt runs composefs ${digest:0:16}, neither deployment"
  [ "$attempt" = $((TRIES + 1)) ] || fail "the machine went back after $((attempt - 1)) failed boots, not $TRIES"
  a_now="$(entry_for "$TABLE" entries "$A_DIGEST")" || fail "no single entry boots the deployment that came back"
  if ! has_fallback_unit; then
    [ "$entry" = "$(id_of "$a_now")" ] || fail "the previous deployment booted, but the boot loader reports entry $entry, not $a_now"
    [ "$b_now" = "${B_ENTRY%.conf}+0-$TRIES.conf" ] \
      || fail "the used-up entry is ${b_now:-<gone>}, expected ${B_ENTRY%.conf}+0-$TRIES.conf to stay on the ESP"
  fi
  # Counted before this boot, it has to be judged and blessed like any other.
  if [ "${a_counted:-no}" = yes ]; then
    [ "$verdict" = good ] || fail "the deployment that came back was on trial and judged '$verdict', not good; see $OUT/04-boot$attempt.txt"
  else
    [ "$verdict" = not-assessed ] || fail "the deployment that came back was not on trial, yet its health check says '$verdict'"
  fi
  [[ "$a_now" != *+* ]] || fail "the deployment that came back is still on trial ($a_now) after a good boot: systemd-bless-boot did not bless it, so it will spend its tries on boots that work; see $OUT/04-boot$attempt.txt"
  guest 'systemctl is-active --quiet greetd.service' || fail "greetd is not active on the deployment that came back"
  ok "boot $attempt: the boot loader went back to $a_now by itself (on trial: ${a_counted:-no}, health: $verdict, blessed)"
  check_fallback_settled "$TRIES"
  break
done

as_root '/usr/libexec/tilo-deployment list 2>&1 || true' > "$OUT/05-list.txt" 2>&1
as_root "rm -f $DROPIN $FAULT_CONFIG && systemctl daemon-reload" >/dev/null 2>&1 || true
printf 'PASS staged=%s tries=%s fault=%s\n' "$B_ENTRY" "$TRIES" "$FAULT" > "$OUT/verdict.txt"
printf '\nPASS: a broken update (%s fault) spent its %s tries and the boot loader went back by itself (evidence in %s)\n' "$FAULT" "$TRIES" "$OUT"
