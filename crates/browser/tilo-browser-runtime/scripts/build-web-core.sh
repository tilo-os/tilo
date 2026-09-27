#!/usr/bin/env bash
# Build the web toolkit core: the dynamic-linking main module app side modules
# load against, post-processed for wasm-bindgen, optimised, and stamped with
# its runtime ABI id. See WEB_RUNTIME.md.
#
#   build-web-core.sh OUTPUT [--features "cloud static-apps"]
#
# OUTPUT gets pkg/ (the core and its wasm-bindgen glue), the page host
# modules, fonts, runtime.json, and build/ with what build-web-app.sh needs.
set -euo pipefail
source "$(dirname "$0")/web-runtime/env.sh"
out=${1:?usage: build-web-core.sh OUTPUT [--features "..."]}
shift
features=""
if [[ $# -gt 0 ]]; then
  [[ $# == 2 && "$1" == --features ]] || { echo 'usage: build-web-core.sh OUTPUT [--features "..."]' >&2; exit 2; }
  features=$2
fi
mkdir -p "$out"
out=$(cd "$out" && pwd)
need_tool wasm-bindgen "cargo install wasm-bindgen-cli --version 0.2.108"
need_tool wasm-opt "binaryen"
need_tool wasm-ld "lld"
need_tool node "Node.js 20+"
need_tool brotli "brotli"
[[ "$(wasm-bindgen --version)" == "wasm-bindgen 0.2.108" ]] || { echo "wasm-bindgen 0.2.108 required, found $(wasm-bindgen --version)" >&2; exit 2; }
tools="$web_scripts/wasm-tools.mjs"
build="$out/build"
mkdir -p "$build" "$out/pkg"

core_features="tilo-browser-runtime/dylink"
for feature in $features; do core_features+=" tilo-browser-runtime/$feature"; done
mapfile -t roots < <(web_roots)
echo "== web core: cargo build (features: $core_features; collection: ${web_collection[*]})" >&2
web_cargo build "${roots[@]}" --lib --features "$core_features" \
  --message-format=json-render-diagnostics >"$build/core-cargo.json"
web_graph "${roots[@]}" --features "$core_features" >"$build/graph.json"
cp "$web_artifacts/tilo_browser_runtime.wasm" "$build/core.linked.wasm"
node "$tools" profile "$build/core.linked.wasm" 60 >"$build/core-profile.txt"

# wasm-bindgen must still find and remove its describe functions, which
# --export-all exposed under mangled names; the lld exports themselves stay.
node "$tools" strip-bindgen-internals "$build/core.linked.wasm" "$build/core.stripped.wasm"
features_flags=(--enable-bulk-memory --enable-mutable-globals --enable-sign-ext --enable-nontrapping-float-to-int
  --enable-reference-types --enable-multivalue)
# Position-independent code takes a function's address as `__table_base + k`
# through a GOT global, constant in this non-PIE link. wasm-bindgen's descriptor
# interpreter does not follow globals, so fold the constants in first.
wasm-opt --simplify-globals --precompute "${features_flags[@]}" "$build/core.stripped.wasm" -o "$build/core.folded.wasm"
rm "$build/core.stripped.wasm"
rm -rf "$out/pkg" && mkdir -p "$out/pkg"
wasm-bindgen --target web --keep-lld-exports --no-typescript --out-dir "$out/pkg" --out-name tilo_browser_runtime "$build/core.folded.wasm"
rm "$build/core.folded.wasm"
# One self-contained glue file, so a page can run a core from a verified source.
node "$web_scripts/bundle-glue.mjs" "$out/pkg" tilo_browser_runtime.js
core="$out/pkg/tilo_browser_runtime_bg.wasm"
if node "$tools" imports-from "$core" __wbindgen_placeholder__ | grep .; then
  echo "wasm-bindgen left the placeholder imports above: an export still reaches a describe function (see isBindgenInternal)" >&2
  exit 1
fi
cp "$core" "$build/core.bindgen.wasm"
wasm-opt -Oz --strip-debug --strip-producers "${features_flags[@]}" "$build/core.bindgen.wasm" -o "$core"
node "$tools" exports "$core" >"$build/core-exports.txt"

abi_json=$(node "$web_scripts/abi.mjs" "$build/core-cargo.json" "$build/graph.json" "$repo" "$web_runtime_name")
printf '%s\n' "$abi_json" >"$build/abi-inputs.json"
abi=$(node -e 'console.log(JSON.parse(process.argv[1]).abi)' "$abi_json")
node "$tools" add-section "$core" "$core" tilo.runtime_abi "$abi"
echo "== web core ABI: $abi" >&2

cp "$web_crate"/web/*.mjs "$web_crate"/web/*.d.ts "$out/"
rm -f "$out"/*.test.mjs
node "$web_scripts/fonts.mjs" "$repo/crates/sdk/ui-core/assets/fonts" "$out" >"$build/fonts-sizes.json"

node - "$out" "$abi" "$web_runtime_name" "$features" "${web_collection[*]}" <<'JS'
const {createHash} = require('node:crypto');
const {readFileSync, writeFileSync} = require('node:fs');
const [out, abi, runtime, features, collection] = process.argv.slice(2);
const file = 'pkg/tilo_browser_runtime_bg.wasm';
const bytes = readFileSync(`${out}/${file}`);
const core = {path: file, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex')};
const words = text => text.split(/\s+/).filter(Boolean);
const manifest = {schema: 1, runtime, abi, target: 'wasm32-unknown-unknown', features: words(features), collection: words(collection), core};
writeFileSync(`${out}/runtime.json`, JSON.stringify(manifest, null, 2) + '\n');
JS
node "$web_scripts/sizes.mjs" core "$build/core.linked.wasm" "$build/core.bindgen.wasm" "$core" >"$build/core-sizes.json"
cat "$build/core-sizes.json" >&2
