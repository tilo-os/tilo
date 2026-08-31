#!/usr/bin/env bash
# Run one packaging target in its own architecture-specific CI container.
# The outer mode is invoked by Forgejo on the existing runner. The inner mode
# runs after docker/setup-qemu-action has registered arm64 binfmt support.
set -euo pipefail

usage() {
    echo "usage: $0 DISTRO IMAGE amd64|arm64 [DEBIAN_REVISION]" >&2
    exit 2
}

if [[ ${QOS_IN_PACKAGE_CONTAINER:-0} != 1 ]]; then
    [[ $# -ge 3 && $# -le 4 ]] || usage
    distro=$1
    image=$2
    arch=$3
    revision=${4:-}
    case "$arch" in amd64|arm64) ;; *) usage ;; esac

    repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
    output=$repo_root/dist/ci-output/$distro-$arch
    [[ ! -e $output ]] || { echo "output path already exists: $output" >&2; exit 1; }
    mkdir -p "$output"

    container=$(docker create --platform "linux/$arch" \
        --entrypoint /bin/sh \
        --env QOS_IN_PACKAGE_CONTAINER=1 \
        --env QOS_PACKAGE_DISTRO="$distro" \
        --env QOS_EXPECT_ARCH="$arch" \
        --env QOS_DEB_REVISION="$revision" \
        --env CARGO_INCREMENTAL=0 \
        --env CARGO_TERM_COLOR=never \
        "$image" -c '
            set -eu
            mkdir /artifacts
            cd /workspace
            exec dist/ci/run-package-job.sh
        ')
    cleanup_container() {
        docker rm --force "$container" >/dev/null 2>&1 || true
    }
    trap cleanup_container EXIT
    # docker cp works with Docker-in-Docker and socket-mounted runners where a
    # bind path inside the runner is not a real path from the daemon's view.
    docker cp "$repo_root/." "$container:/workspace"
    set +e
    docker start --attach "$container"
    container_status=$?
    set -e
    docker cp "$container:/artifacts/." "$output/" || true
    exit "$container_status"
fi

[[ $# == 0 ]] || usage
distro=${QOS_PACKAGE_DISTRO:?missing QOS_PACKAGE_DISTRO}
arch=${QOS_EXPECT_ARCH:?missing QOS_EXPECT_ARCH}
export QOS_EXPECT_ARCH="$arch"

stage=bootstrap
record_failure() {
    local status=$?
    trap - ERR
    printf 'stage=%s status=%s line=%s\n' "$stage" "$status" "${BASH_LINENO[0]:-unknown}" \
        > /artifacts/CI_FAILURE
    exit "$status"
}
trap record_failure ERR

stage=architecture-check
dist/ci/assert-native-arch.sh "$arch"

case "$distro" in
ubuntu-24.04|debian-13)
    export CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup
    export QOS_DEB_REVISION=${QOS_DEB_REVISION:?missing QOS_DEB_REVISION}
    stage=install-dependencies
    dist/ci/install-build-deps.sh debian
    stage=build-deb
    dist/debian/build-deb.sh
    stage=inspect-deb
    dpkg-deb --info dist/debian/build/qos-session_*.deb
    stage=install-test-deb
    dist/debian/test-package.sh dist/debian/build/qos-session_*.deb
    stage=collect-deb
    cp dist/debian/build/qos-session_[0-9]*_"$arch".deb /artifacts/
    if [[ $arch == amd64 ]]; then
        cp dist/debian/build/qos-session-devel_*_all.deb /artifacts/
    fi
    ;;
fedora-42)
    export CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup
    stage=install-dependencies
    dist/ci/install-build-deps.sh fedora
    stage=build-rpm
    dist/rpm/build-rpm.sh
    stage=inspect-rpm
    rpm -qip dist/rpm/build/RPMS/*/qos-session-*.rpm
    stage=install-test-rpm
    dist/rpm/test-package.sh dist/rpm/build/RPMS/*/qos-session-*.rpm
    stage=collect-rpm
    cp dist/rpm/build/RPMS/*/*.rpm /artifacts/
    ;;
arch)
    stage=install-dependencies
    dist/ci/install-build-deps.sh arch
    stage=prepare-arch-builder
    useradd --create-home builder
    chown -R builder:builder /workspace
    stage=build-arch
    su builder -c 'cd /workspace && dist/arch/build-arch.sh'
    stage=inspect-arch
    pacman -Qip dist/arch/build/qos-session-*.pkg.tar.*
    stage=install-test-arch
    dist/arch/test-package.sh dist/arch/build/qos-session-*.pkg.tar.*
    stage=collect-arch
    cp dist/arch/build/*.pkg.tar.zst /artifacts/
    ;;
nix)
    export NIX_CONFIG='experimental-features = nix-command flakes'
    stage=install-nix-tools
    nix-env -iA nixpkgs.gzip nixpkgs.file
    stage=build-nix
    nix build --print-build-logs .#qos-session
    stage=inspect-nix
    if [[ $arch == amd64 ]]; then
        file result/bin/qos-shell | grep -Eq 'ELF 64-bit.*x86-64'
    else
        file result/bin/qos-shell | grep -Eq 'ELF 64-bit.*ARM aarch64'
    fi
    test -f result/share/wayland-sessions/qos.desktop
    nix flake check --no-build
    stage=export-nix-closure
    mapfile -t requisites < <(nix-store --query --requisites result)
    nix-store --export "${requisites[@]}" \
        | gzip -9 > "/artifacts/qos-session-$arch.nar.gz"
    ;;
*)
    echo "unknown package distribution: $distro" >&2
    exit 2
    ;;
esac

stage=verify-artifacts
find /artifacts -mindepth 1 -maxdepth 1 -type f -print
test -n "$(find /artifacts -mindepth 1 -maxdepth 1 -type f -print -quit)"
chmod -R a+rX /artifacts
