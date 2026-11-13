#!/usr/bin/env bash
# Build the desktop bundle, then run the sign-in fields and every app's parity.
set -euo pipefail
tools/web/html-renderer/build-desktop-web.sh
tools/web/html-renderer/run-parity.sh "$@"
