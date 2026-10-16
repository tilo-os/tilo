#!/usr/bin/env bash
# Run what of the Windows imager can run without Windows, under Wine (x86_64
# only: Fedora's Wine runs x86 programs only).
#
#   wine-test.sh units <imager-tests.exe>     the imager's unit tests
#   wine-test.sh install <package-dir>        install the package silently, check what it
#                                             installed, uninstall
#
# Wine is not Windows: the disk, volume and privilege APIs the media writer
# uses are partial or missing there, so tests that need them are listed in
# tools/ci/windows/wine-skip.txt with the reason, and run on real Windows
# (docs/publishing-on-t.md, Windows). Everything else must pass.
set -euo pipefail

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
mode="${1:?units or install}"
# On disk, not in TMPDIR: Wine reports a tmpfs drive as a virtual disk, which
# the unit tests' storage refuses as Windows would.
root=""
for candidate in ${TILO_WINE_ROOT:+"$TILO_WINE_ROOT"} /target/wine /var/tmp/tilo-wine "${HOME:-/nonexistent}/.cache/tilo-wine"; do
  mkdir -p "$candidate" 2>/dev/null || continue
  type="$(stat -f -c %t "$candidate")"
  printf 'wine prefix root %s: filesystem 0x%s\n' "$candidate" "$type"
  if [ "$type" != 1021994 ]; then root="$candidate"; break; fi
done
[ -n "$root" ] || { printf 'no disk-backed directory for the Wine prefix (all tmpfs)\n' >&2; exit 1; }
prefix="$(mktemp -d "$root/prefix.XXXXXX")"
trap 'wineserver -k >/dev/null 2>&1 || true; rm -rf -- "$prefix"' EXIT
# winedbg off: a crashing program ends instead of waiting for a debugger.
export WINEPREFIX="$prefix" WINEDEBUG="${WINEDEBUG:--all}" WINEDLLOVERRIDES="mscoree,mshtml=;winedbg.exe=d"
bash "$here/wine-prefix.sh"

case "$mode" in
  units)
    tests="${2:?the imager-tests.exe to run}"
    skips=()
    while IFS=$'\t' read -r name _; do
      case "$name" in ''|'#'*) continue ;; esac
      skips+=(--skip "$name")
    done < "$here/wine-skip.txt"
    # One test at a time and uncaptured, so each failure's message follows its
    # name and a test that hangs or crashes Wine is the last one started; a
    # hard limit so a hang fails instead of holding the node.
    # The stack the workspace gives tests (.cargo/config.toml [env]), which
    # cargo would set if it ran them.
    stack="$(sed -n 's/^RUST_MIN_STACK = { value = "\([0-9]*\)".*/\1/p' "$here/../../../.cargo/config.toml")"
    RUST_MIN_STACK="${stack:?no RUST_MIN_STACK in .cargo/config.toml}" \
      timeout --kill-after=30 "${TILO_WINE_TEST_TIMEOUT:-1800}" \
      wine "$tests" --test-threads 1 --nocapture "${skips[@]}"
    ;;
  install)
    package="${2:?package directory}"
    arch=x86_64
    setup="$package/tilo-imager-setup-windows-$arch.exe"
    (cd "$package" && sha256sum --check --strict "$(basename -- "$setup").sha256")
    install='C:\tilo-imager'
    dir="$WINEPREFIX/drive_c/tilo-imager"
    wine "$setup" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART "/DIR=$install" "/LOG=C:\\setup.log" \
      || { cat "$WINEPREFIX/drive_c/setup.log" >&2; exit 1; }
    wineserver -w
    for file in tilo-imager.exe README.md Start-Tilo-Imager.ps1 unins000.exe; do
      [ -s "$dir/$file" ] || { printf 'the installer did not install %s\n' "$file" >&2; exit 1; }
    done
    cmp "$dir/tilo-imager.exe" "$package/tilo-imager-windows-$arch.exe"
    # The start menu entry the installer created.
    find "$WINEPREFIX/drive_c" -path '*Start Menu*' -name 'Tilo Installer.lnk' | grep -q . \
      || { printf 'no start menu shortcut\n' >&2; exit 1; }
    # The installed imager is not started here: its storage (tilo-files)
    # publishes only to volumes with positive local-disk evidence, which
    # Wine's volumes do not give, so rendering its UI is left to Windows
    # (docs/publishing-on-t.md, Windows).
    wine "$dir/unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
    wineserver -w
    [ ! -e "$dir/tilo-imager.exe" ] || { printf 'uninstalling left the program behind\n' >&2; exit 1; }
    printf 'installed and uninstalled %s under Wine\n' "$(basename -- "$setup")"
    ;;
  *) printf 'unknown mode: %s\n' "$mode" >&2; exit 2 ;;
esac
