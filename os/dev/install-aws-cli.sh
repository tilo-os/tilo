#!/usr/bin/env bash
# Publication must not depend on the runner distribution shipping awscli.
set -euo pipefail

: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
: "${GITHUB_PATH:?GITHUB_PATH is required}"
version=2.34.29
case "$(uname -m)" in
  x86_64|amd64)
    arch=x86_64
    sha256=8812e303cb4618ec495d39b94e4f338cf37d274007ca89faf587a0bc4792cd0e
    ;;
  aarch64|arm64)
    arch=aarch64
    sha256=d424c1f63258c7e56861966b78b38db68e02f1997e704d515566c590a4cddafa
    ;;
  *) echo "Unsupported AWS CLI architecture: $(uname -m)" >&2; exit 1 ;;
esac

download="$(mktemp -d "$RUNNER_TEMP/aws-cli-download.XXXXXX")"
trap 'rm -rf -- "$download"' EXIT
curl --fail --silent --show-error --location --retry 3 \
  "https://awscli.amazonaws.com/awscli-exe-linux-$arch-$version.zip" \
  -o "$download/aws.zip"
printf '%s  %s\n' "$sha256" "$download/aws.zip" | sha256sum --check --status
unzip -q "$download/aws.zip" -d "$download"
"$download/aws/install" --install-dir "$RUNNER_TEMP/aws-cli" \
  --bin-dir "$RUNNER_TEMP/aws-cli-bin" --update
"$RUNNER_TEMP/aws-cli-bin/aws" --version
printf '%s\n' "$RUNNER_TEMP/aws-cli-bin" >> "$GITHUB_PATH"
