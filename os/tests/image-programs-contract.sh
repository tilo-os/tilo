#!/usr/bin/env bash
# The image's programs, checked in the composed image (os-image-contracts in t.toml): each one a
# device runs is there and executable, the session and the X11 bridge resolve every library they
# load (a gap is a loader error at the first launch, on a device), the browser engine's runtime
# verifies against its publisher. The image build lays the desktop programs over the finished
# image (os/image/binaries.Containerfile), so nothing inside the build can run these.
#
#   podman run --rm --entrypoint bash -v "$PWD/os/tests:/run/tests:ro" IMAGE \
#       /run/tests/image-programs-contract.sh
set -euo pipefail

for f in /usr/libexec/tilo/tilo-server \
    /usr/libexec/tilo-server-firstboot /usr/libexec/tilo-server-state-migrate \
    /usr/libexec/tilo-windows-vm-control /usr/libexec/tilo-windows-rail \
    /usr/bin/tilo-session /usr/bin/fs /usr/libexec/tilo/tilo-greeter \
    /usr/libexec/tilo/tilo-installer /usr/libexec/tilo/tilo-launcher /usr/libexec/tilo/tilo-runtime-bundle \
    /usr/libexec/tilo/tilo-install /usr/libexec/tilo/tilo-app-package /usr/libexec/tilo-cli-shim \
    /usr/libexec/tilo-unlock-helper /usr/libexec/tilo-ush \
    /usr/libexec/tilo-projector /usr/libexec/tilo-doubletake \
    /usr/libexec/tilo-doubletake-test-receiver \
    /usr/libexec/tilo-account-credential; do
  [ -e "$f" ] || { echo "$f is not in the image" >&2; exit 1; }
  mode=$(stat -L -c %a "$f")
  [ "$mode" = 755 ] || { echo "$f has mode $mode, not 755" >&2; exit 1; }
done

missing="$(ldd /usr/bin/tilo-session /usr/bin/xwayland-satellite | awk '/not found/ { print $1 }')"
if [ -n "$missing" ]; then
  printf 'the session or xwayland-satellite has unresolved runtime libraries:\n%s\n' "$missing" >&2
  exit 1
fi

# The browser's engine, where the image carries one (TILO_WITH_BROWSER): its runtime bundle,
# signed by the browser runtime's publisher.
for engine in /usr/lib/tilo-runtime/*/lib/chromium; do
  [ -d "$engine" ] || continue
  runtime=${engine%/lib/chromium}
  [ -x "$engine/tilo-chromium-host" ] && [ -s "$engine/libcef.so" ] \
    || { echo "$runtime carries no whole browser engine" >&2; exit 1; }
  /usr/libexec/tilo/tilo-runtime-bundle verify "$runtime" \
    --require-publisher "$(cat /usr/share/tilo/browser-runtime-publisher.hex)"
done

echo "the image's programs are in place and resolve what they load"
