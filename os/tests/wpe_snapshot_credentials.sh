#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
helper="$repo/os/dev/acquire-wpe-snapshot-credentials.sh"
tmp="$(mktemp -d)"
trusted_test_root=
cleanup() {
  rm -rf "$tmp"
  if [ -n "$trusted_test_root" ]; then
    if [ "$(id -u)" = 0 ]; then
      rm -rf "$trusted_test_root"
    else
      sudo -n rm -rf "$trusted_test_root"
    fi
  fi
}
trap cleanup EXIT
mkdir "$tmp/bin" "$tmp/runner"

cat > "$tmp/bin/docker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
test "$1" = login
test "$2" = code.sandbox.example
test "$3" = --username
test -n "$4"
test "$5" = --password-stdin
password="$(cat)"
test -n "$password"
printf '%s\n' "$4" >> "$QOS_TEST_LOGIN_LOG"
EOF
chmod +x "$tmp/bin/docker"

run_helper() {
  env -i PATH="$tmp/bin:/usr/bin:/bin" \
    RUNNER_TEMP="$tmp/runner" GITHUB_ENV="$tmp/github.env" \
    GITHUB_WORKSPACE="$repo" GITHUB_ACTOR=qos-ci \
    QOS_TEST_LOGIN_LOG="$tmp/login.log" "$@" "$helper" "$mode"
}

mode=unsupported
if run_helper >/dev/null 2>&1; then
  printf 'credential helper accepted an unsupported lease mode\n' >&2
  exit 1
fi
mode=promote
if run_helper >/dev/null 2>&1; then
  printf 'credential helper accepted an unconfigured steady-state job\n' >&2
  exit 1
fi

if run_helper WPE_ALLOW_STATIC_CREDENTIAL_BOOTSTRAP=1 \
    WPE_STATIC_BOOTSTRAP_EXPIRES_AT="$(( $(date +%s) + 86401 ))" \
    REGISTRY_TOKEN=secret >/dev/null 2>&1; then
  printf 'credential helper accepted an overlong bootstrap window\n' >&2
  exit 1
fi

if run_helper WPE_ALLOW_STATIC_CREDENTIAL_BOOTSTRAP=1 \
    WPE_STATIC_BOOTSTRAP_EXPIRES_AT="$(( $(date +%s) - 1 ))" \
    REGISTRY_TOKEN=secret >/dev/null 2>&1; then
  printf 'credential helper accepted an expired bootstrap window\n' >&2
  exit 1
fi

expiry="$(( $(date +%s) + 300 ))"
run_helper WPE_ALLOW_STATIC_CREDENTIAL_BOOTSTRAP=1 \
  WPE_STATIC_BOOTSTRAP_EXPIRES_AT="$expiry" REGISTRY_TOKEN=secret
grep -Fxq qos-ci "$tmp/login.log"

mode=publish-x86_64
run_helper WPE_ALLOW_STATIC_CREDENTIAL_BOOTSTRAP=1 \
  WPE_STATIC_BOOTSTRAP_EXPIRES_AT="$expiry" REGISTRY_TOKEN=secret
grep -Fxq QOS_WPE_STATIC_BOOTSTRAP=1 "$tmp/github.env"

if [ "$(id -u)" = 0 ]; then
  root_do=()
elif sudo -n true >/dev/null 2>&1; then
  root_do=(sudo -n)
else
  printf 'skip  broker-path adversarial tests need root fixture installation\n'
  printf 'ok    WPE credential acquisition fails closed and bounds static bootstrap\n'
  exit 0
fi

trusted_test_root="/run/qos-wpe-credential-test-$$"
"${root_do[@]}" mkdir -p "$trusted_test_root/workspace"
"${root_do[@]}" chmod 0755 "$trusted_test_root" "$trusted_test_root/workspace"
cat > "$tmp/broker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
mode="$1"
output="$2"
case "${QOS_TEST_BROKER_CASE:-valid}" in
  expired) expiry="$(( $(date +%s) - 1 ))" ;;
  overlong) expiry="$(( $(date +%s) + 901 ))" ;;
  *) expiry="$(( $(date +%s) + 300 ))" ;;
