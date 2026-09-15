#!/usr/bin/env bash
# Materialise the immutable Tilo application store for a host-session package.
#
# The OS image has always shipped apps through the normal qpkg -> tilo-install
# path.  Reuse that exact format here: the launcher merges /usr/share/tilo-apps
# with the user's writable store, where a user package with the same id wins.
# That makes an RPM update replace only system-owned payloads; it never
# rewrites ~/.local/share/tilo-apps, Tilo rooms, Flatpaks, or any host app.
set -euo pipefail

usage() {
    echo "usage: $0 APP_RELEASE_DIR BIN_DIR STAGE_DIR" >&2
    exit 2
}

[[ $# == 3 ]] || usage
release_dir=$1
bin_dir=$2
stage=$3
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
arch=$(uname -m)
collection=$release_dir/tilo-apps-$arch.tar.gz

for tool in tilo-install tilo-runtime-bundle; do
    [[ -x $bin_dir/$tool ]] || {
        echo "missing app-store tool: $bin_dir/$tool" >&2
        exit 1
    }
done
[[ -f $collection ]] || { echo "missing app collection: $collection" >&2; exit 1; }
[[ ! -e $stage ]] || { echo "app-store stage already exists: $stage" >&2; exit 1; }

work=$(mktemp -d)
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT

tar -xzf "$collection" -C "$work"
[[ -f $work/index.json && -f $work/SHA256SUMS && -d $work/qpkg && -d $work/tilo-runtime ]] || {
    echo "app collection has an unexpected layout" >&2
    exit 1
}
(cd "$work" && sha256sum --check --strict SHA256SUMS)

runtime_version=$(python3 - "$work/index.json" <<'PY'
import json, pathlib, sys
data = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
versions = {app["runtime"] for app in data["apps"]}
if len(versions) != 1:
    raise SystemExit(f"collection declares {len(versions)} runtime versions")
runtime = versions.pop()
prefix = "tilo-runtime/"
if not runtime.startswith(prefix) or not runtime[len(prefix):]:
    raise SystemExit(f"invalid runtime reference: {runtime!r}")
print(runtime[len(prefix):])
PY
)
[[ -f $work/tilo-runtime/$runtime_version/bundle.toml ]] || {
    echo "collection runtime tilo-runtime/$runtime_version is missing bundle.toml" >&2
    exit 1
}
"$bin_dir/tilo-runtime-bundle" verify "$work/tilo-runtime/$runtime_version" >/dev/null

mkdir -p "$stage/usr/share" "$stage/usr/lib" "$stage/etc/tilo"
cp -a "$work/tilo-runtime" "$stage/usr/lib/tilo-runtime"
store="$stage/usr/share/tilo-apps"
mkdir -p "$store"

# Use a staging root for physical writes but the packaged system runtime for
# dependency resolution.  The registry is normalised to its final FHS path
# immediately below, and the normalisation is checked structurally rather than
# trusting a broad textual substitution.
export TILO_APPS_DATA_DIR="$store"
export TILO_RUNTIME_BUNDLE_DIR="$stage/usr/lib/tilo-runtime"
export TILO_CLI_SHIM_PATH="$bin_dir/tilo-cli-shim"
export TILO_LAUNCHER_PATH=/usr/bin/tilo-launcher
for package in "$work"/qpkg/*.qpkg; do
    "$bin_dir/tilo-install" install "$package" --allow-self-signed >/dev/null
done

python3 - "$store/registry.toml" "$store" /usr/share/tilo-apps <<'PY'
import pathlib, sys, tomllib

registry, staged, final = map(pathlib.Path, sys.argv[1:])
data = tomllib.loads(registry.read_text(encoding="utf-8"))
entries = data.get("entry", [])
if not entries:
    raise SystemExit("system app registry is empty")
for entry in entries:
    for field in ("payload_root", "manifest_path"):
        value = pathlib.Path(entry[field])
        try:
            value.relative_to(staged)
        except ValueError as error:
            raise SystemExit(f"{field} escapes staged store: {value}") from error

# tilo-install owns the registry format.  It writes only the two absolute paths
# above under the staging root, so replace exact quoted prefixes after the
# structural parse.  A later field must not gain a staging path unnoticed.
raw = registry.read_text(encoding="utf-8")
old = f'"{staged}'
new = f'"{final}'
normalised = raw.replace(old, new)
if staged.as_posix() in normalised:
    raise SystemExit("staging path leaked into packaged registry")
registry.write_text(normalised, encoding="utf-8")

checked = tomllib.loads(registry.read_text(encoding="utf-8"))
for entry in checked.get("entry", []):
    for field in ("payload_root", "manifest_path"):
        value = pathlib.Path(entry[field])
        try:
            value.relative_to(final)
        except ValueError as error:
            raise SystemExit(f"normalised {field} is not under {final}: {value}") from error
PY

# The launcher refuses unsigned bundles and pins the system bundle to this
# collection's publisher.  This is package data, not mutable user state: an
# RPM upgrade replaces it together with the new runtime.  A locally altered
# trust policy intentionally makes a mismatched update fail closed.
python3 - "$stage/usr/lib/tilo-runtime/$runtime_version/bundle.toml" \
    "$stage/etc/tilo/runtime-bundle-trusted-publishers.txt" <<'PY'
import pathlib, sys, tomllib
manifest = tomllib.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
publisher = manifest.get("publisher_hex")
if not isinstance(publisher, str) or len(publisher) != 64:
    raise SystemExit("runtime bundle has no valid publisher key")
pathlib.Path(sys.argv[2]).write_text(publisher.lower() + "\n", encoding="utf-8")
PY

[[ -s $store/registry.toml ]]
find "$stage" -type d -exec chmod 0755 {} +
find "$stage" -type f -exec chmod 0644 {} +
find "$stage/usr/share/tilo-apps/payloads" -type f -path '*/bin/*' -exec chmod 0755 {} +
