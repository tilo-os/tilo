#!/usr/bin/env bash
# verify-runtime-bundle-closure.sh BUNDLE_DIR [THIN_APP...]
#
# The launcher runs a thin app with LD_LIBRARY_PATH set to the bundle's lib/
# and nothing else, so every shared object the bundle carries, and every thin
# app given, must resolve all of its NEEDED entries from there or from the
# system loader path. build-thin-apps.sh checks the apps against the build's
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
  if ! report=$(env -u LD_PRELOAD LD_LIBRARY_PATH="$lib" ldd "$object" 2>&1); then
    printf '%s\n%s\n' "$object: ldd failed" "$report" >&2
    broken=1
  elif grep -F 'not found' <<<"$report" >&2; then
    printf '%s: unresolved with only %s on LD_LIBRARY_PATH\n' "$object" "$lib" >&2
    broken=1
  fi
done
[ "$broken" -eq 0 ] || fail "the runtime bundle does not satisfy what it or its thin apps link against"
printf 'verify-runtime-bundle-closure: %d objects resolve against %s\n' "${#objects[@]}" "$lib"
