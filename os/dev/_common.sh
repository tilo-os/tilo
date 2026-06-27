#!/bin/bash
# Shared config + helpers for the qOS headless VM dev toolkit (os/dev/*).
#
# Everything here is overridable via environment variables so the toolkit works
# on other machines / in CI without edits. The defaults match the reference
# setup these scripts were validated on:
#   - build deps live in a Fedora distrobox (the host is immutable bootc/atomic)
#   - qemu/OVMF/virglrenderer/socat/ImageMagick are installed *in that distrobox*
#   - the host has /dev/kvm and a Mesa iGPU render node for GL
#
# If your host has qemu+tools natively, set QOS_DISTROBOX="" to run directly.

set -euo pipefail

# --- paths -------------------------------------------------------------------
_COMMON_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
QOS_OS_DIR="$(cd "$_COMMON_DIR/.." && pwd)"          # the os/ submodule dir
QOS_REPO="$(cd "$QOS_OS_DIR/.." && pwd)"             # cargo workspace root

# --- tunables ----------------------------------------------------------------
QOS_DISTROBOX="${QOS_DISTROBOX-my-distrobox}"        # "" => run tools on host
QOS_IMAGE="${QOS_IMAGE:-localhost/qos:latest}"
QOS_OUTPUT="${QOS_OUTPUT:-$QOS_OS_DIR/output}"
QOS_DISK="${QOS_DISK:-$QOS_OUTPUT/qcow2/disk.qcow2}"
QOS_ISO="${QOS_ISO:-}"   # resolved at boot-iso time (find *.iso under output)
QOS_BIB="${QOS_BIB:-quay.io/centos-bootc/bootc-image-builder:latest}"

# Runtime/scratch dir for the VM. MUST be short: AF_UNIX socket paths are capped
# at 108 bytes, and a deep scratch path overflows it. /tmp/qos-vm is safe.
QOS_RUN="${QOS_RUN:-/tmp/qos-vm}"
QOS_MON="$QOS_RUN/mon.sock"
QOS_SERIAL="$QOS_RUN/serial.log"
QOS_QEMU_LOG="$QOS_RUN/qemu.log"
QOS_PIDFILE="$QOS_RUN/qemu.pid"

# Guest login (from os/config.toml — the bootc-image-builder user).
QOS_USER="${QOS_USER:-quickuser}"
QOS_PASS="${QOS_PASS:-quickdesktop}"

QOS_MEM="${QOS_MEM:-4096}"
QOS_SMP="${QOS_SMP:-4}"

# virtio-gpu resolution (the compositor modesets the connector's preferred
# mode, which these set). Bigger = more software-render cost.
QOS_XRES="${QOS_XRES:-1280}"
QOS_YRES="${QOS_YRES:-800}"

# SSH: the VM forwards guest :22 to host 127.0.0.1:$QOS_SSH_PORT. Needs
# `sshpass` on the host (or swap in a key). Clean shell access beats the
# VT/serial/screendump dance for logs and diagnosis.
QOS_SSH_PORT="${QOS_SSH_PORT:-2222}"

# --- logging -----------------------------------------------------------------
msg() { printf '\033[1;36m[qos]\033[0m %s\n' "$*" >&2; }
warn() { printf '\033[1;33m[qos] WARN:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m[qos] ERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# --- run a command in the build/VM environment (distrobox or host) ------------
# Usage: inenv 'shell command string'
# Runs with the *current working directory preserved* (distrobox does this), so
# relative paths and the cargo workspace resolve correctly.
inenv() {
  if [ -n "$QOS_DISTROBOX" ]; then
    distrobox enter "$QOS_DISTROBOX" -- bash -lc "$1"
  else
    bash -lc "$1"
  fi
}

# Send one HMP monitor command to the running VM, print its reply.
hmp() {
  inenv "printf '%s\n' \"$1\" | socat - 'unix:$QOS_MON'"
}

# Run a command in the guest over SSH (password auth via sshpass, on the host
# since that's where the forwarded port binds). Returns the command's output.
gssh() {
  sshpass -p "$QOS_PASS" ssh -p "$QOS_SSH_PORT" \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=5 -o LogLevel=ERROR \
    "$QOS_USER@127.0.0.1" "$@"
}

# Pick a GL render node for virglrenderer. NVIDIA (0x10de) is skipped: its
# proprietary/open EGL stack works poorly with virglrenderer; a Mesa iGPU
# (AMD/Intel) is strongly preferred. Override with QOS_RENDERNODE.
detect_rendernode() {
  if [ -n "${QOS_RENDERNODE:-}" ]; then echo "$QOS_RENDERNODE"; return; fi
  local n v
  for n in /dev/dri/renderD*; do
    [ -e "$n" ] || continue
    v="$(cat "/sys/class/drm/$(basename "$n")/device/vendor" 2>/dev/null || echo)"
    [ "$v" = "0x10de" ] && continue
    echo "$n"; return
  done
  echo /dev/dri/renderD128
}

# Locate OVMF firmware inside the env.
ovmf_code() {
  inenv 'for f in /usr/share/edk2/ovmf/OVMF_CODE.fd /usr/share/OVMF/OVMF_CODE.fd; do [ -f "$f" ] && { echo "$f"; exit 0; }; done; exit 1' \
    || die "OVMF not found (install edk2-ovmf in ${QOS_DISTROBOX:-the host})"
}
ovmf_vars() {
  inenv 'for f in /usr/share/edk2/ovmf/OVMF_VARS.fd /usr/share/OVMF/OVMF_VARS.fd; do [ -f "$f" ] && { echo "$f"; exit 0; }; done; exit 1' \
    || die "OVMF_VARS not found"
}
