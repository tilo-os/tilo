#!/usr/bin/env bash
# The web entry image's build context (crates/browser/tilo-browser-runtime/container): the
# desktop bundle (web-desktop-bundle.sh, the web-desktop-bundle check's output) laid out under
# its content version, and the server's files.
# usage: web-entry-context.sh OUT BUNDLE
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: web-entry-context.sh OUT BUNDLE}
bundle=${2:?usage: web-entry-context.sh OUT BUNDLE}
cd "$repo_root"
[[ -f $bundle/build.json ]] || die "no desktop bundle at $bundle"
rm -rf "$out"
mkdir -p "$out"
container=crates/browser/tilo-browser-runtime/container
python3 -I "$container/layout.py" "$bundle" "$out/site"
cp "$container/Containerfile" "$container/Caddyfile" "$container/entrypoint.sh" "$out/"
# The image's revision label (the Containerfile's SOURCE_REVISION).
git rev-parse HEAD > "$out/source-revision"
