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
#   tilolang_runtime_version REPO
#       The version the tilolang runtime bundle provides, from the manifest
#       that declares it (the `tilolang/<version>` every package names in
#       `optional-runtimes`).
#
#   tilolang_bake_runtime BUNDLE_BIN BIN IDENTITY OUT VERSION
#       Build the signed bundle of the thin `tilolang-process` binary BIN into
#       OUT (which becomes `<runtime root>/<version>`). The bundle holds the
#       binary alone (`--binaries-only`): the launcher runs it on the Tilo
#       runtime its libraries come from, so a loader set of its own is never
#       read.
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

tilolang_runtime_version() {
    local repo=$1 manifest version
    manifest="$repo/crates/lang/tilolang-process/tilo-runtime.toml"
    [ -f "$manifest" ] || { tilolang_die "no $manifest"; return 1; }
    version=$(sed -n 's/^provides *= *"tilolang\/\([^"]*\)".*/\1/p' "$manifest" | head -1)
    [ -n "$version" ] || { tilolang_die "$manifest provides no tilolang version"; return 1; }
    printf '%s\n' "$version"
}

tilolang_bake_runtime() {
    local bundle_bin=$1 bin=$2 identity=$3 out=$4 version=$5
    [ -n "$version" ] || { tilolang_die "a tilolang runtime bundle needs its version"; return 1; }
    [ -f "$bin" ] || { tilolang_die "no tilolang runtime binary at $bin"; return 1; }
    [ -x "$bin" ] || chmod +x "$bin"
    rm -rf -- "$out"
    "$bundle_bin" build --binaries-only --version "$version" --out "$out" \
        --include-bin "$bin" --identity "$identity" >/dev/null \
        || { tilolang_die "could not bake the tilolang runtime bundle $version"; return 1; }
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
