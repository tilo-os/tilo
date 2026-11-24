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
#       provides.
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

runtime_package_build() {
    local name=$1 bundle_bin=$2 bin_dir=$3 identity=$4 out=$5 version=${6:-}
    local repo archive work tree bundle release arch includes=()
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

    bundle="$out/runtime/$name/$release+$name"
    rm -rf -- "$bundle"
    mkdir -p "$(dirname -- "$bundle")"
    if [[ $name == python ]]; then
        # The standard library, at lib/python3.13 as CPython finds it from
        # $ORIGIN/../lib. The interpreter is linked statically, so no
        # libpython travels with it.
        includes+=(--include "$tree/python/lib/python3.13")
    fi
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
