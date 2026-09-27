#!/usr/bin/env bash
# host-update.sh against a recording rpm-ostree: which packages it swaps, in
# how many transactions, and that it refuses RPMs built for another commit.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
helper=$repo_root/dist/rpm/host-update.sh
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
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
cat >"$work/bin/build-rpm" <<EOF
#!/bin/sh
set -e
mkdir -p "\$TILO_PACKAGE_OUT/RPMS/$arch" "\$TILO_PACKAGE_OUT/RPMS/noarch"
: >"\$TILO_PACKAGE_OUT/RPMS/$arch/tilo-session-$version-\$TILO_RPM_RELEASE.fc44.$arch.rpm"
: >"\$TILO_PACKAGE_OUT/RPMS/$arch/tilo-session-apps-$version-\$TILO_RPM_RELEASE.fc44.$arch.rpm"
: >"\$TILO_PACKAGE_OUT/RPMS/noarch/tilo-session-devel-$version-\$TILO_RPM_RELEASE.fc44.noarch.rpm"
echo "\$*" >"\$TILO_PACKAGE_OUT/build-args"
EOF
chmod +x "$work/bin"/*

export PATH="$work/bin:$PATH"
export TILO_RPM_OSTREE=$work/bin/rpm-ostree TILO_BUILD_RPM=$work/bin/build-rpm
export TILO_PACKAGE_OUT=$work/out FAKE_LOG=$work/log FAKE_STATUS=$work/status

fail() { echo "test-host-update: $*" >&2; exit 1; }
status() {
    printf '{"deployments":[{"booted":true,"requested-local-packages":[%s],"requested-packages":["alacritty"]}]}' "$1" \
        >"$FAKE_STATUS"
}

# Everything layered, including the developer entry: one uninstall+install.
status '"chatgpt-1-1.x86_64","tilo-session-0.1.13-1.fc44.x86_64","tilo-session-apps-0.1.13-1.fc44.x86_64","tilo-session-devel-0.1.13-1.fc44.noarch"'
: >"$FAKE_LOG"
"$helper" --allow-dirty >/dev/null
release=$(find "$TILO_PACKAGE_OUT/RPMS/$arch" -name 'tilo-session-apps-*' -printf '%f\n' \
    | sed -E "s/^tilo-session-apps-$version-(.*)\.fc44\.$arch\.rpm$/\1/")
[[ $release =~ ^0\.[0-9]{12}\.git[0-9a-f]{10}(\.dirty)?$ ]] || fail "release does not name the commit: $release"
[[ $(wc -l <"$FAKE_LOG") -eq 1 ]] || fail "expected exactly one rpm-ostree transaction: $(cat "$FAKE_LOG")"
grep -Eq "^uninstall tilo-session tilo-session-apps tilo-session-devel --install=.*/tilo-session-$version-$release\.fc44\.$arch\.rpm --install=.*/tilo-session-apps-.* --install=.*/tilo-session-devel-.*noarch\.rpm$" \
    "$FAKE_LOG" || fail "unexpected swap: $(cat "$FAKE_LOG")"
grep -q chatgpt "$FAKE_LOG" && fail "touched an unrelated layered package"
[[ -z $(<"$TILO_PACKAGE_OUT/build-args") ]] || fail "a full build passed arguments: $(cat "$TILO_PACKAGE_OUT/build-args")"

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
"$helper" --allow-dirty --apply-live >/dev/null
grep -Fxq 'apply-live --allow-replacement' "$FAKE_LOG" || fail "apply-live was not requested"
if FAKE_TILO_RUNNING=1 "$helper" --allow-dirty --no-build --apply-live >"$work/live.out" 2>&1; then
    fail "applied live under a running Tilo session"
fi
grep -q 'session is running' "$work/live.out" || fail "live refusal did not explain itself"

echo "host-update contract passed"
