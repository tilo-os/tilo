#!/usr/bin/env bash
# Publish the tapps and app collections a Release / apps build signed.
#
# tools/apps/t-release.sh runs this from the repository root with the signed
# packages in release/<arch>/, in two stages that hold different credentials
# (TILO_APP_PUBLISH_STAGE):
#
#   upload  with TILO_PACKAGE_TOKEN, a token that may write generic packages:
#           uploads each tapp, assembles and uploads each collection, writes
#           the collection channel (TILO_APP_CHANNEL_OUT, the pointer an OS
#           build takes its apps from), and uploads the registry's media and
#           the runtime package's objects.
#   submit  with TAPP_PUBLISHER_IDENTITY and no package token: signs each
#           tapp's registry entry and submits it to the registry, then offers
#           the runtime build the apps link as so.tilo.runtime, so a device
#           without that build fetches it with the apps.
#
# Both take VERSION, COLLECTION, COMPLETE, BUILDER_IMAGE, the PRIOR_*
# collection inputs, TILO_PACKAGE_PUBLIC_BASE_URL, TILO_REGISTRY_URL,
# TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY, GITHUB_SERVER_URL, GITHUB_REPOSITORY,
# GITHUB_REF, GITHUB_SHA and RUNNER_TEMP, and with VERIFY_FINAL_RECEIPT=true
# the FINAL_RECEIPT_POLICY inputs.
#
# Unset, the stage is `all`: release-apps.yml's publish job does both with one
# job's OIDC token (enable-openid-connect, audience MAIN_AUDIENCE) and
# TAPP_PUBLISHER_IDENTITY, and for main's default collection advances the
# TILO_APP_COLLECTION_* repository variables instead of writing a channel file.
set -euo pipefail
stage=${TILO_APP_PUBLISH_STAGE:-all}
case $stage in
  upload) jwt=${TILO_PACKAGE_TOKEN:?the upload stage needs TILO_PACKAGE_TOKEN} ;;
  submit) jwt= ;;
  all)
    audience=$MAIN_AUDIENCE
    [[ -n $audience ]] || { printf 'app release Authorized Integration audience is required\n' >&2; exit 1; }
    jwt=$(curl --fail --silent --show-error \
      --header "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
      "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=$audience" | jq -er .value)
    echo "::add-mask::$jwt"
    ;;
  *) printf 'TILO_APP_PUBLISH_STAGE must be upload or submit, not %s\n' "$stage" >&2; exit 2 ;;
esac
if [[ ${VERIFY_FINAL_RECEIPT:-false} == true ]]; then
  policy_file="$RUNNER_TEMP/final-receipt-policy.json"
  printf '%s' "$FINAL_RECEIPT_POLICY" > "$policy_file"
  verifier_tools="$RUNNER_TEMP/platform-verifiers"
  python3 -I tools/ci/python.py tools/canonical_verifier_bundle.py fetch \
    --trust-policy "$policy_file" --trust-policy-sha256 "$FINAL_RECEIPT_POLICY_SHA256" \
    --builder-image "$BUILDER_IMAGE" --output "$verifier_tools"
  python3 -I tools/ci/python.py tools/apps/final_app_receipt.py \
    --trust-policy "$policy_file" --trust-policy-sha256 "$FINAL_RECEIPT_POLICY_SHA256" preflight \
    --pkg-binary "$verifier_tools/tilo-app-package" --runtime-binary "$verifier_tools/tilo-runtime-bundle"
