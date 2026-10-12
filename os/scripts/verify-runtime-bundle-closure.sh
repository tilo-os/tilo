#!/usr/bin/env bash
# verify-runtime-bundle-closure.sh BUNDLE_DIR [THIN_APP...]
#
# The launcher runs a thin app with LD_LIBRARY_PATH set to the bundle's lib/
# and nothing else, so every shared object the bundle carries, and every thin
# app given, must resolve all of its NEEDED entries from that signed directory.
# A build host's system libraries must never hide an incomplete bundle.
# build-thin-apps.sh checks the apps against the build's
# own target dir, which always has libstd; an image whose bundle lacked it
# still shipped, and no thin app could start.
#
# The one exception is the dynamic loader: the launcher binds it from its
# canonical interpreter path (lib/lib64/ld-linux-x86-64.so.2,
# lib/lib/ld-linux-aarch64.so.1, ...) for INTERP, and the mapped interpreter
# answers libc's NEEDED on its soname, so a copy there satisfies that NEEDED
# too. The bundle also signs it at the top of lib/ for tools that follow
# NEEDED by name.
set -euo pipefail

bundle=${1:?usage: verify-runtime-bundle-closure.sh BUNDLE_DIR [THIN_APP...]}
shift
lib=$bundle/lib
fail() { printf 'verify-runtime-bundle-closure: %s\n' "$*" >&2; exit 1; }
# Whether the bundle signs NEEDED entry $1: at the top of lib/, or, for the
# loader, at one of the launcher's canonical interpreter paths
# (CANONICAL_LOADER_PATHS in crates/packaging/launcher/src/sandbox.rs).
signed() {
  [ -f "$lib/$1" ] && return 0
  case $1 in ld-linux*) ;; *) return 1 ;; esac
  local directory
  for directory in lib64 lib usr/lib64 lib/x86_64-linux-gnu lib/aarch64-linux-gnu; do
    [ -f "$lib/$directory/$1" ] && return 0
  done
  return 1
}

[ -d "$lib" ] || fail "$lib does not exist"
[ -f "$lib/libruntime_dylib.so" ] || fail "$lib has no libruntime_dylib.so"
compgen -G "$lib/libstd-*.so" >/dev/null || fail "$lib has no Rust libstd-*.so"

objects=()
while IFS= read -r -d '' object; do
  [ "$(head -c 4 "$object" | od -An -c | tr -d ' ')" = '177ELF' ] && objects+=("$object")
done < <(find "$lib" -maxdepth 1 -type f -name '*.so*' -print0)
objects+=("$@")
[ "${#objects[@]}" -gt 1 ] || fail "found no shared objects to check in $lib"

broken=0
for object in "${objects[@]}"; do
  [ -f "$object" ] || fail "$object does not exist"
  if ! report=$(readelf -dW "$object" 2>&1); then
    printf '%s\n%s\n' "$object: readelf failed" "$report" >&2
    broken=1
    continue
  fi
  while IFS= read -r needed; do
    if ! signed "$needed"; then
      printf '%s: signed runtime is missing %s\n' "$object" "$needed" >&2
      broken=1
    fi
  done < <(sed -n 's/.*(NEEDED).*\[\([^]]*\)\].*/\1/p' <<<"$report")
  # The libraries the runtime loads on first use (GLib, GStreamer;
  # tools/ci/runtime-dylib-lazy-link.py, #4020) must be signed beside it
  # as well: a media app would otherwise abort at its first media call.
  if ! lazy=$(readelf -W -p .tilo.lazy_needed "$object" 2>/dev/null); then
    printf '%s: readelf cannot read its lazily loaded libraries\n' "$object" >&2
    broken=1
    continue
  fi
  while IFS= read -r needed; do
    if [ ! -f "$lib/$needed" ]; then
      printf '%s: signed runtime is missing %s, which it loads on first use\n' "$object" "$needed" >&2
      broken=1
    fi
  done < <(sed -n 's/^ *\[ *[0-9a-f]*\] *\([^ ]*\) *$/\1/p' <<<"$lazy")
done
[ "$broken" -eq 0 ] || fail "the runtime bundle does not satisfy what it or its thin apps link against"

# Then resolve each thin app the way a launch does: the bundle's own loader at
# the app's interpreter path, the bundle's lib/ as its only library path.
# Every library must come from that lib/, and none twice from elsewhere.
for app in "$@"; do
  interpreter=$(readelf -lW "$app" | sed -n 's/.*\[Requesting program interpreter: \(.*\)\]/\1/p')
  [ -n "$interpreter" ] || fail "$app names no ELF interpreter"
  loader=$lib/${interpreter#/}
  [ -f "$loader" ] || fail "$app: the signed runtime lacks its interpreter $interpreter"
  if ! listing=$("$loader" --inhibit-cache --library-path "$lib" --list "$app" 2>&1); then
    printf '%s\n%s\n' "$app: the bundle's loader cannot resolve it" "$listing" >&2
    broken=1
    continue
  fi
  while read -r name arrow path _; do
    [ "$arrow" = '=>' ] || continue
    case $path in
      "$lib"/*) ;;
      *) printf '%s: %s resolves to %s, outside the signed runtime\n' "$app" "$name" "$path" >&2
         broken=1 ;;
    esac
  done <<<"$listing"
done
[ "$broken" -eq 0 ] || fail "a thin app does not load from the runtime bundle alone"
printf 'verify-runtime-bundle-closure: %d objects resolve against %s\n' "${#objects[@]}" "$lib"
