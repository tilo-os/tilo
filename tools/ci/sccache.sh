#!/usr/bin/env bash
# Configure and report the optional remote Rust compiler cache.
#
# The builder image carries sccache, but CI must continue when an updated image
# is rolling out or the cache service is unavailable. This script controls the
# fail-open behavior.
set -euo pipefail

mode="${1:?usage: tools/ci/sccache.sh <prepare|prepare-shell|report>}"

case "$mode" in
  prepare|prepare-shell)
    if path="$(command -v sccache 2>/dev/null)"; then
      # libspa-sys generates C wrappers around PipeWire's static inline
      # functions.  sccache's direct/preprocessor mode can drop declarations
      # from that generated translation unit (observed with Fedora 44's GCC
      # 16), while the identical compiler invocation succeeds uncached.  Keep
      # caching enabled, but use the compiler-driven mode that preserves the
      # real preprocessor semantics for native build scripts as well as rustc.
      export SCCACHE_DIRECT=false
      # Forgejo runner-level environment variables intentionally provide the
      # production defaults, and take precedence over workflow `env` entries.
      # A canary needs a fresh namespace, though, so carry its request under a
      # Tilo-specific name and make it authoritative after runner injection.
      # Restarting matters: sccache reads its backend configuration when its
      # long-lived server starts, and `--zero-stats` below may start it.
      if [ -n "${TILO_SCCACHE_KEY_PREFIX:-}" ]; then
        export SCCACHE_S3_KEY_PREFIX="$TILO_SCCACHE_KEY_PREFIX"
        sccache --stop-server >/dev/null 2>&1 || true
      fi
      compiler="${RUSTC:-}"
      if [ -z "$compiler" ]; then
        compiler="$(command -v rustc 2>/dev/null || true)"
      fi
      # `--zero-stats` proves only that the local server can start. Some
      # backends authenticate lazily on the first compiler request, so probe
      # the exact wrapper path Cargo will use before advertising the cache.
      if sccache --zero-stats \
        && [ -n "$compiler" ] \
        && "$path" "$compiler" -vV >/dev/null; then
        if [ "$mode" = prepare ] && [ -n "${GITHUB_ENV:-}" ]; then
          printf 'RUSTC_WRAPPER=%s\n' "$path" >> "$GITHUB_ENV"
          printf 'SCCACHE_DIRECT=false\n' >> "$GITHUB_ENV"
          printf 'CARGO_INCREMENTAL=%s\n' "${CARGO_INCREMENTAL:-0}" >> "$GITHUB_ENV"
          if [ -n "${TILO_SCCACHE_KEY_PREFIX:-}" ]; then
            printf 'SCCACHE_S3_KEY_PREFIX=%s\n' "$TILO_SCCACHE_KEY_PREFIX" >> "$GITHUB_ENV"
          fi
        else
          export RUSTC_WRAPPER="$path"
          export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
        fi
        echo "remote compiler cache enabled: $($path --version)"
      else
        # A present client is not a usable cache when its backend is down.
        # Clearing the wrapper makes cache outages a performance degradation,
        # not an immediate failure of every rustc invocation in the job.
        sccache --stop-server >/dev/null 2>&1 || true
        if [ "$mode" = prepare ] && [ -n "${GITHUB_ENV:-}" ]; then
          echo 'RUSTC_WRAPPER=' >> "$GITHUB_ENV"
        else
          unset RUSTC_WRAPPER || true
        fi
        echo '::warning::sccache wrapper or backend is unavailable; compiling normally'
      fi
    else
      # An old builder image may briefly overlap its replacement. Never leave
      # the workflow-level wrapper pointing at a nonexistent executable.
      if [ "$mode" = prepare ] && [ -n "${GITHUB_ENV:-}" ]; then
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
