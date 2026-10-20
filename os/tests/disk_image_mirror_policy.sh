#!/bin/bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/out" "$work/udev/data"
export TILO_TEST_REAL_PYTHON="$(command -v python3)"

digest="sha256:$(printf 'a%.0s' {1..64})"
authoritative="registry.tilo.so/tilo/tilo-development@$digest"
mirror_repository="198.51.100.7:5000/tilo/tilo-development"

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
printf '#!/bin/sh\nexit 0\n' >"$work/bin/btrfs"
chmod +x "$work/bin/"*
python3 - "$work/bootc-helper" <<'PY'
import platform
import struct
import sys

header = bytearray(64)
header[:7] = b"\x7fELF\x02\x01\x01"
struct.pack_into("<H", header, 16, 2)
struct.pack_into("<H", header, 18, {"aarch64": 183, "x86_64": 62}[platform.machine()])
with open(sys.argv[1], "wb") as output:
    output.write(header)
PY
chmod +x "$work/bootc-helper"

set +e
PATH="$work/bin:$PATH" TILO_DISTROBOX= TILO_DISK_HOST_UDEV_DIR="$work/udev" \
  "$repo/os/dev/disk-image.sh" "$authoritative" "$work/out" \
  --composefs --sealed-source-imgref "$mirror_repository:local" \
  --bootc-helper "$work/bootc-helper" --reseal-uki --raw \
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
grep -Fxq -- "$work/bootc-helper:/run/tilo-installer-bootc:ro" "$work/podman-argv"
grep -Fxq -- '--entrypoint' "$work/podman-argv"
grep -Fxq -- '/run/tilo-installer-bootc' "$work/podman-argv"
grep -Fxq -- 'BOOTC_INSTALL_RESEAL_UKI=1' "$work/podman-argv"
! grep -Fxq -- 'bootc' "$work/podman-argv"
! grep -Fxq -- "$mirror_repository:local" "$work/podman-argv"
[ ! -e "$work/out/.source-policy.json" ]

# The same authoritative source must keep normal TLS and the image's signed
# policy. It is not an unsigned construction mirror.
cat >"$work/bin/skopeo" <<EOF
#!/bin/sh
printf '%s\n' "\$@" >'$work/skopeo-argv'
printf '%s %s\n' 'registry.tilo.so/tilo/tilo-development' '$digest'
EOF
cat >"$work/bin/podman" <<EOF
#!/bin/sh
printf '%s\n' "\$@" >'$work/podman-argv'
cp '$work/out/.registries.conf' '$work/direct-registries.conf'
exit 41
EOF
set +e
PATH="$work/bin:$PATH" TILO_DISTROBOX= TILO_DISK_HOST_UDEV_DIR="$work/udev" \
  "$repo/os/dev/disk-image.sh" "$authoritative" "$work/out" \
  --composefs --sealed-source-imgref "$authoritative" \
  --bootc-helper "$work/bootc-helper" --raw \
  >"$work/direct-stdout" 2>"$work/direct-stderr"
status=$?
set -e
[ "$status" -ne 0 ]
grep -Fxq -- '--enforce-container-sigpolicy' "$work/podman-argv"
! grep -Fxq -- '--tls-verify=false' "$work/podman-argv"
! grep -Fxq -- '--tls-verify=false' "$work/skopeo-argv"
! grep -Fq -- '/etc/containers/policy.json:ro' "$work/podman-argv"
grep -Fxq 'insecure = false' "$work/direct-registries.conf"
printf 'ok: sealed mirror scope retained; authoritative source keeps TLS and signature enforcement\n' 
# The scratch registry the composefs path publishes a local image to is admitted
# the same way: the image's own policy rejects it (default reject), and bootc
# checks the source against it before installing.
script="$repo/os/dev/disk-image.sh"
grep -Fq 'write_source_policy "$SOURCE_POLICY" "$REG/$repo"' "$script"
[ "$(grep -c 'write_source_policy "$SOURCE_POLICY"' "$script")" -eq 2 ]
echo 'ok: the scratch registry install source is admitted for the install only'
