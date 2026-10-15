#!/usr/bin/env bash
# Build every web-capable example among PACKAGE... with `tilo-app build
# --platform web`: the browser runtime bundle with the example registered. It
# proves the Web column of `tilo-app examples`, which the portability lint
# already ties to what each example calls.
#
#   tools/ci/build-web-examples.sh example-counter example-qr-code ...
#
# check.yml's `wasm` job runs it with the examples tools/ci/affected_packages.py
# selects. The bundles build into one target directory, so the runtime and
# the toolkit compile once and each example only links its own bundle.
set -euo pipefail
cd "$(dirname "$0")/../.."

tool_started=$SECONDS
cargo build --locked --quiet -p tilo-app-cli --bin tilo-app
printf 'TILO_BUILD_TIMING phase=web-build-tool elapsed_seconds=%s\n' "$((SECONDS - tool_started))"
tilo_app=${CARGO_TARGET_DIR:-$PWD/target}/debug/tilo-app
pinned=$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' crates/browser/tilo-browser-runtime/Cargo.toml)
if [[ "$(wasm-bindgen --version 2>/dev/null)" != "wasm-bindgen $pinned" ]]; then
  cargo install --locked wasm-bindgen-cli --version "$pinned"
fi

selected=" $* "
mapfile -t apps < <("$tilo_app" examples --json | node -e '
const {examples} = JSON.parse(require("fs").readFileSync(0, "utf8")).result;
const selected = process.argv[1];
for (const example of examples) {
  if (example.web && selected.includes(` example-${example.name} `)) console.log(example.name);
}' "$selected")
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
for app in "${apps[@]}"; do
  echo "== examples/apps/$app"
  example_started=$SECONDS
  "$tilo_app" build --json --platform web "examples/apps/$app" >"$out-$app.json"
  printf 'TILO_BUILD_TIMING phase=web-example-%s elapsed_seconds=%s\n' "$app" "$((SECONDS - example_started))"
  node -e '
const build = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).result;
require("fs").copyFileSync(process.argv[2], require("path").join(build.output, "media-moq-library.LICENSE.txt"));
console.log(`${build.app_id}: ${build.module_bytes} byte bundle module, ${build.output}/${build.entry}`);
' "$out-$app.json" "$PWD/crates/browser/tilo-browser-runtime/web/media-moq-library.LICENSE.txt"
done