esac
umask 077
{
  printf 'QOS_REGISTRY_USERNAME=qos-wpe\n'
  printf 'QOS_REGISTRY_PASSWORD=short-lease\n'
  printf 'QOS_LEASE_EXPIRES_AT=%s\n' "$expiry"
  case "${QOS_TEST_BROKER_CASE:-valid}" in
    unknown) printf 'UNSUPPORTED=value\n' ;;
    duplicate) printf 'QOS_REGISTRY_USERNAME=again\n' ;;
    promote-signer) printf 'QOS_WPE_SIGNER=%s\n' "$QOS_TEST_SIGNER" ;;
    workspace-signer) printf 'QOS_WPE_SIGNER=%s\n' "$QOS_TEST_WORKSPACE_SIGNER" ;;
    symlink-signer) printf 'QOS_WPE_SIGNER=%s\n' "$QOS_TEST_SYMLINK_SIGNER" ;;
    *)
      if [[ "$mode" = publish-* ]]; then
        printf 'QOS_WPE_SIGNER=%s\n' "$QOS_TEST_SIGNER"
      fi
      ;;
  esac
} > "$output"
[ "${QOS_TEST_BROKER_CASE:-valid}" != lax-mode ] || chmod 0644 "$output"
EOF
cat > "$tmp/signer" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
"${root_do[@]}" install -o root -g root -m 0755 "$tmp/broker" "$trusted_test_root/broker"
"${root_do[@]}" install -o root -g root -m 0755 "$tmp/signer" "$trusted_test_root/signer"
"${root_do[@]}" install -o root -g root -m 0755 "$tmp/signer" "$trusted_test_root/workspace/signer"
"${root_do[@]}" ln -s "$trusted_test_root/signer" "$trusted_test_root/signer-link"
"${root_do[@]}" ln -s "$trusted_test_root/broker" "$trusted_test_root/broker-link"

run_broker() {
  run_helper WPE_SNAPSHOT_CREDENTIAL_BROKER="$trusted_test_root/broker" \
    ACTIONS_ID_TOKEN_REQUEST_URL=https://code.invalid/token?x=1 \
    ACTIONS_ID_TOKEN_REQUEST_TOKEN=request-token \
    QOS_TEST_SIGNER="$trusted_test_root/signer" \
    QOS_TEST_WORKSPACE_SIGNER="$trusted_test_root/workspace/signer" \
    QOS_TEST_SYMLINK_SIGNER="$trusted_test_root/signer-link" "$@"
}

mode=resolve-x86_64
if run_helper WPE_SNAPSHOT_CREDENTIAL_BROKER="$trusted_test_root/broker" \
    QOS_TEST_SIGNER="$trusted_test_root/signer" >/dev/null 2>&1; then
  printf 'credential helper invoked a broker without Forgejo OIDC\n' >&2
  exit 1
fi
if run_helper WPE_SNAPSHOT_CREDENTIAL_BROKER="$tmp/broker" \
    ACTIONS_ID_TOKEN_REQUEST_URL=https://code.invalid/token?x=1 \
    ACTIONS_ID_TOKEN_REQUEST_TOKEN=request-token >/dev/null 2>&1; then
  printf 'credential helper accepted a runner-owned broker\n' >&2
  exit 1
fi
if run_helper WPE_SNAPSHOT_CREDENTIAL_BROKER="$trusted_test_root/broker-link" \
    ACTIONS_ID_TOKEN_REQUEST_URL=https://code.invalid/token?x=1 \
    ACTIONS_ID_TOKEN_REQUEST_TOKEN=request-token >/dev/null 2>&1; then
  printf 'credential helper accepted a symlinked broker\n' >&2
  exit 1
fi
: > "$tmp/github.env"
run_broker QOS_TEST_BROKER_CASE=valid
if grep -Fq QOS_WPE_SIGNER "$tmp/github.env"; then
  printf 'credential helper exposed a signer during base resolution\n' >&2
  exit 1
fi

mode=publish-x86_64
run_broker QOS_TEST_BROKER_CASE=valid
grep -Fxq "QOS_WPE_SIGNER=$trusted_test_root/signer" "$tmp/github.env"
for rejected in expired overlong unknown duplicate lax-mode workspace-signer symlink-signer; do
  extra=()
  [ "$rejected" != workspace-signer ] \
    || extra=(GITHUB_WORKSPACE="$trusted_test_root/workspace")
  if run_broker QOS_TEST_BROKER_CASE="$rejected" "${extra[@]}" >/dev/null 2>&1; then
    printf 'credential helper accepted rejected broker case: %s\n' "$rejected" >&2
    exit 1
  fi
done

mode=promote
run_broker QOS_TEST_BROKER_CASE=valid
if run_broker QOS_TEST_BROKER_CASE=promote-signer >/dev/null 2>&1; then
  printf 'credential helper accepted a signer in a promotion lease\n' >&2
  exit 1
fi

printf 'ok    WPE credential acquisition fails closed and bounds static bootstrap\n'
printf 'ok    WPE broker leases reject unsafe paths, lifetimes, and output shapes\n'
