#!/usr/bin/env bash
# Session lifecycle fixtures may stop graphical targets, rewrite activation
# environments, and exercise logout channels.  They are never safe merely
# because an individual child intends to create a private bus: one missed
# environment override can reach the caller's real desktop.
set -euo pipefail

if [[ ${TILO_DISPOSABLE_TEST_ENV:-0} != 1 ]]; then
    echo 'refusing session-affecting tests outside an explicitly disposable environment' >&2
    exit 64
fi

if [[ ! -e /.dockerenv && ! -e /run/.containerenv ]]; then
    echo 'refusing session-affecting tests outside a container or disposable VM' >&2
    exit 64
fi

for variable in DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS XAUTHORITY \
    XDG_CURRENT_DESKTOP; do
    if [[ -n ${!variable:-} ]]; then
        echo "refusing session-affecting tests while $variable is set" >&2
        exit 64
    fi
done

if [[ -n ${XDG_SESSION_ID:-} || -n ${DESKTOP_SESSION:-} || -n ${GDMSESSION:-} ]]; then
    echo 'refusing session-affecting tests from a login-manager session' >&2
    exit 64
fi
