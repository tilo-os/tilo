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
    # The runner mounts its sccache at RUSTC_WRAPPER; images may carry one.
    path="${RUSTC_WRAPPER:-}"
    [ -x "$path" ] || path="$(command -v sccache 2>/dev/null || true)"
    if [ -n "$path" ]; then
      # libspa-sys generates C wrappers around PipeWire's static inline
      # functions.  sccache's direct/preprocessor mode can drop declarations
      # from that generated translation unit (observed with Fedora 44's GCC
      # 16), while the identical compiler invocation succeeds uncached.  Keep
      # caching enabled, but use the compiler-driven mode that preserves the
      # real preprocessor semantics for native build scripts as well as rustc.
      export SCCACHE_DIRECT=false
      # sccache recognises a rustup proxy by running `rustc +stable`. The
      # images carry only the pinned toolchain, so rustup's default
      # auto-install downloaded all of `stable` into every job before its first
      # compile (9-29 s). Without auto-install that check fails at once and
      # sccache runs the proxy as the compiler: rustup still selects the pinned
      # toolchain, and cache keys (sysroot library digests, arguments, inputs)
      # are unchanged. A job whose pinned toolchain is missing now fails
      # instead of fetching one.
      export RUSTUP_AUTO_INSTALL=0
      # The runner provides the backend (SCCACHE_GHA_ENABLED on the job's
      # Actions cache). A canary or lane can request its own namespace under a
      # Tilo-specific name; sccache's GHA backend keys entries by version.
      # Restarting matters: sccache reads its backend configuration when its
      # long-lived server starts, and `--zero-stats` below may start it.
      if [ -n "${TILO_SCCACHE_KEY_PREFIX:-}" ]; then
        export SCCACHE_GHA_VERSION="$TILO_SCCACHE_KEY_PREFIX"
        "$path" --stop-server >/dev/null 2>&1 || true
      fi
      compiler="${RUSTC:-}"
      if [ -z "$compiler" ]; then
        compiler="$(command -v rustc 2>/dev/null || true)"
      fi
      # `--zero-stats` proves only that the local server can start. Some
      # backends authenticate lazily on the first compiler request, so probe
      # the exact wrapper path Cargo will use before advertising the cache.
      if "$path" --zero-stats \
        && [ -n "$compiler" ] \
        && "$path" "$compiler" -vV >/dev/null; then
        if [ "$mode" = prepare ] && [ -n "${GITHUB_ENV:-}" ]; then
          printf 'RUSTC_WRAPPER=%s\n' "$path" >> "$GITHUB_ENV"
          printf 'SCCACHE_DIRECT=false\n' >> "$GITHUB_ENV"
          printf 'CARGO_INCREMENTAL=%s\n' "${CARGO_INCREMENTAL:-0}" >> "$GITHUB_ENV"
          printf 'RUSTUP_AUTO_INSTALL=0\n' >> "$GITHUB_ENV"
          if [ -n "${TILO_SCCACHE_KEY_PREFIX:-}" ]; then
            printf 'SCCACHE_GHA_VERSION=%s\n' "$TILO_SCCACHE_KEY_PREFIX" >> "$GITHUB_ENV"
          fi
        else
          export RUSTC_WRAPPER="$path"
          export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
        fi
        echo "remote compiler cache enabled: $($path --version) $("$path" --show-stats | grep -m1 'Cache location' || true)"
      else
        # A present client is not a usable cache when its backend is down.
        # Clearing the wrapper makes cache outages a performance degradation,
        # not an immediate failure of every rustc invocation in the job.
        "$path" --stop-server >/dev/null 2>&1 || true
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
