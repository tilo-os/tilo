#!/bin/sh
# The site's text-like files precompressed beside each, as brotli and zstd,
# which Caddy serves by Content-Encoding to browsers that accept them (the
# Caddyfile's `precompressed`); fonts and images are compressed already. The
# image's compress stage runs it, and so does a run of the web entry outside
# the image (os/containers/tilo-sh/stack.sh). Needs brotli, zstd, findutils.
# usage: compress.sh SITE [fast]
# `fast` compresses at brotli 6 and zstd 9 instead of 11 and 19: the local
# stack's, which re-lays the site for every commit. The start module alone
# takes about a minute at 11 and under two seconds at 6, for files about a
# fifth larger, which a page on this machine's loopback does not notice.
set -eu
site=${1:?usage: compress.sh SITE [fast]}
brotli=11 zstd=19
if [ "${2:-}" = fast ]; then brotli=6 zstd=9; fi
export brotli zstd
find "$site" -type f \( -name '*.wasm' -o -name '*.mjs' -o -name '*.js' -o -name '*.json' -o -name '*.html' -o -name '*.css' -o -name '*.svg' -o -name '*.txt' -o -name '*.md' -o -name '*.ttf' -o -name '*.otf' \) -size +1k -print0 \
  | xargs -0 -r -n 1 -P "$(nproc)" sh -c 'brotli -q "$brotli" -k -f "$1" && zstd -q "-$zstd" -k -f "$1"' compress
