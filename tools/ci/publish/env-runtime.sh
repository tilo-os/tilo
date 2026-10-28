#!/usr/bin/env bash
# The env-runtime chain (os/Containerfile.env-runtime): the minimal userland every environment
# starts from, built natively per architecture from the frozen RPM snapshot, sealed into an
# `env-runtime` tapp per architecture and published as
# registry.tilo.so/tilo/env-runtime:set-<fingerprint>, which the OS image and the
# tilo-session-apps RPM resolve (os/dev/resolve-env-runtime.sh).
#
#   env-runtime.sh tree ARCH SNAPSHOT RPMS OUT
#                                           install the runtime into an empty root and export
#                                           OUT/env-runtime-tree-ARCH.tar from the snapshot
#                                           rpm-snapshot-ref.sh named in SNAPSHOT: the published
#                                           one, or this run's image-rpms-ARCH in RPMS while it
#                                           is not published, which no publisher seals (buildah
#                                           image, native)
#   env-runtime.sh packer OUT               build the packer into OUT/tilo-app-package (builder)
#   env-runtime.sh seal TREES PACKER OUT    seal both trees into OUT/env-runtime-ARCH.tapp: with
#                                           TAPP_PUBLISHER_IDENTITY when given, else a throwaway
#                                           key, which no release accepts (builder)
#   env-runtime.sh dev-tree ARCH RPMS OUT   as tree, from the snapshot image-rpms-ARCH built in
#                                           this run (RPMS: its image.oci.tar and digest), for dev
#                                           images; no release seals it (buildah image, native)
#   env-runtime.sh dev ARCH TREES PACKER OUT
#                                           seal ARCH's dev tree with a throwaway key into OUT as
#                                           seal lays it out: the runtime a run's dev images ship
#                                           (os-dev-image-ARCH); nothing publishes it (builder)
#   env-runtime.sh image SEALED OUT         the published image of both tapps, one per platform,
#                                           joined into one index (buildah image)
#   env-runtime.sh release OUT              publish OUT signed as set-<fingerprint>, unless that
#                                           tag already exists (skopeo image)
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
repository=registry.tilo.so/tilo/env-runtime

# Seal ARCH's runtime tree (TREE_TAR, its packages.txt inside) into OUT/env-runtime-ARCH.tapp,
# with its .id, .packages.txt and the snapshot it came from (RPMS_TXT) beside it. Any further
# arguments name the publisher identity; with none the packer seals with a throwaway key.
seal_tree() {
  local arch=$1 tree_tar=$2 rpms_txt=$3 packer=$4 out=$5 work tapp id version
  shift 5
  version=$(bash os/dev/resolve-env-runtime.sh --version)
  work=$scratch/env-runtime-$arch
  rm -rf "$work"
  mkdir -p "$work/tree"
  tar -xpf "$tree_tar" -C "$work/tree"
  mv "$work/tree/packages.txt" "$work/packages.txt"
  test -x "$work/tree/usr/bin/bash"
  tapp=$out/env-runtime-$arch.tapp
  id=$("$packer" seal-image --kind env-runtime --tree "$work/tree" \
    --id so.tilo.env-runtime --version "$version" \
    --name 'Tilo environment runtime' --arch "$arch" \
    --packages "$work/packages.txt" "$@" --output "$tapp")
  [[ $id =~ ^tapp:[0-9a-f]{64}$ ]] || die "the packer printed no package id for $arch"
  "$packer" verify "$tapp"
  printf '%s\n' "$id" > "$out/env-runtime-$arch.id"
  cp "$work/packages.txt" "$out/env-runtime-$arch.packages.txt"
  cp "$rpms_txt" "$out/env-runtime-$arch.rpms.txt"
  printf '%s %s: %s files\n' "$arch" "$id" "$(find "$work/tree" -type f | wc -l)"
  rm -rf "$work"
}

# Install ARCH's runtime from the snapshot RPMS_REF (an image buildah's store holds or can
# pull) into an empty root, exported as OUT/env-runtime-tree-ARCH.tar, with SOURCE, what the
# snapshot is, in OUT/env-runtime-rpms-ARCH.txt.
build_tree() {
  local arch=$1 rpms_ref=$2 source=$3 out=$4 context
  [[ $(uname -m) == "$arch" ]] || die "the $arch runtime is built natively, not on $(uname -m)"
  . os/base-snapshot.env
  # The tree stage copies only its own definition; the repository's .containerignore (which
  # buildah applies, unlike BuildKit) leaves os/Containerfile.* out of a repository context.
  context=$scratch/env-runtime-context
  rm -rf "$context"
  install -D -m 0644 os/Containerfile.env-runtime "$context/os/Containerfile.env-runtime"
  mkdir -p "$out"
  b build --layers=false --timestamp 0 --file os/Containerfile.env-runtime --target tree \
    --build-arg "BASE_REF=$BASE_REF" --build-arg "RPMS_REF=$rpms_ref" \
    --output "type=tar,dest=$out/env-runtime-tree-$arch.tar" "$context"
  printf '%s\n' "$source" > "$out/env-runtime-rpms-$arch.txt"
  ls -l "$out"
}

# buildah with overlay storage, as oci-build.sh: vfs copies every layer of the RPM snapshot
# whole, which filled the micro-VM step's scratch disk before the build started.
use_buildah_overlay() {
  use_buildah
  export STORAGE_DRIVER=overlay
  printf '[storage]\ndriver = "overlay"\n' > "$CONTAINERS_STORAGE_CONF"
}

