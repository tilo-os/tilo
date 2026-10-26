#!/usr/bin/env bash
# The OS image's acceptance scenarios in tilo-vm, as t's vm-acceptance checks
# run them in a micro-VM on an x86-64 node with KVM (t.toml). The disk is this
# tree's own dev image (os-dev-image-x86_64, which the check needs), installed
# as Release / Dev VM disk installs one; scripts/vm-acceptance/run.py then
# drives the checks.
#
#   vm-acceptance.sh           what vm-acceptance runs
#   vm-acceptance.sh cohort    what vm-acceptance-cohort runs: the launcher,
#                              install, shell and apps+runtime layers vm-cohort.sh
#                              produced from this commit enter through verified
#                              swap-artifact, verified by the tilo-vm it built
#                              from main
#
# Inputs (t dispatch <check> --input name=value; every other run gets the
# defaults):
#   swap     vm-acceptance only: none, all (every userspace component in
#            os/components.toml), or a comma-separated list (components,
#            app:<dir>) to build from this tree and swap in, which exercises
#            the swap path; the image is this tree's already
#   only     comma-separated check ids or areas; empty: every check, or on a
#            working tree the checks its changes select
#   outputs  the slot's outputs
#   gl       virgl: auto, on or off
#
# The report and the performance records land in /target/vm-acceptance/report.
set -euo pipefail

mode=${1:-}
[[ -z $mode || $mode == cohort ]] || { echo "usage: vm-acceptance.sh [cohort]" >&2; exit 2; }
arch=x86_64
work=/var/tmp/vm-acceptance
report=${CARGO_TARGET_DIR:-/target}/vm-acceptance/report
dev=/target/publish/os-dev/$arch
cohort=/target/vm-cohort
verifier=/target/vm-cohort-verifier
run_user=acceptance

# The guest's userspace is the builder image: the tools the run needs come from dnf.
dnf --setopt=retries=3 --setopt=install_weak_deps=False install -y \
  libvirt-daemon-driver-qemu libvirt-client passt qemu-kvm qemu-img edk2-ovmf \
  ipxe-roms-qemu openssh-clients rsync python3-pillow binutils btrfs-progs dosfstools \
  e2fsprogs fuse-overlayfs podman jq skopeo slirp4netns socat sudo systemd \
  shadow-utils util-linux bash-devel readline-devel git curl python3 rustup gcc
command -v virsh qemu-system-x86_64 qemu-img passt ssh podman skopeo systemd-repart sudo >/dev/null
python3 -c 'import PIL'
# Rust as rust-toolchain.toml pins it, kept with the build directory.
# Its own homes: the session user builds with them too.
export RUSTUP_HOME=${CARGO_TARGET_DIR:-/target}/rustup
export CARGO_HOME=${CARGO_TARGET_DIR:-/target}/cargo-home
export PATH=$CARGO_HOME/bin:$PATH
command -v rustup >/dev/null 2>&1 && [[ -x $CARGO_HOME/bin/cargo ]] \
  || rustup-init -y --no-modify-path --default-toolchain none --profile minimal >/dev/null
cargo --version

./os/dev/assert-native-arch.sh "$arch"
python3 os/dev/probe-kvm.py "$arch" || {
  echo 'this node cannot create a KVM VM; the acceptance suite never runs under TCG' >&2
  exit 1
}

rm -rf "$work" "$report"
mkdir -p "$work" "$report"
# The swaps build as the session user, in a copy of the tree it owns.
src=$work/src
cp -a --reflink=auto "$PWD" "$src"

target=$(git rev-parse HEAD)
[[ -s $dev/digest && -s $dev/revision && -d $dev/oci ]] \
  || { echo "no dev image in $dev: the check needs os-dev-image-$arch" >&2; exit 1; }
commit=$(cat "$dev/revision")
[[ $commit == "$target" ]] || { echo "the dev image is of $commit, this tree is $target" >&2; exit 1; }

