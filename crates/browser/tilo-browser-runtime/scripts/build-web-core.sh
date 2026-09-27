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
if [[ ! -f "$TILO_WEB_LINK_STASH/args" ]]; then
  # Cargo found the core fresh, but its link inputs are gone: link it again.
  rm -rf "$web_artifacts"/.fingerprint/tilo-browser-runtime-*
  web_cargo build "${roots[@]}" --lib --features "$core_features" \
    --message-format=json-render-diagnostics >"$build/core-cargo.json"
fi
web_graph "${roots[@]}" --features "$core_features" >"$build/graph.json"

abi_json=$(node "$web_scripts/abi.mjs" "$build/core-cargo.json" "$build/graph.json" "$repo" "$web_runtime_name")
printf '%s\n' "$abi_json" >"$build/abi-inputs.json"
abi=$(node -e 'console.log(JSON.parse(process.argv[1]).abi)' "$abi_json")

# Exports: every function and datum the toolkit crates define for other
# crates (their public surface, whole in the core) plus whatever the app
# collection imports from anywhere else. Not every symbol of every object:
# that kept all dead code alive.
: >"$build/collection-imports.txt"
for package in "${web_collection[@]}"; do
  info=$(node "$web_scripts/side-module.mjs" app "$package" "$repo")
  plan=$(node "$web_scripts/side-module.mjs" plan "$build/core-cargo.json" "$build/graph.json" "$build/core-cargo.json" \
    "$build/graph.json" "$info" "$repo" "$web_runtime_name" "$abi")
  web_link_side_module "$plan" "$info" "$build/prelink" "$build/prelink.wasm"
  node "$tools" import-names "$build/prelink.wasm" >>"$build/collection-imports.txt"
  rm -rf "$build/prelink" "$build/prelink.wasm"
done
for crate in $TILO_WEB_WHOLE_CRATES; do
  [[ -d "$TILO_WEB_LINK_STASH/whole/$crate" ]] || { echo "the core's link did not include $crate; it must depend on it and name it" >&2; exit 1; }
done
# At opt-level z every crate exports the instantiations of foreign generics it
# made (Vec<u32>::dedup_by in tilo_resource, say); keeping all of them costs a
# third of the core. By default only what the toolkit crates define is their
# surface, plus what the collection imports; TILO_WEB_EXPORT_GENERICS=1 keeps
# every instantiation, for apps outside the collection that reuse them.
export_policy=toolkit-items
own=(--own "$TILO_WEB_WHOLE_CRATES")
if [[ "${TILO_WEB_EXPORT_GENERICS:-0}" == 1 ]]; then export_policy=toolkit-items+instantiations; own=(); fi
node "$tools" linkable-symbols "${own[@]}" "$TILO_WEB_LINK_STASH"/whole/*/*.o >"$build/toolkit-symbols.txt"
sort -u "$build/toolkit-symbols.txt" "$build/collection-imports.txt" | sed 's/^/--export-if-defined=/' >"$build/exports.rsp"
echo "== web core: exporting $(wc -l <"$build/exports.rsp") symbols ($(wc -l <"$build/toolkit-symbols.txt") toolkit, $(sort -u "$build/collection-imports.txt" | wc -l) imported by the collection)" >&2
mapfile -t link_args <"$TILO_WEB_LINK_STASH/args"
"${WASM_LD:-wasm-ld}" "${link_args[@]}" "@$build/exports.rsp" -o "$build/core.linked.wasm"
# The profile links once more with a data segment per symbol, so data counts too.
"${WASM_LD:-wasm-ld}" "${link_args[@]}" "@$build/exports.rsp" --no-merge-data-segments -o "$build/core.analysis.wasm"
node "$tools" profile "$build/core.analysis.wasm" 60 >"$build/core-profile.txt"
rm "$build/core.analysis.wasm"

# wasm-bindgen must still find and remove its describe functions, which the
# toolkit's surface exports under mangled names; the lld exports themselves stay.
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
node "$tools" add-section "$core" "$core" tilo.runtime_abi "$abi"
echo "== web core ABI: $abi" >&2

cp "$web_crate"/web/*.mjs "$web_crate"/web/*.d.ts "$out/"
rm -f "$out"/*.test.mjs
node "$web_scripts/fonts.mjs" "$repo/crates/sdk/ui-core/assets/fonts" "$out" >"$build/fonts-sizes.json"

node - "$out" "$abi" "$web_runtime_name" "$features" "${web_collection[*]}" "$export_policy" <<'JS'
const {createHash} = require('node:crypto');
const {readFileSync, writeFileSync} = require('node:fs');
const [out, abi, runtime, features, collection, exports] = process.argv.slice(2);
const file = 'pkg/tilo_browser_runtime_bg.wasm';
const bytes = readFileSync(`${out}/${file}`);
const core = {path: file, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex')};
const words = text => text.split(/\s+/).filter(Boolean);
const manifest = {schema: 1, runtime, abi, target: 'wasm32-unknown-unknown', features: words(features), collection: words(collection), exports, core};
writeFileSync(`${out}/runtime.json`, JSON.stringify(manifest, null, 2) + '\n');
JS
node "$web_scripts/sizes.mjs" core "$build/core.linked.wasm" "$build/core.bindgen.wasm" "$core" >"$build/core-sizes.json"
cat "$build/core-sizes.json" >&2
