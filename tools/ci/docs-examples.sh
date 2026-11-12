#!/usr/bin/env bash
# The developer documentation's code examples, run against a local Tilo.
#
# The examples reach a host the way the published ones reach tilo.sh: the door-on-relay example
# of tilo-agent-participant serves one on loopback and runs each program with TILO_DOOR and
# TILO_API_KEY set. That example and the tilo command sh examples run are built by the
# docs-examples-build check, whose outputs this script is handed.
#
# The Python and TypeScript environments come from the ci-sdk image, offline, as the sdk-python
# and sdk-typescript checks take them: uv runs a Python block from sdk/python, and bun resolves
# `tilo-sdk` to the tree's sdk/typescript, so an example runs against this tree's SDKs.
#
# usage: docs-examples.sh <directory of the docs-examples-build outputs> [run arguments]
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
binaries=${1:?the directory the docs-examples-build check wrote}
shift
scratch=${TMPDIR:-/tmp}
export UV_OFFLINE=1
export UV_PYTHON_DOWNLOADS=never
export UV_CACHE_DIR=$scratch/uv-cache
export UV_PROJECT_ENVIRONMENT=$scratch/python-venv
cp -a /opt/tilo-sdk/uv-cache "$UV_CACHE_DIR"
cp -a /opt/tilo-sdk/python-venv "$UV_PROJECT_ENVIRONMENT"
cd "$root"
exec python3 -I tools/docs/doc_examples.py run \
    --door "$binaries/door-on-relay" \
    --tilo "$binaries/tilo" \
    "$@"
