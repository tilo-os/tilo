#!/bin/bash
# Shared config + helpers for the Tilo headless VM dev toolkit (os/dev/*).
#
# Everything here is overridable via environment variables so the toolkit works
# on other machines / in CI without edits. The defaults match the reference
# setup these scripts were validated on:
#   - build deps live in a Fedora distrobox (the host is immutable bootc/atomic)
#   - qemu/OVMF/virglrenderer/socat/ImageMagick are installed *in that distrobox*
#   - the host has /dev/kvm and a Mesa iGPU render node for GL
#
# If your host has qemu+tools natively, set TILO_DISTROBOX="" to run directly.

set -euo pipefail

# --- paths -------------------------------------------------------------------
_COMMON_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TILO_OS_DIR="$(cd "$_COMMON_DIR/.." && pwd)"          # the os/ submodule dir
TILO_REPO="$(cd "$TILO_OS_DIR/.." && pwd)"             # cargo workspace root

# --- tunables ----------------------------------------------------------------
TILO_DISTROBOX="${TILO_DISTROBOX-my-distrobox}"        # "" => run tools on host
# The tag of the locally built images (localhost/tilo, localhost/tilo-vm,
# localhost/tilo-installer), so builds from different checkouts sharing one
# podman store keep theirs apart. TILO_IMAGE and the like still win.
TILO_LOCAL_TAG="${TILO_LOCAL_TAG:-latest}"
if [ "${#TILO_LOCAL_TAG}" -gt 128 ] || ! [[ "$TILO_LOCAL_TAG" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]*$ ]]; then
  printf 'TILO_LOCAL_TAG=%s is not an image tag ([A-Za-z0-9_][A-Za-z0-9_.-]*, at most 128 characters)\n' "$TILO_LOCAL_TAG" >&2
  exit 1
fi
TILO_IMAGE="${TILO_IMAGE:-localhost/tilo:$TILO_LOCAL_TAG}"
TILO_OUTPUT="${TILO_OUTPUT:-$TILO_OS_DIR/output}"
TILO_DISK="${TILO_DISK:-$TILO_OUTPUT/qcow2/disk.qcow2}"

# Custom (self-install) installer: a bootable live qcow2 that runs the Tilo
# graphical installer (installer/, binary tilo-installer). Built by
# os/dev/build-custom-installer.sh; booted by vm.sh boot-installer.
TILO_INSTALLER_IMAGE="${TILO_INSTALLER_IMAGE:-localhost/tilo-installer:$TILO_LOCAL_TAG}"
TILO_INSTALLER_OUTPUT="${TILO_INSTALLER_OUTPUT:-$TILO_OUTPUT/installer}"   # distinct subdir
TILO_INSTALLER_DISK="${TILO_INSTALLER_DISK:-$TILO_INSTALLER_OUTPUT/qcow2/disk.qcow2}"
# Writable overlay used by VM install cycles. The live installer needs scratch
# space for its offline image copy, but a test must never mutate the pristine
# release artifact it is validating.
# A named cycle isolates every mutable VM artifact while preserving it between
# boot-installer and boot-target. CI must use a unique value (run id + arch) and
# remove that exact directory before starting the cycle; stale target, TPM or
# firmware state must never make a release test pass.
TILO_VM_CYCLE="${TILO_VM_CYCLE:-}"
if [ -n "$TILO_VM_CYCLE" ]; then
  case "$TILO_VM_CYCLE" in
    *[!a-zA-Z0-9._-]*|.|..)
      printf 'TILO_VM_CYCLE must contain only letters, digits, dot, underscore or dash\n' >&2
      exit 1
      ;;
  esac
  TILO_CYCLE_DIR="${TILO_CYCLE_DIR:-$TILO_INSTALLER_OUTPUT/vm-cycles/$TILO_VM_CYCLE}"
else
  TILO_CYCLE_DIR="${TILO_CYCLE_DIR:-}"
fi
if [ -n "$TILO_CYCLE_DIR" ]; then
  TILO_INSTALLER_WORK_DISK="${TILO_INSTALLER_WORK_DISK:-$TILO_CYCLE_DIR/installer-work.qcow2}"
else
  TILO_INSTALLER_WORK_DISK="${TILO_INSTALLER_WORK_DISK:-$TILO_INSTALLER_OUTPUT/test-installer-work.qcow2}"
