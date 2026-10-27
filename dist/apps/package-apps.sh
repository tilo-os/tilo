#!/usr/bin/env bash
# Build selected apps in one Cargo invocation and emit independently installable
# tapps plus a machine-readable collection index.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
native_arch=$(uname -m)
# Use the same audited flags for compilation and the subsequently signed ISA
# declaration. ELF machine checks below independently reject a wrong-arch payload.
CARGO_ENCODED_RUSTFLAGS=$(python3 "$repo_root/tools/apps/app_package_isa.py" flags \
    --root "$repo_root" --arch "$native_arch")
export CARGO_ENCODED_RUSTFLAGS
# TILO_APP_RUNTIME_ONLY=1 (with TILO_APP_COMPILE_ONLY=1) compiles what every selection shares, the
# runtime and its engines in the default collection's graph, and the packing tools, but no app
# (tools/apps/t-release.sh thin-runtime).
runtime_only=${TILO_APP_RUNTIME_ONLY:-}
case $runtime_only in
    '') apps=${TILO_RELEASE_APPS:?space-separated app directory names are required} ;;
    1)
        [[ ${TILO_APP_COMPILE_ONLY:-} == 1 && -z ${TILO_RELEASE_APPS:-} && -z ${TILO_APP_PREBUILT:-} ]] || {
            printf 'TILO_APP_RUNTIME_ONLY compiles (TILO_APP_COMPILE_ONLY=1) no app (no TILO_RELEASE_APPS)\n' >&2
            exit 2
        }
        apps= ;;
    *) printf 'TILO_APP_RUNTIME_ONLY must be 1 or unset\n' >&2; exit 2 ;;
esac
out=${TILO_APP_RELEASE_OUT:-$repo_root/dist/app-release}
identity=${TAPP_PUBLISHER_IDENTITY_FILE:-}
runtime_identity=${RUNTIME_BUNDLE_IDENTITY_FILE:-}
source_epoch=${SOURCE_DATE_EPOCH:-0}
[[ $source_epoch =~ ^[0-9]+$ ]] || {
    printf 'SOURCE_DATE_EPOCH must be a non-negative integer\n' >&2
    exit 2
}
# The same byte count the Rust package checks compile in.
size_policy=$repo_root/crates/packaging/tilo-app-package/max-app-bytes
policy_max_tapp_bytes=$(<"$size_policy")
[[ $policy_max_tapp_bytes =~ ^[1-9][0-9]*$ ]] \
    || { printf '%s must hold a positive byte count\n' "$size_policy" >&2; exit 1; }
max_tapp_bytes=${MAX_TAPP_BYTES:-$policy_max_tapp_bytes}
[[ $max_tapp_bytes =~ ^[0-9]+$ ]] \
    || { printf 'MAX_TAPP_BYTES must be an integer\n' >&2; exit 1; }
(( max_tapp_bytes <= policy_max_tapp_bytes )) \
    || { printf 'MAX_TAPP_BYTES cannot exceed the %s-byte app policy\n' "$policy_max_tapp_bytes" >&2; exit 1; }
target_dir=$(realpath -m "${CARGO_TARGET_DIR:-$repo_root/target}")
# A directory with what an earlier compile-only run built (release/ with tilo-app-package and
# tilo-runtime-bundle, thin-app/ with the apps, the runtime and its engines): package and sign
# those without compiling, so the signing step runs no build script or compiler.
prebuilt=${TILO_APP_PREBUILT:-}
if [[ -n $prebuilt ]]; then
    [[ ${TILO_APP_COMPILE_ONLY:-} != 1 ]] \
        || { printf 'TILO_APP_PREBUILT and TILO_APP_COMPILE_ONLY exclude each other\n' >&2; exit 2; }
    target_dir=$(realpath -e "$prebuilt")
fi
# The profile the packing tools (tilo-app-package, tilo-runtime-bundle) are built with. A release
# builds them optimised; the size checks (tools/apps/t-release.sh sizes, app-size) take `dev`, which
# packs the same bytes and builds in seconds where an optimised link takes minutes.
tools_profile=${TILO_APP_TOOLS_PROFILE:-release}
case $tools_profile in
    release) tools_dir=release ;;
    dev) tools_dir=debug ;;
    *) printf 'TILO_APP_TOOLS_PROFILE must be release or dev\n' >&2; exit 2 ;;
esac
release_dir=$target_dir/$tools_dir
thin_app_dir=$target_dir/thin-app
# TILO_APP_TOOLS_BUILT=1: the packing tools are already in $release_dir, built by an earlier run
# (tools/apps/t-release.sh thin-runtime, whose outputs the size checks receive), so none is built.
tools_built=${TILO_APP_TOOLS_BUILT:-}
case $tools_built in
    '') ;;
    1)
        for tool in tilo-app-package tilo-runtime-bundle; do
            [[ -x $release_dir/$tool ]] \
                || { printf 'TILO_APP_TOOLS_BUILT: %s is not built\n' "$release_dir/$tool" >&2; exit 2; }
        done ;;
    *) printf 'TILO_APP_TOOLS_BUILT must be 1 or unset\n' >&2; exit 2 ;;
