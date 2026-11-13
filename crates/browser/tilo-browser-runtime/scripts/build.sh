#!/usr/bin/env bash
# Build the browser runtime bundle into an empty directory:
#   pkg/                wasm-bindgen output (--target web)
#   *.mjs, index.html   page host and the desktop entry page
#   app/index.html      the full-tab app entry page (web-routing.md)
#   apple-touch-icon.png  the entry pages' home-screen icon (Safari takes no SVG)
#   locales/            the entry pages' arrival lines (web/arrival.mjs)
#   fonts/, fonts.json  canonical product fonts, their manifest and licences
#   data/, data.json    application data fetched on request (desktop bundle:
#                       the Typst editor's fonts), with sizes and hashes
#   build.json          revision, tools, profile and output hashes
#
# usage: build.sh OUTPUT_DIR [--profile release|assert|dev|quick] [--[no-]wasm-opt] [--names]
#                 [--bundle runtime|desktop] [--no-split] [--update]
#                 [--wasm-only | --wasm-from WASM_DIR] [--wasm-steps auto|builder|local]
# `--profile release` compiles with Cargo's `wasm-release` profile and runs
# wasm-opt (binaryen) unless `--no-wasm-opt`; `dev` skips it unless `--wasm-opt`.
# `assert` is release optimisation with the assertions and overflow checks a
# dev build keeps (`wasm-assert`): qualification and performance measurement
# run on it, so a faster bundle is never one that stopped checking. `quick`
# is the development loop's: Cargo's `wasm-dev` profile (no debug
# information, light optimisation), whose much smaller module wasm-bindgen
# and the splitter get through in half the time, and no wasm-opt.
# `--bundle desktop` builds crates/browser/tilo-web-desktop instead: the same
# runtime with every web-capable app registered from its manifest, under the
# runtime's module name so the page host loads it unchanged, plus apps.json;
# its module is split (tools/web/wasm-split/split.sh) into a start module and
# one module per app, fetched when the app first opens. `--no-split` (dev and
# quick only) leaves it whole, for a loop that is not about loading apps.
# `--bundle app` builds an app's own bundle: the runtime with that one app
# linked and registered, from the crate `tilo-app build --platform web`
# generates for it (`--manifest-path`, built with the target directory named by
# $CARGO_TARGET_DIR), carrying the app's `apps.json` (`--apps-json`, which also
# names the app `build.json` records). It is not split and has no application
# data. `tilo-app` builds every web bundle through this script, so an app's
# bundle and the site's are one build, and a workspace that defines no
# `wasm-dev`/`wasm-release` profile gets Cargo's own `dev`/`release`.
# `--update` (dev and quick only; also with `--module-only` and `--wasm-only`) brings an OUTPUT_DIR this script built
# before up to date, step by step: the module is bound and split again only
# when the linked module changed, the page's modules and entry pages are
# copied again, and fonts, data and the compute module are kept. A loop that
# edits only web/*.mjs takes seconds; one that edits Rust,
# the compile, wasm-bindgen and, unless `--no-split`, the split.
# `--names` (desktop bundle) keeps the modules' function names (their wasm
# name section), which a shipped bundle strips: a profile of the page then
# names the functions it spends its time in rather than numbering them. It is
# otherwise the same build, compiled, split and optimized alike, so it
# measures what ships; build.json records the wasm-opt flags it ran with.
# `--wasm-only` builds only what Cargo compiles (pkg/ with the compute module,
# data/ and apps.json) and records how in wasm.json; it reads no git
# metadata, so a build cache can key it by its sources alone. `--module-only`
# builds pkg/ and nothing else: a test that drives the runtime's exports from
# a page serves it beside the page's modules, with no fonts, data or compute
# module a browser test does not mount. `--wasm-from`
# assembles the bundle around such a directory instead of compiling: the
# pages, locales and fonts from this tree, and build.json for this tree's
# revision. A change to the pages alone then compiles nothing.
# Needs the wasm32-unknown-unknown target and a wasm-bindgen CLI matching the
# crate's pinned wasm-bindgen. Compiles with `cargo`, or with the cargo `$CARGO`
# names: a cargo that compiles elsewhere brings the module back to this tree's
# target directory, and the script reads it there as it always does.
# `--wasm-steps` says where the steps that turn the linked module into pkg/
# run: wasm-bindgen wants several gigabytes and wasm-opt over one module
# alone wants more, which is the build's memory, not the compile's. `auto`
# (the default, or $TILO_BUNDLE_WASM_STEPS) runs them on the builder when
# `$CARGO` is a drop-in that runs a program there and brings a directory of
# the target dir back (`rcargo exec`), and beside the compile otherwise;
# `builder` refuses to run them here, `local` never sends them away. The
# fetched directory holds what a local run writes — pkg/, data/, apps.json
# and wasm.json — so a builder build and a local one record the same
# build.json, down to the tools that ran on the builder.
set -euo pipefail

