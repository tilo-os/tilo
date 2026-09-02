#!/usr/bin/env bash
# Promote already-signed CI artifacts to the immutable Forgejo tag release.
set -euo pipefail

: "${GITHUB_SERVER_URL:?missing Forgejo server URL}"
: "${GITHUB_REPOSITORY:?missing owner/repository}"
: "${GITHUB_REF_NAME:?missing release tag}"
: "${FORGEJO_TOKEN:?missing release token}"

asset_root="${1:-release-assets}"
api="${GITHUB_SERVER_URL}/api/v1/repos/${GITHUB_REPOSITORY}"
auth="Authorization: token ${FORGEJO_TOKEN}"
release_json="$(mktemp /var/tmp/qos-imager-release.XXXXXX)"
trap 'find "$release_json" -maxdepth 0 -delete' EXIT

if ! curl --fail --silent --show-error -H "$auth" \
  "$api/releases/tags/$GITHUB_REF_NAME" >"$release_json"; then
  payload="$(python3 - "$GITHUB_REF_NAME" <<'PY'
import json, sys
tag = sys.argv[1]
print(json.dumps({
    "tag_name": tag,
    "name": f"qOS Imager {tag}",
    "body": "Signed qOS Imager downloads. Verify the adjacent SHA-256 file before opening an artifact.",
    "draft": False,
    "prerelease": False,
}))
PY
)"
  curl --fail --silent --show-error -H "$auth" -H 'Content-Type: application/json' \
    --data "$payload" "$api/releases" >"$release_json"
fi

release_id="$(python3 - "$release_json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    print(json.load(source)["id"])
PY
)"

uploaded=0
while IFS= read -r -d '' artifact; do
  name="$(basename "$artifact")"
  case "$name" in catalog-x86_64.json|catalog-aarch64.json) continue ;; esac
  curl --fail --silent --show-error -H "$auth" \
    -F "attachment=@$artifact" \
    "$api/releases/$release_id/assets?name=$name" >/dev/null
  printf 'published %s\n' "$name"
  uploaded=$((uploaded + 1))
done < <(find "$asset_root" -type f -print0 | sort -z)

[ "$uploaded" -gt 0 ] || {
  printf 'no release assets found under %s\n' "$asset_root" >&2
  exit 1
}
