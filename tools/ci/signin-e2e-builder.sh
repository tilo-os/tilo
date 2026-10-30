#!/usr/bin/env bash
# signin-e2e on one x86_64 or aarch64 machine with podman and /dev/kvm: this commit's OS image,
# the installer medium built from it, and the sign-in run over that medium
# (tools/ci/signin_e2e.py), in that order.
#
#   signin-e2e-builder.sh --target DIR --out DIR [--jobs N] [--variant V]...
#                         [--pre-pam-tilo-image REF|ARCHIVE] [--dry-run]
#
# Run it from a committed checkout of the commit to test: the medium names its commit. DIR for
# --target holds what the steps share and keep between runs (the pods' stores, the compile's
# build directory, the image, the medium); give each checkout one of its own, and remove one
# with `podman unshare rm -rf WHAT`, as what the steps write as root of a user namespace belongs
# to the subuid range, not to you. --out receives signin-e2e's report.md, report.json,
# screenshots and logs, which it clears first. --variant picks signin_e2e.py's variants
# (default: one-secret and separate-disk-secret); the rollback variant takes the image from
# before pam_tilo as a reference the guest can pull (a sealed release) or as an OCI archive
# below --target (a dev build, which tools/ci/seal-dev-image.sh seals first). --jobs caps the
# compile's cargo jobs (default: cargo's own count). --dry-run prints the steps and their
# commands instead of running them.
#
# What it runs, in order; each step's script names its own inputs:
#   1. The pinned external assets (os/dev/cache-github-assets.sh) and the cosign the image's
#      actions unpack from them.
#   2. The RPM snapshot every layer installs from: the published one for this package set when
#      there is one (os/dev/resolve-rpm-snapshot.sh), else this run's
#      (tools/ci/publish/rpms-build.sh, in the buildah image of
#      tools/ci/publish/mirrored-images.tsv), into TARGET/publish/rpms.
#   3. The OS image: the assembler image (os/image/assembler.sh), the base, the OS builder and
#      its tools, the compile and the browser's engine built apart (os/image/compile.sh), every
#      layer of os/image/layers.toml and the composed image, through os/image/build-layer.sh's
#      actions, into TARGET/os/<arch>/image (oci/ and digest).
#   4. tilo-vm, which boots the medium and the installed system, into TARGET/tilo-vm-bin/tilo-vm.
#   5. The installer medium: tools/ci/publish/dev-installer-medium.sh from that image, in a
#      worker VM (tools/ci/medium-in-worker-vm.sh), because the disk install it does needs EROFS,
#      btrfs and loop devices that this machine's kernel may not have. A medium of this commit
#      and image already in TARGET is kept.
#   6. The sign-in run over that medium (tools/ci/signin-e2e.sh).
#
# Where each step runs: the layers and the image's targets as root in the assembler image, which
# builds them with the podman and buildah inside it; the compile and the browser's engine in the
# OS builder image the builder action leaves; the RPM snapshot in the buildah image; everything
# else in the project's builder image (tools/ci/resolve_builder.py), the medium inside its
# worker VM. The machine needs podman, python3 and git; the images bring the tools their steps
# use, the builder image with the assembler step installing podman into its container first
# (os/image/assembler.sh).
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
for tool in podman python3 git; do
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
os=$target/os/$arch
image=$os/image
medium=$target/publish/dev-installer-medium/$arch
vm_bin=$target/tilo-vm-bin/tilo-vm
work=$target/step-scratch
for dir in "$work/tmp" "$work/var-tmp" "$work/runtime" "$target/podman-store" \
           "$target/buildah-scratch" \
           "$target/compile/cargo-home" "$target/compile/target" "$target/compile/sccache" \
           "$target/chromium/target" "$target/tilo-vm/cargo-home" "$target/tilo-vm/target" \
           "$target/signing-tools" "$target/publish"; do
  mkdir -p "$dir"
