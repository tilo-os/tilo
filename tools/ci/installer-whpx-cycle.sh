#!/usr/bin/env bash
# The installation cycle and WHPX packaging for an offline installer medium.
#
#   installer-whpx-cycle.sh MEDIUM_DIR LIVE_BASE OUT
#
# MEDIUM_DIR is tools/ci/publish/installer-whpx-medium.sh's output. The install-only guided cycle
# (os/dev/test-installer-cycle.sh) boots the medium in a KVM guest, installs onto a disk that
# already holds a foreign Windows and Linux layout, and proves the installed system is exactly the
# target the medium preloads. The medium that passed it is then packaged for the Windows on ARM
# host (tools/ci/publish/installer-whpx.sh). OUT holds cycle/ and whpx/.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/publish/lib.sh"
cd "$repo_root"

medium=${1:?usage: installer-whpx-cycle.sh MEDIUM_DIR LIVE_BASE OUT}
live_base=${2:?}
out=${3:?}
[[ -f $medium/disk.qcow2 ]] || die "$medium holds no installer medium"
[[ -f $medium/target.json ]] || die "$medium names no preloaded target"
profile=$(jq -er '.hardware_profile' "$medium/target.json") || die "the target metadata names no profile"
target="$(jq -er '.repository' "$medium/target.json")@$(jq -er '.target_digest' "$medium/target.json")"

rm -rf "$out"
mkdir -p "$out/cycle"

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
export TILO_INSTALLER_CYCLE_STORAGE="${TILO_INSTALLER_CYCLE_STORAGE:-/var/tmp/tilo-installer-whpx-cycle}"
export TILO_VM_CYCLE="installer-whpx-${T_COMMIT:0:12}"
export TILO_EXPECTED_INSTALLED_REF="$target"

bash os/dev/test-installer-cycle.sh "$medium/disk.qcow2" \
  --guided-alongside --install-only --graphics software --out "$out/cycle" \
  || die "the install-only guided cycle failed"
[[ $(cat "$out/cycle/verdict.txt") == PASS_INSTALL_ONLY ]] \
  || die "the install-only cycle ended '$(cat "$out/cycle/verdict.txt" 2>/dev/null)'"
[[ -s $out/cycle/guest-install-proof.json ]] || die "the cycle left no guest install proof"

bash tools/ci/publish/installer-whpx.sh "$medium" "$profile" "$live_base" \
  "$out/cycle/guest-install-proof.json" "$out/whpx" \
  || die "the WHPX medium did not package"
rm -rf "$TILO_INSTALLER_CYCLE_STORAGE"
printf 'the %s WHPX medium passed the install-only guided cycle and is packaged\n' "$profile"
