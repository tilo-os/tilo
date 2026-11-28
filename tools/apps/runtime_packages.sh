#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# runtime_packages.sh -- the language runtimes an app's `[entrypoint] runtime`
# names, as the registry packages a device installs: bun and Python.
#
# A package writes a script (`[entrypoint] script`) that a runtime's binary
# runs (docs/app-development.md, "A program in JavaScript or Python"). The
# binary is not the app's and never the host's: it travels as a `kind =
# "runtime"` package (`so.tilo.runtime.bun`, `so.tilo.runtime.python`) whose
# payload is a signed runtime bundle of the interpreter
# (`tilo-runtime-bundle build --binaries-only --include-bin`). The launcher
# mounts that bundle and execs its binary over the package's script, exactly
# as it runs the tilolang runtime (tools/apps/tilolang_packages.sh).
#
# Every upstream release is pinned by version and SHA-256 in
# os/external-assets/github-assets.tsv and fetched here, at package-build
# time, through os/dev/cache-github-assets.sh, which refuses a byte that does
# not match. Nothing is taken from the build host.
#
# Each runtime also carries the SDK's native part, so an app the daemon starts
# imports the SDK from the runtime it already has instead of shipping a build
# of it in its own payload (see runtime_package_sdk). The binaries come from
# the build that makes the packing tools (BIN_DIR), so that sharing one Rust
# graph keeps one copy of what the SDK and the runtime link.
#
# Sourced, never run: dist/apps/package-apps.sh builds them on every
# architecture the release ships, and the `script-runtimes` t check builds
# them to hand the launcher's script-entrypoint tests a real package.
#
#   runtime_package_versions
#       The runtimes this file builds, one name each line.
#
#   runtime_package_provides NAME
#       The runtime NAME's package provides, `<name>/<release>+<name>`: the
#       spelling an app names in `[entrypoint] runtime` and `optional-runtimes`.
#
#   runtime_package_build NAME BUNDLE_BIN BIN_DIR IDENTITY OUT [VERSION]
#       Fetch NAME's pinned release for this machine's architecture, build the
#       signed runtime bundle into OUT/runtime/NAME/<release>+NAME, and seal it
#       as the registry package
#       OUT/runtime-package/language/NAME/<id>-<VERSION>-<arch>.tapp (VERSION
#       defaults to the package's own, the upstream release). Prints the
#       provides. BIN_DIR also holds the SDK's native part, built beside the
#       packing tools (`cargo build -p tilo-sdk-node-native
#       -p tilo-sdk-python-native -p tilo-automerge-isolate`).
# ---------------------------------------------------------------------------
runtime_package_die() {
    printf 'runtime packages: %s\n' "$*" >&2
    return 1
}

runtime_package_versions() {
    printf 'bun\npython\n'
}

# The upstream release each runtime package carries. Its SHA-256 is in
# os/external-assets/github-assets.tsv, beside the asset name below.
runtime_package_release() {
    case "${1:-}" in
        bun) printf '1.4.5\n' ;;
        python) printf '3.13.18\n' ;;
        *) runtime_package_die "no runtime package named ${1:-none}" ;;
    esac
}

runtime_package_id() {
    case "${1:-}" in
        bun | python) printf 'so.tilo.runtime.%s\n' "$1" ;;
        *) runtime_package_die "no runtime package named ${1:-none}" ;;
    esac
}

runtime_package_title() {
    case "${1:-}" in
        bun) printf 'bun runtime\n' ;;
        python) printf 'Python runtime\n' ;;
        *) runtime_package_die "no runtime package named ${1:-none}" ;;
    esac
}

# `<name>/<release>+<name>`: the release, then the runtime it is, since a
# device installs a bundle by version and a version carries what it belongs to.
runtime_package_provides() {
    local name=$1 release
    release=$(runtime_package_release "$name")
    printf '%s/%s+%s\n' "$name" "$release" "$name"
}

# The pinned asset holding NAME's interpreter for ARCH. Python's x86_64 build
# is the v3 one, which is the instruction set a Linux x86_64 runtime package
# declares in its `[package.isa]`.
runtime_package_asset() {
    local name=$1 arch=${2:-$(uname -m)}
    case "$name:$arch" in
        bun:x86_64) printf 'bun-1.4.5-linux-x64.zip\n' ;;
        bun:aarch64) printf 'bun-1.4.5-linux-aarch64.zip\n' ;;
        python:x86_64)
            printf 'cpython-3.13.18-20261123-x86_64_v3-unknown-linux-gnu-install_only.tar.gz\n' ;;
        python:aarch64)
            printf 'cpython-3.13.18-20261123-aarch64-unknown-linux-gnu-install_only.tar.gz\n' ;;
        uv:x86_64 | uv:aarch64)
            printf 'uv-%s-unknown-linux-gnu.tar.gz\n' "$arch" ;;
        *) runtime_package_die "no pinned $name release for $arch" ;;
    esac
}

runtime_package_repo() {
    local here
    here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
    cd -- "$here/../.." && pwd
}