fi
owner=${GITHUB_REPOSITORY%%/*}
package_api="$GITHUB_SERVER_URL/api/packages/$owner/generic"
package_public_api=${TILO_PACKAGE_PUBLIC_BASE_URL%/}
[[ $package_public_api == https://*/api/packages/*/generic \
      && $package_public_api != *'.svc.cluster.local'* ]] || {
  printf 'TILO_PACKAGE_PUBLIC_BASE_URL must be a public HTTPS generic-package base: %s\n' \
    "$package_public_api" >&2
  exit 1
}

host_arch=$(uname -m)
registry_cli=
for index in release/*/index.json; do
  if [[ $(jq -er .architecture "$index") == "$host_arch" ]]; then
    registry_cli="${index%/index.json}/tilo-app-package"
    break
  fi
done
[[ -n $registry_cli && -f $registry_cli ]] || {
  printf 'no native tilo-app-package registry client artifact found for %s\n' "$host_arch" >&2
  exit 1
}
chmod 0755 "$registry_cli"

# Every app/version/architecture is independently addressable. A
# one-app update uploads one tapp; collection assembly below fetches
# all unchanged members instead of rebuilding them. Beside each tapp
# goes its compressed twin, <file>.zst, which clients download first
# (a third of the bytes) and decompress, bounded by the signed size,
# before the signed hash check; the registry entry signs only the tapp.
upload_manifest="$RUNNER_TEMP/tilo-package-uploads.tsv"
twins="$RUNNER_TEMP/tilo-tapp-twins"
mkdir -p "$twins"
: > "$upload_manifest"
for index in release/*/index.json; do
  while IFS=$'\t' read -r name version file; do
    tapp="${index%/index.json}/tapp/$file"
    test -f "$tapp"
    "$registry_cli" compress -o "$twins/$file.zst" "$tapp"
    printf '%s\t%s\n' "$tapp" \
      "$package_api/tilo-app-$name/$VERSION/$file" >> "$upload_manifest"
    printf '%s\t%s\n' "$twins/$file.zst" \
      "$package_api/tilo-app-$name/$VERSION/$file.zst" >> "$upload_manifest"
  done < <(jq -r '.apps[] | [.name,.version,.file] | @tsv' "$index")
done
if [[ $stage != submit ]]; then
TILO_PACKAGE_TOKEN="$jwt" TILO_PACKAGE_UPLOAD_WORKERS=8 \
  tools/upload_immutable_packages.sh "$upload_manifest"
fi

# Assemble each collection from this run's tapps and the prior collection's
# unchanged members, upload it, and, for main's default collection, advance
# the OS input channel. This runs before the store registry is touched: a
# release image takes its apps from this channel, never from the registry,
# so a registry refusal or outage must not hold the OS back. The registry
# submission below still fails the job loudly. The channel has then moved
# past this run's apps, so the next main run does not resubmit the ones it
# does not rebuild: they reach the registry with their next change, or at
# once through a Release / apps dispatch that names them with publish on.
publish_collections() {
  collection_plan="$RUNNER_TEMP/tilo-collection-plan.json"
  python3 -I tools/ci/python.py tools/apps/app_release.py --collection "$COLLECTION" > "$collection_plan"
  # t's release steps pass the commit's time, as they run without Git (tools/apps/t-release.sh).
  source_epoch=${TILO_SOURCE_EPOCH:-$(git show -s --format=%ct "$GITHUB_SHA")}
  [[ $source_epoch =~ ^[0-9]+$ ]]
  channel_url=
  channel_sha=
  for index in release/*/index.json; do
    arch=$(jq -er '.architecture | select(. == "x86_64" or . == "aarch64")' "$index")
    tapp_dir="$RUNNER_TEMP/tilo-collection-$arch"
    mkdir -p "$tapp_dir"
    prior_url=
    prior_sha=
    # Both continuous pushes and explicitly publishing manual runs on
    # main may replace one app in the immutable prior collection.
    # Tags remain self-contained release boundaries and rebuild their
    # complete requested set instead of depending on a moving channel.
    if [[ $GITHUB_REF == refs/heads/main ]]; then
      case "$arch" in
        x86_64) prior_url=$PRIOR_X86_URL; prior_sha=$PRIOR_X86_SHA256 ;;
        aarch64) prior_url=$PRIOR_ARM64_URL; prior_sha=$PRIOR_ARM64_SHA256 ;;
      esac
    fi
    TILO_PACKAGE_TOKEN="$jwt" tools/reuse_prior_app_collection.sh \
      "$index" "$collection_plan" "$tapp_dir" "$arch" "$prior_url" "$prior_sha"
    while IFS=$'\t' read -r name version; do
      file="$name-$version-$arch.tapp"
      built="${index%/index.json}/tapp/$file"
      if [[ -f $built ]]; then
        find "$tapp_dir" -maxdepth 1 -type f -name "$name-*-$arch.tapp" -delete
        cp "$built" "$tapp_dir/$file"
      fi
      test -f "$tapp_dir/$file" || {
        printf 'collection member %s was neither rebuilt nor reusable\n' "$file" >&2
        exit 1
      }
    done < <(jq -r '.catalog[] | [.name,.version] | @tsv' "$collection_plan")
    out="dist/app-collections/$arch"
    TILO_APP_COLLECTION_PLAN="$collection_plan" \
      TILO_APP_ARCH="$arch" TILO_APP_TAPP_DIR="$tapp_dir" \
      TILO_APP_RUNTIME_DIR="${index%/index.json}/runtime" \
      TILO_APP_COLLECTION_OUT="$out" SOURCE_DATE_EPOCH="$source_epoch" \
      dist/apps/assemble-collection.sh
    bundle="$out/tilo-apps-$arch.tar.gz"
    digest_file="$bundle.sha256"
    # Runs for one source can select different apps and reuse different prior
    # members. Their immutable collection address must identify the bytes too.
    bundle_sha=$(sha256sum "$bundle" | cut -d' ' -f1)
    collection_version="$VERSION-$bundle_sha"
    bundle_upload_url="$package_api/tilo-app-collections/$collection_version/${bundle##*/}"
    bundle_url="$package_public_api/tilo-app-collections/$collection_version/${bundle##*/}"
    if [[ ${VERIFY_FINAL_RECEIPT:-false} == true ]]; then
      python3 -I tools/ci/python.py tools/apps/final_app_receipt.py \
        --trust-policy "$policy_file" --trust-policy-sha256 "$FINAL_RECEIPT_POLICY_SHA256" verify \
        --collection "$out" --pkg-binary "$verifier_tools/tilo-app-package" \
        --runtime-binary "$verifier_tools/tilo-runtime-bundle" \
        --source-sha "$GITHUB_SHA" --architecture "$arch"
    fi
    bundle_upload="$RUNNER_TEMP/tilo-collection-bundle-$arch.tsv"
    printf '%s\t%s\n' "$bundle" "$bundle_upload_url" > "$bundle_upload"
    TILO_PACKAGE_TOKEN="$jwt" TILO_PACKAGE_UPLOAD_WORKERS=1 \
      tools/upload_immutable_packages.sh "$bundle_upload"
    metadata_uploads="$RUNNER_TEMP/tilo-collection-metadata-$arch.tsv"
    : > "$metadata_uploads"
    if [[ ${VERIFY_FINAL_RECEIPT:-false} == true ]]; then
      # A successful PUT alone is not evidence that consumers can read the bytes.
      readback="$RUNNER_TEMP/tilo-upload-readback-$arch.tar.gz"
      curl --fail --silent --show-error --header "Authorization: Bearer $jwt" \
        "$bundle_upload_url" --output "$readback"
      final_receipt="$out/final-publication-receipt.json"
      python3 -I tools/ci/python.py tools/apps/final_app_receipt.py \
        --trust-policy "$policy_file" --trust-policy-sha256 "$FINAL_RECEIPT_POLICY_SHA256" bind-readback \
        --receipt "$final_receipt" --receipt-sha256 "$(sha256sum "$final_receipt" | cut -d' ' -f1)" \
        --downloaded "$readback" --url "$bundle_url" --output "$out/published-receipt.json"
      rm -f "$readback"
      printf '%s\t%s\n' \
        "$out/published-receipt.json" \
        "$package_api/tilo-app-collections/$collection_version/tilo-apps-$arch.receipt.json" \
        "$out/published-receipt.upload.json" \
        "$package_api/tilo-app-collections/$collection_version/tilo-apps-$arch.upload.json" \
        >> "$metadata_uploads"
    fi
    printf '%s\t%s\n' "$digest_file" \
      "$package_api/tilo-app-collections/$collection_version/${digest_file##*/}" \
      >> "$metadata_uploads"
    TILO_PACKAGE_TOKEN="$jwt" TILO_PACKAGE_UPLOAD_WORKERS=3 \
      tools/upload_immutable_packages.sh "$metadata_uploads"
    if [[ $arch == x86_64 ]]; then
      channel_url=$bundle_url
      channel_sha=$(awk '{print $1}' "$digest_file")
    else
      arm64_channel_url=$bundle_url
      arm64_channel_sha=$(awk '{print $1}' "$digest_file")
    fi
  done

  # Keep the OS input channel current after a successful default/main
  # collection publish. The package URL remains immutable; only these
  # two repository variables move.
  if [[ $stage == all && $GITHUB_REF == refs/heads/main && $COLLECTION == default ]]; then
    [[ -n $channel_url && $channel_sha =~ ^[0-9a-f]{64}$ ]]
    variable_api="${GITHUB_API_URL:-$GITHUB_SERVER_URL/api/v1}/repos/$GITHUB_REPOSITORY/actions/variables"
    update_variable() {
      local name=$1 value=$2 endpoint status method
      endpoint="$variable_api/$name"
      status=$(curl --silent --output /dev/null --write-out '%{http_code}' \
        --header "Authorization: Bearer $jwt" "$endpoint")
      case "$status" in 200) method=PUT ;; 404) method=POST ;; *) return 1 ;; esac
      curl --fail --silent --show-error --request "$method" \
        --header "Authorization: Bearer $jwt" \
        --header 'Content-Type: application/json' \
        --data "$(jq -cn --arg value "$value" '{value:$value}')" "$endpoint"
    }
    update_variable TILO_APP_COLLECTION_URL "$channel_url"
    update_variable TILO_APP_COLLECTION_SHA256 "$channel_sha"
    update_variable TILO_APP_COLLECTION_URL_X86_64 "$channel_url"
    update_variable TILO_APP_COLLECTION_SHA256_X86_64 "$channel_sha"
    if [[ -n ${arm64_channel_url:-} ]]; then
      update_variable TILO_APP_COLLECTION_URL_AARCH64 "$arm64_channel_url"
      update_variable TILO_APP_COLLECTION_SHA256_AARCH64 "$arm64_channel_sha"
    fi
    # Its runtime carries this builder's system libraries; the next
    # main run may reuse the collection only with the same builder.
    update_variable TILO_APP_COLLECTION_BUILDER "$BUILDER_IMAGE"
    # The next main run selects its tapps relative to this commit.
    if [[ $COMPLETE == true ]]; then
      update_variable TILO_APP_COLLECTION_SOURCE "$GITHUB_SHA"
    fi
    echo '::notice::advanced the default OS app-collection channel'
  fi
  # The channel an OS build takes its apps from: this run's immutable
  # collections, the builder their runtime came from (a later run may reuse
  # the collection only with the same builder) and, when this selection
  # covered every change, the source the next run selects its tapps against.
  if [[ $COLLECTION == default && -n ${TILO_APP_CHANNEL_OUT:-} ]]; then
    [[ -n $channel_url && $channel_sha =~ ^[0-9a-f]{64}$ ]]
    jq -n --arg url_x86_64 "$channel_url" --arg sha256_x86_64 "$channel_sha" \
      --arg url_aarch64 "${arm64_channel_url:-}" --arg sha256_aarch64 "${arm64_channel_sha:-}" \
      --arg builder "$BUILDER_IMAGE" --arg source "$([[ $COMPLETE == true ]] && echo "$GITHUB_SHA")" \
      '{schema: 1, collection: "default",
        x86_64: {url: $url_x86_64, sha256: $sha256_x86_64},
        aarch64: (if $url_aarch64 == "" then null else {url: $url_aarch64, sha256: $sha256_aarch64} end),
        builder: $builder, source: (if $source == "" then null else $source end)}' \
      > "$TILO_APP_CHANNEL_OUT"
    printf 'app collection channel: %s\n' "$(jq -c . "$TILO_APP_CHANNEL_OUT")"
  fi
}
if [[ $stage != submit && -n ${COLLECTION:-} ]]; then
  publish_collections
