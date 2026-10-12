#!/usr/bin/env bash
# Observe native capture assembly prerequisites before invoking the C probe.
set -euo pipefail
missing=0
for tool in rustc pkg-config ldd readelf python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'capture assembly requires %s on the pinned native builder PATH\n' "$tool" >&2
        missing=1
    fi
done
compiler=${CC:-cc}
if ! command -v "$compiler" >/dev/null 2>&1; then
    printf 'capture assembly requires the C compiler %s\n' "$compiler" >&2
    missing=1
fi
for executable in /usr/bin/bwrap /usr/bin/tar /usr/bin/ostree; do
    if [ ! -f "$executable" ] || [ -L "$executable" ] || [ ! -x "$executable" ]; then
        printf 'capture assembly requires the native executable %s\n' "$executable" >&2
        missing=1
    fi
done
if command -v pkg-config >/dev/null 2>&1 && ! pkg-config --exists gpgme; then
    printf 'capture assembly requires GPGME development files: install gpgme-devel (Fedora) or libgpgme-dev (Debian/Ubuntu) in this native builder\n' >&2
    missing=1
fi
[ "$missing" = 0 ] || exit 1
rust_libdir=$(rustc --print target-libdir)
[ -d "$rust_libdir" ] && [ -n "$(find "$rust_libdir" -maxdepth 1 -name 'libstd-*.so' -print -quit)" ] \
    || { echo 'capture assembly requires the pinned Rust native shared standard library' >&2; exit 1; }
# A .pc file alone cannot prove the headers and linkable library are present.
# This tiny compiler check is actual native evidence, not a version declaration.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
read -r -a cflags <<< "$(pkg-config --cflags gpgme)"
read -r -a libs <<< "$(pkg-config --libs gpgme)"
printf '#include <gpgme.h>\nint main(void) { return gpgme_check_version(0) == 0; }\n' \
    | "$compiler" -Wall -Wextra -Werror "${cflags[@]}" -x c - "${libs[@]}" -o "$work/gpgme-check" \
    || { echo 'capture assembly cannot compile/link GPGME headers and library in this native builder' >&2; exit 1; }
"$work/gpgme-check" \
    || { echo 'capture assembly cannot load the native GPGME library' >&2; exit 1; }
