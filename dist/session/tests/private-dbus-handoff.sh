#!/usr/bin/env bash
# Verify the real D-Bus activation environment without touching the desktop's
# session bus. systemctl is intentionally a local no-op here; the companion
# private-user-manager test exercises the target with a manager when one can
# be started in the runner.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
. "$repo_root/tools/desktop-isolation.sh"
# The private bus below re-enters this script with its own address.
if [[ ${1:-} != --private-bus ]]; then
    tilo_test_isolate "$@"
fi
for command in dbus-run-session dbus-send dbus-update-activation-environment; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "private D-Bus handoff: SKIP ($command is unavailable)"
        exit 0
    fi
done

work=$(mktemp -d)
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT

private_bus() {
    local root=$1 repo=$2 wrapper_pid
    export XDG_DATA_HOME="$root/data"
    export TILO_SESSION_TEST_LOG="$root/log"
    mkdir -p "$XDG_DATA_HOME/dbus-1/services"

    printf '%s\n' '#!/bin/sh' 'exit 0' >"$root/systemctl"
    printf '%s\n' '#!/bin/sh' 'exit 0' >"$root/loginctl"
    printf '%s\n' \
        '#!/bin/sh' \
        'printf "tilo-shell:%s:%s\\n" "$XDG_CURRENT_DESKTOP" "$XDG_SESSION_TYPE" >>"$TILO_SESSION_TEST_LOG"' \
        'trap '\''exit 0'\'' HUP INT TERM' \
        'while :; do sleep 1; done' \
        >"$root/tilo-shell"
    printf '%s\n' \
        '#!/bin/sh' \
        'printf "dbus-activation:%s:%s\\n" "$XDG_CURRENT_DESKTOP" "$XDG_SESSION_TYPE" >>"$TILO_SESSION_TEST_LOG"' \
        >"$root/dbus-probe"
    chmod 0755 "$root/systemctl" "$root/loginctl" "$root/tilo-shell" "$root/dbus-probe"
    for probe_name in Initial Tilo Empty EmptyLogout Gnome; do
        printf '[D-BUS Service]\nName=org.tilo.SessionProbe%s\nExec=%s\n' \
            "$probe_name" "$root/dbus-probe" \
            >"$XDG_DATA_HOME/dbus-1/services/org.tilo.SessionProbe${probe_name}.service"
    done
    # The wrapper execs the packaged runner by absolute path. Stage both from
    # the tree under test: a developer host may have Tilo installed, and its
    # runner would stop the desktop's real graphical-session.target. The
    # activation environment is what this test proves, so that command stays
    # real and talks to the private bus.
    tilo_stage_script "$repo/dist/session/tilo-session-runner" "$root/tilo-session-runner" \
        SYSTEMCTL="$root/systemctl" \
        LOGINCTL="$root/loginctl" \
        FLIGHT_RECORDER="$repo/dist/session/tilo-flight-recorder.py" || return 1
    tilo_stage_script "$repo/dist/session/tilo-session" "$root/tilo-session" \
        TILO_SHELL="$root/tilo-shell" \
        SESSION_RUNNER="$root/tilo-session-runner" || return 1
    export XDG_RUNTIME_DIR="$root/runtime" XDG_STATE_HOME="$root/state"
    mkdir -m 0700 "$XDG_RUNTIME_DIR"

    probe() {
        local probe_name=$1
        dbus-send --session --dest="org.tilo.SessionProbe${probe_name}" /org/tilo/SessionProbe \
            org.tilo.SessionProbe.Ping >/dev/null 2>&1 || true
    }
    await_probe() {
        local value=$1
        for _ in $(seq 1 100); do
            grep -Fq "$value" "$TILO_SESSION_TEST_LOG" 2>/dev/null && return 0
            sleep 0.02
        done
        grep -Fqx "$value" "$TILO_SESSION_TEST_LOG"
    }

    export XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_TYPE=wayland
    dbus-update-activation-environment \
        TILO_SESSION_TEST_LOG XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
    probe Initial
    await_probe 'dbus-activation:GNOME:wayland'

    "$root/tilo-session" &
    wrapper_pid=$!
    for _ in $(seq 1 100); do
        grep -Fq 'tilo-shell:tilo:wayland' "$TILO_SESSION_TEST_LOG" 2>/dev/null && break
        sleep 0.02
    done
    grep -Fqx 'tilo-shell:tilo:wayland' "$TILO_SESSION_TEST_LOG"
    probe Tilo
    await_probe 'dbus-activation:tilo:wayland'

    kill -TERM "$wrapper_pid"
    wait "$wrapper_pid" || [[ $? == 143 ]]
    probe Empty
    await_probe 'dbus-activation::'

    # Repeat the lifecycle through the compositor's ordinary Power → Log out
    # channel. The first half above proves display-manager termination; this
    # half proves a user-requested logout also clears D-Bus activation before
    # a different desktop starts activating services.
    printf '%s\n' \
        '#!/bin/sh' \
        'case "${TILO_SESSION_LOGOUT_REQUEST:-}" in /*) ;; *) exit 42 ;; esac' \
        'printf "tilo-shell:%s:%s\\n" "$XDG_CURRENT_DESKTOP" "$XDG_SESSION_TYPE" >>"$TILO_SESSION_TEST_LOG"' \
        'printf "%s\\n" "$TILO_SESSION_LOGOUT_REQUEST" >"$TILO_SESSION_TEST_LOG.logout-path"' \
        'printf "logout\\n" >"$TILO_SESSION_LOGOUT_REQUEST"' \
        'exit 19' \
        >"$root/tilo-shell"
    chmod 0755 "$root/tilo-shell"
    set +e
    "$root/tilo-session"
    logout_status=$?
    set -e
    [[ $logout_status == 0 ]]
    logout_path=$(cat "$TILO_SESSION_TEST_LOG.logout-path")
    [[ ! -e $logout_path ]]
    probe EmptyLogout
    await_probe 'dbus-activation::'

    export XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_TYPE=wayland
    dbus-update-activation-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
    probe Gnome
    await_probe 'dbus-activation:GNOME:wayland'
}

if [[ ${1:-} == --private-bus ]]; then
    if ! private_bus "$2" "$3"; then
        exit 1
    fi
    exit 0
fi

if ! XDG_DATA_HOME="$work/data" dbus-run-session -- "$0" --private-bus "$work" "$repo_root" \
    2>"$work/dbus.stderr"; then
    # Some minimal validation containers expose neither Linux memfd nor a
    # usable /tmp-backed stream transport to dbus-daemon.  That prevents the
    # private test bus from activating even the initial probe, before any Tilo
    # session lifecycle code runs.  Keep that host limitation distinct from a
    # real hand-off failure; Fedora package lanes and developer hosts still run
    # the complete test.
    if ! grep -Fq 'dbus-activation:' "$work/log" 2>/dev/null; then
        echo 'private D-Bus handoff: SKIP (private bus could not activate the initial probe)'
        exit 0
    fi
    sed -n '1,160p' "$work/dbus.stderr" >&2
    exit 1
fi
echo 'private D-Bus handoff: PASS'