fi

# Product artwork is immutable alongside its tapp release.  The
# metadata builder always emits an accessible owned icon, and adds
# local or checksum-pinned archived app screenshots, so a store card can
# never become a blank patch of UI again.
registry_url=${TILO_REGISTRY_URL%/}
[[ $registry_url == https://* && $registry_url != *'.svc.cluster.local'* ]] || {
  printf 'TILO_REGISTRY_URL must be a public HTTPS origin\n' >&2
  exit 1
}
registry_metadata="$RUNNER_TEMP/tilo-registry-metadata"
registry_media="$RUNNER_TEMP/tilo-registry-media"
registry_media_urls="$RUNNER_TEMP/tilo-registry-media-public-urls.tsv"
registry_apps=$(for index in release/*/index.json; do jq -r '.apps[].name' "$index"; done | sort -u | xargs)
[[ -n $registry_apps ]] || { printf 'no apps selected for registry metadata\n' >&2; exit 1; }
python3 -I tools/ci/python.py tools/apps/registry_metadata.py build \
  --apps "$registry_apps" \
  --media-base "$registry_url/artifacts/$owner/generic/tilo-app-metadata/$VERSION" \
  --output "$registry_metadata" --media-output "$registry_media"
registry_media_uploads="$RUNNER_TEMP/tilo-registry-media-uploads.tsv"
: > "$registry_media_uploads"
: > "$registry_media_urls"
while IFS= read -r media; do
  printf '%s\t%s\n' "$registry_media/$media" \
    "$package_api/tilo-app-metadata/$VERSION/$media" >> "$registry_media_uploads"
  printf '%s\t%s\n' "$media" \
    "$registry_url/artifacts/$owner/generic/tilo-app-metadata/$VERSION/$media" >> "$registry_media_urls"
done < <(find "$registry_media" -maxdepth 1 -type f -printf '%f\n' | sort)
if [[ $stage != submit ]]; then
  TILO_PACKAGE_TOKEN="$jwt" TILO_PACKAGE_UPLOAD_WORKERS=8 \
    tools/upload_immutable_packages.sh "$registry_media_uploads"
fi

# The runtime package's objects go up with the rest; its entry is signed below.
upload_runtimes() {
  local index
  for index in release/*/index.json; do
    RUNTIME_PACKAGE_STAGE=upload VERSION=$VERSION TILO_PACKAGE_TOKEN=$jwt PACKAGE_API=$package_api \
      REGISTRY_URL=$registry_url OWNER=$owner REGISTRY_CLI=$registry_cli \
      tools/apps/publish-runtime-package.sh "${index%/index.json}/runtime-package" \
      "$(jq -er '.architecture | select(. == "x86_64" or . == "aarch64")' "$index")" \
      so.tilo.runtime tilo-app-runtime
  done
}
if [[ $stage == upload ]]; then
  upload_runtimes
  exit 0
fi

# Archived screenshots are served directly through the existing public proxy.
# Include them in the same availability/type gate before signing any entry.
jq -r --arg base "$registry_url/artifacts/$owner/generic/tilo-app-metadata/$VERSION/" \
  '.images.en[] | select(.url | startswith($base) | not) | [(.url | split("/") | last), .url] | @tsv' \
  "$registry_metadata"/*.json >> "$registry_media_urls"

# Registry metadata is a public API contract.  Do not sign image
# URLs into an entry until the public artifact endpoint can serve
# every generated icon or screenshot with its real image type.
# This guards against the package upload and public proxy drifting
# apart and avoids publishing Store cards that render as blanks.
check_media() {
  local media=$1 media_url=$2 expected_type headers status=000 content_type= attempt
  case "$media" in
    *.svg) expected_type='image/svg+xml' ;;
    *.png) expected_type='image/png' ;;
    *.jpg|*.jpeg) expected_type='image/jpeg' ;;
    *.webp) expected_type='image/webp' ;;
    *) printf 'unsupported registry media type: %s\n' "$media" >&2; return 1 ;;
  esac
  headers=$(mktemp "$RUNNER_TEMP/tilo-registry-media-headers.XXXXXX")
  for attempt in 1 2 3 4 5; do
    : > "$headers"
    status=$(curl --silent --show-error --location --head --dump-header "$headers" \
      --output /dev/null --write-out '%{http_code}' "$media_url") || status='000'
    content_type=$(tr -d '\r' < "$headers" | awk 'tolower($1) == "content-type:" { print tolower($2) }' | tail -n 1)
    [[ $status == 200 && $content_type == "$expected_type" ]] && break
    (( attempt < 5 )) && sleep "$attempt"
  done
  rm -f "$headers"
  [[ $status == 200 && $content_type == "$expected_type" ]] || {
    printf 'public registry media is unavailable or has the wrong type (%s: HTTP %s, %s; expected %s)\n' \
      "$media_url" "$status" "${content_type:-no Content-Type}" "$expected_type" >&2
    return 1
  }
  local version=${media_url%/*} archived_file actual_sha
  version=${version##*/}
  if [[ $version =~ ^[0-9a-f]{64}$ ]]; then
    archived_file=$(mktemp "$RUNNER_TEMP/tilo-registry-archive.XXXXXX")
    # The proxy fetches an archived screenshot from its origin on first
    # read and can answer that read with a gateway timeout (HTTP 524) while
    # the same URL serves on the next request. That single answer used to
    # fail publication after the media uploads had already landed. Retry
    # transient answers (timeouts, 5xx such as 502-504 and 524), as the HEAD
    # check above does. A wrong digest or a persistent error still fails.
    if ! curl --fail --silent --show-error --max-filesize 33554432 \
      --connect-timeout 10 --max-time 180 --retry 4 --retry-delay 2 \
      "$media_url" --output "$archived_file"; then
      rm -f -- "$archived_file"
      return 1
    fi
    actual_sha=$(sha256sum "$archived_file")
    rm -f -- "$archived_file"
    [[ ${actual_sha%% *} == "$version" ]] || {
      printf 'archived registry screenshot SHA-256 mismatch: %s\n' "$media_url" >&2
      return 1
    }
  fi
}
export -f check_media
export RUNNER_TEMP
tr '\t' '\n' < "$registry_media_urls" \
  | xargs -d '\n' -n 2 -P 8 bash -c 'check_media "$1" "$2"' _

