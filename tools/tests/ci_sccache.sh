#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"

cat > "$work/bin/sccache" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  --zero-stats)
    if [ -n "${FAKE_SCCACHE_STARTS:-}" ]; then
      echo start >> "$FAKE_SCCACHE_STARTS"
      if [ "$(wc -l < "$FAKE_SCCACHE_STARTS")" -le "${FAKE_SCCACHE_FAILED_STARTS:-0}" ]; then
        echo 'sccache: error: Timed out waiting for server startup.' >&2
        exit 2
      fi
    fi
    exit "${FAKE_SCCACHE_STATUS:-0}" ;;
  --start-server)
    echo "fake debug start log=${SCCACHE_LOG:-} no_daemon=${SCCACHE_NO_DAEMON:-}" >&2
    exit 2 ;;
  --stop-server) exit 0 ;;
  --version) echo 'sccache 0.test' ;;
  --show-stats) exit 0 ;;
  *) printf '%s\n' "${RUSTUP_AUTO_INSTALL-unset}" > "$FAKE_SCCACHE_PROBE_ENV"
     exit "${FAKE_SCCACHE_PROBE_STATUS:-0}" ;;
esac
EOF
chmod +x "$work/bin/sccache"

# `! grep` never trips `set -e`; a negative assertion must exit explicitly.
refute() {
  if grep -Fq "$1" "$2"; then
    echo "unexpected '$1' in $2" >&2
    exit 1
  fi
}

export PATH="$work/bin:$PATH"
# CI jobs carry the runner's wrapper; these cases exercise the image client.
unset RUSTC_WRAPPER
export TILO_SCCACHE_KEY_PREFIX=tilo/test
export GITHUB_ENV="$work/github-env"
export RUSTC=/fixture/rustc
unset CARGO_INCREMENTAL
export FAKE_SCCACHE_PROBE_ENV="$work/probe-env"
unset RUSTUP_AUTO_INSTALL
unset TILO_SCCACHE_REQUIRED
export TILO_SCCACHE_START_RETRY_SECONDS=0

FAKE_SCCACHE_STATUS=0 FAKE_SCCACHE_PROBE_STATUS=0 \
  bash "$repo/tools/ci/sccache.sh" prepare > "$work/healthy.log"
grep -Fxq "RUSTC_WRAPPER=$work/bin/sccache" "$GITHUB_ENV"
grep -Fxq 'SCCACHE_GHA_VERSION=tilo/test' "$GITHUB_ENV"
grep -Fxq 'CARGO_INCREMENTAL=0' "$GITHUB_ENV"
grep -Fq 'remote compiler cache enabled: sccache 0.test' "$work/healthy.log"
# The rustup-proxy check inside the probe, and every later compile, must not
# download a `stable` toolchain the image does not carry.
grep -Fxq '0' "$FAKE_SCCACHE_PROBE_ENV"
grep -Fxq 'RUSTUP_AUTO_INSTALL=0' "$GITHUB_ENV"

: > "$GITHUB_ENV"
FAKE_SCCACHE_STATUS=0 FAKE_SCCACHE_PROBE_STATUS=1 \
  bash "$repo/tools/ci/sccache.sh" prepare > "$work/bad-credentials.log" 2>&1
grep -Fxq 'RUSTC_WRAPPER=' "$GITHUB_ENV"
refute 'SCCACHE_GHA_VERSION=' "$GITHUB_ENV"
refute 'RUSTUP_AUTO_INSTALL=' "$GITHUB_ENV"
grep -Fq 'sccache wrapper or backend is unavailable; compiling normally' \
  "$work/bad-credentials.log"

: > "$GITHUB_ENV"
FAKE_SCCACHE_STATUS=1 FAKE_SCCACHE_PROBE_STATUS=0 \
  bash "$repo/tools/ci/sccache.sh" prepare > "$work/down.log" 2>&1
grep -Fxq 'RUSTC_WRAPPER=' "$GITHUB_ENV"
refute 'SCCACHE_GHA_VERSION=' "$GITHUB_ENV"
grep -Fq 'sccache wrapper or backend is unavailable; compiling normally' "$work/down.log"

# A server that misses the client's startup wait once is retried, not
# treated as an outage.
: > "$GITHUB_ENV"
: > "$work/starts"
FAKE_SCCACHE_STARTS="$work/starts" FAKE_SCCACHE_FAILED_STARTS=1 \
  bash "$repo/tools/ci/sccache.sh" prepare > "$work/slow-start.log" 2>&1
