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
    bad 'app.slice or session.slice is an oomd target; an app could be killed'
  fi
  has "$etc/user/session.slice.d/50-tilo-interactive.conf" CPUWeight=400
  # The app tree hands each app every controller its tier and accounting use.
  has "$etc/system/user@.service.d/50-tilo-interactive.conf" 'Delegate=cpu io memory pids'
  has "$etc/system/user.slice.d/50-tilo-interactive.conf" CPUWeight=500
  has "$etc/system/user-.slice.d/50-tilo-interactive.conf" MemoryMin=256M
  has "$etc/system/fs.service.d/30-tilo-priority.conf" ManagedOOMPreference=avoid
  has "$etc/oomd.conf.d/50-tilo.conf" SwapUsedLimit=80%
  # systemd-oomd ignores a key it does not know with a warning in the journal,
  # so a misspelt limit silently is no limit: only oomd.conf(5)'s keys.
  unknown=$(grep -E '^[A-Za-z]+=' "$etc/oomd.conf.d/50-tilo.conf" | cut -d= -f1 \
    | grep -vxE 'SwapUsedLimit|DefaultMemoryPressureLimit|DefaultMemoryPressureDurationSec' || true)
  [ -z "$unknown" ] || bad "oomd.conf.d/50-tilo.conf: keys systemd-oomd does not know: $unknown"
  # The kernel's OOM killer never chooses the session (the compositor greetd
  # starts inherits it); apps leave it before they start.
  has "$etc/system/greetd.service.d/50-tilo-oom.conf" OOMScoreAdjust=-1000
  # The compositor lives in logind's session scope, so that scope carries the
  # session's own weights, floors and swap policy, and is not an oomd target.
  # A prefix drop-in applies it to every session scope before anything starts
  # in it, with nothing asking the manager at runtime.
  scope="$etc/system/session-.scope.d/50-tilo-interactive.conf"
  grep -Fqx '[Scope]' "$scope" 2>/dev/null \
    || bad "${scope#"$repo"/}: no [Scope] section or not a session- prefix drop-in"
  has "$scope" CPUWeight=1000
  has "$scope" IOWeight=1000
  has "$scope" MemoryMin=256M
  has "$scope" MemoryLow=1G
  has "$scope" MemorySwapMax=0
  has "$scope" ManagedOOMPreference=avoid
  if grep -E '^(CPUQuota|MemoryMax|MemoryHigh|IOReadBandwidthMax|IOWriteBandwidthMax)=' \
      "$scope" >/dev/null; then
    bad "${scope#"$repo"/} caps the session"
  fi
  ok 'session tier: weights, floors, io delegation, oomd limits, OOM score'
  grep -Fqx 'Slice=session.slice' "$repo/os/usr/lib/systemd/user/tilo-app-host.service" \
    || bad 'tilo-app-host is not in session.slice'
  grep -Fqx 'Slice=session.slice' "$repo/os/usr/lib/systemd/user/tilo-fs-recipient.service" \
    || bad 'tilo-fs-recipient is not in session.slice'
  grep -Fqx 'Slice=background.slice' "$repo/os/usr/lib/systemd/user/tilo-fs-cloud.service" \
    || bad 'tilo-fs-cloud sync is not background'
  # Everything else nobody is waiting on: updates, catalogue and model
  # downloads, cache collection, and the runtime prefetch. A person's launch
  # runs in app.slice, which outweighs all of them.
  for unit in tilo-app-update tilo-cache-gc tilo-models tilo-public-content-refresh \
      tilo-runtime-resident; do
    grep -Fqx 'Slice=background.slice' "$repo/os/usr/lib/systemd/user/$unit.service" \
      || bad "$unit is not background work"
  done
  ok 'background work: updates, downloads and prefetch yield to the desktop'
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
  prop --user tilo-app-update.service Slice background.slice
  prop --user tilo-models.service Slice background.slice
  prop --system fs.service ManagedOOMPreference avoid
  prop --system "user@$(id -u).service" CPUWeight 500
  # The io controller must be delegated, or per-app IOWeight is inert.
  uid=$(id -u)
  controllers=$(cat "/sys/fs/cgroup/user.slice/user-$uid.slice/user@$uid.service/cgroup.controllers")
  case " $controllers " in *' io '*) ok 'io controller delegated to the user manager' ;;
    *) bad "io not delegated: $controllers" ;; esac
  # The compositor's login session scope, set by the image's prefix drop-in
  # (system/session-.scope.d/50-tilo-interactive.conf).
  shell_pid=$(pgrep -xo tilo-session || true)
  if [ -n "$shell_pid" ]; then
    scope=$(sed -n 's#.*/\(session-[0-9]*\.scope\)$#\1#p' "/proc/$shell_pid/cgroup")
    if [ -n "$scope" ]; then
      ok "the compositor runs in $scope"
      prop --system "$scope" CPUWeight 1000
      prop --system "$scope" IOWeight 1000
      prop --system "$scope" MemoryMin 268435456
      prop --system "$scope" MemoryLow 1073741824
      prop --system "$scope" MemorySwapMax 0
      prop --system "$scope" ManagedOOMPreference avoid
    else bad "the compositor is not in a logind session scope: $(cat "/proc/$shell_pid/cgroup")"
    fi
    # The compositor inherits the display manager's OOM score: never oomd's or
    # the OOM killer's choice.
    [ "$(cat "/proc/$shell_pid/oom_score_adj")" = -1000 ] \
      && ok 'the compositor is never the OOM killer'"'"'s choice' \
      || bad "compositor oom_score_adj is $(cat "/proc/$shell_pid/oom_score_adj"), not -1000"
  else bad 'no compositor running'; fi
  # Every app is a scope of its own, with a tier it moves between, and
  # OOMPolicy=continue keeps one OOM kill from stopping every app.
  slice="/sys/fs/cgroup/user.slice/user-$uid.slice/user@$uid.service/app.slice"
  apps=$(cd "$slice" && ls -d app-tilo-*.scope 2>/dev/null || true)
  [ -n "$apps" ] && ok "app scopes: $(echo $apps)" || bad 'no app-tilo-*.scope: launched apps got no cgroup of their own'
  for app in $apps; do
    prop --user "$slice/$app" OOMPolicy continue
    w=$(cat "$slice/$app/cpu.weight")
    case "$w" in 10000 | 800 | 50) ;;
      *) bad "$app has no tier (cpu.weight=$w)" ;; esac
    # A Tilo app is one OOM group; a host app is not delegated and keeps one
    # victim.
    oom_group=$(cat "$slice/$app/memory.oom.group")
    case "$oom_group" in 0 | 1) ;;
      *) bad "$app has no OOM grouping ($oom_group)" ;; esac
  done
  # IO weights need a scheduler that honours them.
  for dev in /sys/block/nvme*n*; do
    [ -e "$dev/queue/scheduler" ] || continue
    sched=$(cat "$dev/queue/scheduler")
    case "$sched" in *'[bfq]'*) ok "$(basename "$dev") io scheduler bfq" ;;
      *) bad "$(basename "$dev") scheduler is not bfq: $sched" ;; esac
  done
  systemctl is-active --quiet systemd-oomd && ok 'systemd-oomd active' || bad 'systemd-oomd inactive'
  grep -q 'SwapUsedLimit=80%' /etc/systemd/oomd.conf.d/50-tilo.conf 2>/dev/null \
    && ok 'oomd policy installed' || bad 'oomd policy missing'
fi
exit "$fail"
