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
  *) exit 2 ;;
esac
EOF
chmod +x "$work/bin/sccache"

export PATH="$work/bin:$PATH"
export QOS_SCCACHE_KEY_PREFIX=qos/test
export GITHUB_ENV="$work/github-env"

FAKE_SCCACHE_STATUS=0 bash "$repo/tools/ci-sccache.sh" prepare > "$work/healthy.log"
grep -Fxq "RUSTC_WRAPPER=$work/bin/sccache" "$GITHUB_ENV"
grep -Fxq 'SCCACHE_S3_KEY_PREFIX=qos/test' "$GITHUB_ENV"
grep -Fq 'remote compiler cache enabled: sccache 0.test' "$work/healthy.log"

: > "$GITHUB_ENV"
FAKE_SCCACHE_STATUS=1 bash "$repo/tools/ci-sccache.sh" prepare > "$work/down.log" 2>&1
grep -Fxq 'RUSTC_WRAPPER=' "$GITHUB_ENV"
! grep -Fq 'SCCACHE_S3_KEY_PREFIX=' "$GITHUB_ENV"
grep -Fq 'sccache backend is unavailable; compiling normally' "$work/down.log"

echo 'sccache fail-open contract OK'
