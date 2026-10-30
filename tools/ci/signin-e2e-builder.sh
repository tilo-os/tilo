#!/usr/bin/env bash
# signin-e2e on one x86_64 or aarch64 machine with podman and /dev/kvm: this commit's OS image,
# the installer medium built from it, and the sign-in run over that medium
# (tools/ci/signin_e2e.py), in that order.
#
#   signin-e2e-builder.sh --target DIR --out DIR [--jobs N] [--variant V]...
#                         [--pre-pam-tilo-image REF|ARCHIVE] [--dry-run]
#
# Run it from a committed checkout of the commit to test: the medium names its commit. The image,
# tilo-vm and the medium come from tools/ci/build-os.sh, with its --target DIR, which holds what
# the steps share and keep between runs; remove one with `podman unshare rm -rf DIR`. --out
# receives signin-e2e's report.md, report.json, screenshots and logs, which it clears first.
# --variant picks signin_e2e.py's variants (default: one-secret and separate-disk-secret); the
# rollback variant takes the image from before pam_tilo as a reference the guest can pull (a
# sealed release) or as an OCI archive below --target (a dev build, which
# tools/ci/seal-dev-image.sh seals first). --jobs caps the compile's cargo jobs (default: cargo's
# own count). --dry-run prints the steps and their commands instead of running them.
#
# What it runs, in order:
#   1. tools/ci/build-os.sh image tilo-vm medium: this commit's OS image, the host's tilo-vm and
#      the installer medium built from the image.
#   2. The sign-in run over that medium (tools/ci/signin-e2e.sh), as root in the project's
#      builder image (tools/ci/resolve_builder.py) with /dev/kvm. The machine needs podman,
#      python3, git and /dev/kvm.
set -euo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."

usage() { sed -n '6,8p' "$0" | sed 's/^# \?//' >&2; exit 2; }
target='' out='' jobs='' variants=() old_image='' dry_run=0
while (($#)); do
  case $1 in
    --target) target=${2:?}; shift 2 ;;
    --out) out=${2:?}; shift 2 ;;
    --jobs) jobs=${2:?}; shift 2 ;;
    --variant) variants+=("${2:?}"); shift 2 ;;
    --pre-pam-tilo-image) old_image=${2:?}; shift 2 ;;
    --dry-run) dry_run=1; shift ;;
    *) usage ;;
  esac
done
[[ -n $target && -n $out ]] || usage
[[ -z $jobs || $jobs =~ ^[1-9][0-9]*$ ]] || { echo "--jobs takes a count" >&2; exit 2; }
for tool in python3 git $( ((dry_run)) || echo podman); do
  command -v "$tool" >/dev/null || { echo "signin-e2e-builder needs $tool" >&2; exit 2; }
done
arch=$(uname -m)
case $arch in
  x86_64|aarch64) ;;
  *) echo "signin-e2e runs on x86_64 or aarch64, not $arch" >&2; exit 2 ;;
esac
mkdir -p "$target" "$out"
target=$(realpath "$target")
out=$(realpath "$out")
repo=$PWD
medium=$target/publish/dev-installer-medium/$arch
work=$target/step-scratch
mkdir -p "$work/tmp" "$work/var-tmp" "$work/runtime"
chmod 700 "$work/runtime"
say() { printf 'signin-e2e-builder: %s\n' "$*" >&2; }
# A step: run it, or with --dry-run print what would run.
step() {
  if ((dry_run)); then printf '  %s\n' "$*" >&2; return 0; fi
  "$@"
}

# The sign-in run over the medium, in the project's builder image: root, with /dev/kvm for the
# guests, the tree read-only, and TARGET for the medium, tilo-vm and the report.
signin() {
  local args=() command
  for variant in "${variants[@]}"; do args+=(--variant "$variant"); done
  [[ -z $old_image ]] || args+=(--pre-pam-tilo-image "$old_image")
  command="bash tools/ci/signin-e2e.sh /target/publish/dev-installer-medium/$arch/disk.qcow2 /target/signin-e2e"
  ((${#args[@]} == 0)) || command+=" $(printf '%q ' "${args[@]}")"
  say "the sign-in run"
  step podman run --rm --privileged --security-opt label=disable --device /dev/kvm --workdir /src \
    -v "$repo:/src:ro" -v "$target:/target" -v "$work/tmp:/tmp" -v "$work/var-tmp:/var/tmp" \
    -v "$work/runtime:/tmp/tilo-xdg-runtime" -e HOME=/tmp -e TMPDIR=/tmp \
    -e XDG_RUNTIME_DIR=/tmp/tilo-xdg-runtime -e TILO_VM_BIN=/target/tilo-vm-bin/tilo-vm \
    --entrypoint bash "$1" -eo pipefail -c "$command"
}

build=(bash tools/ci/build-os.sh --target "$target")
[[ -z $jobs ]] || build+=(--jobs "$jobs")
((!dry_run)) || build+=(--dry-run)
"${build[@]}" image tilo-vm medium
if ((dry_run)); then
  builder=BUILDER-IMAGE
else
  builder=$(python3 -c 'import sys; sys.path.insert(0, "tools/ci"); import resolve_builder; print(resolve_builder.resolve(""))')
fi
if [[ -n $old_image && -f $old_image ]]; then
  # A dev build of the old commit is unsealed, which the installed machine's bootc refuses to
  # switch to: sealed as a release is, into TARGET, which the run reads at /target.
  old_image=$(realpath "$old_image")
  say "sealing $old_image"
  mkdir -p "$target/signin-e2e-inputs"
  step bash tools/ci/seal-dev-image.sh "$old_image" "$target/signin-e2e-inputs/pre-pam-tilo.ociarchive"
  old_image=$target/signin-e2e-inputs/pre-pam-tilo.ociarchive
elif [[ $old_image == /* || $old_image == ./* ]]; then
  echo "no OCI archive at $old_image" >&2
  exit 1
fi
# The run's container sees TARGET at /target, and nothing of this machine's tree elsewhere.
if [[ $old_image == "$target"/* ]]; then
  old_image=/target/${old_image#"$target"/}
fi
status=0
signin "$builder" || status=$?
rm -rf "$target/signin-e2e-inputs"
if [[ -d $target/signin-e2e ]]; then
  find "$out" -mindepth 1 -delete
  cp -a "$target/signin-e2e/." "$out/"
  cp "$medium/source.json" "$out/medium-source.json"
  say "report in $out/report.md"
fi
exit "$status"
