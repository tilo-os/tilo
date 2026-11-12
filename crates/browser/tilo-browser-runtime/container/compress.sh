#!/bin/sh
# The site's text-like files precompressed beside each, as brotli and zstd,
# which Caddy serves by Content-Encoding to browsers that accept them (the
# Caddyfile's `precompressed`); fonts and images are compressed already. The
# image's compress stage runs it, and so does a run of the web entry outside
# the image (os/containers/tilo-sh/stack.sh). Needs brotli, zstd, findutils.
# usage: compress.sh SITE
set -eu
site=${1:?usage: compress.sh SITE}
find "$site" -type f \( -name '*.wasm' -o -name '*.mjs' -o -name '*.js' -o -name '*.json' -o -name '*.html' -o -name '*.css' -o -name '*.svg' -o -name '*.txt' -o -name '*.ttf' -o -name '*.otf' \) -size +1k -print0 \
  | xargs -0 -r -n 1 -P "$(nproc)" sh -c 'brotli -q 11 -k -f "$1" && zstd -q -19 -k -f "$1"' compress
