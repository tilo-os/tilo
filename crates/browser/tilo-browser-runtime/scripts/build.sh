#!/usr/bin/env bash
# Build the browser runtime bundle into an empty directory:
#   pkg/                wasm-bindgen output (--target web)
#   *.mjs, index.html   page host and diagnostic page
#   fonts/, fonts.json  canonical product fonts, their manifest and licences
#   data/, data.json    application data fetched on request (desktop bundle:
#                       the Typst editor's fonts), with sizes and hashes
#   build.json          revision, tools, profile and output hashes
#
# usage: build.sh OUTPUT_DIR [--profile release|dev] [--[no-]wasm-opt] [--bundle runtime|desktop]
# `--profile release` compiles with Cargo's `wasm-release` profile and runs
# wasm-opt (binaryen) unless `--no-wasm-opt`; `dev` skips it unless `--wasm-opt`.
# `--bundle desktop` builds crates/browser/tilo-web-desktop instead: the same
# runtime with every web-capable app registered from its manifest, under the
# runtime's module name so the page host loads it unchanged, plus apps.json.
# Needs the wasm32-unknown-unknown target and a wasm-bindgen CLI matching the
# crate's pinned wasm-bindgen.
set -euo pipefail

crate=$(cd "$(dirname "$0")/.." && pwd)
repo=$(cd "$crate/../../.." && pwd)
out=${1:?usage: build.sh OUTPUT_DIR [--profile release|dev] [--[no-]wasm-opt] [--bundle runtime|desktop]}
shift
profile=release
optimize=
bundle=runtime
while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile) profile=${2:?--profile needs a value}; shift 2 ;;
    --wasm-opt) optimize=1; shift ;;
    --no-wasm-opt) optimize=0; shift ;;
    --bundle) bundle=${2:?--bundle needs a value}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$profile" in
  release) cargo_profile=wasm-release artifact=wasm-release optimize=${optimize:-1} ;;
  dev) cargo_profile=dev artifact=debug optimize=${optimize:-0} ;;
  *) echo "unsupported profile: $profile" >&2; exit 2 ;;
esac
case "$bundle" in
  runtime) package=tilo-browser-runtime ;;
  desktop) package=tilo-web-desktop ;;
  *) echo "unsupported bundle: $bundle" >&2; exit 2 ;;
