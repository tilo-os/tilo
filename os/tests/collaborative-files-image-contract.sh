#!/usr/bin/env bash
# Fail-closed contract for the collaborative Files stack shipped by qOS.
#
#   OS_ROOT=/path/to/rootfs ... image   validate an assembled image tree
#   ... runtime                        additionally validate the installed,
#                                      booted system and logged-in user session
#
# The image mode runs during os/Containerfile itself.  Runtime mode is driven
# over SSH by build-image.yml after bootc installation and graphical login.
# It intentionally does not turn missing FUSE/session facilities into skips.
set -euo pipefail

mode="${1:-image}"
case "$mode" in
  image|runtime) ;;
  *) echo "usage: $0 [image|runtime]" >&2; exit 2 ;;
esac

root="${OS_ROOT:-/}"
root="${root%/}"
[ -n "$root" ] || root=/

at() {
  if [ "$root" = / ]; then printf '/%s\n' "${1#/}"; else printf '%s/%s\n' "$root" "${1#/}"; fi
}
die() { echo "collaborative-files-image-contract: FAIL: $*" >&2; exit 1; }
require_file() { [ -f "$(at "$1")" ] || die "missing $1"; }
require_exec() { [ -x "$(at "$1")" ] || die "missing or non-executable $1"; }
require_link() { [ -L "$(at "$1")" ] || die "$1 is not enabled"; }
require_mask() {
  [ "$(readlink "$(at "$1")" 2>/dev/null)" = /dev/null ] \
    || die "$1 is not masked"
}
require_line() {
  local file="$1" line="$2"
  grep -Fqx -- "$line" "$(at "$file")" || die "$file lacks exact line: $line"
}
require_text() {
  local file="$1" text="$2"
  grep -Fq -- "$text" "$(at "$file")" || die "$file lacks: $text"
}
require_toml_string() {
  local file="$1" dotted_key="$2" expected="$3"
  python3 - "$(at "$file")" "$dotted_key" "$expected" <<'PY' \
    || die "$file does not declare $dotted_key = $expected"
import pathlib
import sys
import tomllib

value = tomllib.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
for component in sys.argv[2].split("."):
    if not isinstance(value, dict) or component not in value:
        raise SystemExit(1)
    value = value[component]
raise SystemExit(0 if value == sys.argv[3] else 1)
PY
}

echo "== collaborative Files shipped-image contract ($mode) =="

# Executables and the host FUSE prerequisite must be in the immutable image.
for path in /usr/bin/fs /usr/bin/file-explorer /usr/bin/qos-launcher \
            /usr/bin/qos-install /usr/bin/qos-runtime-bundle \
            /usr/bin/fusermount3 /usr/bin/cage /usr/libexec/qos/files-atspi-smoke.sh \
            /usr/libexec/qos-session; do
  require_exec "$path"
done
require_file /etc/fuse.conf
require_file /usr/lib/systemd/system/fs.service
require_file /usr/lib/systemd/user/qos-fs-recipient.service
require_file /usr/lib/systemd/user/qos-identity.service
require_file /usr/lib/systemd/user/qos-graphical-session.target
require_file /usr/share/qos/selinux/qos-base.cil
require_file /usr/share/wayland-sessions/shell.desktop
require_file /usr/share/wayland-sessions/shell-peer-sync.desktop
require_file /usr/lib/tmpfiles.d/qos.conf
require_file /usr/share/qos/fs-capabilities.json

# The system host and per-user recipient must be enabled, not merely copied.
require_link /etc/systemd/system/multi-user.target.wants/fs.service
require_link /etc/systemd/system/multi-user.target.wants/qos-growfs-sysroot.service
require_link /etc/systemd/user/graphical-session.target.wants/qos-fs-recipient.service
require_link /etc/systemd/user/graphical-session-pre.target.wants/qos-identity.service
require_file /usr/lib/systemd/system/qos-growfs-sysroot.service
require_file /usr/lib/systemd/system/systemd-growfs-root.service.d/10-qos-composefs.conf
if [ -L "$(at /etc/systemd/system/systemd-growfs-root.service)" ] \
   && [ "$(readlink "$(at /etc/systemd/system/systemd-growfs-root.service)")" = /dev/null ]; then
  die "systemd-growfs-root.service is masked; its composefs layout drop-in cannot apply"
fi
require_line /usr/lib/systemd/system/qos-growfs-sysroot.service "ConditionPathIsMountPoint=/sysroot"
require_line /usr/lib/systemd/system/qos-growfs-sysroot.service "ExecStart=/usr/lib/systemd/systemd-growfs /sysroot"
require_line /usr/lib/systemd/system/systemd-growfs-root.service.d/10-qos-composefs.conf "ConditionPathIsMountPoint=!/sysroot"

