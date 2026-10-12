#!/usr/bin/env bash
# Acquire signed Alpine packages in a disposable rootless container. Run on
# an approved remote builder; this does not install anything on the host.
set -euo pipefail
if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    echo 'Usage: acquire_alpine_runtime.sh docker.io/library/alpine@sha256:<digest> NEW_OUTPUT_DIR [x86_64|aarch64]' >&2
    exit 2
fi
image=$1
output=$2
architecture=${3:-$(uname -m)}
if [[ "$architecture" != x86_64 && "$architecture" != aarch64 ]]; then
    echo 'Unsupported runtime architecture' >&2
    exit 2
fi
if ! [[ "$image" =~ ^docker\.io/library/alpine@sha256:[0-9a-f]{64}$ ]]; then
    echo 'An immutable official Alpine image digest is required' >&2
    exit 2
fi
if [ "$(id -u)" -eq 0 ] || [ "$(podman info --format '{{.Host.Security.Rootless}}')" != true ]; then
    echo 'Runtime acquisition requires rootless podman' >&2
    exit 2
fi
mkdir -m 700 -- "$output"
scratch=$(mktemp -d)
container="tilo-atl-$(basename "$scratch")"
rmdir "$scratch"
created=0
cleanup() {
    if [ "$created" -eq 1 ]; then podman rm -f "$container" >/dev/null; fi
}
trap cleanup EXIT
podman pull "$image" >"$output/image-pull.log" 2>&1
image_arch=$(podman image inspect "$image" --format '{{.Architecture}}')
case "$image_arch" in amd64) image_arch=x86_64 ;; arm64) image_arch=aarch64 ;; esac
if [[ "$image_arch" != x86_64 && "$image_arch" != aarch64 ]]; then
    echo 'Unsupported acquisition image architecture' >&2
    exit 2
fi
created=1
podman create --name "$container" --cpus=2 --memory=2g --pids-limit=256 \
    --env "TILO_ACQUIRE_ARCH=$architecture" \
    --env "TILO_ACQUIRE_IMAGE_ARCH=$image_arch" \
    "$image" sh -eu -c '
    printf "%s\n" https://dl-cdn.alpinelinux.org/alpine/edge/main https://dl-cdn.alpinelinux.org/alpine/edge/community https://dl-cdn.alpinelinux.org/alpine/edge/testing > /etc/apk/repositories
    if [ "$TILO_ACQUIRE_IMAGE_ARCH" = "$TILO_ACQUIRE_ARCH" ]; then
        apk add --no-cache android-translation-layer=0_git20260705-r1 art_standalone=0_git20251009-r2 font-dejavu mesa-dri-gallium
        /usr/bin/android-translation-layer --help
        cat /lib/apk/db/installed
    else
        # Signed package staging only. No binfmt registration, package scripts,
        # emulator, target binary execution or host package installation.
        mkdir -p /runtime-root/etc/apk/keys
        # alpine-keys in the immutable official image contains target
        # authenticated architecture-specific public keys as well as native ones.
        cp -L /usr/share/apk/keys/"$TILO_ACQUIRE_ARCH"/* /runtime-root/etc/apk/keys/
        apk --arch "$TILO_ACQUIRE_ARCH" --root /runtime-root --initdb --no-scripts \
            --keys-dir /runtime-root/etc/apk/keys --repositories-file /etc/apk/repositories \
            add --no-cache android-translation-layer=0_git20260705-r1 \
            art_standalone=0_git20251009-r2 font-dejavu mesa-dri-gallium musl-dev
        cat /runtime-root/lib/apk/db/installed
        printf "%s\n" "Cross-architecture signed staging only; target runtime not executed."
        tar -C /runtime-root -cf /target-rootfs.tar lib usr etc
    fi
' >"$output/container-id.txt"
podman start -a "$container" >"$output/acquisition.log" 2>&1
if [ "$(podman inspect "$container" --format '{{.State.ExitCode}}')" != 0 ]; then
    echo 'Signed runtime acquisition failed; inspect acquisition.log' >&2
    exit 2
fi
if [ "$image_arch" = "$architecture" ]; then
    podman export "$container" >"$output/rootfs.tar"
else
    podman cp "$container:/target-rootfs.tar" "$output/rootfs.tar"
fi
printf '%s\n' "$image" >"$output/base-image.txt"
cat "$output/base-image.txt" "$output/acquisition.log" >"$output/provenance.txt"
sha256sum "$output/rootfs.tar" >"$output/rootfs.sha256"
