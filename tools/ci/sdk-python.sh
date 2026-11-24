#!/usr/bin/env bash
# The Python SDK's suite, run from its lock in the ci-sdk image without network.
#
# The image carries uv and a virtual environment synced from sdk/python/uv.lock; this installs
# the tree's own copy of the project into that environment and runs pytest over the tree's
# tests. The environment is a copy, since a job runs in the image read-only.
#
# usage: sdk-python.sh [pytest arguments]
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root/sdk/python"
scratch=${TMPDIR:-/tmp}
export UV_OFFLINE=1
export UV_PYTHON_DOWNLOADS=never
export UV_CACHE_DIR=$scratch/uv-cache
export UV_PROJECT_ENVIRONMENT=$scratch/python-venv
cp -a /opt/tilo-sdk/uv-cache "$UV_CACHE_DIR"
cp -a /opt/tilo-sdk/python-venv "$UV_PROJECT_ENVIRONMENT"
# The suite writes nothing beside the sources.
exec uv run --frozen --offline --extra dev python -m pytest -q -p no:cacheprovider "$@"
