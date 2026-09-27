#!/usr/bin/env bash
# Drive dist/session/tilo-session-dev against the inputs it is supposed to
# refuse, and one it is supposed to accept.
#
# The script deliberately reads exactly one path — the invoking user's
# ~/.config/tilo/dev-session, resolved from the passwd database, with no
# environment variable or argument that can point it elsewhere. That is what
# makes it worth testing on the real path rather than a temporary one, so it
# only runs in a disposable account: dist/ci/run-dev-session-test.sh gives it
# one in a container. It still saves any existing config and puts it back on
# the way out.
#
# Every case runs a copy of the wrapper next to a fixture lifecycle runner, so
# a refusal that regresses starts the fixture rather than the tree's runner
# with the host's systemctl.
set -uo pipefail

# The production wrapper deliberately rejects group-writable configuration
# directories. Distro defaults vary between 0022 and user-private-group 0002,
# so make the accepting fixture private instead of inheriting the test host's
# policy.
umask 0077

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
wrapper=$here/../tilo-session-dev
if [[ ! -e /.dockerenv && ! -e /run/.containerenv ]]; then
    echo "dev-session refusals: refusing to replace this account's ~/.config/tilo/dev-session;" >&2
    echo "run dist/ci/run-dev-session-test.sh, which gives the test a disposable account" >&2
    exit 2
fi
# The container gets only dist/session; a checkout also has the shared helper.
if [[ -r $here/../../../tools/desktop-isolation.sh ]]; then
    . "$here/../../../tools/desktop-isolation.sh"
    tilo_test_isolate "$@"
fi
home=$(getent passwd "$(id -u)" | cut -d: -f6)
conf=$home/.config/tilo/dev-session
# Under the home directory, not /tmp: the accepting case needs a target no one
# else can write, and /tmp is 1777 by definition.
mkdir -p "$home/.cache"
scratch=$(mktemp -d "$home/.cache/tilo-app-session-test.XXXXXX")
# Minimal packaging containers do not run logind and therefore have no
# /run/user/<uid>. Use a private per-test runtime directory, matching the
# writable XDG_RUNTIME_DIR a real login session supplies.
export XDG_RUNTIME_DIR="$scratch/runtime"
saved=

cleanup() {
    rm -rf "$scratch"
    if [[ -n $saved ]]; then
        mv -f "$saved" "$conf"
    else
        rm -f "$conf"
    fi
}
trap cleanup EXIT

if [[ -e $conf ]]; then
    saved=$scratch/saved-dev-session
    cp -a "$conf" "$saved"
fi
mkdir -p "$(dirname "$conf")"

fails=0
pass=0

