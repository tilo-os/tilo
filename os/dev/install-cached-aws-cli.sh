#!/usr/bin/env bash
# Install the reviewed AWS CLI v2 bundle from the checksum-verified external
# input cache. The runner is amd64; reject other hosts instead of silently
# fetching an unreviewed architecture or falling back to a distro package.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd -- "$script_dir/../.." && pwd)"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
: "${GITHUB_PATH:?GITHUB_PATH is required}"

case "$(uname -m)" in
  x86_64|amd64) asset=awscli-exe-linux-x86_64-2.31.30.zip ;;
  *)
    printf 'unsupported AWS CLI host architecture: %s\n' "$(uname -m)" >&2
    exit 1
    ;;
esac

cache_dir="${TILO_GITHUB_ASSET_CACHE:-$repo/os/external-assets/cache}"
TILO_GITHUB_ASSET_CACHE="$cache_dir" "$script_dir/cache-github-assets.sh" "$asset"

extract_dir="$(mktemp -d "$RUNNER_TEMP/tilo-aws-cli-extract.XXXXXX")"
tool_root="$(mktemp -d "$RUNNER_TEMP/tilo-aws-cli.XXXXXX")"
trap 'rm -rf -- "$extract_dir"' EXIT HUP INT TERM

python3 - "$cache_dir/$asset" "$extract_dir" <<'PY'
import os
from pathlib import Path
import sys
from zipfile import ZipFile

archive = Path(sys.argv[1])
destination = Path(sys.argv[2]).resolve()
with ZipFile(archive) as bundle:
    for member in bundle.infolist():
        target = (destination / member.filename).resolve()
        if target != destination and destination not in target.parents:
            raise ValueError(f"AWS CLI archive escapes extraction root: {member.filename}")
        bundle.extract(member, destination)
        mode = member.external_attr >> 16
        if mode and target.exists():
            os.chmod(target, mode & 0o777)
PY

"$extract_dir/aws/install" \
  --install-dir "$tool_root/aws-cli" \
  --bin-dir "$tool_root/bin"
printf '%s\n' "$tool_root/bin" >> "$GITHUB_PATH"
"$tool_root/bin/aws" --version
