#!/usr/bin/env bash
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trusted=
cleanup() {
  rm -rf "$tmp"
  [ -z "$trusted" ] || "${root_do[@]}" rm -rf "$trusted"
}
trap cleanup EXIT

if [ "$(id -u)" = 0 ]; then
  root_do=()
elif sudo -n true >/dev/null 2>&1; then
  root_do=(sudo -n)
else
  printf 'skip  ephemeral runner fixtures require root installation\n'
  exit 0
fi

case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:amd64) label=runner ;;
  Linux:aarch64|Linux:arm64) label=runner-arm64 ;;
  Darwin:x86_64|Darwin:amd64) label=macos-x64 ;;
  Darwin:aarch64|Darwin:arm64) label=macos-arm64 ;;
  *) printf 'skip  unsupported test host architecture\n'; exit 0 ;;
esac

trusted="/run/qos-ephemeral-runner-test-$$"
"${root_do[@]}" mkdir -m 0755 "$trusted"
"${root_do[@]}" mkdir -m 0755 "$trusted/bin"
"${root_do[@]}" mkdir -m 0700 "$trusted/metadata"
[ "$(id -u)" = 0 ] || "${root_do[@]}" chown "$(id -u):$(id -g)" "$trusted/metadata"
cat > "$tmp/forgejo-runner" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = --version ]; then
  printf 'forgejo-runner version v%s\n' "${QOS_TEST_VERSION:-12.8.0}"
  exit 0
