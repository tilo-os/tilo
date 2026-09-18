#!/usr/bin/env bash
# Opt-in Rust linker for native Linux GNU CI builds; not a system ld replacement.
# Select only CARGO_TARGET_<NATIVE_TRIPLE>_LINKER, never both architecture targets.
set -euo pipefail

fail() { printf 'ci-native-mold: %s\n' "$*" >&2; exit 2; }
[[ $(uname -s) == Linux ]] || fail 'requires Linux'
case "$(uname -m)" in
    aarch64) arch=aarch64 ;;
    x86_64) arch=x86_64 ;;
    *) fail 'unsupported native architecture' ;;
esac
[[ ${TILO_CI_MOLD_TARGET:-} == "$arch-unknown-linux-gnu" ]] ||
    fail 'TILO_CI_MOLD_TARGET must explicitly match the native GNU Rust target'
threads=${TILO_CI_MOLD_THREADS:-2}
[[ $threads =~ ^[1-9][0-9]?$ && $threads -le 32 ]] ||
    fail 'TILO_CI_MOLD_THREADS must be between 1 and 32'
command -v mold >/dev/null || fail 'mold is missing from the builder'
command -v gcc >/dev/null || fail 'gcc is missing from the builder'
case "$(gcc -dumpmachine)" in
    "$arch"-*-linux-gnu|"$arch"-linux-gnu|"$arch"-redhat-linux) ;;
    *) fail 'gcc is not a matching native GNU Linux driver' ;;
esac
# Preserve all rustc-supplied hardening, object, library and runtime arguments.
# Last selection wins over rustc's default -fuse-ld, without replacing RUSTFLAGS.
exec gcc "$@" -fuse-ld=mold "-Wl,--threads=$threads"
