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
  *) exit "${FAKE_SCCACHE_PROBE_STATUS:-0}" ;;
esac
EOF
chmod +x "$work/bin/sccache"

export PATH="$work/bin:$PATH"
export TILO_SCCACHE_KEY_PREFIX=tilo/test
export GITHUB_ENV="$work/github-env"
export RUSTC=/fixture/rustc
unset CARGO_INCREMENTAL

FAKE_SCCACHE_STATUS=0 FAKE_SCCACHE_PROBE_STATUS=0 \
  bash "$repo/tools/ci/sccache.sh" prepare > "$work/healthy.log"
grep -Fxq "RUSTC_WRAPPER=$work/bin/sccache" "$GITHUB_ENV"
grep -Fxq 'SCCACHE_S3_KEY_PREFIX=tilo/test' "$GITHUB_ENV"
grep -Fxq 'CARGO_INCREMENTAL=0' "$GITHUB_ENV"
grep -Fq 'remote compiler cache enabled: sccache 0.test' "$work/healthy.log"

: > "$GITHUB_ENV"
FAKE_SCCACHE_STATUS=0 FAKE_SCCACHE_PROBE_STATUS=1 \
  bash "$repo/tools/ci/sccache.sh" prepare > "$work/bad-credentials.log" 2>&1
grep -Fxq 'RUSTC_WRAPPER=' "$GITHUB_ENV"
! grep -Fq 'SCCACHE_S3_KEY_PREFIX=' "$GITHUB_ENV"
grep -Fq 'sccache wrapper or backend is unavailable; compiling normally' \
  "$work/bad-credentials.log"

: > "$GITHUB_ENV"
FAKE_SCCACHE_STATUS=1 FAKE_SCCACHE_PROBE_STATUS=0 \
  bash "$repo/tools/ci/sccache.sh" prepare > "$work/down.log" 2>&1
grep -Fxq 'RUSTC_WRAPPER=' "$GITHUB_ENV"
! grep -Fq 'SCCACHE_S3_KEY_PREFIX=' "$GITHUB_ENV"
grep -Fq 'sccache wrapper or backend is unavailable; compiling normally' "$work/down.log"

FAKE_SCCACHE_STATUS=0 FAKE_SCCACHE_PROBE_STATUS=0 \
  bash -c '
    unset GITHUB_ENV RUSTC_WRAPPER
    source "$1/tools/ci/sccache.sh" prepare-shell
    test "$RUSTC_WRAPPER" = "$2/bin/sccache"
  ' _ "$repo" "$work" > "$work/shell.log"
grep -Fq 'remote compiler cache enabled: sccache 0.test' "$work/shell.log"
! grep -Fq 'CARGO_INCREMENTAL=' "$GITHUB_ENV"

: > "$GITHUB_ENV"
CARGO_INCREMENTAL=1 FAKE_SCCACHE_STATUS=0 bash "$repo/tools/ci/sccache.sh" prepare > "$work/override.log"
grep -Fxq 'CARGO_INCREMENTAL=1' "$GITHUB_ENV"

echo 'sccache fail-open contract OK'
