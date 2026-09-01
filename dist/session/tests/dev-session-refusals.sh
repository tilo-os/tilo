#!/usr/bin/env bash
# Drive dist/session/qos-session-dev against the inputs it is supposed to
# refuse, and one it is supposed to accept.
#
# The script deliberately reads exactly one path — the invoking user's
# ~/.config/qos/dev-session, resolved from the passwd database, with no
# environment variable or argument that can point it elsewhere. That is what
# makes it worth testing on the real path rather than a temporary one, so this
# saves any existing config and puts it back on the way out.
set -uo pipefail

# The production wrapper deliberately rejects group-writable configuration
# directories. Distro defaults vary between 0022 and user-private-group 0002,
# so make the accepting fixture private instead of inheriting the test host's
# policy.
umask 0077

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
wrapper=$here/../qos-session-dev
home=$(getent passwd "$(id -u)" | cut -d: -f6)
conf=$home/.config/qos/dev-session
# Under the home directory, not /tmp: the accepting case needs a target no one
# else can write, and /tmp is 1777 by definition.
mkdir -p "$home/.cache"
scratch=$(mktemp -d "$home/.cache/qos-dev-session-test.XXXXXX")
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

# refuse <name> — the wrapper must exit non-zero and print a reason.
refuse() {
    local name=$1 out rc
    out=$("$wrapper" 2>&1); rc=$?
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
marked=/tmp/qos-dev-session-test-$$
printf '#!/bin/sh\n# qos-dev-session-build-marker-v1\nexit 0\n' >"$marked"
chmod 0755 "$marked"
echo "$marked" >"$conf"
refuse "target under a world-writable directory"
rm -f "$marked"

# Same binary, this time somewhere only this user can write.
marked=$scratch/marked-shell
printf '#!/bin/sh\n# qos-dev-session-build-marker-v1\nprintf "started %%s\\n" "$*"\n' >"$marked"
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
# thing being tested rather than the wrapper refusing everything.
echo "$marked" >"$conf"
err=$scratch/accept.err
out=$("$wrapper" 2>"$err"); rc=$?
if (( rc == 0 )) && [[ $out == "started --host-desktop-session --firstboot --backend drm" ]]; then
    echo "ok: a marked, privately-owned build starts, with host-session identity setup"
    pass=$((pass + 1))
else
    echo "FAIL: expected the marked build to start; rc=$rc out=<$out>"
    sed 's/^/    /' "$err"
    fails=$((fails + 1))
fi

echo
echo "$pass passed, $fails failed"
(( fails == 0 ))