fi

# Runtime/scratch dir for the VM. MUST be short: AF_UNIX socket paths are capped
# at 108 bytes, and a deep scratch path overflows it. Prefer the per-user runtime
# directory: unlike shared /tmp it is private and cannot consume or inherit an
# unrelated per-user quota. Fall back to /tmp for non-desktop/CI sessions.
if [ -n "$TILO_VM_CYCLE" ]; then
  TILO_RUN="${TILO_RUN:-${XDG_RUNTIME_DIR:-/tmp}/tilo-vm-$TILO_VM_CYCLE}"
else
  TILO_RUN="${TILO_RUN:-${XDG_RUNTIME_DIR:-/tmp}/tilo-vm}"
fi
TILO_MON="$TILO_RUN/mon.sock"
TILO_SERIAL="$TILO_RUN/serial.log"
TILO_QEMU_LOG="$TILO_RUN/qemu.log"
TILO_PIDFILE="$TILO_RUN/qemu.pid"

# The blank disk the custom installer writes onto (becomes the installed
# system), attached as a SECOND virtio disk (/dev/vdb) by vm.sh boot-installer
# and booted on its own by vm.sh boot-target.
# OS targets are multi-gigabyte even when qcow2-sparse. Keep them on the build
# filesystem, not TILO_RUN's commonly-tmpfs /tmp directory, or a realistic
# encrypted install can exhaust RAM-backed storage and surface as guest I/O
# errors. 40 GiB also matches the installer's supported whole-disk envelope.
if [ -n "$TILO_CYCLE_DIR" ]; then
  TILO_TARGET_DISK="${TILO_TARGET_DISK:-$TILO_CYCLE_DIR/target.qcow2}"
else
  TILO_TARGET_DISK="${TILO_TARGET_DISK:-$TILO_INSTALLER_OUTPUT/test-target.qcow2}"
fi
TILO_TARGET_SIZE="${TILO_TARGET_SIZE:-40G}"

# Software TPM 2.0 (swtpm) so encrypted/TPM-bound installs can be booted and
# unlock-tested, and so a measured boot can be observed at all. Off by default;
# enable with `--tpm` on a boot subcommand or TILO_TPM=1. State persists in
# TILO_TPM_DIR so boot-installer (enroll) and boot-target (unlock) share the same
# emulated TPM across a reboot. Attaching a TPM also pins the firmware to a build
# that measures, or refuses the boot.
TILO_TPM="${TILO_TPM:-0}"
TILO_TPM_DIR="${TILO_TPM_DIR:-$TILO_RUN/tpm}"

# Guest login (from os/account.env — the account disk-image.sh creates).
TILO_USER="${TILO_USER:-tester}"
TILO_PASS="${TILO_PASS:-tilo}"

TILO_MEM="${TILO_MEM:-4096}"
TILO_SMP="${TILO_SMP:-4}"
# `auto` uses hardware acceleration when this environment owns a usable KVM
# device, and otherwise falls back to QEMU's multithreaded software emulator.
# The fallback is intentionally slower, but lets the release gate run inside a
# Kata-isolated CI runner without exposing the Kubernetes node's /dev/kvm.
# `hvf` is Apple's Hypervisor.framework: a Homebrew QEMU on an Apple silicon Mac
# running an aarch64 guest (set TILO_AAVMF_DIRS to its edk2 firmware).
TILO_ACCEL="${TILO_ACCEL:-auto}"
# How long a boot may take to reach the greeter (or the installer) before
# `vm.sh wait` fails it, and how long QEMU may take to open its monitor.
TILO_BOOT_WAIT_SECS="${TILO_BOOT_WAIT_SECS:-300}"
TILO_QEMU_START_SECS="${TILO_QEMU_START_SECS:-120}"

# Architecture of the guest and its release medium. Native runners leave this
# unset; cross-architecture diagnostic boots may select a guest explicitly and
# will naturally fall back to TCG. Keep the normalized value in one place so a
# lane cannot build an aarch64 medium and accidentally exercise x86 firmware.
TILO_VM_ARCH="${TILO_VM_ARCH:-$(uname -m)}"
case "$TILO_VM_ARCH" in
  x86_64|amd64) TILO_VM_ARCH=x86_64 ;;
  aarch64|arm64) TILO_VM_ARCH=aarch64 ;;
  *) die "TILO_VM_ARCH must be x86_64 or aarch64 (got $TILO_VM_ARCH)" ;;
