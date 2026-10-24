#!/usr/bin/env bash
# The clock floor follows authenticated time only, and never moves back.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo/os/usr/libexec/tilo-clock-floor"
units="$repo/os/usr/lib/systemd/system"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() { echo "clock_floor: $*" >&2; exit 1; }

# A stand-in for the image-trust gate: prints the time in the file, or
# refuses like an unsynchronised chrony.
gate="$work/trusted-time"
cat >"$gate" <<'GATE'
#!/usr/bin/env bash
[ "$1" = --trusted-time ] || exit 2
[ -s "$TRUSTED_NOW_FILE" ] || exit 1
cat "$TRUSTED_NOW_FILE"
GATE
chmod +x "$gate"
now_file="$work/now"
floor="$work/var/lib/systemd/timesync/clock"
run() {
  TILO_CLOCK_FLOOR="$floor" TILO_CLOCK_FLOOR_TRUSTED_TIME="$gate" \
    TRUSTED_NOW_FILE="$now_file" bash "$script"
}

# An unsynchronised clock records nothing.
: >"$now_file"
run 2>/dev/null
[ ! -e "$floor" ] || fail "an unsynchronised clock became the floor"

# Authenticated time becomes the floor.
echo 1792800000123456789 >"$now_file"
run
[ "$(stat -c %Y "$floor")" = 1792800000 ] || fail "the floor is not the authenticated time"

# An earlier reading never lowers it.
echo 1789654853000000000 >"$now_file"
run
[ "$(stat -c %Y "$floor")" = 1792800000 ] || fail "the floor moved back"

# A later one raises it.
echo 1792803600000000000 >"$now_file"
run
[ "$(stat -c %Y "$floor")" = 1792803600 ] || fail "the floor did not follow the clock"

# The floor is recorded when the clock steps, hourly and at shutdown, after
# chrony and with /var mounted.
grep -Fqx 'OnClockChange=yes' "$units/tilo-clock-floor.timer" || fail "the timer misses clock steps"
grep -Fqx 'OnCalendar=hourly' "$units/tilo-clock-floor.timer" || fail "the timer is not hourly"
grep -Fqx 'ExecStop=/usr/libexec/tilo-clock-floor' "$units/tilo-clock-floor-shutdown.service" \
  || fail "nothing records the floor at shutdown"
for unit in tilo-clock-floor.service tilo-clock-floor-shutdown.service; do
  grep -Fqx 'After=chronyd.service' "$units/$unit" || fail "$unit is not ordered after chronyd"
  grep -Fqx 'RequiresMountsFor=/var/lib/systemd' "$units/$unit" || fail "$unit may run without /var"
done
grep -Fq 'systemctl enable tilo-clock-floor.timer tilo-clock-floor-shutdown.service' \
  "$repo/os/image/system.Containerfile" || fail "the image does not enable the floor units"
echo "clock_floor: ok"
