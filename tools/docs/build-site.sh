#!/usr/bin/env bash
# Build the developer docs served at https://tilo.sh/docs/ into OUT, which the
# web entry's image serves at /docs/ (tools/ci/publish/web-entry-context.sh).
#
# The pages are docs/tilo-sh/pages.toml's, exported with their links resolved
# (developer_docs.py) and pinned to this commit; the generated reference pages
# come from GENERATED directories (`developer_docs.py generate`, the
# docs-reference check), and the build fails when one is missing. The site is
# set in Tilo Sans from the font catalog.
#
# usage: build-site.sh OUT [GENERATED_DIR…]
set -euo pipefail
usage='usage: build-site.sh OUT [GENERATED_DIR…]'
out=${1:?$usage}
shift
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo"

generated=()
for dir in "$@"; do
  generated+=(--generated "$dir")
done
export TILO_FONT_ASSET_CACHE=${TILO_FONT_ASSET_CACHE:-${CARGO_TARGET_DIR:-$repo/target}/font-artifacts}
fonts=$(python3 tools/font-catalog.py root crates/sdk/ui-core/assets/fonts --scope fixtures --cache "$TILO_FONT_ASSET_CACHE")

book=$(mktemp -d "${TMPDIR:-/tmp}/docs-book.XXXXXX")
trap 'rm -rf -- "$book"' EXIT
python3 tools/docs/developer_docs.py --manifest docs/tilo-sh/pages.toml export --out "$book/export" "${generated[@]}"
"${CARGO:-cargo}" run --locked -q -p tilo-docs -- build --book "$book/export" --out "$out" --fonts "$fonts"
