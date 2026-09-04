#!/usr/bin/env bash
# Build selected apps in one Cargo invocation and emit independently installable
# qpkgs plus a machine-readable collection index.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
apps=${QOS_RELEASE_APPS:?space-separated app directory names are required}
out=${QOS_APP_RELEASE_OUT:-$repo_root/dist/app-release}
identity=${QPKG_PUBLISHER_IDENTITY_FILE:-}
runtime_identity=${RUNTIME_BUNDLE_IDENTITY_FILE:-}
source_epoch=${SOURCE_DATE_EPOCH:-$(git -C "$repo_root" log -1 --format=%ct)}
max_qpkg_bytes=${MAX_QPKG_BYTES:-10000000}

out=$(realpath -m "$out")
case "$out" in
    "$repo_root"/dist/*) ;;
    *) printf 'QOS_APP_RELEASE_OUT must be below %s/dist\n' "$repo_root" >&2; exit 1 ;;
esac
[[ ! -L $out ]] || { printf 'refusing symlink output directory: %s\n' "$out" >&2; exit 1; }
mkdir -p "$out/qpkg" "$out/work"
find "$out" -mindepth 1 -depth -delete
mkdir -p "$out/qpkg" "$out/work"

plan="$out/plan.json"
python3 "$repo_root/tools/app_release.py" --apps "$apps" > "$plan"
mapfile -t packages < <(jq -r '.packages[]' "$plan")
features=$(jq -r '.features | join(",")' "$plan")
(cd "$repo_root" && cargo build --release --locked -p pkg-cli -p runtime-bundle)
INCLUDE_PACKAGES="${packages[*]}" APP_FEATURES="$features" \
    "$repo_root/os/scripts/build-thin-apps.sh"
mapfile -t app_binaries < <(
    jq -r --arg root "$repo_root/target/thin-app" \
        '.catalog[] | $root + "/" + .binary' "$plan"
)
"$repo_root/os/scripts/verify-elf-hardening.sh" "${app_binaries[@]}"

runtime_version=$(jq -er '[.catalog[].runtime] | unique | if length == 1 then .[0] else error("apps require different runtimes") end | sub("^qos-runtime/"; "")' "$plan")
[[ $runtime_version =~ ^[0-9A-Za-z][0-9A-Za-z._+-]*$ ]]
if [[ -z $runtime_identity ]]; then
    runtime_identity="$out/work/runtime-publisher.json"
    "$repo_root/target/release/qos-runtime-bundle" key generate --out "$runtime_identity"
fi
runtime_dylib="$repo_root/target/thin-app/libruntime_dylib.so"
rust_libstd=$(find "$(rustc --print target-libdir)" -maxdepth 1 -name 'libstd-*.so' -print -quit)
[[ -f $runtime_dylib && -f $rust_libstd ]]
OUT_DIR="$out/runtime" RUNTIME_VERSION="$runtime_version" FROM=/ \
    SOURCE_DATE_EPOCH="$source_epoch" \
    ARCH="$(uname -m)" IDENTITY_JSON="$runtime_identity" \
    BUNDLE_BIN="$repo_root/target/release/qos-runtime-bundle" \
    EXTRA_LIBS="$runtime_dylib $rust_libstd" \
    "$repo_root/os/scripts/bake-runtime-bundle.sh"

if [[ -z $identity ]]; then
    identity="$out/work/publisher.json"
    "$repo_root/target/release/qos-runtime-bundle" key generate --out "$identity"
fi
pubkey=$("$repo_root/target/release/qos-runtime-bundle" key pubkey --identity "$identity")
[[ $pubkey =~ ^[0-9a-fA-F]{64}$ ]]

while IFS=$'\t' read -r name package binary id version manifest; do
    built="$repo_root/target/thin-app/$binary"
    [[ -x $built ]] || { printf 'missing app binary: %s\n' "$built" >&2; exit 1; }
    payload="$out/work/payload-$name"
    staged_manifest="$out/work/manifest-$name.toml"
    mkdir -p "$payload/bin"
    install -pm 0755 "$built" "$payload/bin/$binary"
    sed "s|^\(identity  *= *\)\"ed25519:[0-9a-fA-F]*\"|\1\"ed25519:$pubkey\"|" \
        "$repo_root/$manifest" > "$staged_manifest"
    qpkg="$out/qpkg/$name-$version-$(uname -m).qpkg"
    "$repo_root/target/release/qos-pkg" pack \
        --manifest "$staged_manifest" --payload "$payload" \
        --identity "$identity" --output "$qpkg"
    "$repo_root/target/release/qos-pkg" verify --trusted-key "$pubkey" "$qpkg"
    qpkg_bytes=$(stat -c %s "$qpkg")
    (( qpkg_bytes < max_qpkg_bytes )) || {
        printf '%s is %s bytes (limit %s); shared-runtime packaging regressed\n' \
            "$qpkg" "$qpkg_bytes" "$max_qpkg_bytes" >&2
        exit 1
    }
done < <(jq -r '.catalog[] | [.name,.package,.binary,.id,.version,.manifest] | @tsv' "$plan")

python3 - "$plan" "$out/qpkg" "$out/index.json" "$(uname -m)" "${GITHUB_SHA:-unknown}" <<'PY'
import hashlib, json, pathlib, sys
plan = json.load(open(sys.argv[1], encoding="utf-8"))
directory = pathlib.Path(sys.argv[2])
entries = []
for app in plan["catalog"]:
    path = next(directory.glob(f"{app['name']}-{app['version']}-*.qpkg"))
    entries.append({**app, "file": path.name, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()})
json.dump({"format": 1, "architecture": sys.argv[4], "source": sys.argv[5], "apps": entries},
          open(sys.argv[3], "w", encoding="utf-8"), indent=2, sort_keys=True)
PY
(cd "$out" && find qpkg runtime -type f -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS \
    && sha256sum index.json >> SHA256SUMS)
tar --sort=name --mtime="@$source_epoch" --owner=0 --group=0 --numeric-owner \
    -C "$out" -czf "$out/qos-apps-$(uname -m).tar.gz" qpkg runtime index.json SHA256SUMS
(
  cd "$out"
  sha256sum "qos-apps-$(uname -m).tar.gz" > "qos-apps-$(uname -m).tar.gz.sha256"
)
printf 'packaged %s app(s) in %s\n' "${#packages[@]}" "$out"
