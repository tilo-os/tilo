#!/usr/bin/env bash
set -euo pipefail

test_root=$(mktemp -d /tmp/report-agent-ci-test.XXXXXX)
cleanup() {
  case "$test_root" in
    /tmp/report-agent-ci-test.*) rm -rf -- "$test_root" ;;
  esac
}
trap cleanup EXIT

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
mkdir -p "$test_root/bin"
export CAPTURE_FILE="$test_root/request.json"
cat > "$test_root/bin/curl" <<'SH'
#!/usr/bin/env bash
set -eu
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--data-binary" ]; then
    printf '%s' "$2" > "$CAPTURE_FILE"
    shift 2
  else
    shift
  fi
done
SH
chmod +x "$test_root/bin/curl"

export PATH="$test_root/bin:$PATH"
export OMNIGENT_CI_INGEST_TOKEN=test-token
export OMNIGENT_CI_URL=https://agents.example.test
export GITHUB_REPOSITORY=qos/qos
export GITHUB_SHA=0123456789abcdef0123456789abcdef01234567
export GITHUB_RUN_ID=987
export GITHUB_RUN_ATTEMPT=2
export GITHUB_WORKFLOW=CI
export GITHUB_SERVER_URL=https://code.podesta.ai
export CI_LOG_TAIL='tests failed safely'

"$script_dir/report-agent-ci.sh" failure
jq -e '
  .repository == "qos/qos"
  and .commit_sha == "0123456789abcdef0123456789abcdef01234567"
  and .run_id == "987"
  and .attempt == 2
  and .conclusion == "failure"
  and .status == "completed"
  and .log_tail == "tests failed safely"
' "$CAPTURE_FILE" >/dev/null

export CI_NEEDS_JSON='{"build":{"result":"success"},"publish":{"result":"skipped"}}'
"$script_dir/report-agent-ci.sh"
jq -e '.conclusion == "success"' "$CAPTURE_FILE" >/dev/null

export CI_NEEDS_JSON='{"build":{"result":"failure"},"publish":{"result":"cancelled"}}'
"$script_dir/report-agent-ci.sh"
jq -e '.conclusion == "failure"' "$CAPTURE_FILE" >/dev/null
