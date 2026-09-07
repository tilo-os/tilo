#!/usr/bin/env bash
# Run one packaging target in its own architecture-specific CI userspace.
# Native hosts start the matching target container directly. ARM can still use
# the PRoot/QEMU fallback when this helper is invoked on an x86-only host.
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
    execution_arch=$arch
    emulated=0
    if [[ $arch == arm64 && $(uname -m) != aarch64 ]]; then
        emulated=1
    fi
    if [[ $arch == arm64 ]]; then
        cargo_jobs=2
    else
        cargo_jobs=4
    fi
    cache_env=()
    if [[ ${QOS_ENABLE_REMOTE_CACHE:-0} == 1 ]]; then
        # sccache hashes compiler identity, flags and native headers into every
        # object key. Sharing the namespace by architecture safely reuses the
        # large distro-neutral Rust graph while ABI-specific objects naturally
        # miss, instead of forcing five entirely isolated cold caches.
        export SCCACHE_S3_KEY_PREFIX="quickdesktop/package/v2/$arch"
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
    source_epoch=${SOURCE_DATE_EPOCH:-$(git -C "$repo_root" log -1 --format=%ct)}
    source_revision=${GITHUB_SHA:-$(git -C "$repo_root" rev-parse HEAD)}
    export SOURCE_DATE_EPOCH="$source_epoch" GITHUB_SHA="$source_revision"
    signing_env=()
    if [[ $distro == fedora-44 ]]; then
        signing_env=(
            --env QOS_RPM_SIGNING_ENABLED
            --env QOS_RPM_SIGNING_KEY_ARTIFACT
            --env QOS_RPM_SIGNING_KEY_B64
            --env QOS_RPM_SIGNING_KEY_ID
            --env QOS_RPM_SIGNING_FINGERPRINT
        )
    else
        # Do not expose the RPM signing secret to unrelated target userspaces.
        unset QOS_RPM_SIGNING_ENABLED QOS_RPM_SIGNING_KEY_ARTIFACT \
            QOS_RPM_SIGNING_KEY_B64 QOS_RPM_SIGNING_KEY_ID QOS_RPM_SIGNING_FINGERPRINT
    fi
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
        PATH=/root/.nix-profile/bin:/nix/var/nix/profiles/default/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
        export PATH
        mkdir -p /artifacts
        cd /workspace
        exec dist/ci/run-package-job.sh
    '
    create_log=$log_dir/create.log
    set +e
    if [[ $emulated == 0 ]]; then
        container=$(docker create --platform "linux/$arch" \
            --entrypoint /bin/sh \
            --env QOS_IN_PACKAGE_CONTAINER=1 \
            --env QOS_PACKAGE_DISTRO="$distro" \
            --env QOS_EXPECT_ARCH="$arch" \
            --env QOS_EXECUTION_ARCH="$execution_arch" \
            --env QOS_EMULATED=0 \
            --env QOS_DEB_REVISION="$revision" \
            --env SOURCE_DATE_EPOCH="$source_epoch" \
            --env GITHUB_SHA="$source_revision" \
            --env CARGO_INCREMENTAL=0 \
            --env CARGO_BUILD_JOBS="$cargo_jobs" \
            --env CARGO_TERM_COLOR=never \
            "${signing_env[@]}" \
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
    if [[ $emulated == 1 ]]; then
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
            mkdir -p "$rootfs/artifacts" "$rootfs/tmp" "$rootfs/var/tmp"
            chmod 1777 "$rootfs/tmp" "$rootfs/var/tmp"
            if [[ $distro == arch ]]; then
                # The Arch OCI layers omit package-manager state directories
                # when they are empty. PRoot/QEMU also fails libalpm's realpath
                # check for the conventional /var/lib/pacman path even after
                # it is recreated. Put package state outside the exported
                # rootfs and bind it at a distinct guest path. The guest
                # performs refresh and dependency
                # installation in one transaction so libalpm never has to
                # reopen a host-seeded database through PRoot's faccessat
                # translation.
                install -d -m 0755 \
                    "$rootfs/var/lib/pacman" \
                    "$rootfs/var/lib/pacman/local" \
                    "$rootfs/var/lib/pacman/sync" \
                    "$log_dir/arch-pacman/db/local" \
                    "$log_dir/arch-pacman/db/sync" \
                    "$log_dir/arch-pacman/cache"
                cp -a "$rootfs/var/lib/pacman/." \
                    "$log_dir/arch-pacman/db/"
            fi
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
        export QOS_EXECUTION_ARCH="$execution_arch"
        export QOS_EMULATED=1
        export QOS_DEB_REVISION="$revision"
        export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS="$cargo_jobs" CARGO_TERM_COLOR=never
        export QOS_QEMU_BINARY="/host-rootfs$tools_dir/qemu-aarch64"
        proot_package_binds=()
        if [[ $distro == arch ]]; then
            # libalpm could not see databases through PRoot's implicit rootfs
            # translation even though the guest shell could stat them. Source
            # explicit binds from outside the translated rootfs so PRoot cannot
            # canonicalize them back into the same invisible guest path.
            proot_package_binds=(
                -b "$log_dir/arch-pacman/db:/var/lib/qos-pacman"
                -b "$log_dir/arch-pacman/cache:/var/cache/qos-pacman"
            )
        fi
        set +e
        "$tools_dir/proot" -0 -q "$tools_dir/qemu-argv0-wrapper" \
            -r "$rootfs" -b /proc -b /dev -b /sys -b /etc/resolv.conf \
            "${proot_package_binds[@]}" \
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
execution_arch=${QOS_EXECUTION_ARCH:-$arch}
emulated=${QOS_EMULATED:-0}
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
dist/ci/assert-native-arch.sh "$execution_arch"

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
    for package in dist/debian/build/qos-session*.deb; do
        dpkg-deb --info "$package"
    done
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
    rpmlint_log=$(mktemp)
    set +e
    rpmlint dist/rpm/build/RPMS/*/qos-session-*.rpm 2>&1 | tee "$rpmlint_log"
    rpmlint_status=${PIPESTATUS[0]}
    set -e
    # Fedora permits no-signature for local and branch-validation artifacts.
    # Protected desktop tags sign only after this install test, in this Fedora
    # userspace, and immediately verify the result before artifact collection.
    # Every package-content or policy error still fails.
    if grep ' E: ' "$rpmlint_log" \
        | grep -Ev ' (no-signature|no-packager-tag)( |$)' >/dev/null; then
        echo "rpmlint reported a package error" >&2
        exit 1
    fi
    if (( rpmlint_status != 0 )) && ! grep -q ' E: ' "$rpmlint_log"; then
        echo "rpmlint could not complete" >&2
        exit "$rpmlint_status"
    fi
    stage=install-test-rpm
    dist/rpm/test-package.sh dist/rpm/build/RPMS/*/qos-session-*.rpm
    stage=collect-rpm
    cp dist/rpm/build/RPMS/*/*.rpm /artifacts/
    if [[ ${QOS_RPM_SIGNING_ENABLED:-0} == 1 ]]; then
        stage=sign-rpm
        QOS_RPM_SIGNING_KEY_ARTIFACT=${QOS_RPM_SIGNING_KEY_ARTIFACT:-0} \
            dist/rpm/sign-rpms.sh /artifacts
        if [[ ${QOS_RPM_SIGNING_KEY_ARTIFACT:-0} == 1 ]]; then
            dist/rpm/make-repo-file.sh "$(<dist/VERSION)" \
                /artifacts/qos-fedora-44.repo
            (cd /artifacts && sha256sum qos-fedora-44.repo > qos-fedora-44.repo.sha256)
        fi
    fi
    stage=collect-os-desktop-payload
    case "$arch" in amd64) payload_arch=x86_64 ;; arm64) payload_arch=aarch64 ;; esac
    dist/desktop/package-os-payload.sh target/release "$payload_arch" /artifacts
    ;;
arch)
    # pacman 7's download sandbox uses Landlock. The Forgejo/Kata kernel does
    # not expose it; Arch documents DisableSandbox specifically for this case.
    sed -i '/^\[options\]/a DisableSandbox' /etc/pacman.conf
    if [[ $arch == arm64 && $emulated == 1 ]]; then
        sed -i '/^\[options\]/a CacheDir = /var/cache/qos-pacman\nDBPath = /var/lib/qos-pacman' \
            /etc/pacman.conf
        install -d -m 0755 \
            /var/lib/qos-pacman/local \
            /var/lib/qos-pacman/sync \
            /var/cache/qos-pacman
        test "$(pacman-conf DBPath)" = /var/lib/qos-pacman
    fi
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
    nix_config=$'experimental-features = nix-command flakes\nsandbox = false\ncores = 2'
    if [[ $arch == arm64 && $emulated == 1 ]]; then
        # Nix's seccomp filter observes qemu-user's host syscalls rather than
        # the emulated guest calls and can SIGKILL store realization. Disable
        # only that filter in this architecture-emulated CI userspace; the
        # build still uses the target-native Nixpkgs package and dependencies.
        nix_config+=$'\nfilter-syscalls = false'
    fi
    export NIX_CONFIG=$nix_config
    stage=build-nix
    nix build --print-build-logs .#qos-session
    stage=inspect-nix
    dist/ci/verify-nix-result.sh result "$arch"
    nix flake check --no-build --all-systems
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