esac
TILO_QEMU_SYSTEM="${TILO_QEMU_SYSTEM:-qemu-system-$TILO_VM_ARCH}"

# virtio-gpu resolution (the compositor modesets the connector's preferred
# mode, which these set). Bigger = more software-render cost.
TILO_XRES="${TILO_XRES:-1280}"
TILO_YRES="${TILO_YRES:-800}"

# SSH: the VM forwards guest :22 to host 127.0.0.1:$TILO_SSH_PORT. Needs
# `sshpass` on the host (or swap in a key). Clean shell access beats the
# VT/serial/screendump dance for logs and diagnosis.
TILO_SSH_PORT="${TILO_SSH_PORT:-2222}"

# --- logging -----------------------------------------------------------------
msg() { printf '\033[1;36m[tilo]\033[0m %s\n' "$*" >&2; }
warn() { printf '\033[1;33m[tilo] WARN:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m[tilo] ERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# Rootless podman storage created while native overlay was unavailable keeps
# committing every layer through fuse-overlayfs, which makes each image build
# step take minutes. Say so once, with the fix, instead of leaving it a mystery.
# Reproducible `podman build` timestamps, one argument per line. Podman 5 clamps
# with --source-date-epoch --rewrite-timestamp and rejects --timestamp beside
# them. Podman 4 (Ubuntu 24.04, the CI runners) has only --timestamp, which pins
# the image and layer times to the epoch. Either way a build and its
# independent rebuild on the same host agree. Extra podman global options
# (e.g. --remote=false) follow the epoch.
podman_epoch_args() {
  local epoch=$1; shift
  if podman "$@" build --help 2>/dev/null | grep -q -- '--source-date-epoch'; then
    printf '%s\n' --source-date-epoch "$epoch" --rewrite-timestamp
  else
    printf '%s\n' --timestamp "$epoch"
  fi
}

# The creation time of an image, in seconds since the epoch: from local storage
# when it is there, else from its registry.
tilo_image_epoch() {
  local created
  created="$(podman image inspect --format '{{.Created.Unix}}' "$1" 2>/dev/null)" \
    || created="$(skopeo inspect --format '{{.Created.Unix}}' "docker://$1" 2>/dev/null)" \
    || return 1
  [[ "$created" =~ ^[1-9][0-9]*$ ]] || return 1
  printf '%s\n' "$created"
}

# When the source of a local image was made, as an RFC 3339 UTC time: its
# org.tilo.source-date-epoch label (the commit time; a local build's layers may
# be dated by its base instead, TILO_LAYER_EPOCH=base), else the image's
# creation time. Empty when neither is readable.
tilo_image_source_time() {
  local epoch
  epoch="$(podman image inspect --format '{{index .Labels "org.tilo.source-date-epoch"}}' "$1" 2>/dev/null || true)"
  if [[ "$epoch" =~ ^[1-9][0-9]*$ ]]; then
    date -u -d "@$epoch" +%Y-%m-%dT%H:%M:%SZ
  else
    podman image inspect --format '{{.Created.UTC.Format "2006-01-02T15:04:05Z"}}' "$1" 2>/dev/null || true
  fi
}

# Flags every local `podman build` of an OS image takes:
#   --jobs  independent stages build in parallel (podman's default is one at a
#           time; BuildKit in CI already runs them concurrently).
#           TILO_PODMAN_BUILD_JOBS overrides it.
#   label=disable  RUN steps bind-mount files straight from the build context
#           (RUN --mount=type=bind,source=os/...). Under SELinux such a file
#           keeps its host label (var_t on a data disk, user_home_t in a home
#           directory), which the container may not read: "Permission denied"
#           from a script that exists. The build's RUN containers are not what
#           SELinux protects the host from; the image's own labels are unchanged.
TILO_PODMAN_BUILD_FLAGS=(--jobs "${TILO_PODMAN_BUILD_JOBS:-4}" --security-opt label=disable)
# An x86 builder with registered qemu-user can produce the aarch64 board image.
# The flag is absent when the machine already is that architecture, so a native
# ARM build is unchanged.
if [ -n "${TILO_BUILD_ARCH:-}" ] && [ "$TILO_BUILD_ARCH" != "$(uname -m)" ]; then
  case "$TILO_BUILD_ARCH" in
    aarch64) _tilo_platform=linux/arm64 ;;
    x86_64) _tilo_platform=linux/amd64 ;;
    *) echo "TILO_BUILD_ARCH must be x86_64 or aarch64, not $TILO_BUILD_ARCH" >&2; exit 1 ;;
  esac
  case " ${TILO_PODMAN_BUILD_FLAGS[*]} " in
    *" $_tilo_platform "*) ;;
    *) TILO_PODMAN_BUILD_FLAGS+=(--platform "$_tilo_platform") ;;
  esac
  unset _tilo_platform
