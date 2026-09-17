#!/usr/bin/env bash
# Fast Fedora 44 edit/build/deploy loop for a disposable Tilo VM.
#
# This is intentionally separate from dist/rpm/build-rpm.sh.  The normal loop
# compiles only the selected debug binary into one target directory and copies
# it to the VM over SSH.  It never builds an RPM, changes a login session, runs
# rpm-ostree, or reboots a machine.
#
# Usage:
#   dist/dev/fedora44-vm-iterate.sh bootstrap
#   dist/dev/fedora44-vm-iterate.sh build [shell|host|PACKAGE ...]
#   TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh deploy [shell|PACKAGE ...]
#   dist/dev/fedora44-vm-iterate.sh handoff
#   TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh verify
#   TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh cycle [shell|PACKAGE ...]
#
# Before the first UI logout/login, install the *developer session* entry in the
# disposable VM and select “Tilo (my build)” once in its greeter.  `deploy`
# writes ~/.config/tilo/dev-session, so subsequent UI logins pick up the new
# marked debug shell without any RPM install.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
container=${TILO_FEDORA44_CONTAINER:-tilo-dev-fedora44}
image=${TILO_FEDORA44_IMAGE:-localhost/tilo-dev-fedora44:current}
target_rel=${TILO_FEDORA44_TARGET_REL:-target/fedora44-dev}
vm_host=${TILO_VM_HOST:-127.0.0.1}
vm_user=${TILO_VM_USER:-quickuser}
vm_root=${TILO_VM_DEV_ROOT:-.local/share/tilo-dev}

# Some build scripts embed CARGO_MANIFEST_DIR. The vendored Vello shader
# builder does this, and its source path differs between the host and the
# /run/host checkout view inside distrobox. Keep this persistent cache scoped
# to one checkout instead of reusing either generic sccache namespace.
repo_cache_id=$(printf '%s' "$repo_root" | cksum | awk '{print $1}')
sccache_port=${TILO_FEDORA44_SCCACHE_PORT:-$((43000 + repo_cache_id % 2000))}
sccache_cache_name="tilo-fedora44-$repo_cache_id"

die() { printf 'fedora44-vm-iterate: %s\n' "$*" >&2; exit 2; }
note() { printf 'fedora44-vm-iterate: %s\n' "$*" >&2; }

