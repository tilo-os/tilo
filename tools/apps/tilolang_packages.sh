#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# tilolang_packages.sh -- the tilolang runtime bundle and the packages under
# apps/tl, in the one place every caller that ships them reads.
#
# A tilolang app is a package directory under `apps/tl`: `tilolang build`
# compiles it, stages its payload as a bundle and signs it as a `.tapp`. It
# runs on the tilolang runtime bundle
# (crates/lang/tilolang-process/tilo-runtime.toml), whose one binary,
# `tilolang-process`, is what a package's `[entrypoint]` executes and the host
# that runs a workspace's instances of it.
# The launcher resolves `tilolang/<version>` the way it resolves any runtime:
# to `<runtime root>/<version>`, beside the Tilo runtime build.
#
# Sourced, never run: the release's packing (dist/apps/package-apps.sh), an OS
# image's (os/scripts/pack-apps.sh) and the development scripts under dist/dev
# build and sign the same things, so they share these functions rather than
# each writing the invocation.
#
#   tilolang_std_digest REPO
#       The digest of the standard library in REPO (crates/lang/tilolang/std):
#       the first 16 hex digits of the SHA-256 of its app.tl followed by its
#       app.ids, as `tilolang::stdlib::digest` takes it.
#
#   tilolang_runtime_version REPO [TILO_RUNTIME]
#       The version the tilolang runtime bundle provides: the build its
#       manifest declares, keyed by the standard library it carries
#       (`<version>+tilolang.<std digest>`), and, baked against the exact Tilo
#       runtime build TILO_RUNTIME (`tilo-runtime/<release>+native.<digest>`),
#       by that build too (`<…>.<first 16 of its digest>`,
#       `runtime_bundle::linked_for`): the thin binary links that build's
#       libraries and runs on it alone. It is the `tilolang/<version>` every
#       package `tilolang build --tilo-runtime TILO_RUNTIME` makes from REPO
#       names, so a package always names the pair it was built with.
#
#   tilolang_bake_runtime BUNDLE_BIN BIN STD_TOOL REPO IDENTITY OUT VERSION [ARCH [LIBRARIES]]
#       Build the signed bundle of the thin `tilolang-process` binary BIN into
#       OUT (which becomes `<runtime root>/<version>`). The bundle holds the
#       binary (`--binaries-only`) and the standard library every package on
#       it runs beside, compiled from REPO by the host tool STD_TOOL
#       (`tilolang-std`, built for the machine that bakes: the module is wasm,
#       the same for every device) and kept as `lib/tilo.std.wasm`; a VERSION
#       keyed by another library is refused. The launcher runs the binary on
#       the Tilo runtime its libraries come from, so a loader set of its own is
#       never read. A Mac's runtime names the architectures of its universal
#       binary in ARCH (`aarch64+x86_64`) and brings the libraries it loads
#       relative to itself as the directory LIBRARIES, kept as `lib/<its name>/`.
#
#   tilolang_build_package TILOLANG_BIN REPO DIR NAME OUT IDENTITY VERSION TILO_RUNTIME [ID]
#       Compile the package at REPO/DIR with the packages it uses (searching
#       REPO/apps/tl), then stage, pack and sign it: OUT/NAME.tapp, named as
#       the collection names the app rather than by its last id segment. ID is
#       the package's app id when the caller already has it (a plan's), and is
#       read from DIR's `app` declaration otherwise.
# ---------------------------------------------------------------------------

# Fail with a line on stderr; callers run under `set -e`, so this ends them.
tilolang_die() {
    printf 'tilolang packages: %s\n' "$*" >&2
    return 1
}