# The pinned asset, verified, in the shared cache.
runtime_package_fetch() {
    local repo=$1 asset=$2 cache
    cache=${TILO_GITHUB_ASSET_CACHE:-$repo/os/external-assets/cache}
    TILO_GITHUB_ASSET_CACHE="$cache" bash "$repo/os/dev/cache-github-assets.sh" "$asset" >&2
    printf '%s/%s\n' "$cache" "$asset"
}

runtime_package_extract() {
    local archive=$1 dest=$2
    mkdir -p "$dest"
    case "$archive" in
        *.zip) python3 -I -m zipfile -e "$archive" "$dest" ;;
        *.tar.gz) tar --no-same-owner --no-same-permissions -xzf "$archive" -C "$dest" ;;
        *) runtime_package_die "no way to extract ${archive##*/}" ;;
    esac
}

# The pinned uv that installs a Python runtime's dependencies from the SDK's
# lock, the same tool the SDK image carries (os/Containerfile.ci-sdk): fetched
# from the asset table, so no image needs it and no version floats.
runtime_package_uv() {
    local repo=$1 arch=$2 work=$3 asset
    asset=$(runtime_package_asset uv "$arch")
    runtime_package_extract "$(runtime_package_fetch "$repo" "$asset")" "$work/uv"
    printf '%s/uv/%s/uv\n' "$work" "${asset%.tar.gz}"
}

# The interpreter, as a regular file at the name the package's entrypoint will
# execute: bun's archive is `bun-linux-<machine>/bun`, CPython's `python3` is a
# symlink to `python3.13`, which a bundle signs no part of.
runtime_package_interpreter() {
    local name=$1 arch=$2 tree=$3 dest=$4 machine
    case "$name" in
        bun)
            case "$arch" in
                x86_64) machine=x64 ;;
                aarch64) machine=aarch64 ;;
                *) runtime_package_die "no bun release for $arch" ;;
            esac
            install -D -m 0755 "$tree/bun-linux-$machine/bun" "$dest/bin/bun"
            ;;
        python)
            # CPython finds its standard library beside its own path
            # (`$ORIGIN/../lib`), which the bundle preserves.
            install -D -m 0755 "$tree/python/bin/python3.13" "$dest/bin/python3"
            ;;
        *) runtime_package_die "no interpreter layout for $name" ;;
    esac
}

# The npm platform name of the addon this machine builds: what
# sdk/typescript/native/index.js names a binary for and what
# sdk/typescript/native/npm/ has a package of its own for.
runtime_package_sdk_platform() {
    case "$(uname -s)-$(uname -m)" in
        Linux-x86_64) printf 'linux-x64-gnu\n' ;;
        Linux-aarch64) printf 'linux-arm64-gnu\n' ;;
        *) runtime_package_die "no SDK native addon for $(uname -s)-$(uname -m) yet" ;;
    esac
}

# Debug info is the build's, not the bundle's: strip a copy, never the build
# tree's own artifact, which a later step reads.
runtime_package_strip_debug() {
    command -v strip >/dev/null 2>&1 || return 0
    strip --strip-debug -- "$@" \
        || runtime_package_die "could not strip debug info from $*"
}

