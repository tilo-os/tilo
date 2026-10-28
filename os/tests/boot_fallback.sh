#!/usr/bin/env bash
# After the boot loader went back from a deployment that failed its tries, the
# running deployment becomes bootc's default again and the person gets a report
# of what failed.
#
#   ./os/tests/boot_fallback.sh
#
# sd-boot starts the previous deployment once the new one has spent its tries
# (`name+0-3.conf`), but bootc still queues the failed one (rollbackQueued), so
# the next rewrite of the entries would hand it fresh tries and the next boot
# would try it again. The production `tilo-deployment fallback` runs here
# against a stand-in ESP, bootc, journal and pstore archive, and each case
# asserts what it settles, what it leaves alone, and the report it writes.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
deployment="$repo/os/usr/libexec/tilo-deployment"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fails=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*"; fails=$((fails + 1)); }

running=$(printf 'a%.0s' {1..128})
failed=$(printf 'b%.0s' {1..128})
staged=$(printf 'c%.0s' {1..128})
logged_boot=1111111111111111111111111111111a
silent_boot=2222222222222222222222222222222b
current_boot="$(tr -d '-' < /proc/sys/kernel/random/boot_id)"

entry() {  # FILE DIGEST SORT-KEY
  printf 'title Fedora Linux 44\nlinux /EFI/Linux/bootc_composefs-%s/vmlinuz\ninitrd /EFI/Linux/bootc_composefs-%s/initrd\noptions root=tilo-auto rw composefs=%s\nsort-key %s\n' \
    "$2" "$2" "$2" "$3" >"$work/esp/loader/entries/$1"
}

