#!/usr/bin/env bash
# Install the reviewed AWS CLI v2 bundle from the checksum-verified external
# input cache. The promotion workflow uses this only for its OIDC-bound ECR
# replication; no runner-global AWS package or credential state is changed.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd -- "$script_dir/../.." && pwd)"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
: "${GITHUB_PATH:?GITHUB_PATH is required}"

case "$(uname -m)" in
  x86_64|amd64) asset=awscli-v2.27.41-linux-x86_64.zip ;;
  aarch64|arm64) asset=awscli-v2.27.41-linux-aarch64.zip ;;
  *)
    printf 'unsupported AWS CLI host architecture: %s\n' "$(uname -m)" >&2
    exit 1
    ;;
esac

cache_dir="${TILO_GITHUB_ASSET_CACHE:-$repo/os/external-assets/cache}"
TILO_GITHUB_ASSET_CACHE="$cache_dir" "$script_dir/cache-github-assets.sh" "$asset"

tool_dir="$(mktemp -d "$RUNNER_TEMP/tilo-awscli-tools.XXXXXX")"
extract_dir="$tool_dir/extract"
install_dir="$tool_dir/aws-cli"
bin_dir="$tool_dir/bin"
cleanup_extract() {
  rm -rf -- "$extract_dir"
}
trap cleanup_extract EXIT
mkdir -p "$extract_dir" "$bin_dir"
unzip -q "$cache_dir/$asset" -d "$extract_dir"
"$extract_dir/aws/install" --install-dir "$install_dir" --bin-dir "$bin_dir"
version="$("$bin_dir/aws" --version 2>&1)"
case "$version" in
  'aws-cli/2.27.41 '*) ;;
  *)
    printf 'AWS CLI bundle reported an unexpected version: %s\n' "$version" >&2
    exit 1
    ;;
esac
printf '%s\n' "$bin_dir" >> "$GITHUB_PATH"
printf '%s\n' "$version"
