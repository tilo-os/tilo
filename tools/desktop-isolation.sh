# Sourced by shell tests that run session code (dist/**/tests, os/tests), so a
# test can neither reach the desktop of whoever runs it nor run what is
# installed on that host instead of the tree under test.
#
#   . "$repo/tools/desktop-isolation.sh"
#   tilo_test_isolate "$@"
#   tilo_stage_script "$repo/dist/session/tilo-session-runner" "$work/runner" \
#       SYSTEMCTL="$work/systemctl" FLIGHT_RECORDER="$repo/dist/session/tilo-flight-recorder.py"
#
# tilo_test_isolate re-runs the calling script inside
# tools/with-test-environment.sh unless it already runs in one: no display,
# a private XDG_RUNTIME_DIR, and D-Bus addresses nothing listens on, so even a
# staging mistake cannot reach the caller's session bus, user manager or
# logind. A test that needs a bus starts its own (dbus-run-session) after this.
#
# tilo_stage_script copies a packaged script, points each named command
# variable at the given path, and fails when a named variable is not assigned
# in the source or when the copy still runs an installed Tilo component or
# the host's systemctl or loginctl.

tilo_test_isolation_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

tilo_test_isolate() {
    if bash "$tilo_test_isolation_root/tools/with-test-environment.sh" --check; then
        return 0
    fi
    exec bash "$tilo_test_isolation_root/tools/with-test-environment.sh" \
        bash "$(realpath -e -- "$0")" "$@"
}

# Installed commands a staged script must never reach on a developer host.
tilo_test_host_commands='/usr/libexec/tilo|/usr/bin/tilo-|/usr/bin/shell\b|/usr/bin/systemctl|/usr/bin/loginctl'

tilo_stage_script() {
    local source=$1 dest=$2 assignment name value
    shift 2
    cp -- "$source" "$dest"
    for assignment in "$@"; do
        name=${assignment%%=*}
        value=${assignment#*=}
        if ! grep -Eq "^(readonly )?$name=" "$dest"; then
            echo "tilo_stage_script: $source assigns no $name" >&2
            return 1
        fi
        # Keep `readonly` where the source has it.
        sed -i -E "s|^(readonly )?$name=.*$|\\1$name=$value|" "$dest"
    done
    chmod 0755 "$dest"
    tilo_assert_staged "$dest"
}

tilo_assert_staged() {
    local file=$1 found
    # Comments may name the installed layout; commands may not.
    if found=$(grep -nE "$tilo_test_host_commands" "$file" | grep -vE '^[0-9]+:[[:space:]]*#'); then
        echo "staged $file still runs installed host commands:" >&2
        printf '    %s\n' "$found" >&2
        return 1
    fi
}
