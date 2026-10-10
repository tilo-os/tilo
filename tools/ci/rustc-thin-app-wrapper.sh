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
        # writes an address-significance table, so lld's safe mode folds
        # exactly what Rust allows: functions and constants (unnamed_addr),
        # never a `static`, whose address Rust keeps unique. lld links x86_64
        # Linux by default; elsewhere the wrapper asks for it when installed,
        # and links as before without it.
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
                if command -v ld.lld >/dev/null 2>&1; then
                    folding=(-C llvm-args=--addrsig -C link-arg=-fuse-ld=lld -C link-arg=-Wl,--icf=safe)
                fi
                ;;
        esac
        exec "${compiler_command[@]}" "$@" -C prefer-dynamic -C opt-level=z -C codegen-units=1 \
            -C llvm-args=--enable-merge-functions \
            -C llvm-args=--enable-machine-outliner=always \
            -C link-arg=-Wl,-z,pack-relative-relocs \
            "${folding[@]}" "${link_map[@]}"
        ;;
esac

if [ "$crate_name" = "runtime_dylib" ]; then
    # Every app process applies this library's dynamic relocations before
    # main (tools/perf/runtime-size-budget/). Packed relative
    # relocations shrink its ~360k R_*_RELATIVE entries from 24 bytes each to
    # a bitmap, as for the apps above. -Bsymbolic-functions binds the
    # library's calls and function pointers to its own functions at link
    # time: Rust never interposes them, apps are PIE (no canonical PLT
    # addresses), and it turns ~60k symbol lookups per app start into
    # relative relocations or direct calls. Data symbols stay preemptible.
    #
    # GLib and GStreamer load on first use, not at every app's exec: runtime-dylib-lazy-link.py links them out of DT_NEEDED and in
    # through hidden trampolines. It runs the linker cargo chose (the last
    # `-C linker=`, else cc) for the real link. The flavor stays the target's
    # default, as it is for any linker name rustc does not recognise. The
    # same link drops the code no app can reach (sections nothing exported
    # uses, and the macro-only crates syn, quote and proc-macro2).
    lazy_link=()
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
        lazy_link=(-C "linker=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/runtime-dylib-lazy-link.py")
    fi
    exec "${compiler_command[@]}" "$@" -C prefer-dynamic \
        -C link-arg=-Wl,-z,pack-relative-relocs \
        -C link-arg=-Wl,-Bsymbolic-functions \
        ${lazy_link[@]+"${lazy_link[@]}"}
fi

# A runtime engine (crates/runtime/engines/*) is a Rust dylib
# chained on the runtime: one app maps it beside libruntime_dylib.so. It
# needs prefer-dynamic to link against the runtime at all (a dylib that links
# another Rust dylib otherwise carries a second libstd), and the runtime's
# relocation flags for the same reasons.
case "$crate_name" in
    tilo_*_engine)
        exec "${compiler_command[@]}" "$@" -C prefer-dynamic \
            -C link-arg=-Wl,-z,pack-relative-relocs \
            -C link-arg=-Wl,-Bsymbolic-functions
        ;;
esac
exec "${compiler_command[@]}" "$@"
