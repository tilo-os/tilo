#!/usr/bin/env bash
# Build selected apps in one Cargo invocation and emit independently installable
# qpkgs plus a machine-readable collection index.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
native_arch=$(uname -m)
# Use the same audited flags for compilation and the subsequently signed ISA
# declaration. ELF machine checks below independently reject a wrong-arch payload.
CARGO_ENCODED_RUSTFLAGS=$(python3 "$repo_root/tools/app_package_isa.py" flags \
    --root "$repo_root" --arch "$native_arch")
export CARGO_ENCODED_RUSTFLAGS
apps=${TILO_RELEASE_APPS:?space-separated app directory names are required}
out=${TILO_APP_RELEASE_OUT:-$repo_root/dist/app-release}
identity=${QPKG_PUBLISHER_IDENTITY_FILE:-}
runtime_identity=${RUNTIME_BUNDLE_IDENTITY_FILE:-}
source_epoch=${SOURCE_DATE_EPOCH:-0}
[[ $source_epoch =~ ^[0-9]+$ ]] || {
    printf 'SOURCE_DATE_EPOCH must be a non-negative integer\n' >&2
    exit 2
}
policy_max_qpkg_bytes=5242880
max_qpkg_bytes=${MAX_QPKG_BYTES:-$policy_max_qpkg_bytes}
[[ $max_qpkg_bytes =~ ^[0-9]+$ ]] \
    || { printf 'MAX_QPKG_BYTES must be an integer\n' >&2; exit 1; }
(( max_qpkg_bytes <= policy_max_qpkg_bytes )) \
    || { printf 'MAX_QPKG_BYTES cannot exceed the 5 MiB app policy\n' >&2; exit 1; }
target_dir=$(realpath -m "${CARGO_TARGET_DIR:-$repo_root/target}")
release_dir=$target_dir/release
thin_app_dir=$target_dir/thin-app

out=$(realpath -m "$out")
[[ $out != / && $out != "$repo_root" ]] || {
    printf 'refusing unsafe TILO_APP_RELEASE_OUT: %s\n' "$out" >&2
    exit 1
}
cursor=$out
while [[ $cursor != / ]]; do
    [[ ! -L $cursor ]] || {
        printf 'refusing symlinked TILO_APP_RELEASE_OUT component: %s\n' "$cursor" >&2
        exit 1
    }
    cursor=$(dirname "$cursor")
done
mkdir -p "$out/qpkg" "$out/work"
find "$out" -mindepth 1 -depth -delete
mkdir -p "$out/qpkg" "$out/work"

plan="$out/plan.json"
python3 "$repo_root/tools/app_release.py" --apps "$apps" > "$plan"
mapfile -t packages < <(jq -r '.packages[]' "$plan")
runtime_plan="$out/runtime-plan.json"
python3 "$repo_root/tools/app_release.py" --collection default > "$runtime_plan"
mapfile -t runtime_packages < <(jq -r '.packages[]' "$runtime_plan")
features=$(jq -r '.features | join(",")' "$runtime_plan")
(cd "$repo_root" && cargo build --release --locked -p pkg-cli -p runtime-bundle)
# The Rust dylib ABI includes dependency crate hashes and Cargo unifies
# features across the selected graph.  Always build the shared runtime against
# the complete default collection, even when only a few changed qpkgs will be
# emitted below.  This makes the runtime root independent of the partial app
# selection and therefore safe to combine with unchanged qpkgs.
INCLUDE_PACKAGES="${runtime_packages[*]}" APP_FEATURES="$features" \
    "$repo_root/os/scripts/build-thin-apps.sh"
mapfile -t app_binaries < <(
    jq -r --arg root "$thin_app_dir" \
        '.catalog[] | $root + "/" + .binary' "$plan"
)
"$repo_root/os/scripts/verify-elf-hardening.sh" "${app_binaries[@]}"

runtime_version=$(jq -er '[.catalog[].runtime] | unique | if length == 1 then .[0] else error("apps require different runtimes") end | sub("^tilo-runtime/"; "")' "$plan")
[[ $runtime_version =~ ^[0-9A-Za-z][0-9A-Za-z._+-]*$ ]]
if [[ -z $runtime_identity ]]; then
    runtime_identity="$out/work/runtime-publisher.json"
    "$release_dir/tilo-runtime-bundle" key generate --out "$runtime_identity"
