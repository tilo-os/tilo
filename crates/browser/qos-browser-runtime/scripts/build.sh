#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "$0")/../../../.." && pwd)
out=${1:?usage: build.sh /absolute/output-directory [--profile release|dev]}
shift
profile=release
if [[ $# -gt 0 ]]; then
  [[ $# == 2 && "$1" == --profile ]] || { echo 'usage: build.sh OUTPUT [--profile release|dev]' >&2; exit 2; }
  profile=$2
fi
case "$profile" in release) artifact_profile=release ;; dev) artifact_profile=debug ;; *) echo "Unsupported profile: $profile" >&2; exit 2 ;; esac
[[ "$out" == /* ]] || { echo 'Output must be an absolute directory' >&2; exit 2; }
# Reusing an output with stale modules would make provenance ambiguous.
[[ ! -d "$out" || -z "$(ls -A "$out")" ]] || { echo 'Output directory must be empty' >&2; exit 2; }
# Read Cargo's ordinary artifact location without changing its cache key.
artifact_target_dir=${CARGO_TARGET_DIR:-"$repo/target"}
cd "$repo"
if [[ "${QOS_USE_ZIG:-0}" == 1 ]]; then
  export CC_wasm32_unknown_unknown="$repo/crates/hosting/tilo-relay/scripts/wasm-cc.sh"
  export AR_wasm32_unknown_unknown="$repo/crates/hosting/tilo-relay/scripts/wasm-ar.sh"
fi
revision=$(git rev-parse HEAD)
source_status=$(git status --porcelain)
cargo build --locked --profile "$profile" -j2 -p qos-browser-runtime --target wasm32-unknown-unknown
mkdir -p "$out/pkg" "$out/fonts"
"${WASM_BINDGEN:-wasm-bindgen}" "$artifact_target_dir/wasm32-unknown-unknown/$artifact_profile/qos_browser_runtime.wasm" --target web --out-dir "$out/pkg"
cp crates/browser/qos-browser-runtime/web/*.mjs crates/browser/qos-browser-runtime/web/*.d.ts "$out/"
cp crates/browser/qos-browser-runtime/web/proof.html "$out/index.html"
cp crates/browser/qos-browser-runtime/web/peer-proof.html "$out/peer-proof.html"
python3 - "$repo" "$out" <<'PY'
import sys,pathlib,re,shutil,json
repo,out=map(pathlib.Path,sys.argv[1:])
source=repo/'crates/sdk/ui-core/assets/fonts'
text=(source/'manifest.toml').read_text()
assets=[]
for block in text.split('[[font]]')[1:]:
    block=block.split('[[license]]')[0]
    if re.search(r'^scope = "product"$',block,re.M):
        name=re.search(r'^file = "(.*?)"$',block,re.M)[1]
        shutil.copy2(source/name,out/'fonts'/name)
        assets.append(dict(
            name=name,
            bytes=int(re.search(r'^bytes = (\d+)$',block,re.M)[1]),
            sha256=re.search(r'^sha256 = "([0-9a-f]{64})"$',block,re.M)[1],
            startup=bool(re.search(r'^browser_startup = true$',block,re.M)),
        ))
assert any(asset['startup'] for asset in assets)
shutil.copytree(source/'LICENSES',out/'fonts/LICENSES',dirs_exist_ok=True)
shutil.copy2(source/'manifest.toml',out/'fonts/manifest.toml')
catalog=re.search(r'^catalog_id = "(.*?)"$',text,re.M)[1]
(out/'fonts.json').write_text(json.dumps(dict(schema=1,catalog=catalog,assets=assets),separators=(',',':'))+'\n')
PY

# Capture the actual tools/profile and output hashes; never infer a prebuilt profile.
[[ "$(git rev-parse HEAD)" == "$revision" && "$(git status --porcelain)" == "$source_status" ]] || { echo 'Source changed during build; discard output' >&2; exit 1; }
python3 - "$out" "$revision" "$source_status" "$profile" "$(rustc --version)" "$("${WASM_BINDGEN:-wasm-bindgen}" --version)" <<'PYMETA'
import hashlib,json,pathlib,sys
out=pathlib.Path(sys.argv[1])
metadata=dict(revision=sys.argv[2],dirty=bool(sys.argv[3]),profile=sys.argv[4],rustc=sys.argv[5],wasmBindgen=sys.argv[6],target='wasm32-unknown-unknown',builtFromSource=True)
metadata['files']={str(p.relative_to(out)):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(out.rglob('*')) if p.is_file()}
(out/'build.json').write_text(json.dumps(metadata,indent=2)+'\n')
PYMETA
