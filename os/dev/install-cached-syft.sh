#!/usr/bin/env bash
# Install Syft from the persistent, checksum-verified external-input cache.
# The archive itself is content-pinned in github-assets.tsv; extracting one
# explicitly named member avoids trusting archive paths or an installer action.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd -- "$script_dir/../.." && pwd)"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
: "${GITHUB_PATH:?GITHUB_PATH is required}"

case "$(uname -m)" in
  x86_64|amd64) asset=syft-1.51.0-linux-amd64.tar.gz ;;
  aarch64|arm64) asset=syft-1.51.0-linux-arm64.tar.gz ;;
  *)
    printf 'unsupported Syft host architecture: %s\n' "$(uname -m)" >&2
    exit 1
    ;;
esac

cache_dir="${QOS_GITHUB_ASSET_CACHE:-$repo/os/external-assets/cache}"
QOS_GITHUB_ASSET_CACHE="$cache_dir" "$script_dir/cache-github-assets.sh" "$asset"

tool_dir="$RUNNER_TEMP/qos-syft-tools"
mkdir -p "$tool_dir"
umask 077
tar --extract --gzip --to-stdout --file "$cache_dir/$asset" syft > "$tool_dir/syft"
chmod 0755 "$tool_dir/syft"
printf '%s\n' "$tool_dir" >> "$GITHUB_PATH"
"$tool_dir/syft" version
