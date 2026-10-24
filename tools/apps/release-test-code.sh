#!/usr/bin/env bash
# release-test-code.sh DIR: fail when a binary app-binaries left in DIR (the default collection's
# apps, the shared runtime and its engines, with their symbol tables) carries the app SDK's test
# helpers. `tilo_app::framework::testing` is for tests; what an app's own modes run (`--surface`,
# `--bench`, `--shots`, `--data-dir`) lives in `tilo_app::framework::headless`, so a release app
# that names `framework::testing` has test code compiled into it.
set -euo pipefail
dir=${1:?usage: release-test-code.sh DIR}
nm=$(command -v nm || command -v llvm-nm) || { echo "no nm to read symbol tables with" >&2; exit 1; }
found=0 read=0
for file in "$dir"/*; do
  [[ -f $file ]] && head -c 4 "$file" | grep -q $'\x7fELF' || continue
  read=$((read + 1))
  table=$("$nm" --demangle --defined-only "$file" 2>/dev/null || true)
  [[ -n $table ]] || { printf '%s has no symbol table to read\n' "$(basename "$file")" >&2; exit 1; }
  symbols=$(printf '%s\n' "$table" | grep -F 'framework::testing::' || true)
  if [[ -n $symbols ]]; then
    found=1
    printf '%s carries test helpers:\n' "$(basename "$file")" >&2
    printf '%s\n' "$symbols" | cut -c1-200 | head -20 | sed 's/^/  /' >&2
  fi
done
(( read > 0 )) || { echo "no binaries in $dir" >&2; exit 1; }
(( found == 0 )) || exit 1
echo "$read binaries, none carries framework::testing"