fi
printf '%s\n' "$@" > "$QOS_TEST_ARGUMENTS"
[ "$(cat "${7#file://}")" = 0123456789abcdef0123456789abcdef01234567 ]
[ -z "${QOS_TEST_SLEEP:-}" ] || sleep "$QOS_TEST_SLEEP"
if [ -n "${QOS_TEST_CHILD_PID_FILE:-}" ]; then
  (trap '' TERM; while :; do sleep 1; done) &
  printf '%s\n' "$!" > "$QOS_TEST_CHILD_PID_FILE"
fi
if [ -n "${QOS_TEST_BREAK_METADATA_CLEANUP:-}" ]; then
  token_path="${7#file://}"
  rm -f "$token_path"
  mkdir "$token_path"
fi
EOF
"${root_do[@]}" install -o root -g root -m 0755 "$tmp/forgejo-runner" "$trusted/bin/forgejo-runner"

install_metadata() {
  printf '%s' '392c9434-6bb9-454b-b9ff-646875cf6691' > "$tmp/uuid"
  printf '%s' '0123456789abcdef0123456789abcdef01234567' > "$tmp/token"
  install -m 0600 "$tmp/uuid" "$trusted/metadata/uuid"
  install -m 0600 "$tmp/token" "$trusted/metadata/token"
}

install_metadata
QOS_FORGEJO_RUNNER="$trusted/bin/forgejo-runner" \
QOS_RUNNER_MAX_WAIT_SECONDS=30 QOS_TEST_ARGUMENTS="$tmp/arguments" \
  "$repo/tools/run-imager-ephemeral-runner.sh" "$label" "$trusted/metadata/uuid" "$trusted/metadata/token" \
  > "$tmp/stdout"
grep -Fxq 'one-job' "$tmp/arguments"
grep -Fxq -- '--label' "$tmp/arguments"
grep -Fxq "$label:host" "$tmp/arguments"
grep -Fxq -- '--wait' "$tmp/arguments"
grep -Fq 'destroy this VM now' "$tmp/stdout"
[ ! -e "$trusted/metadata/uuid" ] && [ ! -e "$trusted/metadata/token" ]

install_metadata
if QOS_FORGEJO_RUNNER="$trusted/bin/forgejo-runner" QOS_RUNNER_MAX_WAIT_SECONDS=30 \
    QOS_TEST_BREAK_METADATA_CLEANUP=1 QOS_TEST_ARGUMENTS="$tmp/cleanup-failure" \
    "$repo/tools/run-imager-ephemeral-runner.sh" "$label" "$trusted/metadata/uuid" "$trusted/metadata/token" \
    >/dev/null 2>&1; then
  printf 'ephemeral runner launcher hid a credential-cleanup failure\n' >&2
  exit 1
fi
rm -f "$trusted/metadata/uuid"
rm -rf "$trusted/metadata/token"

install_metadata
QOS_FORGEJO_RUNNER="$trusted/bin/forgejo-runner" QOS_RUNNER_MAX_WAIT_SECONDS=30 \
QOS_TEST_CHILD_PID_FILE="$tmp/child-pid" QOS_TEST_ARGUMENTS="$tmp/process-tree" \
  "$repo/tools/run-imager-ephemeral-runner.sh" "$label" "$trusted/metadata/uuid" "$trusted/metadata/token" \
  >/dev/null
child_pid="$(cat "$tmp/child-pid")"
child_state="$(ps -o stat= -p "$child_pid" 2>/dev/null || true)"
if [ -n "$child_state" ] && [[ "$child_state" != Z* ]]; then
  printf 'ephemeral runner launcher left a child process alive\n' >&2
  exit 1
fi
[ ! -e "$trusted/metadata/uuid" ] && [ ! -e "$trusted/metadata/token" ]

install_metadata
wrong=runner-arm64
[ "$label" = runner ] || wrong=runner
if QOS_FORGEJO_RUNNER="$trusted/bin/forgejo-runner" QOS_TEST_ARGUMENTS="$tmp/rejected" \
    "$repo/tools/run-imager-ephemeral-runner.sh" "$wrong" "$trusted/metadata/uuid" "$trusted/metadata/token" \
    >/dev/null 2>&1; then
  printf 'ephemeral runner launcher accepted a mismatched native label\n' >&2
  exit 1
fi
[ ! -e "$trusted/metadata/uuid" ] && [ ! -e "$trusted/metadata/token" ]

install_metadata
if QOS_FORGEJO_RUNNER="$trusted/bin/forgejo-runner" \
    QOS_FORGEJO_URL=https://attacker.invalid QOS_TEST_ARGUMENTS="$tmp/rejected" \
    "$repo/tools/run-imager-ephemeral-runner.sh" "$label" "$trusted/metadata/uuid" "$trusted/metadata/token" \
    >/dev/null 2>&1; then
  printf 'ephemeral runner launcher accepted a substituted Forgejo origin\n' >&2
  exit 1
fi
[ ! -e "$trusted/metadata/uuid" ] && [ ! -e "$trusted/metadata/token" ]

install_metadata
if QOS_FORGEJO_RUNNER="$tmp/forgejo-runner" QOS_TEST_ARGUMENTS="$tmp/rejected" \
    "$repo/tools/run-imager-ephemeral-runner.sh" "$label" "$trusted/metadata/uuid" "$trusted/metadata/token" \
    >/dev/null 2>&1; then
  printf 'ephemeral runner launcher accepted a checkout-controlled runner binary\n' >&2
  exit 1
fi
[ ! -e "$trusted/metadata/uuid" ] && [ ! -e "$trusted/metadata/token" ]

install_metadata
printf '%s' bad > "$tmp/token"
install -m 0600 "$tmp/token" "$trusted/metadata/token"
if QOS_FORGEJO_RUNNER="$trusted/bin/forgejo-runner" QOS_TEST_ARGUMENTS="$tmp/rejected" \
    "$repo/tools/run-imager-ephemeral-runner.sh" "$label" "$trusted/metadata/uuid" "$trusted/metadata/token" \
    >/dev/null 2>&1; then
  printf 'ephemeral runner launcher accepted an invalid one-job token\n' >&2
  exit 1
fi
[ ! -e "$trusted/metadata/uuid" ] && [ ! -e "$trusted/metadata/token" ]

install_metadata
printf '\n%s' '0123456789abcdef0123456789abcdef01234567' > "$tmp/token"
install -m 0600 "$tmp/token" "$trusted/metadata/token"
if QOS_FORGEJO_RUNNER="$trusted/bin/forgejo-runner" QOS_TEST_ARGUMENTS="$tmp/rejected" \
    "$repo/tools/run-imager-ephemeral-runner.sh" "$label" "$trusted/metadata/uuid" "$trusted/metadata/token" \
    >/dev/null 2>&1; then
  printf 'ephemeral runner launcher accepted multiline metadata\n' >&2
  exit 1
fi
[ ! -e "$trusted/metadata/uuid" ] && [ ! -e "$trusted/metadata/token" ]

install_metadata
if QOS_FORGEJO_RUNNER="$trusted/bin/forgejo-runner" QOS_RUNNER_MAX_WAIT_SECONDS=1 \
    QOS_TEST_SLEEP=30 QOS_TEST_ARGUMENTS="$tmp/timed-out" \
    "$repo/tools/run-imager-ephemeral-runner.sh" "$label" "$trusted/metadata/uuid" "$trusted/metadata/token" \
    >/dev/null 2>&1; then
  printf 'ephemeral runner launcher reported a timed-out registration as successful\n' >&2
  exit 1
fi
[ ! -e "$trusted/metadata/uuid" ] && [ ! -e "$trusted/metadata/token" ]

install_metadata
if QOS_FORGEJO_RUNNER="$trusted/bin/forgejo-runner" QOS_TEST_VERSION=12.4.9 \
    QOS_TEST_ARGUMENTS="$tmp/old-version" \
    "$repo/tools/run-imager-ephemeral-runner.sh" "$label" "$trusted/metadata/uuid" "$trusted/metadata/token" \
    >/dev/null 2>&1; then
  printf 'ephemeral runner launcher accepted a runner without safe one-job support\n' >&2
  exit 1
fi
[ ! -e "$trusted/metadata/uuid" ] && [ ! -e "$trusted/metadata/token" ]

printf 'ok    native runner launcher binds one verified host to one job\n'