# OUT's publisher.hex, read from ARCH's sealed tapp, and SHA256SUMS of its tapps.
seal_index() {
  local out=$1 packer=$2 arch=$3
  "$packer" inspect "$out/env-runtime-$arch.tapp" \
    | sed -n 's/^publisher-identity: ed25519:\([0-9a-f]\{64\}\)$/\1/p' > "$out/publisher.hex"
  [[ $(cat "$out/publisher.hex") =~ ^[0-9a-f]{64}$ ]] || die 'the sealed runtime names no publisher'
  (cd "$out" && sha256sum ./*.tapp > SHA256SUMS)
}

case ${1:-} in
  tree)
    arch=${2:?usage: env-runtime.sh tree ARCH SNAPSHOT RPMS OUT}
    snapshot=${3:?}
    rpms=${4:?}
    out=${5:?}
    use_buildah_overlay
    snapshot_for_build "$snapshot" "$rpms" "$arch"
    build_tree "$arch" "$rpms_ref" "$rpms_source" "$out"
    ;;
  dev-tree)
    arch=${2:?usage: env-runtime.sh dev-tree ARCH RPMS OUT}
    rpms=${3:?}
    out=${4:?}
    digest=$(cat "$rpms/digest")
    [[ $(archive_digest "$rpms/image.oci.tar") == "$digest" ]] || die "$rpms/image.oci.tar is not the build $digest"
    use_buildah_overlay
    rpms_ref=localhost/tilo-rpms:dev-$arch
    b tag "$(b pull -q "oci-archive:$rpms/image.oci.tar")" "$rpms_ref"
    build_tree "$arch" "$rpms_ref" "image-rpms-$arch $digest" "$out"
    ;;
  packer)
    out=${2:?usage: env-runtime.sh packer OUT}
    cargo build --release --locked -p tilo-app-package-cli --bin tilo-app-package
    mkdir -p "$out"
    cp "${CARGO_TARGET_DIR:-target}/release/tilo-app-package" "$out/"
    ;;
  seal)
    trees=${2:?usage: env-runtime.sh seal TREES PACKER OUT}
    packer=${3:?}/tilo-app-package
    out=${4:?}
    identity_args=()
    if [[ -n ${TAPP_PUBLISHER_IDENTITY:-} ]]; then
      identity=$scratch/env-runtime-publisher.json
      (umask 077; printf '%s' "$TAPP_PUBLISHER_IDENTITY" > "$identity")
      trap 'rm -f "$identity"' EXIT
      identity_args=(--identity "$identity")
      # A publisher seals only runtimes built from the published snapshot.
      for arch in x86_64 aarch64; do require_published_snapshot "$trees/env-runtime-rpms-$arch.txt"; done
    else
      [[ ${T_REF:-} != stable && ${T_REF:-} != tag:* ]] \
        || die 'TAPP_PUBLISHER_IDENTITY is required to seal a release'
      echo 'sealing with a throwaway key: this proves the trees seal, and publishes nothing'
    fi
    unset TAPP_PUBLISHER_IDENTITY
    rm -rf "$out"
    mkdir -p "$out"
    for arch in x86_64 aarch64; do
      seal_tree "$arch" "$trees/env-runtime-tree-$arch.tar" "$trees/env-runtime-rpms-$arch.txt" \
        "$packer" "$out" "${identity_args[@]}"
    done
    [[ $(cat "$out/env-runtime-aarch64.rpms.txt") == "$(cat "$out/env-runtime-x86_64.rpms.txt")" ]] \
      || die 'the two runtimes were built from different RPM snapshots'
    seal_index "$out" "$packer" x86_64
    ;;
  dev)
    arch=${2:?usage: env-runtime.sh dev ARCH TREES PACKER OUT}
    trees=${3:?}
    packer=${4:?}/tilo-app-package
    out=${5:?}
    unset TAPP_PUBLISHER_IDENTITY
    echo "sealing this run's $arch runtime with a throwaway key for its dev images: nothing is published"
    rm -rf "$out"
    mkdir -p "$out"
    seal_tree "$arch" "$trees/env-runtime-tree-$arch.tar" "$trees/env-runtime-rpms-$arch.txt" \
      "$packer" "$out"
    seal_index "$out" "$packer" "$arch"
    ;;
  image)
    sealed=${2:?usage: env-runtime.sh image SEALED OUT}
    out=${3:?}
    . os/base-snapshot.env
    rpms_ref=$(cat "$sealed/env-runtime-x86_64.rpms.txt")
    # The published stage only copies the tapps, so both platforms build on any node; every
    # FROM still needs its argument.
    for platform in linux/amd64 linux/arm64; do
      bash tools/ci/publish/oci-build.sh "$sealed" os/Containerfile.env-runtime "$out/${platform#linux/}" \
        --target published --platform "$platform" --build-context "sealed=$sealed" \
        --build-arg "BASE_REF=$BASE_REF" --build-arg "RPMS_REF=$rpms_ref"
    done
    python3 tools/ci/publish/oci-index.py "$out" "$out/amd64/image.oci.tar" "$out/arm64/image.oci.tar"
    rm -rf "$out/amd64" "$out/arm64"
    ;;
  release)
    out=${2:?usage: env-runtime.sh release OUT}
    tag=$(bash os/dev/resolve-env-runtime.sh --tag)
    status=0
    bash os/dev/resolve-env-runtime.sh --exists || status=$?
    case $status in
      0) printf 'already published: %s:%s\n' "$repository" "$tag"; exit 0 ;;
      1) ;;
      *) die "could not ask the registry for $repository:$tag" ;;
    esac
    bash tools/ci/publish/oci-publish.sh release "$repository" "$out" "$tag" 'sha-{commit}'
    ;;
  *) die 'usage: env-runtime.sh tree|packer|seal|image|release ...' ;;
esac
