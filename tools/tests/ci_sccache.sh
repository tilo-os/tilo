#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"

cat > "$work/bin/sccache" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  --zero-stats) exit "${FAKE_SCCACHE_STATUS:-0}" ;;
  --stop-server) exit 0 ;;
  --version) echo 'sccache 0.test' ;;
  --show-stats) exit 0 ;;
  *) printf '%s\n' "${RUSTUP_AUTO_INSTALL-unset}" > "$FAKE_SCCACHE_PROBE_ENV"
     exit "${FAKE_SCCACHE_PROBE_STATUS:-0}" ;;
esac
EOF
chmod +x "$work/bin/sccache"

export PATH="$work/bin:$PATH"
# CI jobs carry the runner's wrapper; these cases exercise the image client.
unset RUSTC_WRAPPER
export TILO_SCCACHE_KEY_PREFIX=tilo/test
export GITHUB_ENV="$work/github-env"
export RUSTC=/fixture/rustc
unset CARGO_INCREMENTAL
export FAKE_SCCACHE_PROBE_ENV="$work/probe-env"
unset RUSTUP_AUTO_INSTALL

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
! grep -Fq 'SCCACHE_GHA_VERSION=' "$GITHUB_ENV"
! grep -Fq 'RUSTUP_AUTO_INSTALL=' "$GITHUB_ENV"
grep -Fq 'sccache wrapper or backend is unavailable; compiling normally' \
  "$work/bad-credentials.log"

: > "$GITHUB_ENV"
FAKE_SCCACHE_STATUS=1 FAKE_SCCACHE_PROBE_STATUS=0 \
  bash "$repo/tools/ci/sccache.sh" prepare > "$work/down.log" 2>&1
grep -Fxq 'RUSTC_WRAPPER=' "$GITHUB_ENV"
! grep -Fq 'SCCACHE_GHA_VERSION=' "$GITHUB_ENV"
grep -Fq 'sccache wrapper or backend is unavailable; compiling normally' "$work/down.log"

FAKE_SCCACHE_STATUS=0 FAKE_SCCACHE_PROBE_STATUS=0 \
  bash -c '
    unset GITHUB_ENV RUSTC_WRAPPER
    source "$1/tools/ci/sccache.sh" prepare-shell
    test "$RUSTC_WRAPPER" = "$2/bin/sccache"
    test "$RUSTUP_AUTO_INSTALL" = 0
  ' _ "$repo" "$work" > "$work/shell.log"
grep -Fq 'remote compiler cache enabled: sccache 0.test' "$work/shell.log"
! grep -Fq 'CARGO_INCREMENTAL=' "$GITHUB_ENV"

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
