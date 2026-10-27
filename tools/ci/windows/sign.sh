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
# references (lib.sh aws_role), with credentials that last minutes, which jsign reads from the
# environment. No key is stored.
. "$repo/tools/ci/publish/lib.sh"
: "${AWS_SIGNING_ROLE_ARN:?the AWS role to sign as}"
[[ "${TILO_AUTHENTICODE_KMS_KEY:-}" =~ ^arn:aws:kms:([a-z0-9-]+): ]] || die 'TILO_AUTHENTICODE_KMS_KEY is not a KMS ARN'
aws_role "$AWS_SIGNING_ROLE_ARN" "${BASH_REMATCH[1]}"

bash "$repo/apps/imager/packaging/package-windows.sh" "$arch" "$build/imager.exe" "$out" \
  "bash $repo/tools/ci/windows/authenticode.sh"
