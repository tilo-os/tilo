#!/usr/bin/env bash
# The page module and what it runs: `tilolang_web.js`, `tilolang_web_bg.wasm` and
# `tilolang_web.d.ts` (wasm-bindgen's `web` target); `apps.json`, every file of the apps in
# `apps/tl` by its path, which `open_app` and `compile_package` take as they are; and `fonts/`,
# the product catalog's startup fonts a page hands over (`add_font`) before it opens an app and
# the web cuts of Tilo Sans the page's stylesheet names (`/fonts/*.woff2`).
#
#   crates/lang/tilolang-web/build-page.sh [OUT]     (default: target/tilolang-web)
#
# PROFILE picks the cargo profile (default `wasm-release`; `wasm-dev` builds faster).
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../.."
profile="${PROFILE:-wasm-release}"
out="${1:-target/tilolang-web}"

cargo build --locked --target wasm32-unknown-unknown --profile "$profile" -p tilolang-web
mkdir -p "$out/fonts"
wasm-bindgen --target web --out-dir "$out" \
  "${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/$profile/tilolang_web.wasm"

python3 - "$out/apps.json" <<'PY'
import json, pathlib, sys
names = {"app.ids", "tilo-app.toml", "surface.toml"}
files = {}
for path in sorted(pathlib.Path("apps/tl").rglob("*")):
    if path.is_file() and (path.suffix in {".tl", ".ftl"} or path.name in names):
        files[path.as_posix()] = path.read_text(encoding="utf-8")
pathlib.Path(sys.argv[1]).write_text(json.dumps(files, ensure_ascii=False), encoding="utf-8")
PY

fonts="$(python3 -I tools/font-catalog.py root crates/ui/ui-core/assets/fonts --scope product)"
python3 - "$fonts" "$out/fonts" <<'PY'
import pathlib, shutil, sys, tomllib
manifest = tomllib.loads(pathlib.Path("crates/ui/font-catalog/manifest.toml").read_text())
for font in manifest["font"]:
    if font["scope"] == "product" and font.get("browser_startup"):
        shutil.copyfile(pathlib.Path(sys.argv[1]) / font["file"], pathlib.Path(sys.argv[2]) / font["file"])
PY
web="$(python3 -I tools/font-catalog.py root crates/ui/ui-core/assets/fonts --scope web)"
cp "$web"/*.woff2 "$out/fonts/"
ls -l "$out" "$out/fonts"
