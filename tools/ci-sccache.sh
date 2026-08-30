#!/usr/bin/env bash
# Configure and report the optional remote Rust compiler cache.
#
# The builder image carries sccache, but CI deliberately remains correct while
# an updated image is rolling out or the cache service is unavailable. This
# script is the single place that controls that fail-open behaviour.
set -euo pipefail

mode="${1:?usage: ci-sccache.sh <prepare|report>}"

case "$mode" in
  prepare)
    if path="$(command -v sccache 2>/dev/null)"; then
      if [ -n "${GITHUB_ENV:-}" ]; then
        printf 'RUSTC_WRAPPER=%s\n' "$path" >> "$GITHUB_ENV"
      else
        export RUSTC_WRAPPER="$path"
      fi
      sccache --zero-stats || true
      echo "remote compiler cache enabled: $($path --version)"
    else
      # An old builder image may briefly overlap its replacement. Never leave
      # the workflow-level wrapper pointing at a nonexistent executable.
      if [ -n "${GITHUB_ENV:-}" ]; then
        echo 'RUSTC_WRAPPER=' >> "$GITHUB_ENV"
      else
        unset RUSTC_WRAPPER || true
      fi
      echo '::warning::sccache is absent; compiling normally while the builder image rolls out'
    fi
    ;;
  report)
    if command -v sccache >/dev/null 2>&1; then
      sccache --show-stats
    else
      echo 'sccache unavailable; this job used the ordinary compiler'
    fi
    ;;
  *)
    echo "unknown mode: $mode" >&2
    exit 2
    ;;
esac
