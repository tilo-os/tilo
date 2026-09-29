#!/usr/bin/env bash
# Read a package's file list on stdin and fail if it carries any part of the
# desktop session: the runtime package has no compositor, login entry or PAM
# stack, and must install beside tilo-session without owning its files.
set -euo pipefail

if grep -E '(^|/)(usr/bin/tilo-(shell|launcher)|etc/pam\.d/|usr/share/wayland-sessions/|share/xdg-desktop-portal/portals/)' ; then
    echo "the runtime package must not install session files (listed above)" >&2
    exit 1
fi
