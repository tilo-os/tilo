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
  printf 'skip  imager broker adversarial fixtures require root installation\n'
  exit 0
fi
trusted="/run/qos-imager-broker-test-$$"
"${root_do[@]}" mkdir -p "$trusted"

cat > "$tmp/signer" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
printf 'public key\n' > "$tmp/key"
cat > "$tmp/broker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
kind="$1"; output="$2"
[ ! -e "$output" ] || exit 70
umask 077
case "${CASE:-valid}" in
  duplicate) printf 'QOS_LINUX_SIGNER=%s\nQOS_LINUX_SIGNER=%s\n' "$SIGNER" "$SIGNER" > "$output" ;;
  multiline) printf 'QOS_LINUX_SIGNER=%s\nQOS_LINUX_VERIFY_KEY=%s\nBAD=value\n' "$SIGNER" "$KEY" > "$output" ;;
  token-newline) printf 'token-with-newline\n' > "$output" ;;
  *)
    if [ "$kind" = linux ]; then
      printf 'QOS_LINUX_SIGNER=%s\nQOS_LINUX_VERIFY_KEY=%s\n' "$SIGNER" "$KEY" > "$output"
    else
      printf 'one-token-without-newline' > "$output"
    fi
    ;;
esac
[ "${CASE:-}" != lax ] || chmod 0644 "$output"
EOF
"${root_do[@]}" install -o root -g root -m 0755 "$tmp/signer" "$trusted/signer"
"${root_do[@]}" install -o root -g root -m 0644 "$tmp/key" "$trusted/key"
"${root_do[@]}" install -o root -g root -m 0755 "$tmp/broker" "$trusted/broker"

base=(env RUNNER_TEMP="$tmp" GITHUB_WORKSPACE="$repo"
  ACTIONS_ID_TOKEN_REQUEST_URL=https://code.invalid/token
  ACTIONS_ID_TOKEN_REQUEST_TOKEN=request SIGNER="$trusted/signer" KEY="$trusted/key")
output="$("${base[@]}" "$repo/apps/imager/packaging/acquire-release-credentials.sh" \
  linux "$trusted/broker")"
grep -Fxq "QOS_LINUX_SIGNER=$trusted/signer" <<< "$output"
grep -Fxq "QOS_LINUX_VERIFY_KEY=$trusted/key" <<< "$output"

for rejected in duplicate multiline lax; do
  if "${base[@]}" CASE="$rejected" \
      "$repo/apps/imager/packaging/acquire-release-credentials.sh" linux "$trusted/broker" \
      >/dev/null 2>&1; then
    printf 'imager credential helper accepted %s broker output\n' "$rejected" >&2
    exit 1
  fi
done

token="$tmp/token"
"${base[@]}" "$repo/apps/imager/packaging/acquire-release-token.sh" \
  homebrew "$trusted/broker" "$token"
[ "$(cat "$token")" = one-token-without-newline ]
rm -f "$token"
for rejected in token-newline lax; do
  if "${base[@]}" CASE="$rejected" \
      "$repo/apps/imager/packaging/acquire-release-token.sh" homebrew "$trusted/broker" "$token" \
      >/dev/null 2>&1; then
    printf 'imager token helper accepted %s broker output\n' "$rejected" >&2
    exit 1
  fi
  rm -f "$token"
done
printf 'ok    imager broker outputs and trusted paths fail closed\n'
