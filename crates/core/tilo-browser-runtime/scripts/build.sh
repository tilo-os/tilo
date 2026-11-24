#!/usr/bin/env bash
# Build the browser runtime bundle into an empty directory:
#   pkg/                wasm-bindgen output (--target web)
#   *.mjs, index.html   the entry's pages and the modules they load
#   app/index.html      the full-tab app entry page (web-routing.md)
#   apple-touch-icon.png  the entry pages' home-screen icon (Safari takes no SVG)
#   manifest.json, icon.svg  the web app manifest that makes the site
#                       installable as an app window, and its icon
#   locales/            the entry pages' arrival lines (web/arrival.mjs)
#   build.json          revision, tools, profile and output hashes
#
# usage: build.sh OUTPUT_DIR [--profile release|assert|check|local|dev|quick] [--[no-]wasm-opt]
#                 [--no-split] [--update]
#                 [--wasm-only | --wasm-from WASM_DIR] [--wasm-steps auto|builder|local]
# `--profile release` compiles with Cargo's `wasm-release` profile and runs
# wasm-opt (binaryen) unless `--no-wasm-opt`; `dev` skips it unless `--wasm-opt`.
# `assert` is release optimisation with the assertions and overflow checks a
# dev build keeps (`wasm-assert`): qualification and performance measurement
# run on it, so a faster bundle is never one that stopped checking. `quick`
# is the development loop's: Cargo's `wasm-dev` profile (no debug
# information, light optimisation), whose much smaller module wasm-bindgen
# and the splitter get through in half the time, and no wasm-opt.
# `check` (a bundle built from source only) is `release` without its whole-program link
# (Cargo's `wasm-split-check`, as `lounge-check` is to the native release): the
# same opt-level, split and wasm-opt, so a change to one app compiles that app
# and relinks rather than running fat LTO over every app in one codegen unit.
# A check that reads the site's files builds it; what ships stays `release`.
# `local` (a bundle built from source only) is the local tilo.sh stack's
# (os/containers/tilo-sh/stack.sh): `check`'s code, compiled incrementally
# (Cargo's `wasm-split-local`) in a build directory that stays, split as
# `check` is, and not wasm-opt'd unless `--wasm-opt`. It takes `--update`, so
# a module that did not change is neither bound nor split again, a split
# keeps the modules whose content did not change (split.sh), and a change to
# bodies alone patches the previous bind instead of binding every body again
# (bind-patch.sh). A one-line app change then costs the compile, that patch
# and the split, not a minute of optimisation a page on this machine's
# loopback does not notice.
# The runtime bundle is the web entry's: the pages the site serves and the
# module a browser gets into a workspace with. No app is linked into it — a
# page's app is the server's, rendered as HTML.
# `--update` (dev, quick and local only; also with `--module-only` and `--wasm-only`) brings an OUTPUT_DIR this script built
# before up to date, step by step: the module is bound and split again only
# when the linked module changed, the page's modules and entry pages are
# copied again. A loop that
# edits only web/*.mjs takes seconds; one that edits Rust,
# the compile, wasm-bindgen and, unless `--no-split`, the split.
# `--wasm-only` builds only what Cargo compiles (pkg/) and records how in
# wasm.json; it reads no git
# metadata, so a build cache can key it by its sources alone. `--module-only`
# builds pkg/ and nothing else: a test that drives the runtime's exports from
# a page serves it beside the page's modules. `--wasm-from`
# assembles the bundle around such a directory instead of compiling: the
# pages and locales from this tree, and build.json for this tree's
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
# the target dir back (`$CARGO exec --fetch`), and beside the compile otherwise;
# `builder` refuses to run them here, `local` never sends them away. The
# fetched directory holds what a local run writes — pkg/ and wasm.json — so a builder build and a local one record the same
# build.json, down to the tools that ran on the builder.
set -euo pipefail