done
chmod 700 "$work/runtime"
# Podman in a privileged container is still root of a user namespace, where the kernel refuses
# the overlay mount options images' storage.conf set (the assembler's does): the actions' podman
# gets its store below TARGET with plain overlay mounts instead.
storage_conf=$target/storage.conf
printf '[storage]\ndriver = "overlay"\nrunroot = "/run/containers/storage"\ngraphroot = "/var/lib/containers/storage"\n[storage.options.overlay]\nmountopt = "nodev"\n' > "$storage_conf"
# The layers, bottom first (os/image/layers.toml).
layers=$(python3 -I -c 'import tomllib; print(" ".join(l["name"] for l in tomllib.load(open("os/image/layers.toml", "rb"))["layer"]))')
say() { printf 'signin-e2e-builder: %s\n' "$*" >&2; }
# A step: run it, or with --dry-run print what would run.
step() {
  if ((dry_run)); then printf '  %s\n' "$*" >&2; return 0; fi
  "$@"
}
# The project's builder image, resolved to its immutable index digest, and a mirrored image of
# tools/ci/publish/mirrored-images.tsv at the digest the mirror serves.
builder_image() {
  python3 -c 'import sys; sys.path.insert(0, "tools/ci"); import resolve_builder; print(resolve_builder.resolve(""))'
}
mirror_ref() {
  local ref
  ref=$(awk -F '\t' -v name="registry.tilo.so/tilo/$1" '$1 == name { print $1 "@" $3; exit }' \
    tools/ci/publish/mirrored-images.tsv)
  [[ -n $ref ]] || { echo "no $1 in tools/ci/publish/mirrored-images.tsv" >&2; exit 1; }
  printf '%s' "$ref"
}

# One shell script as root in IMAGE: the tree at /src, TARGET at /target where the steps before
# it left what it reads, scratch on the machine's disk. Privileged, because the OS actions build
# images with the podman and buildah inside the image, and may give those a store of their own.
container() {
  local image=$1 script=$2
  shift 2
  step podman run --rm --privileged --security-opt label=disable --workdir /src \
    -v "$repo:/src" -v "$target:/target" -v "$work/tmp:/tmp" -v "$work/var-tmp:/var/tmp" \
    "$@" --entrypoint bash "$image" -eo pipefail -c "$script"
}

# A step of the OS image in the assembler image (os/image/assembler.sh), as its layer actions
# run: the store it builds and loads images in below TARGET, which podman takes its mount
# options from storage_conf.
assembler() {
  container "oci:$os/assembler/oci:tilo" "$1" \
    -v "$target/podman-store:/var/lib/containers" \
    -v "$storage_conf:/run/tilo/storage.conf:ro" -e CONTAINERS_STORAGE_CONF=/run/tilo/storage.conf
}

# The RPM snapshot the layers install from when the registry has none published for this package
# set (tools/ci/publish/rpms-build.sh), in the buildah image. Its store and scratch go to TARGET,
# not to the container's /tmp, so a rerun takes the packages it resolved before.
build_rpms() {
  say "the RPM snapshot"
  step podman run --rm --privileged --security-opt label=disable --workdir /src \
    -v "$repo:/src" -v "$target:/target" -v "$target/buildah-scratch:/scratch" \
    -v "$work/var-tmp:/var/tmp" -e HOME=/root -e TMPDIR=/scratch \
    --entrypoint bash "$(mirror_ref buildah)" -eo pipefail \
    -c "bash tools/ci/publish/rpms-build.sh $arch /target/publish/rpms/$arch"
}

build_os_image() {
  say "the pinned external assets"
  step bash os/dev/cache-github-assets.sh
  # The cosign the layers' use_cosign unpacks (tools/ci/publish/build-lib.sh), from the same
  # pinned assets, into TARGET where the assembler image reads it as /target/signing-tools.
  local cosign_assets
  mapfile -t cosign_assets < <(awk -F '\t' '$2 ~ /^cosign-/ { print $2 }' os/external-assets/github-assets.tsv)
  ((${#cosign_assets[@]})) || { echo 'no cosign in os/external-assets/github-assets.tsv' >&2; exit 1; }
  say "cosign"
  step env TILO_GITHUB_ASSET_CACHE="$target/signing-tools" TILO_ASSET_STORE= \
    bash os/dev/cache-github-assets.sh "${cosign_assets[@]}"
  # The RPM snapshot the layers install from, unless the published one for this package set
  # serves them: then every action takes that, as an action resolves it (build-layer.sh).
  local status=0
  if ((dry_run)); then
    build_rpms
  else
    bash os/dev/resolve-rpm-snapshot.sh --exists 2>/dev/null || status=$?
    case $status in
      0) say "the published RPM snapshot serves this package set" ;;
      1) build_rpms ;;
      *) echo 'signin-e2e-builder: could not ask the registry for the RPM snapshot' >&2; exit 1 ;;
    esac
  fi
  say "the assembler image"
  container "$(builder_image)" "bash os/image/assembler.sh $arch /target/os/$arch/assembler" \
    -v "$target/podman-store:/var/lib/containers" \
    -v "$storage_conf:/run/tilo/storage.conf:ro" -e CONTAINERS_STORAGE_CONF=/run/tilo/storage.conf \
    -e HOME=/root
  say "the base, the OS builder image and its tools"
  assembler "set -e -o pipefail
    bash os/image/build-layer.sh base $arch /target/os/$arch/base
    bash os/image/build-layer.sh image builder $arch /target/os/$arch/builder"
  say "the compile"
  compile compile/target "bash os/image/compile.sh /target/os/$arch/compiled"
  say "the browser's engine, apart from the compiled tree"
  compile chromium/target "bash os/image/compile.sh --chromium /target/os/$arch/chromium"
  say "the layers and the composed image"
  assembler "set -e -o pipefail
    bash os/image/build-layer.sh image tools $arch /target/os/$arch/tools
    for layer in $layers; do
      bash os/image/build-layer.sh layer \"\$layer\" $arch \"/target/os/$arch/layers/\$layer\"
    done
    bash os/image/build-layer.sh compose $arch /target/os/$arch/image"
}