swap=${T_INPUT_SWAP:-none}
case $swap in
  none) swap="" ;;
  all)
    swap=$(python3 -c '
import tomllib
print(",".join(c["name"] for c in tomllib.load(open("os/components.toml", "rb"))["component"]
               if c.get("kind") == "userspace"))') ;;
esac
artifact_args=()
if [[ $mode == cohort ]]; then
  # The cohort's identity comes from its producer's outputs, the receipt digest
  # included: t hands a check only the outputs of the result it needs.
  handoff=$cohort/handoff.json
  [[ -s $handoff && -x $verifier/tilo-vm-artifact ]] \
    || { echo "no cohort: vm-acceptance-cohort needs vm-cohort-$arch and vm-cohort-verifier-$arch" >&2; exit 1; }
  field() { jq -er ".$1" "$handoff"; }
  [[ $(field source) == "$target" ]] || { echo "the cohort is of $(field source), this tree is $target" >&2; exit 1; }
  swap=$(field components)
  install -m 0755 "$verifier/tilo-vm-artifact" /usr/local/bin/tilo-vm-artifact
  cp -a "$cohort/cohort" "$work/cohort"
  artifact_args=(--artifact-receipt "$work/cohort/manifest.json" --artifact-source "$target"
                 --artifact-builder-image "$(field builder_image)" --artifact-run-id "$(field run_id)"
                 --artifact-run-attempt "$(field run_attempt)" --artifact-receipt-sha256 "$(field receipt_sha256)"
                 --artifact-tilo-vm /usr/local/bin/tilo-vm-artifact)
  echo "cohort of ${target:0:12} verified by tilo-vm of main $(jq -r .commit "$verifier/verifier.json")"
fi

# A run of someone's tree checks what its changes reach; main checks all.
only=${T_INPUT_ONLY:-}
if [[ -z $only && -z ${T_TRIGGER:-} ]]; then
  mapfile -t paths < <({ git diff --name-only "$(git merge-base main HEAD)" HEAD; git diff --name-only HEAD; } | sort -u)
  (( ${#paths[@]} == 0 )) || only=$(python3 scripts/vm-acceptance/ci_select.py "${paths[@]}")
fi
gl=${T_INPUT_GL:-auto}
case $gl in
  auto) compgen -G '/dev/dri/renderD*' >/dev/null && gl=on || gl=off ;;
  on|off) ;;
  *) echo 'gl must be auto, on or off' >&2; exit 1 ;;
esac
outputs=${T_INPUT_OUTPUTS:-1920x1080@1,1280x800@1.25}
echo "testing ${target:0:12} on its own dev image $(cat "$dev/digest"); swap: ${swap:-nothing}; checks: ${only:-all}; gl=$gl"

export TILO_TEST_ASSET_CACHE=${TILO_TEST_ASSET_CACHE:-$work/test-assets}
cargo build --locked -p tilo-vm
install -m 0755 "${CARGO_TARGET_DIR:-target}/debug/tilo-vm" /usr/local/bin/tilo-vm

# The same install Release / Dev VM disk ran: build-image.sh with
# TILO_SKIP_BUILD=1 writes the dev image to a bootable disk in one pass.
modprobe fuse 2>/dev/null || true
modprobe loop 2>/dev/null || true
test -c /dev/fuse && test -c /dev/loop-control
chmod 0666 /dev/fuse
losetup --find >/dev/null
# /var/lib/containers is the micro-VM's own scratch disk: native overlay already.
mkdir -p "$work/install" "$work/install-run"
image=$(podman pull -q "oci:$dev/oci")
oci_digest=$(cat "$dev/digest")
[[ $(podman image inspect --format '{{.Digest}}' "$image") == "$oci_digest" ]] \
  || { echo "$dev/oci is not the image $dev/digest names" >&2; exit 1; }
podman tag "$image" localhost/tilo-vm:ci
TILO_IMAGE=localhost/tilo-vm:ci TILO_SKIP_BUILD=1 TILO_DISTROBOX="" \
  TILO_OUTPUT=$work/install TILO_RUN=$work/install-run TILO_DISK_SIZE=64G \
  TILO_TARGET_IMGREF="registry.tilo.so/tilo/tilo-vm:main-${commit:0:7}-ci" ./os/dev/build-image.sh
disk="$work/tilo-dev-${commit:0:9}.qcow2"
mv "$work/install/qcow2/disk.qcow2" "$disk"
podman rmi -f localhost/tilo-vm:ci "$image" >/dev/null 2>&1 || true
jq -n --arg commit "$commit" --arg subject "$(git log -1 --format=%s "$commit")" \
  --arg time "$(git log -1 --format=%cI "$commit")" --arg digest "$oci_digest" \
  --arg built "$(date -u +%Y-%m-%dT%H:%M:%S+00:00)" --arg source "t os-dev-image-$arch" \
  '{commit:$commit, subject:$subject, commit_time:$time, oci_digest:$digest,
    built_at:$built, built_by:"t vm-acceptance", source:$source}' > "$disk.provenance.json"

