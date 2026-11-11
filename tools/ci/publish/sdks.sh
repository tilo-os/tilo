#!/usr/bin/env bash
# The SDKs' release, as npm and PyPI get it. Both are published under the one name tilo-sdk:
# on PyPI the plain name tilo belongs to an unrelated project, so the Python distribution is
# tilo-sdk while the module it installs stays `import tilo`; on npm the @tilo scope is not
# available, so the TypeScript package is the unscoped tilo-sdk. Anyone who installs either
# gets the same name they read in the docs (`uv add tilo-sdk`, `bun add tilo-sdk`).
#
# The version is the release tag's (v0.4.0 publishes 0.4.0 to both registries). Nothing in the
# tree carries it: the tag is the one place a version is decided, so it is written into the
# manifests of a copy, and both packages of one tag carry one version.
#
#   sdks.sh check    Build both packages as a tag would and read the result back, publishing
#                    nothing: the version written into a copy's manifests, `bun pm pack
#                    --dry-run` for the TypeScript package, `uv build` and `uv publish --dry-run`
#                    for the Python one, then the name, the version and the module the built
#                    artifacts carry. This is what runs on main, and it runs again before a
#                    tag's publish, so a broken manifest shows up long before a tag does.
#   sdks.sh publish  Publish both at the tag's version. A version already on the registry is
#                    skipped, so re-running a tag whose run failed halfway finishes the release
#                    instead of failing on what is already published.
#
# The suites are not run here: sdk-python and sdk-typescript are this check's `needs`, so a
# release publishes only what they passed. The registry tokens come from t
# (`secrets = ["NPM_TOKEN", "PYPI_TOKEN"]`) and are never printed: npm's through
# NPM_CONFIG_TOKEN, PyPI's through UV_PUBLISH_TOKEN, and `t` masks either in the log.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

# uv keeps its cache in the image, which a job reads read-only and uv writes to even when it only
# builds; it gets a copy in the job's scratch, as the suites do (tools/ci/sdk-python.sh). bun's
# install only reads its cache, so it needs no copy.
if [[ -d /opt/tilo-sdk/uv-cache ]]; then
  export UV_CACHE_DIR=$scratch/uv-cache
  export UV_PROJECT_ENVIRONMENT=$scratch/python-venv
  cp -a /opt/tilo-sdk/uv-cache "$UV_CACHE_DIR"
fi

usage='usage: sdks.sh check | publish'
name=tilo-sdk
case "${1:-}" in
  check | publish) mode=$1 ;;
  *) die "$usage" ;;
esac

# The version this release carries: the release tag's (vX.Y.Z, maybe with a pre-release part).
version() {
  if [[ ${T_REF:-} =~ ^tag:v([0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?)$ ]]; then
    printf '%s' "${BASH_REMATCH[1]}"
  else
    die "the SDKs are released from a release tag (vX.Y.Z); this run is for ${T_REF:-a working tree}"
  fi
}

work=
trap 'rm -rf -- "${work:-}"' EXIT

# Stage both SDKs in WORK, with VERSION written into their manifests: the version a tag would
# publish. A tag run stages them here too, so the run that publishes builds what the check built.
stage() {
  work=$(mktemp -d "$scratch/sdks.XXXXXX")
  cp -a sdk/typescript sdk/python "$work/"
  rm -rf "$work/typescript/node_modules"
  python3 - "$work/typescript/package.json" "$work/python/pyproject.toml" "$1" <<'PY'
import json, pathlib, re, sys

package, project, version = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3]
metadata = json.loads(package.read_text())
metadata["version"] = version
package.write_text(json.dumps(metadata, indent=2) + "\n")
text, replaced = re.subn(r'(?m)^version = "[^"]+"$', f'version = "{version}"', project.read_text(), count=1)
if replaced != 1:
    raise SystemExit(f"{project}: no version line to set")
project.write_text(text)
PY
}

# The TypeScript package, installed from the image's bun cache (the suites' image carries it, so
# this needs no network) and built: tsc writes dist/, which is what the package publishes.
build_typescript() {
  cd "$work/typescript"
  bun install --frozen-lockfile --offline
  bun run build
}

# What the built TypeScript package says it is and what it carries: the name and version npm
# would be asked to publish, the entry points package.json exports, and the tarball they pack
# into. `bun pm pack` is what `bun publish` packs, without the registry and the token it takes;
# the build above ran tsc, so nothing packs here that a publish would not.
check_typescript() {
  python3 - "$work/typescript" "$name" "$1" <<'PY'
import json, pathlib, sys

root, name, version = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
metadata = json.loads((root / "package.json").read_text())
if metadata.get("name") != name or metadata.get("version") != version:
    raise SystemExit(f"{name}: package.json names {metadata.get('name')}@"
                     f"{metadata.get('version')}, not {name}@{version}")
missing = [entry for entry in ("dist/index.js", "dist/index.d.ts", "dist/types/index.js")
           if not (root / entry).is_file()]
if missing:
    raise SystemExit(f"{name}: the build left no {', '.join(missing)}")
PY
  bun pm pack --dry-run --ignore-scripts
}

