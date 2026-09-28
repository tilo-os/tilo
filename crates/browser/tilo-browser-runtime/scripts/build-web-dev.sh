#!/usr/bin/env bash
# Local development build of the web runtime: the core plus app side modules
# in one directory a static server can serve, with apps.html as index.html.
# resolveApp is satisfied from ./apps/ (app-directory.mjs); no registry.
#
#   build-web-dev.sh OUTPUT [PACKAGE...]     (default: ebook word-processor)
#
# Then e.g. `python3 -m http.server -d OUTPUT` and open it in a browser with
# WebGPU. Sizes of every module are in OUTPUT/build/sizes.md. registry.html
# is the same from a local registry; see WEB_RUNTIME.md.
set -euo pipefail
scripts=$(cd "$(dirname "$0")" && pwd)
out=${1:?usage: build-web-dev.sh OUTPUT [PACKAGE...]}
shift
packages=("$@")
[[ ${#packages[@]} -gt 0 ]] || packages=(ebook word-processor)
mkdir -p "$out"
out=$(cd "$out" && pwd)
rm -rf "$out/apps" "$out/build/apps"
"$scripts/build-web-core.sh" "$out"
for package in "${packages[@]}"; do
  "$scripts/build-web-app.sh" "$out" "$package"
done
cp "$scripts/../web/apps.html" "$out/index.html"
# open.html: MountOptions.open, two mounts of one record (phone and laptop).
cp "$scripts/../web/open.html" "$out/open.html"
# desktop.html: MountOptions.local, a Books record expanding into the desktop.
cp "$scripts/../web/desktop.html" "$out/desktop.html"
node "$scripts/web-runtime/smoke.mjs" "$out" >"$out/build/smoke.json"
# registry.html loads the same core and apps from a local signed registry
# (tools/web/local-registry.sh OUTPUT writes its registry.json).
registry_client=$(cd "$scripts/../../web-registry-client" && pwd)
cp "$scripts/../web/registry.html" "$registry_client/registry-client.mjs" "$registry_client/registry-verifier.mjs" "$out/"
"$registry_client/build-verifier.sh" "$out" >/dev/null
node "$scripts/web-runtime/report.mjs" "$out" >"$out/build/sizes.md"
cat "$out/build/sizes.md"
