#!/bin/sh
# The site's text-like files precompressed beside each, as brotli and zstd,
# which Caddy serves by Content-Encoding to browsers that accept them (the
# Caddyfile's `precompressed`); fonts and images are compressed already. The
# image's compress stage runs it, and so does a run of the web entry outside
# the image (os/containers/tilo-sh/stack.sh). Needs brotli, zstd, findutils.
# usage: compress.sh SITE [fast [CACHE]]
# `fast` compresses at brotli 6 and zstd 9 instead of 11 and 19: the local
# stack's, which re-lays the site for every commit. The start module alone
# takes about a minute at 11 and under two seconds at 6, for files about a
# fifth larger, which a page on this machine's loopback does not notice.
# CACHE (with `fast`) is a directory that keeps each file's compressed forms
# under its content's hash: a site laid out again after one module changed
# compresses that module, and links the rest from there.
set -eu
site=${1:?usage: compress.sh SITE [fast [CACHE]]}
brotli=11 zstd=19 cache=
if [ "${2:-}" = fast ]; then brotli=6 zstd=9 cache=${3:-}; fi
export brotli zstd cache
[ -z "$cache" ] || mkdir -p "$cache"
find "$site" -type f \( -name '*.wasm' -o -name '*.mjs' -o -name '*.js' -o -name '*.json' -o -name '*.html' -o -name '*.css' -o -name '*.svg' -o -name '*.txt' -o -name '*.md' -o -name '*.ttf' -o -name '*.otf' \) -size +1k -print0 \
  | xargs -0 -r -n 1 -P "$(nproc)" sh -c '
    if [ -z "$cache" ]; then brotli -q "$brotli" -k -f "$1" && zstd -q "-$zstd" -k -f "$1"; exit; fi
    hash=$(sha256sum "$1" | cut -c1-64)
    kept=$cache/$hash
    if [ ! -f "$kept.br" ] || [ ! -f "$kept.zst" ]; then
      # Written beside the cache and renamed in: a stopped run never leaves
      # half a file there under a hash.
      brotli -q "$brotli" -c "$1" > "$kept.br.$$" && mv "$kept.br.$$" "$kept.br"
      zstd -q "-$zstd" -c "$1" > "$kept.zst.$$" && mv "$kept.zst.$$" "$kept.zst"
    fi
    ln -f "$kept.br" "$1.br" 2>/dev/null || cp "$kept.br" "$1.br"
    ln -f "$kept.zst" "$1.zst" 2>/dev/null || cp "$kept.zst" "$1.zst"
  ' compress
