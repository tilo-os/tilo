#!/usr/bin/env bash
# The Python SDK's suite, run from its lock in the ci-sdk image without network.
#
# The image carries uv and a virtual environment synced from sdk/python/uv.lock; this installs
# a copy of the tree's project, with the apps' types the sdk-gen check wrote (sdk-gen.sh), into
# that environment and runs pytest over the tree's tests. The environment and the project are
# copies, since a job runs in the image and the tree read-only.
#
# usage: sdk-python.sh [pytest arguments]
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=${TMPDIR:-/tmp}
work=$(mktemp -d "$scratch/sdk-python.XXXXXX")
trap 'rm -rf "$work"' EXIT
tar -C "$root/sdk" --exclude=.venv --exclude=__pycache__ -cf - python | tar -C "$work" -xf -
bash "$root/tools/ci/sdk-gen.sh" place "$work"
cd "$work/python"
export UV_OFFLINE=1
export UV_PYTHON_DOWNLOADS=never
export UV_CACHE_DIR=$scratch/uv-cache
export UV_PROJECT_ENVIRONMENT=$scratch/python-venv
cp -a /opt/tilo-sdk/uv-cache "$UV_CACHE_DIR"
cp -a /opt/tilo-sdk/python-venv "$UV_PROJECT_ENVIRONMENT"
uv run --frozen --offline --extra dev python -m pytest -q -p no:cacheprovider "$@"
