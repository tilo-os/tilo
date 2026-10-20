#!/usr/bin/env bash
# The signing half of the Windows imager release: take the executable the
# build check produced and the package the package check made from it, refuse
# anything whose digest does not match, then package that same executable
# again with Authenticode signing (the EXE before it goes into the zip and the
# installer, then the installer itself).
#
#   sign.sh <arch> <build-dir> <package-dir> <out>
#
# Runs only on release tags (t.toml, sign/windows-*);
# the version comes from the tag, imager-vMAJOR.MINOR.PATCH.
set -euo pipefail

arch="${1:?x86_64 or aarch64}"
build="${2:?build output directory}"
package="${3:?package output directory}"
out="${4:?output directory}"
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"

case "${T_REF:-}" in
  tag:imager-v*) TILO_VERSION="${T_REF#tag:imager-v}" ;;
  *) printf 'signing runs for imager release tags only, not %s\n' "${T_REF:-a working tree}" >&2; exit 1 ;;
esac
export TILO_VERSION

# The build's sidecar names the digest it produced; the package must carry
# exactly that executable, unsigned.
(cd "$build" && sha256sum --check --strict imager.exe.sha256)
(cd "$package" && sha256sum --check --strict "tilo-imager-windows-$arch.exe.sha256")
built="$(cut -d ' ' -f 1 "$build/imager.exe.sha256")"
packaged="$(cut -d ' ' -f 1 "$package/tilo-imager-windows-$arch.exe.sha256")"
if [ "$built" != "$packaged" ]; then
  printf 'packaged executable %s is not the build %s\n' "$packaged" "$built" >&2
  exit 1
fi

# Both signatures are made as AWS_SIGNING_ROLE_ARN, whose trust policy admits only release
# references: STS exchanges t's identity token (`id_token = ["sts.amazonaws.com"]`) for
# credentials that last minutes, which jsign reads from the environment. No key is stored.
. "$repo/tools/ci/publish/lib.sh"
: "${AWS_SIGNING_ROLE_ARN:?the AWS role to sign as}"
[[ "${TILO_AUTHENTICODE_KMS_KEY:-}" =~ ^arn:aws:kms:([a-z0-9-]+): ]] || die 'TILO_AUTHENTICODE_KMS_KEY is not a KMS ARN'
region="${BASH_REMATCH[1]}"
sts="$(mktemp -d "$scratch/sts.XXXXXX")"
trap 'rm -rf -- "$sts"' EXIT
(umask 077 && id_token sts.amazonaws.com > "$sts/token")
# The token goes in the request body from its file, never on a command line.
curl --fail-with-body --silent --show-error --proto '=https' "https://sts.$region.amazonaws.com/" \
  --data-urlencode Action=AssumeRoleWithWebIdentity --data-urlencode Version=2011-06-15 \
  --data-urlencode "RoleArn=$AWS_SIGNING_ROLE_ARN" --data-urlencode DurationSeconds=900 \
  --data-urlencode "RoleSessionName=tilo-t-$(printf '%.12s' "${T_COMMIT:-local}")" \
  --data-urlencode "WebIdentityToken@$sts/token" > "$sts/reply.xml" \
  || die "STS refused the identity token: $(sed -n 's:.*<Message>\(.*\)</Message>.*:\1:p' "$sts/reply.xml")"
sts_field() { tr -d '\n' < "$sts/reply.xml" | sed -n "s:.*<$1>\([^<]*\)</$1>.*:\1:p"; }
unset AWS_PROFILE AWS_DEFAULT_PROFILE AWS_SHARED_CREDENTIALS_FILE AWS_SECURITY_TOKEN
AWS_ACCESS_KEY_ID="$(sts_field AccessKeyId)"
AWS_SECRET_ACCESS_KEY="$(sts_field SecretAccessKey)"
AWS_SESSION_TOKEN="$(sts_field SessionToken)"
[[ -n $AWS_ACCESS_KEY_ID && -n $AWS_SECRET_ACCESS_KEY && -n $AWS_SESSION_TOKEN ]] || die 'STS answered without credentials'
export AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
rm -rf -- "$sts"

bash "$repo/apps/imager/packaging/package-windows.sh" "$arch" "$build/imager.exe" "$out" \
  "bash $repo/tools/ci/windows/authenticode.sh"
