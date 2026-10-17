#!/usr/bin/env bash
# Format-independent assertions run before and after package installation.
set -euo pipefail

[[ $# == 1 ]] || { echo "usage: $0 ROOT" >&2; exit 2; }
root=${1%/}
[[ -n $root ]] || root=/
path() { printf '%s%s' "${root%/}" "$1"; }
libexec_dir=${TILO_LIBEXEC_DIR:-/usr/libexec}
[[ $libexec_dir == /* && $libexec_dir != */ ]] || {
    echo "TILO_LIBEXEC_DIR must be an absolute directory" >&2
    exit 2
}

for file in /usr/bin/tilo-shell /usr/bin/tilo-launcher /usr/bin/tilo-import-folders \
            "$libexec_dir/tilo-unlock-helper" "$libexec_dir/tilo-session" \
            "$libexec_dir/tilo-session-runner" \
            "$libexec_dir/tilo-flight-recorder" \
            "$libexec_dir/tilo-session-report"; do
    [[ -x $(path "$file") ]] || { echo "missing executable: $file" >&2; exit 1; }
done
for file in /usr/share/applications/tilo-import-folders.desktop /etc/pam.d/tilo-unlock \
            /usr/share/wayland-sessions/tilo.desktop \
            /usr/share/xdg-desktop-portal/portals/tilo.portal \
            /usr/share/xdg-desktop-portal/tilo-portals.conf \
            /usr/share/pipewire/tilo-echo-cancel.conf \
            /usr/lib/systemd/user/tilo-graphical-session.target; do
    [[ -f $(path "$file") ]] || { echo "missing file: $file" >&2; exit 1; }
done
if [[ ! -f $(path /usr/share/licenses/tilo-session/LICENSE) \
      && ! -f $(path /usr/share/doc/tilo-session/copyright) ]]; then
    echo "missing Tilo license text" >&2
    exit 1
fi

grep -Fqx "Exec=$libexec_dir/tilo-session" "$(path /usr/share/wayland-sessions/tilo.desktop)"
grep -q '^TryExec=/usr/bin/tilo-shell$' "$(path /usr/share/wayland-sessions/tilo.desktop)"
grep -q '^DesktopNames=tilo$' "$(path /usr/share/wayland-sessions/tilo.desktop)"
grep -q '^UseIn=tilo$' "$(path /usr/share/xdg-desktop-portal/portals/tilo.portal)"
grep -q '^default=tilo$' "$(path /usr/share/xdg-desktop-portal/tilo-portals.conf)"
grep -q 'libpipewire-module-echo-cancel' "$(path /usr/share/pipewire/tilo-echo-cancel.conf)"
grep -q '^TILO_SHELL=/usr/bin/tilo-shell$' "$(path "$libexec_dir/tilo-session")"
grep -Fqx "SESSION_RUNNER=$libexec_dir/tilo-session-runner" "$(path "$libexec_dir/tilo-session")"
grep -q '^SYSTEMD_CAT=/usr/bin/systemd-cat$' "$(path "$libexec_dir/tilo-session-runner")"
grep -Fqx "FLIGHT_RECORDER=$libexec_dir/tilo-flight-recorder" \
    "$(path "$libexec_dir/tilo-session-runner")"
grep -Fq -- '--identifier=tilo-session --level-prefix=false' \
    "$(path "$libexec_dir/tilo-session-runner")"
grep -q -- '--firstboot' "$(path "$libexec_dir/tilo-session-runner")"
grep -Fq '"$SYSTEMCTL" --user start "$SESSION_TARGET"' "$(path "$libexec_dir/tilo-session-runner")"
grep -Fq '"$SYSTEMCTL" --user stop "$SESSION_TARGET"' "$(path "$libexec_dir/tilo-session-runner")"
grep -Fqx 'Requires=graphical-session-pre.target graphical-session.target' \
    "$(path /usr/lib/systemd/user/tilo-graphical-session.target)"
grep -Fqx 'PropagatesStopTo=graphical-session.target graphical-session-pre.target' \
    "$(path /usr/lib/systemd/user/tilo-graphical-session.target)"
grep -Fqx 'Wants=tilo-app-host.socket tilo-models.socket tilo-model-gateway.socket' \
    "$(path /usr/lib/systemd/user/tilo-graphical-session.target)"
# The compositor owns the portal bus name in-process. A D-Bus activator or
# separate user unit would race it and serve an empty workspace.
[[ ! -e $(path /usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.tilo.service) ]]
[[ ! -e $(path /usr/lib/systemd/user/tilo-portal.service) ]]

grep -Fxq "Exec=/usr/bin/tilo-import-folders --import-folders" "$(path /usr/share/applications/tilo-import-folders.desktop)"
grep -Fxq "TryExec=/usr/bin/tilo-import-folders" "$(path /usr/share/applications/tilo-import-folders.desktop)"
"$(path /usr/bin/tilo-import-folders)" --tilo-package-smoke

for elf in /usr/bin/tilo-shell /usr/bin/tilo-launcher /usr/bin/tilo-import-folders "$libexec_dir/tilo-unlock-helper" \
           "$libexec_dir/tilo-flight-recorder" "$libexec_dir/tilo-session-report"; do
    elf_description=$(LC_ALL=C file "$(path "$elf")")
    case ${TILO_EXPECT_ARCH:-} in
        amd64) grep -Eq 'ELF 64-bit.*x86-64' <<<"$elf_description" ;;
        arm64) grep -Eq 'ELF 64-bit.*ARM aarch64' <<<"$elf_description" ;;
        "") grep -Eq 'ELF 64-bit.*(x86-64|ARM aarch64)' <<<"$elf_description" ;;
        *) echo "unsupported TILO_EXPECT_ARCH: $TILO_EXPECT_ARCH" >&2; exit 2 ;;
    esac
    # Static executables have no dynamic-loader dependencies, and ldd commonly
    # exits nonzero for them. Prove that case from successfully inspected ELF
    # headers instead of treating every failed ldd invocation as static.
    if ! elf_headers=$(LC_ALL=C readelf --wide --file-header --program-headers --dynamic \
        "$(path "$elf")" 2>&1); then
        printf '%s\n' "$elf_headers" >&2
        echo "ELF dependency inspection failed for $elf" >&2
        exit 1
    fi
    if grep -Eq '^readelf: (Error|Warning):' <<<"$elf_headers" \
        || ! grep -Eq '^[[:space:]]*Type:[[:space:]]+(EXEC|DYN)[[:space:]]' <<<"$elf_headers" \
        || ! grep -Eq '^[[:space:]]*LOAD[[:space:]].*[[:space:]]E[[:space:]]' <<<"$elf_headers"; then
        echo "invalid executable ELF headers for $elf" >&2
        exit 1
    fi
    if ! grep -Eq '^[[:space:]]*INTERP[[:space:]]|\(NEEDED\)' <<<"$elf_headers"; then
        # ET_DYN also describes shared libraries; only a declared PIE is an
        # executable in this case. Static ET_EXEC needs no dynamic section.
        if grep -Eq '^[[:space:]]*Type:[[:space:]]+EXEC[[:space:]]|\(FLAGS_1\).*\bPIE\b' <<<"$elf_headers"; then
            continue
        fi
        echo "ELF is neither a static executable nor static PIE: $elf" >&2
        exit 1
    fi
    # Capture the inspector's status independently of the missing-library
    # search. A failed ldd or output pipe must never look like a clean payload.
    if ldd_output=$(LC_ALL=C ldd "$(path "$elf")" 2>&1); then
        [[ -n $ldd_output ]] || {
            echo "empty dependency inspection for $elf" >&2
            exit 1
        }
    else
        printf '%s\n' "$ldd_output" >&2
        echo "dependency inspection failed for $elf" >&2
        exit 1
    fi
    printf '%s\n' "$ldd_output" >&2
    if grep -q 'not found' <<<"$ldd_output"; then
        echo "unresolved shared library in $elf" >&2
        exit 1
    fi
done
