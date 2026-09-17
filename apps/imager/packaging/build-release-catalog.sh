#!/usr/bin/env bash
# Merge architecture-specific, digest-bound installer metadata into the exact
# catalog embedded by all native Imager builds.
set -euo pipefail
: "${GITHUB_REF_NAME:?missing release tag}"
release_repository="${TILO_RELEASE_REPOSITORY:-tilo/releases}"
release_server="${GITHUB_SERVER_URL:-https://code.podesta.ai}"
[[ "$release_repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { printf 'release repository must be OWNER/REPOSITORY\n' >&2; exit 2; }
[[ "$release_server" =~ ^https://[^/]+$ ]] \
  || { printf 'release server must be an HTTPS origin\n' >&2; exit 2; }
release_prefix="${release_server}/${release_repository}/releases/download/${GITHUB_REF_NAME}/"
root="${1:-release-assets}"
out="${2:-dist/tilo-installer-catalog}"
mkdir -p "$out"
mapfile -t fragments < <(find "$root" -type f -name 'catalog-*.json' | sort)
[ "${#fragments[@]}" -eq 2 ] || { printf 'expected two installer catalog fragments\n' >&2; exit 1; }
jq -s --arg tag "$GITHUB_REF_NAME" \
  '{schema:1,tag:$tag,releases:(sort_by(.arch))}' "${fragments[@]}" \
  >"$out/tilo-installer-catalog-v1.json"
(cd "$out" && sha256sum tilo-installer-catalog-v1.json) \
  >"$out/tilo-installer-catalog-v1.json.sha256"

jq -e --arg prefix "$release_prefix" '
  .schema == 1 and
  (.tag | test("^imager-v[0-9]+\\.[0-9]+\\.[0-9]+$")) and
  ([.releases[].arch] | sort == ["aarch64", "x86_64"]) and
  all(.releases[];
    (.url | startswith($prefix)) and
    (.archive_sha256 | test("^[0-9a-f]{64}$")) and
    .archive_size > 0 and
    (.image_sha256 | test("^[0-9a-f]{64}$")) and
    .image_size > 0)
' "$out/tilo-installer-catalog-v1.json" >/dev/null
