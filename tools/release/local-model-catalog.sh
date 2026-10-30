#!/usr/bin/env bash
# The local model catalog's steps. Building, signing and publishing are separate steps: only `sign` and
# `review` hold the catalog's signing seed, only `publish` and `publish-runtime` a package token.
#
#   compiler OUT        build tilo-model-catalog without credentials, into OUT/
#   sign COMPILER OUT   fetch the reviewed artifacts, sign the catalog with
#                       TILO_MODEL_CATALOG_SIGNING_KEY into OUT/ and require it to reproduce the
#                       checked-in catalog and keys byte for byte
#   publish SIGNED      upload the speech artifacts, the runtime archives and SIGNED's catalog and
#                       keys to their immutable package URLs with REGISTRY_TOKEN, then read them
#                       back anonymously
#   review COMPILER OUT sign dist/local-models/catalog-source.json with the runtimes this tree
#                       builds for review: OUT/source.json and OUT/catalog.json become the
#                       checked-in source and catalog
#   dev ARCH COMPILER OUT
#                       sign the same source with the runtimes for ARCH and a throwaway key into
#                       OUT/catalog.json and keys.json, the catalog ARCH's dev images carry in
#                       place of the release's
#   publish-runtime ID  upload runtime ID's archive as this tree builds it to its immutable package
#                       URL with TILO_PACKAGES_TOKEN, so dev catalogs naming it download it
#
# The runtimes' archives and entries come from the local-model-runtime checks, under
# $runtimes_out/<id>/.
#
# `sign`, `publish` and `review` run only in trusted runs of stable or a release tag (T_REF),
# `publish-runtime` only in main's; `dev` holds nothing and runs anywhere.
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
source "$repo_root/tools/ci/publish/lib.sh"
cd "$repo_root"

extensions=os/usr/share/tilo/system-extensions
package_base=https://code.tilo.so/api/packages/tilo/generic/tilo-local-model-catalog
runtimes_out=/target/local-model-runtimes
dev_key_id=tilo-local-model-dev

release_only() {
  [[ $(channel) == release ]] || die "the model catalog is signed and published from stable or a release tag, not ${T_REF}"
  require_commit
}

# The models of the catalog's source alone, as the speech preparer reads it, into $1.
models_source() {
  jq '.runtimes = [] | .recommendation = ""' dist/local-models/catalog-source.json > "$1"
}

# The catalog's source with the runtimes this tree builds (for architecture $2 alone, if given),
# as their builds wrote their entries, into $1: what review signs and dev images carry.
built_source() {
  local id entries=()
  for id in $(bash tools/release/local-model-runtime.sh ids); do
    [[ -z ${2:-} || $id == *-"$2" ]] || continue
    [[ -f $runtimes_out/$id/runtime.json ]] || die "no build of runtime $id: the check needs local-model-runtime-$id"
    entries+=("$runtimes_out/$id/runtime.json")
  done
  jq --slurpfile runtimes <(jq -s . "${entries[@]}") '.runtimes = $runtimes[0]' \
    dist/local-models/catalog-source.json > "$1"
}

# Point $1/source.json's runtimes at their archives as their builds left them, after checking
# each against its receipt. They are too large to copy, and the compiler refuses links.
stage_runtimes() {
  local dir=$1 id sha size archive
  while IFS=$'\t' read -r id sha size; do
    archive=$runtimes_out/$id/$id.oci.tar
    [[ -f $archive ]] || die "no build of runtime $id: the check needs local-model-runtime-$id"
    [[ $(stat --format=%s "$archive") == "$size" ]] || die "runtime $id was rebuilt with another size"
    echo "$sha  $archive" | sha256sum --check --quiet || die "runtime $id was rebuilt with other bytes"
  done < <(jq -r '.runtimes[] | [.id, .artifact.sha256, .artifact.size_bytes] | @tsv' "$dir/source.json")
  jq --arg out "$runtimes_out" '.runtimes |= map(.artifact.file = "\($out)/\(.id)/\(.id).oci.tar")' \
    "$dir/source.json" > "$dir/source.staged.json"
  mv "$dir/source.staged.json" "$dir/source.json"
}

