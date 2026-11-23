#!/usr/bin/env bash
# The complete graphical installation cycle on a release installer medium.
#
#   installer-release-cycle.sh MEDIUM_DIR OUT
#
# MEDIUM_DIR is tools/ci/publish/installer-release-medium.sh's output. Each mode runs
# os/dev/test-installer-cycle.sh, which drives the live installer only through the actions its
# console publishes, unlocks the installed encrypted target after a cold power cycle, and reads
# the installed desktop's first login off the framebuffer:
#
#   erase               the medium installs over the target disk
#   prepared-alongside  the medium reuses a prepared Tilo boot and root beside foreign Windows
#                       and Linux filesystems, which must stay byte-for-byte unchanged
#   guided-alongside    the medium appends its own partitions after the foreign ones, leaving
#                       their shared ESP untouched
#
# The three modes are what a release verifies: an installer that only stands alone is not the
# installer people run beside the system they already have.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/publish/lib.sh"
cd "$repo_root"

medium=${1:?usage: installer-release-cycle.sh MEDIUM_DIR OUT}
out=${2:?}
[[ -f $medium/disk.qcow2 ]] || die "$medium holds no installer medium"
[[ -f $medium/disk.qcow2.hardware.json ]] || die "$medium carries no media provenance"

rm -rf "$out"
mkdir -p "$out"
storage="${TILO_INSTALLER_CYCLE_STORAGE:-/var/tmp/tilo-installer-cycle-$$}"
rm -rf "$storage"
mkdir -p "$storage"

# The profile encoder test-installer-cycle.sh reads the installer's own state through is built
# here, from this tree, and pinned by its SHA-256 as a release host pins the one it ships.
encoder_dir="$CARGO_TARGET_DIR/imager-profile"
cargo build --release --locked \
  --manifest-path tools/imager-profile/Cargo.toml --target-dir "$encoder_dir" >/dev/null
encoder="$encoder_dir/release/tilo-imager-profile"
test -f "$encoder" && test ! -L "$encoder" && test -x "$encoder"
export TILO_PROFILE_ENCODER="$encoder"
export TILO_PROFILE_ENCODER_SHA256="$(sha256sum "$encoder" | awk '{print $1}')"

export TILO_VM_DISPLAY=2d TILO_ACCEL=kvm TILO_DISTROBOX= TILO_MEM="${TILO_MEM:-8192}"
export TILO_BOOT_WAIT_SECS="${TILO_BOOT_WAIT_SECS:-1800}"
export TILO_INSTALLER_CYCLE_MAX_INSTALL_SECONDS="${TILO_INSTALLER_CYCLE_MAX_INSTALL_SECONDS:-3600}"
export TILO_INSTALLER_CYCLE_STORAGE="$storage"
export TILO_VM_CYCLE="installer-release-cycle-${T_COMMIT:0:12}"

cycle() {
  local mode=$1 verdict=$2
  shift 2
  printf '=== %s\n' "$mode"
  TILO_VM_CYCLE="installer-release-cycle-${T_COMMIT:0:12}-$mode" \
    bash os/dev/test-installer-cycle.sh "$medium/disk.qcow2" \
      --graphics software "$@" --out "$out/$mode" \
    || die "the $mode installation cycle failed"
  test "$(cat "$out/$mode/verdict.txt")" = "$verdict" \
    || die "the $mode cycle ended '$(cat "$out/$mode/verdict.txt" 2>/dev/null)', not $verdict"
}
cycle erase PASS
cycle prepared-alongside PASS --prepared-alongside
cycle guided-alongside PASS --guided-alongside

rm -rf "$storage"
printf 'the erase, prepared-alongside and guided-alongside installation cycles passed\n'
