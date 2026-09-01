#!/usr/bin/env bash
# Run one packaging target in its own architecture-specific CI userspace.
# amd64 starts the target container normally. ARM is exported and run through
# per-job PRoot/QEMU because this runner isolates binfmt_misc registration from
# the Docker daemon that starts target containers.
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
    if [[ $arch == arm64 ]]; then
        cargo_jobs=2
    else
        cargo_jobs=4
    fi
    cache_env=()
    if [[ ${QOS_ENABLE_REMOTE_CACHE:-0} == 1 ]]; then
        export SCCACHE_S3_KEY_PREFIX="quickdesktop/package/v1/$distro/$arch"
        export SCCACHE_IGNORE_SERVER_IO_ERROR=1
        while IFS= read -r name; do
            cache_env+=(--env "$name")
        done < <(compgen -e | grep -E '^(AWS_|SCCACHE_)' || true)
    else
        # PRoot inherits this process's environment, unlike Docker where only
        # cache_env is forwarded. Scrub runner-injected credentials before an
        # untrusted pull request can enter either target userspace.
        while IFS= read -r name; do
            unset "$name"
        done < <(compgen -e | grep -E '^(AWS_|SCCACHE_)' || true)
    fi

    repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
    output=$repo_root/dist/ci-output/$distro-$arch
    [[ ! -e $output ]] || { echo "output path already exists: $output" >&2; exit 1; }
    mkdir -p "$output"
    log_dir=$(mktemp -d)
    container=
    tool_container=
    rootfs=
    cleanup_container() {
        local cleanup_paths=("$log_dir")
        if [[ -n $container ]]; then
            docker rm --force "$container" >/dev/null 2>&1 || true
        fi
        if [[ -n $tool_container ]]; then
            docker rm --force "$tool_container" >/dev/null 2>&1 || true
        fi
        [[ -z $rootfs ]] || cleanup_paths+=("$rootfs")
        rm -rf -- "${cleanup_paths[@]}"
    }
    trap cleanup_container EXIT

    inner_command='
        set -eu
        mkdir -p /artifacts
        cd /workspace
        exec dist/ci/run-package-job.sh
    '
    create_log=$log_dir/create.log
    set +e
    if [[ $arch == amd64 ]]; then
        container=$(docker create --platform linux/amd64 \
            --entrypoint /bin/sh \
            --env QOS_IN_PACKAGE_CONTAINER=1 \
            --env QOS_PACKAGE_DISTRO="$distro" \
            --env QOS_EXPECT_ARCH="$arch" \
            --env QOS_DEB_REVISION="$revision" \
            --env CARGO_INCREMENTAL=0 \
            --env CARGO_BUILD_JOBS="$cargo_jobs" \
            --env CARGO_TERM_COLOR=never \
            "${cache_env[@]}" \
            "$image" -c "$inner_command" 2> >(tee "$create_log" >&2))
    else
        container=$(docker create --platform linux/arm64 "$image" \
            2> >(tee "$create_log" >&2))
    fi
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
    if [[ $arch == arm64 ]]; then
        qemu_image=docker.io/tonistiigi/binfmt@sha256:400a4873b838d1b89194d982c45e5fb3cda4593fbfd7e08a02e76b03b21166f0
        proot_sha=b7f2adf5a225000a164f4905aabefeebe11c4c1d5bedff5e1fe8866c48dd70d2
        tools_dir=$log_dir/tools
        rootfs=$(mktemp -d)
        mkdir -p "$tools_dir"

        stage_log=$log_dir/emulator-tools.log
        set +e
        (
            set -e
            curl --proto '=https' --tlsv1.2 -fsSL \
                https://proot.gitlab.io/proot/bin/proot -o "$tools_dir/proot"
            printf '%s  %s\n' "$proot_sha" "$tools_dir/proot" | sha256sum --check --status
            chmod +x "$tools_dir/proot"
            docker create --platform linux/amd64 "$qemu_image" \
                > "$log_dir/tool-container-id"
            tool_container=$(<"$log_dir/tool-container-id")
            docker cp "$tool_container:/usr/bin/qemu-aarch64" "$tools_dir/qemu-aarch64"
            cc -O2 -Wall -Wextra -o "$tools_dir/qemu-argv0-wrapper" \
                "$repo_root/dist/ci/qemu-argv0-wrapper.c"
            docker export "$container" | tar --extract --directory "$rootfs"
            # docker export deliberately omits declared volumes. Copy their
            # initialized contents (notably Nix's /nix store) from the stopped
            # container so the exported userspace remains executable.
            while IFS= read -r volume; do
                [[ -z $volume ]] && continue
                mkdir -p "$rootfs$volume"
                docker cp "$container:$volume/." "$rootfs$volume/"
            done < <(docker inspect --format \
                '{{range .Mounts}}{{println .Destination}}{{end}}' "$container")
            mkdir -p "$rootfs/artifacts"
            if [[ $distro == ubuntu-24.04 || $distro == debian-13 ]]; then
                install -Dm755 "$repo_root/dist/ci/proot-apt-key" \
                    "$rootfs/usr/local/libexec/qos-proot-apt-key"
                printf 'Dir::Bin::apt-key "/usr/local/libexec/qos-proot-apt-key";\n' \
                    > "$rootfs/etc/apt/apt.conf.d/00qos-proot"
            fi
        ) > >(tee "$stage_log") 2>&1
        tool_status=$?
        set -e
        if [[ -s $log_dir/tool-container-id ]]; then
            tool_container=$(<"$log_dir/tool-container-id")
        fi
        if (( tool_status != 0 )); then
            {
                printf 'stage=prepare-user-emulation status=%s\n' "$tool_status"
                tail -n 8 "$stage_log"
            } > "$output/CI_FAILURE"
            exit "$tool_status"
        fi

        export QOS_IN_PACKAGE_CONTAINER=1
        export QOS_PACKAGE_DISTRO="$distro"
        export QOS_EXPECT_ARCH="$arch"
        export QOS_DEB_REVISION="$revision"
        export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS="$cargo_jobs" CARGO_TERM_COLOR=never
        export QOS_QEMU_BINARY="/host-rootfs$tools_dir/qemu-aarch64"
        set +e
        "$tools_dir/proot" -0 -q "$tools_dir/qemu-argv0-wrapper" \
            -r "$rootfs" -b /proc -b /dev -b /sys -b /etc/resolv.conf \
            -w /workspace /bin/sh -c "$inner_command" 2>&1 | tee "$start_log"
        container_status=${PIPESTATUS[0]}
        set -e
        copy_out_log=$log_dir/copy-out.log
        set +e
        cp -a "$rootfs/artifacts/." "$output/" 2>&1 | tee "$copy_out_log"
        copy_status=${PIPESTATUS[0]}
        set -e
    else
        set +e
        docker start --attach "$container" 2>&1 | tee "$start_log"
        container_status=${PIPESTATUS[0]}
        set -e
        copy_out_log=$log_dir/copy-out.log
        set +e
        docker cp "$container:/artifacts/." "$output/" 2>&1 | tee "$copy_out_log"
        copy_status=${PIPESTATUS[0]}
        set -e
    fi
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
    stage=install-compiler-cache
    dist/ci/install-sccache.sh "$arch"
    [[ ! -x /usr/local/bin/sccache ]] || export RUSTC_WRAPPER=/usr/local/bin/sccache
    stage=build-deb
    dist/debian/build-deb.sh
    stage=inspect-deb
    dpkg-deb --info dist/debian/build/qos-session*.deb
    stage=install-test-deb
    dist/debian/test-package.sh \
        dist/debian/build/qos-session_[0-9]*.deb \
        dist/debian/build/qos-session-devel_*.deb
    stage=collect-deb
    cp dist/debian/build/qos-session_[0-9]*_"$arch".deb /artifacts/
    if [[ $arch == amd64 ]]; then
        cp dist/debian/build/qos-session-devel_*_all.deb /artifacts/
    fi
    ;;