# The catalog source $2 and its artifacts, fetched and checked against their receipts, in $1
# (source.json and artifacts/).
stage() {
  local dir=$1 source=$2
  mkdir -p "$dir/artifacts"
  cp "$source" "$dir/source.json"
  models_source "$dir/models-source.json"
  python3 -I tools/release/prepare_speech_catalog.py "$dir/staged" --source "$dir/models-source.json"
  cp "$dir/staged/artifacts/"* "$dir/artifacts/"
  stage_runtimes "$dir"
}

# Upload FILE to the immutable package URL with TOKEN unless it holds those bytes already, then
# read it back anonymously. A different object at the URL is a conflict, never overwritten.
publish_file() {
  local file=$1 url=$2 token=$3 sha
  sha=$(sha256sum "$file" | cut -d' ' -f1)
  if curl --fail --silent --head --proto '=https' --output /dev/null "$url"; then
    [[ $(curl --fail --silent --show-error --proto '=https' "$url" | sha256sum | cut -d' ' -f1) == "$sha" ]] \
      || die "$url holds other bytes"
  else
    # The token goes through curl's configuration on stdin, never its arguments.
    printf 'header = "Authorization: token %s"\n' "$token" \
      | curl --fail --silent --show-error --proto '=https' --config - --upload-file "$file" "$url"
  fi
  [[ $(curl --fail --silent --show-error --proto '=https' "$url" | sha256sum | cut -d' ' -f1) == "$sha" ]] \
    || die "$url does not read back as published"
}

# Sign $2 (a source.json beside its artifacts) with the seed $4 into $3/catalog.json and keys.json.
sign_source() {
  local compiler=$1 source=$2 out=$3 seed=$4 key
  mkdir -p "$out"
  key=$(mktemp "$scratch/model-catalog-key.XXXXXX")
  trap 'rm -f "$key"' RETURN
  (umask 077 && printf '%s\n' "$seed" > "$key")
  "$compiler" --source "$source" --signing-key "$key" \
    --catalog-out "$out/catalog.json" --trust-out "$out/keys.json"
}

# The release seed, taken out of the environment.
release_seed() {
  : "${TILO_MODEL_CATALOG_SIGNING_KEY:?the catalog signing seed is not set: the check needs secrets = [\"TILO_MODEL_CATALOG_SIGNING_KEY\"]}"
  seed=$TILO_MODEL_CATALOG_SIGNING_KEY
  unset TILO_MODEL_CATALOG_SIGNING_KEY
}

