#!/bin/bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/fat"
export TILO_TEST_REAL_PYTHON="$(command -v python3)"

digest="sha256:$(printf 'a%.0s' {1..64})"
authoritative="code.example/tilo@$digest"
mirror="127.0.0.1:5000/tilo@$digest"

cat >"$work/bin/skopeo" <<EOF
#!/bin/sh
printf '%s %s\n' '127.0.0.1:5000/tilo' '$digest'
EOF
cat >"$work/bin/sudo" <<'EOF'
#!/bin/sh
[ "${1:-}" != -n ] || shift
exec "$@"
EOF
cat >"$work/bin/systemd-repart" <<'EOF'
#!/bin/sh
exit 0
EOF
cat >"$work/bin/udevadm" <<'EOF'
#!/bin/sh
exit 0
EOF
cat >"$work/bin/blkid" <<'EOF'
#!/bin/sh
printf 'A1B2-C3D4\n'
EOF
cat >"$work/bin/findmnt" <<'EOF'
#!/bin/sh
printf '11111111-2222-3333-4444-555555555555\n'
EOF
cat >"$work/bin/losetup" <<'EOF'
#!/bin/sh
case " $* " in *' -fP '*) printf '/dev/loop-test\n';; esac
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
cat >"$work/bin/mount" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$MOCK_MOUNTS"
case "${1:-}" in
  /dev/loop-testp2) mkdir -p "$2/state/deploy/test/etc" ;;
esac
exit 0
EOF
cat >"$work/bin/umount" <<'EOF'
#!/bin/sh
exit 0
EOF
cat >"$work/bin/podman" <<'EOF'
#!/bin/sh
printf '%s\n' "$@" >"$MOCK_PODMAN_ARGS"
mkdir -p "$MOCK_OUTPUT/.mnt/boot/EFI/Linux" "$MOCK_OUTPUT/.mnt/boot/loader/entries"
printf 'signed uki payload\n' >"$MOCK_OUTPUT/.mnt/boot/EFI/Linux/tilo.efi"
printf 'title Tilo\n' >"$MOCK_OUTPUT/.mnt/boot/loader/entries/tilo.conf"
printf 'UUID=A1B2-C3D4 /boot auto defaults,ro 0 0\n' \
  >"$MOCK_OUTPUT/.mnt/state/deploy/test/etc/fstab"
EOF
cat >"$work/bin/mcopy" <<'EOF'
#!/bin/bash
set -e
args=("$@")
last="${args[-1]}"
if [ "$last" = '::/' ]; then
  sources=("${args[@]:5:${#args[@]}-6}")
  cp -a "${sources[@]}" "$MOCK_FAT/"
else
  destination="$last"
  cp -a "$MOCK_FAT/." "$destination"
fi
EOF
chmod +x "$work/bin/"*

PATH="$work/bin:$PATH" MOCK_OUTPUT="$work/output" MOCK_FAT="$work/fat" \
  MOCK_MOUNTS="$work/mounts.log" MOCK_PODMAN_ARGS="$work/podman-args" \
  TILO_DISTROBOX= TILO_DISK_FORCE_MTOOLS_ESP=1 \
  TILO_DISK_ESP_STAGE_PARENT="$work" \
  "$repo/os/dev/disk-image.sh" "$authoritative" "$work/output" \
  --composefs --sealed-source-imgref "$mirror" --size 20G --raw >/dev/null

cmp "$work/fat/EFI/Linux/tilo.efi" <(printf 'signed uki payload\n')
cmp "$work/fat/loader/entries/tilo.conf" <(printf 'title Tilo\n')
[ -z "$(find "$work/output/.mnt/boot" -mindepth 1 -print -quit)" ]
cmp "$work/output/.esp-source.sha256" "$work/output/.esp-copied.sha256"
grep -Eq -- '^--bind '"$work/tilo-esp-stage\.[^ ]+ $work/output/.mnt/boot"'$' \
  "$work/mounts.log"
grep -Fxq -- '--boot-mount-spec' "$work/podman-args"
grep -Fxq -- 'UUID=A1B2-C3D4' "$work/podman-args"
[ -z "$(find "$work" -maxdepth 1 -name 'tilo-esp-stage.*' -print -quit)" ]

printf 'ok: mtools fallback verifies the ESP and removes the root staging copy\n'
