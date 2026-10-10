#!/bin/sh
# Exactly one program owns org.freedesktop.secrets and serves the portal
# Secret backend: tilo-secret-service. GNOME Keyring stays only to be read once
# at a password login; nothing of it may activate for either name, or a locked
# second provider would answer after a fingerprint or automatic login.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
bus_file="$repo_root/os/usr/share/dbus-1/services/org.freedesktop.secrets.service"
unit="$repo_root/os/usr/lib/systemd/user/tilo-secret-service.service"
container="$repo_root/os/Containerfile"
components="$repo_root/os/components.toml"
pam="$repo_root/os/etc/pam.d/greetd"

ok() { printf 'ok - %s\n' "$1"; }
bad() { printf 'not ok - %s\n' "$1" >&2; exit 1; }

grep -Fxq 'Name=org.freedesktop.secrets' "$bus_file" \
  && grep -Fxq 'SystemdService=tilo-secret-service.service' "$bus_file" \
  && grep -Fq 'Exec=/usr/bin/tilo-secret-service serve' "$bus_file" \
  && ok 'D-Bus activation of org.freedesktop.secrets starts tilo-secret-service' \
  || bad 'org.freedesktop.secrets is not activated by tilo-secret-service'

grep -Fxq 'BusName=org.freedesktop.secrets' "$unit" \
  && grep -Fxq 'ExecStart=/usr/bin/tilo-secret-service serve' "$unit" \
  && ok 'the user unit owns the bus name' \
  || bad 'the user unit does not own org.freedesktop.secrets'

if grep -Fq 'os/usr/share/dbus-1/services/org.freedesktop.secrets.service' "$container" \
   && grep -Fq 'os/usr/lib/systemd/user/tilo-secret-service.service' "$container" \
   && grep -Fq 'rm -f /usr/share/xdg-desktop-portal/portals/gnome-keyring.portal' "$container" \
   && grep -Fq 'org.freedesktop.impl.portal.Secret.service' "$container" \
   && grep -Fq 'gnome-keyring-secrets.desktop' "$container" \
   && grep -Fq 'systemctl --global mask gnome-keyring-daemon.service gnome-keyring-daemon.socket' "$container"; then
  ok 'the image removes every GNOME Keyring activation path for the Secret Service and portal'
else
  bad 'the image still lets GNOME Keyring activate for org.freedesktop.secrets or the portal backend'
fi

if grep -Fq -- '-p tilo-secret-service' "$container" \
   && grep -Fq -- '--bin tilo-secret-service' "$container" \
   && grep -Fq 'name = "secret-service"' "$components"; then
  ok 'tilo-secret-service is built and installed'
else
  bad 'tilo-secret-service is not part of the image build'
fi

# PAM keeps the keyring unlock hooks, as the one-time import's source.
grep -Eq '^auth[[:space:]]+optional[[:space:]]+pam_gnome_keyring\.so$' "$pam" \
  && grep -Fq 'Nothing of gnome-keyring serves the bus name' "$pam" \
  && ok 'PAM unlocks the old keyring for the import only' \
  || bad 'the PAM stack no longer documents or keeps the migration-only keyring unlock'

portal_owner=$(grep -Eo 'org\.freedesktop\.impl\.portal\.Secret' "$repo_root/os/usr/share/xdg-desktop-portal/portals/tilo.portal" | head -n 1)
[ "$portal_owner" = 'org.freedesktop.impl.portal.Secret' ] \
  && ok 'the Tilo portal backend serves the Secret interface' \
  || bad 'the Tilo portal backend does not list the Secret interface'
