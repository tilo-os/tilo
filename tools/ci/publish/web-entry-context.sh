#!/usr/bin/env bash
# The web entry image's build context (crates/browser/tilo-browser-runtime/container): the
# desktop bundle (web-desktop-bundle.sh, the web-desktop-bundle check's output) laid out under
# its content version, the developer docs (tools/docs/build-site.sh, the docs-site check's output)
# under docs/, and the server's files.
# usage: web-entry-context.sh OUT BUNDLE DOCS
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: web-entry-context.sh OUT BUNDLE DOCS'
out=${1:?$usage}
bundle=${2:?$usage}
docs=${3:?$usage}
cd "$repo_root"
[[ -f $bundle/build.json ]] || die "no desktop bundle at $bundle"
[[ -f $docs/index.html && -f $docs/llms.txt ]] || die "no developer docs at $docs"
rm -rf "$out"
mkdir -p "$out"
container=crates/browser/tilo-browser-runtime/container
python3 -I "$container/layout.py" "$bundle" "$out/site"
cp -a "$docs" "$out/site/docs"
cp "$container/Containerfile" "$container/Caddyfile" "$container/entrypoint.sh" "$container/compress.sh" "$out/"
# What `curl -fsSL https://<site> | sh` gets: the tilo command's installer.
cp dist/install/tilo.sh "$out/install.sh"
# The image's revision label (the Containerfile's SOURCE_REVISION).
git rev-parse HEAD > "$out/source-revision"
