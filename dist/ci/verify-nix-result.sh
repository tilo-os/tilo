#!/usr/bin/env bash
# Verify the installed Nix result without assuming an FHS /usr prefix.
set -euo pipefail

[[ $# == 2 ]] || { echo "usage: $0 RESULT amd64|arm64" >&2; exit 2; }
result=$(realpath "$1")
arch=$2
case "$arch" in amd64|arm64) ;; *) exit 2 ;; esac

for file in bin/tilo-shell bin/tilo-launcher bin/tilo-import-folders libexec/tilo-unlock-helper \
            libexec/tilo-session libexec/tilo-session-runner \
            libexec/tilo-flight-recorder libexec/tilo-session-report; do
    [[ -x $result/$file ]] || { echo "missing executable: $file" >&2; exit 1; }
done
for file in share/applications/tilo-import-folders.desktop etc/pam.d/tilo-unlock share/wayland-sessions/tilo.desktop \
            share/xdg-desktop-portal/portals/tilo.portal \
            share/xdg-desktop-portal/tilo-portals.conf \
            share/pipewire/tilo-echo-cancel.conf \
            share/systemd/user/tilo-graphical-session.target \
            share/licenses/tilo-session/LICENSE; do
    [[ -f $result/$file ]] || { echo "missing file: $file" >&2; exit 1; }
done

entry=$result/share/wayland-sessions/tilo.desktop
portal=$result/share/xdg-desktop-portal/portals/tilo.portal
wrapper=$result/libexec/tilo-session
runner=$result/libexec/tilo-session-runner
grep -Fqx "Exec=$result/libexec/tilo-session" "$entry"
grep -Fqx "TryExec=$result/bin/tilo-shell" "$entry"
grep -q '^DesktopNames=tilo$' "$entry"
grep -q '^UseIn=tilo$' "$portal"
grep -q '^default=tilo$' "$result/share/xdg-desktop-portal/tilo-portals.conf"
grep -q 'libpipewire-module-echo-cancel' "$result/share/pipewire/tilo-echo-cancel.conf"
grep -Fqx "TILO_SHELL=$result/bin/tilo-shell" "$wrapper"
grep -Fqx "SESSION_RUNNER=$result/libexec/tilo-session-runner" "$wrapper"
grep -Eq '^SYSTEMD_CAT=/nix/store/.+/bin/systemd-cat$' "$runner"
grep -Eq '^DBUS_UPDATE=/nix/store/.+/bin/dbus-update-activation-environment$' "$runner"
grep -Fqx "FLIGHT_RECORDER=$result/libexec/tilo-flight-recorder" "$runner"
grep -Fq -- '--identifier=tilo-session --level-prefix=false' "$runner"
grep -q -- '--firstboot' "$runner"
grep -Fqx 'Requires=graphical-session-pre.target graphical-session.target' \
    "$result/share/systemd/user/tilo-graphical-session.target"

ldd_command=(ldd)
if ! command -v ldd >/dev/null 2>&1; then
    # The minimal nixos/nix image omits glibc's ldd script. Run the canonical
    # Nixpkgs tool without mutating the image profile; a failed tool lookup or
    # invocation must fail verification instead of looking like clean output.
    ldd_command=(nix shell nixpkgs#glibc.bin --command ldd)
fi
grep -Fxq "Exec=$result/bin/tilo-import-folders --import-folders" "$result/share/applications/tilo-import-folders.desktop"
grep -Fxq "TryExec=$result/bin/tilo-import-folders" "$result/share/applications/tilo-import-folders.desktop"
"$result/bin/tilo-import-folders" --tilo-package-smoke
for installed in bin/tilo-shell bin/tilo-launcher bin/tilo-import-folders libexec/tilo-unlock-helper; do
    directory=$(dirname "$installed")
    basename=$(basename "$installed")
    elf=$result/$installed
    if [[ -x $result/$directory/.$basename-wrapped ]]; then
        elf=$result/$directory/.$basename-wrapped
    fi
    # e_ident[EI_CLASS] is byte 4 (2 = ELF64), and little-endian e_machine is
    # the 16-bit word at byte 18 (62 = x86-64, 183 = AArch64). Using POSIX
    # tooling already present in the Nix image avoids mutating its emulated
    # user profile merely to install `file` before the build starts.
    class=$(od -An -t u1 -j 4 -N 1 "$elf" | tr -d ' ')
    machine=$(od -An -t u2 -j 18 -N 2 "$elf" | tr -d ' ')
    [[ $class == 2 ]] || { echo "not a 64-bit ELF: $installed" >&2; exit 1; }
    if [[ $arch == amd64 ]]; then
        [[ $machine == 62 ]]
    else
        [[ $machine == 183 ]]
    fi
    ldd_output=$("${ldd_command[@]}" "$elf")
    printf '%s\n' "$ldd_output" >&2
    if grep -q 'not found' <<<"$ldd_output"; then
        echo "unresolved shared library in $installed" >&2
        exit 1
    fi
done

# Verify the output's content hash and require Nix to resolve its complete
# registered closure in addition to the executable shared-library checks.
if command -v nix-store >/dev/null 2>&1; then
    nix-store --verify-path "$result"
    nix-store --query --requisites "$result" >/dev/null
fi
