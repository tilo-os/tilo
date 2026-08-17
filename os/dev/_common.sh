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

# Custom (self-install) installer: a bootable live qcow2 that runs the qOS
# graphical installer (installer/, binary qos-installer). Built by
# os/dev/build-custom-installer.sh; booted by vm.sh boot-installer.
QOS_INSTALLER_IMAGE="${QOS_INSTALLER_IMAGE:-localhost/qos-installer:latest}"
QOS_INSTALLER_OUTPUT="${QOS_INSTALLER_OUTPUT:-$QOS_OUTPUT/installer}"   # distinct subdir
QOS_INSTALLER_DISK="${QOS_INSTALLER_DISK:-$QOS_INSTALLER_OUTPUT/qcow2/disk.qcow2}"

# Runtime/scratch dir for the VM. MUST be short: AF_UNIX socket paths are capped
# at 108 bytes, and a deep scratch path overflows it. /tmp/qos-vm is safe.
QOS_RUN="${QOS_RUN:-/tmp/qos-vm}"
QOS_MON="$QOS_RUN/mon.sock"
QOS_SERIAL="$QOS_RUN/serial.log"
QOS_QEMU_LOG="$QOS_RUN/qemu.log"
QOS_PIDFILE="$QOS_RUN/qemu.pid"

# The blank disk the custom installer writes onto (becomes the installed
# system), attached as a SECOND virtio disk (/dev/vdb) by vm.sh boot-installer
# and booted on its own by vm.sh boot-target.
QOS_TARGET_DISK="${QOS_TARGET_DISK:-$QOS_RUN/target.qcow2}"
QOS_TARGET_SIZE="${QOS_TARGET_SIZE:-20G}"

# Software TPM 2.0 (swtpm) so encrypted/TPM-bound installs can be booted and
# unlock-tested, and so a measured boot can be observed at all. Off by default;
# enable with `--tpm` on a boot subcommand or QOS_TPM=1. State persists in
# QOS_TPM_DIR so boot-installer (enroll) and boot-target (unlock) share the same
# emulated TPM across a reboot. Attaching a TPM also pins the firmware choice —
# see the OVMF section below.
QOS_TPM="${QOS_TPM:-0}"
QOS_TPM_DIR="${QOS_TPM_DIR:-$QOS_RUN/tpm}"
QOS_TPM_SOCK="$QOS_TPM_DIR/swtpm-ctrl.sock"

# Guest login (from os/account.env — the account disk-image.sh creates).
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

# --- the container store the image is built in --------------------------------
# A store whose overlay driver runs through a *mount program* (fuse-overlayfs)
# does not preserve a layer's bytes across a layer boundary: containers/storage
# cannot use overlayfs metadata for the changeset, so it walks and re-reads the
# tree, and a large file a transaction rewrote in place comes back with mixed
# page images. In this build that file is the rpm database, and the next dnf
# step fails against damaged indexes. os/tests/layer_capture.sh is the check;
# the Containerfile's rootfs stage carries the measurements.
#
# The condition is a property of the *store*, not the machine — a fresh store on
# the same kernel and the same btrfs uses the native driver — and it is sticky,
# because containers/storage records it once it sees fuse-overlayfs xattrs on
# any layer. So the remedy is a store that has never seen one, which is what
# this creates. It is outside the repository, persistent, and shared between
# worktrees on purpose: it holds the pinned base images, and pulling them once
# is the whole cost.
#
# On a machine whose default store is already native (CI is), nothing here
# changes: the default store is used and no second copy of anything appears.
QOS_BUILD_STORE="${QOS_BUILD_STORE:-${XDG_DATA_HOME:-$HOME/.local/share}/qos-build-store}"

