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

cargo build --locked --quiet -p tilo-app-cli --bin tilo-app
tilo_app=$PWD/target/debug/tilo-app
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

out=$PWD/target/web-examples
mkdir -p "$out"
for app in "${apps[@]}"; do
  echo "== examples/apps/$app"
  "$tilo_app" build --json --platform web "examples/apps/$app" >"$out-$app.json"
  node -e '
const build = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).result;
console.log(`${build.app_id}: ${build.module_bytes} byte bundle module, ${build.output}/${build.entry}`);
' "$out-$app.json"
done
