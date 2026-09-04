#!/usr/bin/env bash
# Install the reviewed Cosign binary from the persistent, checksum-verified
# external-input cache. cache-github-assets.sh is the only network fallback and
# resumes interrupted transfers before accepting the exact pinned digest.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd -- "$script_dir/../.." && pwd)"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
: "${GITHUB_PATH:?GITHUB_PATH is required}"

case "$(uname -m)" in
  x86_64|amd64) asset=cosign-v2.5.2-linux-amd64 ;;
  aarch64|arm64) asset=cosign-v2.5.2-linux-arm64 ;;
  *)
    printf 'unsupported Cosign host architecture: %s\n' "$(uname -m)" >&2
    exit 1
    ;;
esac

cache_dir="${QOS_GITHUB_ASSET_CACHE:-$repo/os/external-assets/cache}"
QOS_GITHUB_ASSET_CACHE="$cache_dir" "$script_dir/cache-github-assets.sh" "$asset"

tool_dir="$RUNNER_TEMP/qos-cosign-tools"
mkdir -p "$tool_dir"
install -m 0755 "$cache_dir/$asset" "$tool_dir/cosign"
printf '%s\n' "$tool_dir" >> "$GITHUB_PATH"
"$tool_dir/cosign" version
