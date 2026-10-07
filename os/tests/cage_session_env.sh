#!/usr/bin/env bash
# os/usr/libexec/tilo-cage-session picks cage's environment from the display
# hardware: QEMU's virtio-gpu gets the software renderer, legacy modesetting
# and the software cursor; a physical GPU keeps all three of its own, above
# all the hardware cursor plane (a composited software cursor leaves a trail
# of old pointer stamps on real hardware).
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
wrapper="$repo/os/usr/libexec/tilo-cage-session"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"
# A cage stand-in that reports the environment it was started with.
cat >"$work/bin/cage" <<'EOF'
#!/bin/sh
printf 'args=%s\n' "$*"
for name in WLR_RENDERER WLR_DRM_NO_ATOMIC WLR_NO_HARDWARE_CURSORS; do
  eval "value=\${$name-unset}"
  printf '%s=%s\n' "$name" "$value"
done
EOF
# Report x86_64 so the ARM-only CPU shim branch stays out of this check.
cat >"$work/bin/uname" <<'EOF'
#!/bin/sh
echo x86_64
EOF
chmod +x "$work/bin/cage" "$work/bin/uname"

card() {
  mkdir -p "$work/drm/$1/device"
  printf 'DRIVER=%s\nPCI_ID=%s\n' "$2" "$3" >"$work/drm/$1/device/uevent"
}

run() {
  env -u WLR_RENDERER -u WLR_DRM_NO_ATOMIC -u WLR_NO_HARDWARE_CURSORS \
    PATH="$work/bin:$PATH" TILO_DRM_SYSFS="$work/drm" TILO_CAGE="$work/bin/cage" \
    bash "$wrapper" /usr/bin/tilo-greeter
}

# --- a physical GPU ------------------------------------------------------
rm -rf "$work/drm"
card card1 msm_dpu ''
out="$(run)"
grep -qx 'args=-s -- /usr/bin/tilo-greeter' <<<"$out" \
  && ok 'the greeter runs in cage, which exits with it' || bad "unexpected cage arguments: $out"
grep -qx 'WLR_NO_HARDWARE_CURSORS=unset' <<<"$out" \
  && ok 'a physical GPU keeps the hardware cursor' || bad "physical GPU forced a software cursor: $out"
grep -qx 'WLR_RENDERER=unset' <<<"$out" && grep -qx 'WLR_DRM_NO_ATOMIC=unset' <<<"$out" \
  && ok 'a physical GPU keeps its renderer and atomic modesetting' || bad "physical GPU lost its renderer: $out"

# --- an amdgpu desktop with a second, non-virtio card ---------------------
rm -rf "$work/drm"
card card0 amdgpu 1002:73BF
card card1 simple-framebuffer ''
out="$(run)"
grep -qx 'WLR_NO_HARDWARE_CURSORS=unset' <<<"$out" \
  && ok 'no virtio card, no software cursor' || bad "non-virtio machine forced a software cursor: $out"

# --- QEMU's virtio-gpu -----------------------------------------------------
rm -rf "$work/drm"
card card0 virtio_gpu 1AF4:1050
out="$(run)"
grep -qx 'WLR_NO_HARDWARE_CURSORS=1' <<<"$out" \
  && ok "virtio-gpu gets the software cursor its flipped hardware cursor needs" \
  || bad "virtio-gpu kept the broken hardware cursor: $out"
grep -qx 'WLR_RENDERER=pixman' <<<"$out" && grep -qx 'WLR_DRM_NO_ATOMIC=1' <<<"$out" \
  && ok 'virtio-gpu renders with pixman on legacy modesetting' || bad "virtio-gpu renderer: $out"

exit "$fail"
