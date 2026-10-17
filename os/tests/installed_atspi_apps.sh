#!/usr/bin/env bash
# Run the manifest-driven external AT-SPI acceptance scenarios in the logged-in
# installed image. This intentionally imports only session coordinates from the
# same user's systemd manager; SSH itself has no graphical environment.
#
# Test tooling, not part of the image: `os/dev/vm.sh test installed-atspi-apps.sh`
# copies this script and the client it runs (tools/atspi_external_client.py,
# named by TILO_ATSPI_CLIENT) from the checkout into a private directory in the
# guest.
set -euo pipefail

client="${TILO_ATSPI_CLIENT:?TILO_ATSPI_CLIENT must name the external AT-SPI client (os/dev/vm.sh test sets it)}"

for executable in busctl gdbus python3 systemctl; do
  command -v "$executable" >/dev/null || {
    echo "installed app AT-SPI smoke requires $executable" >&2
    exit 1
  }
done

# SSH normally omits this even when it connects as the graphical user. Wayland
# clients must use the same per-user runtime directory that owns the published
# compositor socket; do not infer it from a different login session.
user_runtime="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
export XDG_RUNTIME_DIR="$user_runtime"

if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" || -z "${WAYLAND_DISPLAY:-}" ]]; then
  manager_env=$(systemctl --user show-environment)
  for key in DBUS_SESSION_BUS_ADDRESS WAYLAND_DISPLAY; do
    value=$(printf '%s\n' "$manager_env" | sed -n "s/^${key}=//p" | head -n 1)
    [[ -n "$value" ]] && export "$key=$value"
  done
fi

[[ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ]] || {
  echo "graphical user D-Bus address is absent" >&2
  exit 1
}
[[ -n "${WAYLAND_DISPLAY:-}" ]] || {
  echo "Tilo Wayland display is absent" >&2
  exit 1
}
[[ -S "$user_runtime/$WAYLAND_DISPLAY" ]] || {
  echo "published Tilo Wayland socket is absent: $user_runtime/$WAYLAND_DISPLAY" >&2
  exit 1
}

exec python3 "$client" "$@"
