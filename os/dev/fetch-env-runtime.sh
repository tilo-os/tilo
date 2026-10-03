#!/usr/bin/env bash
# Fetch the published env-runtime environment image for this checkout into
# DEST: the sealed tapps, their ids and SHA256SUMS, and publisher.hex — the
# /env-runtime directory of registry.tilo.so/tilo/env-runtime, resolved and
# Cosign-verified by os/dev/resolve-env-runtime.sh. A package build hands
# DEST to dist/rpm/stage-app-store.sh as TILO_ENV_RUNTIME_DIR.
#
#   fetch-env-runtime.sh DEST
#
# Exit 0: DEST written. Exit 1: no runtime is published for this checkout's
# os/Containerfile.env-runtime, nothing written. Anything else: an error.
# Needs: cosign, skopeo, tar.
set -euo pipefail

die() { printf 'fetch-env-runtime: %s\n' "$*" >&2; exit 2; }

[ "$#" -eq 1 ] || die "usage: $0 DEST"
dest=$1
[ ! -e "$dest" ] || die "$dest already exists"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

status=0
ref="$("$repo/os/dev/resolve-env-runtime.sh")" || status=$?
if [ "$status" -ne 0 ]; then
  exists=0
  "$repo/os/dev/resolve-env-runtime.sh" --exists || exists=$?
  [ "$exists" -eq 1 ] && exit 1
  die "the env-runtime could not be resolved and verified"
fi

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
skopeo copy --quiet "docker://$ref" "dir:$work/image"
mkdir "$work/root"
# The image is FROM scratch with one COPY; unpack its layers in order.
python3 - "$work/image/manifest.json" > "$work/layers" <<'PY'
import json, re, sys
for layer in json.load(open(sys.argv[1]))["layers"]:
    digest = layer["digest"]
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise SystemExit(f"unexpected layer digest {digest!r}")
    print(digest.removeprefix("sha256:"))
PY
while read -r layer; do
  tar -xf "$work/image/$layer" -C "$work/root" --no-same-owner
done < "$work/layers"
[ -f "$work/root/env-runtime/SHA256SUMS" ] || die "$ref has no /env-runtime/SHA256SUMS"
(cd "$work/root/env-runtime" && sha256sum --check --strict --quiet SHA256SUMS) \
  || die "$ref does not match its own SHA256SUMS"
mkdir -p "$(dirname "$dest")"
cp -a "$work/root/env-runtime" "$dest"
printf 'fetch-env-runtime: %s -> %s\n' "$ref" "$dest" >&2
