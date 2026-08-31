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
      # Forgejo runner-level environment variables intentionally provide the
      # production defaults, and take precedence over workflow `env` entries.
      # A canary needs a fresh namespace, though, so carry its request under a
      # qOS-specific name and make it authoritative after runner injection.
      # Restarting matters: sccache reads its backend configuration when its
      # long-lived server starts, and `--zero-stats` below may start it.
      if [ -n "${QOS_SCCACHE_KEY_PREFIX:-}" ]; then
        export SCCACHE_S3_KEY_PREFIX="$QOS_SCCACHE_KEY_PREFIX"
        sccache --stop-server >/dev/null 2>&1 || true
      fi
      if [ -n "${GITHUB_ENV:-}" ]; then
        printf 'RUSTC_WRAPPER=%s\n' "$path" >> "$GITHUB_ENV"
        if [ -n "${QOS_SCCACHE_KEY_PREFIX:-}" ]; then
          printf 'SCCACHE_S3_KEY_PREFIX=%s\n' "$QOS_SCCACHE_KEY_PREFIX" >> "$GITHUB_ENV"
        fi
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
