#!/usr/bin/env bash
# This commit's OS on one x86_64 or aarch64 machine with podman, such as a remote build machine:
# the OS image, the installer medium built from it, the development image over it and a board's
# sealed image over that, each with what it needs, in order.
#
#   build-os.sh --target DIR [--jobs N] [--dry-run] WHAT...
#
#   image        the OS image, into DIR/os/<arch>/image (oci/ and digest)
#   tilo-vm      the host's tilo-vm, which boots media and guests, into DIR/tilo-vm-bin/tilo-vm
#   medium       the development installer medium of the image, into
#                DIR/publish/dev-installer-medium/<arch> (disk.qcow2, source.json)
#   dev-image    the development image (os/Containerfile.dev) over the image, into
#                DIR/publish/os-dev/<arch>
#   board BOARD  BOARD's sealed development image (os/Containerfile.board) over the development
#                image, into DIR/publish/os-board/BOARD, and its flashable disk, which needs loop
#                devices
#
# Run it from a committed checkout of the commit to build: the medium and the images name their
# commit. DIR holds what the steps share and keep between runs (the pods' stores, the compiles'
# build directories and compiler cache, the images); give each checkout one of its own, and
# remove one with `podman unshare rm -rf DIR`, as what the steps write as root of a user namespace
# belongs to the subuid range, not to you. --jobs caps the compiles' cargo jobs (default: cargo's
# own count). --dry-run prints the steps and their commands instead of running them.
#
# The steps, in the order they run; each step's script names its own inputs:
#   1. The pinned external assets (os/dev/cache-github-assets.sh) and the cosign the image's
#      actions unpack from them.
#   2. The RPM snapshot every layer installs from: the published one for this package set when
#      there is one (os/dev/resolve-rpm-snapshot.sh), else this run's
#      (tools/ci/publish/rpms-build.sh, in the buildah image of
#      tools/ci/publish/mirrored-images.tsv), into DIR/publish/rpms.
#   3. The OS image: the assembler image (os/image/assembler.sh), the base, the images of
#      os/image/layers.toml (the OS builder and its tools), the compile and the browser's engine
#      built apart (os/image/compile.sh), every layer of os/image/layers.toml and the composed
#      image, through os/image/build-layer.sh's actions.
#   4. tilo-vm (cargo, in the project's builder image).
#   5. The installer medium: tools/ci/publish/dev-installer-medium.sh from the image, in a worker
#      VM (tools/ci/medium-in-worker-vm.sh), because the disk install it does needs EROFS, btrfs
#      and loop devices that this machine's kernel may not have. A medium of this commit and
#      image already in DIR is kept.
#   6. For the development image: its components (os/image/compile.sh --dev, in the compile's
#      build directory, so the crates both build compile once); the environment runtime it ships
#      while none is published for this tree (tools/ci/publish/env-runtime.sh tree and dev, from
#      the RPM snapshot of step 2, sealed with a throwaway key by the packer the compile built);
#      its local model catalog (the runtimes of tools/release/local-model-runtime.sh for this
#      architecture, kept while that script is unchanged, and tools/release/local-model-catalog.sh
#      dev); and the image itself (tools/ci/publish/os-image.sh dev).
#   7. For a board: tools/ci/publish/os-image.sh board over the development image.
#
# Where each step runs: the layers, the image's targets and the development and board images as
# root in the assembler image, which builds them with the podman and buildah inside it; the
# compiles and the environment runtime's seal in the OS builder image the builder action leaves;
# the RPM snapshot, the environment runtime's tree and the llama.cpp runtimes in the buildah
# image, vLLM's runtime in the skopeo image; everything else in the project's builder image
# (tools/ci/resolve_builder.py), the medium inside its worker VM. The machine needs podman,
# python3 and git.
set -euo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."

usage() { sed -n '6p' "$0" | sed 's/^# \?//' >&2; exit 2; }
target='' jobs='' dry_run=0 wanted=()
while (($#)); do
  case $1 in
    --target) target=${2:?}; shift 2 ;;
    --jobs) jobs=${2:?}; shift 2 ;;
    --dry-run) dry_run=1; shift ;;
    image | tilo-vm | medium | dev-image) wanted+=("$1"); shift ;;
    board) wanted+=("board:${2:?board names a board}"); shift 2 ;;
    *) usage ;;
  esac
