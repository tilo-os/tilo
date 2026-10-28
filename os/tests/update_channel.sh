#!/usr/bin/env bash
# tilo-update-channel: which channels an image can follow, the signed reference each names on
# this device, and a developer's pinned build lasting until the channel moves on; and
# tilo-update-policy-set keeping the channel and the pin beside the schedule.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
channel="$repo/os/usr/libexec/tilo-update-channel"
policy_set="$repo/os/usr/libexec/tilo-update-policy-set"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/root/usr/lib/tilo"

# The update policy, written as tilo-update-policy-set writes it (its own paths are fixed).
cat >"$work/bin/policy-set" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$TILO_TEST_CALLS"
case $1 in
  pin) printf 'schedule=system\nmetered=avoid\nchannel=dev\npin=%s\npin-base=%s\n' "$2" "$3" >"$TILO_UPDATE_POLICY" ;;
  unpin) printf 'schedule=system\nmetered=avoid\nchannel=dev\n' >"$TILO_UPDATE_POLICY" ;;
esac
EOF
# The registry: each reference names the digest its file holds.
cat >"$work/bin/skopeo" <<'EOF'
#!/usr/bin/env bash
[[ $1 == inspect && $2 == --raw ]] || exit 2
file="$TILO_TEST_REGISTRY/$(printf '%s' "${3#docker://}" | tr '/:' '__')"
[[ -f $file ]] && cat "$file"
EOF
chmod +x "$work/bin/"*
mkdir -p "$work/registry"

run() {
  PATH="$work/bin:$PATH" TILO_UPDATE_CHANNEL_ROOT="$work/root" TILO_UPDATE_POLICY="$work/policy" \
    TILO_UPDATE_POLICY_SET="$work/bin/policy-set" TILO_UPDATE_CHANNEL_ARCH=x86_64 \
    TILO_TEST_CALLS="$work/calls" TILO_TEST_REGISTRY="$work/registry" \
    bash "$channel" "$@"
}
publish() {  # $1 = reference, $2 = its manifest
  printf '%s' "$2" >"$work/registry/$(printf '%s' "$1" | tr '/:' '__')"
}
expect() {  # $1 = expected output, $2... = command
  local want=$1 got
  shift
  got="$(run "$@")"
  [ "$got" = "$want" ] || { printf 'tilo-update-channel %s: %s, expected %s\n' "$*" "$got" "$want" >&2; exit 1; }
}

# A release image follows stable only, from the origin it was installed with.
expect stable list
expect stable current
expect '' ref
if run pin registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci-x86_64 2>/dev/null; then
  echo 'a release image took a pinned dev build' >&2
  exit 1
fi

# A dev image follows dev by default, main's dev image of its architecture or board.
echo dev >"$work/root/usr/lib/tilo/image-flavour"
expect $'dev\nstable' list
expect dev current
expect registry.tilo.so/tilo/tilo-vm:x86_64-ci ref
expect registry.tilo.so/tilo/tilo:latest ref stable
printf '{\n  "schema_version": 1,\n  "id": "orangepi-3b-v2-1",\n  "architecture": "aarch64"\n}\n' \
  >"$work/root/usr/lib/tilo/hardware-profile.json"
expect registry.tilo.so/tilo/tilo-vm:orangepi-3b-v2-1-ci ref
expect code.tilo.so/tilo/tilo:orangepi-3b-v2-1-latest ref stable
rm "$work/root/usr/lib/tilo/hardware-profile.json"
printf 'schedule=daily\nchannel=stable\n' >"$work/policy"
expect stable current
expect registry.tilo.so/tilo/tilo:latest next
printf 'schedule=daily\nchannel=nightly\n' >"$work/policy"
expect dev current

# A pinned build is followed until the channel publishes something new, then dropped.
rm "$work/policy"
publish registry.tilo.so/tilo/tilo-vm:x86_64-ci one
run pin registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci-x86_64
grep -Fqx "pin registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci-x86_64 sha256:$(printf one | sha256sum | cut -d' ' -f1)" "$work/calls"
expect registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci-x86_64 next
rm "$work/registry/"*
expect registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci-x86_64 next
publish registry.tilo.so/tilo/tilo-vm:x86_64-ci two
expect registry.tilo.so/tilo/tilo-vm:x86_64-ci next
grep -Fqx unpin "$work/calls"
run pin registry.tilo.so/tilo/tilo-vm-ref:ref-fix-1a2b3c4-x86_64
expect registry.tilo.so/tilo/tilo-vm-ref:ref-fix-1a2b3c4-x86_64 next
for bad in docker.io/library/fedora:latest registry.tilo.so/tilo/tilo-vm-reff:x registry.tilo.so/tilo/tilo:latest 'registry.tilo.so/tilo/tilo-vm:a b'; do
  if run pin "$bad" 2>/dev/null; then
    echo "a pin took $bad" >&2
    exit 1
  fi
done

# The policy service keeps the channel and the pin beside the schedule, takes only channels the
# image lists, and ends a pin when the channel is chosen.
grep -Fq 'channel-dev|channel-stable)' "$policy_set"
grep -Fq "/usr/libexec/tilo-update-channel list | grep -qx \"\${1#channel-}\"" "$policy_set"
grep -Fq 'channel=${1#channel-} pin= pin_base= ;;' "$policy_set"
grep -Fq "[ -z \"\$pin\" ] || printf 'pin=%s\\npin-base=%s\\n'" "$policy_set"
for unit in tilo-update-policy@channel-dev.service tilo-update-policy@channel-stable.service; do
  grep -Fq "\"$unit\": true" "$repo/os/etc/polkit-1/rules.d/49-tilo-updates.rules"
done
echo 'ok: devices follow their channel or a pinned build until the channel moves on'
