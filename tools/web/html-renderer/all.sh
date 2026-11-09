#!/usr/bin/env bash
# Build the bundle, then measure it.
set -euo pipefail
ATTRIBUTE=0 tools/web/html-renderer/build-notes-web.sh
tools/web/html-renderer/run-measure.sh "$@"
