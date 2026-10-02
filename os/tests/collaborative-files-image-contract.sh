#!/usr/bin/env bash
# Fail-closed contract for the collaborative Files stack shipped by Tilo.
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
for path in /usr/bin/fs /usr/bin/file-explorer /usr/bin/tilo-launcher \
            /usr/bin/tilo-runtime-mcp \
            /usr/bin/tilo-install /usr/bin/tilo-runtime-bundle \
            /usr/bin/fusermount3 /usr/bin/cage /usr/libexec/tilo/files-atspi-smoke.sh \
            /usr/libexec/tilo/native-atspi-smoke.sh \
            /usr/libexec/tilo/atspi-external-client.py /usr/libexec/tilo/installed-atspi-apps.sh \
            /usr/libexec/tilo-session /usr/libexec/tilo-account-credential \
            /usr/libexec/tilo-flight-recorder /usr/libexec/tilo-session-report \
            /usr/bin/dbus-update-activation-environment; do
  require_exec "$path"
done
require_file /etc/fuse.conf
for sleep_unit in systemd-suspend systemd-hibernate systemd-hybrid-sleep \
                  systemd-suspend-then-hibernate; do
  require_line "/etc/systemd/system/$sleep_unit.service.d/20-tilo-freeze-user-sessions.conf" \
    "Environment=SYSTEMD_SLEEP_FREEZE_USER_SESSIONS=1"
done
require_file /usr/lib/systemd/system/fs.service
require_file /usr/lib/systemd/system/tilo-fs-start.timer
require_file /usr/lib/systemd/system/fs-identity.path
require_file /usr/lib/systemd/user/tilo-fs-recipient.service
require_file /usr/lib/systemd/user/tilo-fs-recipient.socket
require_file /usr/lib/systemd/user/tilo-fs-cloud.service
require_file /usr/lib/systemd/user/tilo-graphical-session.target
require_file /usr/share/tilo/selinux/tilo-base.cil
require_file /usr/share/wayland-sessions/shell.desktop
require_file /usr/share/wayland-sessions/shell-peer-sync.desktop
require_file /usr/lib/tmpfiles.d/tilo.conf
require_file /usr/share/tilo/fs-capabilities.json

# AccessKit's provider requests org.a11y.Bus from the session bus. Keep the
# daemon package explicit: a tree-only smoke must never pass merely because a
# browser happened to pull it in transitively.
if [ "$root" = / ]; then
  rpm -q at-spi2-core >/dev/null 2>&1 || die "at-spi2-core is not installed"
else
  require_file /usr/share/dbus-1/services/org.a11y.Bus.service
fi

# The system host and per-user recipient must be enabled, not merely copied.
require_link /etc/systemd/system/timers.target.wants/tilo-fs-start.timer
require_link /etc/systemd/system/timers.target.wants/tilo-growfs-sysroot.timer
require_link /etc/systemd/system/multi-user.target.wants/fs-identity.path
require_link /etc/systemd/user/graphical-session.target.wants/tilo-fs-recipient.socket
require_link /etc/systemd/user/graphical-session.target.wants/tilo-fs-cloud.service
require_file /usr/lib/systemd/system/tilo-growfs-sysroot.service
require_file /usr/lib/systemd/system/tilo-growfs-sysroot.timer
require_file /usr/lib/systemd/system/systemd-growfs-root.service.d/10-tilo-composefs.conf
if [ -L "$(at /etc/systemd/system/systemd-growfs-root.service)" ] \
   && [ "$(readlink "$(at /etc/systemd/system/systemd-growfs-root.service)")" = /dev/null ]; then
  die "systemd-growfs-root.service is masked; its composefs layout drop-in cannot apply"
