#!/usr/bin/env bash
# Run one packaging target in its own architecture-specific CI container.
# The outer mode is invoked by Forgejo on the existing runner. ARM jobs install
# a pinned QEMU binfmt handler inside this same step before starting the target
# container; a separate setup-action step lands in the wrong Kata namespace.
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
    log_dir=$(mktemp -d)
    container=
    cleanup_container() {
        if [[ -n $container ]]; then
            docker rm --force "$container" >/dev/null 2>&1 || true
        fi
        rm -rf -- "$log_dir"
    }
    trap cleanup_container EXIT

    inner_command='
        set -eu
        mkdir /artifacts
        cd /workspace
        exec dist/ci/run-package-job.sh
    '
    entrypoint=/bin/sh
    container_args=(-c "$inner_command")
    if [[ $arch == arm64 ]]; then
        qemu_image=docker.io/tonistiigi/binfmt@sha256:400a4873b838d1b89194d982c45e5fb3cda4593fbfd7e08a02e76b03b21166f0
        binfmt_log=$log_dir/binfmt.log
        set +e
        docker run --rm --privileged --platform linux/amd64 \
            "$qemu_image" --install arm64 2>&1 | tee "$binfmt_log"
        binfmt_status=${PIPESTATUS[0]}
        set -e
        if (( binfmt_status != 0 )); then
            {
                printf 'stage=install-binfmt status=%s\n' "$binfmt_status"
                tail -n 8 "$binfmt_log"
            } > "$output/CI_FAILURE"
            exit "$binfmt_status"
        fi
    fi

    create_log=$log_dir/create.log
    set +e
    container=$(docker create --platform "linux/$arch" \
        --entrypoint "$entrypoint" \
        --env QOS_IN_PACKAGE_CONTAINER=1 \
        --env QOS_PACKAGE_DISTRO="$distro" \
        --env QOS_EXPECT_ARCH="$arch" \
        --env QOS_DEB_REVISION="$revision" \
        --env CARGO_INCREMENTAL=0 \
        --env CARGO_TERM_COLOR=never \
        "$image" "${container_args[@]}" 2> >(tee "$create_log" >&2))
    container_status=$?
    set -e
    if (( container_status != 0 )); then
        {
            printf 'stage=create-container status=%s\n' "$container_status"
            tail -n 8 "$create_log"
        } > "$output/CI_FAILURE"
        exit "$container_status"
    fi
    # docker cp works with Docker-in-Docker and socket-mounted runners where a
    # bind path inside the runner is not a real path from the daemon's view.
    copy_in_log=$log_dir/copy-in.log
    set +e
    docker cp "$repo_root/." "$container:/workspace" 2>&1 | tee "$copy_in_log"
    container_status=${PIPESTATUS[0]}
    set -e
    if (( container_status != 0 )); then
        {
            printf 'stage=copy-source status=%s\n' "$container_status"
            tail -n 8 "$copy_in_log"
        } > "$output/CI_FAILURE"
        exit "$container_status"
    fi
    start_log=$log_dir/start.log
    set +e
    docker start --attach "$container" 2>&1 | tee "$start_log"
    container_status=${PIPESTATUS[0]}
    set -e
    copy_out_log=$log_dir/copy-out.log
    set +e
    docker cp "$container:/artifacts/." "$output/" 2>&1 | tee "$copy_out_log"
    copy_status=${PIPESTATUS[0]}
    set -e
    if (( copy_status != 0 )); then
        {
            printf 'stage=copy-artifacts status=%s\n' "$copy_status"
            tail -n 8 "$copy_out_log"
        } >> "$output/CI_FAILURE"
        if (( container_status == 0 )); then
            container_status=$copy_status
        fi
    fi
    if (( container_status != 0 )); then
        if [[ ! -f $output/CI_FAILURE ]]; then
            printf 'stage=start-container status=%s\n' "$container_status" \
                > "$output/CI_FAILURE"
        fi
        tail -n 8 "$start_log" >> "$output/CI_FAILURE"
    fi
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
    export PATH=$CARGO_HOME/bin:$PATH
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
    export PATH=$CARGO_HOME/bin:$PATH
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
    # pacman 7's download sandbox uses Landlock. The Forgejo/Kata kernel does
    # not expose it; Arch documents DisableSandbox specifically for this case.
    sed -i '/^\[options\]/a DisableSandbox' /etc/pacman.conf
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