fi
runtime_dylib="$thin_app_dir/libruntime_dylib.so"
rust_libstd=$(find "$(rustc --print target-libdir)" -maxdepth 1 -name 'libstd-*.so' -print -quit)
[[ -f $runtime_dylib && -f $rust_libstd ]]
# Keep the public runtime and publisher key under the root transported by
# release-apps.yml and consumed by assemble-collection.sh. Private identities
# stay in work/ (or the caller's external identity files), outside that root.
OUT_DIR="$out/runtime" RUNTIME_VERSION="$runtime_version" FROM=/ \
    SOURCE_DATE_EPOCH="$source_epoch" \
    ARCH="$(uname -m)" IDENTITY_JSON="$runtime_identity" \
    BUNDLE_BIN="$release_dir/tilo-runtime-bundle" \
    EXTRA_LIBS="$runtime_dylib $rust_libstd" \
    "$repo_root/os/scripts/bake-runtime-bundle.sh"

if [[ -z $identity ]]; then
    identity="$out/work/publisher.json"
    "$release_dir/tilo-runtime-bundle" key generate --out "$identity"
fi
pubkey=$("$release_dir/tilo-runtime-bundle" key pubkey --identity "$identity")
[[ $pubkey =~ ^[0-9a-fA-F]{64}$ ]]
# Capture the public trust inputs from this signing invocation. These are not
# inferred later from a downloaded package, and private identities stay in work/.
runtime_pubkey=$("$release_dir/tilo-runtime-bundle" key pubkey --identity "$runtime_identity")
[[ $runtime_pubkey =~ ^[0-9a-fA-F]{64}$ ]]
python3 - "$out/work/receipt-signers.json" "$pubkey" "$runtime_pubkey" <<'PY'
import json, pathlib, sys
pathlib.Path(sys.argv[1]).write_text(json.dumps({
    "app_publisher": sys.argv[2].lower(), "runtime_publisher": sys.argv[3].lower(),
}) + "\n")
PY

while IFS=$'\t' read -r name package binary id version manifest; do
    built="$thin_app_dir/$binary"
    [[ -x $built ]] || { printf 'missing app binary: %s\n' "$built" >&2; exit 1; }
    payload="$out/work/payload-$name"
    staged_manifest="$out/work/manifest-$name.toml"
    mkdir -p "$payload/bin"
    install -pm 0755 "$built" "$payload/bin/$binary"
    sed "s|^\(identity  *= *\)\"ed25519:[0-9a-fA-F]*\"|\1\"ed25519:$pubkey\"|" \
        "$repo_root/$manifest" > "$staged_manifest"
    python3 "$repo_root/tools/app_package_isa.py" stage --arch "$native_arch" \
        --manifest "$staged_manifest" --binary "$payload/bin/$binary" --output "$staged_manifest"
    qpkg="$out/qpkg/$name-$version-$(uname -m).qpkg"
    binary_bytes=$(stat -c %s "$built")
    printf 'packing app=%s binary=%s binary_bytes=%s output=%s\n' \
        "$name" "$built" "$binary_bytes" "$qpkg"
    if "$release_dir/tilo-pkg" pack \
        --manifest "$staged_manifest" --payload "$payload" \
        --identity "$identity" --output "$qpkg"; then
        :
    else
        pack_status=$?
        printf 'pack failed: app=%s output=%s exit_status=%s binary_bytes=%s limit=%s\n' \
            "$name" "$qpkg" "$pack_status" "$binary_bytes" "$max_qpkg_bytes" >&2
        exit "$pack_status"
    fi
    qpkg_bytes=$(stat -c %s "$qpkg")
    printf 'packed app=%s output=%s qpkg_bytes=%s limit=%s\n' \
        "$name" "$qpkg" "$qpkg_bytes" "$max_qpkg_bytes"
    "$release_dir/tilo-pkg" verify --trusted-key "$pubkey" "$qpkg"
    (( qpkg_bytes <= max_qpkg_bytes )) || {
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
    -C "$out" -czf "$out/tilo-apps-$(uname -m).tar.gz" qpkg runtime index.json SHA256SUMS
(
  cd "$out"
  sha256sum "tilo-apps-$(uname -m).tar.gz" > "tilo-apps-$(uname -m).tar.gz.sha256"
)
printf 'packaged %s app(s) in %s\n' "${#packages[@]}" "$out"
