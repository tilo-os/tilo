#!/usr/bin/env bash
# Run in a booted image, as the account that logged in through the greeter,
# once its desktop is up. A greetd login hands the session the user's SELinux
# context and a logind session scope: the compositor and everything it starts
# run in the user's domain, never greetd's xdm_t; the compositor carries the
# user's login uid inside its session's scope; every host app the launcher
# starts gets its own app scope from the user manager. During the session's
# first minute nothing it runs is denied access to the home directory.
set -euo pipefail

fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

uid=$(id -u)
# The user's own context as pam_selinux gives it to this login: the graphical
# session must have the same user, role and type.
user_context=$(id -Z)
user_domain=$(printf '%s' "$user_context" | cut -d: -f1-3)
case "$user_domain" in
  *:xdm_t|*:kernel_t|*:unlabeled_t|*:init_t|'')
    echo "this login itself runs as $user_context, not in a user domain" >&2
    exit 1
    ;;
esac

# A process can exit between listing and reading it; it then has no answer.
domain_of() { { tr -d '\0' <"/proc/$1/attr/current" | cut -d: -f1-3; } 2>/dev/null || true; }
uid_of() { awk '/^Uid:/ { print $2 }' "/proc/$1/status" 2>/dev/null || true; }
comm_of() { cat "/proc/$1/comm" 2>/dev/null || true; }

# --- the greetd session ---------------------------------------------------
session=
for id in $(loginctl list-sessions --no-legend | awk -v uid="$uid" '$2 == uid { print $1 }'); do
  if [ "$(loginctl show-session "$id" -p Service --value)" = greetd ] \
     && [ "$(loginctl show-session "$id" -p Type --value)" = wayland ] \
     && [ "$(loginctl show-session "$id" -p State --value)" = active ]; then
    session=$id
  fi
done
if [ -z "$session" ]; then
  loginctl list-sessions --no-pager >&2 || true
  echo 'no active greetd wayland session for this account' >&2
  exit 1
fi
scope=$(loginctl show-session "$session" -p Scope --value)
if [ "$scope" = "session-$session.scope" ]; then
  ok "the greetd login is logind session $session in $scope"
else
  bad "logind session $session has scope '$scope'"
fi

# The first minute of the session, for the audit check below.
started_us=$(loginctl show-session "$session" -p TimestampMonotonic --value)
read -r uptime_s _ </proc/uptime
age_s=$(( ${uptime_s%.*} - started_us / 1000000 ))
if [ "$age_s" -lt 60 ]; then
  sleep $(( 60 - age_s ))
fi

session_procs="/sys/fs/cgroup/user.slice/user-$uid.slice/$scope/cgroup.procs"
shell_pid=
if [ -r "$session_procs" ]; then
  while read -r pid; do
    if [ "$(comm_of "$pid")" = shell ] && [ "$(uid_of "$pid")" = "$uid" ]; then
      shell_pid=$pid
    fi
  done <"$session_procs"
fi
if [ -n "$shell_pid" ]; then
  ok "the compositor (pid $shell_pid) runs in $scope"
  shell_domain=$(domain_of "$shell_pid")
  if [ "$shell_domain" = "$user_domain" ]; then
    ok "the compositor runs as $shell_domain"
  else
    bad "the compositor runs as $shell_domain, not the user's $user_domain"
  fi
  loginuid=$(cat "/proc/$shell_pid/loginuid")
  if [ "$loginuid" = "$uid" ]; then
    ok 'the compositor carries the login uid'
  else
    bad "the compositor's login uid is $loginuid, not $uid"
  fi
else
  bad "no compositor of uid $uid in $scope"
fi

# --- host apps ------------------------------------------------------------
# The session starts its XDG autostart entries through tilo-launcher run-host
# as soon as the desktop has a workspace; each must get its own cgroup in the
# compositor's delegated app tree.
slice="/sys/fs/cgroup/user.slice/user-$uid.slice/user@$uid.service/app.slice"
app_cgroups=
for _ in $(seq 1 30); do
  app_cgroups=$(ls -d "$slice"/tilo-apps-*.scope/app-* 2>/dev/null || true)
  [ -n "$app_cgroups" ] && break
  sleep 2
done
if [ -n "$app_cgroups" ]; then
  ok "the launcher started host apps in their own cgroups: $(for c in $app_cgroups; do printf '%s ' "${c##*/}"; done)"
else
  bad 'no app cgroup in a tilo-apps-*.scope: launched apps got no cgroup of their own'
fi
for cgroup in $app_cgroups; do
  [ -r "$cgroup/cgroup.procs" ] || continue
  while read -r pid; do
    domain=$(domain_of "$pid")
    if [ -n "$domain" ] && [ "$domain" != "$user_domain" ]; then
      bad "$(comm_of "$pid") (pid $pid) in ${cgroup##*/} runs as $domain"
    fi
  done <"$cgroup/cgroup.procs"
done

# Nothing of this account runs as the display manager: not the session, not
# an app it launched, not a daemon PAM started for it.
xdm=0
for status in /proc/[0-9]*/status; do
  pid=${status#/proc/}; pid=${pid%/status}
  if [ "$(uid_of "$pid")" != "$uid" ]; then
    continue
  fi
  case "$(domain_of "$pid")" in
    *:xdm_t)
      bad "$(comm_of "$pid") (pid $pid) of uid $uid runs as xdm_t"
      xdm=1
      ;;
  esac
done
[ "$xdm" = 1 ] || ok "no process of uid $uid runs as xdm_t"

log="${XDG_STATE_HOME:-$HOME/.local/state}/tilo/session-logs/latest.log"
if [ -r "$log" ] && grep -F 'runs outside a session scope' "$log" >&2; then
  bad 'the launcher could not give an app its scope'
else
  ok 'the launcher gave every app its scope'
fi

# --- audit ------------------------------------------------------------------
# The kernel's AVC records and the user manager's USER_AVC both reach the
# system journal, which this account (wheel) reads.
if [ -z "$(journalctl -b -q --no-pager -n 1 _PID=1 2>/dev/null)" ]; then
  bad 'the system journal is unreadable, so denials cannot be checked'
fi
# Denials of the session's own processes, in the user's domain or the display
# manager's: system services have their own policy and their own tests.
session_types="(${user_domain##*:}|xdm_t)"
home_types='(user_home_dir_t|user_home_t|home_root_t|[a-z_]*_home_t|xdg_[a-z_]*_t)'
denials=$(journalctl -b -q --no-pager -o cat 2>/dev/null \
  | grep -E "avc: +denied .*scontext=[^ ]*:$session_types:[^ ]* tcontext=[^ ]*:object_r:$home_types:|avc: +denied .*method_start_transient_unit" \
  | sort | uniq -c | sort -rn | head -20 || true)
if [ -z "$denials" ]; then
  ok 'no home-directory or app-scope denials in the session'"'"'s first minute'
else
  printf '%s\n' "$denials" >&2
  bad 'the session was denied home-directory access or app scopes'
fi

exit "$fail"