crate=$(cd "$(dirname "$0")/.." && pwd)
repo=$(cd "$crate/../../.." && pwd)
usage='usage: build.sh OUTPUT_DIR [--profile release|assert|check|local|dev|quick] [--[no-]wasm-opt] [--no-split] [--update] [--wasm-only | --wasm-from WASM_DIR] [--module-only] [--wasm-steps auto|builder|local]'
out=${1:?$usage}
shift
profile=release
optimize=
bundle=runtime
package=tilo-browser-runtime
split=1
update=0
wasm_only=0
module_only=0
wasm_from=
steps=${TILO_BUNDLE_WASM_STEPS:-auto}
while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile) profile=${2:?--profile needs a value}; shift 2 ;;
    --wasm-opt) optimize=1; shift ;;
    --no-wasm-opt) optimize=0; shift ;;
    --no-split) split=0; shift ;;
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
# The workspace whose profiles the build compiles with.
profiles_manifest=$repo/Cargo.toml
defines_profile() { grep -q "^\[profile\.$1\]" "$profiles_manifest" 2>/dev/null; }
# `wasm-dev` and `wasm-release` are this repository's (light iteration, small
# shipping): a workspace without them, as an app's own may be, gets Cargo's
# plain profiles for the same two things.
case "$profile" in
  release) if defines_profile wasm-release; then cargo_profile=wasm-release; else cargo_profile=release; fi; optimize=${optimize:-1} ;;
  assert) defines_profile wasm-assert || {
    echo "--profile assert needs a workspace that declares [profile.wasm-assert], as this repository's does" >&2; exit 2; }
    cargo_profile=wasm-assert optimize=${optimize:-1} ;;
  check) defines_profile wasm-split-check || {
    echo "--profile check needs a workspace that declares [profile.wasm-split-check], as this repository's does" >&2; exit 2; }
    cargo_profile=wasm-split-check optimize=${optimize:-1} ;;
  local) defines_profile wasm-split-local || {
    echo "--profile local needs a workspace that declares [profile.wasm-split-local], as this repository's does" >&2; exit 2; }
    cargo_profile=wasm-split-local optimize=${optimize:-0} ;;
  dev) cargo_profile=dev optimize=${optimize:-0} ;;
  quick) if defines_profile wasm-dev; then cargo_profile=wasm-dev; else cargo_profile=dev; fi; optimize=${optimize:-0} ;;
  *) echo "unsupported profile: $profile" >&2; exit 2 ;;
esac
# A release, qualification or check bundle is always built whole from source
# and split: what ships, and what is measured.
[[ $profile != release && $profile != assert && $profile != check || ( $split == 1 && $update == 0 ) ]] || {
  echo '--no-split and --update are for dev, quick and local bundles' >&2; exit 2;
}
[[ $wasm_only == 0 || -z $wasm_from ]] || { echo '--wasm-only and --wasm-from exclude each other' >&2; exit 2; }
[[ $module_only == 0 || ( $wasm_only == 0 && -z $wasm_from ) ]] || {
  echo '--module-only builds nothing but pkg/; not with --wasm-only or --wasm-from' >&2; exit 2;
}
# --update with --wasm-from keeps pkg/ when the directory's was made from the
# same linked module (its .made-from), as a build on the builder does: the
# caller ran that build itself (os/containers/tilo-sh/stack.sh) and hands its
# output over.
# The runtime bundle is the web entry's: the pages the site serves, and the
# module that gets a browser into a workspace. The apps' own code is not in
# it — the page asks the server for them.
[[ $profile != check && $profile != local ]] || { echo "--profile $profile is for a bundle built from source" >&2; exit 2; }
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
  export CC_wasm32_unknown_unknown="$repo/crates/core/tilo-server/scripts/wasm-cc.sh"
  export AR_wasm32_unknown_unknown="$repo/crates/core/tilo-server/scripts/wasm-ar.sh"
fi
bindgen=${WASM_BINDGEN:-wasm-bindgen}
# Each step's time, on stderr: what a slow bundle spent its minutes on.
took() { echo "build.sh: $1 in $((SECONDS - $2))s" >&2; }
cargo=${CARGO:-cargo}
# Whether this cargo both compiles elsewhere and runs a program there,
# bringing a directory of the target dir back (`$CARGO exec --fetch`): asked of
# the cargo itself, whose help for exec names `--fetch`, so a wrapper around
# one (os/containers/tilo-sh/cargo.sh) counts, and a local cargo answers `no
# such command`. The steps that turn the linked module into pkg/ then go with
# the compile.
runs_on_builder() {
  local help
  help=$("$cargo" exec --help 2>/dev/null) && [[ $help == *--fetch* ]]
}
if [[ $steps == auto ]]; then
  if [[ -z $wasm_from && $module_only == 0 ]] && runs_on_builder; then steps=builder; else steps=local; fi
