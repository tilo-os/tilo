#!/usr/bin/env bash
# Contract checks for the fast Fedora 44 / disposable-VM loop.  The tests use
# fake transport/container commands: they prove command construction without
# creating a container, compiling Rust, or contacting a VM.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)
tool=$repo_root/dist/dev/fedora44-vm-iterate.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"

log=$work/log
cat >"$work/bin/podman" <<'EOF'
#!/usr/bin/env bash
test "$1" = container && test "$2" = exists
EOF
cat >"$work/bin/distrobox" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'distrobox %s\n' "$*" >>"$FAST_LOOP_LOG"
test "$1" = enter
shift 2; test "$1" = --; shift
exec "$@"
EOF
cat >"$work/bin/flock" <<'EOF'
#!/usr/bin/env bash
if [[ ${1:-} == --close ]]; then shift; fi
shift
exec "$@"
EOF
cat >"$work/bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf 'cargo %s\n' "$*" >>"$FAST_LOOP_LOG"
test "$1" = build
case " $* " in
  *' -p shell '* )
    [[ " $* " != *' --profile release '* ]] || exit 99
    [[ " $* " != *' --release '* ]] || exit 99
    mkdir -p "$CARGO_TARGET_DIR/debug"
    printf 'tilo-app-session-build-marker-v1\n' >"$CARGO_TARGET_DIR/debug/shell"
    printf 'recorder\n' >"$CARGO_TARGET_DIR/debug/tilo-flight-recorder"
    chmod 700 "$CARGO_TARGET_DIR/debug/shell" "$CARGO_TARGET_DIR/debug/tilo-flight-recorder"
    ;;
  *' -p launcher '* )
    mkdir -p "$CARGO_TARGET_DIR/debug"
    printf 'launcher\n' >"$CARGO_TARGET_DIR/debug/tilo-launcher"
    chmod 700 "$CARGO_TARGET_DIR/debug/tilo-launcher"
    ;;
esac
EOF
cat >"$work/bin/sudo" <<'EOF'
#!/usr/bin/env bash
printf 'sudo %s\n' "$*" >>"$FAST_LOOP_LOG"
exit 0
EOF
cat >"$work/bin/readelf" <<'EOF'
#!/usr/bin/env bash
printf ' 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]\n'
EOF
cat >"$work/bin/strip" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat >"$work/bin/ssh" <<'EOF'
#!/usr/bin/env bash
printf 'ssh %s\n' "$*" >>"$FAST_LOOP_LOG"
cat >>"$FAST_LOOP_LOG"
exit 0
EOF
chmod 755 "$work/bin"/*

target_rel="target/.fedora44-loop-test-$$"
env PATH="$work/bin:$PATH" FAST_LOOP_LOG="$log" \
    TILO_FEDORA44_CONTAINER=test-container TILO_FEDORA44_TARGET_REL="$target_rel" \
    "$tool" build shell

test -x "$repo_root/$target_rel/debug/shell"
test -f "$repo_root/$target_rel/CACHEDIR.TAG"
grep -Fqx 'Signature: 8a477f597d28d172789f06886806bc55' "$repo_root/$target_rel/CACHEDIR.TAG"
grep -Fq -- '--features dev-session' "$log"
grep -Fq -- '--bin shell' "$log"
grep -Fq -- '-p launcher --bin tilo-launcher' "$log"
grep -Fq -- '--no-default-features' "$log"
grep -Fq 'SCCACHE_SERVER_PORT=' "$log"
grep -Fq 'tilo-fedora44-' "$log"
! grep -Fq 'SCCACHE_SERVER_PORT=4226' "$log"
! grep -Fq 'sccache-distrobox' "$log"
! grep -Eq -- '--release|--profile release|--default-features' "$log"

: >"$log"
env PATH="$work/bin:$PATH" FAST_LOOP_LOG="$log" \
    TILO_FEDORA44_CONTAINER=test-container TILO_FEDORA44_TARGET_REL="$target_rel" \
    TILO_FEDORA44_SCCACHE_PORT=45555 "$tool" build shell
grep -Fq "SCCACHE_SERVER_PORT='45555'" "$log"

: >"$log"
env PATH="$work/bin:$PATH" FAST_LOOP_LOG="$log" \
    TILO_FEDORA44_CONTAINER=test-container TILO_FEDORA44_TARGET_REL="$target_rel" \
    "$tool" build host
grep -Fq -- '-p launcher --bin tilo-launcher' "$log"
grep -Fq -- '-p tilo-unlock-helper --bin tilo-unlock-helper' "$log"

: >"$log"
env PATH="$work/bin:$PATH" FAST_LOOP_LOG="$log" \
    TILO_FEDORA44_CONTAINER=test-container TILO_FEDORA44_TARGET_REL="$target_rel" \
    "$tool" handoff >"$work/handoff.out" 2>&1
grep -Fq 'log out normally' "$work/handoff.out"
! grep -q '^ssh ' "$log"
! grep -Eq 'loginctl|systemctl|reboot|poweroff|rpm-ostree' "$work/handoff.out"

: >"$work/test_vm_key"
chmod 600 "$work/test_vm_key"
: >"$log"
env PATH="$work/bin:$PATH" FAST_LOOP_LOG="$log" \
    TILO_FEDORA44_CONTAINER=test-container TILO_FEDORA44_TARGET_REL="$target_rel" \
    TILO_VM_SSH_PORT=29999 TILO_VM_SSH_IDENTITY="$work/test_vm_key" "$tool" deploy shell
grep -Fq -- "-i $work/test_vm_key" "$log"
grep -Fq 'Tilo *developer* session entry point.' "$log"
grep -Fq 'sudo install -m 0755 "$wrapper" /usr/libexec/tilo-session-dev' "$log"
grep -Fq 'tilo-session-runner' "$log"
grep -Fq '/usr/libexec/tilo-session-runner' "$log"
grep -Fq 'tilo-flight-recorder' "$log"
grep -Fq '/usr/libexec/tilo-flight-recorder' "$log"
grep -Fq 'ldconfig -p' "$log"

: >"$log"
env PATH="$work/bin:$PATH" FAST_LOOP_LOG="$log" \
    TILO_FEDORA44_CONTAINER=test-container TILO_FEDORA44_TARGET_REL="$target_rel" \
    TILO_VM_SSH_PORT=29999 "$tool" verify
grep -Fq 'tilo-app-session/active' "$log"
! grep -Eq 'loginctl|systemctl|terminate-session|reboot|poweroff' "$log"

rm -rf "$repo_root/$target_rel"
echo 'fedora44-vm-iterate: PASS'
