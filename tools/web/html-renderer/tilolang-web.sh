#!/usr/bin/env bash
# One command: build a tilolang app for the web on the builder
# (build-tilolang-web.sh through `$CARGO exec`), fetch it, and serve it on this
# machine's loopback, printing the page that shows it on the canvas and as
# HTML side by side. Serving is all that runs here.
#
# usage: tools/web/html-renderer/tilolang-web.sh [APP_DIR [--search DIR]...] [--port N] [--no-build]
#        (default: apps/tl/gallery --search apps/tl, port 8790)
set -euo pipefail
repo=$(cd "$(dirname "$0")/../../.." && pwd)
port=8790
build=1
args=()
while [[ $# -gt 0 ]]; do
  case $1 in
    --port) port=${2:?--port needs a number}; shift 2 ;;
    --no-build) build=0; shift ;;
    *) args+=("$1"); shift ;;
  esac
done
cd "$repo"
if [[ $build == 1 ]]; then
  "${CARGO:-cargo}" exec tools/web/html-renderer/build-tilolang-web.sh "${args[@]}"
fi
target=${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps --offline | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')}
dir=$target/htmlrenderer/tilolang
app=$(ls "$dir/tl" | head -n 1)
echo "http://127.0.0.1:$port/tilolang.html?package=tl/$app/"
# The checks' own server: nothing cached, so a browser that showed an earlier
# build loads all of this one.
exec node tools/web/html-renderer/serve.mjs "$dir" "$port"
