#!/usr/bin/env bash
# check.sh as t's tilo-sh-local-vm check runs it: in a micro-VM, as root, it
# makes an unprivileged user and runs the check as that user, so this checkout's
# programs run the way they run on a developer's machine, not as root.
#
# The VM host is the one thing here that needs root (KVM, a raw block device,
# rootful podman); the stack starts it through this user's sudo, the way the
# machine's own owner would.
#
# usage: check-in-vm.sh WORK
# Everything else comes from the environment t gives the check (TILO_SH_*).
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
work=${1:?usage: check-in-vm.sh WORK}
user=tilo-sh
[[ $EUID == 0 ]] || { echo 'check-in-vm.sh makes a user, so it runs as root (in a VM)' >&2; exit 2; }

id "$user" >/dev/null 2>&1 || useradd --create-home "$user"
# The stack starts its VM host (Firecracker, a raw block device, cgroups) as
# root through sudo: this user may, without a password, as the machine's owner
# could.
echo "$user ALL=(root) NOPASSWD: ALL" > "/etc/sudoers.d/tilo-sh"
chmod 0440 "/etc/sudoers.d/tilo-sh"
runtime=/tmp/run-$user
install -d -m 0700 -o "$user" -g "$user" "$runtime"
mkdir -p "$work"
find "$work" -mindepth 1 -delete
chown "$user:$user" "$work"

# The stack's state: the Lounge's, the door's, and what the check writes.
state=${TILO_SH_STATE:?}
mkdir -p "$state"
chown "$user:$user" "$state"

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

# tilo-enroll and tilo-approve, where the check built them, are the user's to
# run: the first makes the VM host's member device, and the stack starts the
# host through the user's sudo; the second answers for the person the startup
# page made.
for program in ${TILO_SH_TILO_ENROLL:-} ${TILO_SH_TILO_APPROVE:-}; do
  [[ -x $program ]] || { echo "$program is not a program the check built" >&2; exit 1; }
  dir=$(dirname "$program")
  chmod o+rx "$dir" 2>/dev/null || true
  chmod o+rx "$program" 2>/dev/null || true
done

# The source tree is read-only to the user; the check writes only under
# WORK and the state.
exec runuser -u "$user" -- env XDG_RUNTIME_DIR="$runtime" CHROME="$chrome" \
  bash "$here/check.sh" "$work"