esac

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
mkdir -p "$out/tapp" "$out/work"
find "$out" -mindepth 1 -depth -delete
mkdir -p "$out/tapp" "$out/work"

plan="$out/plan.json"
if [[ $runtime_only == 1 ]]; then
    printf '{"packages": [], "catalog": []}\n' > "$plan"
else
    python3 "$repo_root/tools/apps/app_release.py" --apps "$apps" > "$plan"
fi
if [[ -n ${TILO_APP_RELEASE_SOURCE:-} ]]; then
    [[ ${TILO_APP_RELEASE_SEQUENCE:-} =~ ^[1-9][0-9]*$ ]] || {
        printf 'TILO_APP_RELEASE_SEQUENCE must be a positive integer\n' >&2
        exit 2
    }
    python3 "$repo_root/tools/apps/dev_app_versions.py" --root "$repo_root" \
        --source "$TILO_APP_RELEASE_SOURCE" --sequence "$TILO_APP_RELEASE_SEQUENCE" \
        --stable --plan "$plan"
elif [[ -n ${TILO_APP_DEV_SOURCE:-} ]]; then
    python3 "$repo_root/tools/apps/dev_app_versions.py" --root "$repo_root" \
        --source "$TILO_APP_DEV_SOURCE" --plan "$plan"
fi
mapfile -t packages < <(jq -r '.packages[]' "$plan")
runtime_plan="$out/runtime-plan.json"
python3 "$repo_root/tools/apps/app_release.py" --collection default > "$runtime_plan"
mapfile -t runtime_packages < <(jq -r '.packages[]' "$runtime_plan")
features=$(jq -r '.features | join(",")' "$runtime_plan")
if [[ -z $prebuilt ]]; then
# A compile that only reads the apps (TILO_APP_SKIP_TOOLS=1 with TILO_APP_COMPILE_ONLY, the
# app-binaries check) packs nothing, so it needs no packing tools.
if [[ ${TILO_APP_SKIP_TOOLS:-} != 1 && $tools_built != 1 ]]; then
    (cd "$repo_root" && cargo build --profile "$tools_profile" --locked -p tilo-app-package-cli -p runtime-bundle)
fi
# The Rust dylib ABI includes dependency crate hashes and Cargo unifies
# features across the selected graph.  Always resolve the shared runtime against
# the complete default collection, even when only a few changed tapps will be
# emitted below.  This makes the runtime root independent of the partial app
# selection and therefore safe to combine with unchanged tapps.
INCLUDE_PACKAGES="${runtime_packages[*]}" BUILD_PACKAGES="${packages[*]}" APP_FEATURES="$features" \
    BUILD_RUNTIME_ONLY="$runtime_only" "$repo_root/os/scripts/build-thin-apps.sh"
fi
[[ $runtime_only != 1 ]] || exit 0
mapfile -t app_binaries < <(
    jq -r --arg root "$thin_app_dir" \
        '.catalog[] | $root + "/" + .binary' "$plan"
)
"$repo_root/os/scripts/verify-elf-hardening.sh" "${app_binaries[@]}"
# Compiling needs no publisher identity; a caller can build here and sign in a
# second invocation, whose Cargo builds are then already fresh.
[[ ${TILO_APP_COMPILE_ONLY:-} != 1 ]] || exit 0
[[ ${TILO_APP_SKIP_TOOLS:-} != 1 ]] \
    || { printf 'TILO_APP_SKIP_TOOLS needs TILO_APP_COMPILE_ONLY: packing needs the tools\n' >&2; exit 2; }

runtime_release=$(jq -er '[.catalog[].runtime] | unique | if length == 1 then .[0] else error("apps require different runtimes") end | sub("^tilo-runtime/"; "")' "$plan")
[[ $runtime_release =~ ^[0-9A-Za-z][0-9A-Za-z._-]*$ ]]
if [[ -z $runtime_identity ]]; then
    runtime_identity="$out/work/runtime-publisher.json"
    "$release_dir/tilo-runtime-bundle" key generate --out "$runtime_identity"
