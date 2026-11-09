#!/usr/bin/env bash
# Zig as the C compiler and the linker of a cross build of the `tilo` command
# (tools/ci/publish/cli-release.sh):
#
#   TILO_ZIG_TARGET=aarch64-linux-musl zig-cc.sh [compiler or linker arguments]
#
# Zig carries musl's headers and libc for every architecture, so a program built with it compiles
# and links against one musl and nothing of the machine's glibc.
#
# Zig names a target its own way (`aarch64-linux-musl`), while rustc and cc-rs pass the long
# triple (`aarch64-unknown-linux-musl`), which zig refuses to parse. Those arguments are dropped
# here: the target this wrapper was given is the one zig builds.
set -euo pipefail

: "${TILO_ZIG_TARGET:?TILO_ZIG_TARGET is the zig target to build for, such as aarch64-linux-musl}"

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
  args+=("$arg")
done
exec zig cc -target "$TILO_ZIG_TARGET" ${args[@]+"${args[@]}"}
