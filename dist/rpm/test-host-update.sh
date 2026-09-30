#!/usr/bin/env bash
# host-update.sh against a recording rpm-ostree: which packages it swaps, in
# how many transactions, that it refuses RPMs built for another commit, that
# the deployment is written before it returns rather than at a clean shutdown,
# and that the new build's trusted publisher reaches /etc.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
helper=$repo_root/dist/rpm/host-update.sh
work=$(mktemp -d)
trap 'chmod -R u+w -- "$work" 2>/dev/null; rm -rf -- "$work"' EXIT
arch=$(uname -m)
version=$(<"$repo_root/dist/VERSION")

mkdir -p "$work/bin"
cat >"$work/bin/sudo" <<'EOF'
#!/bin/sh
exec "$@"
EOF
cat >"$work/bin/rpm" <<EOF
#!/bin/sh
echo $arch
EOF
cat >"$work/bin/pgrep" <<'EOF'
#!/bin/sh
[ -n "${FAKE_TILO_RUNNING:-}" ]
EOF
cat >"$work/bin/rpm-ostree" <<'EOF'
#!/bin/sh
if [ "$1" = status ]; then cat "$FAKE_STATUS"; exit 0; fi
echo "$*" >>"$FAKE_LOG"
EOF
cat >"$work/bin/ostree" <<'EOF'
#!/bin/sh
echo "$*" >>"$FAKE_OSTREE_LOG"
[ "$*" = "admin finalize-staged" ] || exit 2
[ -z "${FAKE_FINALIZE_FAIL:-}" ] || exit 1
sed -i 's/"staged":true/"staged":false/' "$FAKE_STATUS"
EOF
cat >"$work/bin/build-rpm" <<EOF
#!/bin/sh
set -e
mkdir -p "\$TILO_PACKAGE_OUT/RPMS/$arch"
: >"\$TILO_PACKAGE_OUT/RPMS/$arch/tilo-session-$version-\$TILO_RPM_RELEASE.fc44.$arch.rpm"
: >"\$TILO_PACKAGE_OUT/RPMS/$arch/tilo-session-apps-$version-\$TILO_RPM_RELEASE.fc44.$arch.rpm"
: >"\$TILO_PACKAGE_OUT/RPMS/$arch/tilo-session-devel-$version-\$TILO_RPM_RELEASE.fc44.$arch.rpm"
echo "\$*" >"\$TILO_PACKAGE_OUT/build-args"
EOF
chmod +x "$work/bin"/*

export PATH="$work/bin:$PATH"
export TILO_RPM_OSTREE=$work/bin/rpm-ostree TILO_BUILD_RPM=$work/bin/build-rpm
export TILO_PACKAGE_OUT=$work/out FAKE_LOG=$work/log FAKE_STATUS=$work/status
export TILO_OSTREE=$work/bin/ostree FAKE_OSTREE_LOG=$work/ostree-log TILO_SYSROOT=$work/root

fail() { echo "test-host-update: $*" >&2; exit 1; }
# The pending deployment rpm-ostree reports: staged, as rpm-ostree leaves it.
status() {
    printf '{"deployments":[{"staged":true,"osname":"default","checksum":"c0ffee","serial":1,"requested-local-packages":[%s],"requested-packages":["alacritty"]},{"booted":true,"osname":"default","checksum":"booted","serial":0}]}' "$1" \
        >"$FAKE_STATUS"
    : >"$FAKE_OSTREE_LOG"
}
# The running tree and the new deployment, each with a packaged key in usr/etc
# and a locally changed /etc copy that the three-way merge would keep.
trust=etc/tilo/runtime-bundle-trusted-publishers.txt
running=$TILO_SYSROOT
deployment=$TILO_SYSROOT/ostree/deploy/default/deploy/c0ffee.1
trees() {
    rm -rf -- "$TILO_SYSROOT"
    for root in "$running" "$deployment"; do
        mkdir -p "$root/usr/etc/tilo" "$root/etc/tilo"
        echo new-publisher >"$root/usr/$trust"
        echo stale-publisher >"$root/$trust"
    done
}
key() { cat "$1/$trust"; }
trees

# Everything layered, including the developer entry: one uninstall+install.
status '"chatgpt-1-1.x86_64","tilo-session-0.1.13-1.fc44.x86_64","tilo-session-apps-0.1.13-1.fc44.x86_64","tilo-session-devel-0.1.13-1.fc44.noarch"'
: >"$FAKE_LOG"
"$helper" --allow-dirty >/dev/null
release=$(find "$TILO_PACKAGE_OUT/RPMS/$arch" -name 'tilo-session-apps-*' -printf '%f\n' \
    | sed -E "s/^tilo-session-apps-$version-(.*)\.fc44\.$arch\.rpm$/\1/")
[[ $release =~ ^0\.[0-9]{12}\.git[0-9a-f]{10}(\.dirty)?$ ]] || fail "release does not name the commit: $release"
[[ $(wc -l <"$FAKE_LOG") -eq 1 ]] || fail "expected exactly one rpm-ostree transaction: $(cat "$FAKE_LOG")"
grep -Eq "^uninstall tilo-session tilo-session-apps tilo-session-devel --install=.*/tilo-session-$version-$release\.fc44\.$arch\.rpm --install=.*/tilo-session-apps-.* --install=.*/tilo-session-devel-.*\.$arch\.rpm$" \
    "$FAKE_LOG" || fail "unexpected swap: $(cat "$FAKE_LOG")"
