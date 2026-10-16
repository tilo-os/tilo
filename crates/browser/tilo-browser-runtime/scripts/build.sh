#!/usr/bin/env bash
# Build the browser runtime bundle into an empty directory:
#   pkg/                wasm-bindgen output (--target web)
#   *.mjs, index.html   page host and the desktop entry page
#   app/index.html      the full-tab app entry page (web-routing.md)
#   locales/            the entry pages' arrival lines (web/arrival.mjs)
#   fonts/, fonts.json  canonical product fonts, their manifest and licences
#   data/, data.json    application data fetched on request (desktop bundle:
#                       the Typst editor's fonts), with sizes and hashes
#   build.json          revision, tools, profile and output hashes
#
# usage: build.sh OUTPUT_DIR [--profile release|dev|quick] [--[no-]wasm-opt]
#                 [--bundle runtime|desktop] [--no-split] [--update]
# `--profile release` compiles with Cargo's `wasm-release` profile and runs
# wasm-opt (binaryen) unless `--no-wasm-opt`; `dev` skips it unless `--wasm-opt`.
# `quick` is the development loop's: Cargo's `wasm-dev` profile (no debug
# information, light optimisation), whose much smaller module wasm-bindgen
# and the splitter get through in half the time, and no wasm-opt.
# `--bundle desktop` builds crates/browser/tilo-web-desktop instead: the same
# runtime with every web-capable app registered from its manifest, under the
# runtime's module name so the page host loads it unchanged, plus apps.json;
# its module is split (tools/web/wasm-split/split.sh) into a start module and
# one module per app, fetched when the app first opens. `--no-split` (dev and
# quick only) leaves it whole, for a loop that is not about loading apps.
# `--update` (dev and quick only) brings an OUTPUT_DIR this script built
# before up to date, step by step: the module is bound and split again only
# when the linked module changed, the page's modules and entry pages are
# copied again, and fonts, data, the compute module and the MoQ library are
# kept. A loop that edits only web/*.mjs takes seconds; one that edits Rust,
# the compile, wasm-bindgen and, unless `--no-split`, the split.
# Needs the wasm32-unknown-unknown target and a wasm-bindgen CLI matching the
# crate's pinned wasm-bindgen.
set -euo pipefail

crate=$(cd "$(dirname "$0")/.." && pwd)
repo=$(cd "$crate/../../.." && pwd)
usage='usage: build.sh OUTPUT_DIR [--profile release|dev|quick] [--[no-]wasm-opt] [--bundle runtime|desktop] [--no-split] [--update]'
out=${1:?$usage}
shift
profile=release
optimize=
bundle=runtime
split=1
update=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile) profile=${2:?--profile needs a value}; shift 2 ;;
    --wasm-opt) optimize=1; shift ;;
    --no-wasm-opt) optimize=0; shift ;;
    --bundle) bundle=${2:?--bundle needs a value}; shift 2 ;;
    --no-split) split=0; shift ;;
    --update) update=1; shift ;;
    *) echo "unknown argument: $1" >&2; echo "$usage" >&2; exit 2 ;;
  esac
done
case "$profile" in
  release) cargo_profile=wasm-release artifact=wasm-release optimize=${optimize:-1} ;;
  dev) cargo_profile=dev artifact=debug optimize=${optimize:-0} ;;
  quick) cargo_profile=wasm-dev artifact=wasm-dev optimize=${optimize:-0} ;;
  *) echo "unsupported profile: $profile" >&2; exit 2 ;;
esac
# A release bundle is always built whole from source and split: what ships.
[[ $profile != release || ( $split == 1 && $update == 0 ) ]] || {
  echo '--no-split and --update are for dev and quick bundles' >&2; exit 2;
}
case "$bundle" in
  runtime) package=tilo-browser-runtime ;;
  desktop) package=tilo-web-desktop ;;
  *) echo "unsupported bundle: $bundle" >&2; exit 2 ;;
esac
# The splitter reads the linked module's symbol table, which `wasm-release`
# strips; `wasm-split` is that profile with the table kept.
bindgen_flags=()
if [[ $bundle == desktop ]]; then
  [[ $profile == release ]] && cargo_profile=wasm-split artifact=wasm-split
  bindgen_flags=(--keep-lld-exports --no-demangle)
