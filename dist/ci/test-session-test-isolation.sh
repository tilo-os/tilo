#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
guard=$repo_root/dist/ci/require-disposable-session-test-env.sh

expect_refusal() {
    local output
    if output=$("$@" 2>&1); then
        echo "unsafe session-test invocation was accepted: $*" >&2
        exit 1
    fi
    grep -Fq 'refusing session-affecting tests' <<<"$output"
}

expect_refusal env -u TILO_DISPOSABLE_TEST_ENV "$guard"
expect_refusal env TILO_DISPOSABLE_TEST_ENV=1 WAYLAND_DISPLAY=wayland-0 "$guard"
expect_refusal env TILO_DISPOSABLE_TEST_ENV=1 DISPLAY=:0 "$guard"
expect_refusal env TILO_DISPOSABLE_TEST_ENV=1 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "$guard"
expect_refusal env TILO_DISPOSABLE_TEST_ENV=1 XAUTHORITY=/run/user/1000/gdm/Xauthority "$guard"
expect_refusal env TILO_DISPOSABLE_TEST_ENV=1 XDG_CURRENT_DESKTOP=GNOME "$guard"
expect_refusal env TILO_DISPOSABLE_TEST_ENV=1 XDG_SESSION_ID=3 "$guard"

if [[ -e /.dockerenv || -e /run/.containerenv ]]; then
    env -u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u XAUTHORITY \
        -u XDG_CURRENT_DESKTOP -u XDG_SESSION_ID -u DESKTOP_SESSION -u GDMSESSION \
        TILO_DISPOSABLE_TEST_ENV=1 "$guard"
fi

grep -Fq 'require-disposable-session-test-env.sh' \
    "$repo_root/dist/ci/run-payload-contract-tests.sh"
grep -Fq 'require-disposable-session-test-env.sh' \
    "$repo_root/dist/ci/run-dev-session-test.sh"
grep -Fq -- '--host-safe' "$repo_root/dist/ci/run-payload-contract-tests.sh"
! grep -Fq 'private-dbus-handoff.sh' "$repo_root/dist/ci/run-payload-contract-tests.sh"
! grep -Fq 'private-user-manager-handoff.sh' "$repo_root/dist/ci/run-payload-contract-tests.sh"

for broken_fixture in private-dbus-handoff.sh private-user-manager-handoff.sh; do
    grep -Fq 'DISABLED: this historical fixture is not an isolation boundary' \
        "$repo_root/dist/session/tests/$broken_fixture"
done

echo 'session-test isolation guard: PASS'
