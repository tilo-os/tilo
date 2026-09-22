#!/bin/bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/out" "$work/udev/data"

digest="sha256:$(printf 'a%.0s' {1..64})"
authoritative="code.podesta.ai/qos/tilo-development@$digest"
mirror_repository="172.25.116.162:5000/qos/tilo-development"

cat >"$work/bin/skopeo" <<EOF
#!/bin/sh
printf '%s %s\n' '$mirror_repository' '$digest'
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
case " $* " in
  *' -fP '*) printf '/dev/loop-test\n' ;;
esac
EOF
cat >"$work/bin/mount" <<'EOF'
#!/bin/sh
exit 0
EOF
cat >"$work/bin/umount" <<'EOF'
#!/bin/sh
exit 0
EOF
cat >"$work/bin/podman" <<EOF
#!/bin/sh
printf '%s\n' "\$@" >'$work/podman-argv'
cp '$work/out/.source-policy.json' '$work/source-policy.json'
exit 41
EOF
chmod +x "$work/bin/"*

set +e
PATH="$work/bin:$PATH" TILO_DISTROBOX= TILO_DISK_HOST_UDEV_DIR="$work/udev" \
  "$repo/os/dev/disk-image.sh" "$authoritative" "$work/out" \
  --composefs --sealed-source-imgref "$mirror_repository:local" --raw \
  >"$work/stdout" 2>"$work/stderr"
status=$?
set -e
[ "$status" -ne 0 ]

python3 - "$work/source-policy.json" "$mirror_repository" <<'PY'
import json
import sys

path, repository = sys.argv[1:]
with open(path, encoding="utf-8") as source:
    policy = json.load(source)
assert policy == {
    "default": [{"type": "reject"}],
    "transports": {
        "docker": {
            repository: [{"type": "insecureAcceptAnything"}],
        },
    },
}
PY

grep -Fxq -- "$mirror_repository@$digest" "$work/podman-argv"
grep -Fxq -- "docker://$mirror_repository@$digest" "$work/podman-argv"
grep -Fxq -- "$authoritative" "$work/podman-argv"
grep -Fxq -- "$work/out/.source-policy.json:/etc/containers/policy.json:ro" \
  "$work/podman-argv"
grep -Fxq -- "$work/udev:/run/udev:ro" "$work/podman-argv"
! grep -Fxq -- "$mirror_repository:local" "$work/podman-argv"
[ ! -e "$work/out/.source-policy.json" ]

printf 'ok: sealed mirror install uses an exact digest and invocation-scoped default-reject policy\n'
