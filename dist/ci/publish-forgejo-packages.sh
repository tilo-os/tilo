#!/usr/bin/env bash
# Publish a verified tag's artifacts to Forgejo's native package registries.
set -euo pipefail

[[ $# == 2 ]] || { echo "usage: $0 RELEASE_DIR VERSION" >&2; exit 2; }
release_dir=$(realpath "$1")
version=$2
server=${FORGEJO_SERVER_URL:-${GITHUB_SERVER_URL:?missing Forgejo server URL}}
server=${server%/}
repository=${FORGEJO_REPOSITORY:-${GITHUB_REPOSITORY:?missing repository name}}
owner=${repository%%/*}
actor=${FORGEJO_ACTOR:-${GITHUB_ACTOR:?missing Forgejo actor}}
if [[ ${TILO_PUBLISH_DRY_RUN:-0} == 1 ]]; then
    token=dry-run
else
    token=${TILO_FORGEJO_TOKEN:?missing TILO_FORGEJO_TOKEN}
fi

upload() {
    local file=$1
    local url=$2
    local mode=${3:-file}
    if [[ ${TILO_PUBLISH_DRY_RUN:-0} == 1 ]]; then
        printf 'would publish %s -> %s\n' "$(basename "$file")" "$url"
        return
    fi
    local response
    response=$(mktemp)
    local status
    if [[ $mode == data ]]; then
        status=$(curl --silent --show-error --retry 3 --retry-all-errors \
            --output "$response" --write-out '%{http_code}' \
            --user "$actor:$token" --request PUT \
            --header 'Content-Type: application/octet-stream' \
            --data-binary "@$file" "$url")
    else
        status=$(curl --silent --show-error --retry 3 --retry-all-errors \
            --output "$response" --write-out '%{http_code}' \
            --user "$actor:$token" --request PUT --upload-file "$file" "$url")
    fi
    case "$status" in
        200|201|204)
            printf 'published %s\n' "$(basename "$file")"
            ;;
        409)
            # Forgejo registries are immutable per package version. A rerun of
            # an already-published tag is therefore complete, not destructive.
            printf 'already published: %s\n' "$(basename "$file")"
            ;;
        *)
            echo "Forgejo upload failed ($status): $(basename "$file")" >&2
            sed -n '1,20p' "$response" >&2
            return 1
            ;;
    esac
}

shopt -s nullglob
for file in "$release_dir"/*+ubuntu24.04*.deb; do
    upload "$file" "$server/api/packages/$owner/debian/pool/noble/main/upload"
done
for file in "$release_dir"/*+deb13*.deb; do
    upload "$file" "$server/api/packages/$owner/debian/pool/trixie/main/upload"
done
for file in "$release_dir"/*.rpm; do
    upload "$file" "$server/api/packages/$owner/rpm/fedora/44/upload"
done
for file in "$release_dir"/*.pkg.tar.zst; do
    upload "$file" "$server/api/packages/$owner/arch/stable" data
done
for file in "$release_dir"/*.nar.gz "$release_dir/SHA256SUMS"; do
    [[ -f $file ]] || continue
    upload "$file" \
        "$server/api/packages/$owner/generic/tilo-session-nix/$version/$(basename "$file")"
done
