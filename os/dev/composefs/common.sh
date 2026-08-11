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
QOS_CFS_RUN="${QOS_CFS_RUN:-$HOME/.cache/qos-cfs}"
QOS_CFS_IMAGE="${QOS_CFS_IMAGE:-localhost/qos-composefs}"
QOS_CFS_SIGN="${QOS_CFS_SIGN:-1}"
QOS_CFS_DISK_SIZE="${QOS_CFS_DISK_SIZE:-20G}"

# The registry the update series is served from. The guest reaches the VM host
# at 10.0.2.2 under user-mode networking, so the same image is
# localhost:5000/... on the host and 10.0.2.2:5000/... in the guest.
QOS_CFS_REG_PORT="${QOS_CFS_REG_PORT:-5000}"
QOS_CFS_REG_HOST="localhost:$QOS_CFS_REG_PORT"
QOS_CFS_REG_GUEST="10.0.2.2:$QOS_CFS_REG_PORT"
QOS_CFS_REPO="${QOS_CFS_REPO:-qos-composefs}"

# The same digest-pinned Fedora bootc base the real qOS image builds FROM.
QOS_CFS_BASE_REF="${QOS_CFS_BASE_REF:-}"
if [ -z "$QOS_CFS_BASE_REF" ]; then
  # shellcheck source=/dev/null
  source "$QOS_OS_DIR/base-snapshot.env"
  QOS_CFS_BASE_REF="$BASE_REF"
fi

# The same digest-pinned frozen RPM snapshot the real image installs from.
QOS_CFS_RPMS_REF="${QOS_CFS_RPMS_REF:-}"
if [ -z "$QOS_CFS_RPMS_REF" ]; then
  # shellcheck source=/dev/null
  source "$QOS_OS_DIR/rpms-snapshot.env"
  QOS_CFS_RPMS_REF="$RPMS_REF"
fi

QOS_CFS_DISK="${QOS_CFS_DISK:-$QOS_CFS_RUN/disk.raw}"
