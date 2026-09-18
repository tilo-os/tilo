#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
tool=$repo_root/tools/ci/receipt.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git init --bare -q "$work/origin.git"
git init -q -b main "$work/source"
git -C "$work/source" config user.name test
git -C "$work/source" config user.email test@example.invalid
printf 'checked\n' > "$work/source/value"
git -C "$work/source" add value
git -C "$work/source" commit -qm checked
git -C "$work/source" remote add origin "$work/origin.git"
git -C "$work/source" push -q -u origin main
sha=$(git -C "$work/source" rev-parse HEAD)

(cd "$work/source" && "$tool" record "$sha" 42 success success success origin >/dev/null)
(cd "$work/source" && "$tool" verify "$sha" origin)

receipt=$(git -C "$work/source" notes --ref=tilo-check-ci show "$sha")
[[ $receipt == "select=success security=success affected=success run=42 sha=$sha" ]]

if (cd "$work/source" && "$tool" record "$sha" nope success success success origin 2>/dev/null); then
  echo 'receipt tool accepted a non-numeric workflow run id' >&2
  exit 1
fi
if (cd "$work/source" && "$tool" record "$sha" 43 unknown success success origin 2>/dev/null); then
  echo 'receipt tool accepted an unknown job result' >&2
  exit 1
fi

(cd "$work/source" && "$tool" record "$sha" 44 success failure success origin >/dev/null)
if (cd "$work/source" && "$tool" verify "$sha" origin 2>/dev/null); then
  echo 'receipt tool accepted a failed check result' >&2
  exit 1
fi

echo 'CI receipt record/verify contract OK'