fi
[[ "$out" == /* ]] || { echo 'OUTPUT_DIR must be absolute' >&2; exit 2; }
# Stale modules beside new ones would make the bundle's provenance ambiguous.
if [[ $update == 1 && -f "$out/build.json" ]]; then
  python3 -c 'import json,sys; b=json.load(open(sys.argv[1])); sys.exit(b.get("bundle")!=sys.argv[2] or b.get("profile")!=sys.argv[3])' \
    "$out/build.json" "$bundle" "$profile" || {
    echo "--update: $out holds another bundle or profile; build into an empty directory" >&2; exit 2;
  }
else
  [[ ! -d "$out" || -z "$(ls -A "$out")" ]] || { echo 'OUTPUT_DIR must be empty (or pass --update)' >&2; exit 2; }
  update=0
fi
# Whether --update keeps this output of an earlier run.
kept() { [[ $update == 1 && -e "$out/$1" ]]; }

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
# A development bundle may be built from a tree without its git metadata (a
# build container's synced copy); it records an unknown revision. A release
# bundle names the commit it was built from.
if [[ $profile != release ]] && ! git rev-parse HEAD >/dev/null 2>&1; then
  git() { [[ $1 == rev-parse ]] && echo unknown; return 0; }
fi
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
linked="$target_dir/wasm32-unknown-unknown/$artifact/${package//-/_}.wasm"
# The linked module this pkg/ was made from, and how; empty when there is
# no module to read (a wasm-bindgen that makes its output itself), so
# such a pkg/ is never reused.
made_from=""
# Size and modification time, with GNU stat or the BSD stat of macOS.
file_stamp() { stat -c '%s %Y' "$1" 2>/dev/null || stat -f '%z %m' "$1"; }
if [[ -f "$linked" ]]; then made_from="$(file_stamp "$linked") split=$split"; fi
if [[ -n "$made_from" ]] && kept pkg/.made-from && [[ "$(cat "$out/pkg/.made-from")" == "$made_from" ]]; then
  echo "pkg: unchanged module, kept" >&2
else
  # Made beside the old pkg/ and swapped in, so a page reloading meanwhile
  # gets one or the other; the compute module's pkg/compute moves over.
  next="$out/pkg.next"
  rm -rf "$next"
  mkdir -p "$next"
  "$bindgen" "$linked" --target web --out-dir "$next" --out-name tilo_browser_runtime "${bindgen_flags[@]}"
  if [[ $bundle == desktop && $split == 1 ]]; then
    bash "$repo/tools/web/wasm-split/split.sh" "$linked" "$next"
  elif [[ $bundle == desktop ]]; then
    bash "$repo/tools/web/wasm-split/split.sh" --unsplit "$next"
  fi
  if [[ -d "$out/pkg/compute" ]]; then mv "$out/pkg/compute" "$next/compute"; fi
  # A release bundle ships exactly what it was built from; no stamp.
  if [[ $profile != release && -n "$made_from" ]]; then printf '%s\n' "$made_from" > "$next/.made-from"; fi
  rm -rf "$out/pkg"
  mv "$next" "$out/pkg"
fi
if [[ $bundle == desktop ]]; then
  cp "$repo/crates/browser/tilo-web-desktop/apps.json" "$out/apps.json"
fi
if [[ $bundle == desktop ]] && ! kept data/typst; then
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
# The split modules keep the name section the splitter read; it is dropped
# here with the rest of the debug information.
wasm_opt_flags="-Os --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext --enable-mutable-globals --enable-reference-types --enable-multivalue --strip-debug --strip-producers"
if [[ $optimize == 1 ]]; then
  command -v wasm-opt >/dev/null || { echo 'wasm-opt (binaryen) is required; pass --no-wasm-opt to skip' >&2; exit 2; }
  # A few at a time: the start module alone takes gigabytes to optimize.
  # shellcheck disable=SC2086
  printf '%s\0' "$out"/pkg/*.wasm | xargs -0 -P 4 -I {} wasm-opt $wasm_opt_flags {} -o {}
else
  wasm_opt_flags=
fi
compute_opt=--no-wasm-opt
[[ $optimize == 1 ]] && compute_opt=--wasm-opt
compute_profile=$profile
[[ $profile == quick ]] && compute_profile=dev
if ! kept pkg/compute; then
  bash "$repo/crates/browser/tilo-browser-compute/scripts/build.sh" "$out" --profile "$compute_profile" "$compute_opt"
fi
moq_kept=0
kept media-moq-library.mjs && moq_kept=1
find "$crate/web" -maxdepth 1 -name '*.mjs' ! -name '*.test.mjs' ! -name media-moq-library.mjs -exec cp {} "$out/" \;
cp "$crate/web/index.html" "$out/index.html"
mkdir -p "$out/locales"
cp "$crate"/web/locales/*.ftl "$out/locales/"
mkdir -p "$out/app"
cp "$crate/web/app/index.html" "$out/app/index.html"
# The live plane's MoQ library is bundled from the published @moq/net pinned
# in scripts/moq-browser/package-lock.json, never committed, never from a
# local development copy in web/. npm ci writes node_modules beside the lock
# file: TILO_MOQ_BROWSER_DIR names a writable copy of scripts/moq-browser for
# builds whose source tree is read-only (t's checks).
if [[ $moq_kept == 0 ]]; then
  (cd "${TILO_MOQ_BROWSER_DIR:-$repo/scripts/moq-browser}" && npm ci --no-audit --no-fund --silent && node build.mjs "$out/media-moq-library.mjs")
fi

kept fonts.json || python3 - "$repo" "$out" <<'PY'
import importlib.util, json, pathlib, re, shutil, sys
repo, out = map(pathlib.Path, sys.argv[1:])
tool = repo / 'tools/font-catalog.py'
spec = importlib.util.spec_from_file_location('font_catalog', tool)
catalog_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(catalog_module)
source = catalog_module.prepared_root(repo / 'crates/sdk/ui-core/assets/fonts', 'product')
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
shutil.copy2(source / 'PROVENANCE.md', out / 'fonts/PROVENANCE.md')
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
metadata = dict(revision=sys.argv[2], dirty=bool(sys.argv[3]), profile=sys.argv[4], builtFromSource=True, rustc=sys.argv[5],
                wasmBindgen=sys.argv[6], wasmOpt=sys.argv[7] == '1', target='wasm32-unknown-unknown',
                cargoProfile=sys.argv[8], bundle=sys.argv[9])
if metadata['wasmOpt']:
    metadata.update(wasmOptVersion=sys.argv[10], wasmOptFlags=sys.argv[11])
# Never this file itself: an --update run finds the previous one in place.
metadata['files'] = {str(p.relative_to(out)): hashlib.sha256(p.read_bytes()).hexdigest()
                     for p in sorted(out.rglob('*')) if p.is_file() and p != out / 'build.json'}
(out / 'build.json').write_text(json.dumps(metadata, indent=2) + '\n')
PY
echo "bundle: $out ($(du -sh "$out/pkg/tilo_browser_runtime_bg.wasm" | cut -f1) wasm, $profile)"
