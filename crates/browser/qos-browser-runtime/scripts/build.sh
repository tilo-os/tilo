#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "$0")/../../../.." && pwd)
out=${1:?usage: build.sh /absolute/output-directory}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$repo/target/browser"}
cd "$repo"
if [[ "${QOS_USE_ZIG:-0}" == 1 ]]; then
  export CC_wasm32_unknown_unknown="$repo/crates/hosting/qos-relay/scripts/wasm-cc.sh"
  export AR_wasm32_unknown_unknown="$repo/crates/hosting/qos-relay/scripts/wasm-ar.sh"
fi
cargo build --locked -j2 -p qos-browser-runtime --target wasm32-unknown-unknown
mkdir -p "$out/pkg" "$out/fonts"
"${WASM_BINDGEN:-wasm-bindgen}" "$CARGO_TARGET_DIR/wasm32-unknown-unknown/debug/qos_browser_runtime.wasm" --target web --out-dir "$out/pkg"
cp crates/browser/qos-browser-runtime/web/*.mjs crates/browser/qos-browser-runtime/web/*.d.ts "$out/"
cp crates/browser/qos-browser-runtime/web/proof.html "$out/index.html"
cp crates/browser/qos-browser-runtime/web/peer-proof.html "$out/peer-proof.html"
python3 - "$repo" "$out" <<'PY'
import sys,pathlib,re,shutil,json
repo,out=map(pathlib.Path,sys.argv[1:])
source=repo/'ui/assets/fonts'
names=[]
for block in (source/'manifest.toml').read_text().split('[[font]]')[1:]:
    block=block.split('[[license]]')[0]
    if re.search(r'^scope = "product"$',block,re.M):
        name=re.search(r'^file = "(.*?)"$',block,re.M)[1]
        shutil.copy2(source/name,out/'fonts'/name)
        names.append(name)
shutil.copytree(source/'LICENSES',out/'fonts/LICENSES',dirs_exist_ok=True)
shutil.copy2(source/'manifest.toml',out/'fonts/manifest.toml')
(out/'fonts.json').write_text(json.dumps(names))
PY
