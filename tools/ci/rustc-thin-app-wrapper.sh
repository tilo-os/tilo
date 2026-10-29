#!/usr/bin/env bash
# Add prefer-dynamic only to the selected application binaries and the shared
# runtime dylib. App-local code is optimized for size so independently
# distributed tapps stay below the application size ceiling; the shared runtime
# remains speed-optimized.
# Cargo invokes this as RUSTC_WORKSPACE_WRAPPER, so third-party dependencies and
# build scripts retain the workspace's ordinary hardening and never become host
# tools that unexpectedly require Rust shared libraries.
set -euo pipefail

compiler="$1"
shift
compiler_command=("$compiler")
if [[ -n ${TILO_THIN_SCCACHE:-} ]]; then
    compiler_command=("$TILO_THIN_SCCACHE" "$compiler")
fi
crate_name=""
previous=""
for argument in "$@"; do
    if [ "$previous" = "--crate-name" ]; then
        crate_name="$argument"
        break
    fi
    previous="$argument"
done

case ":${TILO_THIN_BIN_CRATES:-}:" in
    *":${crate_name}:"*)
        # An app whose code is its engine library (Settings) names that
        # library itself (`#[link]`): the engine depends on the app's package,
        # so Cargo cannot hand it over as a crate. The linker finds it where
        # Cargo writes every dylib; build-thin-apps.sh links the engines
        # before the apps.
        engine_dirs=()
        previous=""
        for argument in "$@"; do
            case "$previous:$argument" in
                -L:dependency=*) engine_dirs+=(-L "native=${argument#dependency=}") ;;
                *:-Ldependency=*) engine_dirs+=(-L "native=${argument#-Ldependency=}") ;;
            esac
            previous="$argument"
        done
        link_map=()
        if [ -n "${TILO_THIN_LINK_MAP_DIR:-}" ]; then
            mkdir -p "$TILO_THIN_LINK_MAP_DIR"
            link_map+=("-C" "link-arg=-Wl,-Map=$TILO_THIN_LINK_MAP_DIR/$crate_name.map")
        fi
        # Release validation raises the parent profile's codegen-unit count to
        # keep the shared runtime build fast. A thin app inherits that parent
        # override unless it is reset here, which made Music's tapp exceed the
        # wire limit despite the declared thin-app profile using one
        # unit. Keep parallel codegen for the large runtime, but optimize each
        # independently shipped executable as one unit.
        # Packed relative relocations (DT_RELR, glibc >= 2.36) store the PIE's
        # ~20k R_X86_64_RELATIVE entries as a bitmap instead of 24 bytes each;
        # for Settings that is ~490 KB of .rela.dyn per tapp.
        #
        # Identical code folding: generic code instantiated for different
        # types is often byte-identical, and so are its unwind entries. LLVM
        # writes an address-significance table, so the safe mode of lld and
        # mold folds exactly what Rust allows: functions and constants
        # (unnamed_addr), never a `static`, whose address Rust keeps unique.
        # lld links x86_64 Linux by default; elsewhere the wrapper asks for
        # lld or mold, whichever is installed (the OS image's builder has only
        # mold), and links as before with neither.
        # A developer session's build (TILO_THIN_OPTIMIZE=speed, the thin-dev
        # profile) is never published: the executable keeps the profile's
        # optimisation and parallel codegen, and links without the size passes.
        if [ "${TILO_THIN_OPTIMIZE:-size}" = speed ]; then
            exec "${compiler_command[@]}" "$@" -C prefer-dynamic \
                -C link-arg=-Wl,-z,pack-relative-relocs ${link_map[@]+"${link_map[@]}"} \
                ${engine_dirs[@]+"${engine_dirs[@]}"}
        fi
        folding=()
        target=""
        previous=""
        for argument in "$@"; do
            [ "$previous" = "--target" ] && target="$argument"
            previous="$argument"
        done
        case "${target:-$(uname -m)-$(uname -s)}" in
            x86_64*[Ll]inux*) folding=(-C llvm-args=--addrsig -C link-arg=-Wl,--icf=safe) ;;
            *[Ll]inux*)
                for linker in lld mold; do
                    if command -v "ld.$linker" >/dev/null 2>&1; then
                        folding=(-C llvm-args=--addrsig -C "link-arg=-fuse-ld=$linker" -C link-arg=-Wl,--icf=safe)
                        break
                    fi
                done
                ;;
        esac
        exec "${compiler_command[@]}" "$@" -C prefer-dynamic -C opt-level=z -C codegen-units=1 \
            -C llvm-args=--enable-merge-functions \
            -C llvm-args=--enable-machine-outliner=always \
            -C link-arg=-Wl,-z,pack-relative-relocs \
            "${folding[@]}" "${link_map[@]}" ${engine_dirs[@]+"${engine_dirs[@]}"}
        ;;
