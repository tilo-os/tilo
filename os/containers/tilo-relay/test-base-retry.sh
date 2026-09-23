#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../../.." && pwd)
helper="$root/os/containers/tilo-relay/resolve-base-with-retry.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"

cat > "$work/bin/docker" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$CALLS"
count=$(wc -l < "$CALLS")
[[ $count -gt ${FAILURES:-0} ]]
SH
cat > "$work/bin/sleep" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$1" >> "$SLEEPS"
SH
chmod +x "$work/bin/docker" "$work/bin/sleep"

ref="code.sandbox.example/qos/fedora-bootc@sha256:$(printf 'a%.0s' {1..64})"
export PATH="$work/bin:$PATH" CALLS="$work/calls" SLEEPS="$work/sleeps" FAILURES=2
: > "$CALLS"
: > "$SLEEPS"
"$helper" relay-builder "$ref"

[[ $(wc -l < "$CALLS") == 3 ]]
[[ $(cat "$SLEEPS") == $'2\n4' ]]
[[ $(grep -Fc -- '--builder relay-builder' "$CALLS") == 3 ]]
[[ $(grep -Fc -- "--build-arg BASE_REF=$ref" "$CALLS") == 3 ]]

: > "$CALLS"
: > "$SLEEPS"
export FAILURES=3
if "$helper" relay-builder "$ref" >"$work/final.out" 2>"$work/final.err"; then
  echo 'helper accepted three failed BuildKit lookups' >&2
  exit 1
fi
grep -Fq 'remained unavailable after 3 attempts' "$work/final.err"
[[ $(wc -l < "$CALLS") == 3 ]]

: > "$CALLS"
if "$helper" relay-builder code.sandbox.example/qos/fedora-bootc:latest >"$work/tag.out" 2>"$work/tag.err"; then
  echo 'helper accepted a mutable base tag' >&2
  exit 1
fi
grep -Fq 'must be the digest-pinned Fedora bootc mirror' "$work/tag.err"
[[ ! -s "$CALLS" ]]

echo 'relay base retry tests passed'
