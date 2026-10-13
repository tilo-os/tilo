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
mirror_repository="198.51.100.7:5000/tilo/tilo-development"

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
for tool in systemd-repart udevadm umount; do
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
if [[ ${1:-} == */check-disk-sectors.py ]]; then exit 0; fi
if [[ ${1:-} == */hold-loop-partition-nodes.py ]]; then
  printf 'READY\n'
  read -r release
  [[ $release == release ]]
else
  exec "$TILO_TEST_REAL_PYTHON" "$@"
fi
EOF
cat >"$work/bin/mount" <<EOF
#!/bin/sh
printf 'mount %s\n' "\$*" >>'$work/storage-order'
EOF
cat >"$work/bin/btrfs" <<EOF
#!/bin/sh
printf 'btrfs %s\n' "\$*" >>'$work/storage-order'
EOF
cat >"$work/bin/tune2fs" <<EOF
#!/bin/sh
printf '%s\n' "\$*" >>'$work/tune2fs-argv'
[ "\${1:-}" != -l ] || printf 'Filesystem features: verity\n'
EOF
# The bootc install is the first root podman call on this path: record it and
# stop there.
cat >"$work/bin/podman" <<EOF
#!/bin/sh
printf '%s\n' "\$@" >'$work/podman-argv'
printf 'bootc\n' >>'$work/storage-order'
exit 41
EOF
chmod +x "$work/bin/"*

run_install() {
  rm -f "$work/podman-argv" "$work/sudo-env-argv" "$work/storage-order"
  mkdir -p "$work/out"
  set +e
  PATH="$work/bin:$PATH" TILO_DISTROBOX= TILO_DISK_HOST_UDEV_DIR="$work/udev" "$@" \
    "$repo/os/dev/disk-image.sh" "$authoritative" "$work/out" \
    --composefs --sealed-source-imgref "$mirror_repository:local" --raw \
    --rootfs "${TILO_TEST_ROOTFS:-btrfs}" --size 16G \
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

# Btrfs compression is in place before bootc writes any objects. The persistent
# property survives the installation mount and applies to newly created dirs.
head -3 "$work/storage-order" >"$work/actual-order"
printf 'mount -o compress=zstd:3 /dev/loop-testp2 %s/out/.mnt\nbtrfs property set %s/out/.mnt compression zstd\nmount /dev/loop-testp1 %s/out/.mnt/boot\n' \
  "$work" "$work" "$work" >"$work/expected-order"
cmp "$work/expected-order" "$work/actual-order"
[ "$(sed -n '4p' "$work/storage-order")" = bootc ]
[ "$(stat -c %s "$work/out/disk.raw")" -eq $((16 * 1024 * 1024 * 1024)) ]

# Ext4 remains uncompressed and still enables fs-verity before installation.
TILO_TEST_ROOTFS=ext4 run_install env
[ "$(head -1 "$work/storage-order")" = "mount /dev/loop-testp2 $work/out/.mnt" ]
! grep -q '^btrfs ' "$work/storage-order"
! grep -q 'compress=' "$work/storage-order"
grep -Fxq -- '-O verity /dev/loop-testp2' "$work/tune2fs-argv"
grep -Fxq -- '-l /dev/loop-testp2' "$work/tune2fs-argv"

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

# sudo strips the rootless storage environment: an explicit root configuration
# must reach the same Podman invocation that installs the image.
run_install env TILO_ROOT_PODMAN_STORE="$store" TILO_ROOT_PODMAN_STORAGE_CONF="$store/storage.conf"
grep -Fxq -- "CONTAINERS_STORAGE_CONF=$store/storage.conf" "$work/sudo-env-argv"
grep -Fxq -- "$store/containers/storage" "$work/podman-argv"

printf 'ok: root podman keeps its store where TILO_ROOT_PODMAN_STORE says, and only there\n'
