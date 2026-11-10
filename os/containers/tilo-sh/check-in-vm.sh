#!/usr/bin/env bash
# check.sh as t's tilo-sh-local check runs it: in a micro-VM, as root, it
# makes an unprivileged user and runs the stack and the check as that user,
# so the stack runs under rootless podman exactly as on a developer's machine
# (rootful podman, for one, gives the web entry no pasta network).
#
# usage: check-in-vm.sh WORK
# Everything else comes from the environment t gives the check (TILO_SH_*).
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
work=${1:?usage: check-in-vm.sh WORK}
user=tilo-sh
[[ $EUID == 0 ]] || { echo 'check-in-vm.sh makes a user, so it runs as root (in a VM)' >&2; exit 2; }

id "$user" >/dev/null 2>&1 || useradd --create-home "$user"
grep -q "^$user:" /etc/subuid || echo "$user:200000:65536" >> /etc/subuid
grep -q "^$user:" /etc/subgid || echo "$user:200000:65536" >> /etc/subgid
# Rootless podman maps the user's subordinate ids through these two, which
# work only setuid root (or with their file capabilities, which the image's
# setup layer does not keep).
chmod u+s /usr/bin/newuidmap /usr/bin/newgidmap
# The web entry's network (pasta) opens the tun device in the user's
# namespace, which a distribution's /dev/net/tun lets anyone do (0666) and
# the VM's does not.
chmod 0666 /dev/net/tun
runtime=/tmp/run-$user
install -d -m 0700 -o "$user" -g "$user" "$runtime"
mkdir -p "$work"
find "$work" -mindepth 1 -delete
chown "$user:$user" "$work"

# The stack's state, its container storage among it, on a disk filesystem
# of its own: the VM's root (the user's home) is overlayfs and its build
# directory virtiofs, and rootless podman can neither stack overlay storage
# on the one nor map its user namespace's ids onto the other. A sparse ext4
# image, loop-mounted, is an ordinary disk to it.
state=${TILO_SH_STATE:?}
mkdir -p "$state"
if ! mountpoint -q "$state"; then
  rm -f "$state.ext4"
  truncate -s 24G "$state.ext4"
  mkfs.ext4 -q -F "$state.ext4"
  mount -o loop "$state.ext4" "$state"
fi
chown "$user:$user" "$state"
home=$(getent passwd "$user" | cut -d: -f6)
install -d -o "$user" -g "$user" "$home/.config" "$home/.config/containers"
printf '[storage]\ndriver = "overlay"\ngraphroot = "%s/containers"\nrunroot = "%s/containers"\n' \
  "$state" "$runtime" > "$home/.config/containers/storage.conf"
chown "$user:$user" "$home/.config/containers/storage.conf"

# The image's Chromium is Playwright's, in root's cache: named for the user,
# whose own home has none, and made reachable.
chrome=${CHROME:-}
if [[ -z $chrome ]]; then
  chrome=$(ls -d "$HOME"/.cache/ms-playwright/chromium-*/chrome-linux*/chrome 2>/dev/null | sort -V | tail -1)
fi
[[ -x $chrome ]] || { echo 'no Chromium in this image (CHROME, or Playwright'"'"'s cache)' >&2; exit 1; }
dir=$chrome
while dir=$(dirname "$dir") && [[ $dir != / ]]; do chmod o+rx "$dir"; done
chmod -R o+rX "$(dirname "$chrome")"

# The source tree is read-only to the user; the check writes only under
# WORK and the state.
exec runuser -u "$user" -- env XDG_RUNTIME_DIR="$runtime" CHROME="$chrome" \
  bash "$here/check.sh" "$work"
