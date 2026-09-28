#!/usr/bin/env bash
# Exercise the host wrapper against a real, private systemd --user manager and
# session bus. It never contacts the caller's user manager or session bus.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
. "$repo_root/tools/desktop-isolation.sh"
# The private manager below re-enters this script with its own bus.
if [[ ${1:-} != --private-manager ]]; then
    tilo_test_isolate "$@"
fi

for command in dbus-daemon dbus-send dbus-update-activation-environment \
               systemctl; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "private user-manager handoff: SKIP ($command is unavailable)"
        exit 0
    fi
done
systemd_bin=/usr/lib/systemd/systemd
if [[ ! -x $systemd_bin ]]; then
    echo "private user-manager handoff: SKIP ($systemd_bin is unavailable)"
    exit 0
fi

work=$(mktemp -d)
private_dbus_pid=
cleanup() {
    # A standalone systemd manager creates mode-000 inaccessible placeholders
    # below its private runtime dir; make this known temporary tree removable.
    if [[ -n $private_dbus_pid ]]; then
        kill "$private_dbus_pid" 2>/dev/null || true
        wait "$private_dbus_pid" 2>/dev/null || true
        private_dbus_pid=
    fi
    chmod -R u+w -- "$work" 2>/dev/null || true
    rm -rf -- "$work"
}
trap cleanup EXIT
private_manager_pid=

manager_cleanup() {
    if [[ -n $private_manager_pid ]]; then
        kill "$private_manager_pid" 2>/dev/null || true
        sleep 0.05
        kill -KILL "$private_manager_pid" 2>/dev/null || true
        wait "$private_manager_pid" 2>/dev/null || true
        private_manager_pid=
    fi
}

