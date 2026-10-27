#!/usr/bin/env bash
# Live AT-SPI smoke for compositor-owned surfaces. Unlike an app-local native
# audit, this crosses the session accessibility bus and catches a missing
# provider adapter, a dead bus, or a tree invisible to external screen readers.
set -euo pipefail

mode=${1:-}
usage="usage: $0 {oobe|oobe-actions|oobe-copy-plain|oobe-copy-secret|oobe-nearby|oobe-nearby-done|oobe-ready|oobe-finish-focused|oobe-finished|oobe-text TEXT...|post-identity|clipboard-is TEXT|clipboard-keeps TEXT SECRET|copy-image|desktop|established|lock|auth|quick-settings}"
case "$mode" in
  oobe|oobe-actions|oobe-copy-plain|oobe-copy-secret|oobe-nearby|oobe-nearby-done|oobe-ready|oobe-finish-focused|oobe-finished|post-identity|copy-image|desktop|established|lock|auth|quick-settings) ;;
  oobe-text) (($# > 1)) || { echo "$usage" >&2; exit 2; } ;;
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
    # into an empty successful tree. The provider's refusal of a node that
    # just went away is that transaction, not an error worth printing.
    name=$(property "$path" Name 2>/dev/null) || return 1
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

# Status lines interpolate runtime values (a pairing code, an error detail),
# so these match a fragment of a node's spoken name rather than all of it.
node_containing() {
  local fragment=$1 path
  for path in "${!node_names[@]}"; do
    [[ ${node_names[$path]} == *"$fragment"* ]] && {
      printf '%s\n' "$path"
      return 0
    }
  done
  return 1
}

wait_for_fragments() {
  local missing fragment
  for _ in $(seq 1 400); do
    if collect_tree; then
      missing=""
      for fragment in "$@"; do
        node_containing "$fragment" >/dev/null || missing+=" '$fragment'"
      done
      [[ -z "$missing" ]] && return 0
    fi
    sleep 0.05
  done
  echo "AT-SPI $mode tree never exposed a name containing:$missing" >&2
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

# The node of the current tree that holds keyboard focus (AT-SPI State 12,
# see wait_for_focused), if any.
focused_node() {
  local path state
  for path in "${!node_names[@]}"; do
    state=$(gdbus call --address "$atspi_address" \
      --dest "$app_bus" \
      --object-path "$path" \
      --method org.a11y.atspi.Accessible.GetState 2>/dev/null) || continue
    if [[ $state =~ uint32[[:space:]]+([0-9]+) ]] && (( (BASH_REMATCH[1] & (1 << 12)) != 0 )); then
      printf '%s\n' "$path"
      return 0
    fi
  done
  return 1
}

wait_for_absent() {
  local expected=$1
  for _ in $(seq 1 400); do
    if collect_tree && ! node_named "$expected" >/dev/null; then
      return 0
    fi
    sleep 0.05
  done
  echo "AT-SPI $mode tree still exposes '$expected'" >&2
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
  wait_for_names "Use my existing Tilo identity" "Create a new identity"
  click_named "Use my existing Tilo identity"
  wait_for_names "Restore from a backup" "Back"
  click_named "Restore from a backup"
  wait_for_names "Identity backup file" "Back"
  focus_named "Identity backup file"
  set_text_named "Identity backup file" "tilo-atspi-edit-probe"
}

# From Intro (or the Connect branch) to the backup step, where the identity
# exists and the backup passphrase is a secret field. Each transition is made
# through org.a11y.atspi.Action, then observed back through the live AT-SPI
# tree. It catches a provider that advertises controls but drops
# ActionRequest messages before the compositor's widget dispatcher.
# The backup step's acknowledgement, as the shell says it
# (onboarding-backup-skip-acknowledgement in crates/session/shell/locales/en-US.ftl).
skip_backup_acknowledgement="I understand that without a recovery key or backup, losing this device means losing this identity."
oobe_to_backup() {
  if node_named "Back" >/dev/null && node_named "Identity backup file" >/dev/null; then
    click_named "Back"
    wait_for_names "Restore from a backup" "Back"
    click_named "Back"
  fi
  wait_for_names "Use my existing Tilo identity" "Create a new identity"
  click_named "Create a new identity"
  wait_for_names \
    "I understand that losing this device without a backup means losing this identity." \
    "Create local identity"
  click_named "I understand that losing this device without a backup means losing this identity."
  wait_for_names "Create local identity"
  click_named "Create local identity"
  wait_for_names \
    "$skip_backup_acknowledgement" \
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
  copy-image)
    # Another app copies a screenshot: a small PNG offered as image/png only,
    # the way a screenshot tool puts one on the clipboard.
    python3 -c '
import struct, sys, zlib
def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
rows = b"".join(b"\x00" + b"\xc8\x28\x28" * 16 for _ in range(12))
sys.stdout.buffer.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 16, 12, 8, 2, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))
' | WAYLAND_DISPLAY=$clipboard_socket wl-copy --type image/png >/dev/null 2>&1
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
      wait_for_names "Use my existing Tilo identity" "Create a new identity"
    fi
    if [[ $mode == oobe-actions ]]; then
      # No backup in a disposable release VM; say so explicitly, as a person
      # skipping it would.
      wait_for_names \
        "$skip_backup_acknowledgement" \
        "Continue without backup"
      click_named "$skip_backup_acknowledgement"
      wait_for_names "Continue without backup"
      click_named "Continue without backup"
      # The media codecs step appears only where the codec pack is offered. A new
      # identity ends on the welcome (a joined device's on "Your identity is ready").
      for _ in $(seq 1 400); do
        collect_tree || { sleep 0.05; continue; }
        node_named "Welcome to Tilo" >/dev/null && break
        if node_named "Not now" >/dev/null; then
          click_named "Not now"
          break
        fi
        sleep 0.05
      done
      wait_for_names "Welcome to Tilo"
    fi
    ;;
  oobe-nearby)
    # Use my existing Tilo identity -> Connect a nearby device, as a person
    # does; choosing it starts pairing. A retry after a failed attempt is
    # already on the pairing screen and asks again with Try again.
    for _ in $(seq 1 400); do
      collect_tree || { sleep 0.05; continue; }
      if node_named "Try again" >/dev/null; then
        click_named "Try again"
        exit 0
      fi
      if node_named "Restore from a backup" >/dev/null; then
        click_named "Connect a nearby device"
        # Pairing starts once the nearby screen is up.
        wait_for_fragments "waiting for your other device to approve it"
        exit 0
      fi
      if node_named "Use my existing Tilo identity" >/dev/null; then
        click_named "Use my existing Tilo identity"
      fi
      sleep 0.05
    done
    echo "AT-SPI $mode tree never offered nearby pairing" >&2
    printf '%s\n' "$names" >&2
    exit 1
    ;;
  oobe-nearby-done)
    # Joining an identity ends first use on its ready screen. The media codecs
    # step appears first only where the codec pack is offered.
    for _ in $(seq 1 1200); do
      collect_tree || { sleep 0.05; continue; }
      node_containing "identity is ready" >/dev/null && break
      if node_named "Not now" >/dev/null; then
        click_named "Not now"
      fi
      sleep 0.05
    done
    wait_for_fragments "identity is ready"
    if node_named "Try again" >/dev/null; then
      echo "AT-SPI $mode tree still offers to try pairing again" >&2
      exit 1
    fi
    ;;
  oobe-text)
    wait_for_fragments "${@:2}"
    ;;
  post-identity)
    # A session started after the identity exists shows the desktop, or the
    # first-use steps that follow the identity. It never asks for an identity
    # again and never lands on identity recovery.
    for _ in $(seq 1 1200); do
      collect_tree || { sleep 0.05; continue; }
      node_named "Tilo workspace" >/dev/null && break
      node_containing "identity is ready" >/dev/null && break
      node_containing "needs recovery" >/dev/null && break
      node_named "Use my existing Tilo identity" >/dev/null && break
      sleep 0.05
    done
    for refused in "Use my existing Tilo identity" "Create a new identity" "Create your local" "needs recovery"; do
      if node_containing "$refused" >/dev/null; then
        echo "AT-SPI $mode tree shows '$refused' after the identity was established" >&2
        printf '%s\n' "$names" >&2
        exit 1
      fi
    done
    node_named "Tilo workspace" >/dev/null || node_containing "identity is ready" >/dev/null || {
      echo "AT-SPI $mode tree shows neither the desktop nor the post-identity first-use step" >&2
      printf '%s\n' "$names" >&2
      exit 1
    }
    ;;
  oobe-ready)
    # The ready screen takes keyboard focus once it is on screen: before
    # that, keys still go to the step it replaced.
    wait_for_names "Continue to desktop"
    for _ in $(seq 1 400); do
      collect_tree && node_named "Continue to desktop" >/dev/null && focused_node >/dev/null && break
      sleep 0.05
    done
    focused_node >/dev/null || {
      echo "AT-SPI $mode tree: nothing on the ready screen holds keyboard focus" >&2
      printf '%s\n' "$names" >&2
      exit 1
    }
    ;;
  oobe-finish-focused)
    # Tab from where the ready screen put focus reaches its primary action.
    focused_name=""
    for _ in $(seq 1 400); do
      if collect_tree && focused=$(focused_node); then
        focused_name=${node_names[$focused]}
        [[ $focused_name == *"'Continue to desktop'"* ]] && break
      fi
      sleep 0.05
    done
    [[ $focused_name == *"'Continue to desktop'"* ]] || {
      echo "AT-SPI $mode: keyboard focus is on ${focused_name:-nothing}, not 'Continue to desktop'" >&2
      exit 1
    }
    ;;
  oobe-finished)
    wait_for_absent "Continue to desktop"
    ;;
  desktop)
    wait_for_names "Tilo workspace"
    ;;
  established)
    # A session for an identity this device already holds: the desktop, with
    # neither identity setup nor identity recovery in front of it.
    wait_for_names "Tilo workspace"
    for refused in "Use my existing Tilo identity" "Create a new identity" "Your existing identity needs recovery"; do
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
  quick-settings)
    # Open Quick Settings from the top bar and change the power profile
    # there, then confirm the change in the power daemon itself: a panel
    # that opens but whose controls never reach the system fails here.
    # Only the profiles the daemon offers may be chosen, so every press
    # must land on one of them.
    command -v powerprofilesctl >/dev/null
    before=$(powerprofilesctl get)
    offered=$(powerprofilesctl list | sed -n 's/^[* ] *\([a-z-]*\):$/\1/p' | wc -l)
    ((offered >= 2)) || { echo "the power daemon offers only $offered profile(s)" >&2; exit 1; }
    wait_for_names "Open quick settings"
    click_named "Open quick settings"
    wait_for_names "Change power profile"
    changed=""
    for press in $(seq 1 "$offered"); do
      current=$(powerprofilesctl get)
      # The panel learns which profiles exist from a read that starts
      # when it opens; a press before that lands may be refused once.
      # A panel that asks for a profile the daemon lacks keeps failing.
      for attempt in 1 2 3; do
        click_named "Change power profile"
        for _ in $(seq 1 60); do
          now=$(powerprofilesctl get)
          [[ $now != "$current" ]] && break
          sleep 0.05
        done
        [[ $now != "$current" ]] && break
        collect_tree
      done
      [[ $now != "$current" ]] || {
        echo "press $press on the power profile left the daemon at '$current'" >&2
        exit 1
      }
      [[ -n $changed ]] || changed=$now
      collect_tree
    done
    # Pressing through every offered profile comes back to where it began.
    [[ $(powerprofilesctl get) == "$before" ]] || {
      echo "cycling the offered profiles did not return to '$before'" >&2
      exit 1
    }
    echo "Quick Settings changed the power profile from '$before' to '$changed' and back"
    click_named "Open quick settings"
    wait_for_absent "Change power profile"
    ;;
  auth)
    # Ask polkit for an administrator action on behalf of the graphical
    # session, exactly as pkexec from a terminal in it would. polkit hands
    # the request to the agent that session registered — the compositor —
    # which draws the system authentication sheet. The sheet must be fully
    # operable by an external reader: password text that takes focus and
    # input without publishing it, and a Cancel that answers polkit.
    session=$(loginctl show-user "$(id -un)" -p Display --value)
    [[ -n "$session" ]] || { echo "no graphical login session" >&2; exit 1; }
    # pkexec names the process asking, never the session: polkit's rules
    # engine reads a subject as a process or a bus name only, and polkitd
    # aborts on a unix-session subject. The asker here is one of this
    # user's processes in the graphical session, which polkit maps to that
    # session and so to the agent the compositor registered for it.
    scope="/sys/fs/cgroup/user.slice/user-$(id -u).slice/session-$session.scope/cgroup.procs"
    asker=""
    while read -r pid; do
      [[ $(stat -c %u "/proc/$pid" 2>/dev/null) == "$(id -u)" ]] && { asker=$pid; break; }
    done <"$scope"
    [[ -n "$asker" ]] || { echo "no process of $(id -un) in session $session ($scope)" >&2; exit 1; }
    stat_line=$(<"/proc/$asker/stat")
    read -r -a stat_fields <<<"${stat_line##*) }"
    # Field 22 of stat(5), counted after the command name's closing paren.
    started=${stat_fields[19]}
    answer=$(mktemp)
    checker=""
    trap 'rm -f "$answer"; [[ -z "$checker" ]] || kill "$checker" 2>/dev/null || true' EXIT
    gdbus call --system --timeout 120 \
      --dest org.freedesktop.PolicyKit1 \
      --object-path /org/freedesktop/PolicyKit1/Authority \
      --method org.freedesktop.PolicyKit1.Authority.CheckAuthorization \
      "('unix-process', {'pid': <uint32 $asker>, 'start-time': <uint64 $started>, 'uid': <int32 $(id -u)>})" \
      org.freedesktop.policykit.exec '{}' 1 '' >"$answer" 2>&1 &
    checker=$!
    wait_for_names "Administrator access" "Password" "Authenticate" "Cancel" || {
      echo "polkit answered the request with:" >&2
      cat "$answer" >&2
      exit 1
    }
    assert_password_role "Password"
    focus_named "Password"
    path=$(editable_node_named "Password")
    result=$(gdbus call --address "$atspi_address" \
      --dest "$app_bus" \
      --object-path "$path" \
      --method org.a11y.atspi.EditableText.SetTextContents "not-the-password")
    [[ ${result//[[:space:]]/} == '(true,)' ]] || {
      echo "AT-SPI text update on the password was rejected: $result" >&2
      exit 1
    }
    text=$(gdbus call --address "$atspi_address" \
      --dest "$app_bus" \
      --object-path "$path" \
      --method org.a11y.atspi.Text.GetText -- 0 -1 2>&1 || true)
    [[ $text != *not-the-password* ]] || {
      echo "the password field published what was typed: $text" >&2
      exit 1
    }
    click_named "Cancel"
    wait "$checker" || true
    checker=""
    grep -q '((false,' "$answer" || {
      echo "polkit did not see the sheet dismissed:" >&2
      cat "$answer" >&2
      exit 1
    }
    wait_for_absent "Administrator access"
    ;;
esac

printf 'native AT-SPI smoke passed: %s compositor surface is externally readable.\n' "$mode"
