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
# emulated TPM across a reboot. Attaching a TPM also pins the firmware choice —
# see the OVMF section below.
TILO_TPM="${TILO_TPM:-0}"
TILO_TPM_DIR="${TILO_TPM_DIR:-$TILO_RUN/tpm}"
TILO_TPM_SOCK="$TILO_TPM_DIR/swtpm-ctrl.sock"

# Guest login (from os/account.env — the account disk-image.sh creates).
TILO_USER="${TILO_USER:-quickuser}"
TILO_PASS="${TILO_PASS:-tilo}"

TILO_MEM="${TILO_MEM:-4096}"
TILO_SMP="${TILO_SMP:-4}"
# `auto` uses hardware acceleration when this environment owns a usable KVM
# device, and otherwise falls back to QEMU's multithreaded software emulator.
# The fallback is intentionally slower, but lets the release gate run inside a
# Kata-isolated CI runner without exposing the Kubernetes node's /dev/kvm.
TILO_ACCEL="${TILO_ACCEL:-auto}"
TILO_BOOT_WAIT_ATTEMPTS="${TILO_BOOT_WAIT_ATTEMPTS:-90}"
TILO_QEMU_START_ATTEMPTS="${TILO_QEMU_START_ATTEMPTS:-100}"

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

qemu_accel_args() {
  case "$TILO_ACCEL" in
    auto)
      # Kata can expose a /dev/kvm node while denying KVM_CREATE_VM.
      local probe
      printf -v probe 'python3 %q %q' "$_COMMON_DIR/probe-kvm.py" "$TILO_VM_ARCH"
      if inenv "$probe"; then
        echo "-enable-kvm -cpu host"
      else
        echo "-accel tcg,thread=multi -cpu max"
      fi
      ;;
    kvm) echo "-enable-kvm -cpu host" ;;
    tcg) echo "-accel tcg,thread=multi -cpu max" ;;
    *) die "TILO_ACCEL must be auto, kvm, or tcg (got $TILO_ACCEL)" ;;
  esac
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

# Keep the distrobox command scope alive until QEMU has finished its cold-start
# work and published the monitor socket. Returning immediately after `disown`
# races distrobox's scope cleanup on large, cold images and can kill a healthy
# QEMU before the caller's separate one-second probe sees it.
start_qemu() {
  local args="$1"
  inenv "setsid nohup '$TILO_QEMU_SYSTEM' $args >'$TILO_QEMU_LOG' 2>&1 &
    qemu_pid=\$!
    for _ in \$(seq 1 '$TILO_QEMU_START_ATTEMPTS'); do
      test -S '$TILO_MON' && exit 0
      kill -0 \$qemu_pid 2>/dev/null || break
      sleep 0.1
    done
    wait \$qemu_pid 2>/dev/null || true
    exit 1"
}

# Run a command in the guest over SSH (password auth via sshpass, on the host
# since that's where the forwarded port binds). Returns the command's output.
gssh() {
  # VM automation must never fall through to the desktop's graphical askpass
  # helper. sshpass owns the one permitted password prompt; if it cannot
  # answer, fail the test instead of disrupting the physical session.
  command -v sshpass >/dev/null 2>&1 || die "sshpass is required for unattended VM access"
  env -u SSH_ASKPASS -u SSH_ASKPASS_REQUIRE DISPLAY= WAYLAND_DISPLAY= \
    sshpass -p "$TILO_PASS" ssh -F /dev/null -p "$TILO_SSH_PORT" \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=5 -o LogLevel=ERROR -o BatchMode=no \
    -o NumberOfPasswordPrompts=1 -o PreferredAuthentications=password \
    -o PubkeyAuthentication=no -o KbdInteractiveAuthentication=no \
    "$TILO_USER@127.0.0.1" "$@"
}