grep -Fxq "RUSTC_WRAPPER=$work/bin/sccache" "$GITHUB_ENV"
grep -Fq 'sccache server start attempt 1/3 failed; retrying' "$work/slow-start.log"
grep -Fq 'remote compiler cache enabled' "$work/slow-start.log"
test "$(wc -l < "$work/starts")" -eq 2

# Retries are bounded; ordinary lanes still fail open afterwards.
: > "$GITHUB_ENV"
: > "$work/starts"
FAKE_SCCACHE_STARTS="$work/starts" FAKE_SCCACHE_FAILED_STARTS=99 \
  bash "$repo/tools/ci/sccache.sh" prepare > "$work/never-starts.log" 2>&1
test "$(wc -l < "$work/starts")" -eq 3
grep -Fxq 'RUSTC_WRAPPER=' "$GITHUB_ENV"
grep -Fq 'sccache server did not start after 3 attempts' "$work/never-starts.log"
grep -Fq 'sccache wrapper or backend is unavailable; compiling normally' \
  "$work/never-starts.log"
refute 'fake debug start log' "$work/never-starts.log"

# A lane that measures the cache fails with a debug start log instead.
: > "$GITHUB_ENV"
: > "$work/starts"
if TILO_SCCACHE_REQUIRED=1 FAKE_SCCACHE_STARTS="$work/starts" FAKE_SCCACHE_FAILED_STARTS=99 \
  bash "$repo/tools/ci/sccache.sh" prepare > "$work/required.log" 2>&1; then
  echo 'required cache lane passed without a cache' >&2
  exit 1
fi
test "$(wc -l < "$work/starts")" -eq 3
grep -Fxq 'RUSTC_WRAPPER=' "$GITHUB_ENV"
grep -Fq '::error::sccache wrapper or backend is unavailable; this lane requires' \
  "$work/required.log"
if command -v timeout >/dev/null 2>&1; then
  grep -Fq 'fake debug start log=debug no_daemon=1' "$work/required.log"
fi

# A required lane with a working cache is unaffected.
: > "$GITHUB_ENV"
TILO_SCCACHE_REQUIRED=1 bash "$repo/tools/ci/sccache.sh" prepare > "$work/required-ok.log"
grep -Fxq "RUSTC_WRAPPER=$work/bin/sccache" "$GITHUB_ENV"

# A required lane also fails when the backend rejects the first compile.
: > "$GITHUB_ENV"
if TILO_SCCACHE_REQUIRED=1 FAKE_SCCACHE_PROBE_STATUS=1 \
  bash "$repo/tools/ci/sccache.sh" prepare > "$work/required-probe.log" 2>&1; then
  echo 'required cache lane passed with a rejected backend' >&2
  exit 1
fi
refute 'fake debug start log' "$work/required-probe.log"

FAKE_SCCACHE_STATUS=0 FAKE_SCCACHE_PROBE_STATUS=0 \
  bash -c '
    unset GITHUB_ENV RUSTC_WRAPPER
    source "$1/tools/ci/sccache.sh" prepare-shell
    test "$RUSTC_WRAPPER" = "$2/bin/sccache"
    test "$RUSTUP_AUTO_INSTALL" = 0
  ' _ "$repo" "$work" > "$work/shell.log"
grep -Fq 'remote compiler cache enabled: sccache 0.test' "$work/shell.log"
refute 'CARGO_INCREMENTAL=' "$GITHUB_ENV"

: > "$GITHUB_ENV"
CARGO_INCREMENTAL=1 FAKE_SCCACHE_STATUS=0 bash "$repo/tools/ci/sccache.sh" prepare > "$work/override.log"
grep -Fxq 'CARGO_INCREMENTAL=1' "$GITHUB_ENV"

# The runner's mounted client wins over one on PATH.
mkdir -p "$work/ci-tools"
cp "$work/bin/sccache" "$work/ci-tools/sccache"
: > "$GITHUB_ENV"
RUSTC_WRAPPER="$work/ci-tools/sccache" FAKE_SCCACHE_STATUS=0 \
  bash "$repo/tools/ci/sccache.sh" prepare > "$work/runner.log"
grep -Fxq "RUSTC_WRAPPER=$work/ci-tools/sccache" "$GITHUB_ENV"

echo 'sccache fail-open contract OK'