crate=$(cd "$(dirname "$0")/.." && pwd)
repo=$(cd "$crate/../../.." && pwd)
usage='usage: build.sh OUTPUT_DIR [--profile release|assert|dev|quick] [--[no-]wasm-opt] [--names] [--bundle runtime|desktop|app] [--manifest-path FILE --apps-json FILE] [--no-split] [--update] [--wasm-only | --wasm-from WASM_DIR] [--module-only] [--wasm-steps auto|builder|local]'
out=${1:?$usage}
shift
profile=release
optimize=
bundle=runtime
split=1
update=0
wasm_only=0
module_only=0
wasm_from=
names=0
manifest=
apps_json=
app_id=
steps=${TILO_BUNDLE_WASM_STEPS:-auto}
while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile) profile=${2:?--profile needs a value}; shift 2 ;;
    --wasm-opt) optimize=1; shift ;;
    --no-wasm-opt) optimize=0; shift ;;
    --bundle) bundle=${2:?--bundle needs a value}; shift 2 ;;
    --manifest-path) manifest=${2:?--manifest-path needs a file}; shift 2 ;;
    --apps-json) apps_json=${2:?--apps-json needs a file}; shift 2 ;;
    --no-split) split=0; shift ;;
    --names) names=1; shift ;;
    --update) update=1; shift ;;
    --wasm-only) wasm_only=1; shift ;;
    --module-only) module_only=1; shift ;;
    --wasm-from) wasm_from=${2:?--wasm-from needs a directory}; shift 2 ;;
    --wasm-steps) steps=${2:?--wasm-steps needs a value}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; echo "$usage" >&2; exit 2 ;;
  esac
done
case "$steps" in
  auto|builder|local) ;;
  *) echo "unsupported --wasm-steps: $steps" >&2; exit 2 ;;
esac
# The workspace whose profiles the build compiles with: this repository's, or
# the generated app crate's (`--bundle app`), which carries its app workspace's.
profiles_manifest=$repo/Cargo.toml
[[ -z $manifest ]] || profiles_manifest=$manifest
defines_profile() { grep -q "^\[profile\.$1\]" "$profiles_manifest" 2>/dev/null; }
# `wasm-dev` and `wasm-release` are this repository's (light iteration, small
# shipping): a workspace without them, as an app's own may be, gets Cargo's
# plain profiles for the same two things.
case "$profile" in
  release) if defines_profile wasm-release; then cargo_profile=wasm-release; else cargo_profile=release; fi; optimize=${optimize:-1} ;;
  assert) defines_profile wasm-assert || {
    echo "--profile assert needs a workspace that declares [profile.wasm-assert], as this repository's does" >&2; exit 2; }
    cargo_profile=wasm-assert optimize=${optimize:-1} ;;
  dev) cargo_profile=dev optimize=${optimize:-0} ;;
  quick) if defines_profile wasm-dev; then cargo_profile=wasm-dev; else cargo_profile=dev; fi; optimize=${optimize:-0} ;;
  *) echo "unsupported profile: $profile" >&2; exit 2 ;;