# os/image/compile.sh in the OS builder image the builder action left: its toolchain and glibc
# are the image's, and cargo's home, the build directory and the compiler cache stay below
# TARGET. The tree leaves the browser's engine to the compile of its own (TILO_CHROMIUM_ENGINE
# apart), which the image layer takes beside it.
compile() {
  local target_dir=$1 script=$2
  local args=(podman run --rm --security-opt label=disable --workdir /src
    -v "$repo:/src" -v "$target:/target" -v "$work/tmp:/tmp"
    -v "$target/compile/cargo-home:/cargo-home" -v "$target/$target_dir:/build-target"
    -v "$target/compile/sccache:/sccache" -e HOME=/tmp -e TMPDIR=/tmp
    -e CARGO_HOME=/cargo-home -e CARGO_TARGET_DIR=/build-target -e SCCACHE_DIR=/sccache
    -e TILO_COMPILER_CACHE=local -e TILO_CARGO_FRESHNESS=content -e TILO_CHROMIUM_ENGINE=apart
    -e TILO_RUST_LTO=off -e TILO_RUST_DEBUGINFO=0 -e TILO_WITH_BROWSER=1
    -e TILO_GITHUB_ASSET_CACHE=/src/os/external-assets/cache)
  [[ -z $jobs ]] || args+=(-e "TILO_CARGO_BUILD_JOBS=$jobs")
  args+=(--entrypoint bash "oci:$os/builder/builder-base/oci:tilo" -eo pipefail -c "$script")
  step "${args[@]}"
}

build_vm() {
  say "tilo-vm"
  # A tool for the steps after it, not a lint gate: warnings stay warnings, so the workspace's
  # -D warnings rustflags are cleared.
  step podman run --rm --security-opt label=disable --workdir /src \
    -v "$repo:/src" -v "$target:/target" -v "$work/tmp:/tmp" \
    -v "$target/tilo-vm/cargo-home:/cargo-home" -v "$target/tilo-vm/target:/build-target" \
    -e HOME=/tmp -e TMPDIR=/tmp -e CARGO_HOME=/cargo-home -e CARGO_TARGET_DIR=/build-target \
    -e CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS= \
    -e CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS= \
    --entrypoint bash "$1" -eo pipefail -c 'cargo build --locked -p tilo-vm --bin tilo-vm
      rm -rf /target/tilo-vm-bin && mkdir -p /target/tilo-vm-bin
      cp "$CARGO_TARGET_DIR/debug/tilo-vm" /target/tilo-vm-bin/'
}

build_medium() {
  local commit digest
  commit=$(git rev-parse HEAD)
  if ((!dry_run)); then
    digest=$(tr -d '[:space:]' < "$image/digest")
    if [[ -f $medium/disk.qcow2 ]] && python3 - "$medium/source.json" "$commit" "$digest" <<'EOF'
import json, sys
source = json.load(open(sys.argv[1]))
sys.exit(0 if (source["source_commit"], source["source_image_digest"]) == tuple(sys.argv[2:]) else 1)
EOF
    then
      say "keeping the medium of $commit and $digest"
      return 0
    fi
  fi
  say "the installer medium"
  step env TILO_VM_BIN="$vm_bin" bash tools/ci/medium-in-worker-vm.sh "$arch" "$image" "$medium"
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

builder=$(builder_image)
say "$(git log --oneline -1) on $arch"
build_os_image
build_vm "$builder"
build_medium
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
