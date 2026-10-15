#!/usr/bin/env bash
# Build the MoQ library that the browser suites import, from the @moq/net
# pinned in scripts/moq-browser/package-lock.json, as the browser runtime build
# does; it is never committed. Run from the repository root, in a tree the
# check may write (node_modules and the built library land in
# scripts/moq-browser). A stalled registry fetch can fail npm ci after minutes
# of silence, so a failed install is retried from a clean tree.
set -euo pipefail
# ci-fast ships node without npm: without one, use a pinned npm release.
npm_version=10.9.2
npm_sha256=5cd1e5ab971ea6333f910bc2d50700167c5ef4e66da279b2a3efc874c6b116e4
scratch="${TMPDIR:-/tmp}"
export npm_config_cache="$scratch/npm-cache"
if ! command -v npm >/dev/null; then
  curl -fsSL --retry 3 -o "$scratch/npm.tgz" "https://registry.npmjs.org/npm/-/npm-$npm_version.tgz"
  echo "$npm_sha256  $scratch/npm.tgz" | sha256sum -c --quiet
  mkdir -p "$scratch/npm"
  tar -xzf "$scratch/npm.tgz" -C "$scratch/npm"
  npm() { node "$scratch/npm/package/bin/npm-cli.js" "$@"; }
fi
cd scripts/moq-browser
for attempt in 1 2 3; do
  if npm ci --no-audit --no-fund --loglevel=error --fetch-timeout=60000; then
    break
  fi
  if [ "$attempt" -eq 3 ]; then
    echo "npm ci for scripts/moq-browser failed after $attempt attempts" >&2
    exit 1
  fi
  echo "npm ci for scripts/moq-browser failed; retrying ($attempt/3)" >&2
  rm -rf node_modules
  sleep $((attempt * 5))
done
node build.mjs