install -d "$work/bin"
# bootc: status from the fixture, and a rollback that does what bootc's does
# on this backend: it un-queues the rollback deployment and rewrites every
# entry, dropping the counters in their names.
cat >"$work/bin/bootc" <<'STUB'
#!/usr/bin/env bash
echo "bootc $*" >>"$FIXTURE/log"
case "$*" in
  "status --format json")
    staged=null
    [ -z "$FIXTURE_STAGED" ] || staged="{\"image\": {\"image\": {\"image\": \"registry.tilo.so/tilo/tilo-vm:next\"}}, \"composefs\": {\"verity\": \"$FIXTURE_STAGED\"}}"
    cat <<JSON
{"status": {
  "rollbackQueued": $(cat "$FIXTURE/queued"),
  "booted": {"image": {"image": {"image": "registry.tilo.so/tilo/tilo-vm:main-1a2b3c4"}, "version": "44.20261022.0"},
             "composefs": {"verity": "sha512:$RUNNING"}},
  "staged": $staged,
  "rollback": {"image": {"image": {"image": "registry.tilo.so/tilo/tilo-vm:main-5d6e7f8"}, "version": "44.20261023.0",
               "timestamp": "2026-10-23T21:40:00Z"}, "composefs": {"verity": "sha512:$FAILED"}}
}}
JSON
    ;;
  rollback)
    [ -z "${FIXTURE_ROLLBACK_FAILS:-}" ] || exit 1
    if [ "$(cat "$FIXTURE/queued")" = true ]; then echo false; else echo true; fi >"$FIXTURE/queued.new"
    mv "$FIXTURE/queued.new" "$FIXTURE/queued"
    for f in "$FIXTURE"/esp/loader/entries/*+*.conf; do
      [ -e "$f" ] || continue
      base="${f##*/}"; mv "$f" "${f%/*}/${base%%+*}.conf"
    done ;;
  *) exit 2 ;;
esac
STUB
cat >"$work/bin/tilo-image-trust" <<'STUB'
#!/usr/bin/env python3
import json, sys
status = json.load(sys.stdin)["status"]
if sys.argv[2] == "rollback-queued":
    q = status.get("rollbackQueued")
    print("" if q is None else str(q).lower())
    sys.exit(0)
role, field = sys.argv[2].split("-", 1)
value = status.get(role) or {}
path = {"image": ["image", "image", "image"], "version": ["image", "version"],
        "timestamp": ["image", "timestamp"], "composefs": ["composefs", "verity"]}[field]
for key in path:
    value = value.get(key) if isinstance(value, dict) else None
print(value or "")
STUB
# The journal: two earlier boots and this one. Only the first earlier boot
# ran the failed deployment far enough to log; the others ran the good one.
cat >"$work/bin/journalctl" <<'STUB'
#!/usr/bin/env bash
args="$*"
case "$args" in
  *--list-boots*)
    printf '[{"index":-2,"boot_id":"%s","first_entry":1,"last_entry":2},' "$LOGGED_BOOT"
    printf '{"index":-1,"boot_id":"%s","first_entry":3,"last_entry":4},' "$SILENT_BOOT"
    printf '{"index":0,"boot_id":"%s","first_entry":5,"last_entry":6}]\n' "$CURRENT_BOOT"
    exit 0 ;;
esac
boot=""; set -- "$@"
while [ $# -gt 0 ]; do [ "$1" = -b ] && boot="$2"; shift; done
[ "$boot" = "$LOGGED_BOOT" ] || exit 0
case "$args" in
  *"--grep composefs=$FAILED"*) echo "Kernel command line: root=tilo-auto composefs=$FAILED" ;;
  *"-o short-unix"*) echo "1761255600.123456 tilo kernel: Booting Linux" ;;
  *"-o cat -u tilo-boot-health.service"*) echo "tilo-boot-health: WARNING: the greeter did not hold for 20s within 180s" ;;
  *"-u tilo-boot-health.service"*) echo "2026-10-23T23:40:00+0200 tilo tilo-boot-health: WARNING: the greeter did not hold" ;;
  *"-p warning"*) echo "[    4.2] tilo kernel: msm_dpu: probe failed" ;;
esac
STUB
cat >"$work/bin/findmnt" <<'STUB'
#!/usr/bin/env bash
[ "$*" = "-n -o SOURCE /" ] && { echo "composefs:$RUNNING"; exit 0; }
exit 1
STUB
cat >"$work/bin/id" <<'STUB'
#!/usr/bin/env bash
[ "$1" = -u ] && { echo 0; exit 0; }
exec /usr/bin/id "$@"
STUB
# tilo-esp-write applies a plan; here every plan is only recorded.
cat >"$work/bin/record-esp-plan" <<'STUB'
#!/usr/bin/env bash
cat "$FIXTURE/plan/plan.json" >>"$FIXTURE/plans"
STUB
cat >"$work/bin/chgrp" <<'STUB'
#!/usr/bin/env bash
exit 0
STUB
chmod +x "$work/bin"/*
echo dev >"$work/flavour"

reset() {  # FAILED-ENTRY-FILE QUEUED
  rm -rf "$work/esp" "$work/reports" "$work/pstore" "$work/log" "$work/plan" "$work/plans"
  install -d "$work/esp/loader/entries" "$work/pstore"
  entry bootc_bootc-44-1.conf "$running" bootc-bootc-1
  entry "$1" "$failed" bootc-bootc-0
  echo "$2" >"$work/queued"
  : >"$work/log"
}

run() {  # ARGS...
  out="$(env -i PATH="$work/bin:/usr/bin:/bin" FIXTURE="$work" \
    RUNNING="$running" FAILED="$failed" FIXTURE_STAGED="${FIXTURE_STAGED:-}" \
    FIXTURE_ROLLBACK_FAILS="${FIXTURE_ROLLBACK_FAILS:-}" \
    LOGGED_BOOT="$logged_boot" SILENT_BOOT="$silent_boot" CURRENT_BOOT="$current_boot" \
    TILO_ESP="$work/esp" TILO_ESP_CANDIDATES="$work/nowhere" \
    TILO_IMAGE_FLAVOUR_FILE="$work/flavour" TILO_ESP_PLAN_DIR="$work/plan" \
    TILO_ESP_WRITE_CMD=record-esp-plan TILO_FALLBACK_DIR="$work/reports" TILO_PSTORE_ARCHIVE="$work/pstore" \
    bash "$deployment" "$@" 2>&1)"
  status=$?
}

report() { ls -d "$work"/reports/*-"${failed:0:16}" 2>/dev/null | head -1; }

# --- the boot loader went back by itself --------------------------------------
reset 'bootc_bootc-44-0+0-3.conf' true
install -d "$work/pstore/1761255700000000"
echo 'Kernel panic - not syncing' >"$work/pstore/1761255700000000/dmesg.txt"
touch -d '@1761255700' "$work/pstore/1761255700000000"
run fallback
[ "$status" = 0 ] && ok "a deployment that spent its tries is settled" || bad "fallback failed ($status): $out"
[ "$(cat "$work/queued")" = false ] \
  && ok "the running deployment is bootc's default again" || bad "bootc still queues the failed deployment: $out"
grep -qx 'bootc rollback' "$work/log" && ok "it was bootc's own rollback that changed it" || bad "no bootc rollback: $(cat "$work/log")"
dir="$(report)"
if [ -n "$dir" ] && [ -f "$dir/summary.txt" ]; then
  ok "a report was written: ${dir##*/}"
  summary="$(cat "$dir/summary.txt")"
  grep -q 'main-5d6e7f8 44.20261023.0' <<<"$summary" && ok "the report names the image that failed" \
    || bad "the report does not name the failed image: $summary"
  grep -q 'Tried:          3 times' <<<"$summary" && ok "the report says how many tries were spent" \
    || bad "the report does not count the tries: $summary"
  grep -q '2 tries left no log' <<<"$summary" && ok "tries that stopped before the system started are counted as leaving no log" \
    || bad "the silent tries are not reported: $summary"
  grep -q 'boot-1.log' <<<"$summary" && grep -q 'msm_dpu' "$dir/boot-1.log" \
    && ok "the try that left a journal has its warnings in the report" || bad "the logged try is missing: $summary"
  [ -f "$dir/pstore/1761255700000000/dmesg.txt" ] && ok "the firmware's crash record is in the report" \
    || bad "the pstore record was not kept: $(ls -R "$dir")"
  grep -qx 'settled=yes' "$dir/report" && grep -qx 'tries=3' "$dir/report" \
    && ok "the machine-readable report says it settled" || bad "report: $(cat "$dir/report")"