# Point every later `podman` at a store that keeps a layer's bytes. Call once,
# early, from anything that builds or reads the image.
use_build_store() {
  [ -n "${QOS_BUILD_STORE_READY:-}" ] && return 0
  if bash "$QOS_OS_DIR/tests/layer_capture.sh" >/dev/null 2>&1; then
    msg "container store: default (layer changesets come from overlayfs metadata)"
    export QOS_BUILD_STORE_READY=default
    return 0
  fi
  local graph="$QOS_BUILD_STORE/graph" runroot="/run/user/$(id -u)/qos-build-store"
  local conf="$QOS_BUILD_STORE/storage.conf"
  mkdir -p "$graph" "$runroot" "$(dirname "$conf")"
  # Written every time rather than only when absent: the paths are derived, and
  # a stale file pointing at a store someone deleted is a confusing failure.
  # mountopt is spelled out because the system storage.conf on the reference
  # machine asks for metacopy=on, which rootless overlay cannot have — and
  # asking for it is by itself enough to send the probe back to fuse-overlayfs.
  cat > "$conf" <<EOF
# Generated by os/dev/_common.sh. See os/tests/layer_capture.sh.
[storage]
driver = "overlay"
graphroot = "$graph"
runroot = "$runroot"

[storage.options.overlay]
mountopt = "nodev"
EOF
  export CONTAINERS_STORAGE_CONF="$conf"
  bash "$QOS_OS_DIR/tests/layer_capture.sh" >/dev/null 2>&1 || {
    bash "$QOS_OS_DIR/tests/layer_capture.sh" || true
    die "the default container store does not preserve a layer's bytes, and the
  dedicated one at $graph does not either (see above). Building the image here
  would produce a corrupt rpm database at an unpredictable layer."
  }
  msg "container store: $graph (the default one runs through fuse-overlayfs; see os/tests/layer_capture.sh)"
  export QOS_BUILD_STORE_READY="$graph"
}

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
# variable store (see QOS_SECBOOT_KEYS). SMM and a write-protected pflash go with
# the secboot build unconditionally, because that firmware expects them and a
# variable store the guest can rewrite makes enforcement theatre.
QOS_SECBOOT="${QOS_SECBOOT:-0}"

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
#              (`/usr/libexec/qos-secureboot enroll` in the guest), not something
#              the template did for us.
#   microsoft  OVMF_VARS_4M.secboot.qcow2 — Microsoft's KEK and db. NOT "the
#              secure one": a qOS UKI is signed with the owner's key, which that
#              db does not cover, so under it nothing boots and the firmware says
#              nothing about why. Selecting it is an explicit request to test
#              vendor-key enforcement.
#   <path>     a prepared store, used as-is and kept across boots.
QOS_SECBOOT_KEYS="${QOS_SECBOOT_KEYS:-none}"

# Where to look for the firmware. Overridable so the "this firmware cannot
# measure" refusal can be exercised against a directory holding only the 2 MB
# build, instead of being a branch nobody ever takes.
QOS_OVMF_DIRS="${QOS_OVMF_DIRS:-/usr/share/edk2/ovmf /usr/share/OVMF}"

# Filled in by firmware_setup; read by machine_args/secboot_qemu_args/pflash_args.
QOS_FW_CODE=""; QOS_FW_VARS=""; QOS_FW_FMT=""; QOS_FW_MEASURES=0; QOS_FW_SMM=0

# Echo the first of $@ that exists in the env, or nothing.
_env_first_file() {
  local f list=""
  for f in "$@"; do list="$list '$f'"; done
  inenv "for f in $list; do [ -f \"\$f\" ] && { printf '%s\n' \"\$f\"; exit 0; }; done; exit 1" \
    2>/dev/null || true
}

