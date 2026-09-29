#!/usr/bin/env bash
# Install the repository-pinned static sccache client when the runner exposes
# its optional backend (the job's Actions cache). A missing or unavailable cache must never affect
# correctness; it only changes how much compiler work an iteration repeats.
set -euo pipefail

[[ $# == 1 ]] || { echo "usage: $0 amd64|arm64" >&2; exit 2; }
arch=$1

if [[ ${SCCACHE_GHA_ENABLED:-} != on || -z ${ACTIONS_CACHE_URL:-} ]]; then
    echo "remote compiler cache is not configured; compiling normally"
    exit 0
fi

version=0.17.0
case "$arch" in
amd64)
    target=x86_64-unknown-linux-musl
    checksum=67c4a96dd237c1f518f6b36083f270f9976d516f1e57fce891755ea782e50006
    ;;
arm64)
    target=aarch64-unknown-linux-musl
    checksum=821a86343191aa1cbab74bd42f9e93c9a63bf85e4742945f40d3ae84193c1c77
    ;;
*)
    echo "unsupported sccache architecture: $arch" >&2
    exit 2
    ;;
esac

archive=$(mktemp)
unpack=$(mktemp -d)
cleanup() {
    rm -f -- "$archive"
    rm -rf -- "$unpack"
}
trap cleanup EXIT

name="sccache-v${version}-${target}"
repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cached_archive="$repo_root/os/external-assets/cache/${name}.tar.gz"
if [[ -f $cached_archive ]]; then
    cp -- "$cached_archive" "$archive"
else
    if ! curl --proto '=https' --tlsv1.2 -fsSL \
        "https://github.com/mozilla/sccache/releases/download/v${version}/${name}.tar.gz" \
        -o "$archive"; then
        echo "::warning::could not download optional sccache client; compiling normally"
        exit 0
    fi
fi
if ! printf '%s  %s\n' "$checksum" "$archive" | sha256sum --check --status; then
    echo "::warning::optional sccache client checksum failed; compiling normally"
    exit 0
fi
if ! tar -xzf "$archive" -C "$unpack" \
    || ! install -Dm755 "$unpack/$name/sccache" /usr/local/bin/sccache; then
    echo "::warning::could not install optional sccache client; compiling normally"
    exit 0
fi

if ! installed_version=$(sccache --version); then
    rm -f -- /usr/local/bin/sccache
    echo "::warning::optional sccache client cannot execute; compiling normally"
    exit 0
fi
printf 'remote compiler cache enabled: %s (%s)\n' "$installed_version" \
    "${SCCACHE_GHA_VERSION:-default namespace}"
