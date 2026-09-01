#!/usr/bin/env bash
# Verify the installed Nix result without assuming an FHS /usr prefix.
set -euo pipefail

[[ $# == 2 ]] || { echo "usage: $0 RESULT amd64|arm64" >&2; exit 2; }
result=$(realpath "$1")
arch=$2
case "$arch" in amd64|arm64) ;; *) exit 2 ;; esac
case "$(uname -m)" in
x86_64) execution_arch=amd64 ;;
aarch64) execution_arch=arm64 ;;
*) echo "unsupported execution architecture: $(uname -m)" >&2; exit 1 ;;
esac

for file in bin/qos-shell bin/qos-launcher libexec/qos-unlock-helper libexec/qos-session; do
    [[ -x $result/$file ]] || { echo "missing executable: $file" >&2; exit 1; }
done
for file in etc/pam.d/qos-unlock share/wayland-sessions/qos.desktop \
            share/xdg-desktop-portal/portals/qos.portal \
            share/licenses/qos-session/LICENSE; do
    [[ -f $result/$file ]] || { echo "missing file: $file" >&2; exit 1; }
done

entry=$result/share/wayland-sessions/qos.desktop
portal=$result/share/xdg-desktop-portal/portals/qos.portal
wrapper=$result/libexec/qos-session
grep -Fqx "Exec=$result/libexec/qos-session" "$entry"
grep -Fqx "TryExec=$result/bin/qos-shell" "$entry"
grep -q '^DesktopNames=nirios$' "$entry"
grep -q '^UseIn=nirios$' "$portal"
grep -Fqx "QOS_SHELL=$result/bin/qos-shell" "$wrapper"
grep -q -- '--firstboot' "$wrapper"

launcher_elf=
ldd_command=(ldd)
if [[ $execution_arch == "$arch" ]] && ! command -v ldd >/dev/null 2>&1; then
    # The minimal nixos/nix image omits glibc's ldd script. Run the canonical
    # Nixpkgs tool without mutating the image profile; a failed tool lookup or
    # invocation must fail verification instead of looking like clean output.
    ldd_command=(nix shell nixpkgs#glibc.bin --command ldd)
fi
for installed in bin/qos-shell bin/qos-launcher libexec/qos-unlock-helper; do
    directory=$(dirname "$installed")
    basename=$(basename "$installed")
    elf=$result/$installed
    if [[ -x $result/$directory/.$basename-wrapped ]]; then
        elf=$result/$directory/.$basename-wrapped
    fi
    [[ $installed != bin/qos-launcher ]] || launcher_elf=$elf
    # e_ident[EI_CLASS] is byte 4 (2 = ELF64), and little-endian e_machine is
    # the 16-bit word at byte 18 (62 = x86-64, 183 = AArch64). Using POSIX
    # tooling already present in the Nix image avoids mutating its emulated
    # user profile merely to install `file` before the real build starts.
    class=$(od -An -t u1 -j 4 -N 1 "$elf" | tr -d ' ')
    machine=$(od -An -t u2 -j 18 -N 2 "$elf" | tr -d ' ')
    [[ $class == 2 ]] || { echo "not a 64-bit ELF: $installed" >&2; exit 1; }
    if [[ $arch == amd64 ]]; then
        [[ $machine == 62 ]]
    else
        [[ $machine == 183 ]]
    fi
    if [[ $execution_arch == "$arch" ]]; then
        ldd_output=$("${ldd_command[@]}" "$elf")
        printf '%s\n' "$ldd_output" >&2
        if grep -q 'not found' <<<"$ldd_output"; then
            echo "unresolved shared library in $installed" >&2
            exit 1
        fi
    fi
done

# The cross target still executes a harmless command through qemu, proving
# that its Nix-store interpreter and runtime library closure resolve. This is
# stronger than accepting a correctly labelled but unloadable ELF.
if [[ $execution_arch != "$arch" ]]; then
    [[ -x /qemu-aarch64 ]]
    /qemu-aarch64 "$launcher_elf" --help >/dev/null
fi

# Cross-built binaries cannot be executed by host `ldd`. Verify the output's
# content hash and require Nix to resolve its complete registered closure; the
# native architecture job retains the executable ldd check above.
if command -v nix-store >/dev/null 2>&1; then
    nix-store --verify-path "$result"
    nix-store --query --requisites "$result" >/dev/null
fi
