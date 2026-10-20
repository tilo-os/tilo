#!/usr/bin/env bash
# Build every web-capable example among PACKAGE... with `tilo-app build
# --platform web`: the browser runtime bundle with the example registered. It
# proves the Web column of `tilo-app examples`, which the portability lint
# already ties to what each example calls.
#
#   tools/ci/build-web-examples.sh example-counter example-qr-code ...
#
# t.toml's `web-examples` check runs it with every example
# (tools/ci/package_sets.py). The bundles build into one target directory, so the runtime and
# the toolkit compile once, with the first example, and each later example
# only links its own bundle. What follows the link (wasm-bindgen over a
# development module of about 215 MB, then the page assets) takes 30-50 s on
# one core per example, against 3-5 s of cargo, so the later examples run
# WEB_EXAMPLE_JOBS (default 4) at a time; their cargo steps take turns on the
# build directory's lock.
set -euo pipefail
cd "$(dirname "$0")/../.."

tool_started=$SECONDS
# Not --quiet: cargo then reports when it waits for a lock another build holds.
cargo build --locked -p tilo-app-cli --bin tilo-app
printf 'TILO_BUILD_TIMING phase=web-build-tool elapsed_seconds=%s\n' "$((SECONDS - tool_started))"
tilo_app=${CARGO_TARGET_DIR:-$PWD/target}/debug/tilo-app
pinned=$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' crates/browser/tilo-browser-runtime/Cargo.toml)
if [[ "$(wasm-bindgen --version 2>/dev/null)" != "wasm-bindgen $pinned" ]]; then
  cargo install --locked wasm-bindgen-cli --version "$pinned"
fi

selected=" $* "
apps_text=$("$tilo_app" examples --json | node -e '
const {examples} = JSON.parse(require("fs").readFileSync(0, "utf8")).result;
const selected = process.argv[1];
for (const example of examples) {
  if (example.web && selected.includes(` example-${example.name} `)) console.log(example.name);
}' "$selected")
apps=()
if [[ -n $apps_text ]]; then
  mapfile -t apps <<<"$apps_text"
fi
if [[ ${#apps[@]} -eq 0 ]]; then
  echo "no web-capable example selected"
  exit 0
fi

# Every fresh app bundle otherwise runs npm ci and rebuilds this identical,
# pinned library. Build it once into the canonical page-host assets: the web
# target copies those assets and keeps its existing lock-freshness check.
# Both the self-contained module and its licenses come from the same builder.
moq_started=$SECONDS
(
  cd scripts/moq-browser
  npm ci --no-audit --no-fund --silent
  node build.mjs
)
printf 'TILO_BUILD_TIMING phase=web-moq-shared elapsed_seconds=%s\n' "$((SECONDS - moq_started))"
printf 'TILO_WEB_MOQ_BUILDS=1 examples=%s\n' "${#apps[@]}"

out=${CARGO_TARGET_DIR:-$PWD/target}/web-examples
mkdir -p "$out"
build_example() {
  local app=$1 example_started=$SECONDS
  echo "== examples/apps/$app"
  "$tilo_app" build --json --platform web "examples/apps/$app" >"$out-$app.json" || return
  printf 'TILO_BUILD_TIMING phase=web-example-%s elapsed_seconds=%s\n' "$app" "$((SECONDS - example_started))"
  node -e '
const build = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).result;
require("fs").copyFileSync(process.argv[2], require("path").join(build.output, "media-moq-library.LICENSE.txt"));
console.log(`${build.app_id}: ${build.module_bytes} byte bundle module, ${build.output}/${build.entry}`);
' "$out-$app.json" "$PWD/crates/browser/tilo-browser-runtime/web/media-moq-library.LICENSE.txt"
}

# The first example compiles what they all share, on every core.
build_example "${apps[0]}"
jobs=${WEB_EXAMPLE_JOBS:-4}
for app in "${apps[@]:1}"; do
  while (($(jobs -rp | wc -l) >= jobs)); do wait -n || true; done
  (if build_example "$app" >"$out-$app.log" 2>&1; then echo 0; else echo 1; fi >"$out-$app.status") &
done
wait
failed=()
for app in "${apps[@]:1}"; do
  cat "$out-$app.log"
  [[ $(cat "$out-$app.status") == 0 ]] || failed+=("$app")
done
if ((${#failed[@]})); then
  echo "web build failed: ${failed[*]}" >&2
  exit 1
fi