grep -q chatgpt "$FAKE_LOG" && fail "touched an unrelated layered package"
[[ -z $(<"$TILO_PACKAGE_OUT/build-args") ]] || fail "a full build passed arguments: $(cat "$TILO_PACKAGE_OUT/build-args")"
# Written now, not left for a shutdown that a hard reset never runs.
grep -Fxq 'admin finalize-staged' "$FAKE_OSTREE_LOG" || fail "the deployment was left staged"
[[ $(key "$deployment") == new-publisher ]] || fail "the new deployment kept a stale trusted publisher"
[[ $(key "$running") == stale-publisher ]] || fail "changed the running trust policy without --apply-live"

# Nothing layered: a plain install, and the developer entry stays opt-in.
status '"chatgpt-1-1.x86_64"'
: >"$FAKE_LOG"
"$helper" --allow-dirty --no-cargo >/dev/null
grep -Fxq -- --no-cargo "$TILO_PACKAGE_OUT/build-args" || fail "--no-cargo did not reach build-rpm"
grep -Eq '^install .*/tilo-session-[0-9].* .*/tilo-session-apps-[0-9][^ ]*$' "$FAKE_LOG" \
    || fail "unexpected fresh install: $(cat "$FAKE_LOG")"

# A build from another commit is never what gets installed.
rm -rf "$TILO_PACKAGE_OUT"
TILO_RPM_RELEASE=0.202001010000.gitdeadbeef00 TILO_PACKAGE_OUT=$TILO_PACKAGE_OUT "$work/bin/build-rpm"
: >"$FAKE_LOG"
if "$helper" --allow-dirty --no-build >"$work/stale.out" 2>&1; then
    fail "installed RPMs built for another commit"
fi
grep -q 'no tilo-session' "$work/stale.out" || fail "stale refusal did not explain itself"
[[ ! -s $FAKE_LOG ]] || fail "ran rpm-ostree for a stale build"

# Live apply, and never under a running Tilo session.
status '"tilo-session-0.1.13-1.fc44.x86_64","tilo-session-apps-0.1.13-1.fc44.x86_64"'
trees
"$helper" --allow-dirty --apply-live >/dev/null
grep -Fxq 'apply-live --allow-replacement' "$FAKE_LOG" || fail "apply-live was not requested"
[[ $(key "$running") == new-publisher ]] || fail "the live system kept a stale trusted publisher"
[[ $(key "$deployment") == new-publisher ]] || fail "the live deployment kept a stale trusted publisher"
if FAKE_TILO_RUNNING=1 "$helper" --allow-dirty --no-build --apply-live >"$work/live.out" 2>&1; then
    fail "applied live under a running Tilo session"
fi
grep -q 'session is running' "$work/live.out" || fail "live refusal did not explain itself"

# A written deployment's /etc is read-only; one that already carries the
# packaged key, as it does once finalizing merged a live /etc, is left alone.
status '"tilo-session-0.1.13-1.fc44.x86_64","tilo-session-apps-0.1.13-1.fc44.x86_64"'
trees
echo new-publisher >"$deployment/$trust"
chmod a-w "$deployment/$trust" "$deployment/etc/tilo"
"$helper" --allow-dirty --no-build >"$work/ro.out" 2>&1 \
    || fail "rewrote a read-only deployment /etc that already matched: $(cat "$work/ro.out")"
chmod u+w "$deployment/$trust" "$deployment/etc/tilo"

# Left staged on request, or because finalizing failed: the install is not
# durable, and the helper says so and fails rather than reporting success.
for mode in --stage-only finalize-fails; do
    status '"tilo-session-0.1.13-1.fc44.x86_64","tilo-session-apps-0.1.13-1.fc44.x86_64"'
    trees
    if [[ $mode == --stage-only ]]; then
        args=(--stage-only) fail_finalize=
    else
        args=() fail_finalize=1
    fi
    if FAKE_FINALIZE_FAIL=$fail_finalize "$helper" --allow-dirty --no-build "${args[@]}" \
        >"$work/staged.out" 2>&1; then
        fail "$mode: reported success for a deployment a hard reset discards"
    fi
    grep -q 'only STAGED' "$work/staged.out" || fail "$mode: no warning: $(cat "$work/staged.out")"
    if [[ $mode == --stage-only ]] && [[ -s $FAKE_OSTREE_LOG ]]; then
        fail "--stage-only still finalized: $(cat "$FAKE_OSTREE_LOG")"
    fi
done

echo "host-update contract passed"
