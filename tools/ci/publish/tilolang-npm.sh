#!/usr/bin/env bash
# tilolang in a browser, as npm gets it: the package `tilolang`, which is tilolang-web's page
# module (crates/lang/tilolang-web/build-page.sh: wasm-bindgen's `web` target, so
# `import init, { compile_package, open_app } from "tilolang"` works in any bundler or a plain
# <script type="module">) and the startup fonts its windows set text in, with their licences.
# The playground's apps that build-page.sh also writes (apps.json) are a demo, not the package.
#
# The version is the release tag's (v0.4.0 publishes 0.4.0), as for the crates, the SDKs and the
# tilo command: the tree carries none, so a copy's package.json gets it.
#
#   tilolang-npm.sh build OUT    build the module and lay the package out in OUT/package, at the
#                                stand-in version 0.0.0; holds no credentials
#   tilolang-npm.sh check DIR    the package in DIR/package as a tag would publish it, read back:
#                                its version written into a copy, the files package.json names
#                                present, `bun pm pack --dry-run` of it; publishes nothing
#   tilolang-npm.sh publish DIR  the same at the tag's version, then `bun publish`; a version
#                                already on npm is skipped, so re-running a tag finishes it
#
# The registry token comes from t (`secrets = ["NPM_TOKEN"]`, the one the SDKs publish with) and
# reaches bun as NPM_CONFIG_TOKEN alone.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
usage='usage: tilolang-npm.sh build OUT | check DIR | publish DIR'
name=tilolang