else
  bad "no report under $work/reports: $out"
fi
# The renames go through tilo-esp-write's plans, which this fixture only records.
grep -q '"dst": "loader/entries/bootc_bootc-44-0+3.conf"' "$work/plans" 2>/dev/null \
  && ok "the entry bootc rewrote for the failed deployment is put back on trial" \
  || bad "after the rollback the failed deployment's entry gets no counter: $(cat "$work/plans" 2>&1)"

run list
grep -q 'Last fallback' <<<"$out" && grep -q 'did not start after 3 tries' <<<"$out" \
  && ok "tilo status shows the last fallback" || bad "tilo status does not show the fallback: $out"

run fallback
[ "$status" = 0 ] && grep -q 'nothing to settle' <<<"$out" && [ "$(ls -d "$work"/reports/*/ | wc -l)" = 1 ] \
  && ok "the next boot finds nothing to settle and writes no second report" || bad "second run: $out"

# --- chosen by hand from the boot menu after two failed tries ----------------
reset 'bootc_bootc-44-0+1-2.conf' true
run fallback
[ "$status" = 0 ] && [ "$(cat "$work/queued")" = false ] && grep -qx 'tries=2' "$(report)/report" \
  && ok "picking the previous entry after failed tries keeps it the default" || bad "manual fallback: $out"

# --- deliberate choices are left alone ------------------------------------------
reset 'bootc_bootc-44-0.conf' true
run fallback
[ "$status" = 0 ] && ! grep -q rollback "$work/log" && [ -z "$(report)" ] \
  && ok "a deployment that booted well before is not overruled" || bad "a proven deployment was settled away: $out"

reset 'bootc_bootc-44-0+3.conf' true
run fallback
[ "$status" = 0 ] && ! grep -q rollback "$work/log" && [ -z "$(report)" ] \
  && ok "a deployment that was never tried is not reported as failed" || bad "an untried deployment was settled away: $out"

reset 'bootc_bootc-44-0+0-3.conf' false
run fallback
[ "$status" = 0 ] && ! grep -q rollback "$work/log" \
  && ok "nothing happens when the running deployment already boots next" || bad "settled a machine that needed nothing: $out"

reset 'bootc_bootc-44-0+0-3.conf' true
FIXTURE_STAGED="$staged" run fallback
[ "$status" = 0 ] && ! grep -q rollback "$work/log" \
  && ok "a staged update is left to boot next" || bad "a staged update was overruled: $out"

# --- bootc refuses ----------------------------------------------------------------
reset 'bootc_bootc-44-0+0-3.conf' true
FIXTURE_ROLLBACK_FAILS=1 run fallback
[ "$status" != 0 ] && grep -qx 'settled=no' "$(report)/report" && grep -q 'bootc still queues' <<<"$out" \
  && ok "when bootc cannot settle it, the unit fails and the report says so" || bad "a failed rollback passed as settled: $out"

exit "$fails"