# libvirt's session daemon runs as an unprivileged user, as on a workstation.
id "$run_user" >/dev/null 2>&1 || useradd --create-home --shell /bin/bash "$run_user"
uid=$(id -u "$run_user")
install -d -m 0700 -o "$run_user" -g "$run_user" "/run/user/$uid"
chmod 0666 /dev/kvm
# install -d gives -o only to the directories it names, so .config is named too: a
# root-owned .config would leave the user unable to make tilo-vm's config and keys there.
install -d -o "$run_user" -g "$run_user" "/home/$run_user/.config" "/home/$run_user/.config/libvirt"
# The micro-VM's /dev has no /dev/stdin to install from.
printf 'max_core = 0\n' > "/home/$run_user/.config/libvirt/qemu.conf"
chown "$run_user:" "/home/$run_user/.config/libvirt/qemu.conf"
mkdir -p "$src/target/dev-runs"
chown -R "$run_user:" "$work" "$report"
chown -R "$run_user:" "$CARGO_HOME" "$RUSTUP_HOME"
keep=$(env | sed -n 's/^\(RUSTC_WRAPPER\|SCCACHE_[A-Z0-9_]*\|CARGO_[A-Z0-9_]*\|RUSTUP_HOME\|TILO_[A-Z0-9_]*\)=.*/\1/p' \
  | grep -vx CARGO_TARGET_DIR | paste -sd, -)
as_user() { sudo -n -u "$run_user" -H --preserve-env="$keep" env "XDG_RUNTIME_DIR=/run/user/$uid" "PATH=$PATH" "$@"; }
as_user virsh --connect qemu:///session capabilities | grep -q "<domain type='kvm'"

args=(--disk "$disk" --commit "$commit" --outputs "$outputs" --out "$report"
      --tilo-vm /usr/local/bin/tilo-vm --records "$report/records.jsonl" --runner t)
[[ -z $swap ]] || args+=(--swap "$swap" --swap-from "$src" --swap-ref "$target")
args+=("${artifact_args[@]}")
[[ -z $only ]] || args+=(--only "$only")
[[ $gl != on ]] || args+=(--gl)
status=0
cd "$src"
as_user env GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0='*' \
  python3 scripts/vm-acceptance/run.py "${args[@]}" || status=$?
[[ ! -f $report/report.md ]] || cat "$report/report.md"
# A failed check keeps no outputs, so its text evidence (tracebacks, boot gate,
# memory maps, shell runtime excerpts) is printed here, bounded by report.py.
if [[ -s $report/failure-evidence.txt ]]; then
  echo "## Evidence of the failed checks"
  cat "$report/failure-evidence.txt"
fi
[[ ! -f $report/report.json ]] || jq -r '"score: \([.results[] | select(.status == "pass")] | length)/\(.results | length) checks pass (\(.overall))"' "$report/report.json"
exit "$status"
