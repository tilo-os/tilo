#!/bin/bash
# disk-image.sh runs bootc from root podman. TILO_ROOT_PODMAN_STORE moves root
# podman's image store and scratch off /var into one directory (a build host
# with a small root filesystem), and the bootc container sees that store as its
# /var/lib/containers; without it, root podman keeps its defaults.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/udev/data"
export TILO_TEST_REAL_PYTHON="$(command -v python3)"

digest="sha256:$(printf 'a%.0s' {1..64})"
authoritative="registry.tilo.so/tilo/tilo-development@$digest"
mirror_repository="172.25.116.162:5000/tilo/tilo-development"

cat >"$work/bin/skopeo" <<EOF
#!/bin/sh
printf '%s %s\n' '$mirror_repository' '$digest'
EOF
# sudo -n env VAR=... podman ...: record the environment root podman gets.
cat >"$work/bin/sudo" <<EOF
#!/bin/sh
[ "\${1:-}" != -n ] || shift
if [ "\${1:-}" = env ]; then printf '%s\n' "\$@" >'$work/sudo-env-argv'; fi
exec "\$@"
EOF
for tool in systemd-repart udevadm mount umount; do
  printf '#!/bin/sh\nexit 0\n' >"$work/bin/$tool"
done
cat >"$work/bin/losetup" <<'EOF'
#!/bin/sh
case " $* " in
  *' -fP '*) printf '/dev/loop-test\n' ;;
esac
EOF
cat >"$work/bin/python3" <<'EOF'
#!/bin/bash
if [[ ${1:-} == -u && ${2:-} == */hold-loop-partition-nodes.py ]]; then shift; fi
if [[ ${1:-} == */check-composefs-verity.py ]]; then exit 0; fi
if [[ ${1:-} == */hold-loop-partition-nodes.py ]]; then
  printf 'READY\n'
  read -r release
  [[ $release == release ]]
else
  exec "$TILO_TEST_REAL_PYTHON" "$@"
fi
EOF
# The bootc install is the first root podman call on this path: record it and
# stop there.
cat >"$work/bin/podman" <<EOF
#!/bin/sh
printf '%s\n' "\$@" >'$work/podman-argv'
exit 41
EOF
chmod +x "$work/bin/"*

run_install() {
  rm -f "$work/podman-argv" "$work/sudo-env-argv"
  mkdir -p "$work/out"
  set +e
  PATH="$work/bin:$PATH" TILO_DISTROBOX= TILO_DISK_HOST_UDEV_DIR="$work/udev" "$@" \
    "$repo/os/dev/disk-image.sh" "$authoritative" "$work/out" \
    --composefs --sealed-source-imgref "$mirror_repository:local" --raw \
    >"$work/stdout" 2>"$work/stderr"
  local status=$?
  set -e
  [ "$status" -ne 0 ] || { echo "disk-image.sh passed a failing install" >&2; exit 1; }
  [ -s "$work/podman-argv" ] || { cat "$work/stderr" >&2; echo "bootc install never ran" >&2; exit 1; }
}

# Default: root podman's own store, bound into the bootc container as it is.
run_install env
grep -Fxq -- '/var/lib/containers:/var/lib/containers' "$work/podman-argv"
! grep -Fxq -- '--root' "$work/podman-argv"

# A store directory: root podman keeps images, run state and scratch there.
store="$work/root-store"
run_install env TILO_ROOT_PODMAN_STORE="$store"
first_two="$(head -2 "$work/podman-argv" | tr '\n' ' ')"
[ "$first_two" = "--root $store/containers/storage " ] \
  || { echo "root podman did not start with --root $store/containers/storage: $first_two" >&2; exit 1; }
grep -Fxq -- '--runroot' "$work/podman-argv"
grep -Fxq -- "$store/run" "$work/podman-argv"
grep -Fxq -- "$store/containers:/var/lib/containers" "$work/podman-argv"
! grep -Fxq -- '/var/lib/containers:/var/lib/containers' "$work/podman-argv"
grep -Fxq -- "TMPDIR=$store/tmp" "$work/sudo-env-argv"
[ -d "$store/containers/storage" ] && [ -d "$store/run" ] && [ -d "$store/tmp" ]

printf 'ok: root podman keeps its store where TILO_ROOT_PODMAN_STORE says, and only there\n'
