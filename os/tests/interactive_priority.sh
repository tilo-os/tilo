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
  # The app tree hands each app every controller its tier and accounting use.
  has "$etc/system/user@.service.d/50-tilo-interactive.conf" 'Delegate=cpu io memory pids'
  has "$etc/system/user.slice.d/50-tilo-interactive.conf" CPUWeight=500
  has "$etc/system/user-.slice.d/50-tilo-interactive.conf" MemoryMin=256M
  has "$etc/system/fs.service.d/30-tilo-priority.conf" ManagedOOMPreference=avoid
  has "$etc/oomd.conf.d/50-tilo.conf" SwapUsedLimitPercent=80%
  # The kernel's OOM killer never chooses the session (the compositor greetd
  # starts inherits it); apps leave it before they start.
  has "$etc/system/greetd.service.d/50-tilo-oom.conf" OOMScoreAdjust=-1000
  ok 'session tier: weights, floors, io delegation, oomd limits, OOM score'
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
    [ "$(cat "/proc/$shell_pid/oom_score_adj")" = -1000 ] \
      && ok 'the compositor is never the OOM killer'"'"'s choice' \
      || bad "compositor oom_score_adj is $(cat "/proc/$shell_pid/oom_score_adj"), not -1000"
  else bad 'no compositor running'; fi
  # Every app is a cgroup of the session's delegated tree, with a tier, and
  # the tree's scope keeps one OOM kill from stopping every app.
  slice="/sys/fs/cgroup/user.slice/user-$uid.slice/user@$uid.service/app.slice"
  trees=$(cd "$slice" && ls -d tilo-apps-*.scope 2>/dev/null || true)
  [ -n "$trees" ] && ok "app trees: $(echo $trees)" || bad 'no tilo-apps-*.scope: the app spawner did not get its tree'
  boosted=0
  for tree in $trees; do
    prop --user "$tree" Delegate yes
    prop --user "$tree" OOMPolicy continue
    for app in "$slice/$tree"/app-*; do
      [ -d "$app" ] || continue
      w=$(cat "$app/cpu.weight")
      case "$w" in 800) boosted=$((boosted + 1)) ;; 50) ;;
        *) bad "${app##*/} has no foreground/background tier (cpu.weight=$w)" ;; esac
      # The app's own cgroup holds no process and is no OOM group: its main
      # leaf is (a Tilo app, 1; a host app, 0) and each job is its own (1).
      [ "$(cat "$app/memory.oom.group")" = 0 ] || bad "${app##*/} is one OOM group with its jobs"
      [ -z "$(cat "$app/cgroup.procs")" ] || bad "${app##*/} holds processes outside its leaves"
      [ -d "$app/main" ] || bad "${app##*/} has no main leaf"
      for job in "$app"/job-*; do
        [ -d "$job" ] || continue
        [ "$(cat "$job/memory.oom.group")" = 1 ] || bad "${job#"$slice"/} is no OOM group of its own"
      done
    done
  done
  # Exactly one app is boosted: the focused one.
  [ "$boosted" -le 1 ] && ok "at most one app is boosted ($boosted)" || bad "$boosted apps boosted"
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