# Resolve the firmware for the current settings into QOS_FW_*, or die with the
# remedy. Must be called before machine_args / secboot_qemu_args / pflash_args:
# the pflash format is part of the answer (the 4 MB build ships as qcow2, the
# 2 MB one as raw, and the wrong `format=` makes qemu read firmware as garbage).
firmware_setup() {
  if [ "$QOS_SECBOOT" = "1" ] && [ "$QOS_SECBOOT_KEYS" = "none" ]; then
    QOS_SECBOOT_KEYS=persist
    msg "--secboot: variable store will persist across boots so an enrolment sticks"
  fi

  local dir d cands
  cands=""; for d in $QOS_OVMF_DIRS; do cands="$cands $d/OVMF_CODE_4M.secboot.qcow2"; done
  # shellcheck disable=SC2086
  QOS_FW_CODE="$(_env_first_file $cands)"
  if [ -n "$QOS_FW_CODE" ]; then
    QOS_FW_FMT=qcow2; QOS_FW_MEASURES=1; QOS_FW_SMM=1
    dir="$(dirname "$QOS_FW_CODE")"
    case "$QOS_SECBOOT_KEYS" in
      none|persist) QOS_FW_VARS="$dir/OVMF_VARS_4M.qcow2" ;;
      microsoft)    QOS_FW_VARS="$dir/OVMF_VARS_4M.secboot.qcow2" ;;
      *)            QOS_FW_VARS="$QOS_SECBOOT_KEYS" ;;
    esac
  else
    cands=""
    for d in $QOS_OVMF_DIRS; do cands="$cands $d/OVMF_CODE.secboot.fd"; done
    for d in $QOS_OVMF_DIRS; do cands="$cands $d/OVMF_CODE.fd"; done
    # shellcheck disable=SC2086
    QOS_FW_CODE="$(_env_first_file $cands)"
    [ -n "$QOS_FW_CODE" ] || die "no OVMF firmware in ${QOS_DISTROBOX:-the host}.
  Remedy: distrobox enter ${QOS_DISTROBOX:-…} -- sudo dnf install edk2-ovmf"
    if [ "$QOS_TPM" = "1" ]; then
      die "the only OVMF build here is the 2 MB one ($QOS_FW_CODE), which has no TPM
  support: its TCG2 protocol reports zero active PCR banks, so nothing is ever
  measured. Every PCR would read zero and no event log would appear — the same
  evidence a UKI that failed to measure leaves behind — so a measured-boot check
  would fail for a reason that has nothing to do with the kernel. Refusing to
  attach a TPM to a firmware that cannot measure into it.
  Remedy: install a current edk2-ovmf in ${QOS_DISTROBOX:-the host}; it ships
    /usr/share/edk2/ovmf/OVMF_CODE_4M.secboot.qcow2 + OVMF_VARS_4M.qcow2
    (distrobox enter ${QOS_DISTROBOX:-…} -- sudo dnf install edk2-ovmf)"
    fi
    QOS_FW_FMT=raw; QOS_FW_MEASURES=0
    case "$QOS_FW_CODE" in *.secboot.fd) QOS_FW_SMM=1 ;; *) QOS_FW_SMM=0 ;; esac
    warn "the 2 MB OVMF build ($QOS_FW_CODE) cannot measure: no PCR, event-log or"
    warn "measured-boot result from this VM is evidence of anything."
    dir="$(dirname "$QOS_FW_CODE")"
    case "$QOS_SECBOOT_KEYS" in
      none|persist) QOS_FW_VARS="$dir/OVMF_VARS.fd" ;;
      microsoft)    QOS_FW_VARS="$dir/OVMF_VARS.secboot.fd" ;;
      *)            QOS_FW_VARS="$QOS_SECBOOT_KEYS" ;;
    esac
  fi

  if [ -z "$(_env_first_file "$QOS_FW_VARS")" ]; then
    case "$QOS_SECBOOT_KEYS" in
      none|persist|microsoft)
        die "OVMF variable-store template missing: $QOS_FW_VARS
  Remedy: distrobox enter ${QOS_DISTROBOX:-…} -- sudo dnf install edk2-ovmf" ;;
      *)
        die "QOS_SECBOOT_KEYS=$QOS_SECBOOT_KEYS names a variable store that does not
  exist. Enrolling an owner key is a deliberate step, not something a boot does
  on the way past: boot with QOS_SECBOOT=1 (pristine store, setup mode, kept
  across boots), run \`/usr/libexec/qos-secureboot enroll\` in the guest, and
  reboot the guest — the store then carries the owner PK/KEK/db." ;;
    esac
  fi

  if [ "$QOS_SECBOOT_KEYS" = "microsoft" ]; then
    warn "variable store: Microsoft's keys ($QOS_FW_VARS). The firmware refuses an"
    warn "image signed with any other key before it prints anything, so an"
    warn "owner-signed UKI produces a guest that never appears. Use"
    warn "QOS_SECBOOT_KEYS=none (setup mode — still measures) unless you are"
    warn "specifically testing vendor-key enforcement."
  fi

  msg "firmware: $(basename "$QOS_FW_CODE") [$QOS_FW_FMT] keys=$QOS_SECBOOT_KEYS measures=$QOS_FW_MEASURES smm=$QOS_FW_SMM"
}

# Seed and echo the working variable store for this VM. `none` is re-seeded every
# boot so a boot is reproducible; every other selection is kept, because an
# enrolment a restart silently threw away is indistinguishable from one that
# never happened. $1 = run dir (defaults to QOS_RUN).
firmware_varstore() {
  local run="${1:-$QOS_RUN}" tag out
  tag="$(printf '%s' "$QOS_SECBOOT_KEYS" | tr -c 'a-zA-Z0-9' '-')"
  out="$run/vars-$QOS_FW_FMT-$tag.img"
  if [ "$QOS_SECBOOT_KEYS" = "none" ]; then
    inenv "mkdir -p '$run' && cp -f '$QOS_FW_VARS' '$out'"
  else
    inenv "mkdir -p '$run' && { test -f '$out' || cp -f '$QOS_FW_VARS' '$out'; }"
  fi
  printf '%s\n' "$out"
}

# The two -drive lines for the firmware. $1 = working variable store.
pflash_args() {
  printf -- '-drive if=pflash,format=%s,readonly=on,file=%s -drive if=pflash,format=%s,file=%s' \
    "$QOS_FW_FMT" "$QOS_FW_CODE" "$QOS_FW_FMT" "$1"
}

# Extra qemu args the Secure-Boot firmware needs: SMM, and a pflash that refuses
# writes from outside SMM. Without both, the variable store is writable from the
# guest and enforcement is theatre.
secboot_qemu_args() {
  [ "$QOS_FW_SMM" = "1" ] || return 0
  # disable_s3: the SMM-protected variable store and S3 resume do not coexist in
  # OVMF, and leaving S3 on makes the firmware complain and fall back.
  printf -- '-global driver=cfi.pflash01,property=secure,value=on -global ICH9-LPC.disable_s3=1'
}
# q35 machine options; SMM is required by the Secure-Boot firmware.
machine_args() {
  if [ "$QOS_FW_SMM" = "1" ]; then
    printf -- '-machine q35,accel=kvm,smm=on'
  else
    printf -- '-machine q35,accel=kvm'
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
  [ "$QOS_TPM" = "1" ] || return 0
  inenv "command -v swtpm >/dev/null || { echo 'swtpm not installed in ${QOS_DISTROBOX:-env}'; exit 1; }" \
    || die "swtpm missing (sudo dnf install swtpm swtpm-tools in ${QOS_DISTROBOX:-the env})"

  # A leftover swtpm from before this fix would be reused silently and put the
  # unstarted TPM back, so check what is actually listening rather than trusting
  # the socket's existence.
  #
  # The '^' matters. `pgrep -f 'swtpm socket'` also matches the shell running this
  # very check — its command line contains the pattern, the socket path and the
  # flag name — so an unanchored match finds itself and reports every leftover as
  # correctly started. Anchoring restricts it to processes whose command line
  # begins with swtpm.
  if inenv "test -S '$QOS_TPM_SOCK'" && \
     ! inenv "pgrep -af '^swtpm socket' | grep -F -- '$QOS_TPM_SOCK' | grep -q startup-clear"; then
    warn "a swtpm without --flags startup-clear holds $QOS_TPM_SOCK; replacing it"
    swtpm_stop; sleep 1; inenv "rm -f '$QOS_TPM_SOCK'"
  fi

  # --flags not-need-init,startup-clear is what makes this a *started* TPM.
  # Without it swtpm answers every command with TPM_RC_INITIALIZE (256): the
  # firmware measures nothing, no event log is produced, every PCR reads zero, and
  # Linux logs "A TPM error (256) occurred attempting the self test" before
  # issuing TPM2_Startup itself — far too late for the boot to have been measured.
  # The evidence that leaves behind is identical to a firmware without TPM
  # support, so it is ruled out here rather than diagnosed afterwards.
  inenv "mkdir -p '$QOS_TPM_DIR'; \
    if ! test -S '$QOS_TPM_SOCK'; then \
      setsid nohup swtpm socket --tpmstate dir='$QOS_TPM_DIR' \
        --ctrl type=unixio,path='$QOS_TPM_SOCK' --tpm2 \
        --flags not-need-init,startup-clear \
        >'$QOS_TPM_DIR/swtpm.log' 2>&1 & disown; \
      for _ in \$(seq 1 50); do test -S '$QOS_TPM_SOCK' && break; sleep 0.1; done; \
    fi; \
    test -S '$QOS_TPM_SOCK'" \
    || die "swtpm failed to start (see $QOS_TPM_DIR/swtpm.log).
  If it rejected '--flags not-need-init,startup-clear', the swtpm in
  ${QOS_DISTROBOX:-the env} is too old to start the TPM itself; without that the
  firmware cannot measure. Remedy: sudo dnf install --refresh swtpm swtpm-tools"

  # Prove the emulator answers, rather than reading a socket as a working TPM.
  inenv "command -v swtpm_ioctl >/dev/null || exit 0; swtpm_ioctl -c --unix '$QOS_TPM_SOCK' >/dev/null" \
    || die "swtpm is listening on $QOS_TPM_SOCK but not answering control commands
  (see $QOS_TPM_DIR/swtpm.log). A guest booted against it would measure nothing
  and report every PCR as zero."
  msg "swtpm up (TPM 2.0, started, state=$QOS_TPM_DIR)"
}
swtpm_stop() {
  # Anchored for the same reason as the check above, and here it also stops the
  # kill from landing on the wrong process: unanchored, the pattern matches the
  # shell running pkill — so this killed its own wrapper (that is the "Terminated"
  # line every `vm.sh stop` printed) and could leave the emulator running.
  inenv "pkill -f \"^swtpm socket .*$QOS_TPM_SOCK\" 2>/dev/null || true"
}
# qemu args attaching the swtpm to the guest (empty when TPM disabled).
tpm_qemu_args() {
  [ "$QOS_TPM" = "1" ] || return 0
  printf -- '-chardev socket,id=chrtpm,path=%s -tpmdev emulator,id=tpm0,chardev=chrtpm -device tpm-tis,tpmdev=tpm0' "$QOS_TPM_SOCK"
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
    warn "  Firmware in use: ${QOS_FW_CODE:-<not selected>} (measures=${QOS_FW_MEASURES:-?})"
    return 0
  fi
  if [ "$empty" = 1 ] && [ "$QOS_SECBOOT_KEYS" != "none" ] && [ "$QOS_SECBOOT_KEYS" != "persist" ]; then
    warn "nothing at all on the serial line, with '$QOS_SECBOOT_KEYS' keys in the"
    warn "variable store: the firmware refuses an image signed with any other key"
    warn "before it prints a character, so this is what a rejected image looks like."
    warn "  Remedy: QOS_SECBOOT_KEYS=none (setup mode — still measures), or enrol"
    warn "  the signing certificate in the guest and keep the store (--secboot)."
    return 0
  fi
  if [ "$QOS_TPM" = "1" ] && [ "$empty" = 0 ] && \
     ! _serial_has "$log" 'tpm_tis|tpm_crb|TPM.?2\.0|tpm[0-9]'; then
    warn "a TPM was attached but the guest never reported one: the firmware did not"
    warn "expose TCG2, so PCR and event-log checks in the guest would report"
    warn "'nothing measured' whatever the kernel did. Firmware in use:"
    warn "  $QOS_FW_CODE (measures=$QOS_FW_MEASURES)"
    return 0
  fi
  return 1
}