elif [[ $steps == builder ]]; then
  runs_on_builder || {
    echo "--wasm-steps builder needs a cargo that runs a program on its builder (\`$CARGO exec --fetch\`), and CARGO ($cargo) has no exec" >&2
    exit 2
  }
  [[ -z $wasm_from && $module_only == 0 ]] || {
    echo '--wasm-steps builder is for the module and the bundle; not with --module-only or --wasm-from' >&2
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

# What Cargo compiles: pkg/ (the runtime's module) and
# the tools that made them.
build_wasm() {
local -a compile=(build --locked -p "$package" --lib --target wasm32-unknown-unknown --profile "$cargo_profile")
# The splitter reads the module's symbol table, and CI strips `dev`'s, which `wasm-dev` inherits
# (`CARGO_PROFILE_DEV_STRIP` in t.toml). The runtime keeps the strip each picks without it: none
# for `dev`, whose debug information is line tables, debuginfo for `wasm-dev`, which has none.
case $cargo_profile in
  dev) compile+=(--config "profile.dev.package.$package.strip=\"none\"") ;;
  wasm-dev) compile+=(--config "profile.wasm-dev.package.$package.strip=\"debuginfo\"") ;;
esac
local -a metadata=(metadata --format-version 1 --no-deps --offline)
local began=$SECONDS
"$cargo" "${compile[@]}"
took "compile ($cargo_profile)" "$began"
# Read Cargo's artifact location; never redirect it. A build told its directory by an absolute
# `CARGO_TARGET_DIR` (a cargo drop-in may) already knows it: asking cargo loads the whole
# workspace's manifests again, beside the compile it just ran.
target_dir=${CARGO_TARGET_DIR:-}
if [[ $target_dir != /* ]]; then
  target_dir=$("$cargo" "${metadata[@]}" |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
fi
mkdir -p "$out/pkg"
linked="$target_dir/wasm32-unknown-unknown/$artifact/${package//-/_}.wasm"
# The linked module this pkg/ was made from, and how; empty when there is
# no module to read (a wasm-bindgen that makes its output itself), so
# such a pkg/ is never reused.
made_from=""
# Its content, not its modification time: a cargo that compiles elsewhere (t,
# or any drop-in that builds on a builder) writes the module it brings back
# anew on every build, changed or not, and a new time alone would bind and
# split it again (minutes) for nothing.
file_stamp() { { sha256sum "$1" 2>/dev/null || shasum -a 256 "$1"; } | cut -d' ' -f1; }
if [[ -f "$linked" ]]; then made_from="$(file_stamp "$linked") split=$split"; fi
if [[ -n "$made_from" ]] && kept pkg/.made-from && [[ "$(cat "$out/pkg/.made-from")" == "$made_from" ]]; then
  echo "pkg: unchanged module, kept" >&2
else
  # Made beside the old pkg/ and swapped in, so a page reloading meanwhile
  # gets one or the other.
  next="$out/pkg.next"
  rm -rf "$next"
  mkdir -p "$next"
  began=$SECONDS
  # The local bundle binds the linked module without its linking and
  # relocation sections, a third of it: the splitter reads them from the
  # linked module itself, and binding them only copied them through, into
  # every module the splitter parses. The split comes out byte for byte the
  # same.
  local bind_input=$linked
  if [[ $profile == local ]]; then
    bind_input=$next/.bind-input.wasm
    python3 "$repo/tools/web/wasm-split/without-link-sections.py" "$linked" "$bind_input"
  fi
  # A local bundle's change to one body patches the modules wasm-bindgen made
  # the last time (bind-patch.sh, tools/web/wasm-split/src/bin/bind_patch.rs)
  # instead of binding every body again: the patch translates the body that
  # changed with what the bind recorded of the module's index spaces, and
  # writes exactly what a full bind would have. It asks for a full bind when
  # anything but bodies changed, and the map it keeps is refreshed after one.
  # The bound modules and the map live with the build directory's split
  # cache, not in pkg/: they are large, and the fetch back to this machine
  # does not carry them.
  local bind_cache=$target_dir/wasm-split-cache/$bundle-$profile-bind
  local bounded=0
  if [[ $profile == local && $update == 1 ]]; then
    if bash "$repo/tools/web/wasm-split/bind-patch.sh" apply "$bind_input" "$bind_cache" "$next/tilo_browser_runtime_bg.wasm"; then
      bounded=1
      took 'wasm-bindgen (patched)' "$began"
    fi
  fi
  if [[ $bounded == 0 ]]; then
    # A few threads each: on a build server of hundreds of cores, wasm-bindgen
    # and the splitter on every core spent minutes of system time contending
    # for memory (wasm-bindgen 203 s on all of them, 32 s on eight), for
    # byte-identical output. Sixteen bind the desktop bundle in about four
    # seconds fewer than eight (17.9 s against 22.3 s over a 264 MB module);
    # past that the contention the first line describes comes back (19.4 s on
    # twenty-four).
    RAYON_NUM_THREADS=${RAYON_NUM_THREADS:-16} \
      "$bindgen" "$bind_input" --target web --out-dir "$next" --out-name tilo_browser_runtime
    took wasm-bindgen "$began"
    if [[ $profile == local && $update == 1 ]]; then
      bash "$repo/tools/web/wasm-split/bind-patch.sh" record "$bind_input" \
        "$next/tilo_browser_runtime_bg.wasm" "$bind_cache" >&2 || true
    fi
  fi
  [[ $bind_input == "$linked" ]] || rm -f "$bind_input"
  # A release or qualification bundle ships exactly what it was built from; no
  # stamp.
  if [[ $profile != release && $profile != assert && $profile != check && -n "$made_from" ]]; then printf '%s\n' "$made_from" > "$next/.made-from"; fi
  rm -rf "$out/pkg"
  mv "$next" "$out/pkg"
fi
# What the app SDK's own web build passes wasm-opt, so the module a size check
# measures is the module the site serves (tools/web/wasm-opt-flags).
wasm_opt_flags=$(grep -v '^[[:space:]]*#' "$repo/tools/web/wasm-opt-flags" | xargs)
if [[ $optimize == 1 ]]; then
  command -v wasm-opt >/dev/null || { echo 'wasm-opt (binaryen) is required; pass --no-wasm-opt to skip' >&2; exit 2; }
  # A few at a time: the start module alone takes gigabytes to optimize.
  # shellcheck disable=SC2086
  began=$SECONDS
  printf '%s\0' "$out"/pkg/*.wasm | xargs -0 -P 4 -I {} wasm-opt $wasm_opt_flags {} -o {}
  took wasm-opt "$began"
else
  wasm_opt_flags=
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
  local stage=web-bundle-wasm-$bundle-$profile args=(--profile "$profile")
  local script=${crate#"$repo"/}/scripts/build-wasm-on-builder.sh
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
  target_dir=${CARGO_TARGET_DIR:-}
  if [[ $target_dir != /* ]]; then
    target_dir=$("$cargo" metadata --format-version 1 --no-deps --offline |
      python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
  fi
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
find "$crate/web" -maxdepth 1 -name '*.mjs' ! -name '*.test.mjs' -exec cp {} "$out/" \;
cp "$crate/web/index.html" "$out/index.html"
cp "$crate/web/sign-in.html" "$out/sign-in.html"
cp "$crate/web/apple-touch-icon.png" "$out/apple-touch-icon.png"
cp "$crate/web/manifest.json" "$crate/web/icon.svg" "$out/"
mkdir -p "$out/locales"
cp "$crate"/web/locales/*.ftl "$out/locales/"
mkdir -p "$out/app"
cp "$crate/web/app/index.html" "$out/app/index.html"
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
