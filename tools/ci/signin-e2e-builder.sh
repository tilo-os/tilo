#!/usr/bin/env bash
# signin-e2e-<arch> without t, on one x86_64 or aarch64 machine with podman and /dev/kvm, such as
# an rcargo builder (`rcargo builder ssh`): this commit's dev OS image, its installer medium and
# the sign-in run over it, each as its t.toml check does.
#
#   signin-e2e-builder.sh --target DIR --out DIR [--jobs N] [--variant V]...
#                         [--pre-pam-tilo-image REF|ARCHIVE]
#
# Run it from a committed checkout of the commit to test. DIR for --target holds what the checks
# share and keep between runs (tools/ci/run-checks.py's /target: the image's layers, the cargo
# builds, the medium); give each checkout one of its own. --out receives signin-e2e's report.md,
# report.json, screenshots and logs. --variant picks signin_e2e.py's variants (default: one-secret
# and separate-disk-secret); the rollback variant takes the image from before pam_tilo as a
# reference the guest can pull or as an OCI archive here.
#
#   1. tools/ci/run-checks.py builds os-image-<arch> and tilo-vm-binary-<arch>.
#   2. tools/ci/medium-in-worker-vm.sh runs dev-installer-medium-<arch>'s script in a worker VM,
#      as a machine whose kernel lacks EROFS, btrfs or loop devices can't build the medium; a
#      medium of this commit and image already in DIR is kept.
#   3. run-checks.py runs signin-e2e-<arch> over it, with the medium given.
#
# Needs python3 (3.11 or later) and git beside podman; on an rcargo builder,
# `sudo dnf install -y python3` once.
set -euo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."

usage() { sed -n '6,7p' "$0" | sed 's/^# *//' >&2; exit 2; }
target='' out='' jobs=6 variants=() old_image=''
while (($#)); do
  case $1 in
    --target) target=${2:?}; shift 2 ;;
    --out) out=${2:?}; shift 2 ;;
    --jobs) jobs=${2:?}; shift 2 ;;
    --variant) variants+=("${2:?}"); shift 2 ;;
    --pre-pam-tilo-image) old_image=${2:?}; shift 2 ;;
    *) usage ;;
  esac
done
[[ -n $target && -n $out ]] || usage
command -v python3 >/dev/null || { echo "signin-e2e-builder needs python3 (sudo dnf install -y python3)" >&2; exit 2; }
arch=$(uname -m)
case $arch in
  x86_64) vm_check=tilo-vm-binary-x86_64 ;;
  aarch64) vm_check=tilo-vm-binary-arm64 ;;
  *) echo "signin-e2e runs on x86_64 or aarch64, not $arch" >&2; exit 2 ;;
esac
mkdir -p "$target" "$out"
target=$(realpath "$target")
out=$(realpath "$out")
run_checks=(python3 tools/ci/run-checks.py --target "$target" --ref main --jobs "$jobs")

echo "signin-e2e-builder: $(git log --oneline -1) on $arch"
"${run_checks[@]}" "os-image-$arch" "$vm_check"

image=$target/os/$arch/image
medium=$target/publish/dev-installer-medium/$arch
commit=$(git rev-parse HEAD)
digest=$(tr -d '[:space:]' < "$image/digest")
if [[ -f $medium/disk.qcow2 ]] && python3 - "$medium/source.json" "$commit" "$digest" <<'EOF'
import json, sys
source = json.load(open(sys.argv[1]))
sys.exit(0 if (source["source_commit"], source["source_image_digest"]) == tuple(sys.argv[2:]) else 1)
EOF
then
  echo "signin-e2e-builder: keeping the medium of $commit and $digest"
else
  TILO_VM_BIN=$target/tilo-vm-bin/tilo-vm bash tools/ci/medium-in-worker-vm.sh "$arch" "$image" "$medium"
fi

inputs=(--input "variants=${variants[*]}")
if [[ -n $old_image ]]; then
  if [[ -f $old_image ]]; then
    # The check sees /target only: the archive goes there, a reflink where the filesystem has them.
    mkdir -p "$target/signin-e2e-inputs"
    cp --reflink=auto "$old_image" "$target/signin-e2e-inputs/pre-pam-tilo.ociarchive"
    inputs+=(--input pre_pam_tilo_image=/target/signin-e2e-inputs/pre-pam-tilo.ociarchive)
  else
    inputs+=(--input "pre_pam_tilo_image=$old_image")
  fi
fi
status=0
"${run_checks[@]}" --given "dev-installer-medium-$arch" "${inputs[@]}" "signin-e2e-$arch" || status=$?
rm -rf "$target/signin-e2e-inputs"
if [[ -d $target/signin-e2e ]]; then
  find "$out" -mindepth 1 -delete
  cp -a "$target/signin-e2e/." "$out/"
  cp "$medium/source.json" "$out/medium-source.json"
  echo "signin-e2e-builder: report in $out/report.md"
fi
exit "$status"