# Keep the unit/launcher protocol literal and paths tied together.  Exact-line
# checks catch a continuation accidentally being dropped from ExecStart.
require_text /usr/lib/systemd/system/fs.service "/usr/bin/fs serve --mount-point /var/workspace --cache-dir /var/cache/quick-fs"
require_text /usr/lib/systemd/system/fs.service "--acl /var/lib/qos/fs-capabilities.json --principal person:local"
require_line /usr/lib/systemd/system/fs.service "ConditionPathExists=/dev/fuse"
require_line /usr/lib/systemd/system/fs.service "ExecStop=/usr/bin/fusermount3 -u -z /var/workspace"
require_text /usr/lib/systemd/user/qos-fs-recipient.service "/usr/bin/fs recipient-service --socket %t/qos/services/qos-fs.sock"
require_text /usr/lib/systemd/user/qos-fs-recipient.service "--mounts-root %h/Shared --cache-root %h/.cache/qos/received-files"
require_line /usr/lib/systemd/user/qos-fs-recipient.service "Requires=qos-identity.service"
require_line /usr/lib/systemd/user/qos-fs-recipient.service "PartOf=graphical-session.target"
require_line /usr/lib/systemd/user/qos-identity.service "ExecStart=/usr/bin/shell --provision-identity"
require_text /usr/lib/systemd/user/qos-graphical-session.target "Requires=graphical-session-pre.target graphical-session.target"
require_text /usr/lib/systemd/user/qos-graphical-session.target "PropagatesStopTo=graphical-session.target graphical-session-pre.target"
require_text /usr/share/qos/selinux/qos-base.cil "qos_session_unit_file_t"
require_text /usr/share/qos/selinux/qos-base.cil "(allow xdm_t qos_session_unit_file_t (service (start stop status)))"
require_line /usr/share/wayland-sessions/shell.desktop "Exec=/usr/libexec/qos-session --backend drm"
require_line /usr/share/wayland-sessions/shell-peer-sync.desktop "Exec=/usr/libexec/qos-session --backend drm --enable-peer-sync"
require_text /usr/libexec/qos-session '"$SYSTEMCTL" --user start "$SESSION_TARGET"'
require_text /usr/libexec/qos-session 'run_managed "$CAGE_BIN" -s -- "$SHELL_BIN" --provision-identity'
require_text /usr/libexec/qos-session '"$SHELL_BIN" --verify-collaboration-identity'
require_text /usr/lib/tmpfiles.d/qos.conf "d  /var/workspace"
require_text /usr/lib/tmpfiles.d/qos.conf "d  /var/cache/quick-fs"
require_text /usr/lib/tmpfiles.d/qos.conf "C  /var/lib/qos/fs-capabilities.json"

# Files must be installed through the same registry/payload route the launcher
# consumes.  A loose /usr/bin copy alone does not make it launchable as an app.
registry=/usr/share/qos-apps/registry.toml
payload=/usr/share/qos-apps/payloads/dev.qos.file-explorer
require_file "$registry"
require_file "$payload/manifest.toml"
require_exec "$payload/bin/file-explorer"
require_text "$registry" 'id = "dev.qos.file-explorer"'
require_text "$registry" 'payload_root = "/usr/share/qos-apps/payloads/dev.qos.file-explorer"'
require_text "$registry" 'manifest_path = "/usr/share/qos-apps/payloads/dev.qos.file-explorer/manifest.toml"'
# The packer serializes a normalized manifest rather than preserving authoring
# whitespace. Validate the installed TOML values, not one formatter's spacing.
require_toml_string "$payload/manifest.toml" package.id "dev.qos.file-explorer"
require_toml_string "$payload/manifest.toml" sandbox.services.qos-fs "optional@1"
require_toml_string "$payload/manifest.toml" package.runtime "qos-runtime/2026.07"
# A release check must exercise the package the launcher resolves, not an
# unrelated loose copy that happens to have the same filename.  pack-apps.sh
# currently installs the same build artifact in both places; fail if those two
# routes ever diverge so the headless parser check and graphical payload check
# cannot silently describe different applications.
cmp -s "$(at /usr/bin/file-explorer)" "$(at "$payload/bin/file-explorer")" \
  || die "Files system binary and installed payload differ"
require_file /usr/lib/qos-runtime/2026.07/bundle.toml
require_file /usr/share/qos/runtime-bundle-publisher.hex
require_file /etc/qos/runtime-bundle-trusted-publishers.txt

# These checks execute the actual shipped command parsers when validating the
# real root.  Alternate OS_ROOT trees are for offline inspection only.
if [ "$root" = / ]; then
  fs_help="$(/usr/bin/fs --help)" || die "shipped fs --help failed"
  grep -Fq 'serve' <<<"$fs_help" || die "shipped fs lacks serve"
  grep -Fq 'recipient-service' <<<"$fs_help" || die "shipped fs lacks recipient-service"
  scratch="$(mktemp -d)"
  trap 'rm -rf "$scratch"' EXIT
  HOME="$scratch" XDG_DATA_HOME="$scratch/data" XDG_CONFIG_HOME="$scratch/config" \
    /usr/bin/file-explorer --headless-drive '{}' >/dev/null \
    || die "shipped Files headless frame failed"
  /usr/bin/qos-launcher --help >/dev/null || die "shipped launcher CLI failed"
  QOS_APPS_DATA_DIR=/usr/share/qos-apps \
    /usr/bin/qos-install verify dev.qos.file-explorer >/dev/null \
    || die "installed Files payload failed its recorded integrity check"
  /usr/bin/qos-runtime-bundle verify /usr/lib/qos-runtime/2026.07 \
    --require-publisher "$(cat /usr/share/qos/runtime-bundle-publisher.hex)" >/dev/null \
    || die "shipped runtime bundle signature or content check failed"