# The dist-tag a version publishes under: npm's `latest` points at a final release, so a
# pre-release goes under `next` and `bun install tilo-sdk` keeps getting the newest final one,
# as the `tilo` command's own `latest` does.
npm_tag() {
  if [[ $1 == *-* ]]; then printf 'next'; else printf 'latest'; fi
}

# The Python package as `uv build` writes it: the sdist and the wheel that PyPI takes.
build_python() {
  cd "$work/python"
  rm -rf dist
  uv build
}

# What the built Python package says it is: the metadata PyPI would index, and that the module
# inside stays `tilo`, the name `import tilo` uses. PEP 440 respells a version its own way
# (0.4.0-rc.1 as 0.4.0rc1) in the metadata and in the file names, so both sides are compared
# with separators dropped: the spelling is the build backend's, the version in it is the tag's.
check_python() {
  python3 - "$work/python/dist" "$name" "$1" <<'PY'
import pathlib, re, sys, zipfile

def plain(text):
    return re.sub(r"[-_.]", "", text).lower()

dist, name, version = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
expected = plain(name) + plain(version)
files = sorted(dist.iterdir())
wheel = [path for path in files if path.suffix == ".whl" and plain(path.name).startswith(expected)]
if not wheel:
    raise SystemExit(f"{dist}: the build left no {name} {version} wheel ({', '.join(p.name for p in files)})")
with zipfile.ZipFile(wheel[-1]) as archive:
    metadata = next(entry for entry in archive.namelist() if entry.endswith(".dist-info/METADATA"))
    fields = dict(line.split(": ", 1) for line in archive.read(metadata).decode().splitlines()
                  if ": " in line)
    if fields.get("Name") != name or plain(fields.get("Version", "")) != plain(version):
        raise SystemExit(f"{wheel[-1].name}: METADATA names {fields.get('Name')}@"
                         f"{fields.get('Version')}, not {name}@{version}")
    if "tilo/__init__.py" not in archive.namelist():
        raise SystemExit(f"{wheel[-1].name}: carries no tilo module")
if not [path for path in files if path.suffix == ".gz" and plain(path.name).startswith(expected)]:
    raise SystemExit(f"{dist}: the build left no {name} {version} sdist")
PY
  cd "$work/python"
  uv publish --dry-run dist/*
}

# Whether URL answers, i.e. that version is already on the registry: registries take a version
# once and refuse it after, so a run that finds it there has nothing to publish.
published() {
  python3 - "$1" <<'PY'
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

publish_typescript() {
  local v=$1 url=https://registry.npmjs.org/$name/$1
  if published "$url"; then
    printf '%s@%s is already on npm; skipping it\n' "$name" "$v"
    return
  fi
  cd "$work/typescript"
  # The build above is the one that packs: a `prepack` script here would run tsc a second time,
  # and bun runs a lifecycle script in a sandbox where tsc may not be found, which a publish
  # must not die of after everything it sends is ready.
  NPM_CONFIG_TOKEN=${NPM_TOKEN:?the check needs secrets = ["NPM_TOKEN"]} \
    bun publish --ignore-scripts --access public --tag "$(npm_tag "$v")"
}

publish_python() {
  local v=$1 url=https://pypi.org/pypi/$name/$v/json
  if published "$url"; then
    printf '%s@%s is already on PyPI; skipping it\n' "$name" "$v"
    return
  fi
  cd "$work/python"
  UV_PUBLISH_TOKEN=${PYPI_TOKEN:?the check needs secrets = ["PYPI_TOKEN"]} uv publish dist/*
}

if [[ $mode == check ]]; then
  # A tag run checks the version it is about to publish; anywhere else the version is one no
  # release carries, so the same rewrite is exercised without standing for a real version.
  if [[ ${T_REF:-} == tag:* ]]; then v=$(version); else v=0.0.0; fi
  stage "$v"
  build_typescript
  check_typescript "$v"
  build_python
  check_python "$v"
  printf '%s %s is ready to publish as a release tag would: both packages build, and carry the name, the version and the module they should\n' \
    "$name" "$v"
  exit 0
fi

v=$(version)
[[ -n ${NPM_TOKEN:-} && -n ${PYPI_TOKEN:-} ]] \
  || die 'publishing needs both registry tokens: secrets = ["NPM_TOKEN", "PYPI_TOKEN"]'
stage "$v"
build_typescript
check_typescript "$v"
build_python
check_python "$v"
publish_typescript "$v"
publish_python "$v"
printf '%s %s is on npm and PyPI\n' "$name" "$v"