fedora-44)
    export CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup
    export PATH=$CARGO_HOME/bin:$PATH
    stage=install-dependencies
    dist/ci/install-build-deps.sh fedora
    stage=install-compiler-cache
    dist/ci/install-sccache.sh "$arch"
    [[ ! -x /usr/local/bin/sccache ]] || export RUSTC_WRAPPER=/usr/local/bin/sccache
    stage=build-rpm
    dist/rpm/build-rpm.sh
    stage=inspect-rpm
    rpm -qip dist/rpm/build/RPMS/*/qos-session-*.rpm
    stage=lint-rpm
    rpmlint dist/rpm/build/RPMS/*/qos-session-*.rpm
    stage=install-test-rpm
    dist/rpm/test-package.sh dist/rpm/build/RPMS/*/qos-session-*.rpm
    stage=collect-rpm
    cp dist/rpm/build/RPMS/*/*.rpm /artifacts/
    ;;
arch)
    # pacman 7's download sandbox uses Landlock. The Forgejo/Kata kernel does
    # not expose it; Arch documents DisableSandbox specifically for this case.
    sed -i '/^\[options\]/a DisableSandbox' /etc/pacman.conf
    # Empty directories are not represented in an OCI layer and can disappear
    # when the ARM image is exported rather than started by Docker.
    mkdir -p /var/lib/pacman
    stage=install-dependencies
    dist/ci/install-build-deps.sh arch
    stage=install-compiler-cache
    dist/ci/install-sccache.sh "$arch"
    [[ ! -x /usr/local/bin/sccache ]] || export RUSTC_WRAPPER=/usr/local/bin/sccache
    stage=prepare-arch-builder
    useradd --create-home builder
    chown -R builder:builder /workspace
    stage=build-arch
    su builder -c 'cd /workspace && dist/arch/build-arch.sh'
    stage=inspect-arch
    pacman -Qip dist/arch/build/qos-session-*.pkg.tar.*
    stage=lint-arch
    namcap_log=$(mktemp)
    namcap dist/arch/build/qos-session-*.pkg.tar.* | tee "$namcap_log"
    if grep -q ' E: ' "$namcap_log"; then
        echo "namcap reported a package error" >&2
        exit 1
    fi
    stage=install-test-arch
    dist/arch/test-package.sh dist/arch/build/qos-session-*.pkg.tar.*
    stage=collect-arch
    cp dist/arch/build/*.pkg.tar.zst /artifacts/
    ;;
nix)
    export NIX_CONFIG=$'experimental-features = nix-command flakes\nsandbox = false\ncores = 2'
    stage=install-nix-tools
    nix-env -iA nixpkgs.gzip nixpkgs.file
    stage=build-nix
    nix build --print-build-logs .#qos-session
    stage=inspect-nix
    dist/ci/verify-nix-result.sh result "$arch"
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
if command -v sccache >/dev/null 2>&1; then
    sccache --show-stats || true
fi
chmod -R a+rX /artifacts
