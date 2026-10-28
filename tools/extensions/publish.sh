#!/usr/bin/env bash
# Sign and publish one resolved system-extension manifest.
#
#   tools/extensions/publish.sh [--sign-only] MANIFEST
#
# MANIFEST is what tools/extensions/resolve.py wrote for one image. It is
# signed with the extension-manifest key (KMS alias/tilo-extension-manifests,
# public half keys/extensions.pub; not the OS release key), the signature is
# verified against keys/extensions.pub, and both
# go to the generic package registry as
#
#   <packages>/tilo-extension-<id>/<arch>-<fingerprint>/manifest-r<revision>.json[.sig]
#   <packages>/tilo-extension-<id>/<arch>-<fingerprint>/manifest.json[.sig]
#
# Once the release root exists (keys/release/root.crt), the same key signs under
# its operational certificate (keys/operational/extension-manifests.crt) and
# the signature is entered in the transparency log
# (tools/signing/sign-blob-certified.sh): a `.bundle` beside each `.sig` holds
# both, and is what images carrying the root verify. The `.sig` stays for
# images that pin keys/extensions.pub.
#
# The revision files are immutable history; manifest.json is the newest. Generic
# package files cannot be overwritten, so the newest pair is deleted and
# uploaded again. A device that reads between the two sees a missing file or a
# signature that does not match, refuses it and tries again later.
#
# Nothing is published when the current manifest.json carries a valid
# signature and pins exactly the same base and artifacts (resolve.py same):
# re-resolving every day must not mint revisions that change nothing.
#
# --sign-only writes a verified MANIFEST.sig for isolated qualification. It
# requires a reviewed material-content hash, never contacts the package registry
# and never activates an extension. Normal publication may use the same hash to
# reject upstream drift between qualification and promotion.
#
# Environment:
#   TILO_EXPECTED_EXTENSION_CONTENT_SHA256 reviewed resolve.py content-sha256
#   COSIGN_KEY_REF, AWS_SIGNING_ROLE_ARN,  the extension-manifest key, its role
#   TILO_SIGNING_AUDIENCE                  and OIDC audience (see
#                                          os/dev/configure-cosign-kms.sh)
#   TILO_PACKAGE_TOKEN                     generic package write access
#   TILO_PACKAGE_PUBLIC_BASE_URL           https://<forge>/api/packages/<owner>/generic
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RESOLVE="$REPO/tools/extensions/resolve.py"
PUBLIC_KEY="$REPO/keys/extensions.pub"
RELEASE_ROOT="$REPO/keys/release/root.crt"
OPERATIONAL_CERT="$REPO/keys/operational/extension-manifests.crt"

die() { printf '::error::%s\n' "$*" >&2; exit 1; }
note() { printf '::notice::%s\n' "$*"; }

sign_only=false
if [[ ${1:-} == --sign-only ]]; then
  sign_only=true
  shift
fi
manifest=${1:?usage: publish.sh [--sign-only] MANIFEST}
[[ -s $manifest ]] || die "no manifest at $manifest"
qualification=()
if [[ -n ${TILO_EXPECTED_EXTENSION_CONTENT_SHA256:-} ]]; then
  qualification=(--expected-content-sha256 "$TILO_EXPECTED_EXTENSION_CONTENT_SHA256")
