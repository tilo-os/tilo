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

tool_dir="$(mktemp -d "$RUNNER_TEMP/tilo-awscli-tools.XXXXXX")"
download="$tool_dir/download"
install_dir="$tool_dir/aws-cli"
bin_dir="$tool_dir/bin"
cleanup_download() {
  rm -rf -- "$download"
}
trap cleanup_download EXIT
mkdir -p "$download" "$bin_dir"
curl --fail --silent --show-error --location --retry 3 \
  "https://awscli.amazonaws.com/awscli-exe-linux-$arch-$version.zip" \
  -o "$download/aws.zip"
printf '%s  %s\n' "$sha256" "$download/aws.zip" | sha256sum --check --status
unzip -q "$download/aws.zip" -d "$download"
"$download/aws/install" --install-dir "$install_dir" --bin-dir "$bin_dir"
reported_version="$("$bin_dir/aws" --version 2>&1)"
case "$reported_version" in
  'aws-cli/2.34.29 '*) ;;
  *)
    printf 'AWS CLI bundle reported an unexpected version: %s\n' "$reported_version" >&2
    exit 1
    ;;
esac
printf '%s\n' "$bin_dir" >> "$GITHUB_PATH"
printf '%s\n' "$reported_version"
