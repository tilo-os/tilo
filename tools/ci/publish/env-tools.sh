#!/usr/bin/env bash
# The env tool catalog (os/env-tools-catalog.txt): each tool built by tilo-env-tools exactly as a
# device builds one, against the published env-runtime (or an unpublished preview of it while
# there is none), and sealed into a `tool` tapp. Sealed here with a throwaway key: this proves
# the catalog builds; publishing it with the publisher identity stays with build-env-tools.yml.
#
#   env-tools.sh binaries OUT       tilo-env-tools and the packer into OUT (CI builder, native)
#   env-tools.sh build BINARIES OUT IMAGE
#                                   build the catalog into OUT in the CI builder IMAGE, run
#                                   privileged: the build sandbox makes a user namespace and
#                                   mounts overlays (a privileged node, as root)
#   env-tools.sh inside BINARIES OUT
#                                   the catalog build itself, inside that container
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

case ${1:-} in
  binaries)
    out=${2:?usage: env-tools.sh binaries OUT}
    cargo build --release --locked -p env-tools --bin tilo-env-tools \
      -p tilo-app-package-cli --bin tilo-app-package
    mkdir -p "$out"
    cp "${CARGO_TARGET_DIR:-target}"/release/{tilo-env-tools,tilo-app-package} "$out/"
    ;;
  build)
    binaries=${2:?usage: env-tools.sh build BINARIES OUT IMAGE}
    out=${3:?}
    image=${4:?}
    mkdir -p "$out"
    # The VM's /dev is static; the loop devices the state volume needs appear with devtmpfs.
    modprobe loop 2>/dev/null || true
    if [[ ! -c /dev/loop-control ]]; then
      mount -t devtmpfs devtmpfs /dev
      mkdir -p /dev/pts /dev/shm
      [[ $(stat -f -c %T /dev/pts) == devpts ]] || mount -t devpts -o gid=5,mode=620,ptmxmode=666 devpts /dev/pts
      mountpoint -q /dev/shm || mount -t tmpfs -o mode=1777 tmpfs /dev/shm
    fi
    podman run --rm --privileged --network host \
      -v "$repo_root:/w:ro" -v "$binaries:/binaries:ro" -v "$out:/out" -w /w \
      "$image" bash tools/ci/publish/env-tools.sh inside /binaries /out
    ls -l "$out"
    ;;
  inside)
    binaries=${2:?usage: env-tools.sh inside BINARIES OUT}
    out=${3:?}
    arch=$(uname -m)
    dnf install -y --setopt=install_weak_deps=False bubblewrap skopeo e2fsprogs >/dev/null
    export PATH=$binaries:$PATH
    use_cosign
    runtime=$scratch/runtime.tapp
    status=0
    bash os/dev/resolve-env-runtime.sh --exists || status=$?
    case $status in
      0) bash os/dev/fetch-env-runtime.sh "$scratch/env-runtime"
         cp "$scratch/env-runtime/env-runtime-$arch.tapp" "$runtime" ;;
      1) echo 'no published env-runtime for this tree: building against an unpublished preview'
         bash os/dev/preview-env-runtime.sh "$runtime" ;;
      *) die 'could not ask the registry for the env-runtime' ;;
    esac
    # Overlay uppers cannot live on the container's own overlay root: give the builds an ext4
    # of their own.
    state=$scratch/env-tools-state
    mkdir -p "$state"
    case $(stat -f -c %T "$state") in
      overlayfs | tmpfs)
        truncate -s 40G "$scratch/env-tools-state.img"
        mkfs.ext4 -q "$scratch/env-tools-state.img"
        mount -o loop "$scratch/env-tools-state.img" "$state" ;;
    esac
    work=$scratch/env-tools-$arch
    TILO_ENV_TOOLS_NETWORK=host TILO_ENV_TOOLS_STATE=$state bash os/dev/build-env-tools.sh "$runtime" "$work"
    cp "$work"/*.tapp "$out/"
    cp "$work/catalog.json" "$out/catalog-$arch.json"
    cp "$work/report.json" "$out/report-$arch.json"
    ;;
  *) die 'usage: env-tools.sh binaries|build|inside ...' ;;
esac
