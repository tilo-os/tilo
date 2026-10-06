#!/usr/bin/env bash
# Acquire signed Alpine packages in a disposable rootless container. Run on
# an approved remote builder; this does not install anything on the host.
set -euo pipefail
if [ "$#" -ne 2 ]; then
    echo 'Usage: acquire_alpine_runtime.sh docker.io/library/alpine@sha256:<digest> NEW_OUTPUT_DIR' >&2
    exit 2
fi
image=$1
output=$2
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
podman create --name "$container" --cpus=2 --memory=2g --pids-limit=256 \
    "$image" sh -eu -c '
    printf "%s\n" https://dl-cdn.alpinelinux.org/alpine/edge/main https://dl-cdn.alpinelinux.org/alpine/edge/community https://dl-cdn.alpinelinux.org/alpine/edge/testing > /etc/apk/repositories
    apk add --no-cache android-translation-layer=0_git20260705-r1 art_standalone=0_git20251009-r2 font-dejavu mesa-dri-gallium
    /usr/bin/android-translation-layer --help
    cat /lib/apk/db/installed
' >"$output/container-id.txt"
created=1
podman start -a "$container" >"$output/acquisition.log" 2>&1
if [ "$(podman inspect "$container" --format '{{.State.ExitCode}}')" != 0 ]; then
    echo 'Signed runtime acquisition failed; inspect acquisition.log' >&2
    exit 2
fi
podman export "$container" >"$output/rootfs.tar"
printf '%s\n' "$image" >"$output/base-image.txt"
cat "$output/base-image.txt" "$output/acquisition.log" >"$output/provenance.txt"
sha256sum "$output/rootfs.tar" >"$output/rootfs.sha256"
