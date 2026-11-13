#!/usr/bin/env bash
# Playwright's builds of Firefox and WebKit, for the tools that show the page
# in them as well as in Chrome for Testing (engines.mjs).
#
# `install` puts `playwright-core` beside the other build outputs and prints
# the PLAYWRIGHT path to import it from; `check BROWSER` starts that browser
# once and says what it is missing, if anything: a host needs what Playwright's
# own install would give it (`playwright install --with-deps` on a distribution
# those builds are made for). On a Fedora host the WebKit build wants a few
# libraries by older sonames than Fedora ships (libicu 74, libjpeg-turbo 8) —
# with those in place it runs, and `check` names them instead of letting a run
# fail with "Target page, context or browser has been closed".
#
# usage: tools/web/html-renderer/engines.sh [install|check chromium|firefox|webkit]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=${CARGO_TARGET_DIR:-$(cd "$here/../../.." && pwd)/target}/htmlrenderer
version=1.64.1
prefix=$out/playwright
export PLAYWRIGHT=${PLAYWRIGHT:-$prefix/node_modules/playwright-core/index.mjs}

install() {
  mkdir -p "$prefix"
  printf '{"name":"html-renderer-playwright","private":true,"dependencies":{"playwright-core":"%s"}}' "$version" > "$prefix/package.json"
  if command -v bun >/dev/null; then
    (cd "$prefix" && bun install --silent)
  elif command -v npm >/dev/null; then
    (cd "$prefix" && npm install --silent --no-audit --no-fund)
  else
    echo "engines.sh: need bun or npm to install playwright-core" >&2
    exit 1
  fi
  echo "PLAYWRIGHT=$PLAYWRIGHT"
}

check() {
  local engine=${1:-webkit}
  node --input-type=module -e "
    const {open} = await import('$here/engines.mjs');
    const opened = await open({engine: '$engine', width: 800, height: 600});
    const tab = opened.tab;
    await tab.navigate('data:text/html,<p>ok');
    console.log('$engine', await tab.evaluate('navigator.userAgent'));
    await tab.close();
  "
}

case ${1:-} in
  install) install ;;
  check) check "${2:-webkit}" ;;
  *) echo "usage: tools/web/html-renderer/engines.sh [install|check chromium|firefox|webkit]" >&2; exit 2 ;;
esac
