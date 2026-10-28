#!/usr/bin/env bash
# Promote already-signed CI artifacts to the immutable Forgejo tag release.
set -euo pipefail

: "${GITHUB_SERVER_URL:?missing Forgejo server URL}"
: "${GITHUB_REF_NAME:?missing release tag}"
: "${FORGEJO_TOKEN:?missing release token}"

release_repository="${TILO_RELEASE_REPOSITORY:-tilo/releases}"
[[ "$release_repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { printf 'release repository must be OWNER/REPOSITORY\n' >&2; exit 2; }
[ "${GITHUB_REPOSITORY:-}" != "$release_repository" ] \
  || { printf 'release repository must be separate from the source repository\n' >&2; exit 2; }
[[ "$GITHUB_REF_NAME" =~ ^imager-v[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { printf 'release tag must be imager-vMAJOR.MINOR.PATCH\n' >&2; exit 2; }

asset_root="${1:-release-assets}"
if find "$asset_root" -type f -name 'tilo-imager-linux-*' -print -quit \
   | grep -q .; then
  "$(dirname "$0")/verify-linux-release.sh" "$asset_root"
fi
api="${GITHUB_SERVER_URL%/}/api/v1/repos/${release_repository}"
auth="Authorization: token ${FORGEJO_TOKEN}"
work="$(mktemp -d /var/tmp/tilo-imager-release.XXXXXX)"
trap 'find "$work" -depth -delete' EXIT
release_json="$work/release.json"
assets_json="$work/assets.json"
repository_json="$work/repository.json"

api_get() {
  curl --fail --silent --show-error \
    --retry 5 --retry-all-errors --retry-max-time 120 \
    --connect-timeout 20 --max-time 120 \
    -H "$auth" "$@"
}

# A typo or an over-broad source-repository token must never silently publish
# downloads back into the private source repository. The dedicated target is
# pre-created and public; the OIDC broker grants this job write access to that
# repository only.
api_get "${GITHUB_SERVER_URL%/}/api/v1/repos/${release_repository}" >"$repository_json"
python3 - "$repository_json" "$release_repository" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    repository = json.load(source)
if repository.get("full_name") != sys.argv[2]:
    raise SystemExit("Forgejo returned a different release repository")
if repository.get("private") is not False or repository.get("internal") is not False:
    raise SystemExit("release repository must be public")
if repository.get("archived") is True:
    raise SystemExit("release repository must not be archived")
PY

# Forgejo may serve private attachments itself or redirect public downloads to
# object storage. Credentials are sent only to the configured Forgejo origin.
download_asset() {
  local url="$1" output="$2" access
  access="$(python3 - "$GITHUB_SERVER_URL" "$url" <<'PY'
import sys
from urllib.parse import urlsplit

server = urlsplit(sys.argv[1])
asset = urlsplit(sys.argv[2])
if not asset.scheme or not asset.netloc:
    raise SystemExit("release attachment has a non-absolute download URL")
if (asset.scheme, asset.netloc) == (server.scheme, server.netloc):
    print("authenticated")
elif asset.scheme == "https":
    print("public")
else:
    raise SystemExit("cross-origin release attachment download is not HTTPS")
PY
)"
  if [ "$access" = authenticated ]; then
    api_get --output "$output" "$url"
  else
    curl --fail --silent --show-error \
      --retry 5 --retry-all-errors --retry-max-time 120 \
      --connect-timeout 20 --max-time 120 \
      --output "$output" "$url"
  fi
}

release_status="$(curl --silent --show-error \
  --retry 5 --retry-max-time 120 \
  --connect-timeout 20 --max-time 120 \
  -H "$auth" --output "$release_json" --write-out '%{http_code}' \
  "$api/releases/tags/$GITHUB_REF_NAME")"
if [ "$release_status" = 404 ]; then
  payload="$(python3 - "$GITHUB_REF_NAME" <<'PY'
import json, sys
tag = sys.argv[1]
print(json.dumps({
    "tag_name": tag,
    "target_commitish": "main",
    "name": f"Tilo Imager {tag}",
    "body": "Signed Tilo Imager downloads. Verify each adjacent SHA-256 file before opening an artifact. Linux users can authenticate detached signatures with tilo-release-key.asc after comparing tilo-release-key.fingerprint with the fingerprint published through an independent Tilo channel.",
    "draft": True,
    "prerelease": False,
}))
PY
)"
  curl --fail --silent --show-error -H "$auth" -H 'Content-Type: application/json' \
    --data "$payload" "$api/releases" >"$release_json"
elif [ "$release_status" != 200 ]; then
  printf 'could not read Forgejo release %s (HTTP %s)\n' \
    "$GITHUB_REF_NAME" "$release_status" >&2
  exit 1
fi

read -r release_id release_state < <(python3 - "$release_json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    release = json.load(source)
draft = release.get("draft")
if not isinstance(draft, bool):
    raise SystemExit("Forgejo release response has no boolean draft state")
print(release["id"], "draft" if draft else "published")
PY
)
api_get "$api/releases/$release_id/assets" >"$assets_json"

# Different artifact jobs may use nested directories. Refuse ambiguous local
# basenames before publishing anything because Forgejo release names are flat.
declare -A local_names=()
while IFS= read -r -d '' artifact; do
  name="$(basename "$artifact")"
  # Catalog inputs, not downloads; internal bundles and target policies are Forgejo packages.
  case "$name" in catalog-x86_64.json|catalog-aarch64.json|internal-catalog-*.json|tilo-internal-installer-*.json|target-policy-catalog-*.json|tilo-target-policy-*.tar) continue ;; esac
  if [[ -v "local_names[$name]" ]]; then
    printf 'duplicate local release asset name %s: %s and %s\n' \
      "$name" "${local_names[$name]}" "$artifact" >&2
    exit 1
  fi
  local_names["$name"]="$artifact"
done < <(find "$asset_root" -type f -print0 | sort -z)

# A resumed draft may contain uploads from an earlier failed attempt. Publish
# only when its remote name set is a subset of this run's exact local set;
# otherwise an obsolete or injected attachment could become public unnoticed.
while IFS= read -r -d '' remote_name; do
  if [[ ! -v "local_names[$remote_name]" ]]; then
    printf 'release draft contains unexpected asset %s; refusing publication\n' \
      "$remote_name" >&2
    exit 1
  fi
done < <(python3 - "$assets_json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    assets = json.load(source)
for asset in assets:
    name = asset.get("name")
    if not isinstance(name, str) or not name or "\0" in name:
        raise SystemExit("release contains an invalid asset name")
    sys.stdout.buffer.write(name.encode() + b"\0")
PY
)

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
    url = asset.get("browser_download_url")
    size = asset.get("size")
    if not isinstance(url, str) or not url:
        raise SystemExit(f"release asset {sys.argv[2]} has no download URL")
    if not isinstance(size, int) or size < 0:
        raise SystemExit(f"release asset {sys.argv[2]} has an invalid size")
    print(f"{size}\t{url}")
PY
)"

  if [ -n "$metadata" ]; then
    IFS=$'\t' read -r remote_size remote_url <<<"$metadata"
    local_size="$(wc -c <"$artifact" | tr -d '[:space:]')"
    if [ "$remote_size" != "$local_size" ]; then
      printf 'release asset mismatch for %s: existing size %s, local size %s\n' \
        "$name" "$remote_size" "$local_size" >&2
      exit 1
    fi
    existing="$work/existing-$index"
    download_asset "$remote_url" "$existing"
    if ! cmp --silent "$artifact" "$existing"; then
      printf 'release asset mismatch for %s: existing bytes differ from local artifact\n' \
        "$name" >&2
      exit 1
    fi
    printf 'verified existing %s\n' "$name"
  else
    if [ "$release_state" != draft ]; then
      printf 'published release is missing immutable asset %s; refusing to modify it\n' \
        "$name" >&2
      exit 1
    fi
    uploaded_json="$work/uploaded-$index.json"
    encoded_name="$(python3 - "$name" <<'PY'
import sys
from urllib.parse import quote
print(quote(sys.argv[1], safe=""))
PY
)"
    curl --fail --silent --show-error -H "$auth" \
      -F "attachment=@$artifact" \
      "$api/releases/$release_id/assets?name=$encoded_name" >"$uploaded_json"
    uploaded_metadata="$(python3 - "$uploaded_json" "$name" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    asset = json.load(source)
