#!/usr/bin/env bash
# Live AT-SPI smoke for compositor-owned surfaces. Unlike an app-local native
# audit, this crosses the session accessibility bus and catches a missing
# provider adapter, a dead bus, or a tree invisible to external screen readers.
set -euo pipefail

mode=${1:-}
usage="usage: $0 {oobe|oobe-actions|oobe-copy-plain|oobe-copy-secret|clipboard-is TEXT|clipboard-keeps TEXT SECRET|desktop|established|lock}"
case "$mode" in
  oobe|oobe-actions|oobe-copy-plain|oobe-copy-secret|desktop|established|lock) ;;
  clipboard-is) (($# == 2)) || { echo "$usage" >&2; exit 2; } ;;
  clipboard-keeps) (($# == 3)) || { echo "$usage" >&2; exit 2; } ;;
  *) echo "$usage" >&2; exit 2 ;;
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

# Turn accessibility on the way a screen reader does: AccessKit providers join
# the AT-SPI bus only once org.a11y.Status.IsEnabled is true.
busctl --user set-property org.a11y.Bus /org/a11y/bus org.a11y.Status IsEnabled b true

# Find the compositor through the portal backend name only it owns on the
# session bus, not through /proc: it turns non-dumpable once it holds identity
# keys, and a hardened /proc hides such a process even from its own user.
shell_pid=${TILO_SHELL_PID:-}
for _ in $(seq 1 100); do
  [[ -z "$shell_pid" ]] || break
  shell_pid=$(busctl --user call org.freedesktop.DBus /org/freedesktop/DBus \
    org.freedesktop.DBus GetConnectionUnixProcessID s \
    org.freedesktop.impl.portal.desktop.tilo 2>/dev/null | awk '$1 == "u" { print $2 }')
  [[ -n "$shell_pid" ]] || sleep 0.1
done
[[ -n "$shell_pid" ]] || { echo "Tilo compositor process is absent" >&2; exit 1; }

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
  echo "Tilo compositor did not register on the AT-SPI bus" >&2
  echo "shell pid: $shell_pid" >&2
  busctl --address="$atspi_address" list --no-pager >&2 || true
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
# release. Keep each node's AT-SPI object path: action validation must
# address the same externally-exported node a screen reader sees, not an
# in-process mirror or a synthetic test bus.
declare -A node_names=()
names=""

collect_tree() {
  local path child name
  local -a queue=(/org/a11y/atspi/accessible/root)
  local -A seen=()
  node_names=()
  names=""
  for _ in $(seq 1 256); do
    ((${#queue[@]} > 0)) || break
    path=${queue[0]}
    queue=("${queue[@]:1}")
    [[ -z "${seen[$path]:-}" ]] || continue
    seen[$path]=1
    # A tree can be replaced between GetChildren and Get. Treat that as an
    # in-flight transaction and retry the whole bounded walk; never turn it
    # into an empty successful tree.
    name=$(property "$path" Name) || return 1
    node_names["$path"]=$name
    names+="$name"$'\n'
    while IFS= read -r child; do
      [[ -n "$child" && -z "${seen[$child]:-}" ]] && queue+=("$child")
    done < <(children "$path" \
      | grep -Eo '/org/a11y/atspi/accessible/[0-9/]+' || true)
  done
  ((${#node_names[@]} > 0))
}

# One complete walk, retried while a tree transaction is in flight.
collect_settled_tree() {
  for _ in $(seq 1 400); do
    collect_tree && return 0
    sleep 0.05
  done
  echo "AT-SPI $mode tree never settled" >&2
  return 1
}

node_named() {
  local expected=$1 path needle
  needle="'$expected'"
  for path in "${!node_names[@]}"; do
    [[ ${node_names[$path]} == *"$needle"* ]] && {
      printf '%s\n' "$path"
      return 0
    }
  done
  return 1
}

wait_for_names() {
  local missing expected attempt
  for attempt in $(seq 1 400); do
    if collect_tree; then
      missing=""
      for expected in "$@"; do
        node_named "$expected" >/dev/null || missing+=" $expected"
      done
      [[ -z "$missing" ]] && return 0
    fi
    sleep 0.05
  done
  echo "AT-SPI $mode tree never exposed:$missing" >&2
  printf '%s\n' "$names" >&2
  return 1
}

click_named() {
  local expected=$1 path candidate needle actions result
  needle="'$expected'"
  # A labelled control can have a sibling text node with the same spoken
  # label. Resolve the actionable one from the live AT-SPI interfaces rather
  # than relying on traversal order or an implementation-specific path.
  for candidate in "${!node_names[@]}"; do
    [[ ${node_names[$candidate]} == *"$needle"* ]] || continue
    actions=$(gdbus call --address "$atspi_address" \
      --dest "$app_bus" \
      --object-path "$candidate" \
      --method org.a11y.atspi.Action.GetActions 2>/dev/null) || continue
    if [[ $actions == *"('click',"* ]]; then
      path=$candidate
      break
    fi
  done
  [[ -n ${path:-} ]] || {
    echo "AT-SPI $mode tree has no click-capable node named '$expected'" >&2
    return 1
  }
  result=$(gdbus call --address "$atspi_address" \
    --dest "$app_bus" \
    --object-path "$path" \
    --method org.a11y.atspi.Action.DoAction 0)
  [[ ${result//[[:space:]]/} == '(true,)' ]] || {
    echo "AT-SPI click on '$expected' was rejected: $result" >&2
    return 1
  }
}

editable_node_named() {
  local expected=$1 candidate needle interfaces
  needle="'$expected'"
  for candidate in "${!node_names[@]}"; do
    [[ ${node_names[$candidate]} == *"$needle"* ]] || continue
    # Interfaces are a method on Accessible, not a property.
    interfaces=$(gdbus call --address "$atspi_address" \
      --dest "$app_bus" \
      --object-path "$candidate" \
      --method org.a11y.atspi.Accessible.GetInterfaces 2>/dev/null) || continue
    if [[ $interfaces == *"org.a11y.atspi.EditableText"* \
      && $interfaces == *"org.a11y.atspi.Text"* ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

# AT-SPI exposes focus as an action on Component, rather than through the
# Action interface used by buttons.  Verify both halves: the provider accepts
# the request and the following live tree transaction marks that exact object
# focused.  This catches an adapter which exports names/actions but never
# forwards focus requests into the compositor's widget dispatcher.
focus_named() {
  local expected=$1 path result
  path=$(editable_node_named "$expected") || {
    echo "AT-SPI $mode tree has no editable node named '$expected' to focus" >&2
    return 1
  }
  result=$(gdbus call --address "$atspi_address" \
    --dest "$app_bus" \
    --object-path "$path" \
    --method org.a11y.atspi.Component.GrabFocus)
  [[ ${result//[[:space:]]/} == '(true,)' ]] || {
    echo "AT-SPI focus on '$expected' was rejected: $result" >&2
    return 1
  }
  wait_for_focused "$path" "$expected"
}

# `Focused` is AT-SPI State 12, represented in the first uint32 word returned
# by Accessible.GetState.  Keep the wire-level assertion here instead of
# trusting a successful GrabFocus reply: requests are asynchronous and a
# provider may acknowledge one while dropping it before the next tree update.
wait_for_focused() {
  local path=$1 expected=$2 state word
  for _ in $(seq 1 400); do
    state=$(gdbus call --address "$atspi_address" \
      --dest "$app_bus" \
      --object-path "$path" \
      --method org.a11y.atspi.Accessible.GetState 2>/dev/null) || {
      sleep 0.05
      continue
    }
    if [[ $state =~ uint32[[:space:]]+([0-9]+) ]]; then
      word=${BASH_REMATCH[1]}
      (( (word & (1 << 12)) != 0 )) && return 0
    fi
    sleep 0.05
  done
  echo "AT-SPI focus on '$expected' never appeared in Accessible.GetState" >&2
  return 1
}

# Text fields use EditableText.SetTextContents, then expose their live value
# through the AT-SPI Text interface.  This is intentionally a non-secret
# OOBE field: it proves the screen-reader editing path without ever reading a
# password or changing the identity flow's credentials.
set_text_named() {
  local expected=$1 value=$2 path result
  path=$(editable_node_named "$expected") || {
    echo "AT-SPI $mode tree has no text field named '$expected'" >&2
    return 1
  }
  result=$(gdbus call --address "$atspi_address" \
    --dest "$app_bus" \
    --object-path "$path" \
    --method org.a11y.atspi.EditableText.SetTextContents "$value")
  [[ ${result//[[:space:]]/} == '(true,)' ]] || {
    echo "AT-SPI text update on '$expected' was rejected: $result" >&2
    return 1
  }
  wait_for_text "$path" "$expected" "$value"
}

wait_for_text() {
  local path=$1 expected=$2 value=$3 text
  for _ in $(seq 1 400); do
    text=$(gdbus call --address "$atspi_address" \
      --dest "$app_bus" \
      --object-path "$path" \
      --method org.a11y.atspi.Text.GetText -- 0 -1 2>/dev/null) || {
      sleep 0.05
      continue
    }
    [[ $text == *"'$value'"* ]] && return 0
    sleep 0.05
  done
  echo "AT-SPI value for '$expected' never reflected '$value'" >&2
  return 1
}

# A lock field must be identified as a password to the actual AT-SPI client,
# not merely be a TextInput whose value happens to be blank. This platform
# role tells readers and braille displays to suppress credential echo while
# preserving ordinary editable-text navigation.
assert_password_role() {
  local expected=$1 path role
  path=$(editable_node_named "$expected") || {
    echo "AT-SPI $mode tree has no editable password node named '$expected'" >&2
    return 1
  }
  role=$(gdbus call --address "$atspi_address" \
    --dest "$app_bus" \
    --object-path "$path" \
    --method org.a11y.atspi.Accessible.GetLocalizedRoleName)
  [[ $role == *"password text"* ]] || {
    echo "AT-SPI '$expected' is not exposed as password text: $role" >&2
    return 1
  }
}

# The session's clipboard as a clipboard manager sees it: through the
# compositor's privileged socket, where data-control is offered. A sentinel
# put there first tells "nothing was copied" apart from an empty clipboard.
clipboard_socket="tilo-privileged-$shell_pid"
clipboard_seed() {
  WAYLAND_DISPLAY=$clipboard_socket wl-copy -- "$1" </dev/null >/dev/null 2>&1
}
clipboard_read() {
  WAYLAND_DISPLAY=$clipboard_socket wl-paste "$@" --no-newline 2>/dev/null || true
}
clipboard_sentinel=tilo-clipboard-sentinel

# Walk the disposable Connect branch to its non-secret field and focus it.
# Proves the complete external-reader contract before the irreversible
# identity choice: focus and write the field solely over AT-SPI and observe
# the value through that same bus.
oobe_to_connect_field() {
  wait_for_names "Use existing" "Create identity"
  click_named "Use existing"
  wait_for_names "Identity backup file" "Back"
  focus_named "Identity backup file"
  set_text_named "Identity backup file" "tilo-atspi-edit-probe"
}

# From Intro (or the Connect branch) to the backup step, where the identity
# exists and the backup passphrase is a secret field. Each transition is made
# through org.a11y.atspi.Action, then observed back through the live AT-SPI
# tree. It catches a provider that advertises controls but drops
# ActionRequest messages before the compositor's widget dispatcher.
oobe_to_backup() {
  if node_named "Back" >/dev/null && node_named "Identity backup file" >/dev/null; then
    click_named "Back"
  fi
  wait_for_names "Use existing" "Create identity"
  click_named "Create identity"
  wait_for_names \
    "I understand that losing this device without a backup means losing this identity." \
    "Create local identity"
  click_named "I understand that losing this device without a backup means losing this identity."
  wait_for_names "Create local identity"
  click_named "Create local identity"
  wait_for_names \
    "I understand that without an encrypted backup, losing this device means losing this identity." \
    "Continue without backup"
}

case "$mode" in
  oobe-copy-plain)
    # The control for `oobe-copy-secret`: keys typed and copied in an
    # ordinary chrome field do reach the clipboard this script reads.
    oobe_to_connect_field
    set_text_named "Identity backup file" ""
    clipboard_seed "$clipboard_sentinel"
    ;;
  oobe-copy-secret)
    wait_for_names "Identity backup file" "Back"
    oobe_to_backup
    focus_named "New backup passphrase"
    clipboard_seed "$clipboard_sentinel"
    ;;
  clipboard-is)
    for _ in $(seq 1 100); do
      [[ $(clipboard_read) == "$2" ]] && break
      sleep 0.05
    done
    copied=$(clipboard_read)
    [[ $copied == "$2" ]] || {
      echo "the clipboard holds '$copied', not the copied '$2'" >&2
      exit 1
    }
    ;;
  clipboard-keeps)
    # Give a copy that should not have happened time to land first.
    sleep 1
    copied=$(clipboard_read)
    primary=$(clipboard_read --primary)
    [[ $copied != *"$3"* && $primary != *"$3"* ]] || {
      echo "a password field's value reached the clipboard" >&2
      exit 1
    }
    [[ $copied == "$2" ]] || {
      echo "the clipboard changed while copying from a password field: '$copied'" >&2
      exit 1
    }
    ;;
  oobe|oobe-actions)
    collect_settled_tree
    if [[ $mode == oobe-actions ]] && ! node_named "Continue without backup" >/dev/null; then
      oobe_to_connect_field
      oobe_to_backup
    elif [[ $mode == oobe ]]; then
      wait_for_names "Use existing" "Create identity"
    fi
    if [[ $mode == oobe-actions ]]; then
      # No backup in a disposable release VM; say so explicitly, as a person
      # skipping it would.
      wait_for_names \
        "I understand that without an encrypted backup, losing this device means losing this identity." \
        "Continue without backup"
      click_named "I understand that without an encrypted backup, losing this device means losing this identity."
      wait_for_names "Continue without backup"
      click_named "Continue without backup"
      # The media codecs step appears only where the codec pack is offered.
      for _ in $(seq 1 400); do
        collect_tree || { sleep 0.05; continue; }
        node_named "Your identity is ready" >/dev/null && break
        if node_named "Not now" >/dev/null; then
          click_named "Not now"
          break
        fi
        sleep 0.05
      done
      wait_for_names "Your identity is ready"
    fi
    ;;
  desktop)
    wait_for_names "Tilo workspace"
    ;;
  established)
    # A session for an identity this device already holds: the desktop, with
    # neither identity setup nor identity recovery in front of it.
    wait_for_names "Tilo workspace"
    for refused in "Use existing" "Create identity" "Your existing identity needs recovery"; do
      if node_named "$refused" >/dev/null; then
        echo "AT-SPI $mode tree shows '$refused' for an established identity" >&2
        printf '%s\n' "$names" >&2
        exit 1
      fi
    done
    ;;
  lock)
    wait_for_names "Tilo lock screen" "Password" "Type your password and press Enter"
    assert_password_role "Password"
    ;;
esac

printf 'native AT-SPI smoke passed: %s compositor surface is externally readable.\n' "$mode"
