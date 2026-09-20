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
#   dist/dev/fedora44-vm-iterate.sh test PACKAGE [CARGO_TEST_FILTER]
#   TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh deploy [shell|PACKAGE ...]
#   TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh deploy-apps RELEASE_STAGE
#   dist/dev/fedora44-vm-iterate.sh handoff
#   TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh verify
#   TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh diagnose
#   TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh control JSON
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

ensure_target_visible_in_container() {
    local target=$repo_root/$target_rel
    if [[ -L $target ]]; then
        local resolved
        resolved=$(readlink -f -- "$target") || die "cannot resolve target cache symlink: $target"
        case "$resolved" in
            /var/tmp/*|/var/home/*|/home/*) ;;
            *) die "target cache symlink resolves outside host-mounted development paths: $resolved" ;;
        esac
        # Absolute host paths live below /run/host inside distrobox. Cargo is
        # given that visible path directly; otherwise mkdir follows a host
        # symlink to /var/tmp *inside the container* and reports the existing
        # cache as an uncreatable directory.
        target_rel=/run/host$resolved
    fi
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
        command -v mold
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
        # Match the shipped host-session feature set. The default HDR decoder
        # pulls Fedora multimedia DSOs which are deliberately not part of the
        # host-session runtime contract.
        shell) args=(-p shell --bin shell --no-default-features --features dev-session) ;;
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
        # Generated libspa C wrappers require the compiler's own preprocessor;
        # sccache direct mode can omit PipeWire inline declarations on Fedora.
        export SCCACHE_DIRECT=false
        export QCARGO_BYPASS=1
        # Keep sccache on rustc, but make cc-rs invoke the real compiler. C
        # helper output depends on live Fedora headers (notably PipeWire SPA),
        # and sccache can otherwise return an object generated for older ones.
        export CC=/usr/bin/gcc CXX=/usr/bin/g++
        mkdir -p '$target_rel'
        mkdir -p '$target_rel/tmp'
        export TMPDIR='$target_rel/tmp'
        # This is a persistent, disposable build cache rather than source. The
        # standard tag keeps backup/indexing tools from treating it as project
        # content when a developer snapshots the checkout.
        if [ ! -e '$target_rel/CACHEDIR.TAG' ]; then
          printf '%s\\n' 'Signature: 8a477f597d28d172789f06886806bc55' \\
            '# This file is a cache directory tag created by Tilo.' \\
            '# For information about cache directory tags, see:' \\
            '#       http://www.brynosaurus.com/cachedir/' > '$target_rel/CACHEDIR.TAG'
        fi
        # flock stays behind as the lock owner while Cargo runs. Its --close
        # mode prevents Cargo, rustc, and sccache's long-lived server from
        # inheriting the lock descriptor and wedging every later build.
        exec flock --close '$target_rel/.build.lock' env \\
          CARGO_TARGET_DIR='$target_rel' \\
          RUSTC_WRAPPER= \\
          RUSTC_WORKSPACE_WRAPPER=sccache \\
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS='-C target-cpu=x86-64-v3 -C force-frame-pointers=yes -C relro-level=full -C link-arg=-Wl,-z,noexecstack -C link-arg=-Wl,-z,shstk -C link-arg=-fuse-ld=mold --remap-path-prefix=/build=.' \\
          cargo build --locked$quoted"
}

build_shell_pair() {
    note "debug-building shell and its matched launcher in one cached Cargo invocation"
    container_run "set -euo pipefail
        export SCCACHE_SERVER_PORT='$sccache_port'
        export SCCACHE_DIR=\"\$HOME/.cache/$sccache_cache_name\"
        export SCCACHE_DIRECT=false
        export QCARGO_BYPASS=1
        export CC=/usr/bin/gcc CXX=/usr/bin/g++
        mkdir -p '$target_rel'
        mkdir -p '$target_rel/tmp'
        export TMPDIR='$target_rel/tmp'
        if [ ! -e '$target_rel/CACHEDIR.TAG' ]; then
          printf '%s\\n' 'Signature: 8a477f597d28d172789f06886806bc55' \\
            '# This file is a cache directory tag created by Tilo.' \\
            '# For information about cache directory tags, see:' \\
            '#       http://www.brynosaurus.com/cachedir/' > '$target_rel/CACHEDIR.TAG'
        fi
        exec flock --close '$target_rel/.build.lock' env \\
          CARGO_TARGET_DIR='$target_rel' \\
          RUSTC_WRAPPER= \\
          RUSTC_WORKSPACE_WRAPPER=sccache \\
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS='-C target-cpu=x86-64-v3 -C force-frame-pointers=yes -C relro-level=full -C link-arg=-Wl,-z,noexecstack -C link-arg=-Wl,-z,shstk -C link-arg=-fuse-ld=mold --remap-path-prefix=/build=.' \\
          cargo build --locked -p shell --bin shell --no-default-features --features dev-session \\
            -p launcher --bin tilo-launcher"
}

cmd_build() {
    (($#)) || set -- shell
    safe_target_rel
    ensure_target_visible_in_container
    safe_sccache_port
    container_exists || die "container $container is absent; run: $0 bootstrap"
    local package
    for package in "$@"; do
        if [[ $package == host ]]; then
            build_one tilo-launcher
            build_one tilo-unlock-helper
        elif [[ $package == shell ]]; then
            build_shell_pair
        else
            note "debug-building $package only"
            build_one "$package"
        fi
    done
}

cmd_test() {
    (($# >= 1 && $# <= 2)) || die 'test requires PACKAGE and optionally one Cargo test filter'
    local package=$1 filter=${2:-}
    safe_name "$package"
    safe_target_rel
    ensure_target_visible_in_container
    safe_sccache_port
    container_exists || die "container $container is absent; run: $0 bootstrap"
    local filter_arg=
    if [[ -n $filter ]]; then
        [[ $filter =~ ^[A-Za-z0-9_:.-]+$ ]] || die 'unsafe Cargo test filter'
        filter_arg=" '$filter'"
    fi
    note "focused-testing $package${filter:+ ($filter)}"
    container_run "set -euo pipefail
        export PATH='/usr/lib64/sccache:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin'
        export SCCACHE_DIR=\"\${XDG_CACHE_HOME:-\$HOME/.cache}/$sccache_cache_name\"
        export SCCACHE_SERVER_PORT='$sccache_port'
        export SCCACHE_NO_DAEMON=1
        command -v mold >/dev/null
        command -v sccache >/dev/null
        mkdir -p '$target_rel'
        exec flock --close '$target_rel/.build.lock' env \\
          CARGO_TARGET_DIR='$target_rel' \\
          RUSTC_WRAPPER= \\
          RUSTC_WORKSPACE_WRAPPER=sccache \\
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS='-C target-cpu=x86-64-v3 -C force-frame-pointers=yes -C relro-level=full -C link-arg=-Wl,-z,noexecstack -C link-arg=-Wl,-z,shstk -C link-arg=-fuse-ld=mold --remap-path-prefix=/build=.' \\
          cargo test --locked -p '$package'$filter_arg -- --test-threads 1"
}

vm_port() {
    [[ ${TILO_VM_SSH_PORT:-} =~ ^[0-9]{1,5}$ ]] || die "set TILO_VM_SSH_PORT to the disposable VM's SSH forward"
    printf '%s\n' "$TILO_VM_SSH_PORT"
}

ssh_base() {
    local port
    port=$(vm_port)
    # Never let an unattended VM probe fall through to a graphical askpass
    # helper on the developer's physical desktop.  A missing credential is a
    # fast, explicit failure; password-based fixtures must opt in through
    # TILO_VM_PASSWORD and are driven by sshpass below.
    local opts=(-F /dev/null -p "$port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=8 -o KbdInteractiveAuthentication=no)
    if [[ -n ${TILO_VM_SSH_IDENTITY:-} ]]; then
        [[ -f $TILO_VM_SSH_IDENTITY ]] || die "TILO_VM_SSH_IDENTITY does not name a private key: $TILO_VM_SSH_IDENTITY"
        opts+=(-i "$TILO_VM_SSH_IDENTITY" -o IdentitiesOnly=yes)
    fi
    if [[ -n ${TILO_VM_PASSWORD:-} ]]; then
        require_command sshpass
        env -u SSH_ASKPASS -u SSH_ASKPASS_REQUIRE sshpass -p "$TILO_VM_PASSWORD" ssh "${opts[@]}" -o BatchMode=no -o NumberOfPasswordPrompts=1 -o PreferredAuthentications=password -o PubkeyAuthentication=no "$vm_user@$vm_host" "$@"
    else
        env -u SSH_ASKPASS -u SSH_ASKPASS_REQUIRE ssh "${opts[@]}" -o BatchMode=yes -o NumberOfPasswordPrompts=0 "$vm_user@$vm_host" "$@"
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
        # An unattended graphical VM otherwise follows the desktop's normal
        # idle policy and suspends QEMU in the middle of a long first-overlay
        # build. Keep only this disposable test guest awake; the physical host
        # and production power policy are untouched.  logind refuses an
        # unprivileged inhibitor in a non-interactive SSH session, so use this
        # fixture account's non-interactive sudo rather than triggering polkit.
        install -d -m 700 \"\$HOME/.config/systemd/user\"
        keep_awake=\"\$HOME/.config/systemd/user/tilo-dev-keep-awake.service\"
        printf '%s\\n' \
          '[Unit]' \
          'Description=Keep disposable Tilo development VM awake' \
          '[Service]' \
          'Type=simple' \
          'ExecStart=/usr/bin/sudo -n /usr/bin/systemd-inhibit --what=sleep:idle --who=Tilo-VM-test --why=Unattended-development-acceptance --mode=block /usr/bin/sleep infinity' \
          '[Install]' \
          'WantedBy=default.target' \
          > \"\$keep_awake\"
        systemctl --user daemon-reload
        systemctl --user reset-failed tilo-dev-keep-awake.service
        systemctl --user enable tilo-dev-keep-awake.service >/dev/null
        if ! systemctl --user is-active --quiet tilo-dev-keep-awake.service; then
          systemctl --user start tilo-dev-keep-awake.service
        fi
        systemctl --user is-active --quiet tilo-dev-keep-awake.service
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
    local expected actual
    expected=$(sha256sum -- "$local_path" | cut -d' ' -f1)
    actual=$(ssh_base "sha256sum -- \"\$HOME/$vm_root/$remote_name\" | cut -d' ' -f1")
    [[ $actual == "$expected" ]] \
        || die "deployed $remote_name digest mismatch: expected $expected, got $actual"
}

dev_transfer_image() {
    local source=$1 output=$2
    [[ -x $source ]] || die "built executable not found: $source"
    require_command strip
    # Keep the persistent Cargo artifact intact for incremental linking. The
    # VM only needs an executable image; local DWARF/symbol tables account for
    # hundreds of megabytes and make every otherwise-fast iteration wait on
    # SSH. Crash diagnostics still come from structured tracing/flight logs.
    cp --reflink=auto -- "$source" "$output"
    strip --strip-all -- "$output"
    chmod 700 -- "$output"
}

assert_vm_runtime_compatible() {
    local local_path=$1 label=$2
    [[ -x $local_path ]] || die "built executable not found: $local_path"
    safe_name "$label"
    require_command readelf
    assert_disposable_guest
    # Send only the ELF NEEDED names (a few hundred bytes), then resolve each
    # with the target guest's own loader cache. This catches ABI drift before a
    # costly graphical login without transferring the large debug binary twice.
    local needed
    needed=$(readelf -d "$local_path" \
        | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p')
    [[ -n $needed ]] || die "cannot read dynamic dependencies from $local_path"
    ssh_base "set -e
        missing=\$(while IFS= read -r soname; do
            # Avoid grep -q here: with pipefail it exits early and makes
            # ldconfig's expected SIGPIPE look like a missing library.
            ldconfig -p | grep -F \"\$soname \" >/dev/null \\
                || printf '%s\\n' \"\$soname\"
        done)
        if [ -n \"\$missing\" ]; then
            printf 'candidate %s needs unavailable target libraries:\\n%s\\n' '$label' \"\$missing\" >&2
            exit 1
        fi" <<<"$needed"
}

deploy_one() {
    local package=$1 binary=$package
    [[ $package == shell ]] && binary=shell
    [[ $package == ush ]] && binary=tilo-ush
    safe_name "$binary"
    local source="$repo_root/$target_rel/debug/$binary"
    local transfer
    transfer=$(mktemp "${TMPDIR:-/tmp}/tilo-$binary.XXXXXX")
    dev_transfer_image "$source" "$transfer"
    assert_vm_runtime_compatible "$transfer" "$binary"
    remote_install "$transfer" "$binary"
    if [[ $package == ush ]]; then
        local unit=$repo_root/os/usr/lib/systemd/user/tilo-ush.service
        [[ -f $unit ]] || die "ush user unit is missing: $unit"
        ssh_base "set -eu
            incoming=\"\$HOME/$vm_root/incoming/tilo-ush.service.\$\$\"
            cat > \"\$incoming\"
            sudo install -o root -g root -m 0755 \
              \"\$HOME/$vm_root/tilo-ush\" /usr/libexec/tilo-ush
            sudo install -o root -g root -m 0644 \"\$incoming\" \
              /usr/lib/systemd/user/tilo-ush.service
            rm -f -- \"\$incoming\"
            systemctl --user daemon-reload
            # Do not restart a required app service underneath an active
            # compositor: tilo-launcher deliberately keeps connected service
            # capabilities, and replacing their server invalidates every
            # later app launch in that login. The new executable/unit are used
            # by the next normal developer-session login.
            if ! systemctl --user is-active --quiet tilo-graphical-session.target; then
              systemctl --user restart tilo-ush.service
              test \"\$(systemctl --user show -p ActiveState --value tilo-ush.service)\" = active
            fi" <"$unit"
    fi
    if [[ $package == terminal ]]; then
        # A Tilo app is launched from its package payload, not PATH. Build a
        # private developer-system-store overlay once and replace only this
        # entrypoint there. The package-owned dev wrapper validates and exports
        # these roots on every login, so an ordinary logout/login cannot
        # silently restore the old /usr payload under the next test run.
        ssh_base "set -eu
            root=\"\$HOME/$vm_root/apps-single\"
            payload=\"\$root/usr/lib/tilo-apps/payloads/dev.tilo.terminal/bin/terminal\"
            if ! test -f \"\$root/usr/share/tilo-apps/registry.toml\"; then
              install -d -m 700 \"\$root/usr/share\" \"\$root/usr/lib\"
              cp -a /usr/share/tilo-apps \"\$root/usr/share/tilo-apps\"
              cp -a /usr/lib/tilo-apps \"\$root/usr/lib/tilo-apps\"
              cp -a /usr/lib/tilo-runtime \"\$root/usr/lib/tilo-runtime\"
              publisher=\$(sed -n 's/^publisher_hex = \"\([0-9a-fA-F]\{64\}\)\"$/\1/p' \
                \"\$root/usr/lib/tilo-runtime/2026.07/bundle.toml\")
              test \"\${#publisher}\" -eq 64
              printf '%s\\n' \"\$publisher\" > \
                \"\$root/usr/lib/tilo-runtime/trusted-publishers.txt\"
            fi
            test -x \"\$payload\"
            install -m 0755 \"\$HOME/$vm_root/terminal\" \"\$payload.new\"
            mv -f -- \"\$payload.new\" \"\$payload\"
            find \"\$root\" -type d -exec chmod go-w {} +
            find \"\$root\" -type f -exec chmod go-w {} +
            source_sha=\$(sha256sum -- \"\$HOME/$vm_root/terminal\" | cut -d' ' -f1)
            payload_sha=\$(sha256sum -- \"\$payload\" | cut -d' ' -f1)
            test \"\$payload_sha\" = \"\$source_sha\" || {
              printf 'terminal overlay digest mismatch: source=%s payload=%s\\n' \
                \"\$source_sha\" \"\$payload_sha\" >&2
              exit 1
            }
            install -d -m 700 \"\$HOME/.config/tilo\"
            printf '%s\\n%s\\n%s\\n' \
              \"\$root/usr/share/tilo-apps\" \
              \"\$root/usr/lib/tilo-apps\" \
              \"\$root/usr/lib/tilo-runtime\" \
              > \"\$HOME/.config/tilo/dev-app-environment\"
            chmod 600 \"\$HOME/.config/tilo/dev-app-environment\"
            printf '%s\\n' \"\$payload_sha\" \
              > \"\$HOME/$vm_root/terminal-payload.sha256\"
            chmod 600 \"\$HOME/$vm_root/terminal-payload.sha256\""
    fi
    if [[ $package == tilo-unlock-helper ]]; then
        local pam_stack=$repo_root/os/etc/pam.d/tilo-unlock
        [[ -f $pam_stack ]] || die "unlock PAM stack is missing: $pam_stack"
        # The compositor deliberately invokes the same fixed path as a
        # packaged session.  Install it as a system file: copying it only into
        # the developer's private directory gives PAM the wrong ownership and
        # bypasses Fedora's normal file labelling on an enforcing guest.
        ssh_base "set -eu
            incoming=\"\$HOME/$vm_root/incoming/tilo-unlock.pam.\$\$\"
            umask 077
            cat > \"\$incoming\"
            sudo install -o root -g root -m 0755 \
              \"\$HOME/$vm_root/tilo-unlock-helper\" \
              /usr/libexec/tilo-unlock-helper
            sudo install -o root -g root -m 0644 \"\$incoming\" \
              /etc/pam.d/tilo-unlock
            rm -f -- \"\$incoming\"
            if command -v restorecon >/dev/null 2>&1; then
              sudo restorecon -F /usr/libexec/tilo-unlock-helper \
                /etc/pam.d/tilo-unlock
            fi
            test \"\$(stat -c '%U:%G:%a:%F' \
              /usr/libexec/tilo-unlock-helper)\" = \
              'root:root:755:regular file'
            test \"\$(stat -c '%U:%G:%a:%F' /etc/pam.d/tilo-unlock)\" = \
              'root:root:644:regular file'
            test -u /usr/sbin/unix_chkpwd
            test \"\$(stat -c '%U:%G' /usr/sbin/unix_chkpwd)\" = root:root
            if command -v selinuxenabled >/dev/null 2>&1 && selinuxenabled; then
              helper_type=\$(stat -c '%C' /usr/libexec/tilo-unlock-helper \
                | sed -n 's/^[^:]*:[^:]*:\([^:]*\):.*$/\1/p')
              pam_type=\$(stat -c '%C' /etc/pam.d/tilo-unlock \
                | sed -n 's/^[^:]*:[^:]*:\([^:]*\):.*$/\1/p')
              test \"\$helper_type\" = bin_t
              test \"\$pam_type\" = etc_t
            fi" <"$pam_stack"
    fi
    if [[ $package == shell ]]; then
        local launcher_transfer
        launcher_transfer=$(mktemp "${TMPDIR:-/tmp}/tilo-launcher.XXXXXX")
        dev_transfer_image "$repo_root/$target_rel/debug/tilo-launcher" "$launcher_transfer"
        assert_vm_runtime_compatible "$launcher_transfer" tilo-launcher
        remote_install "$launcher_transfer" tilo-launcher
        rm -f -- "$launcher_transfer"
        # The packaged developer wrapper intentionally accepts only an absolute,
        # private path carrying this marker.  Set up precisely that contract.
        wrapper=$repo_root/dist/session/tilo-session-dev
        runner=$repo_root/dist/session/tilo-session-runner
        recorder=$repo_root/dist/session/tilo-flight-recorder.py
        [[ -x $wrapper ]] || die "developer session wrapper is missing: $wrapper"
        [[ -x $runner ]] || die "session lifecycle runner is missing: $runner"
        [[ -x $recorder ]] || die "flight recorder is missing: $recorder"
        ssh_base "set -eu
            shell_path=\"\$HOME/$vm_root/shell\"
            grep -aFq 'tilo-dev-session-build-marker-v1' \"\$shell_path\"
            install -d -m 700 \"\$HOME/.config/tilo\"
            printf '%s\\n' \"\$shell_path\" > \"\$HOME/.config/tilo/dev-session\"
            sha256sum \"\$shell_path\" | cut -d' ' -f1 > \"\$HOME/.config/tilo/dev-session.sha256\"
            chmod 600 \"\$HOME/.config/tilo/dev-session\"
            chmod 600 \"\$HOME/.config/tilo/dev-session.sha256\"
            wrapper=\"\$HOME/$vm_root/tilo-session-dev\"
            incoming=\"\$HOME/$vm_root/incoming/tilo-session-dev.\$\$\"
            cat > \"\$incoming\"
            chmod 700 \"\$incoming\"
            mv -f \"\$incoming\" \"\$wrapper\"
            sudo install -m 0755 \"\$wrapper\" /usr/libexec/tilo-session-dev" <"$wrapper"
        remote_install "$runner" tilo-session-runner
        ssh_base "sudo install -m 0755 \"\$HOME/$vm_root/tilo-session-runner\" /usr/libexec/tilo-session-runner"
        remote_install "$recorder" tilo-flight-recorder
        ssh_base "sudo install -m 0755 \"\$HOME/$vm_root/tilo-flight-recorder\" /usr/libexec/tilo-flight-recorder"
    fi
    rm -f -- "$transfer"
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

cmd_deploy_apps() {
    (($# == 1)) || die 'deploy-apps takes one staged app-store directory'
    local stage=$1
    [[ -f $stage/usr/share/tilo-apps/registry.toml ]] || die "missing staged registry: $stage"
    [[ -d $stage/usr/lib/tilo-apps && -d $stage/usr/lib/tilo-runtime ]] \
        || die "missing staged payload/runtime: $stage"
    assert_disposable_guest
    local archive
    archive=$(mktemp "${TMPDIR:-/tmp}/tilo-vm-apps.XXXXXX.tar")
    trap 'rm -f "$archive"' RETURN
    tar -C "$stage" -cf "$archive" usr/share/tilo-apps usr/lib/tilo-apps usr/lib/tilo-runtime
    ssh_base "set -euo pipefail
        root=\"\$HOME/$vm_root/apps\"
        incoming=\"\$HOME/$vm_root/apps.incoming.\$\$\"
        old=\"\$HOME/$vm_root/apps.old.\$\$\"
        umask 077
        mkdir -p \"\$incoming\"
        tar -xf - -C \"\$incoming\"
        if [ -e \"\$root\" ]; then mv \"\$root\" \"\$old\"; fi
        mv \"\$incoming\" \"\$root\"
        rm -rf -- \"\$old\"
        cp \"\$HOME/$vm_root/apps/usr/lib/tilo-runtime/2026.07/bundle.toml\" /dev/null
        publisher=\$(sed -n 's/^publisher_hex = \"\\([0-9a-fA-F]\\{64\\}\\)\"$/\\1/p' \
          \"\$root/usr/lib/tilo-runtime/2026.07/bundle.toml\")
        test \"\${#publisher}\" -eq 64
        printf '%s\\n' \"\$publisher\" > \"\$root/usr/lib/tilo-runtime/trusted-publishers.txt\"
        chmod 600 \"\$root/usr/lib/tilo-runtime/trusted-publishers.txt\"
        install -d -m 700 \"\$HOME/.config/tilo\"
        printf '%s\\n%s\\n%s\\n' \\
          \"\$root/usr/share/tilo-apps\" \\
          \"\$root/usr/lib/tilo-apps\" \\
          \"\$root/usr/lib/tilo-runtime\" \\
          > \"\$HOME/.config/tilo/dev-app-environment\"
        chmod 600 \"\$HOME/.config/tilo/dev-app-environment\"" <"$archive"
    note 'developer app/runtime overlay staged; it becomes active after a normal VM UI logout/login'
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
    # The trusted developer wrapper records the canonical target and its
    # launch-time digest. This works under Fedora's /proc restrictions and also
    # detects when a newer binary was atomically deployed after login.
    ssh_base "set -euo pipefail
        wanted=\$(readlink -e \"\$HOME/$vm_root/shell\")
        test -n \"\$wanted\"
        grep -aFq 'tilo-dev-session-build-marker-v1' \"\$wanted\"
        attest=\"\${XDG_RUNTIME_DIR:-/run/user/\$(id -u)}/tilo-dev-session/active\"
        test -r \"\$attest\" || { echo 'developer session has no launch attestation' >&2; exit 1; }
        pid=\$(sed -n 's/^pid=//p' \"\$attest\")
        actual_path=\$(sed -n 's/^path=//p' \"\$attest\")
        actual_sha=\$(sed -n 's/^sha256=//p' \"\$attest\")
        wanted_sha=\$(sha256sum -- \"\$wanted\" | cut -d' ' -f1)
        test \"\$actual_path\" = \"\$wanted\" || { echo 'active developer session used a different path' >&2; exit 1; }
        test \"\$actual_sha\" = \"\$wanted_sha\" || { echo 'active developer session is an older deployed build' >&2; exit 1; }
        kill -0 \"\$pid\" 2>/dev/null || { echo 'attested developer shell is no longer running' >&2; exit 1; }
        printf 'VM developer shell verified: pid=%s exe=%s sha256=%s\\n' \"\$pid\" \"\$wanted\" \"\$actual_sha\""
}

cmd_diagnose() {
    assert_disposable_guest
    # Fixed, read-only post-mortem for an expensive graphical test run. Keep
    # this curated instead of exposing a generic remote shell: the output is
    # reproducible and the command cannot accidentally alter either session.
    ssh_base 'set +e
        runtime=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
        state=${XDG_STATE_HOME:-$HOME/.local/state}
        printf "tilo-target=%s\n" "$(systemctl --user is-active tilo-graphical-session.target 2>/dev/null)"
        printf "ush-service=%s\n" "$(systemctl --user is-active tilo-ush.service 2>/dev/null)"
        if test -r "$runtime/tilo-dev-session/active"; then
            echo "attestation=present"
            sed -n "1,12p" "$runtime/tilo-dev-session/active"
        else
            echo "attestation=absent"
        fi
        echo "-- developer app overlay --"
        app_env="$HOME/.config/tilo/dev-app-environment"
        if test -r "$app_env"; then
            sed -n "1,3p" "$app_env"
            payload_root=$(sed -n "2p" "$app_env")
            terminal="$payload_root/payloads/dev.tilo.terminal/bin/terminal"
            if test -x "$terminal"; then
                printf "terminal-payload-sha256="
                sha256sum -- "$terminal" | cut -d" " -f1
            else
                echo "terminal-payload=missing"
            fi
        else
            echo "developer-app-environment=absent"
        fi
        test -S "$runtime/tilo-dev-session/control.sock"
        printf "control-socket=%s\n" "$?"
        if test -S "$runtime/tilo-dev-session/control.sock"; then
            python3 - "$runtime/tilo-dev-session/control.sock" <<'"'"'PY'"'"'
import json
import socket
import sys

path = sys.argv[1]
for request in ({"op": "list_targets"}, {"op": "state_dump"}):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(2)
        client.connect(path)
        client.sendall(json.dumps(request).encode() + b"\n")
        response = b""
        while not response.endswith(b"\n"):
            chunk = client.recv(65536)
            if not chunk:
                break
            response += chunk
    decoded = json.loads(response)
    if request["op"] == "list_targets":
        targets = decoded.get("targets", decoded.get("Targets", []))
        print("-- relevant targets --")
        for target in targets:
            if "power" in target.get("name", "") or "topbar_tray" in target.get("name", ""):
                print(json.dumps(target, sort_keys=True))
    else:
        print("-- live state --")
        print(json.dumps(decoded, sort_keys=True)[:12000])
PY
        fi
        echo "-- session processes --"
        ps -eo pid,ppid,stat,etimes,comm,args \
          | grep -E "tilo-session|tilo-flight|target/debug/shell|/tilo-shell|systemd-cat" \
          | grep -v grep || true
        echo "-- flight recorder --"
        tail -n 80 "$state/tilo/session-logs/latest.log" 2>/dev/null || true
        echo "-- session journal --"
        journalctl --user -b --no-pager -n 120 \
          -u tilo-graphical-session.target -u tilo-ush.service 2>/dev/null || true'
}

cmd_control() {
    local wait_target=
    if [[ ${1:-} == --wait-target ]]; then
        (($# >= 3)) || die 'control --wait-target takes a name and one JSON object'
        wait_target=$2
        shift 2
    fi
    (($# == 1)) || die 'control takes one JSON object'
    assert_disposable_guest
    local client=$repo_root/dist/dev/tilo-vm-control.py
    [[ -f $client ]] || die "missing control client: $client"
    # The helper is streamed into Python; no executable or valuable source is
    # left in the guest. The request is one positional value and the client
    # only connects to this developer session's private control socket.
    local wait_arg=
    if [[ -n $wait_target ]]; then
        [[ $wait_target =~ ^[A-Za-z0-9_.:-]+$ ]] || die 'unsafe control target name'
        wait_arg="--wait-target '$wait_target'"
    fi
    ssh_base "python3 - \"\${XDG_RUNTIME_DIR:-/run/user/\$(id -u)}/tilo-dev-session/control.sock\" '$1' $wait_arg" <"$client"
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
  dist/dev/fedora44-vm-iterate.sh test PACKAGE [CARGO_TEST_FILTER]
  TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh deploy [shell|PACKAGE ...]
  TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh deploy-apps RELEASE_STAGE
  dist/dev/fedora44-vm-iterate.sh handoff
  TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh verify
  TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh diagnose
  TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh control JSON
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
    test) shift; cmd_test "$@" ;;
    deploy) shift; cmd_deploy "$@" ;;
    deploy-apps) shift; cmd_deploy_apps "$@" ;;
    handoff) shift; (($# == 0)) || die 'handoff takes no arguments'; cmd_handoff ;;
    verify) shift; (($# == 0)) || die 'verify takes no arguments'; cmd_verify ;;
    diagnose) shift; (($# == 0)) || die 'diagnose takes no arguments'; cmd_diagnose ;;
    control) shift; cmd_control "$@" ;;
    restart) die 'automatic session termination is intentionally disabled; run handoff and log out in the VM UI' ;;
    cycle) shift; cmd_cycle "$@" ;;
    -h|--help|help) usage ;;
    *) usage >&2; exit 2 ;;
esac
