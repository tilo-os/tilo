#!/usr/bin/env bash
# Build the runtime module the page tests serve beside the page's sources
# (tools/web/page-bundle.mjs), before the tests run: the browser peer's
# nextest setup script (.config/nextest.toml, `page-bundle`).
#
#   tools/build-page-bundle.sh
#
# A page test that built the module itself spent its first minutes compiling
# wasm inside the time it is given to answer, so on a cold build it looked
# hung and failed; two such tests also built it twice at once. Built here,
# once per run, the module's directory is handed to the run's tests as
# TILO_PAGE_BUNDLE (through nextest's NEXTEST_ENV), which pageBundle()
# answers with and builds nothing.
#
# A run already handed a bundle (TILO_PAGE_BUNDLE set to a built one) builds
# nothing and hands that one on.
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

hand() {
    if [[ -n "${NEXTEST_ENV:-}" ]]; then
        echo "TILO_PAGE_BUNDLE=$1" >>"$NEXTEST_ENV"
    fi
}

if [[ -n "${TILO_PAGE_BUNDLE:-}" && -f "$TILO_PAGE_BUNDLE/pkg/tilo_browser_runtime.js" ]]; then
    hand "$TILO_PAGE_BUNDLE"
    exit 0
fi

cd "$repo"
target=$("${CARGO:-cargo}" metadata --format-version 1 --no-deps --offline |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
# In the target directory, not /tmp: Cargo's own output is there already,
# and the module is kept for the next run to replace.
bundle="$target/page-bundle"
mkdir -p "$bundle"
# Its progress to stderr: a setup script's stdout is nextest's. `--update`
# binds the module again only when Cargo linked a new one.
bash crates/browser/tilo-browser-runtime/scripts/build.sh "$bundle" --profile quick --module-only --update >&2
hand "$bundle"