wrapper_copy=$scratch/tilo-session-dev
lifecycle=$scratch/tilo-session-runner
test_runtime=$scratch/runtime
cp "$wrapper" "$wrapper_copy"
mkdir -m 0700 "$test_runtime"
cat >"$lifecycle" <<'SH'
#!/bin/sh
case "${1:-}" in /*) ;; *) exit 73 ;; esac
test -x "$1" || exit 74
env | grep '^TILO_TEST_CONTROL=' >"$(dirname -- "$XDG_RUNTIME_DIR")/managed-environment"
test -n "${TILO_DEV_SESSION_ATTESTATION:-}" || exit 75
attest_tmp=$TILO_DEV_SESSION_ATTESTATION.$$
{
    printf 'runner_pid=%s\n' "$$"
    printf 'compositor_pid=%s\n' "$$"
    sed -n '/^path=/p;/^sha256=/p;/^revision=/p;/^source_state=/p' \
        "$TILO_DEV_SESSION_ATTESTATION"
} >"$attest_tmp"
chmod 0600 "$attest_tmp"
mv -f "$attest_tmp" "$TILO_DEV_SESSION_ATTESTATION"
printf 'managed %s ::' "$1"
shift
if (( $# )); then
    printf ' %s' "$@"
fi
printf '\n'
SH
chmod 0755 "$wrapper_copy" "$lifecycle"

# refuse <name> — the wrapper must exit non-zero and print a reason.
refuse() {
    local name=$1 out rc
    out=$("$wrapper_copy" 2>&1); rc=$?
    if (( rc == 0 )); then
        echo "FAIL: $name — wrapper accepted it"
        fails=$((fails + 1))
    elif ! grep -q 'refusing to start a session' <<<"$out"; then
        echo "FAIL: $name — exited $rc but printed no refusal:"
        sed 's/^/    /' <<<"$out"
        fails=$((fails + 1))
    else
        echo "ok: $name"
        pass=$((pass + 1))
    fi
}

rm -f "$conf"
refuse "no config file at all"

echo 'target/debug/shell' >"$conf"
refuse "relative path"

echo '/does/not/exist/shell' >"$conf"
refuse "target does not exist"

# A world-writable directory anywhere above the target means someone else can
# decide what runs as this session.
marked=/tmp/tilo-app-session-test-$$
printf '#!/bin/sh\n# tilo-app-session-build-marker-v1\nexit 0\n' >"$marked"
chmod 0755 "$marked"
echo "$marked" >"$conf"
refuse "target under a world-writable directory"
rm -f "$marked"

# Same binary, this time somewhere only this user can write.
marked=$scratch/marked-shell
printf '#!/bin/sh\n# tilo-app-session-build-marker-v1\nprintf "started %%s\\n" "$*"\n' >"$marked"
chmod 0755 "$marked"
chmod 0755 "$scratch"

echo "$marked" >"$conf"
chmod 0666 "$conf"
refuse "config file writable by anyone"
chmod 0600 "$conf"

# No marker: not a compositor someone deliberately built for this.
unmarked=$scratch/unmarked-shell
printf '#!/bin/sh\nexit 0\n' >"$unmarked"
chmod 0755 "$unmarked"
echo "$unmarked" >"$conf"
refuse "target carries no dev-session build marker"

# And the accepting case, so the refusals above are known to be about the
# thing being tested rather than the wrapper refusing everything. Install a
# sibling lifecycle fixture: the real developer wrapper must hand the verified
# binary to the package-owned session lifecycle instead of bypassing it.
echo "$marked" >"$conf"
err=$scratch/accept.err
out=$(XDG_RUNTIME_DIR="$test_runtime" "$wrapper_copy" 2>"$err"); rc=$?
expected="managed $(realpath -e "$marked") ::"
if (( rc == 0 )) && [[ $out == "$expected" ]]; then
    echo "ok: a marked, privately-owned build enters the package session lifecycle"
    pass=$((pass + 1))
else
    echo "FAIL: expected the marked build to enter the lifecycle; rc=$rc out=<$out>"
    sed 's/^/    /' "$err"
    fails=$((fails + 1))
fi

attest=$test_runtime/tilo-app-session/active
if [[ -r $attest ]] \
    && grep -Fqx "path=$(realpath -e "$marked")" "$attest" \
    && grep -Eq '^runner_pid=[0-9]+$' "$attest" \
    && grep -Eq '^compositor_pid=[0-9]+$' "$attest" \
    && ! grep -q '^pid=' "$attest" \
    && grep -Eq '^sha256=[0-9a-f]{64}$' "$attest"; then
    echo "ok: developer launch records a protected binary attestation"
    pass=$((pass + 1))
else
    echo "FAIL: developer launch attestation is missing or malformed"
    fails=$((fails + 1))
fi

if grep -Fqx "TILO_TEST_CONTROL=$test_runtime/tilo-app-session/control.sock" "$scratch/managed-environment"; then
    echo "ok: developer lifecycle receives a private live-control endpoint"
    pass=$((pass + 1))
else
    echo "FAIL: developer lifecycle did not receive its private live-control endpoint"
    fails=$((fails + 1))
fi

echo
echo "$pass passed, $fails failed"
(( fails == 0 ))
