#!/bin/bash
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
verifier="$repo/os/dev/verify-authenticode-envelope.py"

python3 - "$work" <<'PY'
import pathlib, struct, sys
out = pathlib.Path(sys.argv[1])
pe = 0x80
optional = pe + 24
optional_size = 0xf0
section = optional + optional_size
data = bytearray(0x213)
data[:2] = b"MZ"
struct.pack_into("<I", data, 0x3c, pe)
data[pe:pe+4] = b"PE\0\0"
struct.pack_into("<HHIIIHH", data, pe + 4, 0x8664, 1, 0, 0, 0,
                 optional_size, 0x2022)
struct.pack_into("<H", data, optional, 0x20b)
struct.pack_into("<I", data, optional + 16, 0x1000)  # AddressOfEntryPoint
struct.pack_into("<H", data, optional + 68, 10)      # Subsystem
struct.pack_into("<I", data, optional + 108, 16)    # NumberOfRvaAndSizes
data[section:section+8] = b".text\0\0\0"
struct.pack_into("<IIII", data, section + 8, 16, 0x1000, 16, 0x200)
struct.pack_into("<I", data, section + 36, 0x60000020)
data[0x200:0x210] = bytes(range(16))
data[0x210:0x213] = b"end"
(out / "original.efi").write_bytes(data)

signed = bytearray(data)
struct.pack_into("<I", signed, optional + 64, 0x12345678)  # Checksum
padding = (-len(signed)) % 8
signed += b"\0" * padding
cert_offset = len(signed)
certificate = struct.pack("<IHH4s", 12, 0x0200, 2, b"cert") + b"\0" * 4
signed += certificate
struct.pack_into("<II", signed, optional + 112 + 8 * 4,
                 cert_offset, len(certificate))
(out / "signed.efi").write_bytes(signed)

mutations = {
    "entrypoint": (optional + 16, 0x2000, "I"),
    "machine": (pe + 4, 0xaa64, "H"),
    "subsystem": (optional + 68, 3, "H"),
    "section-header": (section + 36, 0xe0000020, "I"),
}
for name, (offset, value, kind) in mutations.items():
    candidate = bytearray(signed)
    struct.pack_into("<" + kind, candidate, offset, value)
    (out / f"{name}.efi").write_bytes(candidate)
PY

"$verifier" "$work/original.efi" "$work/signed.efi"
echo 'ok: checksum, Security Directory, alignment, and terminal certificate are accepted'

for mutation in entrypoint machine subsystem section-header; do
  if "$verifier" "$work/original.efi" "$work/$mutation.efi" 2>/dev/null; then
    echo "PE header mutation passed Authenticode-envelope verification: $mutation" >&2
    exit 1
  fi
done
echo 'ok: entry point, machine, subsystem, and section-header mutations are rejected'
