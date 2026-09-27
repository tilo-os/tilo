#!/usr/bin/env bash
# Build one component app as a wasm side module of a web core built by
# build-web-core.sh. The app needs no web-specific code: a generated shim
# hands `<crate>::definition()` to the core's catalog. See WEB_RUNTIME.md.
#
#   build-web-app.sh CORE_OUTPUT PACKAGE
#
# Writes CORE_OUTPUT/apps/<app id>/{app.wasm,app.json} and lists the app in
# CORE_OUTPUT/apps/index.json.
set -euo pipefail
source "$(dirname "$0")/web-runtime/env.sh"
core_out=${1:?usage: build-web-app.sh CORE_OUTPUT PACKAGE}
package=${2:?usage: build-web-app.sh CORE_OUTPUT PACKAGE}
core_out=$(cd "$core_out" && pwd)
[[ -f "$core_out/runtime.json" && -f "$core_out/build/core-cargo.json" ]] || { echo "$core_out holds no web core; run build-web-core.sh first" >&2; exit 2; }
tools="$web_scripts/wasm-tools.mjs"
side="$web_scripts/side-module.mjs"
json() { web_json "$@"; }

abi=$(json abi <"$core_out/runtime.json")
core_features=$(json features <"$core_out/runtime.json")
info=$(node "$side" app "$package" "$repo")
app_id=$(json id <<<"$info")
app_dir=$(json dir <<<"$info")
runtime=$(json runtime <<<"$info")
[[ "$runtime" == "$web_runtime_name" ]] || { echo "$package targets runtime $runtime; this web core is $web_runtime_name" >&2; exit 2; }
work="$core_out/build/apps/$app_id"
out="$core_out/apps/$app_id"
rm -rf "$work" "$out"
mkdir -p "$work" "$out"

echo "== $app_id: cargo build $package against the core" >&2
features="tilo-browser-runtime/dylink"
for feature in $core_features; do features+=" tilo-browser-runtime/$feature"; done
# The same invocation as the core's, so shared crates resolve to its units.
read -r -a web_collection <<<"$(json collection <"$core_out/runtime.json")"
extra=()
[[ " ${web_collection[*]} " == *" $package "* ]] || extra=("$package")
mapfile -t roots < <(web_roots "${extra[@]}")
web_cargo build "${roots[@]}" --lib --features "$features" \
  --message-format=json-render-diagnostics >"$work/cargo.json"
web_graph "${roots[@]}" --features "$features" >"$work/graph.json"
plan=$(node "$side" plan "$core_out/build/core-cargo.json" "$core_out/build/graph.json" "$work/cargo.json" "$work/graph.json" \
  "$info" "$repo" "$web_runtime_name" "$abi")
web_link_side_module "$plan" "$info" "$work" "$work/app.linked.wasm"
node "$tools" profile "$work/app.linked.wasm" 40 >"$work/profile.txt"
if ! node "$tools" check-links "$work/app.linked.wasm" "$core_out/build/core-exports.txt" >"$work/unresolved.txt"; then
  echo "$app_id imports what the core does not export:" >&2
  head -40 "$work/unresolved.txt" >&2
  echo "(full list: $work/unresolved.txt)" >&2
  exit 1
fi

rm "$work/cargo.json"
wasm-opt -Oz --strip-debug --strip-producers \
  --enable-bulk-memory --enable-mutable-globals --enable-sign-ext --enable-nontrapping-float-to-int \
  --enable-reference-types --enable-multivalue \
  "$work/app.linked.wasm" -o "$work/app.opt.wasm"
node "$tools" add-section "$work/app.opt.wasm" "$out/app.wasm" tilo.runtime_abi "$abi"
node "$side" manifest "$out" "$info" "$abi" "$app_dir/assets/web"
node - "$core_out/apps/index.json" "$app_id" <<'JS'
const {existsSync, readFileSync, writeFileSync} = require('node:fs');
const [path, id] = process.argv.slice(2);
const index = existsSync(path) ? JSON.parse(readFileSync(path, 'utf8')) : {schema: 1, apps: {}};
index.apps[id] = `apps/${id}/app.json`;
writeFileSync(path, JSON.stringify(index, null, 2) + '\n');
JS
node "$web_scripts/sizes.mjs" "$app_id" "$work/app.linked.wasm" "$work/app.opt.wasm" "$out/app.wasm" >"$work/sizes.json"
cat "$work/sizes.json" >&2