# Submit every newly built immutable tapp to the signed registry.
# The request is signed by the same identity as the package; retries
# are idempotent and the registry's collector/publisher jobs perform
# independent verification before advancing the public snapshot.
[[ -n ${TAPP_PUBLISHER_IDENTITY:-} ]] || {
  printf 'TAPP_PUBLISHER_IDENTITY is required for registry publication\n' >&2
  exit 1
}
registry_identity="$RUNNER_TEMP/tilo-registry-publisher.json"
(umask 077; printf '%s' "$TAPP_PUBLISHER_IDENTITY" > "$registry_identity")
trap 'rm -f "$registry_identity"' EXIT
export SOURCE_DATE_EPOCH
SOURCE_DATE_EPOCH=${TILO_SOURCE_EPOCH:-$(git log -1 --format=%ct)}
snapshot="$RUNNER_TEMP/tilo-registry-snapshot.json"
response="$RUNNER_TEMP/tilo-registry-response.txt"
publication_plan="$RUNNER_TEMP/tilo-registry-publication.tsv"
: "${TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY:?existing registry owner pin required}"
: > "$publication_plan"
for index in release/*/index.json; do
  arch=$(jq -er '.architecture | select(. == "x86_64" or . == "aarch64")' "$index")
  while IFS=$'\t' read -r id name file; do
    printf '%s\t%s\t%s\t%s\t%s\n' \
      "$id" "$name" "$file" "$arch" "${index%/index.json}" >> "$publication_plan"
  done < <(jq -r '.apps[] | [.id,.name,.file] | @tsv' "$index")
done
LC_ALL=C sort -t $'\t' -k1,1 -k4,4 -o "$publication_plan" "$publication_plan"

# One package id is one publisher-signed registry entry. Fold every
# architecture into that entry before POSTing so the registry never
# has to synthesize or re-sign publisher data. This also upgrades an
# already-published single-architecture entry append-only.
# One snapshot serves every id; only a refused attempt re-reads it.
source tools/apps/registry-runtime-readiness.sh
fetch_snapshot
runtime_readiness_deadline=$((SECONDS + 1200))
while IFS= read -r id; do
  accepted=false
  architectures=$(awk -F $'\t' -v id="$id" '$1 == id {print $4}' \
    "$publication_plan" | paste -sd, -)
  for attempt in 1 2 3; do
    (( attempt == 1 )) || fetch_snapshot
    matches=$(jq --arg id "$id" '[.entries[] | select(.package_id == $id)] | length' "$snapshot")
    previous="$RUNNER_TEMP/tilo-registry-previous.json"
    aggregate="$RUNNER_TEMP/tilo-registry-aggregate.json"
    request="$RUNNER_TEMP/tilo-registry-request.json"
    missing_runtime="$RUNNER_TEMP/tilo-registry-missing-runtime.json"
    rm -f "$previous" "$aggregate" "$request" "$missing_runtime"
    # A request still waiting for review is replaced by this one and
    # must keep its versions, so build on it when there is one.
    "$registry_cli" pending --registry "$registry_url" \
      --identity "$registry_identity" "$id" > "$previous"
    [[ -s $previous ]] || rm -f "$previous"
    case "$matches" in
      0) action=add ;;
      1)
        action=update
        [[ -f $previous ]] || jq --arg id "$id" \
          '.entries[] | select(.package_id == $id)' "$snapshot" > "$previous"
        ;;
      *)
        printf 'registry snapshot contains duplicate package id %s\n' "$id" >&2
        exit 1
        ;;
    esac
    [[ -f $previous ]] && cp "$previous" "$aggregate"
    while IFS=$'\t' read -r planned_id name file arch artifact_root; do
      [[ $planned_id == "$id" ]] || continue
      tapp="$artifact_root/tapp/$file"
      metadata="$registry_metadata/$name.json"
      test -f "$metadata"
      tapp_url="$registry_url/artifacts/$owner/generic/tilo-app-$name/$VERSION/$file"
      args=(entry --arch "$arch" --identity "$registry_identity" \
        --metadata "$metadata" --action "$action" --url "$tapp_url"
        --runtime-catalogue "$snapshot"
        --registry-owner-pubkey "$TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY"
        --missing-runtime-output "$missing_runtime")
      [[ -f $aggregate ]] && args+=(--previous-entry "$aggregate")
      await_registry_runtime_ready "$runtime_readiness_deadline" "$request" "$missing_runtime" \
        "$registry_cli" "${args[@]}" "$tapp"
      jq -e --arg id "$id" '.entry | select(.package_id == $id)' \
        "$request" > "$aggregate.next"
      mv "$aggregate.next" "$aggregate"
    done < "$publication_plan"
    [[ -s $request && -s $aggregate ]] || {
      printf 'no registry artifacts planned for %s\n' "$id" >&2
      exit 1
    }
    if [[ -f $previous ]] && jq -e -s '.[0] == .[1]' \
      "$previous" "$aggregate" >/dev/null; then
      accepted=true
      echo "::notice::$id ($architectures) is already current in the Tilo registry"
      break
    fi
    status=$(curl --silent --show-error --connect-timeout 10 --max-time 30 \
      --retry 3 --retry-all-errors \
      --output "$response" --write-out '%{http_code}' --request POST \
      --header 'Content-Type: application/json' --data-binary "@$request" \
      "$registry_url/entries")
    if [[ $status == 202 ]]; then
      accepted=true
      echo "::notice::submitted $id ($architectures) to the Tilo registry"
      break
    fi
    # A concurrent publication can make the fetched predecessor
    # stale. Re-read and regenerate the signed aggregate before the
    # next bounded attempt; all other failures remain visible.
    printf 'registry answered %s with HTTP %s: %s\n' \
      "$id" "$status" "$(head -c 1000 "$response")" >&2
    [[ $status == 409 || $status == 429 || $status == 5?? ]] || exit 1
    sleep "$attempt"
  done
  [[ $accepted == true ]] || {
    printf 'registry did not accept %s after bounded retries\n' "$id" >&2
    exit 1
  }
done < <(cut -f1 "$publication_plan" | uniq)

# Offer the runtime build every app above links, as `so.tilo.runtime`: its
# thin package beside its content-addressed payload objects, which devices
# fetch through the registry before an app that names the build. A build the
# registry already offers is not submitted again. Runs last so a refusal here
# fails the job loudly without holding back the apps or the collections;
# until it is offered, devices refuse those apps with a clear message rather
# than installing apps they cannot start.
runtime_failed=0
publish_runtimes() {
  local index metadata
  metadata="$RUNNER_TEMP/tilo-runtime-metadata.json"
  jq -n '{name: {en: "Tilo runtime"},
          description: {en: "The shared libraries Tilo apps are built against. The Store installs the build an app needs together with the app."},
          tagline: {en: "Runtime for Tilo apps"},
          supported_languages: ["en"],
          source_url: "https://code.tilo.so/tilo/tilo"}' > "$metadata"
  for index in release/*/index.json; do
    RUNTIME_PACKAGE_STAGE=$([[ $stage == all ]] && echo all || echo submit) \
      VERSION=$VERSION TILO_PACKAGE_TOKEN=$jwt PACKAGE_API=$package_api \
      REGISTRY_URL=$registry_url OWNER=$owner REGISTRY_CLI=$registry_cli \
      REGISTRY_IDENTITY=$registry_identity \
      tools/apps/publish-runtime-package.sh "${index%/index.json}/runtime-package" \
      "$(jq -er '.architecture | select(. == "x86_64" or . == "aarch64")' "$index")" \
      so.tilo.runtime tilo-app-runtime "$metadata" \
      || runtime_failed=1
  done
  (( runtime_failed == 0 )) || {
    printf 'the runtime build the published apps link is not offered by the registry\n' >&2
    return 1
  }
}