done
[[ -n $target && ${#wanted[@]} -gt 0 ]] || usage
[[ -z $jobs || $jobs =~ ^[1-9][0-9]*$ ]] || { echo "--jobs takes a count" >&2; exit 2; }
for tool in python3 git $( ((dry_run)) || echo podman); do
  command -v "$tool" >/dev/null || { echo "build-os needs $tool" >&2; exit 2; }
done
arch=$(uname -m)
case $arch in
  x86_64|aarch64) ;;
  *) echo "the OS builds on x86_64 or aarch64, not $arch" >&2; exit 2 ;;
esac
mkdir -p "$target"
target=$(realpath "$target")
repo=$PWD
os=$target/os/$arch
image=$os/image
medium=$target/publish/dev-installer-medium/$arch
dev=$target/publish/os-dev/$arch
work=$target/step-scratch
for dir in "$work/tmp" "$work/var-tmp" "$target/podman-store" "$target/buildah-scratch" \
           "$target/compile/cargo-home" "$target/compile/target" "$target/compile/sccache" \
           "$target/chromium/target" "$target/tools/cargo-home" "$target/tools/target" \
           "$target/signing-tools" "$target/publish"; do
  mkdir -p "$dir"
done
# Podman in a privileged container is still root of a user namespace, where the kernel refuses
# the overlay mount options images' storage.conf set (the assembler's does): the actions' podman
# gets its store below DIR with plain overlay mounts instead.
storage_conf=$target/storage.conf
printf '[storage]\ndriver = "overlay"\nrunroot = "/run/containers/storage"\ngraphroot = "/var/lib/containers/storage"\n[storage.options.overlay]\nmountopt = "nodev"\n' > "$storage_conf"
# The image actions and the layers, bottom first (os/image/layers.toml).
read -r images layers < <(python3 -I -c '
import tomllib
plan = tomllib.load(open("os/image/layers.toml", "rb"))
print(",".join(i["name"] for i in plan.get("image", [])), ",".join(l["name"] for l in plan["layer"]))')
images=${images//,/ } layers=${layers//,/ }
say() { printf 'build-os: %s\n' "$*" >&2; }
# A step: run it, or with --dry-run print what would run.
step() {
  if ((dry_run)); then printf '  %s\n' "$*" >&2; return 0; fi
  "$@"
}
# The project's builder image, resolved to its immutable index digest once, and a mirrored image
# of tools/ci/publish/mirrored-images.tsv at the digest the mirror serves.
builder=''
builder_image() {
  if [[ -z $builder ]]; then
    if ((dry_run)); then builder=BUILDER-IMAGE; else
      builder=$(python3 -c 'import sys; sys.path.insert(0, "tools/ci"); import resolve_builder; print(resolve_builder.resolve(""))')
    fi
  fi
  printf '%s' "$builder"
}
mirror_ref() {
  local ref
  ref=$(awk -F '\t' -v name="registry.tilo.so/tilo/$1" '$1 == name { print $1 "@" $3; exit }' \
    tools/ci/publish/mirrored-images.tsv)
  [[ -n $ref ]] || { echo "no $1 in tools/ci/publish/mirrored-images.tsv" >&2; exit 1; }
  printf '%s' "$ref"
}
# Whether the registry has OS/dev/resolve-SCRIPT.sh's published artifact for this tree: 0 yes, 1
# no. A dry run takes it as not published, which runs every step.
published() {
  ((dry_run)) && return 1
  local status=0
  bash "os/dev/resolve-$1.sh" --exists >/dev/null 2>&1 || status=$?
  case $status in
    0|1) return "$status" ;;
    *) echo "build-os: could not ask the registry for the published $1" >&2; exit 1 ;;
  esac
}

# One shell script as root in IMAGE: the tree at /src, DIR at /target where the steps before it
# left what it reads, scratch on the machine's disk. Privileged, because the OS actions build
# images with the podman and buildah inside the image, and may give those a store of their own.
container() {
  local image=$1 script=$2
  shift 2
  step podman run --rm --privileged --security-opt label=disable --workdir /src \
    -v "$repo:/src" -v "$target:/target" -v "$work/tmp:/tmp" -v "$work/var-tmp:/var/tmp" \
    "$@" --entrypoint bash "$image" -eo pipefail -c "$script"
}

# A step of the OS in the assembler image (os/image/assembler.sh), as its layer actions run: the
# store it builds and loads images in below DIR, which podman takes its mount options from
# storage_conf.
assembler() {
  container "oci:$os/assembler/oci:tilo" "$1" \
    -v "$target/podman-store:/var/lib/containers" \
    -v "$storage_conf:/run/tilo/storage.conf:ro" -e CONTAINERS_STORAGE_CONF=/run/tilo/storage.conf
}

# A step in a mirrored image (buildah, skopeo): its store and scratch in DIR, not in the
# container's /tmp, so a rerun takes the packages and layers it fetched before.
mirrored() {
  step podman run --rm --privileged --security-opt label=disable --workdir /src \
    -v "$repo:/src" -v "$target:/target" -v "$target/buildah-scratch:/scratch" \
    -v "$work/var-tmp:/var/tmp" -e HOME=/root -e TMPDIR=/scratch \
    --entrypoint bash "$(mirror_ref "$1")" -eo pipefail -c "$2"
}

# A cargo build of a tool in the project's builder image, with cargo's home and the build
# directory kept below DIR. Tools to run, not lint gates: warnings stay warnings, so the
# workspace's -D warnings rustflags are cleared.
tool_build() {
  step podman run --rm --security-opt label=disable --workdir /src \
    -v "$repo:/src" -v "$target:/target" -v "$work/tmp:/tmp" \
    -v "$target/tools/cargo-home:/cargo-home" -v "$target/tools/target:/build-target" \
    -e HOME=/tmp -e TMPDIR=/tmp -e CARGO_HOME=/cargo-home -e CARGO_TARGET_DIR=/build-target \
    -e CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS= \
    -e CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS= \
    --entrypoint bash "$(builder_image)" -eo pipefail -c "$1"
}

# The RPM snapshot the layers install from when the registry has none published for this package
# set (tools/ci/publish/rpms-build.sh), in the buildah image.
build_rpms() {
  say "the RPM snapshot"
  mirrored buildah "bash tools/ci/publish/rpms-build.sh $arch /target/publish/rpms/$arch"
}

build_os_image() {
  say "the pinned external assets"
  step bash os/dev/cache-github-assets.sh
  # The cosign the layers' use_cosign unpacks (tools/ci/publish/build-lib.sh), from the same
  # pinned assets, into DIR where the assembler image reads it as /target/signing-tools.
  local cosign_assets
  mapfile -t cosign_assets < <(awk -F '\t' '$2 ~ /^cosign-/ { print $2 }' os/external-assets/github-assets.tsv)
  ((${#cosign_assets[@]})) || { echo 'no cosign in os/external-assets/github-assets.tsv' >&2; exit 1; }
  say "cosign"
  step env TILO_GITHUB_ASSET_CACHE="$target/signing-tools" TILO_ASSET_STORE= \
    bash os/dev/cache-github-assets.sh "${cosign_assets[@]}"
  # The RPM snapshot the layers install from, unless the published one for this package set
  # serves them: then every action takes that, as an action resolves it (build-layer.sh).
  if published rpm-snapshot; then
    say "the published RPM snapshot serves this package set"
  else
    build_rpms
  fi
  say "the assembler image"
  container "$(builder_image)" "bash os/image/assembler.sh $arch /target/os/$arch/assembler" \
    -v "$target/podman-store:/var/lib/containers" \
    -v "$storage_conf:/run/tilo/storage.conf:ro" -e CONTAINERS_STORAGE_CONF=/run/tilo/storage.conf \
    -e HOME=/root
  say "the base and the images the layers are built with: $images"
  local script="set -e -o pipefail
    bash os/image/build-layer.sh base $arch /target/os/$arch/base"
  for name in $images; do
    script+="
    bash os/image/build-layer.sh image $name $arch /target/os/$arch/$name"
  done
  assembler "$script"
  say "the compile"
  compile compile/target "bash os/image/compile.sh /target/os/$arch/compiled"
  say "the browser's engine, apart from the compiled tree"
  compile chromium/target "bash os/image/compile.sh --chromium /target/os/$arch/chromium"
  say "the layers and the composed image"
  script="set -e -o pipefail"
  for layer in $layers; do
    script+="
    bash os/image/build-layer.sh layer $layer $arch /target/os/$arch/layers/$layer"
  done
  script+="
    bash os/image/build-layer.sh compose $arch /target/os/$arch/image"
  assembler "$script"
}

# os/image/compile.sh in the OS builder image the builder action left: its toolchain and glibc
# are the image's, and cargo's home, the build directory and the compiler cache stay below DIR.
# The tree leaves the browser's engine to the compile of its own (TILO_CHROMIUM_ENGINE apart),
# which the image layer takes beside it.
compile() {
  local target_dir=$1 script=$2
  local args=(podman run --rm --security-opt label=disable --workdir /src
    -v "$repo:/src" -v "$target:/target" -v "$work/tmp:/tmp"
    -v "$target/compile/cargo-home:/cargo-home" -v "$target/$target_dir:/build-target"
    -v "$target/compile/sccache:/sccache" -e HOME=/tmp -e TMPDIR=/tmp
    -e CARGO_HOME=/cargo-home -e CARGO_TARGET_DIR=/build-target -e SCCACHE_DIR=/sccache
    -e TILO_COMPILER_CACHE=local -e TILO_CARGO_FRESHNESS=content -e TILO_CHROMIUM_ENGINE=apart
    -e TILO_RUST_LTO=off -e TILO_RUST_DEBUGINFO=0 -e TILO_DEV_RUST_LTO=off -e TILO_WITH_BROWSER=1
    -e TILO_GITHUB_ASSET_CACHE=/src/os/external-assets/cache)
  [[ -z $jobs ]] || args+=(-e "TILO_CARGO_BUILD_JOBS=$jobs" -e "CARGO_BUILD_JOBS=$jobs")
  args+=(--entrypoint bash "oci:$os/builder/builder-base/oci:tilo" -eo pipefail -c "$script")
  step "${args[@]}"
}

build_vm() {
  say "tilo-vm"
  tool_build 'cargo build --locked -p tilo-vm --bin tilo-vm
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
  step env TILO_VM_BIN="$target/tilo-vm-bin/tilo-vm" bash tools/ci/medium-in-worker-vm.sh "$arch" "$image" "$medium"
}

# The environment runtime the development image ships while none is published for this tree:
# its tree from the RPM snapshot the image was built from, sealed with a throwaway key by the
# packer the compile built, in the image it was built for. With one published, an empty
# directory: the image ships that one.
build_env_runtime() {
  local out=/target/publish/env-runtime/dev/$arch trees=/target/publish/env-runtime/dev-trees
  if published env-runtime; then
    say "the published environment runtime serves this tree"
    step rm -rf "$target/publish/env-runtime/dev/$arch"
    step mkdir -p "$target/publish/env-runtime/dev/$arch"
    return 0
  fi
  say "the environment runtime"
  assembler "bash tools/ci/publish/rpm-snapshot-ref.sh /target/publish/rpm-snapshot"
  mirrored buildah "bash tools/ci/publish/env-runtime.sh tree $arch /target/publish/rpm-snapshot /target/publish/rpms/$arch $trees"
  compile compile/target "bash tools/ci/publish/env-runtime.sh dev $arch $trees /target/os/$arch/compiled $out"
}

# The local model catalog the development image carries: this architecture's runtimes, each
# built again only when tools/release/local-model-runtime.sh changed, and the catalog signed with
# a throwaway key.
build_model_catalog() {
  local id inputs runtime
  say "the local model catalog's compiler"
  tool_build 'bash tools/release/local-model-catalog.sh compiler /target/model-catalog/bin'
  inputs=$(sha256sum tools/release/local-model-runtime.sh | cut -d ' ' -f 1)
  for id in $(bash tools/release/local-model-runtime.sh ids); do
    [[ $id == *-"$arch" ]] || continue
    runtime=$target/local-model-runtimes/$id
    if [[ -f $runtime/runtime.json && $(cat "$runtime/inputs" 2>/dev/null) == "$inputs" ]]; then
      say "keeping the model runtime $id"
      continue
    fi
    say "the model runtime $id"
    # The llama.cpp builds run in the buildah image, vLLM's in the skopeo image.
    mirrored "$([[ $id == vllm-* ]] && echo skopeo || echo buildah)" \
      "bash tools/release/local-model-runtime.sh build $id /target/local-model-runtimes/$id"
    ((dry_run)) || printf '%s\n' "$inputs" > "$runtime/inputs"
  done
  say "the local model catalog"
  tool_build "bash tools/release/local-model-catalog.sh dev $arch /target/model-catalog/bin/tilo-model-catalog /target/model-catalog/dev/$arch"
}

build_dev_image() {
  say "the development image's components"
  compile compile/target "bash os/image/compile.sh --dev /target/os/$arch/compiled-dev"
  build_env_runtime
  build_model_catalog
  say "the development image"
  assembler "bash tools/ci/publish/os-image.sh dev $arch /target/os/$arch/image /target/publish/os-dev/$arch /target/os/$arch/compiled-dev"
}

build_board() {
  local board=$1 board_arch
  [[ -f os/hardware/profiles/$board.json ]] || { echo "no hardware profile os/hardware/profiles/$board.json" >&2; exit 2; }
  board_arch=$(python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1]))["architecture"])' "os/hardware/profiles/$board.json")
  [[ $board_arch == "$arch" ]] || { echo "the $board image builds on $board_arch, not on $arch" >&2; exit 2; }
  say "the $board image"
  assembler "bash tools/ci/publish/os-image.sh board $board /target/publish/os-dev/$arch /target/publish/os-board/$board"
}

# Each step once, after what it needs.
declare -A done_steps=()
run() {
  [[ -z ${done_steps[$1]:-} ]] || return 0
  case $1 in
    image) build_os_image ;;
    tilo-vm) build_vm ;;
    medium) run image; run tilo-vm; build_medium ;;
    dev-image) run image; build_dev_image ;;
    board:*) run dev-image; build_board "${1#board:}" ;;
  esac
  done_steps[$1]=1
}

say "$(git log --oneline -1) on $arch"
for what in "${wanted[@]}"; do
  run "$what"
done