fi

# Parallel cargo jobs for an in-image compile: a CPU per job, as far as memory
# holds out at $1 GiB a job; 8 when memory cannot be read.
cargo_build_jobs() {
  local per_job=$1 cpus mem_jobs
  cpus="$(nproc 2>/dev/null || echo 4)"
  mem_jobs="$(awk -v g="$per_job" '/^MemTotal:/ { print int($2 / (g * 1048576)) }' /proc/meminfo 2>/dev/null || true)"
  [ -n "$mem_jobs" ] && [ "$mem_jobs" -ge 1 ] || mem_jobs=8
  [ "$cpus" -le "$mem_jobs" ] || cpus="$mem_jobs"
  printf '%s\n' "$cpus"
}

# The two digests of bootc install's composefs refusal, read from its log $1:
#   The UKI has the wrong composefs= parameter (is '<A>', should be '<B>')
# Older bootc prefixed both with `sha512:` and left <B> unquoted; accept either.
# Print nothing, and still succeed, when the log holds no such refusal.
composefs_expected_digest() {
  { sed -n "s/.*should be '\{0,1\}\(sha512:\)\{0,1\}\([0-9a-f]\{128\}\)'\{0,1\}.*/\2/p" "$1" 2>/dev/null || true; } | tail -1
}
composefs_sealed_digest() {
  { sed -n "s/.*is '\(sha512:\)\{0,1\}\([0-9a-f]\{128\}\)'.*/\2/p" "$1" 2>/dev/null || true; } | tail -1
}

# `podman run` flags that give files a build container writes to bind mounts this user's
# ownership. Rootless, --userns=keep-id does it; as root (a build container's root, such as
# a remote builder's, whose user namespace maps only 65536 ids) there is nothing to keep,
# and keep-id would make podman copy and re-own each image layer, which fails there on ids
# outside that range.
keep_id_args() {
  [ "$(id -u)" -eq 0 ] || printf '%s\n' --userns=keep-id
}

