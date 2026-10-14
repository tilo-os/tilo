#!/usr/bin/env bash
# Configure and report the optional remote Rust compiler cache.
#
# The builder image carries sccache, but CI must continue when an updated image
# is rolling out or the cache service is unavailable. This script controls the
# fail-open behavior.
set -euo pipefail

mode="${1:?usage: tools/ci/sccache.sh <prepare|prepare-shell|report>}"

# Lanes that exist to prove the cache works (the builder canaries) set
# TILO_SCCACHE_REQUIRED=1: an unusable cache then fails the step with a
# diagnosis instead of degrading to an uncached build.
sccache_unavailable() {
  local reason="$1"
  if [ "${TILO_SCCACHE_REQUIRED:-0}" = 1 ]; then
    echo "::error::$reason; this lane requires the remote compiler cache"
    return 1
  fi
  echo "::warning::$reason; compiling normally"
}

# The client waits about 10 s for a new server, which initialises its storage
# backend before it answers; a slow backend handshake can outlast that once.
# The server keeps starting after the client gives up, so later attempts
# usually find it listening. Bounded: worst case adds about 25 s.
start_sccache_server() {
  local path="$1" attempts="${TILO_SCCACHE_START_ATTEMPTS:-3}" attempt=1
  local delay="${TILO_SCCACHE_START_RETRY_SECONDS:-2}"
  while :; do
    if "$path" --zero-stats; then
      return 0
    fi
    if [ "$attempt" -ge "$attempts" ]; then
      echo "sccache server did not start after $attempts attempts"
      return 1
    fi
    echo "sccache server start attempt $attempt/$attempts failed; retrying in ${delay} s"
    attempt=$((attempt + 1))
    sleep "$delay"
  done
}

# Run one bounded foreground start with debug logging so a persistent failure
# says why (backend unreachable, credentials, port) in the job log.
print_sccache_start_diagnosis() {
  local path="$1"
  if ! command -v timeout >/dev/null 2>&1; then
    echo 'timeout(1) is unavailable; skipping the sccache debug start'
    return 0
  fi
  "$path" --stop-server >/dev/null 2>&1 || true
  echo '::group::sccache debug server start (SCCACHE_LOG=debug SCCACHE_NO_DAEMON=1)'
  SCCACHE_LOG=debug SCCACHE_NO_DAEMON=1 \
    timeout -k 5 "${TILO_SCCACHE_DIAGNOSE_SECONDS:-20}" "$path" --start-server 2>&1 \
    | tail -n 200 || true
  echo '::endgroup::'
  "$path" --stop-server >/dev/null 2>&1 || true
}

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
      server_started=0
      if start_sccache_server "$path"; then
        server_started=1
      fi
      if [ "$server_started" = 1 ] \
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
        if [ "$server_started" = 0 ] && [ "${TILO_SCCACHE_REQUIRED:-0}" = 1 ]; then
          print_sccache_start_diagnosis "$path"
        fi
        "$path" --stop-server >/dev/null 2>&1 || true
        if [ "$mode" = prepare ] && [ -n "${GITHUB_ENV:-}" ]; then
          echo 'RUSTC_WRAPPER=' >> "$GITHUB_ENV"
        else
          unset RUSTC_WRAPPER || true
        fi
        sccache_unavailable 'sccache wrapper or backend is unavailable' \
          || return 1 2>/dev/null || exit 1
      fi
    else
      # An old builder image may briefly overlap its replacement. Never leave
      # the workflow-level wrapper pointing at a nonexistent executable.
      if [ "$mode" = prepare ] && [ -n "${GITHUB_ENV:-}" ]; then
        echo 'RUSTC_WRAPPER=' >> "$GITHUB_ENV"
      else
        unset RUSTC_WRAPPER || true
      fi
      sccache_unavailable 'sccache is absent while the builder image rolls out' \
        || return 1 2>/dev/null || exit 1
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