# The version this release carries: the release tag's (vX.Y.Z, maybe with a pre-release part).
version() {
  if [[ ${T_REF:-} =~ ^tag:v([0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?)$ ]]; then
    printf '%s' "${BASH_REMATCH[1]}"
  else
    die "tilolang is released to npm from a release tag (vX.Y.Z); this run is for ${T_REF:-a working tree}"
  fi
}

cmd_build() {
  local out=${1:?$usage} page
  page=$(mktemp -d "$scratch/tilolang-page.XXXXXX")
  bash crates/lang/tilolang-web/build-page.sh "$page"
  local fonts
  fonts=$(python3 -I tools/font-catalog.py root crates/ui/ui-core/assets/fonts --scope product)
  rm -rf "$out"
  mkdir -p "$out"
  cargo metadata --locked --format-version 1 --no-deps > "$scratch/tilolang-metadata.json"
  python3 -I - "$page" "$fonts" "$out/package" "$scratch/tilolang-metadata.json" <<'PY'
import json, pathlib, shutil, sys, tomllib

page, fonts, package, metadata = (pathlib.Path(arg) for arg in sys.argv[1:5])
repo = pathlib.Path.cwd()
crate = next(p for p in json.loads(metadata.read_text())["packages"] if p["name"] == "tilolang-web")
# The licence is the crate's: the package is its build, so it says what the crate says.
license_files = {"MIT": "LICENSE-MIT", "AGPL-3.0-only": "LICENSE"}
if crate["license"] not in license_files:
    raise SystemExit(f"tilolang-web is licensed {crate['license']}: name its licence file here")
package.mkdir(parents=True)
module = ["tilolang_web.js", "tilolang_web.d.ts", "tilolang_web_bg.wasm", "tilolang_web_bg.wasm.d.ts"]
for file in module:
    shutil.copyfile(page / file, package / file)
shutil.copyfile(repo / license_files[crate["license"]], package / "LICENSE")
shutil.copyfile(repo / "crates/lang/tilolang-web/npm/README.md", package / "README.md")
# The startup fonts (`startup_fonts()` names them, `add_font` takes them), each with the licence
# files the catalog's manifest names for it.
catalog = tomllib.loads((repo / "crates/ui/font-catalog/manifest.toml").read_text())
(package / "fonts/LICENSES").mkdir(parents=True)
startup = [font for font in catalog["font"] if font["scope"] == "product" and font.get("browser_startup")]
if not startup:
    raise SystemExit("the font catalog names no browser startup fonts")
for font in startup:
    shutil.copyfile(fonts / font["file"], package / "fonts" / font["file"])
    for licence in font.get("license_files", []):
        shutil.copyfile(repo / "crates/ui/ui-core/assets/fonts" / licence, package / "fonts" / licence)
manifest = {
    "name": "tilolang",
    "version": "0.0.0",
    "description": crate["description"],
    "license": crate["license"],
    "homepage": "https://tilolang.dev",
    "repository": {
        "type": "git",
        "url": "git+" + crate["repository"].rstrip("/") + ".git",
        "directory": "crates/lang/tilolang-web",
    },
    "type": "module",
    "main": "./tilolang_web.js",
    "types": "./tilolang_web.d.ts",
    "exports": {
        ".": {"types": "./tilolang_web.d.ts", "import": "./tilolang_web.js"},
        "./tilolang_web_bg.wasm": "./tilolang_web_bg.wasm",
        "./fonts/*": "./fonts/*",
    },
    "files": module + ["fonts"],
    "keywords": ["tilolang", "tilo", "wasm", "local-first", "crdt"],
}
(package / "package.json").write_text(json.dumps(manifest, indent=2) + "\n")
PY
  rm -rf "$page"
  check_files "$out/package" 0.0.0
  du -sh "$out/package"
}

# The package in $1 says it is tilolang@$2 and holds every file it names.
check_files() {
  python3 -I - "$1" "$name" "$2" <<'PY'
import json, pathlib, sys

root, name, version = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
manifest = json.loads((root / "package.json").read_text())
if manifest.get("name") != name or manifest.get("version") != version:
    raise SystemExit(f"{root}/package.json names {manifest.get('name')}@{manifest.get('version')}, "
                     f"not {name}@{version}")
for field in ("description", "license", "repository"):
    if not manifest.get(field):
        raise SystemExit(f"{root}/package.json has no {field}")
missing = [entry for entry in manifest["files"] + ["README.md", "LICENSE"] if not (root / entry).exists()]
if missing:
    raise SystemExit(f"{name}: the package names {', '.join(missing)}, which it does not hold")
wasm = (root / "tilolang_web_bg.wasm").read_bytes()
if wasm[:4] != b"\0asm":
    raise SystemExit(f"{name}: tilolang_web_bg.wasm is no WebAssembly module")
module = (root / "tilolang_web.js").read_text()
for export in ("compile_package", "open_app", "startup_fonts", "add_font", "fonts_ready"):
    if f"export function {export}(" not in module:
        raise SystemExit(f"{name}: tilolang_web.js exports no {export}")
if not any((root / "fonts").glob("*.?tf")):
    raise SystemExit(f"{name}: the package holds no startup fonts")
PY
}

work=
trap 'rm -rf -- "${work:-}"' EXIT

# The package of $1 copied and set at version $2, read back and packed as `bun publish` packs
# it; the copy is left in $work/package.
stage() {
  local dir=${1:?$usage} v=$2
  [[ -f $dir/package/package.json ]] || die "$dir holds no package (tilolang-npm.sh build)"
  work=$(mktemp -d "$scratch/tilolang-npm.XXXXXX")
  cp -a "$dir/package" "$work/package"
  python3 -I - "$work/package/package.json" "$v" <<'PY'
import json, pathlib, sys
path, version = pathlib.Path(sys.argv[1]), sys.argv[2]
manifest = json.loads(path.read_text())
manifest["version"] = version
path.write_text(json.dumps(manifest, indent=2) + "\n")
PY
  check_files "$work/package" "$v"
  (cd "$work/package" && bun pm pack --dry-run --ignore-scripts)
}

# Whether $1@$2 is on npm already: npm takes a version once and refuses it after.
published() {
  python3 -I - "https://registry.npmjs.org/$1/$2" <<'PY'
import sys, urllib.error, urllib.request
try:
    with urllib.request.urlopen(sys.argv[1], timeout=30):
        pass
except urllib.error.HTTPError as error:
    if error.code == 404:
        raise SystemExit(1)
    raise
PY
}

case ${1:-} in
  build) shift; cmd_build "$@" ;;
  check)
    shift
    # A tag run checks the version it is about to publish; anywhere else the stand-in.
    if [[ ${T_REF:-} == tag:* ]]; then v=$(version); else v=0.0.0; fi
    stage "${1:?$usage}" "$v"
    printf '%s %s is ready to publish as a release tag would\n' "$name" "$v"
    ;;
  publish)
    shift
    v=$(version)
    [[ -n ${NPM_TOKEN:-} ]] || die 'publishing needs the registry token: secrets = ["NPM_TOKEN"]'
    stage "${1:?$usage}" "$v"
    if published "$name" "$v"; then
      printf '%s@%s is already on npm; skipping it\n' "$name" "$v"
      exit 0
    fi
    # A pre-release goes under `next`, so `bun add tilolang` keeps getting the newest final one.
    tag=latest
    if [[ $v == *-* ]]; then tag=next; fi
    (cd "$work/package" && NPM_CONFIG_TOKEN=$NPM_TOKEN bun publish --ignore-scripts --tag "$tag")
    printf '%s %s is on npm\n' "$name" "$v"
    ;;
  *) die "$usage" ;;
esac
