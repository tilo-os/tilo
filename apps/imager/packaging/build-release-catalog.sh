#!/usr/bin/env bash
# Merge architecture-specific, digest-bound installer metadata into the exact
# catalog embedded by all native Imager builds.
set -euo pipefail
: "${GITHUB_REF_NAME:?missing release tag}"
root="${1:-release-assets}"
out="${2:-dist/qos-installer-catalog}"
mkdir -p "$out"
mapfile -t fragments < <(find "$root" -type f -name 'catalog-*.json' | sort)
[ "${#fragments[@]}" -eq 2 ] || { printf 'expected two installer catalog fragments\n' >&2; exit 1; }
jq -s --arg tag "$GITHUB_REF_NAME" \
  '{schema:1,tag:$tag,releases:(sort_by(.arch))}' "${fragments[@]}" \
  >"$out/qos-installer-catalog-v1.json"
(cd "$out" && sha256sum qos-installer-catalog-v1.json) \
  >"$out/qos-installer-catalog-v1.json.sha256"

jq -e '
  .schema == 1 and
  (.tag | test("^imager-v[0-9]+\\.[0-9]+\\.[0-9]+$")) and
  ([.releases[].arch] | sort == ["aarch64", "x86_64"]) and
  all(.releases[];
    (.url | startswith("https://code.sandbox.example/qos/qos/releases/download/" + $tag + "/")) and
    (.archive_sha256 | test("^[0-9a-f]{64}$")) and
    .archive_size > 0 and
    (.image_sha256 | test("^[0-9a-f]{64}$")) and
    .image_size > 0)
' --arg tag "$GITHUB_REF_NAME" "$out/qos-installer-catalog-v1.json" >/dev/null
