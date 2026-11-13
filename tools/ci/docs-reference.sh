#!/usr/bin/env bash
# The reference pages written from the code they document: the REST API, the
# Python SDK, the `tilo` command and the tools an agent gets in a workspace.
#
# Each generator reads the tree and writes one Markdown page, so a page says
# what the code says now and a public item with no doc text fails the run
# rather than appearing as a bare name. `tools/docs/reference/cli.py` builds and
# runs the command itself; `mcp_tools.py` asks the runtime for the tool list it
# serves.
#
# The generators import `page.py` beside them, which `python3 -I` does not put
# on sys.path: they add their own directory, which is why these run as files of
# that directory rather than as installed tools.
#
# usage: docs-reference.sh [output directory]
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
out=${1:-/target/docs/generated}
mkdir -p "$out"
python3 -I "$root/tools/docs/reference/openapi.py" --root "$root" --spec "$root/docs/api/openapi.json" --out "$out/rest-api.md"
python3 -I "$root/tools/docs/reference/python_sdk.py" --root "$root" --out "$out/python-sdk.md"
python3 -I "$root/tools/docs/reference/cli.py" --root "$root" --out "$out/cli.md"
python3 -I "$root/tools/docs/reference/mcp_tools.py" --root "$root" --out "$out/mcp-tools.md"