command=${1:-}
shift || true
case $command in
  compiler)
    out=${1:?usage: local-model-catalog.sh compiler OUT}
    cargo build --locked -p local-model-service --bin tilo-model-catalog
    install -D -m 0755 "${CARGO_TARGET_DIR:-target}/debug/tilo-model-catalog" "$out/tilo-model-catalog"
    ;;
  sign)
    compiler=${1:?usage: local-model-catalog.sh sign COMPILER OUT} out=${2:?}
    release_only
    release_seed
    work=$(mktemp -d "$scratch/model-catalog.XXXXXX")
    stage "$work" dist/local-models/catalog-source.json
    sign_source "$compiler" "$work/source.json" "$out" "$seed"
    rm -rf "$work"
    # The catalog an OS image carries is the checked-in one: signing must reproduce it exactly.
    cmp "$out/catalog.json" "$extensions/local-model-catalog.json"
    cmp "$out/keys.json" "$extensions/local-model-keys.json"
    printf 'Signed the reviewed catalog %s\n' "$(sha256sum "$out/catalog.json" | cut -d' ' -f1)"
    ;;
  publish)
    signed=${1:?usage: local-model-catalog.sh publish SIGNED}
    release_only
    : "${REGISTRY_TOKEN:?the package token is not set: the check needs secrets = [\"REGISTRY_TOKEN\"]}"
    cmp "$signed/catalog.json" "$extensions/local-model-catalog.json"
    cmp "$signed/keys.json" "$extensions/local-model-keys.json"
    work=$(mktemp -d "$scratch/model-catalog.XXXXXX")
    trap 'rm -rf "$work"' EXIT
    models_source "$work/models-source.json"
    python3 -I tools/release/prepare_speech_catalog.py "$work/staged" --source "$work/models-source.json"
    PACKAGE_TOKEN=$REGISTRY_TOKEN python3 -I tools/release/publish_speech_artifacts.py "$work/staged"
    cp dist/local-models/catalog-source.json "$work/source.json"
    stage_runtimes "$work"
    while IFS=$'\t' read -r file url; do
      publish_file "$file" "$url" "$REGISTRY_TOKEN"
    done < <(jq -r '.runtimes[] | [.artifact.file, .artifact.url] | @tsv' "$work/source.json")
    version=$(sha256sum "$signed/catalog.json" | cut -d' ' -f1)
    base=$package_base/$version
    for file in catalog.json keys.json; do
      publish_file "$signed/$file" "$base/$file" "$REGISTRY_TOKEN"
    done
    printf 'Published the reviewed catalog %s/catalog.json from %s\n' "$base" "$T_COMMIT"
    ;;
  review)
    compiler=${1:?usage: local-model-catalog.sh review COMPILER OUT} out=${2:?}
    release_only
    release_seed
    mkdir -p "$out"
    built_source "$out/source.json"
    work=$(mktemp -d "$scratch/model-catalog-review.XXXXXX")
    stage "$work" "$out/source.json"
    sign_source "$compiler" "$work/source.json" "$out" "$seed"
    rm -rf "$work"
    cmp "$out/keys.json" "$extensions/local-model-keys.json"
    (cd "$out" && sha256sum catalog.json keys.json source.json > review.sha256)
    printf 'Signed the catalog source for review: t outputs review-model-catalog\n'
    ;;
  dev)
    arch=${1:?usage: local-model-catalog.sh dev ARCH COMPILER OUT} compiler=${2:?} out=${3:?}
    work=$(mktemp -d "$scratch/model-catalog-dev.XXXXXX")
    built_source "$work/built.json" "$arch"
    jq --arg key "$dev_key_id" '.key_id = $key' "$work/built.json" > "$work/dev.json"
    stage "$work" "$work/dev.json"
    # A dev image carries this catalog and the key that verifies it, both covered by the image's
    # own signature: the development key vouches for them, so the catalog key is throwaway.
    sign_source "$compiler" "$work/source.json" "$out" "$(od -An -tx1 -N32 /dev/urandom | tr -d ' \n')"
    rm -rf "$work"
    jq -e --arg key "$dev_key_id" '.key_id == $key and (.payload.runtimes | length) > 0' \
      "$out/catalog.json" >/dev/null || die 'the dev catalog names no runtime'
    printf 'Signed the %s dev catalog with %s runtimes\n' "$arch" "$(jq '.payload.runtimes | length' "$out/catalog.json")"
    ;;
  publish-runtime)
    id=${1:?usage: local-model-catalog.sh publish-runtime ID}
    [[ $(channel) == dev ]] || die "the runtimes dev images name are published from main, not ${T_REF}"
    require_commit
    : "${TILO_PACKAGES_TOKEN:?the package token is not set: the check needs secrets = [\"TILO_PACKAGES_TOKEN\"]}"
    entry=$runtimes_out/$id/runtime.json
    [[ -f $entry ]] || die "no build of runtime $id: the check needs local-model-runtime-$id"
    archive=$runtimes_out/$id/$id.oci.tar
    echo "$(jq -r .artifact.sha256 "$entry")  $archive" | sha256sum --check --quiet \
      || die "runtime $id's archive is not the one its entry names"
    url=$(jq -r .artifact.url "$entry")
    publish_file "$archive" "$url" "$TILO_PACKAGES_TOKEN"
    printf 'published %s\n' "$url"
    ;;
  *)
    die "usage: local-model-catalog.sh compiler|sign|publish|review|dev|publish-runtime ..."
    ;;
esac