private_manager() {
    local root=$1 repo=$2
    local wrapper_pid

    # systemctl normally prefers DBUS_SESSION_BUS_ADDRESS. Pointing it at the
    # private manager's own control socket instead avoids activating the real
    # user's org.freedesktop.systemd1 service on this private test bus.
    managerctl() {
        env -u DBUS_SESSION_BUS_ADDRESS /usr/bin/systemctl --user "$@"
    }

    export HOME="$root/home"
    export XDG_CONFIG_HOME="$root/config"
    export XDG_DATA_HOME="$root/data"
    export XDG_STATE_HOME="$root/state"
    export XDG_RUNTIME_DIR="$root/runtime"
    export TILO_SESSION_TEST_LOG="$root/log"
    mkdir -p "$HOME" "$XDG_CONFIG_HOME/systemd/user/graphical-session.target.wants" \
        "$XDG_DATA_HOME/dbus-1/services" "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR"
    printf '%s\n' '#!/bin/sh' 'exit 0' >"$root/loginctl"
    chmod 0755 "$root/loginctl"
    chmod 0700 "$XDG_RUNTIME_DIR"

    # Keep the production Tilo target intact. The two standard targets and the
    # GNOME portal fixture are local replacements so the private manager has
    # no relationship to the desktop that launched this test.
    install -m 0644 "$repo/os/usr/lib/systemd/user/tilo-graphical-session.target" \
        "$XDG_CONFIG_HOME/systemd/user/tilo-graphical-session.target"
    printf '%s\n' \
        '[Unit]' \
        'Description=private graphical-session-pre target' \
        >"$XDG_CONFIG_HOME/systemd/user/graphical-session-pre.target"
    printf '%s\n' \
        '[Unit]' \
        'Description=private graphical-session target' \
        'Wants=gnome-portal.service' \
        'After=graphical-session-pre.target' \
        >"$XDG_CONFIG_HOME/systemd/user/graphical-session.target"
    printf '%s\n' \
        '[Unit]' \
        'Description=private GNOME portal fixture' \
        'PartOf=graphical-session.target' \
        'ConditionEnvironment=XDG_CURRENT_DESKTOP=GNOME' \
        '' \
        '[Service]' \
        'Type=oneshot' \
        'RemainAfterExit=yes' \
        "ExecStart=$root/record-event gnome-portal-start" \
        "ExecStop=$root/record-event gnome-portal-stop" \
        >"$XDG_CONFIG_HOME/systemd/user/gnome-portal.service"
    ln -s ../gnome-portal.service \
        "$XDG_CONFIG_HOME/systemd/user/graphical-session.target.wants/gnome-portal.service"

    printf '%s\n' \
        '#!/bin/sh' \
        'if [ "$1" = gnome-portal-stop ]; then' \
        '    printf "%s:%s:%s\\n" "$1" "$XDG_CURRENT_DESKTOP" "$XDG_SESSION_TYPE" >>"$TILO_SESSION_TEST_LOG"' \
        'else' \
        '    printf "%s\\n" "$1" >>"$TILO_SESSION_TEST_LOG"' \
        'fi' \
        >"$root/record-event"
    printf '%s\n' \
        '#!/bin/sh' \
        'printf "tilo-portal-start:%s:%s\\n" "$XDG_CURRENT_DESKTOP" "$XDG_SESSION_TYPE" >>"$TILO_SESSION_TEST_LOG"' \
        'trap '\''printf "tilo-portal-stop\\n" >>"$TILO_SESSION_TEST_LOG"; exit 0'\'' HUP INT TERM' \
        'while :; do sleep 1; done' \
        >"$root/tilo-shell"
    printf '%s\n' '#!/bin/sh' 'exec env -u DBUS_SESSION_BUS_ADDRESS /usr/bin/systemctl "$@"' >"$root/systemctl"
    printf '%s\n' \
        '#!/bin/sh' \
        'printf "dbus-activation:%s:%s\\n" "$XDG_CURRENT_DESKTOP" "$XDG_SESSION_TYPE" >>"$TILO_SESSION_TEST_LOG"' \
        >"$root/dbus-probe"
    chmod 0755 "$root/record-event" "$root/tilo-shell" "$root/systemctl" "$root/dbus-probe"
    for probe_name in Tilo Empty Gnome; do
        printf '[D-BUS Service]\nName=org.tilo.SessionProbe%s\nExec=%s\n' \
            "$probe_name" "$root/dbus-probe" \
            >"$XDG_DATA_HOME/dbus-1/services/org.tilo.SessionProbe${probe_name}.service"
    done

    # Stage the runner from the tree under test too; an installed host runner
    # would talk to the caller's real user manager.
    tilo_stage_script "$repo/dist/session/tilo-session-runner" "$root/tilo-session-runner" \
        SYSTEMCTL="$root/systemctl" \
        LOGINCTL="$root/loginctl" \
        FLIGHT_RECORDER="$repo/dist/session/tilo-flight-recorder.py" || return 1
    tilo_stage_script "$repo/dist/session/tilo-session" "$root/tilo-session" \
        TILO_SHELL="$root/tilo-shell" \
        SESSION_RUNNER="$root/tilo-session-runner" || return 1

    "$systemd_bin" --user --unit=basic.target >"$root/systemd.stdout" 2>"$root/systemd.stderr" &
    private_manager_pid=$!
    trap 'manager_cleanup; cleanup' EXIT
    for _ in $(seq 1 60); do
        [[ -S $XDG_RUNTIME_DIR/systemd/private ]] && \
            managerctl show-environment >/dev/null 2>&1 && break
        sleep 0.05
    done
    if [[ ! -S $XDG_RUNTIME_DIR/systemd/private ]]; then
        echo 'private user-manager handoff: SKIP (manager did not expose a private control socket)'
        return 0
    fi
    managerctl show-environment >/dev/null
    if ! dbus-send --session --print-reply --dest=org.freedesktop.DBus \
        /org/freedesktop/DBus org.freedesktop.DBus.ListNames 2>/dev/null | \
        grep -Fq 'org.freedesktop.systemd1'; then
        echo 'private user-manager handoff: SKIP (manager did not register on the private bus)'
        return 0
    fi

    # Model the end of a normal GNOME login. Tilo must neither keep its portal
    # fixture nor retain GNOME's identity in the user manager.
    export XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_TYPE=wayland DISPLAY=:99
    dbus-update-activation-environment --systemd \
        TILO_SESSION_TEST_LOG XDG_CURRENT_DESKTOP XDG_SESSION_TYPE 2>/dev/null || true
    managerctl import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE DISPLAY
    managerctl start graphical-session.target
    managerctl is-active --quiet gnome-portal.service

    # Tilo is a real child process here. Deliberately leave GNOME's standard
    # graphical target and portal fixture active to model a killed wrapper or
    # incomplete prior logout: the Tilo handoff must stop that stale lifecycle
    # before publishing its own desktop identity and starting its target.
    "$root/tilo-session" &
    wrapper_pid=$!
    for _ in $(seq 1 100); do
        grep -Fq 'tilo-portal-start:tilo:wayland' "$TILO_SESSION_TEST_LOG" 2>/dev/null && break
        sleep 0.05
    done
    grep -Fqx 'tilo-portal-start:tilo:wayland' "$TILO_SESSION_TEST_LOG"
    managerctl is-active --quiet tilo-graphical-session.target
    ! managerctl is-active --quiet gnome-portal.service
    # The stop hook must still see GNOME's identity, proving Tilo did not
    # change the shared user-manager environment before tearing it down.
    grep -Fqx 'gnome-portal-stop:GNOME:wayland' "$TILO_SESSION_TEST_LOG"
    gnome_stop_line=$(grep -n -m1 -F 'gnome-portal-stop' "$TILO_SESSION_TEST_LOG" | cut -d: -f1)
    tilo_start_line=$(grep -n -m1 -F 'tilo-portal-start:tilo:wayland' "$TILO_SESSION_TEST_LOG" | cut -d: -f1)
    [[ $gnome_stop_line -lt $tilo_start_line ]]
    managerctl show-environment | grep -Fqx 'XDG_CURRENT_DESKTOP=tilo'
    managerctl show-environment | grep -Fqx 'XDG_SESSION_TYPE=wayland'
    ! managerctl show-environment | grep -Fq 'DISPLAY='

    # This is classic D-Bus (not systemd) activation, the path used by GCR's
    # SystemPrompter service. It observes Tilo's desktop identity.
    dbus-send --session --dest=org.tilo.SessionProbeTilo /org/tilo/SessionProbe \
        org.tilo.SessionProbe.Ping >/dev/null 2>&1 || true
    for _ in $(seq 1 100); do
        grep -Fq 'dbus-activation:tilo:wayland' "$TILO_SESSION_TEST_LOG" 2>/dev/null && break
        sleep 0.05
    done
    grep -Fqx 'dbus-activation:tilo:wayland' "$TILO_SESSION_TEST_LOG"

    kill -TERM "$wrapper_pid"
    wait "$wrapper_pid" || [[ $? == 143 ]]
    grep -Fqx 'tilo-portal-stop' "$TILO_SESSION_TEST_LOG"
    ! managerctl is-active --quiet tilo-graphical-session.target
    ! managerctl is-active --quiet graphical-session.target
    ! managerctl show-environment | grep -Fq '^XDG_CURRENT_DESKTOP='
    ! managerctl show-environment | grep -Fq '^XDG_SESSION_TYPE='

    # D-Bus cannot delete variables, so cleanup writes empty values rather
    # than allowing a request in the handoff window to select Tilo again.
    dbus-send --session --dest=org.tilo.SessionProbeEmpty /org/tilo/SessionProbe \
        org.tilo.SessionProbe.Ping >/dev/null 2>&1 || true
    for _ in $(seq 1 100); do
        grep -Fq 'dbus-activation::' "$TILO_SESSION_TEST_LOG" 2>/dev/null && break
        sleep 0.05
    done
    grep -Fqx 'dbus-activation::' "$TILO_SESSION_TEST_LOG"

    # GNOME's ordinary login update and graphical target activation recover
    # its backend without Tilo touching its units, apps, or Flatpak settings.
    export XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_TYPE=wayland
    dbus-update-activation-environment --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE 2>/dev/null || true
    managerctl import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
    managerctl start graphical-session.target
    managerctl is-active --quiet gnome-portal.service
    managerctl show-environment | grep -Fqx 'XDG_CURRENT_DESKTOP=GNOME'
    dbus-send --session --dest=org.tilo.SessionProbeGnome /org/tilo/SessionProbe \
        org.tilo.SessionProbe.Ping >/dev/null 2>&1 || true
    for _ in $(seq 1 100); do
        grep -Fq 'dbus-activation:GNOME:wayland' "$TILO_SESSION_TEST_LOG" 2>/dev/null && break
        sleep 0.05
    done
    grep -Fqx 'dbus-activation:GNOME:wayland' "$TILO_SESSION_TEST_LOG"
    managerctl stop graphical-session.target
    echo 'private user-manager handoff: PASS'
}

if [[ ${1:-} == --private-manager ]]; then
    private_manager "$2" "$3"
    exit 0
fi

# systemd --user discovers the user bus at $XDG_RUNTIME_DIR/bus. dbus-run-
# session uses an unrelated /tmp socket, so create the bus at the private
# runtime path the production user manager uses.
mkdir -p "$work/runtime"
chmod 0700 "$work/runtime"
export XDG_RUNTIME_DIR="$work/runtime"
export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
private_dbus_pid=$(dbus-daemon --session --address="$DBUS_SESSION_BUS_ADDRESS" --fork --print-pid)
"$0" --private-manager "$work" "$repo_root"
