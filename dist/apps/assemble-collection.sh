#!/usr/bin/env bash
# Create one deterministic architecture collection from already-built tapps.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
plan=${TILO_APP_COLLECTION_PLAN:?full collection plan JSON is required}
architecture=${TILO_APP_ARCH:?x86_64 or aarch64 is required}
tapp_dir=${TILO_APP_TAPP_DIR:?directory containing the complete tapp set is required}
runtime_dir=${TILO_APP_RUNTIME_DIR:?matching signed runtime bundle is required}
out=${TILO_APP_COLLECTION_OUT:?collection output directory is required}
source=${GITHUB_SHA:-${TILO_APP_SOURCE:-unknown}}
source_epoch=${SOURCE_DATE_EPOCH:-0}
[[ $source_epoch =~ ^[0-9]+$ ]] || {
    printf 'SOURCE_DATE_EPOCH must be a non-negative integer\n' >&2
    exit 2
}

case "$architecture" in x86_64|aarch64) ;; *) exit 2 ;; esac
plan=$(realpath "$plan")
tapp_dir=$(realpath "$tapp_dir")
runtime_dir=$(realpath "$runtime_dir")
out=$(realpath -m "$out")
case "$out" in "$repo_root"/dist/*) ;; *)
  printf 'TILO_APP_COLLECTION_OUT must be below %s/dist\n' "$repo_root" >&2
  exit 1
esac
[[ ! -L $out ]]
mkdir -p "$out/tapp" "$out/runtime"
find "$out" -mindepth 1 -depth -delete
mkdir -p "$out/tapp" "$out/runtime"
cp "$tapp_dir"/*.tapp "$out/tapp/"
cp -a "$runtime_dir"/. "$out/runtime/"
python3 "$repo_root/tools/apps/assemble_app_collection.py" \
  --plan "$plan" --architecture "$architecture" --tapp-dir "$out/tapp" \
  --runtime-dir "$out/runtime" --source "$source" --output "$out/index.json"
(cd "$out" && find tapp runtime -type f -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS \
  && sha256sum index.json >> SHA256SUMS)
tar --sort=name --mtime="@$source_epoch" --owner=0 --group=0 --numeric-owner \
  -C "$out" -czf "$out/tilo-apps-$architecture.tar.gz" tapp runtime index.json SHA256SUMS
(cd "$out" && sha256sum "tilo-apps-$architecture.tar.gz" \
  > "tilo-apps-$architecture.tar.gz.sha256")