tilolang_std_digest() {
    local std="$1/crates/lang/tilolang/std" digest
    [ -f "$std/app.tl" ] && [ -f "$std/app.ids" ] \
        || { tilolang_die "no standard library at $std"; return 1; }
    if command -v sha256sum >/dev/null; then
        digest=$(cat "$std/app.tl" "$std/app.ids" | sha256sum | cut -c1-16)
    else
        digest=$(cat "$std/app.tl" "$std/app.ids" | shasum -a 256 | cut -c1-16)
    fi
    [ ${#digest} -eq 16 ] || { tilolang_die "could not hash the standard library at $std"; return 1; }
    printf '%s\n' "$digest"
}

tilolang_runtime_version() {
    local repo=$1 tilo_runtime=${2:-} manifest version digest abi
    manifest="$repo/crates/lang/tilolang-process/tilo-runtime.toml"
    [ -f "$manifest" ] || { tilolang_die "no $manifest"; return 1; }
    version=$(sed -n 's/^provides *= *"tilolang\/\([^"]*\)".*/\1/p' "$manifest" | head -1)
    [ -n "$version" ] || { tilolang_die "$manifest provides no tilolang version"; return 1; }
    digest=$(tilolang_std_digest "$repo") || return 1
    abi=${tilo_runtime##*+native.}
    if [ "$abi" != "$tilo_runtime" ] && [ ${#abi} -ge 16 ]; then
        printf '%s.%s.%s\n' "$version" "$digest" "${abi:0:16}"
    else
        printf '%s.%s\n' "$version" "$digest"
    fi
}

tilolang_bake_runtime() {
    local bundle_bin=$1 bin=$2 std_tool=$3 repo=$4 identity=$5 out=$6 version=$7
    local arch=${8:-} libraries=${9:-} work digest
    [ -n "$version" ] || { tilolang_die "a tilolang runtime bundle needs its version"; return 1; }
    local extra=()
    [ -z "$arch" ] || extra+=(--arch "$arch")
    if [ -n "$libraries" ]; then
        [ -d "$libraries" ] || { tilolang_die "no runtime libraries at $libraries"; return 1; }
        extra+=(--include "$libraries")
    fi
    [ -f "$bin" ] || { tilolang_die "no tilolang runtime binary at $bin"; return 1; }
    [ -x "$bin" ] || chmod +x "$bin"
    [ -x "$std_tool" ] || { tilolang_die "no standard library compiler at $std_tool"; return 1; }
    work=$(mktemp -d) || return 1
    digest=$("$std_tool" "$repo/crates/lang/tilolang/std" --out "$work/tilo.std.wasm") \
        || { rm -rf -- "$work"; tilolang_die "the standard library in $repo does not compile"; return 1; }
    local keyed=${version##*+tilolang.}
    if [ "${keyed%%.*}" != "$digest" ]; then
        rm -rf -- "$work"
        tilolang_die "the runtime $version is keyed by another standard library than $repo's ($digest)"
        return 1
    fi
    rm -rf -- "$out"
    "$bundle_bin" build --binaries-only --version "$version" --out "$out" \
        --include-bin "$bin" --include "$work/tilo.std.wasm" --identity "$identity" \
        "${extra[@]}" >/dev/null \
        || { rm -rf -- "$work"; tilolang_die "could not bake the tilolang runtime bundle $version"; return 1; }
    rm -rf -- "$work"
}

tilolang_build_package() {
    local tilolang_bin=$1 repo=$2 dir=$3 name=$4 out=$5 identity=$6 version=$7
    local tilo_runtime=$8 id=${9:-} staging built
    [ -d "$repo/$dir" ] || { tilolang_die "no package at $repo/$dir"; return 1; }
    if [ -z "$id" ]; then
        id=$(sed -n 's/^[[:space:]]*app[[:space:]]\{1,\}\([^[:space:]]\{1,\}\).*/\1/p' \
            "$repo/$dir/app.tl" | head -1)
    fi
    [ -n "$id" ] || { tilolang_die "$dir declares no app id"; return 1; }
    # A staging directory of its own: one build also emits the tapps of the
    # packages it uses, which the collection installs under their own names.
    staging="$out/.$name"
    rm -rf -- "$staging"
    mkdir -p "$staging"
    "$tilolang_bin" build "$repo/$dir" --search "$repo/apps/tl" --out "$staging" \
        --identity "$identity" --version "$version" --tilo-runtime "$tilo_runtime" \
        || { tilolang_die "$dir did not build"; return 1; }
    built="$staging/${id##*.}.tapp"
    [ -f "$built" ] || { tilolang_die "the build of $dir made no $(basename "$built")"; return 1; }
    mv -f -- "$built" "$out/$name.tapp"
    rm -rf -- "$staging"
}
