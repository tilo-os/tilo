#!/usr/bin/env bash
# The interactive-priority policy is shipped and, on a booted system, in force.
#
#   os/tests/interactive_priority.sh            # static: the files the image ships
#   os/tests/interactive_priority.sh --live     # booted: the manager's view of them
#
# Static mode reads the tree and runs anywhere. Live mode only reads
# (`systemctl show`, /sys); run it as the desktop's user on a booted image.
# Design: crates/packaging/launcher/src/priority.rs.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
etc="$repo/os/etc/systemd"
fail=0
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
ok() { printf 'ok    %s\n' "$*"; }

has() { # has FILE LINE
  grep -Fqx "$2" "$1" 2>/dev/null || bad "${1#"$repo"/}: missing '$2'"
}

mode=static
[ "${1:-}" = --live ] && mode=live

if [ "$mode" = static ]; then
  for f in background user; do
    c="$etc/user/$f.slice.d/50-tilo-batch.conf"
    has "$c" CPUWeight=10
    has "$c" IOWeight=10
    has "$c" MemoryLow=0
    has "$c" ManagedOOMMemoryPressure=kill
    has "$c" ManagedOOMSwap=kill
  done
  ok 'batch tier: background.slice and the podman slice yield and are the OOM targets'
  # never capped
  if grep -rE '^(CPUQuota|MemoryMax|MemoryHigh|IOReadBandwidthMax|IOWriteBandwidthMax)=' \
      "$etc/user" >/dev/null; then
    bad 'a user slice caps resources; batch work must only yield'
  else ok 'nothing in the user tier is capped'; fi
  has "$etc/user/app.slice.d/50-tilo-interactive.conf" MemoryLow=15%
  if grep -q '^ManagedOOM' "$etc/user/app.slice.d/50-tilo-interactive.conf" \
      "$etc/user/session.slice.d/50-tilo-interactive.conf"; then
    bad 'app.slice or session.slice is an oomd target; the focused app could be killed'
  fi
  has "$etc/user/session.slice.d/50-tilo-interactive.conf" CPUWeight=400
  has "$etc/system/user@.service.d/50-tilo-interactive.conf" Delegate=io
  has "$etc/system/user.slice.d/50-tilo-interactive.conf" CPUWeight=500
  has "$etc/system/user-.slice.d/50-tilo-interactive.conf" MemoryMin=256M
  has "$etc/system/fs.service.d/30-tilo-priority.conf" ManagedOOMPreference=avoid
  has "$etc/oomd.conf.d/50-tilo.conf" SwapUsedLimitPercent=80%
  ok 'session tier: weights, floors, io delegation, oomd limits'
  grep -Fqx 'Slice=session.slice' "$repo/os/usr/lib/systemd/user/tilo-app-host.service" \
    || bad 'tilo-app-host is not in session.slice'
  grep -Fqx 'Slice=session.slice' "$repo/os/usr/lib/systemd/user/tilo-fs-recipient.service" \
    || bad 'tilo-fs-recipient is not in session.slice'
  grep -Fqx 'Slice=background.slice' "$repo/os/usr/lib/systemd/user/tilo-fs-cloud.service" \
    || bad 'tilo-fs-cloud sync is not background'
  grep -Fq 'set-property' "$repo/os/etc/polkit-1/rules.d/49-tilo-session-priority.rules" \
    && grep -Fq 'subject.session' "$repo/os/etc/polkit-1/rules.d/49-tilo-session-priority.rules" \
    || bad 'polkit rule for the session scope is missing or too broad'
  grep -Fq '49-tilo-session-priority.rules' "$repo/os/Containerfile" \
    || bad 'the polkit rule is not copied into the image'
  [ -f "$etc/zram-generator.conf" ] && ok 'zram policy shipped'
else
  prop() { # prop MANAGER UNIT NAME EXPECTED
    local got
    got=$(systemctl "$1" show -P "$3" "$2" 2>/dev/null) || got='<unit missing>'
    [ "$got" = "$4" ] && ok "$2 $3=$4" || bad "$2 $3: expected $4, manager says $got"
  }
  prop --user background.slice CPUWeight 10
  prop --user background.slice IOWeight 10
  prop --user background.slice ManagedOOMMemoryPressure kill
  prop --user user.slice CPUWeight 10
  prop --user user.slice ManagedOOMMemoryPressure kill
  prop --user app.slice ManagedOOMMemoryPressure auto
  prop --user session.slice CPUWeight 400
  prop --user tilo-app-host.service Slice session.slice
  prop --system fs.service ManagedOOMPreference avoid
  prop --system "user@$(id -u).service" CPUWeight 500
  # The io controller must be delegated, or per-app IOWeight is inert.
  uid=$(id -u)
  controllers=$(cat "/sys/fs/cgroup/user.slice/user-$uid.slice/user@$uid.service/cgroup.controllers")
  case " $controllers " in *' io '*) ok 'io controller delegated to the user manager' ;;
    *) bad "io not delegated: $controllers" ;; esac
  # The compositor's session scope, set by the compositor at startup.
  shell_pid=$(pgrep -xo shell || true)
  if [ -n "$shell_pid" ]; then
    scope=$(sed -n 's#.*/\(session-[0-9]*\.scope\)$#\1#p' "/proc/$shell_pid/cgroup")
    prop --system "$scope" CPUWeight 1000
    prop --system "$scope" MemorySwapMax 0
  else bad 'no compositor running'; fi
  # Every app scope is placed in app.slice with a tier.
  while read -r unit; do
    w=$(systemctl --user show -P CPUWeight "$unit")
    case "$w" in 800|50) ;; *) bad "$unit has no foreground/background tier (CPUWeight=$w)" ;; esac
    # One process the kernel kills for memory must not stop the whole app.
    o=$(systemctl --user show -P OOMPolicy "$unit")
    [ "$o" = continue ] || bad "$unit OOMPolicy=$o: one OOM kill would stop the whole app"
  done < <(systemctl --user list-units --plain --no-legend 'app-tilo-*.scope' | awk '{print $1}')
  # Exactly one scope is boosted: the focused one.
  boosted=$(systemctl --user list-units --plain --no-legend 'app-tilo-*.scope' | awk '{print $1}' |
    while read -r u; do [ "$(systemctl --user show -P CPUWeight "$u")" = 800 ] && echo "$u"; done | wc -l)
  [ "$boosted" -le 1 ] && ok "at most one app scope is boosted ($boosted)" || bad "$boosted app scopes boosted"
  # IO weights need a scheduler that honours them.
  for dev in /sys/block/nvme*n*; do
    [ -e "$dev/queue/scheduler" ] || continue
    sched=$(cat "$dev/queue/scheduler")
    case "$sched" in *'[bfq]'*) ok "$(basename "$dev") io scheduler bfq" ;;
      *) bad "$(basename "$dev") scheduler is not bfq: $sched" ;; esac
  done
  systemctl is-active --quiet systemd-oomd && ok 'systemd-oomd active' || bad 'systemd-oomd inactive'
  grep -q 'SwapUsedLimitPercent=80%' /etc/systemd/oomd.conf.d/50-tilo.conf 2>/dev/null \
    && ok 'oomd policy installed' || bad 'oomd policy missing'
fi
exit "$fail"
