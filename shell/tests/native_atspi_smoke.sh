#!/usr/bin/env bash
# Live AT-SPI smoke for compositor-owned surfaces. Unlike an app-local native
# audit, this crosses the real session accessibility bus and catches a missing
# provider adapter, a dead bus, or a tree invisible to external screen readers.
set -euo pipefail

mode=${1:-}
case "$mode" in
  oobe|desktop|lock) ;;
  *) echo "usage: $0 {oobe|desktop|lock}" >&2; exit 2 ;;
esac

command -v busctl >/dev/null
command -v gdbus >/dev/null

# An SSH release-test process does not inherit the graphical environment. Read
# only the same user's bus address from its systemd manager; never guess or use
# another session's bus.
if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]]; then
  DBUS_SESSION_BUS_ADDRESS=$(systemctl --user show-environment \
    | sed -n 's/^DBUS_SESSION_BUS_ADDRESS=//p' | head -n 1)
  export DBUS_SESSION_BUS_ADDRESS
fi
[[ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ]] || {
  echo "graphical user D-Bus address is absent" >&2
  exit 1
}

shell_pid=${QOS_SHELL_PID:-}
if [[ -z "$shell_pid" ]]; then
  shell_pid=$(pgrep -o -f '^/usr/bin/shell --backend drm( |$)' || true)
fi
[[ -n "$shell_pid" ]] || { echo "qOS compositor process is absent" >&2; exit 1; }

atspi_address=$(
  gdbus call --session \
    --dest org.a11y.Bus \
    --object-path /org/a11y/bus \
    --method org.a11y.Bus.GetAddress \
    | sed -E "s/^\('([^']+)'.*/\1/"
)
[[ -n "$atspi_address" ]] || { echo "AT-SPI bus address is absent" >&2; exit 1; }

app_bus=""
for _ in $(seq 1 200); do
  app_bus=$(busctl --address="$atspi_address" list --no-pager \
    | awk -v pid="$shell_pid" '$2 == pid { print $1; exit }')
  [[ -n "$app_bus" ]] && break
  kill -0 "$shell_pid" 2>/dev/null || break
  sleep 0.05
done
[[ -n "$app_bus" ]] || {
  echo "qOS compositor did not register on the AT-SPI bus" >&2
  exit 1
}

property() {
  local object_path=$1 property_name=$2
  gdbus call --address "$atspi_address" \
    --dest "$app_bus" \
    --object-path "$object_path" \
    --method org.freedesktop.DBus.Properties.Get \
    org.a11y.atspi.Accessible "$property_name"
}

children() {
  gdbus call --address "$atspi_address" \
    --dest "$app_bus" \
    --object-path "$1" \
    --method org.a11y.atspi.Accessible.GetChildren
}

# Bound the walk so a corrupt cyclic provider fails rather than hanging the
# release. Names are the user-facing contract an external reader consumes.
queue=(/org/a11y/atspi/accessible/root)
declare -A seen=()
names=""
for _ in $(seq 1 256); do
  ((${#queue[@]} > 0)) || break
  path=${queue[0]}
  queue=("${queue[@]:1}")
  [[ -z "${seen[$path]:-}" ]] || continue
  seen[$path]=1
  names+="$(property "$path" Name 2>/dev/null || true)"$'\n'
  while IFS= read -r child; do
    [[ -n "$child" && -z "${seen[$child]:-}" ]] && queue+=("$child")
  done < <(children "$path" 2>/dev/null \
    | grep -Eo '/org/a11y/atspi/accessible/[0-9/]+' || true)
done

require_name() {
  local expected=$1
  [[ "$names" == *"$expected"* ]] || {
    echo "AT-SPI $mode tree is missing '$expected'" >&2
    printf '%s\n' "$names" >&2
    exit 1
  }
}

case "$mode" in
  oobe)
    require_name "Use existing"
    require_name "Create identity"
    ;;
  desktop)
    require_name "qOS workspace"
    ;;
  lock)
    require_name "qOS lock screen"
    require_name "Password"
    require_name "Type your password and press Enter"
    ;;
esac

printf 'native AT-SPI smoke passed: %s compositor surface is externally readable.\n' "$mode"
