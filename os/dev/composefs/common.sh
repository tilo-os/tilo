#!/bin/bash
# Shared config for the composefs-backend spike (os/dev/composefs/*).
#
# Layered on os/dev/_common.sh so the VM helpers (OVMF discovery, swtpm,
# distrobox routing) are the ones the rest of the toolkit already uses.
set -euo pipefail

_CFS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$_CFS_DIR/../_common.sh"

# Scratch dir. NOT under /tmp: that is a 16 GiB tmpfs here, and a 20 GiB disk
# image on it fails with "Quota exceeded" from whichever command happens to be
# writing when it fills — which is never the command at fault. Still short,
# because the qemu monitor socket lives here and AF_UNIX paths cap at 108 bytes.
TILO_CFS_RUN="${TILO_CFS_RUN:-$HOME/.cache/tilo-cfs}"
TILO_CFS_IMAGE="${TILO_CFS_IMAGE:-localhost/tilo-composefs}"
TILO_CFS_SIGN="${TILO_CFS_SIGN:-1}"
TILO_CFS_DISK_SIZE="${TILO_CFS_DISK_SIZE:-20G}"

# The registry the update series is served from. The guest reaches the VM host
# at 10.0.2.2 under user-mode networking, so the same image is
# localhost:5000/... on the host and 10.0.2.2:5000/... in the guest.
TILO_CFS_REG_PORT="${TILO_CFS_REG_PORT:-5000}"
TILO_CFS_REG_HOST="localhost:$TILO_CFS_REG_PORT"
TILO_CFS_REG_GUEST="10.0.2.2:$TILO_CFS_REG_PORT"
TILO_CFS_REPO="${TILO_CFS_REPO:-tilo-composefs}"

# The same digest-pinned Fedora bootc base the desktop image builds FROM.
TILO_CFS_BASE_REF="${TILO_CFS_BASE_REF:-}"
if [ -z "$TILO_CFS_BASE_REF" ]; then
  # shellcheck source=/dev/null
  source "$TILO_OS_DIR/base-snapshot.env"
  TILO_CFS_BASE_REF="$BASE_REF"
fi

# The same digest-pinned frozen RPM snapshot the desktop image installs from.
TILO_CFS_RPMS_REF="${TILO_CFS_RPMS_REF:-}"
if [ -z "$TILO_CFS_RPMS_REF" ]; then
  TILO_CFS_RPMS_REF="$("$TILO_OS_DIR/dev/resolve-rpm-snapshot.sh")"     || { echo "composefs: resolve-rpm-snapshot.sh failed" >&2; exit 1; }
fi

TILO_CFS_DISK="${TILO_CFS_DISK:-$TILO_CFS_RUN/disk.raw}"