# The SDKs' native parts, staged where their language finds them in the
# bundle, so that an app the launcher starts imports the SDK from the runtime
# instead of carrying a build of it:
#
#   bun     <stage>/node_modules/tilo-sdk-native/{index.js,index.d.ts,
#           package.json,tilo-sdk-native.<platform>.node,tilo-automerge-worker}
#           bundled as lib/node_modules/…, found through NODE_PATH, which the
#           launcher sets for a script entrypoint whose runtime carries it.
#   python  <tree>/python/lib/python3.<minor>/site-packages/tilo/{…,
#           _native.abi3.so,tilo-automerge-worker}
#           inside the standard library include, where CPython looks for a
#           package in its own lib directory.
#
# Prints the directory to bundle, or nothing when the runtime carries none.
runtime_package_sdk() {
    local name=$1 repo=$2 bin_dir=$3 tree=$4 work=$5
    local platform dir libdir pkg
    case "$name" in
        bun)
            dir="$work/sdk/node_modules/tilo-sdk-native"
            runtime_package_sdk_have "$bin_dir" libtilo_sdk_node_native.so
            runtime_package_sdk_have "$bin_dir" tilo-automerge-worker
            platform=$(runtime_package_sdk_platform) || return 1
            install -D -m 0644 "$repo/sdk/typescript/native/index.js" "$dir/index.js"
            install -D -m 0644 "$repo/sdk/typescript/native/index.d.ts" "$dir/index.d.ts"
            install -D -m 0644 "$repo/sdk/typescript/native/package.json" "$dir/package.json"
            install -D -m 0755 "$bin_dir/libtilo_sdk_node_native.so" \
                "$dir/tilo-sdk-native.$platform.node"
            install -D -m 0755 "$bin_dir/tilo-automerge-worker" "$dir/tilo-automerge-worker"
            runtime_package_strip_debug "$dir/tilo-sdk-native.$platform.node" \
                "$dir/tilo-automerge-worker"
            printf '%s\n' "$work/sdk/node_modules"
            ;;
        python)
            # CPython's lib directory is named for its minor version, which the
            # release names: it is whichever one it extracted.
            set -- "$tree"/python/lib/python3.*
            [[ $# == 1 && -d $1 ]] \
                || runtime_package_die "unexpected Python lib layout: $*"
            libdir=$1
            runtime_package_sdk_have "$bin_dir" lib_native.so
            runtime_package_sdk_have "$bin_dir" tilo-automerge-worker
            pkg=$libdir/site-packages/tilo
            mkdir -p "$pkg"
            cp -a "$repo/sdk/python/src/tilo/." "$pkg/"
            # A checkout's own build output is not what this bundle signs.
            rm -f "$pkg"/_native*.so
            install -D -m 0755 "$bin_dir/lib_native.so" "$pkg/_native.abi3.so"
            install -D -m 0755 "$bin_dir/tilo-automerge-worker" "$pkg/tilo-automerge-worker"
            runtime_package_strip_debug "$pkg/_native.abi3.so" "$pkg/tilo-automerge-worker"
            # The SDK's dependencies, pinned by sdk/python's lock: a script app
            # imports `tilo`, which imports them. The pinned uv reads the lock
            # and the wheels land in the same site-packages, so the bundle
            # carries them and the app needs no environment of its own.
            local uv requirements
            uv=$(runtime_package_uv "$repo" "$(uname -m)" "$work")
            requirements="$work/sdk-requirements.txt"
            UV_CACHE_DIR="$work/uv-cache" "$uv" export --project "$repo/sdk/python" \
                --frozen --no-dev --no-emit-project --format requirements-txt >"$requirements"
            UV_CACHE_DIR="$work/uv-cache" "$uv" pip install \
                --python "$tree/python/bin/python3" --target "$libdir/site-packages" \
                --require-hashes --no-build -r "$requirements" >/dev/null
            rm -rf "$libdir/site-packages/bin"
            printf '%s\n' "$libdir"
            ;;
        *) runtime_package_die "no SDK native part for $name" ;;
    esac
}

runtime_package_sdk_have() {
    local bin_dir=$1 file=$2
    [[ -f $bin_dir/$file ]] \
        || runtime_package_die \
            "$bin_dir/$file is missing: build the SDK's native part beside the packing tools (\`cargo build -p tilo-sdk-node-native -p tilo-sdk-python-native -p tilo-automerge-isolate\`)"
}

runtime_package_build() {
    local name=$1 bundle_bin=$2 bin_dir=$3 identity=$4 out=$5 version=${6:-}
    local repo archive work tree bundle release arch includes=() sdk_include
    repo=$(runtime_package_repo)
    [ -n "$version" ] || version=$(runtime_package_release "$name")
    release=$(runtime_package_release "$name")
    arch=$(uname -m)
    archive=$(runtime_package_fetch "$repo" "$(runtime_package_asset "$name" "$arch")")

    work=$(mktemp -d "${TMPDIR:-/tmp}/tilo-runtime-package-$name.XXXXXX")
    if ! runtime_package_extract "$archive" "$work/extract"; then
        rm -rf -- "$work"
        runtime_package_die "could not extract ${archive##*/}"
    fi
    tree="$work/extract"
    runtime_package_interpreter "$name" "$arch" "$tree" "$work/root"

    # The SDK's native part, at the place this runtime's own resolution finds
    # a package: bun's module path, or CPython's site-packages in its lib dir.
    sdk_include=$(runtime_package_sdk "$name" "$repo" "$bin_dir" "$tree" "$work") || {
        rm -rf -- "$work"
        runtime_package_die "could not stage the SDK native part into the $name runtime"
    }
    [[ -z $sdk_include ]] || includes+=(--include "$sdk_include")

    # Python's include is its standard library, at lib/python3.13 as CPython
    # finds it from $ORIGIN/../lib, with the SDK's package in its
    # site-packages. The interpreter is linked statically, so no libpython
    # travels with it.
    bundle="$out/runtime/$name/$release+$name"
    rm -rf -- "$bundle"
    mkdir -p "$(dirname -- "$bundle")"
    if ! "$bundle_bin" build --binaries-only --version "$release+$name" --out "$bundle" \
        --include-bin "$work/root/bin"/* --identity "$identity" "${includes[@]}" >/dev/null
    then
        rm -rf -- "$work"
        runtime_package_die "could not bake the $name runtime bundle"
    fi
    rm -rf -- "$work"

    "$repo/tools/apps/pack-runtime-package.sh" "$bundle" "$(runtime_package_id "$name")" \
        "$(runtime_package_title "$name")" "$version" "$identity" "$bin_dir" \
        "$out/runtime-package/language/$name" >/dev/null \
        || runtime_package_die "could not seal the $name runtime package"
    runtime_package_provides "$name"
}
