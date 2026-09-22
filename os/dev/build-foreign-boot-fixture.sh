#!/usr/bin/env bash
# Build one architecture-matched controlled EFI application for the graphical
# alongside-install boot-selection proof. Nothing produced here ships to users.
set -euo pipefail

dev_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source_file="$dev_dir/../tests/foreign_boot_fixture.c"
arch="${1:?usage: build-foreign-boot-fixture.sh ARCH windows|linux OUTPUT}"
kind="${2:-}"
output="${3:-}"
clang_bin="${TILO_EFI_FIXTURE_CLANG:-clang}"
linker="${TILO_EFI_FIXTURE_LINKER:-ld.lld}"

case "$arch" in
  x86_64|amd64) target=x86_64-pc-win32-coff; machine=0x8664 ;;
  aarch64|arm64) target=aarch64-pc-win32-coff; machine=0xaa64 ;;
  *) printf 'unsupported EFI fixture architecture: %s\n' "$arch" >&2; exit 2 ;;
esac
case "$kind" in
  windows) marker='TILO FOREIGN WINDOWS BOOT PASS' ;;
  linux) marker='TILO FOREIGN LINUX BOOT PASS' ;;
  *) printf 'fixture kind must be windows or linux\n' >&2; exit 2 ;;
esac
[ -n "$output" ] || { printf 'EFI fixture output is required\n' >&2; exit 2; }
case "$output" in .|..|*/.|*/..|*/) printf 'EFI fixture output must name a file\n' >&2; exit 2;; esac
[ ! -L "$output" ] && [ ! -d "$output" ] \
  || { printf 'EFI fixture output is not a plain file path\n' >&2; exit 2; }
command -v "$clang_bin" >/dev/null 2>&1 || { printf 'clang is required for the EFI fixture\n' >&2; exit 1; }
command -v "$linker" >/dev/null 2>&1 || { printf 'ld.lld is required for the EFI fixture\n' >&2; exit 1; }
[ -f "$source_file" ] && [ ! -L "$source_file" ] || { printf 'EFI fixture source is missing or unsafe\n' >&2; exit 1; }

work="$(mktemp -d "${TMPDIR:-/tmp}/tilo-foreign-efi.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT
"$clang_bin" -target "$target" -ffreestanding -fno-stack-protector \
  -fshort-wchar -Wall -Wextra -Werror \
  -DTILO_FOREIGN_BOOT_MARKER="L\"\\r\\n\\r\\n  $marker\\r\\n\\r\\n\"" \
  -c "$source_file" -o "$work/fixture.obj"
case "${linker##*/}" in
  lld-link) linker_flavor=() ;;
  *) linker_flavor=(-flavor link) ;;
esac
"$linker" "${linker_flavor[@]}" /subsystem:efi_application /entry:efi_main \
  "/out:$work/fixture.efi" "$work/fixture.obj"

# Parse the PE header ourselves so success does not depend on `file` wording or
# architecture-specific objdump support on the native release host.
python3 - "$work/fixture.efi" "$machine" "$marker" <<'PY'
import struct
import sys
from pathlib import Path

path = Path(sys.argv[1])
expected_machine = int(sys.argv[2], 16)
marker = sys.argv[3].encode("utf-16-le")
data = path.read_bytes()
if len(data) < 0x100 or data[:2] != b"MZ":
    raise SystemExit("EFI fixture is not a DOS/PE image")
pe = struct.unpack_from("<I", data, 0x3C)[0]
if data[pe:pe + 4] != b"PE\0\0":
    raise SystemExit("EFI fixture has no PE signature")
machine, sections, _, _, _, optional_size, _ = struct.unpack_from("<HHIIIHH", data, pe + 4)
optional = pe + 24
if machine != expected_machine or sections == 0:
    raise SystemExit("EFI fixture machine or section table differs")
if optional_size < 70 or struct.unpack_from("<H", data, optional)[0] != 0x20B:
    raise SystemExit("EFI fixture is not PE32+")
if struct.unpack_from("<I", data, optional + 16)[0] == 0:
    raise SystemExit("EFI fixture has no entry point")
if struct.unpack_from("<H", data, optional + 68)[0] != 10:
    raise SystemExit("EFI fixture is not an EFI application")
if marker not in data:
    raise SystemExit("EFI fixture marker is absent")
PY

install -m 0644 "$work/fixture.efi" "$output"
