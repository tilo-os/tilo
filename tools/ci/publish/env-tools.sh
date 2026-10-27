#!/usr/bin/env bash
# The env tool catalog's build (os/env-tools-catalog.txt): each tool built by tilo-env-tools
# exactly as a device builds one, against the published env-runtime (or, for a working tree whose
# runtime is not published yet, an unpublished preview of it), and sealed into a `tool` tapp.
# The build runs arbitrary code (dnf and package scriptlets), so it never holds the publisher
# identity: it seals every tool for the runtime's publisher without the key, with a placeholder
# signature (`tilo-env-tools build --publisher`), which seal-env-tools signs on stable
# (env-tools-release.sh, beside this script, which also publishes the catalog). The heavy checks
# read only this script, so a change to the publishing steps rebuilds no tools.
#
#   env-tools.sh binaries OUT PLAN       tilo-env-tools and the packer into OUT (CI builder, native)
#   env-tools.sh build BINARIES OUT PLAN build the catalog into OUT as root in the CI builder: the
#                                        build sandbox makes a user namespace and mounts overlays
#                                        (a micro-VM step)
#
# PLAN is env-tools-plan's directory on stable: the runtime to build on, or that the catalog is
# published already and nothing is built. A working tree has none and resolves the runtime itself.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

inspect_field() {
  "$1" inspect "$3" | sed -n "s/^$2: //p"
}

published_already() {
  if [[ -s $1/published ]]; then
    printf 'registry.tilo.so/tilo/env-tools:%s is published already: nothing to build\n' "$(cat "$1/published")"
    exit 0
  fi
}

case ${1:-} in
  binaries)
    out=${2:?usage: env-tools.sh binaries OUT PLAN}
    published_already "${3:?}"
    cargo build --release --locked -p env-tools --bin tilo-env-tools \
      -p tilo-app-package-cli --bin tilo-app-package
    mkdir -p "$out"
    cp "${CARGO_TARGET_DIR:-target}"/release/{tilo-env-tools,tilo-app-package} "$out/"
    ;;
  build)
    binaries=${2:?usage: env-tools.sh build BINARIES OUT PLAN}
    out=${3:?}
    plan=${4:?}
    published_already "$plan"
    mkdir -p "$out"
    arch=$(uname -m)
    dnf install -y --setopt=install_weak_deps=False bubblewrap skopeo e2fsprogs >/dev/null
    export PATH=$binaries:$PATH
    use_cosign
    runtime=$scratch/runtime.tapp
    if [[ -d $plan ]]; then
      # Stable: the runtime the plan fetched, which seal-env-tools checks the tools against.
      cp "$plan/runtime/env-runtime-$arch.tapp" "$runtime"
      source=published
    else
      status=0
      bash os/dev/resolve-env-runtime.sh --exists || status=$?
      case $status in
        0) bash os/dev/fetch-env-runtime.sh "$scratch/env-runtime"
           cp "$scratch/env-runtime/env-runtime-$arch.tapp" "$runtime"
           source=published ;;
        1) echo 'no published env-runtime for this tree: building against an unpublished preview'
           bash os/dev/preview-env-runtime.sh "$runtime"
           source=preview ;;
        *) die 'could not ask the registry for the env-runtime' ;;
      esac
    fi
    runtime_id=$(inspect_field tilo-app-package package-id "$runtime")
    [[ $runtime_id =~ ^tapp:[0-9a-f]{64}$ ]] || die "the runtime names no package id: $runtime_id"
    publisher=$(inspect_field tilo-app-package publisher-identity "$runtime")
    publisher=${publisher#ed25519:}
    [[ $publisher =~ ^[0-9a-f]{64}$ ]] || die "the runtime names no publisher: $publisher"
    # Overlay uppers cannot live on the container's own overlay root: give the builds an ext4
    # of their own.
    state=$scratch/env-tools-state
    mkdir -p "$state"
    case $(stat -f -c %T "$state") in
      overlayfs | tmpfs)
        # A micro-VM's /dev is static; the loop devices appear with devtmpfs.
        modprobe loop 2>/dev/null || true
        [[ -c /dev/loop-control ]] || mount -t devtmpfs devtmpfs /dev
        truncate -s 40G "$scratch/env-tools-state.img"
        mkfs.ext4 -q "$scratch/env-tools-state.img"
        mount -o loop "$scratch/env-tools-state.img" "$state" ;;
    esac
    work=$scratch/env-tools-$arch
    # Sealed for the runtime's publisher without its key: only seal-env-tools signs.
    TILO_ENV_TOOLS_PUBLISHER=$publisher TILO_ENV_TOOLS_NETWORK=host TILO_ENV_TOOLS_STATE=$state \
      bash os/dev/build-env-tools.sh "$runtime" "$work"
    cp "$work"/*.tapp "$out/"
    cp "$work/catalog.json" "$out/catalog-$arch.json"
    cp "$work/report.json" "$out/report-$arch.json"
    printf '%s\n' "$runtime_id" > "$out/runtime.id"
    printf '%s\n' "$publisher" > "$out/publisher.hex"
    printf '%s\n' "$source" > "$out/runtime-source"
    ;;
  *) die 'usage: env-tools.sh binaries|build ...' ;;
esac