fi
runtime_dylib="$thin_app_dir/libruntime_dylib.so"
rust_libstd=$(find "$(rustc --print target-libdir)" -maxdepth 1 -name 'libstd-*.so' -print -quit)
[[ -f $runtime_dylib && -f $rust_libstd ]]
# The runtime engines build-thin-apps.sh built with this runtime, whichever
# apps were selected: each is signed beside the runtime, which is
# what makes the runtime id cover it, and only its app loads it.
[[ -f $thin_app_dir/runtime-engines ]] || {
    printf 'build-thin-apps.sh left no runtime engine list in %s\n' "$thin_app_dir" >&2
    exit 1
}
runtime_engines=()
while IFS= read -r engine; do
    [[ $engine =~ ^lib[a-z0-9_]+\.so$ && -f $thin_app_dir/$engine ]] || {
        printf 'runtime engine %s is not a built library in %s\n' "$engine" "$thin_app_dir" >&2
        exit 1
    }
    runtime_engines+=("$thin_app_dir/$engine")
done < "$thin_app_dir/runtime-engines"
# The default SDK also serves osk/settings's native keyboard-map dependency.
# Include this fixed platform root for every selection: using only the emitted
# apps' dependencies would change the SDK identity for a partial release.
keyboard_libdir=$(pkg-config --variable=libdir xkbcommon)
keyboard_library="$keyboard_libdir/libxkbcommon.so.0"
[[ -n $keyboard_libdir && -f $keyboard_library ]] || {
    printf 'default SDK keyboard library is missing: %s\n' "$keyboard_library" >&2
    exit 1
}
# Every retained SDK owns the product catalog its apps were compiled against.
# Prepare verified bytes before signing; checkout cache paths are not available
# inside an installed app's private namespace.
runtime_fonts="$out/work/fonts"
python3 "$repo_root/tools/font-catalog.py" prepare \
    "$repo_root/crates/sdk/ui-core/assets/fonts" "$runtime_fonts" --scope product
# Keep the public runtime and publisher key under the root transported by
# release-apps.yml and consumed by assemble-collection.sh. Private identities
# stay in work/ (or the caller's external identity files), outside that root.
OUT_DIR="$out/runtime" RUNTIME_RELEASE="$runtime_release" FROM=/ \
    VERSION_OUT="$out/work/runtime-version" \
    SOURCE_DATE_EPOCH="$source_epoch" \
    ARCH="$(uname -m)" IDENTITY_JSON="$runtime_identity" \
    BUNDLE_BIN="$release_dir/tilo-runtime-bundle" \
    FONT_CATALOG_ROOT="$runtime_fonts" \
    ACCOUNT_CAPABILITY_PROBE="$thin_app_dir/tilo-runtime-account-capability" \
    EXTRA_LIBS="$runtime_dylib ${runtime_engines[*]} $rust_libstd $keyboard_library" \
    "$repo_root/os/scripts/bake-runtime-bundle.sh"
# Every app below links this exact runtime build and names it.
runtime_version=$(<"$out/work/runtime-version")
runtime_id="tilo-runtime/$runtime_version"
"$repo_root/os/scripts/verify-runtime-bundle-closure.sh" \
    "$out/runtime/tilo-runtime/$runtime_version" "${app_binaries[@]}"

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
    # The manifest is staged in a directory of its own with the app's
    # search/*.ftl beside it: the packer joins their keywords and tag line
    # into the signed manifest from the directory the manifest is in.
    staged_dir="$out/work/manifest-$name"
    staged_manifest="$staged_dir/tilo-app.toml"
    mkdir -p "$payload/bin" "$staged_dir"
    app_dir="$repo_root/$(dirname "$manifest")"
    if [[ -d $app_dir/search ]]; then
        rm -rf "$staged_dir/search"
        cp -R "$app_dir/search" "$staged_dir/search"
    fi
    # The Store's gallery shows the screenshots the package carries.
    if [[ -d $app_dir/screenshots ]]; then
        mkdir -p "$payload/screenshots"
        find "$app_dir/screenshots" -maxdepth 1 -type f \
            \( -iname '*.png' -o -iname '*.jpg' -o -iname '*.jpeg' -o -iname '*.webp' \) \
            -exec install -pm 0644 -t "$payload/screenshots" {} +
    fi
    install -pm 0755 "$built" "$payload/bin/$binary"
    # The app's public surface ships at the payload root, as `tilo-app package`
    # stages it: the launcher names that file to the app, which reads it from
    # there, and relays fetch it from the registry by the hash its ops name.
    surface="$repo_root/$(dirname "$manifest")/surface.toml"
    if [[ -f $surface ]]; then
        install -pm 0644 "$surface" "$payload/surface.toml"
    fi
    sed "s|^\(identity  *= *\)\"ed25519:[0-9a-fA-F]*\"|\1\"ed25519:$pubkey\"|" \
        "$repo_root/$manifest" > "$staged_manifest"
    python3 "$repo_root/tools/apps/dev_app_versions.py" --manifest "$staged_manifest" --version "$version"
    "$repo_root/os/scripts/stamp-runtime.sh" "$staged_manifest" "$runtime_id"
    python3 "$repo_root/tools/apps/app_package_isa.py" stage --arch "$native_arch" \
        --manifest "$staged_manifest" --binary "$payload/bin/$binary" --output "$staged_manifest"
    tapp="$out/tapp/$name-$version-$(uname -m).tapp"
    binary_bytes=$(stat -c %s "$built")
    printf 'packing app=%s binary=%s binary_bytes=%s output=%s\n' \
        "$name" "$built" "$binary_bytes" "$tapp"
    if "$release_dir/tilo-app-package" pack \
        --manifest "$staged_manifest" --payload "$payload" \
        --identity "$identity" --output "$tapp"; then
        :
    else
        pack_status=$?
        printf 'pack failed: app=%s output=%s exit_status=%s binary_bytes=%s limit=%s\n' \
            "$name" "$tapp" "$pack_status" "$binary_bytes" "$max_tapp_bytes" >&2
        exit "$pack_status"
    fi
    tapp_bytes=$(stat -c %s "$tapp")
    # What a client downloads: the compressed twin the registry publishes
    # beside the .tapp. Measured here, then dropped; publishing writes its own.
    twin="$out/work/$name.tapp.zst"
    "$release_dir/tilo-app-package" compress --output "$twin" "$tapp" >/dev/null
    zst_bytes=$(stat -c %s "$twin")
    rm -f "$twin"
    printf 'packed app=%s output=%s tapp_bytes=%s limit=%s zst_bytes=%s\n' \
        "$name" "$tapp" "$tapp_bytes" "$max_tapp_bytes" "$zst_bytes"
    "$release_dir/tilo-app-package" verify --trusted-key "$pubkey" "$tapp"
    (( tapp_bytes <= max_tapp_bytes )) || {
        printf '%s is %s bytes (limit %s); shared-runtime packaging regressed\n' \
            "$tapp" "$tapp_bytes" "$max_tapp_bytes" >&2
        exit 1
    }