esac
# A release or qualification bundle is always built whole from source and
# split: what ships, and what is measured.
[[ $profile != release && $profile != assert || ( $split == 1 && $update == 0 ) ]] || {
  echo '--no-split and --update are for dev and quick bundles' >&2; exit 2;
}
[[ $wasm_only == 0 || -z $wasm_from ]] || { echo '--wasm-only and --wasm-from exclude each other' >&2; exit 2; }
[[ $module_only == 0 || ( $wasm_only == 0 && -z $wasm_from ) ]] || {
  echo '--module-only builds nothing but pkg/; not with --wasm-only or --wasm-from' >&2; exit 2;
}
[[ $update == 0 || -z $wasm_from ]] || {
  echo '--update builds what it compiles; not with --wasm-from' >&2; exit 2;
}
# The runtime bundle's profile strips the symbols at link time; the desktop's
# keeps them for its splitter.
[[ $names == 0 || $bundle == desktop ]] || { echo '--names is for the desktop bundle' >&2; exit 2; }
case "$bundle" in
  runtime) package=tilo-browser-runtime ;;
  desktop) package=tilo-web-desktop ;;
  app) ;;
  *) echo "unsupported bundle: $bundle" >&2; exit 2 ;;
esac
# An app bundle is the one crate `--manifest-path` names, with its `apps.json`;
# the bundles of this repository's own crates take neither.
if [[ $bundle == app ]]; then
  [[ -n $manifest && -n $apps_json ]] || {
    echo '--bundle app builds the crate --manifest-path names, with the --apps-json to carry' >&2; exit 2;
  }
  eval "$(python3 - "$manifest" "$apps_json" <<'PY'
import json, shlex, sys, tomllib
manifest, apps = sys.argv[1:]
package = tomllib.load(open(manifest, 'rb'))['package']['name']
app = json.load(open(apps))['applications']
assert len(app) == 1, f"{apps} lists {len(app)} applications; an app bundle carries one"
print('package=' + shlex.quote(package))
print('app_id=' + shlex.quote(app[0]['id']))
PY
)"
else
  [[ -z $manifest && -z $apps_json ]] || {
    echo '--manifest-path and --apps-json are for --bundle app' >&2; exit 2;
  }
fi
# The splitter reads the linked module's symbol table, which `wasm-release`
# and `wasm-assert` strip; `wasm-split` and `wasm-assert-split` are those
# profiles with the table kept.
bindgen_flags=()
if [[ $bundle == desktop ]]; then
  case $profile in
    release) cargo_profile=wasm-split ;;
    assert) cargo_profile=wasm-assert-split ;;
  esac
  bindgen_flags=(--keep-lld-exports --no-demangle)
