#!/usr/bin/env bash
# Fail a release build whose binaries have grown past their budget.
#
#   tools/check-binary-size.sh [target-dir]      # default target/release
#
# Budgets live in tools/binary-budget.toml, one per binary in MiB. A binary
# with no entry is reported but does not fail, so adding one is not blocked on
# picking a number; add it to the file when you know what it should be.

set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
TARGET="${1:-$ROOT/target/release}"
BUDGETS="$ROOT/tools/binary-budget.toml"

[ -d "$TARGET" ] || { echo "no such directory: $TARGET" >&2; exit 2; }
[ -f "$BUDGETS" ] || { echo "no budget file: $BUDGETS" >&2; exit 2; }

over=0
unbudgeted=0
while IFS= read -r bin; do
  name=$(basename "$bin")
  size=$(stat -c%s "$bin")
  mib=$(( (size + 1048575) / 1048576 ))
  budget=$(sed -n "s/^\"${name}\" = \([0-9]*\).*/\1/p" "$BUDGETS" | head -1)
  if [ -z "$budget" ]; then
    printf '  ?     %4d MiB  %s (no budget)\n' "$mib" "$name"
    unbudgeted=$((unbudgeted + 1))
  elif [ "$mib" -gt "$budget" ]; then
    printf '  OVER  %4d MiB  %s (budget %d MiB)\n' "$mib" "$name" "$budget"
    over=$((over + 1))
  else
    printf '  ok    %4d MiB  %s (budget %d MiB)\n' "$mib" "$name" "$budget"
  fi
done < <(find "$TARGET" -maxdepth 1 -type f -executable ! -name '*.so' ! -name '*.d' | sort)

echo
if [ "$over" -gt 0 ]; then
  echo "$over binaries over budget. Either the growth is wrong, or the budget in"
  echo "tools/binary-budget.toml should move — deliberately, in a diff."
  exit 1
fi
echo "all binaries within budget${unbudgeted:+ ($unbudgeted without one)}"