# Pick a GL render node for virglrenderer. NVIDIA (0x10de) is skipped: its
# proprietary/open EGL stack works poorly with virglrenderer; a Mesa iGPU
# (AMD/Intel) is strongly preferred. Override with TILO_RENDERNODE.
detect_rendernode() {
  if [ -n "${TILO_RENDERNODE:-}" ]; then echo "$TILO_RENDERNODE"; return; fi
  local n v
  for n in /dev/dri/renderD*; do
    [ -e "$n" ] || continue
    v="$(cat "/sys/class/drm/$(basename "$n")/device/vendor" 2>/dev/null || echo)"
    [ "$v" = "0x10de" ] && continue
    echo "$n"; return
  done
  echo /dev/dri/renderD128
}

# --- OVMF firmware ------------------------------------------------------------
# Fedora ships two OVMF builds and only one of them can measure:
#
#   OVMF_CODE_4M.secboot.qcow2  4 MB. TPM support compiled in. Its TCG2 protocol
#                               reports an active SHA-256 PCR bank, so the
#                               firmware starts the TPM, extends PCRs 0-7 itself,
#                               writes an event log, and hands systemd-stub a
#                               TCG2 it can extend PCR 11 through.
#   OVMF_CODE.secboot.fd        2 MB. No TPM support. TCG2 reports ZERO active
#                               PCR banks, so nothing is ever extended: every PCR
#                               reads zero, no firmware event log appears, the TPM
#                               is never started (Linux logs "A TPM error (256)
#                               occurred attempting the self test") and
#                               ConditionSecurity=measured-uki is unmet.
#
# That second list is exactly what a UKI which failed to measure looks like, so
# the 4 MB build is a requirement here rather than a preference: under the 2 MB
# one os/tests/rollback-boot-check.sh reports "nothing measured into PCR 11" for a
# kernel that measured perfectly, and no amount of guest-side checking can tell
# the two apart. When a TPM is attached and the 4 MB build is absent this harness
# refuses to boot and says how to get it, instead of producing a result that
# cannot mean anything.
#
# The secboot flavour is chosen for its measurement support, not to enforce
# anything: Secure Boot enforcement is not a qemu flag, it is a property of the
# variable store (see TILO_SECBOOT_KEYS). SMM and a write-protected pflash go with
# the secboot build unconditionally, because that firmware expects them and a
# variable store the guest can rewrite makes enforcement theatre.
TILO_SECBOOT="${TILO_SECBOOT:-0}"

# Which keys the variable store starts with. This decides whether an owner-signed
# image is bootable at all, so it is a deliberate choice rather than whichever
# template happens to sit next to the firmware:
#
#   none       (default) OVMF_VARS_4M.qcow2 — no keys, Secure Boot in setup mode.
#              Measures into PCR 7 like any other store, and accepts an
#              owner-signed UKI. Re-seeded from the template on every boot, so
#              every boot starts from the same firmware state.
#   persist    the same pristine template, but kept across boots so that an
#              enrolment performed in the guest survives a restart of qemu. This
#              is what `--secboot` selects: enrolment is then an explicit act
#              (`/usr/libexec/tilo-secureboot enroll` in the guest), not something
#              the template did for us.
#   microsoft  OVMF_VARS_4M.secboot.qcow2 — Microsoft's KEK and db. NOT "the
#              secure one": a Tilo UKI is signed with the owner's key, which that
#              db does not cover, so under it nothing boots and the firmware says
#              nothing about why. Selecting it is an explicit request to test
#              vendor-key enforcement.
#   <path>     a prepared store, used as-is and kept across boots.
TILO_SECBOOT_KEYS="${TILO_SECBOOT_KEYS:-none}"

# Where to look for the firmware. Overridable so the "this firmware cannot
# measure" refusal can be exercised against a directory holding only the 2 MB
# build, instead of being a branch nobody ever takes.
TILO_OVMF_DIRS="${TILO_OVMF_DIRS:-/usr/share/edk2/ovmf /usr/share/OVMF}"
TILO_AAVMF_DIRS="${TILO_AAVMF_DIRS:-/usr/share/edk2/aarch64 /usr/share/AAVMF /usr/share/qemu-efi-aarch64}"

