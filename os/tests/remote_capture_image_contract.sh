#!/usr/bin/env bash
# Release-image contract for shell-owned remote capture backends.
set -euo pipefail

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd -- "$here/../.." && pwd)"
containerfile="$repo/os/Containerfile"

fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

for package in sane-backends sane-backends-drivers-scanners; do
  if grep -q "^    $package \\\\$" "$containerfile"; then
    ok "$package is part of the immutable scanner source"
  else
    bad "$package is absent while the shell advertises remote scanner support"
  fi
done

# Camera document scans and Photos imports both route OCR through the shared
# tilo-ocr adapter, whose production backend invokes this local binary with
# an explicit language. Shipping the scanner without the engine or its English
# model would leave a visible OCR action that can never succeed on the image.
for package in tesseract tesseract-langpack-eng; do
  if grep -q "^    $package \\\\$" "$containerfile"; then
    ok "$package is part of the immutable local OCR runtime"
  else
    bad "$package is absent while Camera and Photos advertise local OCR"
  fi
done

# Keep the image contract tied to the production probe rather than merely
# proving that an unrelated scanner package happens to be installed.
# The probe (host.rs) resolves every helper program through trusted_command;
# the source offers scanning only when it found scanimage.
source_adapter="$repo/crates/session/shell/src/shell/remote_capture/source.rs"
capture_probe="$repo/crates/session/shell/src/shell/remote_capture/host.rs"
if grep -Fq 'probe.tool("scanimage")' "$source_adapter" \
    && grep -Fqx '    "scanimage",' "$capture_probe" \
    && grep -Fq 'trusted_command(name)' "$capture_probe"; then
  ok 'remote scanner discovery requires the packaged scanimage binary'
else
  bad 'remote scanner discovery no longer matches the image contract'
fi

kernel_containerfile="$repo/os/kernel/Containerfile"
kernel_acceptance="$repo/os/kernel/tests/test-rpm.sh"
camera_options="$repo/os/etc/modprobe.d/tilo-remote-camera.conf"
camera_modules="$repo/os/etc/modules-load.d/tilo-remote-camera.conf"

if grep -q '^    v4l-utils \\$' "$containerfile"; then
  ok 'v4l2-ctl is part of the immutable remote camera receiver'
else
  bad 'v4l-utils is absent while camera detection requires v4l2-ctl'
fi

if grep -Fq 'REMOTE_CAMERA_LABEL: &str = "Tilo Remote Camera";' "$capture_probe" \
    && grep -Fq 'label.starts_with(REMOTE_CAMERA_LABEL)' "$capture_probe" \
    && grep -Fq 'card_label="Tilo Remote Camera"' "$camera_options"; then
  ok 'runtime detection and the module card label agree exactly'
else
  bad 'remote camera runtime detection and module label have diverged'
fi

if grep -Fqx 'v4l2loopback' "$camera_modules" \
    && grep -Fq 'video_nr=42' "$camera_options" \
    && grep -Fq 'exclusive_caps=1' "$camera_options"; then
  ok 'one exclusive compositor-owned loopback camera loads at boot'
else
  bad 'remote camera module boot configuration is incomplete'
fi

if grep -Fq 'v4l2loopback-0.15.3-a7e4a4d.tar.gz' "$kernel_containerfile" \
    && grep -Fq 'extra/v4l2loopback.ko' "$repo/os/kernel/build-support.sh" \
    && grep -Fq "modinfo -F signer" "$kernel_acceptance"; then
  ok 'the exact-kernel signed loopback module is packaged and release-tested'
else
  bad 'remote camera is advertised without a signed packaged kernel backend'
fi

if grep -Fq 'COPY --from=remote-capture-files / /' "$containerfile" \
    && grep -Fq 'COPY os/etc/modprobe.d/tilo-remote-camera.conf /etc/modprobe.d/tilo-remote-camera.conf' "$containerfile" \
    && grep -Eq '^COPY os/etc/modules-load\.d/tilo-remote-camera\.conf( [^ ]+)* /etc/modules-load\.d/(tilo-remote-camera\.conf)?$' "$containerfile"; then
  ok 'image installs both remote camera module configurations'
else
  bad 'image does not install the remote camera module configuration overlay'
fi

exit "$fail"
