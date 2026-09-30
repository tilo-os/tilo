#!/bin/bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"
export TILO_TEST_REAL_PYTHON="$(command -v python3)"

digest="sha256:$(printf 'a%.0s' {1..64})"
authoritative="code.example/tilo@$digest"
mirror="127.0.0.1:5000/tilo@$digest"
printf 'alice:$6$salt$%s\n' "$(printf 'a%.0s' {1..86})" >"$work/seed"

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
case "${1:-}" in
  /dev/loop-testp2)
    mkdir -p "$2/state/deploy/test/etc"
    ;;
esac
exit 0
EOF
cat >"$work/bin/umount" <<'EOF'
#!/bin/sh
exit 0
EOF
cat >"$work/bin/podman" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$work/bin/"*

PATH="$work/bin:$PATH" TILO_DISTROBOX= \
  "$repo/os/dev/disk-image.sh" "$authoritative" "$work/output" \
  --composefs --sealed-source-imgref "$mirror" --firstboot-seed "$work/seed" \
  --size 20G --raw >/dev/null

target="$work/output/.mnt/state/deploy/test/etc"
cmp "$work/seed" "$target/tilo/install-user.conf"
[ "$(stat -c '%a' "$target/tilo/install-user.conf")" = 600 ]
[ "$(readlink "$target/systemd/system/multi-user.target.wants/tilo-firstboot-user.service")" = \
  /usr/lib/systemd/system/tilo-firstboot-user.service ]

printf 'ok: disposable disk uses the installer first-boot account handoff\n'