done < <(jq -r '.catalog[] | [.name,.package,.binary,.id,.version,.manifest] | @tsv' "$plan")

# The runtime itself, as the registry offers it: a `kind = "runtime"`
# package whose payload is the signed bundle above, split into its thin
# form plus content-addressed payload objects that are uploaded beside it.
runtime_package_version=0.1.0
if [[ -n ${TILO_APP_RELEASE_SOURCE:-} ]]; then
    runtime_package_version="0.1.${TILO_APP_RELEASE_SEQUENCE}+g${TILO_APP_RELEASE_SOURCE}"
fi
"$repo_root/tools/apps/pack-runtime-package.sh" \
    "$out/runtime/tilo-runtime/$runtime_version" so.tilo.runtime "Tilo runtime" \
    "$runtime_package_version" "$identity" "$release_dir" "$out/runtime-package" >/dev/null

# Every language other than English, as the registry offers it: one signed
# `kind = "language-pack"` package per locale, data for every architecture
# (either build signs the same bytes), beside its registry listing. A size
# check, which packages one app, leaves them out.
if [[ -z ${TILO_APP_SKIP_LANGUAGE_PACKS:-} ]]; then
    python3 "$repo_root/tools/apps/language_packs.py" pack "$repo_root" "$out/language-packs" \
        "$runtime_package_version" "$identity" "$pubkey" "$release_dir/tilo-app-package" >/dev/null
fi

python3 - "$plan" "$out/tapp" "$out/index.json" "$(uname -m)" "${GITHUB_SHA:-unknown}" "$runtime_id" <<'PY'
import hashlib, json, pathlib, sys
plan = json.load(open(sys.argv[1], encoding="utf-8"))
directory = pathlib.Path(sys.argv[2])
entries = []
for app in plan["catalog"]:
    path = next(directory.glob(f"{app['name']}-{app['version']}-*.tapp"))
    entries.append({**app, "runtime": sys.argv[6], "file": path.name,
                    "sha256": hashlib.sha256(path.read_bytes()).hexdigest()})
json.dump({"format": 1, "architecture": sys.argv[4], "source": sys.argv[5], "apps": entries},
          open(sys.argv[3], "w", encoding="utf-8"), indent=2, sort_keys=True)
PY
(cd "$out" && find tapp runtime -type f -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS \
    && sha256sum index.json >> SHA256SUMS)
tar --sort=name --mtime="@$source_epoch" --owner=0 --group=0 --numeric-owner \
    -C "$out" -czf "$out/tilo-apps-$(uname -m).tar.gz" tapp runtime index.json SHA256SUMS
(
  cd "$out"
  sha256sum "tilo-apps-$(uname -m).tar.gz" > "tilo-apps-$(uname -m).tar.gz.sha256"
)
printf 'packaged %s app(s) in %s\n' "${#packages[@]}" "$out"