esac
[[ "$out" == /* ]] || { echo 'OUTPUT_DIR must be absolute' >&2; exit 2; }
# Stale modules beside new ones would make the bundle's provenance ambiguous.
[[ ! -d "$out" || -z "$(ls -A "$out")" ]] || { echo 'OUTPUT_DIR must be empty' >&2; exit 2; }

cd "$repo"
# A host whose system clang lacks WebAssembly compiles the C dependencies
# (zstd, ring) with Zig's clang instead.
if [[ "${TILO_USE_ZIG:-0}" == 1 ]]; then
  export CC_wasm32_unknown_unknown="$repo/crates/hosting/tilo-relay/scripts/wasm-cc.sh"
  export AR_wasm32_unknown_unknown="$repo/crates/hosting/tilo-relay/scripts/wasm-ar.sh"
fi
bindgen=${WASM_BINDGEN:-wasm-bindgen}
pinned=$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' "$crate/Cargo.toml")
[[ "$("$bindgen" --version)" == "wasm-bindgen $pinned" ]] || {
  echo "wasm-bindgen CLI must be $pinned to match the crate" >&2; exit 2;
}
revision=$(git rev-parse HEAD)
source_status=$(git status --porcelain)

if [[ $bundle == desktop ]]; then
  python3 tools/web/generate_web_desktop.py --check
fi
cargo build --locked -p "$package" --lib --target wasm32-unknown-unknown --profile "$cargo_profile"
# Read Cargo's artifact location; never redirect it.
target_dir=$(cargo metadata --format-version 1 --no-deps --offline |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
mkdir -p "$out/pkg" "$out/fonts"
"$bindgen" "$target_dir/wasm32-unknown-unknown/$artifact/${package//-/_}.wasm" --target web \
  --out-dir "$out/pkg" --out-name tilo_browser_runtime
if [[ $bundle == desktop ]]; then
  cp "$repo/crates/browser/tilo-web-desktop/apps.json" "$out/apps.json"
  # The Typst editor's fonts, which only native builds embed; the editor asks
  # for them by name when it opens (crates/platform/tilo-typst-fonts).
  typst_assets=$(cargo metadata --format-version 1 --locked --offline --filter-platform wasm32-unknown-unknown |
    python3 -c 'import json,os,sys; print(next(os.path.dirname(p["manifest_path"]) for p in json.load(sys.stdin)["packages"] if p["name"]=="typst-assets"))')
  mkdir -p "$out/data/typst"
  cp "$typst_assets"/files/fonts/*.otf "$typst_assets"/files/fonts/*.ttf "$out/data/typst/"
  cp "$typst_assets/NOTICE" "$out/data/typst/NOTICE.txt"
  python3 - "$repo/crates/platform/tilo-typst-fonts/src/lib.rs" "$out/data/typst" <<'PY'
import pathlib, re, sys
wanted = set(re.findall(r'"typst/([^"]+)"', pathlib.Path(sys.argv[1]).read_text()))
shipped = {p.name for p in pathlib.Path(sys.argv[2]).iterdir() if p.suffix in ('.otf', '.ttf')}
assert wanted == shipped, f'tilo_typst_fonts::HOST_FONTS and typst-assets differ: {sorted(wanted ^ shipped)}'
PY
fi
# The features rustc enables for wasm32-unknown-unknown; wasm-opt refuses
# a module that uses one it was not told about.
wasm_opt_flags="-Os --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext --enable-mutable-globals --enable-reference-types --enable-multivalue"
if [[ $optimize == 1 ]]; then
  command -v wasm-opt >/dev/null || { echo 'wasm-opt (binaryen) is required; pass --no-wasm-opt to skip' >&2; exit 2; }
  # shellcheck disable=SC2086
  wasm-opt $wasm_opt_flags "$out/pkg/tilo_browser_runtime_bg.wasm" -o "$out/pkg/tilo_browser_runtime_bg.wasm"
else
  wasm_opt_flags=
fi
find "$crate/web" -maxdepth 1 -name '*.mjs' ! -name '*.test.mjs' -exec cp {} "$out/" \;
cp "$crate/web/index.html" "$out/index.html"
# The live plane's MoQ library is bundled from the published @moq/net pinned
# in scripts/moq-browser/package-lock.json, never committed; this replaces
# any local development copy just copied.
(cd "$repo/scripts/moq-browser" && npm ci --no-audit --no-fund --silent && node build.mjs "$out/media-moq-library.mjs")

python3 - "$repo" "$out" <<'PY'
import json, pathlib, re, shutil, sys
repo, out = map(pathlib.Path, sys.argv[1:])
source = repo / 'crates/sdk/ui-core/assets/fonts'
text = (source / 'manifest.toml').read_text()
assets = []
for block in text.split('[[font]]')[1:]:
    block = block.split('[[license]]')[0]
    if re.search(r'^scope = "product"$', block, re.M):
        name = re.search(r'^file = "(.*?)"$', block, re.M)[1]
        shutil.copy2(source / name, out / 'fonts' / name)
        assets.append(dict(
            name=name,
            bytes=int(re.search(r'^bytes = (\d+)$', block, re.M)[1]),
            sha256=re.search(r'^sha256 = "([0-9a-f]{64})"$', block, re.M)[1],
            startup=bool(re.search(r'^browser_startup = true$', block, re.M)),
        ))
assert any(asset['startup'] for asset in assets), 'no browser startup fonts in the manifest'
shutil.copytree(source / 'LICENSES', out / 'fonts/LICENSES', dirs_exist_ok=True)
shutil.copy2(source / 'manifest.toml', out / 'fonts/manifest.toml')
catalog = re.search(r'^catalog_id = "(.*?)"$', text, re.M)[1]
(out / 'fonts.json').write_text(json.dumps(dict(schema=1, catalog=catalog, assets=assets), separators=(',', ':')) + '\n')
PY

if [[ -d "$out/data" ]]; then
  python3 - "$out" <<'PY'
import hashlib, json, pathlib, sys
out = pathlib.Path(sys.argv[1])
assets = [dict(name=str(p.relative_to(out / 'data')), bytes=p.stat().st_size, sha256=hashlib.sha256(p.read_bytes()).hexdigest())
          for p in sorted((out / 'data').rglob('*')) if p.is_file()]
(out / 'data.json').write_text(json.dumps(dict(schema=1, assets=assets), separators=(',', ':')) + '\n')
PY
fi

# Record the actual tools, profile and output hashes; never infer a prebuilt profile.
[[ "$(git rev-parse HEAD)" == "$revision" && "$(git status --porcelain)" == "$source_status" ]] || {
  echo 'Source changed during build; discard output' >&2; exit 1;
}
wasm_opt_version=
[[ $optimize == 1 ]] && wasm_opt_version=$(wasm-opt --version)
python3 - "$out" "$revision" "$source_status" "$profile" "$(rustc --version)" "$("$bindgen" --version)" "$optimize" \
  "$cargo_profile" "$bundle" "$wasm_opt_version" "$wasm_opt_flags" <<'PY'
import hashlib, json, pathlib, sys
out = pathlib.Path(sys.argv[1])
metadata = dict(revision=sys.argv[2], dirty=bool(sys.argv[3]), profile=sys.argv[4], rustc=sys.argv[5],
                wasmBindgen=sys.argv[6], wasmOpt=sys.argv[7] == '1', target='wasm32-unknown-unknown',
                cargoProfile=sys.argv[8], bundle=sys.argv[9])
if metadata['wasmOpt']:
    metadata.update(wasmOptVersion=sys.argv[10], wasmOptFlags=sys.argv[11])
metadata['files'] = {str(p.relative_to(out)): hashlib.sha256(p.read_bytes()).hexdigest()
                     for p in sorted(out.rglob('*')) if p.is_file()}
(out / 'build.json').write_text(json.dumps(metadata, indent=2) + '\n')
PY
echo "bundle: $out ($(du -sh "$out/pkg/tilo_browser_runtime_bg.wasm" | cut -f1) wasm, $profile)"
