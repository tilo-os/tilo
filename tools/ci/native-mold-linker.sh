#!/usr/bin/env bash
# Opt-in Rust linker for native GNU/Linux CI builds.
set -euo pipefail

fail() {
    printf 'tilo-ci-mold: %s\n' "$*" >&2
    exit 2
}

[[ $(uname -s) == Linux ]] || fail 'requires Linux'
case "$(uname -m)" in
    aarch64) native_arch=aarch64 ;;
    x86_64) native_arch=x86_64 ;;
    *) fail 'unsupported native architecture' ;;
esac

[[ ${TILO_CI_MOLD_TARGET:-} == "$native_arch-unknown-linux-gnu" ]] ||
    fail 'TILO_CI_MOLD_TARGET must match the native GNU Rust target'

threads=${TILO_CI_MOLD_THREADS:-2}
[[ $threads =~ ^[1-9][0-9]?$ && $threads -le 32 ]] ||
    fail 'TILO_CI_MOLD_THREADS must be between 1 and 32'

command -v mold >/dev/null || fail 'mold is missing from the builder'
command -v gcc >/dev/null || fail 'gcc is missing from the builder'
case "$(gcc -dumpmachine)" in
    "$native_arch"-*-linux-gnu|"$native_arch"-linux-gnu|"$native_arch"-redhat-linux) ;;
    *) fail 'gcc is not a matching native GNU/Linux driver' ;;
esac

# Preserve every object, library, and hardening argument supplied by rustc.
# The final linker selection wins over any default injected by the driver.
exec gcc "$@" -fuse-ld=mold "-Wl,--threads=$threads"
