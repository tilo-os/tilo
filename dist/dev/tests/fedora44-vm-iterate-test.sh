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
export FAST_LOOP_DEPLOY_SHA_FILE=$work/deploy.sha256
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
if [[ $1 == test ]]; then exit 0; fi
test "$1" = build
case " $* " in
  *' -p shell '* )
    [[ " $* " != *' --profile release '* ]] || exit 99
    [[ " $* " != *' --release '* ]] || exit 99
    mkdir -p "$CARGO_TARGET_DIR/debug"
    printf 'tilo-dev-session-build-marker-v1\n' >"$CARGO_TARGET_DIR/debug/shell"
    chmod 700 "$CARGO_TARGET_DIR/debug/shell"
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
command=${!#}
if [[ $command == sha256sum* ]]; then
  name=$(printf '%s' "$command" | sed -n 's#.*\.local/share/tilo-dev/\([^"/]*\)".*#\1#p')
  cat "$FAST_LOOP_DEPLOY_SHA_FILE.$name"
else
  payload=$(cat)
  if [[ -n $payload && $command == *'/incoming/'* ]]; then
    printf '%s' "$payload" >>"$FAST_LOOP_LOG"
    name=$(printf '%s' "$command" | sed -n 's#.*incoming/\([^.$"/]*\)\.\$\$.*#\1#p')
    printf '%s' "$payload" | sha256sum | cut -d' ' -f1 >"$FAST_LOOP_DEPLOY_SHA_FILE.$name"
  fi
fi
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
grep -Fq 'cargo test --locked -p' "$tool"
grep -Fq -- '-fuse-ld=mold' "$tool"
grep -Fq 'focused-testing' "$tool"
python3 - "$repo_root/dist/dev/tilo-vm-control.py" <<'PY'
import ast
import pathlib
import sys

tree = ast.parse(pathlib.Path(sys.argv[1]).read_text())
main = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "main")
source = ast.unparse(main)
assert source.index("decoded = transact(request)") < source.index('transact({\'op\': \'list_targets\'})')
PY

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
grep -Fq -- '-F /dev/null' "$tool"
grep -Fq -- '-o NumberOfPasswordPrompts=0' "$tool"
grep -Fq -- '-o KbdInteractiveAuthentication=no' "$tool"
grep -Fq 'env -u SSH_ASKPASS -u SSH_ASKPASS_REQUIRE' "$tool"
grep -Fq 'sudo install -m 0755 \"\$wrapper\" /usr/libexec/tilo-session-dev' "$tool"
grep -Fq 'tilo-session-runner' "$tool"
grep -Fq '/usr/libexec/tilo-session-runner' "$tool"
grep -Fq 'tilo-flight-recorder' "$tool"
grep -Fq '/usr/libexec/tilo-flight-recorder' "$tool"
grep -Fq 'ldconfig -p' "$tool"

# Unlock-helper deployment is not an ordinary private dev-binary update. PAM
# and Fedora's unix_chkpwd boundary require the production system paths to be
# root-owned and correctly labelled.
mkdir -p "$repo_root/$target_rel/debug"
printf 'unlock helper\n' >"$repo_root/$target_rel/debug/tilo-unlock-helper"
chmod 700 "$repo_root/$target_rel/debug/tilo-unlock-helper"
grep -Fq 'sudo install -o root -g root -m 0755' "$tool"
grep -Fq '/usr/libexec/tilo-unlock-helper' "$tool"
grep -Fq 'sudo install -o root -g root -m 0644' "$tool"
grep -Fq '/etc/pam.d/tilo-unlock' "$tool"
grep -Fq 'restorecon -F /usr/libexec/tilo-unlock-helper' "$tool"
grep -Fq "root:root:755:regular file" "$tool"
grep -Fq "root:root:644:regular file" "$tool"
grep -Fq 'test -u /usr/sbin/unix_chkpwd' "$tool"

grep -Fq 'tilo-dev-session/active' "$tool"
grep -Fq 'apps-single' "$tool"
grep -Fq 'dev-app-environment' "$tool"
grep -Fq 'TILO_APPS_SYSTEM_PAYLOAD_DIR' "$repo_root/dist/session/tilo-session-dev"
grep -Fq ': "${TILO_REDUCE_MOTION:=1}"' "$repo_root/dist/session/tilo-session-dev"
grep -Fq 'export WGPU_BACKEND WGPU_ADAPTER_NAME VK_DRIVER_FILES TILO_REDUCE_MOTION' \
    "$repo_root/dist/session/tilo-session-dev"

rm -rf "$repo_root/$target_rel"
echo 'fedora44-vm-iterate: PASS'