fi
# Cargo writes a profile's output under the profile's own name, but dev's
# under `debug`.
artifact=$cargo_profile
[[ $artifact != dev ]] || artifact=debug
[[ "$out" == /* ]] || { echo 'OUTPUT_DIR must be absolute' >&2; exit 2; }
# Stale modules beside new ones would make the bundle's provenance ambiguous.
# What says an output is this script's: build.json, or wasm.json for --wasm-only.
record=build.json
[[ $wasm_only == 0 ]] || record=wasm.json
# A module-only output is pkg/ alone (pkg.next is what a stopped build left,
# which it replaces), and its stamp names the linked module
# (and so the profile) it was made from.
if [[ $update == 1 && $module_only == 1 && -d "$out" && -z "$(ls -A "$out" | grep -vxE 'pkg|pkg\.next')" ]]; then
  :
elif [[ $update == 1 && -f "$out/$record" ]]; then
  python3 -c 'import json,sys; b=json.load(open(sys.argv[1])); sys.exit(b.get("bundle")!=sys.argv[2] or b.get("profile")!=sys.argv[3])' \
    "$out/$record" "$bundle" "$profile" || {
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
cargo=${CARGO:-cargo}
# Whether this cargo both compiles elsewhere and runs a program there,
# bringing a directory of the target dir back (`rcargo exec`): asked of the
# cargo itself, whose help for exec names `--fetch`, so a wrapper around one
# (os/containers/tilo-sh/cargo.sh) counts, and a local cargo answers `no such
# command`. The steps that turn the linked module into pkg/ then go with the
# compile.
runs_on_builder() {
  local help
  help=$("$cargo" exec --help 2>/dev/null) && [[ $help == *--fetch* ]]
}
if [[ $steps == auto ]]; then
  # An app bundle's crate is this tree's own, not the builder's.
  if [[ -z $wasm_from && -z $manifest && $module_only == 0 ]] && runs_on_builder; then steps=builder; else steps=local; fi
elif [[ $steps == builder ]]; then
  runs_on_builder || {
    echo "--wasm-steps builder needs a cargo that runs a program on its builder (\`rcargo exec\`), and CARGO ($cargo) has no exec" >&2
    exit 2
  }
  [[ -z $wasm_from && -z $manifest && $module_only == 0 ]] || {
    echo '--wasm-steps builder is for the module and the bundle; not with --module-only, --wasm-from or a --manifest-path' >&2
    exit 2
  }
fi
if [[ -z $wasm_from && $steps == local ]]; then
  pinned=$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' "$crate/Cargo.toml")
  [[ "$("$bindgen" --version)" == "wasm-bindgen $pinned" ]] || {
    echo "wasm-bindgen CLI must be $pinned to match the crate" >&2
    echo "  help: cargo install --locked wasm-bindgen-cli --version $pinned, and rustup target add wasm32-unknown-unknown" >&2
    exit 2
  }
fi
if [[ $wasm_only == 0 ]]; then
  # A bundle may be built from a tree without its git metadata (a build
  # container's synced copy, a checkout a measurement was handed): it records
  # the revision `$TILO_SOURCE_REVISION` names when a build farm names one,
  # else an unknown one, and has no source status to compare at the end.
  have_git=0
  git rev-parse HEAD >/dev/null 2>&1 && have_git=1
  if [[ $have_git == 1 ]]; then
    revision=$(git rev-parse HEAD)
    source_status=$(git status --porcelain)
  else
    revision=${TILO_SOURCE_REVISION:-unknown}
    source_status=
  fi
fi

# What Cargo compiles: pkg/ (the runtime's module, split for the desktop, and
# the compute module), data/ and apps.json, and the tools that made them.
build_wasm() {
if [[ $bundle == desktop ]]; then
  python3 tools/web/generate_web_desktop.py --check
fi
# An app bundle compiles the crate `tilo-app` generated for it: a workspace of
# its own, whose lock file cargo writes, so it is named rather than `-p`, and
# built without `--locked` and without `--offline`.
local -a compile metadata
if [[ -n $manifest ]]; then
  compile=(build --manifest-path "$manifest" --lib --target wasm32-unknown-unknown --profile "$cargo_profile")
  metadata=(metadata --format-version 1 --no-deps --manifest-path "$manifest")
else
  compile=(build --locked -p "$package" --lib --target wasm32-unknown-unknown --profile "$cargo_profile")
  metadata=(metadata --format-version 1 --no-deps --offline)
fi
"$cargo" "${compile[@]}"
# Read Cargo's artifact location; never redirect it.
target_dir=$("$cargo" "${metadata[@]}" |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
mkdir -p "$out/pkg"
linked="$target_dir/wasm32-unknown-unknown/$artifact/${package//-/_}.wasm"
# The linked module this pkg/ was made from, and how; empty when there is
# no module to read (a wasm-bindgen that makes its output itself), so
# such a pkg/ is never reused.
made_from=""
# Its content, not its modification time: a cargo that compiles elsewhere (t,
# rcargo) writes the module it brings back anew on every build, changed or not,
# and a new time alone would bind and split it again (minutes) for nothing.
file_stamp() { { sha256sum "$1" 2>/dev/null || shasum -a 256 "$1"; } | cut -d' ' -f1; }
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
  # A release or qualification bundle ships exactly what it was built from; no
  # stamp.
  if [[ $profile != release && $profile != assert && -n "$made_from" ]]; then printf '%s\n' "$made_from" > "$next/.made-from"; fi
  rm -rf "$out/pkg"
  mv "$next" "$out/pkg"
fi
if [[ -n $apps_json ]]; then
  cp "$apps_json" "$out/apps.json"
elif [[ $bundle == desktop ]]; then
  cp "$repo/crates/browser/tilo-web-desktop/apps.json" "$out/apps.json"
fi
if [[ $bundle == desktop ]] && ! kept data/typst; then
  # The Typst editor's fonts, which only native builds embed; the editor asks
  # for them by name when it opens (crates/platform/tilo-typst-fonts).
  # The sources the build above compiled typst-assets from, as its dep-info
  # names them. Not `cargo metadata`: that reads every package the lockfile
  # names, for every platform, so offline it fails on any the build never
  # fetched.
  typst_assets=$(python3 "$repo/tools/web/crate_source.py" typst-assets \
    "$target_dir/wasm32-unknown-unknown/$artifact" "$repo/Cargo.lock")
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
# What the app SDK's own web build passes wasm-opt, so the module a size gate
# measures is the module the site serves (tools/web/wasm-opt-flags).
wasm_opt_flags=$(grep -v '^[[:space:]]*#' "$repo/tools/web/wasm-opt-flags" | xargs)
# With --names, the same passes, writing the name section the splitter keeps
# (-g: binaryen writes no names without it) instead of dropping it.
if [[ $names == 1 ]]; then
  wasm_opt_flags="$(printf '%s\n' $wasm_opt_flags | grep -vx -- --strip-debug | xargs) -g"
fi
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
if [[ $module_only == 0 ]] && ! kept pkg/compute; then
  # The same profile the page module is built in.
  bash "$repo/crates/browser/tilo-browser-compute/scripts/build.sh" "$out" --profile "$profile" "$compute_opt" --module-only
fi
rustc_version=$(rustc --version)
bindgen_version=$("$bindgen" --version)
wasm_opt_version=
if [[ $optimize == 1 ]]; then wasm_opt_version=$(wasm-opt --version); fi
}

# The same wasm phase, on the builder the cargo compiles on, and the directory
# of this tree's target dir the fetch brings its output back in. One directory
# per bundle, profile and options, replaced by each build there: wasm-bindgen
# writes several gigabytes and wasm-opt over the start module alone more,
# which is memory this machine does not spend when it is not the builder.
wasm_on_builder() {
  local stage=web-bundle-wasm-$bundle-$profile args=(--bundle "$bundle" --profile "$profile")
  local script=${crate#"$repo"/}/scripts/build-wasm-on-builder.sh
  if [[ $names == 1 ]]; then stage+=-names; args+=(--names); fi
  if [[ $optimize == 1 ]]; then args+=(--wasm-opt); else stage+=-no-wasm-opt; args+=(--no-wasm-opt); fi
  if [[ $split == 0 ]]; then stage+=-no-split; args+=(--no-split); fi
  # An --update loop keeps the builder's previous output too, so a module that
  # did not change is not bound and split again there.
  if [[ $update == 1 ]]; then args+=(--update); fi
  # Named in what this run fetches, so a directory an earlier run left here
  # is never taken for this one's.
  local run
  run=$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')
  echo "wasm: binding, splitting and optimizing on the builder ($cargo exec; $stage)" >&2
  "$cargo" --fetch "$stage" exec bash "$script" "$stage" "$run" "${args[@]}" >&2 || {
    echo 'the wasm phase failed on the builder (above)' >&2; exit 1;
  }
  target_dir=$("$cargo" metadata --format-version 1 --no-deps --offline |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
  local dir=$target_dir/$stage file missing=()
  # What a build of it writes: a directory without these is a fetch that
  # brought too little, never a bundle, and says which file it looked for.
  for file in wasm.json pkg/tilo_browser_runtime.js pkg/tilo_browser_runtime_bg.wasm; do
    [[ -s "$dir/$file" ]] || missing+=("$file")
  done
  [[ ${#missing[@]} == 0 ]] || {
    echo "the builder build left no ${missing[*]} in $dir (fetched from the builder's $stage)" >&2
    exit 1
  }
  [[ -f $dir/run && "$(<"$dir/run")" == "$run" ]] || {
    echo "$dir is not what this build made on the builder (its run is not $run): did the fetch bring it back?" >&2
    exit 1
  }
  printf '%s\n' "$dir"
}

if [[ -z $wasm_from && $steps == builder ]]; then
  wasm_from=$(wasm_on_builder)
fi
if [[ -z $wasm_from ]]; then
  build_wasm
else
  wasm_from=$(cd "$wasm_from" && pwd)
  # How the module was built, which build.json records, and that it is the
  # bundle and profile asked for.
  read_wasm=$(python3 - "$wasm_from/wasm.json" "$bundle" "$profile" <<'PY'
import json, shlex, sys
w = json.load(open(sys.argv[1]))
if (w['bundle'], w['profile']) != (sys.argv[2], sys.argv[3]):
    sys.exit(f"--wasm-from: {sys.argv[1]} is a {w['profile']} {w['bundle']} module, not {sys.argv[3]} {sys.argv[2]}")
for name, key in [('rustc_version', 'rustc'), ('bindgen_version', 'wasmBindgen'), ('cargo_profile', 'cargoProfile'),
                  ('wasm_opt_version', 'wasmOptVersion'), ('wasm_opt_flags', 'wasmOptFlags')]:
    print(f'{name}={shlex.quote(w.get(key, ""))}')
print(f"optimize={1 if w['wasmOpt'] else 0}")
PY
  ) || exit 2
  eval "$read_wasm"
  mkdir -p "$out"
  # pkg/ is swapped, never copied into: an output this script built before
  # holds one already, and a copy beside it is a bundle with two.
  if [[ $update == 1 && -f $wasm_from/pkg/.made-from && -f $out/pkg/.made-from ]] &&
     [[ "$(<"$wasm_from/pkg/.made-from")" == "$(<"$out/pkg/.made-from")" ]]; then
    echo 'pkg: unchanged module, kept' >&2
  else
    rm -rf "$out/pkg"
    cp -R "$wasm_from/pkg" "$out/pkg"
  fi
  # What the module was compiled from can be unchanged while these are not
  # (a manifest that names apps), so they follow the module and not --update.
  if [[ -d $wasm_from/data ]]; then
    rm -rf "$out/data"
    cp -R "$wasm_from/data" "$out/data"
  fi
  [[ ! -f $wasm_from/apps.json ]] || cp "$wasm_from/apps.json" "$out/apps.json"
fi
# The module the page loads first, whole: a pkg/ without it is a page that
# never starts, so the build stops here rather than reporting a bundle. It goes
# missing when something else writes into OUTPUT_DIR while this build runs.
for file in tilo_browser_runtime.js tilo_browser_runtime_bg.wasm; do
  [[ -s "$out/pkg/$file" ]] || {
    echo "$out/pkg has no $file: did another build write into $out meanwhile?" >&2; exit 1;
  }
done
if [[ $module_only == 1 ]]; then
  # Only what Cargo compiles and wasm-bindgen writes: what a test that drives
  # the runtime's exports serves beside the page's modules.
  echo "module: $out/pkg"
  exit 0
fi
if [[ $wasm_only == 1 ]]; then
  python3 - "$out/wasm.json" "$bundle" "$profile" "$cargo_profile" "$rustc_version" "$bindgen_version" "$optimize" \
    "$wasm_opt_version" "$wasm_opt_flags" <<'PY'
import json, sys
path, bundle, profile, cargo_profile, rustc, bindgen, optimize, opt_version, opt_flags = sys.argv[1:]
wasm = dict(bundle=bundle, profile=profile, cargoProfile=cargo_profile, rustc=rustc, wasmBindgen=bindgen,
            wasmOpt=optimize == '1')
if wasm['wasmOpt']:
    wasm.update(wasmOptVersion=opt_version, wasmOptFlags=opt_flags)
open(path, 'w').write(json.dumps(wasm, indent=2) + '\n')
PY
  echo "wasm: $out ($profile $bundle)"
  exit 0
fi
bash "$repo/crates/browser/tilo-browser-compute/scripts/build.sh" "$out" --assets-only
mkdir -p "$out/fonts"
find "$crate/web" -maxdepth 1 -name '*.mjs' ! -name '*.test.mjs' -exec cp {} "$out/" \;
cp "$crate/web/index.html" "$out/index.html"
cp "$crate/web/sign-in.html" "$out/sign-in.html"
cp "$crate/web/apple-touch-icon.png" "$out/apple-touch-icon.png"
mkdir -p "$out/locales"
cp "$crate"/web/locales/*.ftl "$out/locales/"
mkdir -p "$out/app"
cp "$crate/web/app/index.html" "$out/app/index.html"
kept fonts.json || python3 - "$repo" "$out" <<'PY'
import importlib.util, json, os, pathlib, re, shutil, sys
repo, out = map(pathlib.Path, sys.argv[1:])
tool = repo / 'tools/font-catalog.py'
spec = importlib.util.spec_from_file_location('font_catalog', tool)
catalog_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(catalog_module)
source = catalog_module.prepared_root(repo / 'crates/sdk/ui-core/assets/fonts', 'product')
def install(src, dst):
    # A hard link where the file system allows one: the bundle shares the
    # cached product fonts, 40 MB it would otherwise copy for itself.
    try:
        os.link(src, dst)
    except OSError:
        shutil.copy2(src, dst)
text = (source / 'manifest.toml').read_text()
assets = []
for block in text.split('[[font]]')[1:]:
    block = block.split('[[license]]')[0]
    if re.search(r'^scope = "product"$', block, re.M):
        name = re.search(r'^file = "(.*?)"$', block, re.M)[1]
        install(source / name, out / 'fonts' / name)
        assets.append(dict(
            name=name,
            bytes=int(re.search(r'^bytes = (\d+)$', block, re.M)[1]),
            sha256=re.search(r'^sha256 = "([0-9a-f]{64})"$', block, re.M)[1],
            startup=bool(re.search(r'^browser_startup = true$', block, re.M)),
        ))
assert any(asset['startup'] for asset in assets), 'no browser startup fonts in the manifest'
shutil.copytree(source / 'LICENSES', out / 'fonts/LICENSES', dirs_exist_ok=True, copy_function=install)
install(source / 'manifest.toml', out / 'fonts/manifest.toml')
install(source / 'PROVENANCE.md', out / 'fonts/PROVENANCE.md')
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

# What the page has to resolve: every module the generated JavaScript imports. A bare name is
# one no page has to resolve — nothing outside a browser provides it — and a page has nothing to
# resolve it with: the entry stops at its first import and never reaches the desktop. The build
# refuses such a bundle here rather than shipping a page that cannot start.
python3 - "$out" <<'PY'
import pathlib, re, sys
out = pathlib.Path(sys.argv[1])
statement = re.compile(r'''^[ \t]*(?:import|export)[^;]*?\bfrom\s*["']([^"']+)["']'''
                       r'''|^[ \t]*(?:import|\}\s*from)\s*["']([^"']+)["']''')
bare = sorted({match[1] or match[2]
               for path in (out / 'pkg').glob('*.js')
               for line in path.read_text(errors='replace').splitlines()
               if (match := statement.match(line))
               if not (match[1] or match[2]).startswith(('.', '/'))})
if bare:
    sys.exit('the page cannot resolve the modules it imports: ' + ' '.join(bare))
PY

# Record the actual tools, profile and output hashes; never infer a prebuilt profile.
if [[ $have_git == 1 ]] &&
   { [[ "$(git rev-parse HEAD)" != "$revision" ]] || [[ "$(git status --porcelain)" != "$source_status" ]]; }; then
  echo 'Source changed during build; discard output' >&2; exit 1
fi
python3 - "$out" "$revision" "$source_status" "$profile" "$rustc_version" "$bindgen_version" "$optimize" \
  "$cargo_profile" "$bundle" "$wasm_opt_version" "$wasm_opt_flags" "$app_id" <<'PY'
import hashlib, json, pathlib, sys
out = pathlib.Path(sys.argv[1])
metadata = dict(revision=sys.argv[2], dirty=bool(sys.argv[3]), profile=sys.argv[4], builtFromSource=True, rustc=sys.argv[5],
                wasmBindgen=sys.argv[6], wasmOpt=sys.argv[7] == '1', target='wasm32-unknown-unknown',
                cargoProfile=sys.argv[8], bundle=sys.argv[9])
# An app's own bundle names the app it carries, as its page host and the
# registry read it.
if sys.argv[12]:
    metadata['app'] = sys.argv[12]
if metadata['wasmOpt']:
    metadata.update(wasmOptVersion=sys.argv[10], wasmOptFlags=sys.argv[11])
# Never this file itself: an --update run finds the previous one in place.
metadata['files'] = {str(p.relative_to(out)): hashlib.sha256(p.read_bytes()).hexdigest()
                     for p in sorted(out.rglob('*')) if p.is_file() and p != out / 'build.json'}
(out / 'build.json').write_text(json.dumps(metadata, indent=2) + '\n')
PY
echo "bundle: $out ($(du -sh "$out/pkg/tilo_browser_runtime_bg.wasm" | cut -f1) wasm, $profile)"
