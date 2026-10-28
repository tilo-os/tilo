#!/usr/bin/env bash
# Promote already-signed CI artifacts to an immutable public GitHub release.
set -euo pipefail

: "${GITHUB_REF_NAME:?missing release tag}"
: "${GITHUB_RELEASE_TOKEN:?missing brokered GitHub App installation token}"
release_repository="${TILO_RELEASE_REPOSITORY:-tilo-os/releases}"
api="${TILO_RELEASE_API_URL:-https://api.github.com}"
uploads="${TILO_RELEASE_UPLOADS_URL:-https://uploads.github.com}"
[[ "$release_repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { printf 'release repository must be OWNER/REPOSITORY\n' >&2; exit 2; }
release_tag="$GITHUB_REF_NAME"
prerelease=false
if [[ ${TILO_CONTINUOUS_PUBLICATION:-0} == 1 ]]; then
  identity="$(python3 "$(dirname "$0")/continuous-release.py" identity "$(dirname "$0")/../Cargo.toml")"
  release_tag="$(printf '%s\n' "$identity" | awk -F= '$1 == "release-tag" { print $2 }')"
  prerelease=true
  python3 "$(dirname "$0")/continuous-release.py" assets "${1:-release-assets}"
else
  [[ "$release_tag" =~ ^imager-v[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || { printf 'release tag must be imager-vMAJOR.MINOR.PATCH\n' >&2; exit 2; }
fi

asset_root="${1:-release-assets}"
if find "$asset_root" -type f -name 'tilo-imager-linux-*' -print -quit | grep -q .; then
  "$(dirname "$0")/verify-linux-release.sh" "$asset_root"
fi

work="$(mktemp -d /var/tmp/tilo-imager-github-release.XXXXXX)"
trap 'find "$work" -depth -delete' EXIT
auth="Authorization: Bearer ${GITHUB_RELEASE_TOKEN}"
accept='Accept: application/vnd.github+json'
version='X-GitHub-Api-Version: 2022-11-28'
release_api="$api/repos/$release_repository/releases"

api_get() {
  curl --fail --silent --show-error --location \
    --retry 5 --retry-all-errors --retry-max-time 120 \
    --connect-timeout 20 --max-time 120 \
    -H "$auth" -H "$accept" -H "$version" "$@"
}

GITHUB_INSTALLATION_TOKEN="$GITHUB_RELEASE_TOKEN" \
  "$(dirname "$0")/assert-github-installation-token.sh" "$release_repository"
api_get "$api/repos/$release_repository" >"$work/repository.json"
python3 - "$work/repository.json" "$release_repository" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    repository = json.load(source)
if repository.get("full_name") != sys.argv[2]:
    raise SystemExit("GitHub returned a different release repository")
if repository.get("private") is not False or repository.get("visibility") != "public":
    raise SystemExit("release repository must be public")
if repository.get("archived") is True:
    raise SystemExit("release repository must not be archived")
PY

declare -A local_names=()
while IFS= read -r -d '' artifact; do
  name="$(basename "$artifact")"
  # Catalog inputs, not downloads; internal bundles and target policies are Forgejo packages.
  case "$name" in catalog-x86_64.json|catalog-aarch64.json|internal-catalog-*.json|tilo-internal-installer-*.json|target-policy-catalog-*.json|tilo-target-policy-*.tar) continue ;; esac
  [[ ! -v "local_names[$name]" ]] \
    || { printf 'duplicate local release asset name %s\n' "$name" >&2; exit 1; }
  size="$(wc -c <"$artifact" | tr -d '[:space:]')"
  [ "$size" -le 2147483648 ] \
    || { printf 'GitHub release asset exceeds the 2 GiB limit: %s\n' "$name" >&2; exit 1; }
  local_names["$name"]="$artifact"
done < <(find "$asset_root" -type f -print0 | sort -z)
[ "${#local_names[@]}" -gt 0 ] || { printf 'no release assets found under %s\n' "$asset_root" >&2; exit 1; }

release_json="$work/release.json"
status="$(curl --silent --show-error \
  --retry 5 --retry-max-time 120 --connect-timeout 20 --max-time 120 \
  -H "$auth" -H "$accept" -H "$version" \
  --output "$release_json" --write-out '%{http_code}' \
  "$release_api/tags/$release_tag")"
if [ "$status" = 404 ]; then
  payload="$(python3 - "$release_tag" "$prerelease" "${GITHUB_SHA:-}" <<'PY'
import json, sys
tag, channel, source = sys.argv[1:]
continuous = channel == "true"
print(json.dumps({
    "tag_name": tag,
    "target_commitish": "main",
    "name": f"Tilo Installer {tag}",
    "body": "Signed Tilo Installer downloads. Verify each adjacent SHA-256 file before opening an artifact."
            + (f"\nContinuous build from source revision {source}; not a stable release." if continuous else ""),
    "draft": True,
    "prerelease": continuous,
    "make_latest": "false" if continuous else "true",
}))
PY
)"
  curl --fail --silent --show-error \
    -H "$auth" -H "$accept" -H "$version" -H 'Content-Type: application/json' \
    --data "$payload" "$release_api" >"$release_json"
elif [ "$status" != 200 ]; then
  printf 'could not read GitHub release %s (HTTP %s)\n' "$release_tag" "$status" >&2
  exit 1
fi

read -r release_id release_state < <(python3 - "$release_json" "$release_tag" "$prerelease" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    release = json.load(source)
if release.get("tag_name") != sys.argv[2]:
    raise SystemExit("GitHub returned a different release identity")
if sys.argv[3] == "true" and release.get("prerelease") is not True:
    raise SystemExit("continuous release must remain a prerelease")
draft = release.get("draft")
if not isinstance(draft, bool):
    raise SystemExit("GitHub release response has no boolean draft state")
print(release["id"], "draft" if draft else "published")
PY
)
assets_json="$work/assets.json"
api_get "$release_api/$release_id/assets?per_page=100" >"$assets_json"
python3 - "$assets_json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    assets = json.load(source)
if not isinstance(assets, list) or len(assets) >= 100:
    raise SystemExit("GitHub release asset list is invalid or requires pagination")
PY

# Validate outside process substitution: Bash does not propagate a producer's
# failure to the consuming loop. A malformed inventory must stop promotion.
python3 - "$assets_json" > "$work/remote-names" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    assets = json.load(source)
for asset in assets:
    name = asset.get("name")
    if not isinstance(name, str) or not name or "\0" in name:
        raise SystemExit("release contains an invalid asset name")
    sys.stdout.buffer.write(name.encode() + b"\0")
PY
while IFS= read -r -d '' remote_name; do
  [[ -v "local_names[$remote_name]" ]] \
    || { printf 'release draft contains unexpected asset %s; refusing publication\n' "$remote_name" >&2; exit 1; }
done < "$work/remote-names"

processed=0
index=0
while IFS= read -r -d '' artifact; do
  name="$(basename "$artifact")"
  # Catalog inputs, not downloads; internal bundles and target policies are Forgejo packages.
  case "$name" in catalog-x86_64.json|catalog-aarch64.json|internal-catalog-*.json|tilo-internal-installer-*.json|target-policy-catalog-*.json|tilo-target-policy-*.tar) continue ;; esac
  index=$((index + 1))
  metadata="$(python3 - "$assets_json" "$name" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    assets = json.load(source)
matches = [asset for asset in assets if asset.get("name") == sys.argv[2]]
if len(matches) > 1:
    raise SystemExit(f"release contains duplicate assets named {sys.argv[2]}")
if matches:
    asset = matches[0]
    if not isinstance(asset.get("id"), int) or not isinstance(asset.get("size"), int):
        raise SystemExit(f"release asset {sys.argv[2]} has invalid metadata")
    print(f"{asset['id']}\t{asset['size']}")
PY
)"
  local_size="$(wc -c <"$artifact" | tr -d '[:space:]')"
  if [ -n "$metadata" ]; then
    IFS=$'\t' read -r asset_id remote_size <<<"$metadata"
    [ "$remote_size" = "$local_size" ] \
      || { printf 'release asset mismatch for %s\n' "$name" >&2; exit 1; }
    api_get -H 'Accept: application/octet-stream' \
      "$api/repos/$release_repository/releases/assets/$asset_id" >"$work/existing-$index"
    cmp --silent "$artifact" "$work/existing-$index" \
      || { printf 'release asset mismatch for %s\n' "$name" >&2; exit 1; }
    printf 'verified existing %s\n' "$name"
  else
    [ "$release_state" = draft ] \
      || { printf 'published release is missing immutable asset %s\n' "$name" >&2; exit 1; }
    encoded_name="$(python3 - "$name" <<'PY'
import sys
from urllib.parse import quote
print(quote(sys.argv[1], safe=""))
PY
)"
    curl --fail --silent --show-error \
      -H "$auth" -H "$accept" -H "$version" -H 'Content-Type: application/octet-stream' \
      --data-binary "@$artifact" \
      "$uploads/repos/$release_repository/releases/$release_id/assets?name=$encoded_name" \
      >"$work/uploaded-$index.json"
    read -r asset_id uploaded_size < <(python3 - "$work/uploaded-$index.json" "$name" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    asset = json.load(source)
if asset.get("name") != sys.argv[2] or not isinstance(asset.get("id"), int) or not isinstance(asset.get("size"), int):
    raise SystemExit("GitHub returned invalid uploaded asset metadata")
print(asset["id"], asset["size"])
PY
)
    [ "$uploaded_size" = "$local_size" ] \
      || { printf 'uploaded release asset size mismatch for %s\n' "$name" >&2; exit 1; }
    api_get -H 'Accept: application/octet-stream' \
      "$api/repos/$release_repository/releases/assets/$asset_id" >"$work/uploaded-bytes-$index"
    cmp --silent "$artifact" "$work/uploaded-bytes-$index" \
      || { printf 'uploaded release asset bytes differ from local artifact: %s\n' "$name" >&2; exit 1; }
    printf 'published and verified %s\n' "$name"
  fi
  processed=$((processed + 1))
done < <(find "$asset_root" -type f -print0 | sort -z)

[ "$processed" -gt 0 ]
if [ "$release_state" = draft ]; then
  payload="$(python3 - "$release_tag" "$prerelease" <<'PY'
import json, sys
tag, channel = sys.argv[1:]
continuous = channel == "true"
print(json.dumps({"tag_name": tag, "name": f"Tilo Installer {tag}", "draft": False,
                  "prerelease": continuous, "make_latest": "false" if continuous else "true"}))
PY
)"
  curl --fail --silent --show-error -X PATCH \
    -H "$auth" -H "$accept" -H "$version" -H 'Content-Type: application/json' \
    --data "$payload" "$release_api/$release_id" >"$release_json"
  python3 - "$release_json" "$release_tag" "$prerelease" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    release = json.load(source)
if release.get("tag_name") != sys.argv[2] or (sys.argv[3] == "true" and release.get("prerelease") is not True):
    raise SystemExit("GitHub changed the release identity or continuous channel")
if release.get("draft") is not False:
    raise SystemExit("GitHub did not publish the fully verified release")
PY
  printf 'published complete release %s\n' "$release_tag"
fi