# Offer every language other than English as its language pack, which
# Settings installs when a person chooses the language. A pack is data for
# every architecture, so the native build's packs are published once, with
# the architecture `any`. Like the runtime, a refusal fails the job without
# holding back the apps.
publish_language_packs() {
  local packs=${registry_cli%/tilo-app-package}/language-packs
  [[ -f $packs/index.json ]] || { printf 'no signed language packs in %s\n' "$packs" >&2; return 1; }
  local uploads="$RUNNER_TEMP/tilo-language-pack-uploads.tsv" failed=0
  : > "$uploads"
  while IFS=$'\t' read -r id file listing; do
    "$registry_cli" compress -o "$twins/$file.zst" "$packs/$file"
    printf '%s\t%s\n' "$packs/$file" "$package_api/tilo-language-packs/$VERSION/$file" >> "$uploads"
    printf '%s\t%s\n' "$twins/$file.zst" \
      "$package_api/tilo-language-packs/$VERSION/$file.zst" >> "$uploads"
  done < <(jq -r '.packs[] | [.id,.file,.metadata] | @tsv' "$packs/index.json")
  if [[ $stage != submit ]]; then
    TILO_PACKAGE_TOKEN="$jwt" TILO_PACKAGE_UPLOAD_WORKERS=8 \
      tools/upload_immutable_packages.sh "$uploads"
  fi
  [[ $stage != upload ]] || return 0
  while IFS=$'\t' read -r id file listing; do
    local accepted=false attempt previous request action
    for attempt in 1 2 3; do
      fetch_snapshot
      previous="$RUNNER_TEMP/tilo-language-pack-previous.json"
      request="$RUNNER_TEMP/tilo-language-pack-request.json"
      rm -f "$previous" "$request"
      "$registry_cli" pending --registry "$registry_url" \
        --identity "$registry_identity" "$id" > "$previous"
      if jq -e --arg id "$id" '[.entries[] | select(.package_id == $id)] | length > 0' \
          "$snapshot" >/dev/null; then
        action=update
        [[ -s $previous ]] || jq --arg id "$id" \
          '.entries[] | select(.package_id == $id)' "$snapshot" > "$previous"
      else
        action=add
      fi
      local args=(entry --arch any --identity "$registry_identity" \
        --metadata "$packs/$listing" --action "$action" --keep-releases 3 \
        --url "$registry_url/artifacts/$owner/generic/tilo-language-packs/$VERSION/$file")
      [[ -s $previous ]] && args+=(--previous-entry "$previous")
      "$registry_cli" "${args[@]}" "$packs/$file" > "$request"
      status=$(curl --silent --show-error --connect-timeout 10 --max-time 30 \
        --retry 3 --retry-all-errors \
        --output "$response" --write-out '%{http_code}' --request POST \
        --header 'Content-Type: application/json' --data-binary "@$request" \
        "$registry_url/entries")
      if [[ $status == 202 ]]; then
        accepted=true
        echo "::notice::submitted $id to the Tilo registry"
        break
      fi
      printf 'registry answered %s with HTTP %s: %s\n' \
        "$id" "$status" "$(head -c 1000 "$response")" >&2
      [[ $status == 409 || $status == 429 || $status == 5?? ]] || break
      sleep "$attempt"
    done
    [[ $accepted == true ]] || failed=1
  done < <(jq -r '.packs[] | [.id,.file,.metadata] | @tsv' "$packs/index.json")
  (( failed == 0 )) || {
    printf 'the registry did not accept every language pack\n' >&2
    return 1
  }
}

offered=0
publish_runtimes || offered=1
publish_language_packs || offered=1
exit "$offered"