fi
if [[ $sign_only == true && ${#qualification[@]} == 0 ]]; then
  die "sign-only qualification requires TILO_EXPECTED_EXTENSION_CONTENT_SHA256"
fi
checked=$(python3 "$RESOLVE" check "$manifest" "${qualification[@]}") || die "$manifest is not a publishable manifest"
read -r id arch fingerprint revision <<< "$checked"
if [[ $sign_only == false ]]; then
  : "${TILO_PACKAGE_TOKEN:?TILO_PACKAGE_TOKEN is required}"
  package_api=${TILO_PACKAGE_PUBLIC_BASE_URL:?TILO_PACKAGE_PUBLIC_BASE_URL is required}
  package_api=${package_api%/}
  [[ $package_api == https://*/api/packages/*/generic && $package_api != *'.svc.cluster.local'* ]] \
    || die "TILO_PACKAGE_PUBLIC_BASE_URL must be a public HTTPS generic-package base"
  package="$package_api/tilo-extension-$id/$arch-$fingerprint"
fi

work=$(mktemp -d "${RUNNER_TEMP:-/var/tmp}/tilo-extension-publish.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
auth=(--header "Authorization: Bearer ${TILO_PACKAGE_TOKEN:-}")

fetch() {  # fetch URL FILE -> HTTP status
  curl --silent --show-error --retry 3 --retry-all-errors "${auth[@]}" \
    --output "$2" --write-out '%{http_code}' "$1"
}

verify() {  # verify FILE SIGNATURE
  cosign verify-blob --key "$PUBLIC_KEY" --insecure-ignore-tlog=true --signature "$2" "$1" >/dev/null 2>&1
}

# The files that authenticate a manifest: the bare signature, and with the
# release root also the bundle images carrying the root verify.
suffixes=(.sig)
certified=false
if [[ -e $RELEASE_ROOT ]]; then
  [[ -s $OPERATIONAL_CERT ]] \
    || die "keys/release/root.crt exists but $OPERATIONAL_CERT does not: run the ceremony (keys/README.md)"
  # shellcheck source=keys/release/trust.env
  . "$REPO/keys/release/trust.env"
  suffixes=(.sig .bundle)
  certified=true
fi

verify_bundle() {  # verify_bundle FILE BUNDLE
  cosign verify-blob --bundle "$2" --certificate-chain "$RELEASE_ROOT" \
    --certificate-identity "extension-manifests@$TILO_RELEASE_IDENTITY_DOMAIN" \
    --certificate-oidc-issuer "$TILO_RELEASE_ISSUER_PREFIX$TILO_RELEASE_EPOCH" \
    --insecure-ignore-sct "$1" >/dev/null 2>&1
}

authentic() {  # authentic FILE: its published signature files verify
  verify "$1" "$1.sig" || return 1
  [[ $certified == false ]] || verify_bundle "$1" "$1.bundle"
}

if [[ $sign_only == false ]]; then
  status=$(fetch "$package/manifest.json" "$work/latest.json")
  case "$status" in
    200)
      signed=true
      for suffix in "${suffixes[@]}"; do
        [[ $(fetch "$package/manifest.json$suffix" "$work/latest.json$suffix") == 200 ]] || signed=false
      done
      if [[ $signed == true ]] && authentic "$work/latest.json"; then
        if python3 "$RESOLVE" same "$work/latest.json" "$manifest"; then
          note "tilo-extension-$id $arch-$fingerprint already publishes these artifacts ($package/manifest.json)"
          exit 0
        fi
        latest_revision=$(python3 "$RESOLVE" check "$work/latest.json" | awk '{print $4}')
        (( revision > latest_revision )) \
          || die "revision $revision is not newer than the published $latest_revision"
      else
        note "the published manifest.json for $arch-$fingerprint has no valid signature; replacing it"
      fi
      ;;
    404) ;;
    3??) die "reading $package/manifest.json was redirected (HTTP $status): the package base moved; update TILO_PACKAGE_PUBLIC_BASE_URL" ;;
    *) die "reading $package/manifest.json answered HTTP $status" ;;
  esac
fi

cp -- "$manifest" "$work/manifest.json"
python3 "$RESOLVE" check "$work/manifest.json" "${qualification[@]}" >/dev/null \
  || die "the copied manifest differs from the reviewed content"
(
  # shellcheck source=os/dev/configure-cosign-kms.sh
  . "$REPO/os/dev/configure-cosign-kms.sh"
  if [[ $certified == true ]]; then
    bash "$REPO/tools/signing/sign-blob-certified.sh" extension-manifests "$OPERATIONAL_CERT" \
      "$work/manifest.json" "$work/manifest.json"
  else
    cosign sign-blob --yes --tlog-upload=false --key "$COSIGN_KEY_REF" \
      --output-signature "$work/manifest.json.sig" "$work/manifest.json"
  fi
)
verify "$work/manifest.json" "$work/manifest.json.sig" \
  || die "the new signature does not verify against keys/extensions.pub"
[[ $certified == false ]] || verify_bundle "$work/manifest.json" "$work/manifest.json.bundle" \
  || die "the new bundle does not verify under keys/release/root.crt"
if [[ $sign_only == true ]]; then
  for suffix in "${suffixes[@]}"; do
    cp -- "$work/manifest.json$suffix" "$manifest$suffix"
  done
  note "signed $manifest for isolated qualification only; no package registry request or activation"
  exit 0
fi
cp -- "$work/manifest.json" "$work/manifest-r$revision.json"
for suffix in "${suffixes[@]}"; do
  cp -- "$work/manifest.json$suffix" "$work/manifest-r$revision.json$suffix"
done

uploads="$work/uploads.tsv"
printf '%s\t%s\n' "$work/manifest-r$revision.json" "$package/manifest-r$revision.json" > "$uploads"
for suffix in "${suffixes[@]}"; do
  printf '%s\t%s\n' "$work/manifest-r$revision.json$suffix" "$package/manifest-r$revision.json$suffix" >> "$uploads"
done
TILO_PACKAGE_UPLOAD_WORKERS=1 "$REPO/tools/upload_immutable_packages.sh" "$uploads"

for file in "${suffixes[@]/#/manifest.json}" manifest.json; do
  status=$(curl --silent --show-error --retry 3 --retry-all-errors "${auth[@]}" \
    --output /dev/null --write-out '%{http_code}' --request DELETE "$package/$file")
  case "$status" in
    200|204|404) ;;
    *) die "deleting $package/$file answered HTTP $status" ;;
  esac
done
printf '%s\t%s\n' \
  "$work/manifest.json" "$package/manifest.json" > "$uploads"
"$REPO/tools/upload_immutable_packages.sh" "$uploads"
for suffix in "${suffixes[@]}"; do
  printf '%s\t%s\n' \
    "$work/manifest.json$suffix" "$package/manifest.json$suffix" > "$uploads"
  "$REPO/tools/upload_immutable_packages.sh" "$uploads"
done

[[ $(fetch "$package/manifest.json" "$work/check.json") == 200 ]] || die "manifest.json is not readable after upload"
for suffix in "${suffixes[@]}"; do
  [[ $(fetch "$package/manifest.json$suffix" "$work/check.json$suffix") == 200 ]] \
    || die "manifest.json$suffix is not readable after upload"
done
cmp --silent "$work/check.json" "$work/manifest.json" || die "the registry serves other manifest.json bytes"
authentic "$work/check.json" || die "the published pair does not verify"
note "published tilo-extension-$id $arch-$fingerprint revision $revision"
if [[ -n ${GITHUB_STEP_SUMMARY:-} ]]; then
  {
    echo "### tilo-extension-$id ($arch) revision $revision"
    echo
    echo "Base \`$fingerprint\`: \`$package/manifest.json\`"
    echo
    echo '| Part | State | Artifacts |'
    echo '| --- | --- | ---: |'
    jq -r '.parts[] | "| \(.name) | \(.state) | \(.artifacts | length) |"' "$work/manifest.json"
  } >> "$GITHUB_STEP_SUMMARY"
fi