esac

# The runtime and its engines link through runtime-dylib-lazy-link.py, which
# runs the linker cargo chose (the last `-C linker=`, else cc) for the real
# link. The flavor stays the target's default, as it is for any linker name
# rustc does not recognise.
dylib_link=()
if [ "$(uname -s)" = Linux ]; then
    chosen=""
    previous=""
    for argument in "$@"; do
        case "$previous:$argument" in
            -C:linker=*) chosen="${argument#linker=}" ;;
            *:-Clinker=*) chosen="${argument#-Clinker=}" ;;
        esac
        previous="$argument"
    done
    export TILO_LAZY_LINK_CC="${chosen:-cc}"
    dylib_link=(-C "linker=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/runtime-dylib-lazy-link.py")
fi

if [ "$crate_name" = "runtime_dylib" ]; then
    # Every app process applies this library's dynamic relocations before
    # main (#4000; tools/perf/runtime-size-budget/). Packed relative
    # relocations shrink its ~360k R_*_RELATIVE entries from 24 bytes each to
    # a bitmap, as for the apps above. -Bsymbolic binds the library's
    # references to its own functions and data at link time: Rust never
    # interposes a symbol, and apps are PIE that reach the runtime's data
    # through the GOT, never by a copy relocation (build-thin-apps.sh fails
    # an app or engine with one), so nothing else can define them. With
    # functions alone (-Bsymbolic-functions) the loader still looked up
    # ~3.8k of the library's own data symbols (vtables, statics, Wayland
    # interface tables) in every app process before main.
    #
    # GLib and GStreamer load on first use, not at every app's exec:
    # runtime-dylib-lazy-link.py links them out of DT_NEEDED and in
    # through hidden trampolines. The same link drops the code no app can
    # reach (sections nothing exported uses, and the macro-only crates syn,
    # quote and proc-macro2).
    export TILO_LAZY_LINK_ROLE=runtime
    exec "${compiler_command[@]}" "$@" -C prefer-dynamic \
        -C link-arg=-Wl,-z,pack-relative-relocs \
        -C link-arg=-Wl,-Bsymbolic \
        ${dylib_link[@]+"${dylib_link[@]}"}
fi

# A runtime engine (crates/runtime/engines/*) is a Rust dylib
# chained on the runtime: one app maps it beside libruntime_dylib.so. It
# needs prefer-dynamic to link against the runtime at all (a dylib that links
# another Rust dylib otherwise carries a second libstd), and the runtime's
# relocation flags for the same reasons. Its link drops the code nothing it
# exports reaches. An engine no crate compiles against
# (TILO_THIN_ENTRY_ENGINES, which build-thin-apps.sh reads from the
# workspace) exports only its unmangled entry points: rustc would export
# every item of every crate linked into it, Settings' whole library included.
case "$crate_name" in
    tilo_*_engine)
        case ":${TILO_THIN_ENTRY_ENGINES:-}:" in
            *":${crate_name}:"*) export TILO_LAZY_LINK_ROLE=entry-engine ;;
            *) export TILO_LAZY_LINK_ROLE=engine ;;
        esac
        exec "${compiler_command[@]}" "$@" -C prefer-dynamic \
            -C link-arg=-Wl,-z,pack-relative-relocs \
            -C link-arg=-Wl,-Bsymbolic \
            ${dylib_link[@]+"${dylib_link[@]}"}
        ;;
esac
exec "${compiler_command[@]}" "$@"
