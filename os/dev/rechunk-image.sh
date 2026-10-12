#!/usr/bin/env bash
# Re-lay a built OS image out by content (TIL-747), prove it is the same tree,
# and publish it in place of the raw image.
#
#   rechunk-image.sh --builder NAME --source SOURCE --name IMAGE:TAG --epoch SECONDS
#                    (--push | --archive PATH) [--report FILE] [--max-layers N]
#
#   SOURCE  docker://HOST/REPO@sha256:...  an image the dev lane pushed
#           oci-archive:PATH               buildx's OCI archive of one image
#
# os/Containerfile.rechunk runs chunkah over the raw image's root (BuildKit
# `--build-context rootfs=...`); os/scripts/oci-rechunk.py adds the directory
# layer, gives the result the raw image's config and refuses it unless every
# path matches the raw image. --push copies it to IMAGE:TAG (blobs the
# registry has are not sent again); --archive writes it as an OCI archive with
# buildx's names, replacing PATH when PATH is the source. The finished
# manifest digest is the last line of standard output.
#
# The epoch caps the time of files no package owns. Two builds share a layer
# only if they pass the same epoch, so callers pass one that moves with the
# base image and the RPM snapshot, not with the commit.
#
# CI-only: it needs a buildx builder. A local image keeps its Containerfile
# layers, which podman's layer cache is what makes cheap to rebuild.
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

builder=""; source_ref=""; name=""; epoch=""; push=0; archive=""; report=""; max_layers=96
while [ "$#" -gt 0 ]; do
  case "$1" in
    --push) push=1; shift; continue ;;
  esac
  [ "$#" -ge 2 ] && [ -n "$2" ] || die "missing value for $1"
  case "$1" in
    --builder) builder="$2" ;;
    --source) source_ref="$2" ;;
    --name) name="$2" ;;
    --epoch) epoch="$2" ;;
    --archive) archive="$2" ;;
    --report) report="$2" ;;
    --max-layers) max_layers="$2" ;;
    *) die "unknown option: $1" ;;
  esac
  shift 2
done
[ -n "$builder" ] && [ -n "$source_ref" ] && [ -n "$name" ] && [ -n "$epoch" ] \
  || die "usage: $0 --builder NAME --source SOURCE --name IMAGE:TAG --epoch SECONDS (--push | --archive PATH)"
[ "$push" = 1 ] || [ -n "$archive" ] || die "choose --push or --archive"
[[ $epoch =~ ^[1-9][0-9]{0,11}$ ]] || die "--epoch must be a positive Unix time"
[[ $max_layers =~ ^[1-9][0-9]{0,2}$ ]] || die "--max-layers must be a small positive number"
[[ ${name##*/} == *:* ]] || die "--name must carry a tag: $name"
python3 -c 'import compression.zstd' 2>/dev/null || command -v zstd >/dev/null \
  || die "reading zstd layers needs Python 3.14 or the zstd command"
command -v skopeo >/dev/null || die "skopeo is required"

work="$(mktemp -d "${TMPDIR:-/var/tmp}/tilo-rechunk.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT
mkdir "$work/context"

case "$source_ref" in
  docker://*@sha256:*)
    ref="${source_ref#docker://}"
    context="docker-image://$ref"
    raw="$ref"
    ;;
  oci-archive:*)
    path="${source_ref#oci-archive:}"
    [ -f "$path" ] || die "no OCI archive at $path"
    mkdir "$work/raw"
    tar -xf "$path" -C "$work/raw"
    digest="$(python3 - "$work/raw/index.json" <<'PY'
import json, re, sys
digests = {item.get("digest") for item in json.load(open(sys.argv[1])).get("manifests", [])}
if len(digests) != 1 or not re.fullmatch(r"sha256:[0-9a-f]{64}", next(iter(digests))):
    sys.exit(f"the archive must hold exactly one image, not {sorted(map(str, digests))}")
print(next(iter(digests)))
PY
)"
    # Written back over itself: drop the raw copy now, the layout holds it.
    if [ -n "$archive" ] && [ "$(realpath -m -- "$archive")" = "$(realpath -- "$path")" ]; then
      rm -f -- "$path"
    fi
    context="oci-layout://$work/raw@$digest"
    raw="oci:$work/raw"
    ;;
  *) die "--source must be docker://REF@sha256:... or oci-archive:PATH" ;;
esac

"$TILO_OS_DIR/dev/cache-github-assets.sh" chunkah-0.7.1.tar.gz chunkah-0.7.1-vendor.tar.gz >&2

started=$(date +%s)
docker buildx build --builder "$builder" --progress=plain \
  --file "$TILO_OS_DIR/Containerfile.rechunk" --target rechunked \
  --build-context "rootfs=$context" \
  --build-context "github-assets=$TILO_OS_DIR/external-assets/cache" \
  --build-context "tilo-scripts=$TILO_OS_DIR/scripts" \
  --build-arg "SOURCE_DATE_EPOCH=$epoch" \
  --build-arg "TILO_RECHUNK_MAX_LAYERS=$max_layers" \
  --output "type=local,dest=$work/out" \
  "$work/context" >&2
msg "chunkah wrote $(find "$work/out" -path '*/blobs/sha256/*' -type f | wc -l) blobs in $(( $(date +%s) - started ))s"

finish=(--raw "$raw" --chunked "$work/out/image" --name "$name")
if [ -n "$report" ]; then
  cp -- "$work/out/components.json" "${report%.json}-components.json"
fi
# Trees with layers of their own (os/scripts/rechunk-chunkah.py).
for tree in "$work"/out/trees/*/; do
  [ -d "$tree/image" ] || continue
  finish+=(--extra "${tree%/}/image")
  [ -z "$report" ] || cp -- "$tree/components.json" "${report%.json}-$(basename "$tree")-components.json"
done
[ -z "$report" ] || finish+=(--report "$report")
[ -z "$archive" ] || finish+=(--archive "$archive")
digest="$(python3 "$TILO_OS_DIR/scripts/oci-rechunk.py" finish "${finish[@]}" | tail -n1)"
[[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "oci-rechunk.py returned no digest"
msg "rechunked and verified in $(( $(date +%s) - started ))s: $digest"

if [ "$push" = 1 ]; then
  skopeo copy --retry-times 3 --preserve-digests --digestfile "$work/pushed" \
    "oci:$work/out/image:${name##*:}" "docker://$name" >&2
  pushed="$(tr -d '[:space:]' < "$work/pushed")"
  [ "$pushed" = "$digest" ] || die "pushed $pushed, expected $digest"
  msg "pushed $name ($digest)"
fi
printf '%s\n' "$digest"
