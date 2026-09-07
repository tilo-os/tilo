#!/usr/bin/env bash
# Verify the real D-Bus activation environment without touching the desktop's
# session bus. systemctl is intentionally a local no-op here; the companion
# private-user-manager test exercises the target with a manager when one can
# be started in the runner.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
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
    export QOS_SESSION_TEST_LOG="$root/log"
    mkdir -p "$XDG_DATA_HOME/dbus-1/services"

    printf '%s\n' '#!/bin/sh' 'exit 0' >"$root/systemctl"
    printf '%s\n' \
        '#!/bin/sh' \
        'printf "qos-shell:%s:%s\\n" "$XDG_CURRENT_DESKTOP" "$XDG_SESSION_TYPE" >>"$QOS_SESSION_TEST_LOG"' \
        'trap '\''exit 0'\'' HUP INT TERM' \
        'while :; do sleep 1; done' \
        >"$root/qos-shell"
    printf '%s\n' \
        '#!/bin/sh' \
        'printf "dbus-activation:%s:%s\\n" "$XDG_CURRENT_DESKTOP" "$XDG_SESSION_TYPE" >>"$QOS_SESSION_TEST_LOG"' \
        >"$root/dbus-probe"
    chmod 0755 "$root/systemctl" "$root/qos-shell" "$root/dbus-probe"
    for probe_name in Initial Qos Empty Gnome; do
        printf '[D-BUS Service]\nName=org.qos.SessionProbe%s\nExec=%s\n' \
            "$probe_name" "$root/dbus-probe" \
            >"$XDG_DATA_HOME/dbus-1/services/org.qos.SessionProbe${probe_name}.service"
    done
    sed \
        -e "s|QOS_SHELL=/usr/bin/qos-shell|QOS_SHELL=$root/qos-shell|" \
        -e "s|SYSTEMCTL=/usr/bin/systemctl|SYSTEMCTL=$root/systemctl|" \
        "$repo/dist/session/qos-session" >"$root/qos-session"
    chmod 0755 "$root/qos-session"

    probe() {
        local probe_name=$1
        dbus-send --session --dest="org.qos.SessionProbe${probe_name}" /org/qos/SessionProbe \
            org.qos.SessionProbe.Ping >/dev/null 2>&1 || true
    }
    await_probe() {
        local value=$1
        for _ in $(seq 1 100); do
            grep -Fq "$value" "$QOS_SESSION_TEST_LOG" 2>/dev/null && return 0
            sleep 0.02
        done
        grep -Fqx "$value" "$QOS_SESSION_TEST_LOG"
    }

    export XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_TYPE=wayland
    dbus-update-activation-environment \
        QOS_SESSION_TEST_LOG XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
    probe Initial
    await_probe 'dbus-activation:GNOME:wayland'

    "$root/qos-session" &
    wrapper_pid=$!
    for _ in $(seq 1 100); do
        grep -Fq 'qos-shell:nirios:wayland' "$QOS_SESSION_TEST_LOG" 2>/dev/null && break
        sleep 0.02
    done
    grep -Fqx 'qos-shell:nirios:wayland' "$QOS_SESSION_TEST_LOG"
    probe Qos
    await_probe 'dbus-activation:nirios:wayland'

    kill -TERM "$wrapper_pid"
    wait "$wrapper_pid" || [[ $? == 143 ]]
    probe Empty
    await_probe 'dbus-activation::'

    export XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_TYPE=wayland
    dbus-update-activation-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
    probe Gnome
    await_probe 'dbus-activation:GNOME:wayland'
}

if [[ ${1:-} == --private-bus ]]; then
    private_bus "$2" "$3"
    exit 0
fi

if ! XDG_DATA_HOME="$work/data" dbus-run-session -- "$0" --private-bus "$work" "$repo_root" \
    2>"$work/dbus.stderr"; then
    sed -n '1,160p' "$work/dbus.stderr" >&2
    exit 1
fi
echo 'private D-Bus handoff: PASS'