fi
require_line /usr/lib/systemd/system/tilo-growfs-sysroot.service "ConditionPathIsMountPoint=/sysroot"
require_line /usr/lib/systemd/system/tilo-growfs-sysroot.service "ExecStart=/usr/bin/sh -c 'mount -o remount,rw /sysroot && exec /usr/lib/systemd/systemd-growfs /sysroot'"
require_line /usr/lib/systemd/system/tilo-growfs-sysroot.timer "Unit=tilo-growfs-sysroot.service"
require_line /usr/lib/systemd/system/tilo-growfs-sysroot.timer "OnBootSec=5s"
require_line /usr/lib/systemd/system/systemd-growfs-root.service.d/10-tilo-composefs.conf "ConditionPathIsMountPoint=!/sysroot"

# Keep the unit/launcher protocol literal and paths tied together.  Exact-line
# checks catch a continuation accidentally being dropped from ExecStart.
require_text /usr/lib/systemd/system/fs.service "/usr/bin/fs serve --mount-point /var/workspace --cache-dir /var/cache/quick-fs"
require_text /usr/lib/systemd/system/fs.service "--acl /var/lib/tilo/fs-capabilities.json --principal person:local"
require_line /usr/lib/systemd/system/fs.service "ConditionPathExists=/dev/fuse"
require_line /usr/lib/systemd/system/fs.service "ConditionPathExists=/var/lib/tilo/device/cert.bin"
require_line /usr/lib/systemd/system/fs.service "ExecStop=/usr/bin/fusermount3 -u -z /var/workspace"
# The FUSE mount (or Tier 1 bind) must reach the host: no directive that
# gives the service its own mount namespace, in the unit or a drop-in.
if cat "$root/usr/lib/systemd/system/fs.service" "$root"/usr/lib/systemd/system/fs.service.d/*.conf 2>/dev/null \
    | grep -Eq '^(PrivateIPC|PrivateTmp|PrivateDevices|PrivateMounts|PrivateUsers|ProtectSystem|ProtectHome|ProtectKernelTunables|ProtectKernelModules|ProtectKernelLogs|ProtectControlGroups|ProtectProc|ReadOnlyPaths|ReadWritePaths|InaccessiblePaths|TemporaryFileSystem|BindPaths|BindReadOnlyPaths|MountFlags)='; then
  die "fs.service gets a private mount namespace, so /var/workspace would never reach the host"
fi
# The room store volume: prepared before the daemon, which serves FUSE unless
# the preparation announced a mounted store.
require_exec /usr/libexec/tilo-store-volume
require_file /usr/lib/systemd/system/tilo-store-volume.service
require_file /usr/lib/systemd/system/tilo-store-loop.service
require_line /usr/lib/systemd/system/fs.service "ExecStopPost=-/usr/bin/umount --lazy /var/workspace"
require_line /usr/lib/systemd/system/fs.service "Wants=tilo-store-volume.service var-lib-tilo-store.mount"
require_line /usr/lib/systemd/system/fs.service "After=tilo-store-volume.service var-lib-tilo-store.mount"
require_line /usr/lib/systemd/system/fs.service "Environment=TILO_FS_DATA_PLANE_ARGS=--data-plane=fuse"
require_line /usr/lib/systemd/system/fs.service "EnvironmentFile=-/run/tilo-store/fs.env"
require_line /usr/lib/systemd/system/fs.service "    \$TILO_FS_DATA_PLANE_ARGS"
require_line /usr/lib/systemd/system/tilo-store-volume.service "ExecStart=/usr/libexec/tilo-store-volume ensure"
require_line /usr/lib/systemd/system/tilo-store-volume.service "Before=fs.service"
require_line /usr/lib/systemd/system/tilo-store-volume.service "RuntimeDirectory=tilo-store"
require_line /usr/lib/systemd/system/tilo-store-loop.service "ExecStart=/usr/libexec/tilo-store-volume attach"
require_line /usr/lib/systemd/system/fs-identity.path "PathChanged=/var/lib/tilo/device/cert.bin"
require_line /usr/lib/systemd/system/fs-identity.path "Unit=fs.service"
require_text /usr/lib/systemd/user/tilo-fs-recipient.service "/usr/bin/fs recipient-service --socket %t/tilo/services/tilo-fs.sock"
require_text /usr/lib/systemd/user/tilo-fs-recipient.service "--mounts-root %h/Shared --cache-root %h/.cache/tilo/received-files"
require_line /usr/lib/systemd/user/tilo-fs-cloud.service "Environment=TILO_ACCOUNT_CREDENTIAL_HELPER=/usr/libexec/tilo-account-credential"
require_line /usr/lib/systemd/user/tilo-fs-recipient.service "PartOf=graphical-session.target"
require_line /usr/lib/systemd/user/tilo-fs-recipient.socket "ListenStream=%t/tilo/services/tilo-fs.sock"
require_line /usr/lib/systemd/user/tilo-fs-recipient.socket "Service=tilo-fs-recipient.service"
require_line /usr/lib/systemd/user/tilo-fs-recipient.socket "SocketMode=0600"
require_text /usr/lib/systemd/user/tilo-graphical-session.target "Requires=graphical-session-pre.target graphical-session.target"
require_text /usr/lib/systemd/user/tilo-graphical-session.target "PropagatesStopTo=graphical-session.target graphical-session-pre.target"
require_text /usr/share/tilo/selinux/tilo-base.cil "tilo_session_unit_file_t"
require_text /usr/share/tilo/selinux/tilo-base.cil "(allow xdm_t tilo_session_unit_file_t (service (start stop status)))"
require_text /usr/share/tilo/selinux/tilo-base.cil "(allow init_t tilo_fs_t (process2 (nnp_transition nosuid_transition)))"
require_line /usr/share/wayland-sessions/shell.desktop "Exec=/usr/libexec/tilo-session --backend drm"
require_line /usr/share/wayland-sessions/shell-peer-sync.desktop "Exec=/usr/libexec/tilo-session --backend drm --enable-peer-sync"
require_text /usr/libexec/tilo-session '"$SYSTEMCTL" --user start "$SESSION_TARGET"'
require_text /usr/libexec/tilo-session 'run_managed "$SHELL_BIN" "$@"'
require_text /usr/libexec/tilo-session '"$DBUS_UPDATE" --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE'
require_text /usr/libexec/tilo-session '"$SYSTEMCTL" --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE'
require_text /usr/libexec/tilo-session '"$DBUS_UPDATE" --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE'
require_text /usr/libexec/tilo-session '"$SYSTEMCTL" --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE'
if grep -Fq -- '--provision-identity' "$(at /usr/libexec/tilo-session)" \
   || grep -Fq -- '--verify-collaboration-identity' "$(at /usr/libexec/tilo-session)"; then
  die "session wrapper must let the trusted native compositor own first-login OOBE"
fi
require_text /usr/lib/tmpfiles.d/tilo.conf "d  /var/workspace"
require_text /usr/lib/tmpfiles.d/tilo.conf "d  /var/cache/quick-fs"
require_text /usr/lib/tmpfiles.d/tilo.conf "C  /var/lib/tilo/fs-capabilities.json"

# Files must be installed through the same registry/payload route the launcher
# consumes.  A loose /usr/bin copy alone does not make it launchable as an app.
registry=/usr/share/tilo-apps/registry.toml
require_file "$registry"
require_text "$registry" 'id = "so.tilo.file-explorer"'
# The package store names each tree by its signed package id, so read where
# the row points instead of assuming a path.
registry_field() {
  python3 - "$(at "$registry")" "$1" "$2" <<'PY'
import pathlib
import sys
import tomllib

rows = tomllib.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
for row in rows.get("entry", []):
    if row.get("id") == sys.argv[2]:
        print(row[sys.argv[3]])
        raise SystemExit(0)
raise SystemExit(1)
PY
}
payload="$(registry_field so.tilo.file-explorer payload_root)" \
  || die "registry has no payload root for Files"
manifest="$(registry_field so.tilo.file-explorer manifest_path)" \
  || die "registry has no manifest for Files"
case "$payload" in
  /usr/share/tilo-apps/pkgs/*) ;;
  *) die "Files payload $payload is outside the system package store" ;;
esac
case "$manifest" in
  /usr/share/tilo-apps/pkgs/*.pkg/manifest.toml) ;;
  *) die "Files manifest $manifest is not the signed record" ;;
esac
require_file "$manifest"
require_exec "$payload/bin/file-explorer"
# The packer serializes a normalized manifest rather than preserving authoring
# whitespace. Validate the installed TOML values, not one formatter's spacing.
require_toml_string "$manifest" package.id "so.tilo.file-explorer"
require_toml_string "$manifest" sandbox.services.tilo-fs "optional@1"
require_toml_string "$manifest" package.runtime "tilo-runtime/2026.07"
# These are deliberately allowed to be distinct signed build artifacts.  The
# loose binary is the privileged recovery surface built with the OS, while the
# launcher resolves the checksum-pinned app-collection payload.  Requiring
# byte equality made every legitimate pinned-collection image fail before its
# installed-session AT-SPI gate could run.  The checks below exercise the
# recovery binary and verify the installed package independently; runtime mode
# then launches this exact payload for the external AT-SPI smoke.
require_file /usr/lib/tilo-runtime/2026.07/bundle.toml
require_file /usr/share/tilo/runtime-bundle-publisher.hex
require_file /etc/tilo/runtime-bundle-trusted-publishers.txt

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
  /usr/bin/tilo-launcher --help >/dev/null || die "shipped launcher CLI failed"
  TILO_APPS_DATA_DIR=/usr/share/tilo-apps \
    /usr/bin/tilo-install verify so.tilo.file-explorer >/dev/null \
    || die "installed Files payload failed its recorded integrity check"
  /usr/bin/tilo-runtime-bundle verify /usr/lib/tilo-runtime/2026.07 \
    --require-publisher "$(cat /usr/share/tilo/runtime-bundle-publisher.hex)" >/dev/null \
    || die "shipped runtime bundle signature or content check failed"
  # Everything the store volume helper runs, at install and at boot.
  for tool in mkfs.btrfs cryptsetup losetup fallocate chattr blkid findmnt systemd-escape; do
    command -v "$tool" >/dev/null 2>&1 || die "store volume tool $tool is not in the image"
  done
  /usr/libexec/tilo-store-volume plan --disk-bytes 137438953472 --reserved-bytes 1612709888 \
    | grep -q '^STORE_BYTES=[1-9]' || die "shipped store volume helper plans no store on a 128 GiB disk"
fi

if [ "$mode" = runtime ]; then
  [ "$root" = / ] || die "runtime mode requires OS_ROOT=/"
  [ -c /dev/fuse ] || die "/dev/fuse is absent or not a character device"
  systemctl is-enabled --quiet tilo-fs-start.timer || die "tilo-fs-start.timer is not enabled"
  systemctl is-enabled --quiet fs-identity.path || die "fs-identity.path is not enabled"
  systemctl is-active --quiet fs.service || die "fs.service is not active"
  mountpoint -q /var/workspace || die "/var/workspace is not mounted"
  systemctl is-active --quiet tilo-store-volume.service || die "tilo-store-volume.service is not active"
  [ -r /run/tilo-store/fs.env ] || die "the store preparation announced no data plane"
  fs_type="$(findmnt -n -o FSTYPE --target /var/workspace)"
  # The data plane follows the store: a mounted, announced store volume means
  # the native tree is bound at /var/workspace (unless the daemon logged why the
  # kernel probes refused it); no store means FUSE.
  if grep -Fq -- '--data-plane=auto' /run/tilo-store/fs.env; then
    [ "$(findmnt -n -o FSTYPE -M /var/lib/tilo/store)" = btrfs ] \
      || die "a store was announced but /var/lib/tilo/store is not a mounted Btrfs volume"
    case "$fs_type" in
      btrfs) ;;
      fuse|fuse.*)
        journalctl -b -u fs.service --no-pager | grep -F 'Data plane: FUSE' \
          || die "/var/workspace is FUSE on a store volume and fs.service logged no reason"
        ;;
      *) die "/var/workspace type is $fs_type, neither the native tree nor FUSE" ;;
    esac
  else
    case "$fs_type" in fuse|fuse.*) ;; *) die "/var/workspace type is $fs_type without a store, not FUSE" ;; esac
  fi
  [ -r /var/lib/tilo/fs-capabilities.json ] || die "live ACL seed is missing"

  # This command is run as the installed login user over SSH, after the
  # graphical login has started its systemd --user manager.
  user_runtime="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
  export XDG_RUNTIME_DIR="$user_runtime"
  systemctl --user is-enabled --quiet tilo-fs-recipient.socket || die "recipient socket is not enabled"

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
  await_user_active tilo-fs-recipient.socket
  systemctl --user is-active --quiet tilo-graphical-session.target \
    || die "Tilo graphical-session lifecycle target is not active"

  # Unit state and non-empty files are not cryptographic evidence. Require the
  # personal root and every device-chain artifact, then run the same read-only
  # verifier that gates compositor startup.
  id -nG | tr ' ' '\n' | grep -Fqx tilo-identity \
    || die "login user is not a member of tilo-identity"
  for identity_file in \
    "$HOME/.config/tilo/identity/primary.json" \
    /var/lib/tilo/user-identity/pubkey.bin \
    /var/lib/tilo/device-private/keypair.bin \
    /var/lib/tilo/device/cert.bin; do
    [ -s "$identity_file" ] || die "identity artifact is absent or empty: $identity_file"
  done
  [ ! -e /var/lib/tilo/user-identity/privkey.bin ] \
    || die "personal root private key is duplicated into shared system identity storage"
  /usr/bin/shell --verify-collaboration-identity >/dev/null \
    || die "installed collaboration identity chain does not verify"

  for attempt in $(seq 1 30); do
    [ -S "$user_runtime/tilo/services/tilo-fs.sock" ] \
      && [ -d "$HOME/.cache/tilo/received-files" ] \
      && break
    sleep 1
  done
  [ -S "$user_runtime/tilo/services/tilo-fs.sock" ] || die "recipient service socket is absent"
  [ -d "$HOME/.cache/tilo/received-files" ] || die "recipient state root is absent"

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
  # AT-SPI tree rendered without the installed Tilo session.
  display_file="$user_runtime/tilo-session/wayland-display"
  for attempt in $(seq 1 60); do
    [ -s "$display_file" ] && break
    sleep 1
  done
  [ -s "$display_file" ] || die "Tilo compositor did not publish its Wayland display"
  WAYLAND_DISPLAY="$(cat "$display_file")"
  case "$WAYLAND_DISPLAY" in
    wayland-[0-9]*) ;;
    *) die "invalid Tilo Wayland display name: $WAYLAND_DISPLAY" ;;
  esac
  export WAYLAND_DISPLAY
  [ -S "$user_runtime/$WAYLAND_DISPLAY" ] \
    || die "published Tilo Wayland socket is absent"

  # Exercise the verified immutable package payload, which is the artifact the
  # installed registry and launcher resolve. Thin packages require the signed
  # runtime bundle's library directory, just as the launcher supplies it. Do
  # not inherit host library overrides into this installed-payload check.
  LD_LIBRARY_PATH=/usr/lib/tilo-runtime/2026.07/lib \
  TILO_FILES_BINARY="$payload/bin/file-explorer" \
    /usr/libexec/tilo/files-atspi-smoke.sh \
    || die "installed Files AT-SPI smoke failed"
fi

echo "collaborative-files-image-contract: PASS ($mode)"
