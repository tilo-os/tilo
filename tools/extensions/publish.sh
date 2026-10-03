#!/usr/bin/env bash
# Sign and publish one resolved system-extension manifest.
#
#   tools/extensions/publish.sh MANIFEST
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
# The revision files are immutable history; manifest.json is the newest. Generic
# package files cannot be overwritten, so the newest pair is deleted and
# uploaded again. A device that reads between the two sees a missing file or a
# signature that does not match, refuses it and tries again later.
#
# Nothing is published when the current manifest.json carries a valid
# signature and pins exactly the same base and artifacts (resolve.py same):
# re-resolving every day must not mint revisions that change nothing.
#
# Environment:
#   COSIGN_KEY_REF, AWS_SIGNING_ROLE_ARN,  the extension-manifest key, its role
#   TILO_SIGNING_AUDIENCE                  and OIDC audience (see
#                                          os/dev/configure-cosign-kms.sh)
#   TILO_PACKAGE_TOKEN                     generic package write access
#   TILO_PACKAGE_PUBLIC_BASE_URL           https://<forge>/api/packages/<owner>/generic
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RESOLVE="$REPO/tools/extensions/resolve.py"
PUBLIC_KEY="$REPO/keys/extensions.pub"

die() { printf '::error::%s\n' "$*" >&2; exit 1; }
note() { printf '::notice::%s\n' "$*"; }

manifest=${1:?usage: publish.sh MANIFEST}
[[ -s $manifest ]] || die "no manifest at $manifest"
: "${TILO_PACKAGE_TOKEN:?TILO_PACKAGE_TOKEN is required}"
package_api=${TILO_PACKAGE_PUBLIC_BASE_URL:?TILO_PACKAGE_PUBLIC_BASE_URL is required}
package_api=${package_api%/}
[[ $package_api == https://*/api/packages/*/generic && $package_api != *'.svc.cluster.local'* ]] \
  || die "TILO_PACKAGE_PUBLIC_BASE_URL must be a public HTTPS generic-package base"

checked=$(python3 "$RESOLVE" check "$manifest") || die "$manifest is not a publishable manifest"
read -r id arch fingerprint revision <<< "$checked"
package="$package_api/tilo-extension-$id/$arch-$fingerprint"

work=$(mktemp -d "${RUNNER_TEMP:-/var/tmp}/tilo-extension-publish.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
auth=(--header "Authorization: Bearer $TILO_PACKAGE_TOKEN")

fetch() {  # fetch URL FILE -> HTTP status
  curl --silent --show-error --retry 3 --retry-all-errors "${auth[@]}" \
    --output "$2" --write-out '%{http_code}' "$1"
}

verify() {  # verify FILE SIGNATURE
  cosign verify-blob --key "$PUBLIC_KEY" --insecure-ignore-tlog=true --signature "$2" "$1" >/dev/null 2>&1
}

# ---- 1. the newest publication for this base ---------------------------------
status=$(fetch "$package/manifest.json" "$work/latest.json")
case "$status" in
  200)
    if [[ $(fetch "$package/manifest.json.sig" "$work/latest.json.sig") == 200 ]] \
        && verify "$work/latest.json" "$work/latest.json.sig"; then
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

# ---- 2. sign, and check the signature verifies as a device will ----------------
cp -- "$manifest" "$work/manifest.json"
(
  # shellcheck source=os/dev/configure-cosign-kms.sh
  . "$REPO/os/dev/configure-cosign-kms.sh"
  cosign sign-blob --yes --tlog-upload=false --key "$COSIGN_KEY_REF" \
    --output-signature "$work/manifest.json.sig" "$work/manifest.json"
)
verify "$work/manifest.json" "$work/manifest.json.sig" \
  || die "the new signature does not verify against keys/extensions.pub"
cp -- "$work/manifest.json" "$work/manifest-r$revision.json"
cp -- "$work/manifest.json.sig" "$work/manifest-r$revision.json.sig"

# ---- 3. the immutable revision -------------------------------------------------
uploads="$work/uploads.tsv"
printf '%s\t%s\n' \
  "$work/manifest-r$revision.json" "$package/manifest-r$revision.json" \
  "$work/manifest-r$revision.json.sig" "$package/manifest-r$revision.json.sig" > "$uploads"
TILO_PACKAGE_UPLOAD_WORKERS=1 "$REPO/tools/upload_immutable_packages.sh" "$uploads"

# ---- 4. the newest pair --------------------------------------------------------
for file in manifest.json.sig manifest.json; do
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
printf '%s\t%s\n' \
  "$work/manifest.json.sig" "$package/manifest.json.sig" > "$uploads"
"$REPO/tools/upload_immutable_packages.sh" "$uploads"

# ---- 5. read back what a device will read ---------------------------------------
[[ $(fetch "$package/manifest.json" "$work/check.json") == 200 ]] || die "manifest.json is not readable after upload"
[[ $(fetch "$package/manifest.json.sig" "$work/check.json.sig") == 200 ]] || die "manifest.json.sig is not readable after upload"
cmp --silent "$work/check.json" "$work/manifest.json" || die "the registry serves other manifest.json bytes"
verify "$work/check.json" "$work/check.json.sig" || die "the published pair does not verify"
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
