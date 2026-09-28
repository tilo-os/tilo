#!/usr/bin/env bash
# The web build of one app outside the Tilo tree (`tilo-app build --platform
# web`): the web core, the app as a side module of it, and a page that mounts
# the app, in one directory a static server can serve. See WEB_RUNTIME.md.
#
#   build-web-standalone.sh OUTPUT WORK APP_DIR [--app-only]
#
# WORK holds the mirrored workspace (external-workspace.mjs), its target dir
# and the core's build products; OUTPUT only what the page loads. --app-only
# relinks the app against the core already in WORK (the development loop).
set -euo pipefail
scripts=$(cd "$(dirname "$0")" && pwd)
usage='usage: build-web-standalone.sh OUTPUT WORK APP_DIR [--app-only]'
out=${1:?$usage}
work=${2:?$usage}
app_dir=${3:?$usage}
app_only=${4:-}
[[ -z "$app_only" || "$app_only" == --app-only ]] || { echo "$usage" >&2; exit 2; }
tilo_root=$(cd "$scripts/../../../.." && pwd)
mkdir -p "$out" "$work"
out=$(cd "$out" && pwd)
work=$(cd "$work" && pwd)

info=$(node "$scripts/web-runtime/external-workspace.mjs" "$tilo_root" "$app_dir" "$work/workspace")
package=$(node -e 'console.log(JSON.parse(process.argv[1]).package)' "$info")
export TILO_WEB_WORKSPACE="$work/workspace"
# The app joins the web collection: what it instantiates of the toolkit's
# generics reaches into the core's crates (automerge's AutoCommit::load, say),
# and the core exports exactly what the collection imports. An app needing no
# crate features the collection lacks leaves the runtime ABI id unchanged.
read -r -a collection <<<"${TILO_WEB_COLLECTION:-ebook word-processor}"
[[ " ${collection[*]} " == *" $package "* ]] || collection+=("$package")
export TILO_WEB_COLLECTION="${collection[*]}"
# Records the app's own packages in the mirrored lock; the builds are --locked.
(cd "$TILO_WEB_WORKSPACE" && cargo metadata --format-version 1 >/dev/null)

core_out="$work/web"
if [[ -z "$app_only" || ! -f "$core_out/runtime.json" ]]; then
  "$scripts/build-web-core.sh" "$core_out"
fi
rm -rf "$core_out/apps" "$core_out/build/apps"
if ! "$scripts/build-web-app.sh" "$core_out" "$package"; then
  # An edit can make the app import what the core does not export yet; the
  # core's export list is the collection's imports, so link it again.
  unresolved=("$core_out"/build/apps/*/unresolved.txt)
  [[ -n "$app_only" && -s "${unresolved[0]}" ]] || exit 1
  echo "== the app now imports what the core does not export; linking the core again" >&2
  "$scripts/build-web-core.sh" "$core_out"
  rm -rf "$core_out/apps" "$core_out/build/apps"
  "$scripts/build-web-app.sh" "$core_out" "$package"
fi

# The page: the core's output without its build products, the app, and a
# development page that mounts the app and reopens its record when a rebuilt
# module arrives. Against an unchanged core only apps/ is replaced, whole.
if [[ -z "$app_only" ]] || ! cmp -s "$core_out/runtime.json" "$out/runtime.json"; then
  find "$out" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
  for entry in "$core_out"/*; do
    [[ "$(basename "$entry")" == build ]] || cp -a "$entry" "$out/"
  done
else
  rm -rf "$out/apps.next"
  cp -a "$core_out/apps" "$out/apps.next"
  rm -rf "$out/apps"
  mv "$out/apps.next" "$out/apps"
fi
cp "$scripts/../web/dev.html" "$out/index.html"
node - "$out" <<'JS'
const {readFileSync} = require('node:fs');
const out = process.argv[2];
const runtime = JSON.parse(readFileSync(`${out}/runtime.json`, 'utf8'));
const index = JSON.parse(readFileSync(`${out}/apps/index.json`, 'utf8'));
const [id] = Object.keys(index.apps);
const app = JSON.parse(readFileSync(`${out}/${index.apps[id]}`, 'utf8'));
console.log(JSON.stringify({id, abi: runtime.abi, core: runtime.core, module: app.module}));
JS
