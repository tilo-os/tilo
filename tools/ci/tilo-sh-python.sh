#!/usr/bin/env bash
# Prepare the local stack's Python SDK dependencies while t's setup has egress.
# Its offline flows import the SDK from the checkout using this interpreter.
set -euo pipefail
uv_tools=$(mktemp -d)
trap 'rm -rf "$uv_tools"' EXIT
python3 -m pip install --target "$uv_tools" --no-cache-dir --only-binary=:all: "uv==0.13.7"
UV_PROJECT_ENVIRONMENT=/opt/tilo-sdk/python-venv UV_PYTHON_DOWNLOADS=never \
  "$uv_tools/bin/uv" sync --no-cache --frozen --no-dev --project sdk/python --python /usr/bin/python3
