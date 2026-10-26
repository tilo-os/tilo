#!/usr/bin/env bash
# The env-runtime chain (os/Containerfile.env-runtime): the minimal userland every environment
# starts from, built natively per architecture from the frozen RPM snapshot, sealed into an
# `env-runtime` tapp per architecture and published as
# registry.tilo.so/tilo/env-runtime:set-<fingerprint>, which the OS image and the
# tilo-session-apps RPM resolve (os/dev/resolve-env-runtime.sh).
#
#   env-runtime.sh tree ARCH SNAPSHOT OUT   install the runtime into an empty root and export
#                                           OUT/env-runtime-tree-ARCH.tar (buildah image, native)
#   env-runtime.sh packer OUT               build the packer into OUT/tilo-app-package (builder)
#   env-runtime.sh seal TREES PACKER OUT    seal both trees into OUT/env-runtime-ARCH.tapp: with
#                                           TAPP_PUBLISHER_IDENTITY when given, else a throwaway
#                                           key, which no release accepts (builder)
#   env-runtime.sh preview TREES PACKER OUT seal them with a throwaway key on any branch, stable
#                                           included: the runtime a run's dev images ship while
#                                           stable has published none, which nothing publishes
#   env-runtime.sh image SEALED OUT         the published image of both tapps, one per platform,
#                                           joined into one index (buildah image)
#   env-runtime.sh release OUT              publish OUT signed as set-<fingerprint>, unless that
#                                           tag already exists (skopeo image)
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
repository=registry.tilo.so/tilo/env-runtime

case ${1:-} in
  tree)
    arch=${2:?usage: env-runtime.sh tree ARCH SNAPSHOT OUT}
    snapshot=${3:?}
    out=${4:?}
    [[ $(uname -m) == "$arch" ]] || die "the $arch runtime is built natively, not on $(uname -m)"
    . os/base-snapshot.env
    rpms_ref=$(cat "$snapshot/rpms-ref")
    [[ $rpms_ref =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]] || die "unexpected snapshot $rpms_ref"
    # The tree stage copies only its own definition; the repository's .containerignore (which
    # buildah applies, unlike BuildKit) leaves os/Containerfile.* out of a repository context.
    context=$scratch/env-runtime-context
    rm -rf "$context"
    install -D -m 0644 os/Containerfile.env-runtime "$context/os/Containerfile.env-runtime"
    use_buildah
    # overlay, as oci-build.sh: vfs copies every layer of the RPM snapshot whole, which filled
    # the micro-VM step's scratch disk before the build started. overlay works on that disk.
    export STORAGE_DRIVER=overlay
    printf '[storage]\ndriver = "overlay"\n' > "$CONTAINERS_STORAGE_CONF"
    mkdir -p "$out"
    b build --layers=false --timestamp 0 --file os/Containerfile.env-runtime --target tree \
      --build-arg "BASE_REF=$BASE_REF" --build-arg "RPMS_REF=$rpms_ref" \
      --output "type=tar,dest=$out/env-runtime-tree-$arch.tar" "$context"
    printf '%s\n' "$rpms_ref" > "$out/env-runtime-rpms-$arch.txt"
    ls -l "$out"
    ;;
  packer)
    out=${2:?usage: env-runtime.sh packer OUT}
    cargo build --release --locked -p tilo-app-package-cli --bin tilo-app-package
    mkdir -p "$out"
    cp "${CARGO_TARGET_DIR:-target}/release/tilo-app-package" "$out/"
    ;;
  seal|preview)
    trees=${2:?usage: env-runtime.sh seal|preview TREES PACKER OUT}
    packer=${3:?}/tilo-app-package
    out=${4:?}
    version=$(bash os/dev/resolve-env-runtime.sh --version)
    identity_args=()
    if [[ $1 == preview ]]; then
      echo "sealing with a throwaway key for this run's dev images: nothing is published"
    elif [[ -n ${TAPP_PUBLISHER_IDENTITY:-} ]]; then
      identity=$scratch/env-runtime-publisher.json
      (umask 077; printf '%s' "$TAPP_PUBLISHER_IDENTITY" > "$identity")
      trap 'rm -f "$identity"' EXIT
      identity_args=(--identity "$identity")
    else
      [[ ${T_REF:-} != stable && ${T_REF:-} != tag:* ]] \
        || die 'TAPP_PUBLISHER_IDENTITY is required to seal a release'
      echo 'sealing with a throwaway key: this proves the trees seal, and publishes nothing'
    fi
    unset TAPP_PUBLISHER_IDENTITY
    rm -rf "$out"
    mkdir -p "$out"
    for arch in x86_64 aarch64; do
      work=$scratch/env-runtime-$arch
      rm -rf "$work"
      mkdir -p "$work/tree"
      tar -xpf "$trees/env-runtime-tree-$arch.tar" -C "$work/tree"
      mv "$work/tree/packages.txt" "$work/packages.txt"
      test -x "$work/tree/usr/bin/bash"
      tapp=$out/env-runtime-$arch.tapp
      id=$("$packer" seal-image --kind env-runtime --tree "$work/tree" \
        --id so.tilo.env-runtime --version "$version" \
        --name 'Tilo environment runtime' --arch "$arch" \
        --packages "$work/packages.txt" "${identity_args[@]}" --output "$tapp")
      [[ $id =~ ^tapp:[0-9a-f]{64}$ ]] || die "the packer printed no package id for $arch"
      "$packer" verify "$tapp"
      printf '%s\n' "$id" > "$out/env-runtime-$arch.id"
      cp "$work/packages.txt" "$out/env-runtime-$arch.packages.txt"
      cp "$trees/env-runtime-rpms-$arch.txt" "$out/env-runtime-$arch.rpms.txt"
      printf '%s %s: %s files\n' "$arch" "$id" "$(find "$work/tree" -type f | wc -l)"
      rm -rf "$work"
    done
    [[ $(cat "$out/env-runtime-aarch64.rpms.txt") == "$(cat "$out/env-runtime-x86_64.rpms.txt")" ]] \
      || die 'the two runtimes were built from different RPM snapshots'
    "$packer" inspect "$out/env-runtime-x86_64.tapp" \
      | sed -n 's/^publisher-identity: ed25519:\([0-9a-f]\{64\}\)$/\1/p' > "$out/publisher.hex"
    [[ $(cat "$out/publisher.hex") =~ ^[0-9a-f]{64}$ ]] || die 'the sealed runtime names no publisher'
    (cd "$out" && sha256sum ./*.tapp > SHA256SUMS)
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
