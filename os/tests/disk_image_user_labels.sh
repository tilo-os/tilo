#!/bin/bash
# The offline dev-account customization must relabel account databases after
# shadow-utils replaces them. Otherwise an enforcing guest cannot read NSS.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"
export TILO_TEST_REAL_PYTHON="$(command -v python3)"
export TILO_TEST_PODMAN_LOG="$work/podman-args"

digest="sha256:$(printf 'a%.0s' {1..64})"
cat >"$work/bin/skopeo" <<EOF
#!/bin/sh
printf '%s %s\n' '127.0.0.1:5000/tilo' '$digest'
EOF
cat >"$work/bin/sudo" <<'EOF'
#!/bin/sh
[ "${1:-}" != -n ] || shift
exec "$@"
EOF
for tool in systemd-repart udevadm umount; do
  printf '#!/bin/sh\nexit 0\n' >"$work/bin/$tool"
done
cat >"$work/bin/losetup" <<'EOF'
#!/bin/sh
case " $* " in *' -fP '*) printf '/dev/loop-test\n';; esac
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
case "${1:-}" in
  /dev/loop-testp2)
    mkdir -p "$2/state/deploy/test/etc" "$2/state/os/test/var"
    ;;
esac
exit 0
EOF
cat >"$work/bin/podman" <<'EOF'
#!/bin/sh
printf '%s\n' "$@" >> "$TILO_TEST_PODMAN_LOG"
exit 0
EOF
chmod +x "$work/bin/"*

PATH="$work/bin:$PATH" TILO_DISTROBOX= \
  "$repo/os/dev/disk-image.sh" "code.example/tilo@$digest" "$work/output" \
  --composefs --sealed-source-imgref "127.0.0.1:5000/tilo@$digest" \
  --user quickuser:test:wheel --size 20G --raw >/dev/null

# Inspect the customization actually sent to root Podman, not an unused helper.
grep -Fq 'for db in passwd group shadow gshadow; do' "$work/podman-args"
grep -Fq 'setfiles -F "$fc" "/etc/$db"' "$work/podman-args"
grep -Fq 'setfiles -F "$fc" /var/home' "$work/podman-args"
printf 'ok: offline dev account relabels NSS databases and home\n'