require_command() { command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"; }

safe_name() {
    [[ $1 =~ ^[A-Za-z0-9._-]+$ ]] || die "unsafe package name: $1"
}

safe_vm_root() {
    [[ $vm_root =~ ^[A-Za-z0-9._/-]+$ && $vm_root != /* && $vm_root != *'..'* ]] \
        || die "TILO_VM_DEV_ROOT must be a relative, non-traversing path"
}

safe_target_rel() {
    [[ $target_rel =~ ^target/[A-Za-z0-9._/-]+$ && $target_rel != *'..'* ]] \
        || die "TILO_FEDORA44_TARGET_REL must be a safe path below target/"
}

safe_vm_user() {
    [[ $vm_user =~ ^[a-z_][a-z0-9_-]*\$?$ ]] || die "TILO_VM_USER is not a valid user name"
}

safe_sccache_port() {
    [[ $sccache_port =~ ^[0-9]{1,5}$ ]] && (( sccache_port >= 1024 && sccache_port <= 65535 )) \
        || die "TILO_FEDORA44_SCCACHE_PORT must be a TCP port from 1024 through 65535"
}

container_exists() { podman container exists "$container" 2>/dev/null; }

container_run() {
    # distrobox preserves PWD but changes the visible host prefix to /run/host.
    # The target is deliberately relative to that preserved checkout, so each
    # invocation reaches the same mounted directory without assuming a host
    # mount layout.
    cd "$repo_root"
    distrobox enter "$container" -- bash -lc "$1"
}

cmd_bootstrap() {
    require_command podman
    require_command distrobox
    if ! podman image exists "$image"; then
        note "preparing persistent Fedora 44 dependency image $image (one-time)"
        # The Containerfile needs exactly one small helper.  Do not hand podman
        # the repository root as its build context: even ignored target trees
        # make context walking expensive and turn this one-time step into a
        # multi-gigabyte copy.
        local build_context
        build_context=$(mktemp -d "${TMPDIR:-/tmp}/tilo-fedora44-context.XXXXXX")
        trap 'rm -rf "$build_context"' RETURN
        install -m 0755 "$repo_root/dist/ci/install-build-deps.sh" "$build_context/install-build-deps.sh"
        podman build --tag "$image" --file "$repo_root/dist/dev/Containerfile.fedora44" "$build_context"
    fi
    if ! container_exists; then
        note "creating persistent distrobox $container"
        distrobox create --name "$container" --image "$image" --yes
    fi
    container_run 'set -eu
        . /etc/os-release
        test "$VERSION_ID" = 44
        command -v cargo
        command -v sccache
        rpm -q libxkbcommon-devel capnproto libavcodec-free-devel \
            libswscale-free-devel >/dev/null
        # A persistent distrobox does not change when its source image tag is
        # rebuilt.  Refresh the live development headers too; otherwise Cargo
        # can retain generated libspa wrappers from an older Fedora 44 base
        # PipeWire headers even after the prepared image has been updated.
        sudo dnf upgrade -y --refresh pipewire-devel pipewire-libs >/dev/null
        grep -Fq spa_buffer_has_meta_features \
            /usr/include/spa-0.2/spa/buffer/buffer.h
        printf "Fedora %s development container ready\\n" "$VERSION_ID"'
    note "ready; target cache is $repo_root/$target_rel"
}

build_one() {
    local package=$1 args=()
    case "$package" in
        shell) args=(-p shell --bin shell --features dev-session) ;;
        tilo-launcher) args=(-p launcher --bin tilo-launcher) ;;
        tilo-unlock-helper) args=(-p tilo-unlock-helper --bin tilo-unlock-helper) ;;
        *) safe_name "$package"; args=(-p "$package") ;;
    esac
    # The lock prevents two commands from corrupting the one persistent Cargo
    # target.  Debug is Cargo's default profile: no release LTO is involved.
    local quoted
    printf -v quoted ' %q' "${args[@]}"
    container_run "set -euo pipefail
        # Do not reuse the generic host or distrobox sccache namespace: build
        # scripts can bake CARGO_MANIFEST_DIR into their executable, so caching
        # across host and /run/host paths can corrupt a later host build.
        export SCCACHE_SERVER_PORT='$sccache_port'
        export SCCACHE_DIR=\"\$HOME/.cache/$sccache_cache_name\"
        mkdir -p '$target_rel'
        # This is a persistent, disposable build cache rather than source. The
        # standard tag keeps backup/indexing tools from treating it as project
        # content when a developer snapshots the checkout.
        if [ ! -e '$target_rel/CACHEDIR.TAG' ]; then
          printf '%s\\n' 'Signature: 8a477f597d28d172789f06886806bc55' \\
            '# This file is a cache directory tag created by Tilo.' \\
            '# For information about cache directory tags, see:' \\
            '#       http://www.brynosaurus.com/cachedir/' > '$target_rel/CACHEDIR.TAG'
        fi
        exec flock '$target_rel/.build.lock' env \\
          CARGO_TARGET_DIR='$target_rel' \\
          RUSTC_WRAPPER=sccache \\
          cargo build --locked$quoted"
}

cmd_build() {
    (($#)) || set -- shell
    safe_target_rel
    safe_sccache_port
    container_exists || die "container $container is absent; run: $0 bootstrap"
    local package
    for package in "$@"; do
        if [[ $package == host ]]; then
            build_one tilo-launcher
            build_one tilo-unlock-helper
        else
            note "debug-building $package only"
            build_one "$package"
        fi
    done
}

vm_port() {
    [[ ${TILO_VM_SSH_PORT:-} =~ ^[0-9]{1,5}$ ]] || die "set TILO_VM_SSH_PORT to the disposable VM's SSH forward"
    printf '%s\n' "$TILO_VM_SSH_PORT"
}

ssh_base() {
    local port
    port=$(vm_port)
    local opts=(-p "$port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=8)
    if [[ -n ${TILO_VM_SSH_IDENTITY:-} ]]; then
        [[ -f $TILO_VM_SSH_IDENTITY ]] || die "TILO_VM_SSH_IDENTITY does not name a private key: $TILO_VM_SSH_IDENTITY"
        opts+=(-i "$TILO_VM_SSH_IDENTITY" -o IdentitiesOnly=yes)
    fi
    if [[ -n ${TILO_VM_PASSWORD:-} ]]; then
        require_command sshpass
        sshpass -p "$TILO_VM_PASSWORD" ssh "${opts[@]}" "$vm_user@$vm_host" "$@"
    else
        ssh "${opts[@]}" "$vm_user@$vm_host" "$@"
    fi
}

assert_disposable_guest() {
    safe_vm_root
    safe_vm_user
    # This is deliberately a remote assertion.  No local loginctl/systemctl is
    # ever called by this tool, which makes an accidental host-session restart
    # impossible even when the caller has a shell on the physical machine.
    ssh_base "set -eu
        systemd-detect-virt --quiet
        test \"\$(id -un)\" = '$vm_user'
        umask 077
        install -d -m 700 \"\$HOME/$vm_root/incoming\"
        printf 'disposable guest accepted\\n'"
}

remote_install() {
    local local_path=$1 remote_name=$2
    [[ -x $local_path ]] || die "built executable not found: $local_path"
    safe_name "$remote_name"
    assert_disposable_guest
    # Write a private temporary file then rename atomically.  A running shell
    # keeps its old inode; the next developer-session start gets the new one.
    ssh_base \
        "set -eu; umask 077; cat > \"\$HOME/$vm_root/incoming/$remote_name.\$\$\"; chmod 700 \"\$HOME/$vm_root/incoming/$remote_name.\$\$\"; mv -f \"\$HOME/$vm_root/incoming/$remote_name.\$\$\" \"\$HOME/$vm_root/$remote_name\"" \
        <"$local_path"
}

deploy_one() {
    local package=$1 binary=$package
    [[ $package == shell ]] && binary=shell
    safe_name "$binary"
    remote_install "$repo_root/$target_rel/debug/$binary" "$binary"
    if [[ $package == shell ]]; then
        # The packaged developer wrapper intentionally accepts only an absolute,
        # private path carrying this marker.  Set up precisely that contract.
        ssh_base "set -eu
            shell_path=\"\$HOME/$vm_root/shell\"
            grep -aFq 'tilo-dev-session-build-marker-v1' \"\$shell_path\"
            install -d -m 700 \"\$HOME/.config/tilo\"
            printf '%s\\n' \"\$shell_path\" > \"\$HOME/.config/tilo/dev-session\"
            chmod 600 \"\$HOME/.config/tilo/dev-session\""
    fi
}

cmd_deploy() {
    (($#)) || set -- shell
    local package
    for package in "$@"; do
        [[ $package != host ]] || die "deploy host as its concrete binaries: tilo-launcher tilo-unlock-helper"
        note "deploying $package into the disposable VM"
        deploy_one "$package"
    done
}

cmd_handoff() {
    # Do not terminate a graphical session remotely. The desktop wrapper has
    # cleanup behaviour that must run through a real UI logout, and killing it
    # from SSH would make a broken cleanup path look healthy.
    note 'new VM payload is staged.'
    note 'In the disposable VM UI: log out normally, select “Tilo (my build)”, then log in.'
    note 'The VM stays running; this tool never sends loginctl/systemctl/reboot commands.'
}

cmd_verify() {
    assert_disposable_guest
    # This is proof after the user has completed the normal UI logout/login.
    # It is read-only: identify the active compositor by executable inode, not
    # by a log line or the configuration that merely says what should start.
    ssh_base "set -euo pipefail
        wanted=\$(readlink -e \"\$HOME/$vm_root/shell\")
        test -n \"\$wanted\"
        grep -aFq 'tilo-dev-session-build-marker-v1' \"\$wanted\"
        found=
        for pid in \$(pgrep -u \"\$(id -u)\" -x shell || true); do
            actual=\$(readlink -e \"/proc/\$pid/exe\" || true)
            if test \"\$actual\" = \"\$wanted\"; then
                printf 'VM developer shell verified: pid=%s exe=%s\\n' \"\$pid\" \"\$actual\"
                found=1
            fi
        done
        test -n \"\$found\" || { echo 'no running shell has the deployed developer executable' >&2; exit 1; }"
}

cmd_cycle() {
    cmd_build "$@"
    cmd_deploy "$@"
    cmd_handoff
}

usage() {
    cat <<'EOF'
Usage:
  dist/dev/fedora44-vm-iterate.sh bootstrap
  dist/dev/fedora44-vm-iterate.sh build [shell|host|PACKAGE ...]
  TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh deploy [shell|PACKAGE ...]
  dist/dev/fedora44-vm-iterate.sh handoff
  TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh verify
  TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh cycle [shell|PACKAGE ...]

`bootstrap` creates a persistent Fedora 44 dependency container.  `cycle`
uses debug builds only, deploys executable files into a disposable VM, then
hands off to a normal UI logout/login so wrapper cleanup is exercised.

The Fedora 44 lane uses its own container-local sccache server and cache
directory. This is required: a copied host server could compile SPA/PipeWire C
shims in the host's header namespace rather than Fedora 44's.
EOF
}

case "${1:-}" in
    bootstrap) shift; (($# == 0)) || die 'bootstrap takes no arguments'; cmd_bootstrap ;;
    build) shift; cmd_build "$@" ;;
    deploy) shift; cmd_deploy "$@" ;;
    handoff) shift; (($# == 0)) || die 'handoff takes no arguments'; cmd_handoff ;;
    verify) shift; (($# == 0)) || die 'verify takes no arguments'; cmd_verify ;;
    restart) die 'automatic session termination is intentionally disabled; run handoff and log out in the VM UI' ;;
    cycle) shift; cmd_cycle "$@" ;;
    -h|--help|help) usage ;;
    *) usage >&2; exit 2 ;;
esac