warn_slow_podman_storage() {
  local root="${CONTAINERS_STORAGE_ROOT:-${XDG_DATA_HOME:-$HOME/.local/share}/containers/storage}"
  [ "$(cat "$root/overlay/.has-mount-program" 2>/dev/null)" = true ] || return 0
  warn "podman stores images through fuse-overlayfs ($root/overlay/.has-mount-program),
  so every image build step is several times slower than on native overlay. This
  kernel supports native rootless overlay; switching needs a fresh store:
  export the containers and images you need, then run \`podman system reset\`."
}

# --- run a command in the build/VM environment (distrobox or host) ------------
# Usage: inenv 'shell command string'
# Runs with the *current working directory preserved* (distrobox does this), so
# relative paths and the cargo workspace resolve correctly.
inenv() {
  # Already inside a container (the distrobox itself, a build container), or no
  # distrobox here: the tools are where this runs.
  if [ -n "$TILO_DISTROBOX" ] && [ ! -e /run/.containerenv ] && command -v distrobox >/dev/null; then
    distrobox enter "$TILO_DISTROBOX" -- bash -lc "$1"
  else
    bash -lc "$1"
  fi
}

# Run tilo-vm where the VM tools live. It builds every QEMU command line
# (`tilo-vm qemu`), so firmware, TPM and device choices exist in one place.
# TILO_VM_BIN names a built tilo-vm (CI passes tilo-vm-binary-<arch>'s);
# otherwise this checkout's is built on first use, in the same environment.
tilo_vm() {
  if [ -z "${TILO_VM_BIN:-}" ]; then
    TILO_VM_BIN="$(inenv "cd $(printf '%q' "$TILO_REPO") && cargo build --quiet --locked -p tilo-vm --bin tilo-vm --message-format=json-render-diagnostics" \
      | python3 -c 'import json, sys; print(next(m["executable"] for m in map(json.loads, sys.stdin) if m.get("executable") and m["target"]["name"] == "tilo-vm"))')" \
      || die "building tilo-vm failed; set TILO_VM_BIN to a built tilo-vm"
    export TILO_VM_BIN
  fi
  inenv "$(printf '%q ' "$TILO_VM_BIN" "$@")"
}

# Send one HMP monitor command to the running VM, print its reply.
hmp() {
  # The monitor socket is on the host-visible TILO_RUN mount. Prefer the host's
  # socat even when QEMU itself lives in a distrobox; entering the container for
  # every individual key made a login take minutes instead of seconds.
  if command -v socat >/dev/null 2>&1; then
    printf '%s\n' "$1" | socat - "unix:$TILO_MON"
  else
    inenv "printf '%s\n' \"$1\" | socat - 'unix:$TILO_MON'"
  fi
}

# One QMP command on the socket TILO_VM_INPUT=touch opens, after the
# capabilities handshake every QMP session starts with.
qmp() {
  local session
  session="$(printf '{"execute":"qmp_capabilities"}\n%s\n' "$(printf '%s' "$1" | tr -d '\n')")"
  if command -v socat >/dev/null 2>&1; then
    printf '%s\n' "$session" | socat -t1 - "unix:$TILO_QMP" | grep -q '"error"' && return 1
  else
    printf '%s\n' "$session" | inenv "socat -t1 - 'unix:$TILO_QMP'" | grep -q '"error"' && return 1
  fi
  return 0
}

# Run a command in the guest over SSH (password auth via sshpass, on the host
# since that's where the forwarded port binds; with TILO_SSH_IDENTITY, that key
# instead, as vm.sh hands the guest root's public key in TILO_VM_ROOT_KEY_FILE).
# Returns the command's output.
gssh() {
  if [ -n "${TILO_SSH_IDENTITY:-}" ]; then
    ${TILO_SSH_BOUND:+timeout -k 2 "$TILO_SSH_BOUND"} \
      ssh -F /dev/null -p "$TILO_SSH_PORT" -i "$TILO_SSH_IDENTITY" -o IdentitiesOnly=yes \
      -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
      -o ConnectTimeout=5 -o ServerAliveInterval=10 -o ServerAliveCountMax=6 \
      -o LogLevel=ERROR -o BatchMode=yes -o PreferredAuthentications=publickey \
      "$TILO_USER@127.0.0.1" "$@"
    return
  fi
  # VM automation must never fall through to the desktop's graphical askpass
  # helper. sshpass owns the one permitted password prompt; if it cannot
  # answer, fail the test instead of disrupting the physical session.
  command -v sshpass >/dev/null 2>&1 || die "sshpass is required for unattended VM access"
  # A probe sets TILO_SSH_BOUND to the seconds it may take: a guest that
  # accepts the connection and then stalls holds ssh in its key exchange,
  # where neither timeout below applies.
  env -u SSH_ASKPASS -u SSH_ASKPASS_REQUIRE DISPLAY= WAYLAND_DISPLAY= \
    ${TILO_SSH_BOUND:+timeout -k 2 "$TILO_SSH_BOUND"} \
    sshpass -p "$TILO_PASS" ssh -F /dev/null -p "$TILO_SSH_PORT" \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=5 -o ServerAliveInterval=10 -o ServerAliveCountMax=6 \
    -o LogLevel=ERROR -o BatchMode=no \
    -o NumberOfPasswordPrompts=1 -o PreferredAuthentications=password \
    -o PubkeyAuthentication=no -o KbdInteractiveAuthentication=no \
    "$TILO_USER@127.0.0.1" "$@"
}

# --- the machine ---------------------------------------------------------------
# tilo-vm builds the QEMU command line (`tilo-vm qemu --help` explains every
# choice: firmware and what it can measure, TPM, display, input, network). These
# are this toolkit's settings for it; qemu_flags turns them into its flags.
#
# TILO_SECBOOT=1 keeps the pristine variable store across boots so an enrolment
# in the guest sticks. TILO_SECBOOT_KEYS picks the store's keys: none (setup
# mode, still measures; re-seeded every boot outside a named cycle), persist,
# microsoft (vendor keys: an owner-signed UKI does not boot), or the path of a
# prepared store. TILO_OVMF_DIRS / TILO_AAVMF_DIRS say where to look for the
# firmware (a Homebrew QEMU on a Mac names its edk2 directory here).
TILO_SECBOOT="${TILO_SECBOOT:-0}"
TILO_SECBOOT_KEYS="${TILO_SECBOOT_KEYS:-none}"
TILO_OVMF_DIRS="${TILO_OVMF_DIRS:-/usr/share/edk2/ovmf /usr/share/OVMF}"
TILO_AAVMF_DIRS="${TILO_AAVMF_DIRS:-/usr/share/edk2/aarch64 /usr/share/AAVMF /usr/share/qemu-efi-aarch64}"
# TILO_VM_S3=1 offers the guest suspend-to-RAM, which the monitor's
# `system_wakeup` resumes (x86_64 only; ARM `virt` has no S3).
TILO_VM_S3="${TILO_VM_S3:-0}"
# The installer/greeter console channel's host end (tilo-control --socket).
TILO_CONSOLE="$TILO_RUN/console.sock"
# Optional second NIC: a point-to-point Ethernet segment to one other VM on this
# host. Set all three, and the mirror image on the peer VM:
#   TILO_VM_LAN_PORT=23901 TILO_VM_LAN_PEER=23902 TILO_VM_LAN_MAC=52:54:00:23:83:0a
# TILO_VM_LAN_MODEL=e1000 gives a NIC that implements Wake-on-LAN settings.
TILO_VM_LAN_PORT="${TILO_VM_LAN_PORT:-}"
TILO_VM_LAN_PEER="${TILO_VM_LAN_PEER:-}"
TILO_VM_LAN_MAC="${TILO_VM_LAN_MAC:-}"
TILO_VM_LAN_MODEL="${TILO_VM_LAN_MODEL:-virtio-net-pci}"
# The guest's input devices: keyboard (a tablet and a keyboard), or touch (a
# virtio multitouch screen and no keyboard at all, as on a tablet with its
# keyboard detached); `vm.sh tap` touches the screen through the QMP socket.
TILO_VM_INPUT="${TILO_VM_INPUT:-keyboard}"
TILO_QMP="$TILO_RUN/qmp.sock"
# TILO_VM_AUDIO_WAV=PATH gives the guest a sound card whose output QEMU writes
# to PATH as a WAV file, so a check can hear what the guest said.
TILO_VM_AUDIO_WAV="${TILO_VM_AUDIO_WAV:-}"
# TILO_SERIAL_SOCKET=PATH also serves the serial line on a socket, so a
# process on this host can type into the guest; TILO_VNC_SOCKET=PATH serves the
# display as VNC on a socket, so one can watch it. Both stay unix sockets in
# the run directory: nothing here listens on the host's network.
TILO_SERIAL_SOCKET="${TILO_SERIAL_SOCKET:-}"
TILO_VNC_SOCKET="${TILO_VNC_SOCKET:-}"

# The `tilo-vm qemu` flags for these settings, in the array QEMU_FLAGS.
qemu_flags() {
  local dir
  QEMU_FLAGS=(--run "$TILO_RUN" --arch "$TILO_VM_ARCH" --qemu "$TILO_QEMU_SYSTEM"
    --accel "$TILO_ACCEL" --cpu-variant "${TILO_KVM_CPU_VARIANT:-host}"
    --memory "$TILO_MEM" --smp "$TILO_SMP" --resolution "${TILO_XRES}x${TILO_YRES}"
    --input "$TILO_VM_INPUT" --keys "$TILO_SECBOOT_KEYS" --tpm-dir "$TILO_TPM_DIR")
  [ "$TILO_TPM" != 1 ] || QEMU_FLAGS+=(--tpm)
  [ "$TILO_SECBOOT" != 1 ] || QEMU_FLAGS+=(--secboot)
  [ "$TILO_VM_S3" != 1 ] || QEMU_FLAGS+=(--s3)
  [ -z "$TILO_SERIAL_SOCKET" ] || QEMU_FLAGS+=(--serial-socket)
  [ -z "$TILO_VNC_SOCKET" ] || QEMU_FLAGS+=(--vnc "$TILO_VNC_SOCKET")
  # A named installation cycle keeps its firmware state from the installer
  # boot to the target boot.
  [ -z "$TILO_VM_CYCLE" ] || QEMU_FLAGS+=(--keep-vars)
  [ -z "${TILO_RENDERNODE:-}" ] || QEMU_FLAGS+=(--rendernode "$TILO_RENDERNODE")
  for dir in $TILO_OVMF_DIRS; do QEMU_FLAGS+=(--ovmf-dir "$dir"); done
  for dir in $TILO_AAVMF_DIRS; do QEMU_FLAGS+=(--aavmf-dir "$dir"); done
}

# Stop the software TPM of this toolkit's VM, if one serves its socket.
swtpm_stop() {
  [ -S "$TILO_TPM_DIR/swtpm-ctrl.sock" ] || return 0
  tilo_vm qemu stop --run "$TILO_RUN" --tpm-dir "$TILO_TPM_DIR" >/dev/null
}

# --- explaining a boot that produced nothing ----------------------------------
# Every failure this file guards against has the same shape: the harness could
# not observe something and would otherwise report that as absence. So the paths
# that give up on a boot call this first, and it names the cause and the remedy
# from the evidence on the host. $1 = serial log. Returns 0 if it explained
# something.
#
# Patterns are matched against the file, never against a captured copy piped into
# grep. This file sets `set -o pipefail`, and under it `printf "$text" | grep -q P`
# reports NO MATCH for any log larger than the 64 KB pipe buffer: grep -q exits at
# the first hit, printf dies of SIGPIPE, and pipefail turns that into a failed
# pipeline. That is this file's own bug in miniature — a check that cannot observe
# reporting absence — and it silently mis-diagnosed a boot whose serial log had
# grown past 64 KB, which is every boot that reaches userspace.
_serial_has() {   # $1 = log, $2 = extended regex
  [ -n "$1" ] && [ -f "$1" ] && grep -qiaE "$2" "$1"
}

boot_diagnose() {
  local log="${1:-}" empty=1
  [ -n "$log" ] && [ -s "$log" ] && empty=0

  if _serial_has "$log" 'A TPM error \(256\)|TPM_RC_INITIALIZE'; then
    warn "the guest found a TPM nobody had started (error 256), and started it by"
    warn "hand — long after the boot it was supposed to measure. Expect every PCR"
    warn "to read zero, no event log, and ConditionSecurity=measured-uki unmet."
    warn "  Two things start a TPM, and neither did: the firmware issues"
    warn "  TPM2_Startup only if it has TPM support (the 2 MB OVMF build does not),"
    warn "  and swtpm issues it itself only with --flags not-need-init,startup-clear."
    warn "  \`vm.sh firmware\` prints the firmware in use and whether it measures."
    return 0
  fi
  if [ "$empty" = 1 ] && [ "$TILO_SECBOOT_KEYS" != "none" ] && [ "$TILO_SECBOOT_KEYS" != "persist" ]; then
    warn "nothing at all on the serial line, with '$TILO_SECBOOT_KEYS' keys in the"
    warn "variable store: the firmware refuses an image signed with any other key"
    warn "before it prints a character, so this is what a rejected image looks like."
    warn "  Remedy: TILO_SECBOOT_KEYS=none (setup mode — still measures), or enrol"
    warn "  the signing certificate in the guest and keep the store (--secboot)."
    return 0
  fi
  if [ "$TILO_TPM" = "1" ] && [ "$empty" = 0 ] && \
     ! _serial_has "$log" 'tpm_tis|tpm_crb|TPM.?2\.0|tpm[0-9]'; then
    warn "a TPM was attached but the guest never reported one: the firmware did not"
    warn "expose TCG2, so PCR and event-log checks in the guest would report"
    warn "'nothing measured' whatever the kernel did. \`vm.sh firmware --tpm\`"
    warn "prints the firmware in use and whether it measures."
    return 0
  fi
  return 1
}