if asset.get("name") != sys.argv[2]:
    raise SystemExit(f"Forgejo returned the wrong uploaded asset name for {sys.argv[2]}")
url = asset.get("browser_download_url")
size = asset.get("size")
if not isinstance(url, str) or not url:
    raise SystemExit(f"uploaded release asset {sys.argv[2]} has no download URL")
if not isinstance(size, int) or size < 0:
    raise SystemExit(f"uploaded release asset {sys.argv[2]} has an invalid size")
print(f"{size}\t{url}")
PY
)"
    IFS=$'\t' read -r uploaded_size uploaded_url <<<"$uploaded_metadata"
    local_size="$(wc -c <"$artifact" | tr -d '[:space:]')"
    if [ "$uploaded_size" != "$local_size" ]; then
      printf 'uploaded release asset size mismatch for %s: remote size %s, local size %s\n' \
        "$name" "$uploaded_size" "$local_size" >&2
      exit 1
    fi
    uploaded="$work/uploaded-bytes-$index"
    download_asset "$uploaded_url" "$uploaded"
    if ! cmp --silent "$artifact" "$uploaded"; then
      printf 'uploaded release asset bytes differ from local artifact: %s\n' "$name" >&2
      exit 1
    fi
    printf 'published and verified %s\n' "$name"
  fi
  processed=$((processed + 1))
done < <(find "$asset_root" -type f -print0 | sort -z)

[ "$processed" -gt 0 ] || {
  printf 'no release assets found under %s\n' "$asset_root" >&2
  exit 1
}

# Keep a newly created or resumed release private until every local artifact
# has been uploaded and read back byte-for-byte. A failed run therefore leaves
# a recoverable draft rather than a partially visible public release.
if [ "$release_state" = draft ]; then
  publish_payload="$(python3 - "$GITHUB_REF_NAME" <<'PY'
import json, sys
tag = sys.argv[1]
print(json.dumps({
    "tag_name": tag,
    "name": f"Tilo Imager {tag}",
    "body": "Signed Tilo Imager downloads. Verify each adjacent SHA-256 file before opening an artifact. Linux users can authenticate detached signatures with tilo-release-key.asc after comparing tilo-release-key.fingerprint with the fingerprint published through an independent Tilo channel.",
    "draft": False,
    "prerelease": False,
}))
PY
)"
  curl --fail --silent --show-error -X PATCH \
    -H "$auth" -H 'Content-Type: application/json' \
    --data "$publish_payload" "$api/releases/$release_id" >"$release_json"
  python3 - "$release_json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    release = json.load(source)
if release.get("draft") is not False:
    raise SystemExit("Forgejo did not publish the fully verified release")
PY
  printf 'published complete release %s\n' "$GITHUB_REF_NAME"
fi
