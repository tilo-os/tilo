#!/usr/bin/env bash
# Materialise the immutable Tilo application store for a host-session package.
#
# The OS image has always shipped apps through the normal tapp -> tilo-install
# path. Reuse that format here: registry metadata remains in
# /usr/share/tilo-apps while architecture-dependent payloads live below
# /usr/lib/tilo-apps. User packages with the same id still win.
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
final_app_root=${TILO_STAGE_FINAL_APP_ROOT:-/usr/lib/tilo-apps}
final_runtime_root=${TILO_STAGE_FINAL_RUNTIME_ROOT:-/usr/lib/tilo-runtime}
final_registry_root=${TILO_STAGE_FINAL_REGISTRY_ROOT:-/usr/share/tilo-apps}
for final_root in "$final_app_root" "$final_runtime_root" "$final_registry_root"; do
    [[ $final_root == /* && $final_root != / && $final_root != *'/../'* && $final_root != */.. ]] || {
        echo "unsafe final app-store path: $final_root" >&2
        exit 1
    }
done
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# Use the same independently configured release authority as the OS image.
# Never discover a trust root from the catalogue being authenticated.
owner=${TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY:-}
[[ $owner =~ ^[0-9a-fA-F]{64}$ && $owner != $(printf '0%.0s' {1..64}) ]] || {
    echo "TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY must be a non-placeholder 32-byte hex key" >&2
    exit 1
}

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
# App collection v1 deliberately keeps the signed bundle below `runtime/`.
# This is the same tree transported by release-apps.yml and consumed by the
# image build; accepting a second RPM-only archive layout let the package
# fixture drift away from every collection produced by package-apps.sh.
collection_runtime=$work/runtime/tilo-runtime
[[ -f $work/index.json && -f $work/SHA256SUMS && -d $work/tapp && -d $collection_runtime ]] || {
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
[[ -f $collection_runtime/$runtime_version/bundle.toml ]] || {
    echo "collection runtime tilo-runtime/$runtime_version is missing bundle.toml" >&2
    exit 1
}
"$bin_dir/tilo-runtime-bundle" verify "$collection_runtime/$runtime_version" >/dev/null

mkdir -p "$stage/usr/share/tilo-apps" "$stage/usr/lib" "$stage/etc/tilo"
cp -a "$collection_runtime" "$stage/usr/lib/tilo-runtime"
store="$stage/usr/lib/tilo-apps"
registry="$stage/usr/share/tilo-apps/registry.toml"
mkdir -p "$store"

# Use a staging root for physical writes but the packaged system runtime for
# dependency resolution.  The registry is normalised to its final FHS path
# immediately below, and the normalisation is checked structurally rather than
# trusting a broad textual substitution.
export TILO_APPS_DATA_DIR="$store"
export TILO_RUNTIME_BUNDLE_DIR="$stage/usr/lib/tilo-runtime"
export TILO_CLI_SHIM_PATH="$bin_dir/tilo-cli-shim"
export TILO_LAUNCHER_PATH=/usr/bin/tilo-launcher
for package in "$work"/tapp/*.tapp; do
    "$bin_dir/tilo-install" install "$package" --allow-self-signed >/dev/null
done

python3 - "$store/registry.toml" "$registry" "$store" "$final_app_root" <<'PY'
import pathlib, sys, tomllib

source_registry, registry, staged, final = map(pathlib.Path, sys.argv[1:])
data = tomllib.loads(source_registry.read_text(encoding="utf-8"))
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
raw = source_registry.read_text(encoding="utf-8")
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
rm -- "$store/registry.toml"
# The payload trees are materialised; the system store is read-only and never
# installs again, so its content store, staging and writer lock do not ship.
rm -rf -- "$store/content" "$store/tmp" "$store/.lock"
# Each install above held the runtime collector off by sharing .gc.lock in the
# runtime root it resolved against (runtime-bundle's gc.rs), which created the
# file. Collection, its lock and its leases belong to the per-user root only:
# nothing locks or writes the read-only system root, so none of its state
# ships. Anything else hidden there is unexplained and fails the package.
runtime_root=$stage/usr/lib/tilo-runtime
rm -f -- "$runtime_root/.gc.lock"
runtime_state=$(find "$runtime_root" -mindepth 1 -maxdepth 1 -name '.*' -print)
[[ -z $runtime_state ]] || {
    echo "collector state left in the packaged runtime root: $runtime_state" >&2
    exit 1
}

# The collection's baked runtime and official Store runtimes have separate
# signers. Ship both pins, as the OS image does, so ordinary Store updates do
# not need a manual runtime trust grant. This remains package-owned policy.
python3 - "$stage/usr/lib/tilo-runtime/$runtime_version/bundle.toml" \
    "$stage/etc/tilo/runtime-bundle-trusted-publishers.txt" \
    "$repo_root/os/registry-runtime-publisher.hex" <<'PY'
import pathlib, re, sys, tomllib
manifest = tomllib.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
publisher = manifest.get("publisher_hex")
if not isinstance(publisher, str) or not re.fullmatch(r"[0-9a-fA-F]{64}", publisher):
    raise SystemExit("runtime bundle has no valid publisher key")
try:
    registry_publisher = pathlib.Path(sys.argv[3]).read_text(encoding="utf-8").strip()
except OSError as error:
    raise SystemExit(f"registry runtime publisher pin unavailable: {error}") from error
if not re.fullmatch(r"[0-9a-f]{64}", registry_publisher):
    raise SystemExit("registry runtime publisher pin is not a 64-character public key")
publishers = dict.fromkeys((publisher.lower(), registry_publisher))
pathlib.Path(sys.argv[2]).write_text("".join(key + "\n" for key in publishers), encoding="utf-8")
PY

install -d -m 0755 "$stage/usr/share/tilo/trust"
printf '%s\n' "${owner,,}" > "$stage/usr/share/tilo/trust/tilo-main-registry-owner.pub"
chmod 0644 "$stage/usr/share/tilo/trust/tilo-main-registry-owner.pub"

[[ -s $registry ]]
find "$stage" -type d -exec chmod 0755 {} +
find "$stage/usr/share/tilo-apps" "$stage/etc/tilo" -type f -exec chmod 0644 {} +
find "$stage/usr/lib/tilo-apps" -type f -exec chmod 0644 {} +
find "$stage/usr/lib/tilo-apps/pkgs" -type f -path '*/bin/*' -exec chmod 0755 {} +
find "$stage/usr/lib/tilo-apps/bin" -type f ! -name '*.tilo-shim' -exec chmod 0755 {} + 2>/dev/null || true
# Signed runtime metadata remains data, while loaders and shared objects need
# execute permission. Signatures cover bytes, not modes.
find "$stage/usr/lib/tilo-runtime" -type f -exec chmod 0644 {} +
find "$stage/usr/lib/tilo-runtime" -type f -exec sh -c '
    for path do
        [ "$(dd if="$path" bs=4 count=1 2>/dev/null)" != "$(printf "\177ELF")" ] || chmod 0755 "$path"
    done
' sh {} +

# The env-runtime environment image, so a first environment opens offline.
# Installed after the mode normalisation above: an image's tree keeps the
# execute bits and links its signature records, and nothing may flatten them.
# TILO_ENV_RUNTIME_DIR is the published image's /env-runtime directory
# (os/dev/fetch-env-runtime.sh); without one the package ships no runtime and
# environments fetch it on first use.
if [[ -n ${TILO_ENV_RUNTIME_DIR:-} ]]; then
    "$repo_root/os/scripts/install-env-runtime.sh" "$TILO_ENV_RUNTIME_DIR" \
        "$store" "$stage/usr/share/tilo-apps" "$bin_dir/tilo-install"
    [[ ! -e $stage/usr/share/tilo-apps/image-publishers ]] \
        || chmod 0644 "$stage/usr/share/tilo-apps/image-publishers"
else
    echo "warning: no TILO_ENV_RUNTIME_DIR; this package ships no env-runtime" >&2
fi
