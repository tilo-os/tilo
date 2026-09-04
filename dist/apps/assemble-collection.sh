#!/usr/bin/env bash
# Create one deterministic architecture collection from already-built qpkgs.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
plan=${QOS_APP_COLLECTION_PLAN:?full collection plan JSON is required}
architecture=${QOS_APP_ARCH:?x86_64 or aarch64 is required}
qpkg_dir=${QOS_APP_QPKG_DIR:?directory containing the complete qpkg set is required}
out=${QOS_APP_COLLECTION_OUT:?collection output directory is required}
source=${GITHUB_SHA:-${QOS_APP_SOURCE:-unknown}}
source_epoch=${SOURCE_DATE_EPOCH:-$(git -C "$repo_root" log -1 --format=%ct)}

case "$architecture" in x86_64|aarch64) ;; *) exit 2 ;; esac
plan=$(realpath "$plan")
qpkg_dir=$(realpath "$qpkg_dir")
out=$(realpath -m "$out")
case "$out" in "$repo_root"/dist/*) ;; *)
  printf 'QOS_APP_COLLECTION_OUT must be below %s/dist\n' "$repo_root" >&2
  exit 1
esac
[[ ! -L $out ]]
mkdir -p "$out/qpkg"
find "$out" -mindepth 1 -depth -delete
mkdir -p "$out/qpkg"
cp "$qpkg_dir"/*.qpkg "$out/qpkg/"
python3 "$repo_root/tools/assemble_app_collection.py" \
  --plan "$plan" --architecture "$architecture" --qpkg-dir "$out/qpkg" \
  --source "$source" --output "$out/index.json"
(cd "$out" && sha256sum qpkg/*.qpkg index.json > SHA256SUMS)
tar --sort=name --mtime="@$source_epoch" --owner=0 --group=0 --numeric-owner \
  -C "$out" -czf "$out/qos-apps-$architecture.tar.gz" qpkg index.json SHA256SUMS
(cd "$out" && sha256sum "qos-apps-$architecture.tar.gz" \
  > "qos-apps-$architecture.tar.gz.sha256")
