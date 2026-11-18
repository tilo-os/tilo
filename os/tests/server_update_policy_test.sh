#!/usr/bin/env bash
# tilo-server-update-policy: applies exactly the settings a server's entry
# asked for, through tilo-update-policy-set, and refuses a request naming
# anything else whole.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
helper="$repo/os/usr/libexec/tilo-server-update-policy"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cat >"$work/policy-set" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$TILO_TEST_CALLS"
EOF
chmod +x "$work/policy-set"
request="$work/update-policy.request"
calls="$work/calls"
export TILO_TEST_CALLS="$calls"
export TILO_SERVER_POLICY_REQUEST="$request"
export TILO_UPDATE_POLICY_SET="$work/policy-set"

apply() {
    : >"$calls"
    : >"$request"
    printf '%s' "$1" >"$request"
    "$helper"
    sort "$calls"
}

# The updates and window the entry names reach the policy helper as its own verbs.
[ "$(apply 'updates=stable
window=03:00-04:00
')" = 'channel-stable
window 03:00-04:00' ]

# No window line clears the window the entry no longer names; no updates line
# leaves the image's own updates alone.
[ "$(apply 'updates=dev
')" = 'channel-dev
window none' ]
[ "$(apply '')" = 'window none' ]

# A window across midnight is a time of day like any other.
[ "$(apply 'window=22:00-02:00
')" = 'window 22:00-02:00' ]

# A request naming what an entry cannot say is refused whole: nothing is
# applied, not even the lines that were fine.
for refused in \
    'updates=nightly
' \
    'updates=stable
window=24:00-04:00
' \
    'updates=stable
window=03:00-04:00
window=03:00-05:00
' \
    'schedule=daily
' \
    'window=3:00-4:00
'; do
    : >"$calls"
    printf '%s' "$refused" >"$request"
    if "$helper" 2>/dev/null; then
        echo "a request naming '$refused' was applied" >&2
        exit 1
    fi
    [ ! -s "$calls" ] || { echo "a refused request was applied" >&2; exit 1; }
done

# A machine with no request writes nothing.
rm -f "$request"
: >"$calls"
"$helper"
[ ! -s "$calls" ]

echo "tilo-server-update-policy: ok"
