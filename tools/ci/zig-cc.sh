#!/usr/bin/env bash
# Zig as the C compiler and the linker of a cross build of the `tilo` command
# (tools/ci/publish/cli-release.sh):
#
#   TILO_ZIG_TARGET=aarch64-linux-musl zig-cc.sh [compiler or linker arguments]
#
# Zig carries musl's headers and libc for every architecture, so a program built with it compiles
# and links against one musl and nothing of the machine's glibc.
#
# Zig names a target its own way (`aarch64-linux-musl`, `aarch64-macos.11.0`), while rustc and
# cc-rs pass the long triple (`aarch64-unknown-linux-musl`, `aarch64-apple-darwin`), which zig
# refuses to parse. Those arguments are dropped here: the target this wrapper was given is the one
# zig builds.
#
# Zig's driver also refuses some linker flags a Mac asks for, and passes no unknown flag on. A
# library of the graph (a crate that is both an rlib and a cdylib) is linked with
# `-exported_symbols_list`, which only hides the dylib's other symbols: it is dropped, and that
# dylib -- nothing of the program's -- exports everything it defines. `-bind_at_load` is refused
# the same way; a call the program makes into a framework binds at its first use instead of at
# launch.
set -euo pipefail

: "${TILO_ZIG_TARGET:?TILO_ZIG_TARGET is the zig target to build for, such as aarch64-linux-musl}"

darwin=
case $TILO_ZIG_TARGET in *macos*) darwin=1 ;; esac

args=()
skip=0
for arg in "$@"; do
  if ((skip)); then
    skip=0
    continue
  fi
  case $arg in
    -target | --target)
      skip=1
      continue
      ;;
    -target=* | --target=*) continue ;;
  esac
  if [[ -n $darwin ]]; then
    case $arg in
      -Wl,-exported_symbols_list)
        skip=1
        continue
        ;;
      -Wl,-exported_symbols_list=* | -Wl,-bind_at_load | -Wl,-bind_at_load=*) continue ;;
    esac
  fi
  args+=("$arg")
done
exec zig cc -target "$TILO_ZIG_TARGET" ${args[@]+"${args[@]}"}
