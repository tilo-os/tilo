#!/usr/bin/env bash
# Reuse an authenticated, digest-pinned prior collection only for partial builds.

set -euo pipefail

index=${1:?usage: reuse_prior_app_collection.sh INDEX PLAN TAPP_DIR ARCH URL SHA256}
collection_plan=${2:?usage: reuse_prior_app_collection.sh INDEX PLAN TAPP_DIR ARCH URL SHA256}
tapp_dir=${3:?usage: reuse_prior_app_collection.sh INDEX PLAN TAPP_DIR ARCH URL SHA256}
arch=${4:?usage: reuse_prior_app_collection.sh INDEX PLAN TAPP_DIR ARCH URL SHA256}
prior_url=${5:-}
prior_sha=${6:-}
temp_root=${RUNNER_TEMP:-${TMPDIR:-/tmp}}

[[ $arch == x86_64 || $arch == aarch64 ]] || {
  printf 'unsupported app collection architecture: %s\n' "$arch" >&2
  exit 1
}

built_count=$(jq -er '.apps | length' "$index")
collection_count=$(jq -er '.catalog | length' "$collection_plan")
(( built_count <= collection_count )) || {
  printf 'built app count exceeds collection plan: %s > %s\n' "$built_count" "$collection_count" >&2
  exit 1
}

# A complete build already contains every member. Returning before URL, token,
# or network validation is deliberate: the prior collection is irrelevant.
if (( built_count == collection_count )); then
  python3 "$(dirname "$0")/apps/dev_app_versions.py" --plan "$collection_plan" --built-index "$index"
  exit 0
fi

[[ -n $prior_url ]] || {
  printf 'partial app build requires a prior collection URL\n' >&2
  exit 1
}
[[ $prior_sha =~ ^[0-9a-fA-F]{64}$ ]] || {
  printf 'partial app build requires a pinned prior collection SHA-256\n' >&2
  exit 1
}
[[ -n ${TILO_PACKAGE_TOKEN:-} ]] || {
  printf 'TILO_PACKAGE_TOKEN is required to read the prior collection\n' >&2
  exit 1
}
public_base=${TILO_PACKAGE_PUBLIC_BASE_URL%/}
[[ $public_base == https://*/api/packages/*/generic \
      && $public_base != *'.svc.cluster.local'* ]] || {
  printf 'TILO_PACKAGE_PUBLIC_BASE_URL must be a public HTTPS generic-package base: %s\n' \
    "$public_base" >&2
  exit 1
}

# Repository variables created by older runners can contain the cluster-local
# Forgejo address. Never send the bearer token over that plain-HTTP URL: use
# the equivalent public TLS origin for every authenticated read.
cluster_package_api=http://forgejo.forgejo.svc.cluster.local:3000/api/packages/
case "$prior_url" in
  "$cluster_package_api"*/generic/*)
    prior_url="$public_base/${prior_url#*/generic/}"
    ;;
  "$public_base"/*) ;;
  *)
    printf 'unsupported prior app collection URL: %s\n' "$prior_url" >&2
    exit 1
    ;;
esac

state=$(mktemp -d "$temp_root/tilo-prior-collection.XXXXXX")
cleanup() { rm -rf -- "$state"; }
trap cleanup EXIT
prior="$state/collection.tar.gz"
prior_dir="$state/extracted"
mkdir -p "$prior_dir" "$tapp_dir"

curl --fail --silent --show-error --retry 3 --retry-all-errors \
  --proto '=https' --proto-redir '=https' \
  --header "Authorization: Bearer $TILO_PACKAGE_TOKEN" \
  "$prior_url" --output "$prior"
printf '%s  %s\n' "$prior_sha" "$prior" | sha256sum --check --strict

tar -tzf "$prior" | "$(dirname "${BASH_SOURCE[0]}")/apps/check-collection-members.sh" "prior collection"
tar --extract --gzip --file "$prior" --directory "$prior_dir" \
  --no-same-owner --no-same-permissions
[[ -d $prior_dir/tapp && -d $prior_dir/runtime/tilo-runtime ]] || {
  printf 'prior app collection is missing tapp or runtime content\n' >&2
  exit 1
}
if find "$prior_dir/tapp" "$prior_dir/runtime" -type l -o \( ! -type f ! -type d \) | grep -q .; then
  printf 'prior app collection contains links or special files\n' >&2
  exit 1
fi

mapfile -t new_manifests < <(find "${index%/index.json}/runtime/tilo-runtime" \
  -mindepth 2 -maxdepth 2 -name bundle.toml -type f -print)
mapfile -t prior_manifests < <(find "$prior_dir/runtime/tilo-runtime" \
  -mindepth 2 -maxdepth 2 -name bundle.toml -type f -print)
if (( ${#new_manifests[@]} != 1 || ${#prior_manifests[@]} != 1 )); then
  printf 'partial app build requires exactly one new and prior runtime manifest\n' >&2
  exit 1
fi
new_root=$(sed -n 's/^root_hash *= *"\([0-9a-fA-F]\{64\}\)".*/\1/p' "${new_manifests[0]}")
prior_root=$(sed -n 's/^root_hash *= *"\([0-9a-fA-F]\{64\}\)".*/\1/p' "${prior_manifests[0]}")
if [[ -z $new_root || ${new_root,,} != ${prior_root,,} ]]; then
  printf 'shared runtime changed; the full collection must be rebuilt\n' >&2
  exit 1
fi

shopt -s nullglob
prior_tapps=("$prior_dir"/tapp/*.tapp)
(( ${#prior_tapps[@]} > 0 )) || {
  printf 'prior app collection contains no tapps\n' >&2
  exit 1
}
python3 "$(dirname "$0")/apps/dev_app_versions.py" --plan "$collection_plan" \
  --built-index "$index" --prior-index "$prior_dir/index.json"
cp "${prior_tapps[@]}" "$tapp_dir/"
