#!/usr/bin/env bash
# The local model catalog's release steps, as t's checks run them (t.toml, "Local model
# catalog"; docs/publishing-on-t.md). Building, signing and publishing are separate checks: only
# `sign` and `review` hold the catalog's signing seed, only `publish` the package token.
#
#   compiler OUT        build tilo-model-catalog without credentials, into OUT/
#   sign COMPILER OUT   fetch the reviewed artifacts, sign the catalog with
#                       TILO_MODEL_CATALOG_SIGNING_KEY into OUT/ and require it to reproduce the
#                       checked-in catalog and keys byte for byte
#   publish SIGNED      upload the speech artifacts and SIGNED's catalog and keys to their
#                       immutable package URLs with REGISTRY_TOKEN, then read them back anonymously
#   review COMPILER OUT sign the speech candidate (catalog-source-speech-candidate.json) for review
#
# `sign`, `publish` and `review` run only in trusted runs of stable or a release tag (T_REF).
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
source "$repo_root/tools/ci/publish/lib.sh"
cd "$repo_root"

extensions=os/usr/share/tilo/system-extensions
package_base=https://code.tilo.so/api/packages/tilo/generic/tilo-local-model-catalog

release_only() {
  [[ $(channel) == release ]] || die "the model catalog is signed and published from stable or a release tag, not ${T_REF}"
  require_commit
}

# The reviewed catalog's source and its artifacts, fetched and checked against their receipts,
# in $1 (source.json and artifacts/).
stage() {
  local dir=$1
  mkdir -p "$dir/artifacts"
  cp dist/local-models/catalog-source.json "$dir/source.json"
  if [[ $(jq '.models | length' "$dir/source.json") == 4 ]]; then
    python3 -I tools/release/prepare_speech_catalog.py "$dir/staged" \
      --source dist/local-models/catalog-source.json
    cp "$dir/staged/artifacts/"* "$dir/artifacts/"
  else
    jq -e '.production == true and .runtimes == [] and .recommendation == ""
      and (.models | length) == 1 and .models[0].id == "u2net-subject"
      and .models[0].artifact.file == "artifacts/u2net.onnx"' "$dir/source.json" >/dev/null
    curl --fail --silent --show-error --proto '=https' --tlsv1.2 \
      --output "$dir/artifacts/u2net.onnx" "$(jq -er '.models[0].artifact.url' "$dir/source.json")"
    curl --fail --silent --show-error --proto '=https' --tlsv1.2 \
      --output "$dir/artifacts/LICENSE" "$(jq -er '.models[0].license_url' "$dir/source.json")"
    (cd "$dir/artifacts" && sha256sum -c "$repo_root/dist/local-models/u2net-subject/model-files.sha256")
    [[ $(stat --format=%s "$dir/artifacts/u2net.onnx") == 175997641 ]]
    [[ $(stat --format=%s "$dir/artifacts/LICENSE") == 11357 ]]
  fi
}

# Sign $2 (a source.json beside its artifacts) with the seed into $3/catalog.json and keys.json.
sign_source() {
  local compiler=$1 source=$2 out=$3 key
  : "${TILO_MODEL_CATALOG_SIGNING_KEY:?the catalog signing seed is not set: the check needs secrets = [\"TILO_MODEL_CATALOG_SIGNING_KEY\"]}"
  mkdir -p "$out"
  key=$(mktemp "$scratch/model-catalog-key.XXXXXX")
  trap 'rm -f "$key"' RETURN
  (umask 077 && printf '%s\n' "$TILO_MODEL_CATALOG_SIGNING_KEY" > "$key")
  unset TILO_MODEL_CATALOG_SIGNING_KEY
  "$compiler" --source "$source" --signing-key "$key" \
    --catalog-out "$out/catalog.json" --trust-out "$out/keys.json"
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
    work=$(mktemp -d "$scratch/model-catalog.XXXXXX")
    stage "$work"
    sign_source "$compiler" "$work/source.json" "$out"
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
    if [[ $(jq '.models | length' dist/local-models/catalog-source.json) == 4 ]]; then
      python3 -I tools/release/prepare_speech_catalog.py "$work/staged" \
        --source dist/local-models/catalog-source.json
      PACKAGE_TOKEN=$REGISTRY_TOKEN python3 -I tools/release/publish_speech_artifacts.py "$work/staged"
    fi
    version=$(sha256sum "$signed/catalog.json" | cut -d' ' -f1)
    base=$package_base/$version
    for file in catalog.json keys.json; do
      if curl --fail --silent --show-error --proto '=https' --output "$work/existing-$file" "$base/$file"; then
        cmp "$signed/$file" "$work/existing-$file"
      else
        # The token goes through curl's configuration on stdin, never its arguments.
        printf 'header = "Authorization: token %s"\n' "$REGISTRY_TOKEN" \
          | curl --fail --silent --show-error --proto '=https' --config - \
            --upload-file "$signed/$file" "$base/$file"
      fi
      curl --fail --silent --show-error --proto '=https' --output "$work/published-$file" "$base/$file"
      cmp "$signed/$file" "$work/published-$file"
    done
    printf 'Published the reviewed catalog %s/catalog.json from %s\n' "$base" "$T_COMMIT"
    ;;
  review)
    compiler=${1:?usage: local-model-catalog.sh review COMPILER OUT} out=${2:?}
    release_only
    work=$(mktemp -d "$scratch/speech-review.XXXXXX")
    python3 -I tools/release/prepare_speech_catalog.py "$work/speech-review"
    sign_source "$compiler" "$work/speech-review/source.json" "$out"
    cp "$work/speech-review/source.json" "$out/source.json"
    rm -rf "$work"
    cmp "$out/keys.json" "$extensions/local-model-keys.json"
    (cd "$out" && sha256sum catalog.json keys.json source.json > review.sha256)
    printf 'Signed the speech candidate for review: t outputs review-speech-catalog\n'
    ;;
  *)
    die "usage: local-model-catalog.sh compiler|sign|publish|review ..."
    ;;
esac
