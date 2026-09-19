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

for file in /usr/bin/tilo-shell /usr/bin/tilo-launcher \
            "$libexec_dir/tilo-unlock-helper" "$libexec_dir/tilo-session" \
            "$libexec_dir/tilo-session-runner" \
            "$libexec_dir/tilo-flight-recorder"; do
    [[ -x $(path "$file") ]] || { echo "missing executable: $file" >&2; exit 1; }
done
for file in /etc/pam.d/tilo-unlock \
            /usr/share/wayland-sessions/tilo.desktop \
            /usr/share/xdg-desktop-portal/portals/tilo.portal \
            /usr/share/xdg-desktop-portal/tilo-portals.conf \
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
# The compositor owns the portal bus name in-process. A D-Bus activator or
# separate user unit would race it and serve an empty workspace.
[[ ! -e $(path /usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.tilo.service) ]]
[[ ! -e $(path /usr/lib/systemd/user/tilo-portal.service) ]]

for elf in /usr/bin/tilo-shell /usr/bin/tilo-launcher "$libexec_dir/tilo-unlock-helper"; do
    elf_description=$(file "$(path "$elf")")
    case ${TILO_EXPECT_ARCH:-} in
        amd64) grep -Eq 'ELF 64-bit.*x86-64' <<<"$elf_description" ;;
        arm64) grep -Eq 'ELF 64-bit.*ARM aarch64' <<<"$elf_description" ;;
        "") grep -Eq 'ELF 64-bit.*(x86-64|ARM aarch64)' <<<"$elf_description" ;;
        *) echo "unsupported TILO_EXPECT_ARCH: $TILO_EXPECT_ARCH" >&2; exit 2 ;;
    esac
    if ldd "$(path "$elf")" | tee /dev/stderr | grep -q 'not found'; then
        echo "unresolved shared library in $elf" >&2
        exit 1
    fi
done
