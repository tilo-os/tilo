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
set -euo pipefail

bundle=${1:?usage: verify-runtime-bundle-closure.sh BUNDLE_DIR [THIN_APP...]}
shift
lib=$bundle/lib
fail() { printf 'verify-runtime-bundle-closure: %s\n' "$*" >&2; exit 1; }

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
    if [ ! -f "$lib/$needed" ]; then
      printf '%s: signed runtime is missing %s\n' "$object" "$needed" >&2
      broken=1
    fi
  done < <(sed -n 's/.*(NEEDED).*\[\([^]]*\)\].*/\1/p' <<<"$report")
  # The libraries the runtime loads on first use (GLib, GStreamer;
  # tools/ci/runtime-dylib-lazy-link.py, TIL-713) must be signed beside it
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
printf 'verify-runtime-bundle-closure: %d objects resolve against %s\n' "${#objects[@]}" "$lib"
