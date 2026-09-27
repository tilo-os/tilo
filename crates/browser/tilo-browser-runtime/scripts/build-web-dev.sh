#!/usr/bin/env bash
# Local development build of the web runtime: the core plus app side modules
# in one directory a static server can serve, with apps.html as index.html.
# resolveApp is satisfied from ./apps/ (app-directory.mjs); no registry.
#
#   build-web-dev.sh OUTPUT [PACKAGE...]     (default: ebook word-processor)
#
# Then e.g. `python3 -m http.server -d OUTPUT` and open it in a browser with
# WebGPU. Sizes of every module are in OUTPUT/build/sizes.md.
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
node "$scripts/web-runtime/smoke.mjs" "$out" >"$out/build/smoke.json"
node "$scripts/web-runtime/report.mjs" "$out" >"$out/build/sizes.md"
cat "$out/build/sizes.md"