# Filled in by firmware_setup; read by machine_args/secboot_qemu_args/pflash_args.
TILO_FW_CODE=""; TILO_FW_VARS=""; TILO_FW_FMT=""; TILO_FW_MEASURES=0; TILO_FW_SMM=0

# Echo the first of $@ that exists in the env, or nothing.
_env_first_file() {
  local f list=""
  for f in "$@"; do list="$list '$f'"; done
  inenv "for f in $list; do [ -f \"\$f\" ] && { printf '%s\n' \"\$f\"; exit 0; }; done; exit 1" \
    2>/dev/null || true
}

# Resolve the firmware for the current settings into TILO_FW_*, or die with the
# remedy. Must be called before machine_args / secboot_qemu_args / pflash_args:
# the pflash format is part of the answer (the 4 MB build ships as qcow2, the
# 2 MB one as raw, and the wrong `format=` makes qemu read firmware as garbage).
firmware_setup() {
  if [ "$TILO_SECBOOT" = "1" ] && [ "$TILO_SECBOOT_KEYS" = "none" ]; then
    TILO_SECBOOT_KEYS=persist
    msg "--secboot: variable store will persist across boots so an enrolment sticks"
  fi

  local dir d cands four_mb=0
  if [ "$TILO_VM_ARCH" = aarch64 ]; then
    # AAVMF/QEMU_EFI is distributed under different names by Fedora and
    # Debian-family hosts. Both layouts provide a writable, blank variable-store
    # template next to the read-only code image. This release cycle deliberately
    # exercises the strong-passphrase path; do not imply measured boot from an
    # arbitrary AAVMF build merely because it happens to expose a TPM device.
    [ "$TILO_TPM" = 0 ] || die "the aarch64 VM cycle has no verified measured-AAVMF profile yet.
  Run the release-grade ARM installer cycle with --no-tpm; TPM enrollment remains
  disabled in Tilo until signed measured-boot predictions are implemented."
    [ "$TILO_SECBOOT_KEYS" = none ] || die "aarch64 VM Secure Boot key stores are not verified yet;
  use TILO_SECBOOT_KEYS=none for the ARM installer cycle"
    cands=""
    for d in $TILO_AAVMF_DIRS; do
      cands="$cands $d/QEMU_EFI-pflash.raw $d/AAVMF_CODE.fd"
    done
    # shellcheck disable=SC2086
    TILO_FW_CODE="$(_env_first_file $cands)"
    [ -n "$TILO_FW_CODE" ] || die "no AAVMF firmware in ${TILO_DISTROBOX:-the host}.
  Remedy: install edk2-aarch64 (Fedora) or qemu-efi-aarch64 (Debian/Ubuntu)."
    dir="$(dirname "$TILO_FW_CODE")"
    case "$(basename "$TILO_FW_CODE")" in
      QEMU_EFI-pflash.raw)
        TILO_FW_VARS="$dir/vars-template-pflash.raw" ;;
      AAVMF_CODE.fd)
        TILO_FW_VARS="$dir/AAVMF_VARS.fd" ;;
      *) die "unsupported AAVMF code image: $TILO_FW_CODE" ;;
    esac
    [ -n "$(_env_first_file "$TILO_FW_VARS")" ] || die "AAVMF variable-store template missing: $TILO_FW_VARS"
    TILO_FW_FMT=raw
    TILO_FW_MEASURES=0
    TILO_FW_SMM=0
    msg "firmware: $(basename "$TILO_FW_CODE") [raw] keys=none measures=unverified smm=0"
    return
  fi

  cands=""; for d in $TILO_OVMF_DIRS; do cands="$cands $d/OVMF_CODE_4M.secboot.qcow2"; done
  # shellcheck disable=SC2086
  TILO_FW_CODE="$(_env_first_file $cands)"
  if [ -n "$TILO_FW_CODE" ]; then
    TILO_FW_FMT=qcow2; TILO_FW_MEASURES=1; TILO_FW_SMM=1
    dir="$(dirname "$TILO_FW_CODE")"
    case "$TILO_SECBOOT_KEYS" in
      none|persist) TILO_FW_VARS="$dir/OVMF_VARS_4M.qcow2" ;;
      microsoft)    TILO_FW_VARS="$dir/OVMF_VARS_4M.secboot.qcow2" ;;
      *)            TILO_FW_VARS="$TILO_SECBOOT_KEYS" ;;
    esac
  else
    # Debian and Ubuntu ship the 4 MB build as raw .fd (OVMF_CODE_4M*.fd with
    # OVMF_VARS_4M*.fd) and no 2 MB one; Fedora's 2 MB build is OVMF_CODE*.fd.
    cands=""
    for d in $TILO_OVMF_DIRS; do cands="$cands $d/OVMF_CODE_4M.secboot.fd $d/OVMF_CODE_4M.fd"; done
    for d in $TILO_OVMF_DIRS; do cands="$cands $d/OVMF_CODE.secboot.fd"; done
    for d in $TILO_OVMF_DIRS; do cands="$cands $d/OVMF_CODE.fd"; done
    # shellcheck disable=SC2086
    TILO_FW_CODE="$(_env_first_file $cands)"
    [ -n "$TILO_FW_CODE" ] || die "no OVMF firmware in ${TILO_DISTROBOX:-the host} (looked in $TILO_OVMF_DIRS).
  Remedy: distrobox enter ${TILO_DISTROBOX:-…} -- sudo dnf install edk2-ovmf (Fedora), or apt install ovmf (Debian/Ubuntu)"
    case "$TILO_FW_CODE" in *_4M*) four_mb=1 ;; *) four_mb=0 ;; esac
    if [ "$TILO_TPM" = "1" ] && [ "$four_mb" = 1 ]; then
      die "the OVMF build here ($TILO_FW_CODE) is the Debian-family 4 MB raw image,
  whose measured boot this toolkit has not verified. Refusing to attach a TPM
  whose PCRs would not be evidence of anything.
  Remedy: use a host with Fedora's edk2-ovmf, which ships
    /usr/share/edk2/ovmf/OVMF_CODE_4M.secboot.qcow2 + OVMF_VARS_4M.qcow2"
    fi
    if [ "$TILO_TPM" = "1" ]; then
      die "the only OVMF build here is the 2 MB one ($TILO_FW_CODE), which has no TPM
  support: its TCG2 protocol reports zero active PCR banks, so nothing is ever
  measured. Every PCR would read zero and no event log would appear — the same
  evidence a UKI that failed to measure leaves behind — so a measured-boot check
  would fail for a reason that has nothing to do with the kernel. Refusing to
  attach a TPM to a firmware that cannot measure into it.
  Remedy: install a current edk2-ovmf in ${TILO_DISTROBOX:-the host}; it ships
    /usr/share/edk2/ovmf/OVMF_CODE_4M.secboot.qcow2 + OVMF_VARS_4M.qcow2
    (distrobox enter ${TILO_DISTROBOX:-…} -- sudo dnf install edk2-ovmf)"
    fi
    TILO_FW_FMT=raw; TILO_FW_MEASURES=0
    case "$TILO_FW_CODE" in *.secboot.fd) TILO_FW_SMM=1 ;; *) TILO_FW_SMM=0 ;; esac
    dir="$(dirname "$TILO_FW_CODE")"
    if [ "$four_mb" = 1 ]; then
      warn "the Debian-family OVMF build ($TILO_FW_CODE) is not a verified measuring"
      warn "firmware here: no PCR, event-log or measured-boot result from this VM is evidence."
      case "$TILO_SECBOOT_KEYS" in
        none|persist) TILO_FW_VARS="$dir/OVMF_VARS_4M.fd" ;;
        microsoft)    TILO_FW_VARS="$dir/OVMF_VARS_4M.ms.fd" ;;
        *)            TILO_FW_VARS="$TILO_SECBOOT_KEYS" ;;
      esac
    else
      warn "the 2 MB OVMF build ($TILO_FW_CODE) cannot measure: no PCR, event-log or"
      warn "measured-boot result from this VM is evidence of anything."
      case "$TILO_SECBOOT_KEYS" in
        none|persist) TILO_FW_VARS="$dir/OVMF_VARS.fd" ;;
        microsoft)    TILO_FW_VARS="$dir/OVMF_VARS.secboot.fd" ;;
        *)            TILO_FW_VARS="$TILO_SECBOOT_KEYS" ;;
      esac
    fi
  fi

  if [ -z "$(_env_first_file "$TILO_FW_VARS")" ]; then
    case "$TILO_SECBOOT_KEYS" in
      none|persist|microsoft)
        die "OVMF variable-store template missing: $TILO_FW_VARS
  Remedy: distrobox enter ${TILO_DISTROBOX:-…} -- sudo dnf install edk2-ovmf" ;;
      *)
        die "TILO_SECBOOT_KEYS=$TILO_SECBOOT_KEYS names a variable store that does not
  exist. Enrolling an owner key is a deliberate step, not something a boot does
  on the way past: boot with TILO_SECBOOT=1 (pristine store, setup mode, kept
  across boots), run \`/usr/libexec/tilo-secureboot enroll\` in the guest, and
  reboot the guest — the store then carries the owner PK/KEK/db." ;;
    esac
  fi

  if [ "$TILO_SECBOOT_KEYS" = "microsoft" ]; then
    warn "variable store: Microsoft's keys ($TILO_FW_VARS). The firmware refuses an"
    warn "image signed with any other key before it prints anything, so an"
    warn "owner-signed UKI produces a guest that never appears. Use"
    warn "TILO_SECBOOT_KEYS=none (setup mode — still measures) unless you are"
    warn "specifically testing vendor-key enforcement."
  fi

  msg "firmware: $(basename "$TILO_FW_CODE") [$TILO_FW_FMT] keys=$TILO_SECBOOT_KEYS measures=$TILO_FW_MEASURES smm=$TILO_FW_SMM"
}

# Seed and echo the working variable store for this VM. `none` is re-seeded every
# boot for ad-hoc VM use, but is persisted inside a named installation cycle;
# every other selection is kept, because an
# enrolment a restart silently threw away is indistinguishable from one that
# never happened. $1 = run dir (defaults to TILO_RUN).
firmware_varstore() {
  local run="${1:-$TILO_RUN}" tag out
  tag="$(printf '%s' "$TILO_SECBOOT_KEYS" | tr -c 'a-zA-Z0-9' '-')"
  out="$run/vars-$TILO_FW_FMT-$tag.img"
  if [ "$TILO_SECBOOT_KEYS" = "none" ] && [ -z "$TILO_VM_CYCLE" ]; then
    inenv "mkdir -p '$run' && cp -f '$TILO_FW_VARS' '$out'"
  else
    inenv "mkdir -p '$run' && { test -f '$out' || cp -f '$TILO_FW_VARS' '$out'; }"
  fi
  printf '%s\n' "$out"
}

# The two -drive lines for the firmware. $1 = working variable store.
pflash_args() {
  printf -- '-drive if=pflash,format=%s,readonly=on,file=%s -drive if=pflash,format=%s,file=%s' \
    "$TILO_FW_FMT" "$TILO_FW_CODE" "$TILO_FW_FMT" "$1"
}

# Extra qemu args the Secure-Boot firmware needs: SMM, and a pflash that refuses
# writes from outside SMM. Without both, the variable store is writable from the
# guest and enforcement is theatre.
secboot_qemu_args() {
  [ "$TILO_FW_SMM" = "1" ] || return 0
  # disable_s3: the SMM-protected variable store and S3 resume do not coexist in
  # OVMF, and leaving S3 on makes the firmware complain and fall back.
  printf -- '-global driver=cfi.pflash01,property=secure,value=on -global ICH9-LPC.disable_s3=1'
}
# q35 machine options; SMM is required by the Secure-Boot firmware.
machine_args() {
  if [ "$TILO_VM_ARCH" = aarch64 ]; then
    printf -- '-machine virt,gic-version=max'
    return
  fi
  if [ "$TILO_FW_SMM" = "1" ]; then
    printf -- '-machine q35,smm=on'
  else
    printf -- '-machine q35'
  fi
}

# QEMU's VGA-compatible virtio device is specific to PC machines. ARM's `virt`
# board uses the PCI virtio-gpu variant, which AAVMF exposes through GOP and the
# graphical installer can modeset in exactly the same way after ExitBootServices.
display_qemu_args() {
  local mode="$1" res="xres=$TILO_XRES,yres=$TILO_YRES" rn
  case "$mode:$TILO_VM_ARCH" in
    gl:x86_64)
      rn="$(detect_rendernode)"
      printf -- '-device virtio-vga-gl,%s -display egl-headless,rendernode=%s' "$res" "$rn" ;;
    gl:aarch64)
      rn="$(detect_rendernode)"
      printf -- '-device virtio-gpu-gl-pci,%s -display egl-headless,rendernode=%s' "$res" "$rn" ;;
    window:x86_64)
      printf -- '-device virtio-vga-gl,%s -display gtk,gl=on' "$res" ;;
    window:aarch64)
      printf -- '-device virtio-gpu-gl-pci,%s -display gtk,gl=on' "$res" ;;
    2d:x86_64)
      printf -- '-device virtio-vga,%s -display none' "$res" ;;
    2d:aarch64)
      printf -- '-device virtio-gpu-pci,%s -display none' "$res" ;;
    *) die "unsupported display mode/architecture: $mode/$TILO_VM_ARCH" ;;
  esac
}

# Console channel. The installer and the greeter answer the tilo-control action
# protocol (actions list / actions invoke / state-dump; see
# crates/session/console-control) on a virtio-serial port named
# org.tilo.console.0. This command line is what creates that port; its host end
# is the Unix socket $TILO_CONSOLE, reachable only by the user running QEMU.
# Physical machines have no virtio-serial bus, so the channel cannot exist there.
TILO_CONSOLE="$TILO_RUN/console.sock"
console_qemu_args() {
  printf -- '-device virtio-serial-pci -chardev socket,id=tiloconsole,path=%s,server=on,wait=off -device virtserialport,chardev=tiloconsole,name=org.tilo.console.0' \
    "$TILO_CONSOLE"
}

input_qemu_args() {
  if [ "$TILO_VM_ARCH" = aarch64 ]; then
    # ARM virt has no implicit PC keyboard; monitor sendkey needs a guest device.
    printf -- '-device qemu-xhci -device usb-tablet -device usb-kbd'
  else
    printf -- '-usb -device usb-tablet'
  fi
}

# There are deliberately no single-value ovmf_code/ovmf_vars accessors any more.
# Read in a command substitution they swallowed the refusals above — the caller
# got an empty string and carried on booting something unmeasurable, which is the
# class of bug this file exists to remove.

# --- software TPM (swtpm) -----------------------------------------------------
# Start a persistent swtpm 2.0 socket inside the env (where qemu runs). Idempotent
# — reuses an already-running instance so boot-installer/boot-target share state.
swtpm_start() {
  [ "$TILO_TPM" = "1" ] || return 0
  inenv "command -v swtpm >/dev/null || { echo 'swtpm not installed in ${TILO_DISTROBOX:-env}'; exit 1; }" \
    || die "swtpm missing (sudo dnf install swtpm swtpm-tools in ${TILO_DISTROBOX:-the env})"

  # A listening emulator is not enough: swtpm enters permanent TPM failure mode
  # when its NVRAM backend cannot persist state. Catch full/quota-limited runtime
  # storage before firmware boots into a misleading PCR-service reboot loop.
  inenv "mkdir -p '$TILO_TPM_DIR' && \
    printf 'Tilo TPM state preflight\n' >'$TILO_TPM_DIR/.write-test' && \
    sync '$TILO_TPM_DIR/.write-test' && rm -f '$TILO_TPM_DIR/.write-test'" \
    || die "cannot persist software-TPM state in $TILO_TPM_DIR (filesystem full or quota exceeded)"

  # A leftover swtpm from before this fix would be reused silently and put the
  # unstarted TPM back, so check what is actually listening rather than trusting
  # the socket's existence.
  #
  # The '^' matters. `pgrep -f 'swtpm socket'` also matches the shell running this
  # very check — its command line contains the pattern, the socket path and the
  # flag name — so an unanchored match finds itself and reports every leftover as
  # correctly started. Anchoring restricts it to processes whose command line
  # begins with swtpm.
  if inenv "test -S '$TILO_TPM_SOCK'" && \
     ! inenv "pgrep -af '^swtpm socket' | grep -F -- '$TILO_TPM_SOCK' | grep -q startup-clear"; then
    warn "a swtpm without --flags startup-clear holds $TILO_TPM_SOCK; replacing it"
    swtpm_stop; sleep 1; inenv "rm -f '$TILO_TPM_SOCK'"
  fi

  # --flags not-need-init,startup-clear is what makes this a *started* TPM.
  # Without it swtpm answers every command with TPM_RC_INITIALIZE (256): the
  # firmware measures nothing, no event log is produced, every PCR reads zero, and
  # Linux logs "A TPM error (256) occurred attempting the self test" before
  # issuing TPM2_Startup itself — far too late for the boot to have been measured.
  # The evidence that leaves behind is identical to a firmware without TPM
  # support, so it is ruled out here rather than diagnosed afterwards.
  inenv "mkdir -p '$TILO_TPM_DIR'; \
    if ! test -S '$TILO_TPM_SOCK'; then \
      setsid nohup swtpm socket --tpmstate dir='$TILO_TPM_DIR' \
        --ctrl type=unixio,path='$TILO_TPM_SOCK' --tpm2 \
        --flags not-need-init,startup-clear \
        >'$TILO_TPM_DIR/swtpm.log' 2>&1 & disown; \
      for _ in \$(seq 1 50); do test -S '$TILO_TPM_SOCK' && break; sleep 0.1; done; \
    fi; \
    test -S '$TILO_TPM_SOCK'" \
    || die "swtpm failed to start (see $TILO_TPM_DIR/swtpm.log).
  If it rejected '--flags not-need-init,startup-clear', the swtpm in
  ${TILO_DISTROBOX:-the env} is too old to start the TPM itself; without that the
  firmware cannot measure. Remedy: sudo dnf install --refresh swtpm swtpm-tools"

  # Prove the emulator answers, rather than reading a socket as a working TPM.
  inenv "command -v swtpm_ioctl >/dev/null || exit 0; swtpm_ioctl -c --unix '$TILO_TPM_SOCK' >/dev/null" \
    || die "swtpm is listening on $TILO_TPM_SOCK but not answering control commands
  (see $TILO_TPM_DIR/swtpm.log). A guest booted against it would measure nothing
  and report every PCR as zero."
  msg "swtpm up (TPM 2.0, started, state=$TILO_TPM_DIR)"
}
swtpm_stop() {
  # Anchored for the same reason as the check above, and here it also stops the
  # kill from landing on the wrong process: unanchored, the pattern matches the
  # shell running pkill — so this killed its own wrapper (that is the "Terminated"
  # line every `vm.sh stop` printed) and could leave the emulator running.
  inenv "pkill -f \"^swtpm socket .*$TILO_TPM_SOCK\" 2>/dev/null || true"
}
# qemu args attaching the swtpm to the guest (empty when TPM disabled).
tpm_qemu_args() {
  [ "$TILO_TPM" = "1" ] || return 0
  printf -- '-chardev socket,id=chrtpm,path=%s -tpmdev emulator,id=tpm0,chardev=chrtpm -device tpm-tis,tpmdev=tpm0' "$TILO_TPM_SOCK"
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
    warn "  Firmware in use: ${TILO_FW_CODE:-<not selected>} (measures=${TILO_FW_MEASURES:-?})"
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
    warn "'nothing measured' whatever the kernel did. Firmware in use:"
    warn "  $TILO_FW_CODE (measures=$TILO_FW_MEASURES)"
    return 0
  fi
  return 1
}
