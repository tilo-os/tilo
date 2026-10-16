#!/usr/bin/env bash
# The web entry image's build context (crates/browser/tilo-browser-runtime/container): the
# desktop bundle laid out under its content version, and the server's files.
# usage: web-entry-context.sh OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: web-entry-context.sh OUT}
cd "$repo_root"
export TILO_FONT_ASSET_CACHE=${CARGO_TARGET_DIR:-$repo_root/target}/font-artifacts
python3 tools/font-catalog.py root crates/sdk/ui-core/assets/fonts --scope fixtures --cache "$TILO_FONT_ASSET_CACHE" >/dev/null
rm -rf "$out"
mkdir -p "$out"
bundle=$scratch/web-entry-bundle
moq=$scratch/moq-browser
rm -rf "$bundle" "$moq" && cp -r scripts/moq-browser "$moq"
TILO_MOQ_BROWSER_DIR=$moq crates/browser/tilo-browser-runtime/scripts/build.sh "$bundle" --bundle desktop >/dev/null
container=crates/browser/tilo-browser-runtime/container
python3 -I "$container/layout.py" "$bundle" "$out/site"
cp "$container/Containerfile" "$container/Caddyfile" "$container/entrypoint.sh" "$out/"
# The image's revision label (the Containerfile's SOURCE_REVISION).
git rev-parse HEAD > "$out/source-revision"