fi

if [ "$mode" = runtime ]; then
  [ "$root" = / ] || die "runtime mode requires OS_ROOT=/"
  [ -c /dev/fuse ] || die "/dev/fuse is absent or not a character device"
  systemctl is-enabled --quiet fs.service || die "fs.service is not enabled"
  systemctl is-active --quiet fs.service || die "fs.service is not active"
  mountpoint -q /var/workspace || die "/var/workspace is not mounted"
  fs_type="$(findmnt -n -o FSTYPE --target /var/workspace)"
  case "$fs_type" in fuse|fuse.*) ;; *) die "/var/workspace type is $fs_type, not FUSE" ;; esac
  [ -r /var/lib/qos/fs-capabilities.json ] || die "live ACL seed is missing"

  # This command is run as the installed login user over SSH, after the
  # graphical login has started its systemd --user manager.
  user_runtime="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
  export XDG_RUNTIME_DIR="$user_runtime"
  systemctl --user is-enabled --quiet qos-identity.service || die "identity user unit is not enabled"
  systemctl --user is-enabled --quiet qos-fs-recipient.service || die "recipient user unit is not enabled"

  # SSH is normally ready before greetd has finished the graphical login.
  # Give the bounded boot transaction time to settle, while still failing
  # closed if either service never becomes active.
  await_user_active() {
    local unit="$1" attempt
    for attempt in $(seq 1 60); do
      systemctl --user is-active --quiet "$unit" && return 0
      sleep 1
    done
    die "$unit is not active after 60 seconds"
  }
  await_user_active qos-identity.service
  await_user_active qos-fs-recipient.service
  systemctl --user is-active --quiet qos-graphical-session.target \
    || die "qOS graphical-session lifecycle target is not active"

  # Unit state and non-empty files are not cryptographic evidence. Require the
  # personal root and every device-chain artifact, then run the same read-only
  # verifier that gates compositor startup.
  id -nG | tr ' ' '\n' | grep -Fqx qos-identity \
    || die "login user is not a member of qos-identity"
  for identity_file in \
    "$HOME/.config/qos/identity/primary.json" \
    /var/lib/qos/user-identity/pubkey.bin \
    /var/lib/qos/device/keypair.bin \
    /var/lib/qos/device/cert.bin; do
    [ -s "$identity_file" ] || die "identity artifact is absent or empty: $identity_file"
  done
  [ ! -e /var/lib/qos/user-identity/privkey.bin ] \
    || die "personal root private key is duplicated into shared system identity storage"
  /usr/bin/shell --verify-collaboration-identity >/dev/null \
    || die "installed collaboration identity chain does not verify"

  for attempt in $(seq 1 30); do
    [ -S "$user_runtime/qos/services/qos-fs.sock" ] \
      && [ -d "$HOME/.cache/qos/received-files" ] \
      && break
    sleep 1
  done
  [ -S "$user_runtime/qos/services/qos-fs.sock" ] || die "recipient service socket is absent"
  [ -d "$HOME/.cache/qos/received-files" ] || die "recipient state root is absent"

  # SSH does not inherit the graphical user's display variables. Import only
  # the display and bus values from that same user's systemd manager. The
  # runtime directory is derived from the authenticated SSH uid above.
  manager_env="$(systemctl --user show-environment)"
  for key in DISPLAY DBUS_SESSION_BUS_ADDRESS; do
    value="$(printf '%s\n' "$manager_env" | sed -n "s/^${key}=//p" | head -n 1)"
    [ -n "$value" ] && export "$key=$value"
  done
  [ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ] || die "graphical user D-Bus address is absent"

  # The compositor publishes the authoritative socket name after it has bound
  # the real session socket.  Do not accept a stale SSH/manager variable or an
  # AT-SPI tree rendered without the installed qOS session.
  display_file="$user_runtime/qos-session/wayland-display"
  for attempt in $(seq 1 60); do
    [ -s "$display_file" ] && break
    sleep 1
  done
  [ -s "$display_file" ] || die "qOS compositor did not publish its Wayland display"
  WAYLAND_DISPLAY="$(cat "$display_file")"
  case "$WAYLAND_DISPLAY" in
    wayland-[0-9]*) ;;
    *) die "invalid qOS Wayland display name: $WAYLAND_DISPLAY" ;;
  esac
  export WAYLAND_DISPLAY
  [ -S "$user_runtime/$WAYLAND_DISPLAY" ] \
    || die "published qOS Wayland socket is absent"

  # Exercise the verified immutable package payload, which is the artifact the
  # installed registry and launcher resolve.
  QOS_FILES_BINARY="$payload/bin/file-explorer" \
    /usr/libexec/qos/files-atspi-smoke.sh \
    || die "installed Files AT-SPI smoke failed"
fi

echo "collaborative-files-image-contract: PASS ($mode)"
