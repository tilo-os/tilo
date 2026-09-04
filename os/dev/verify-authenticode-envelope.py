#!/usr/bin/python3
"""Verify that PE signing changed only the Authenticode envelope."""

import struct
import sys
from pathlib import Path


class InvalidPE(ValueError):
    pass


def u16(data: bytes, offset: int) -> int:
    if offset < 0 or offset + 2 > len(data):
        raise InvalidPE("truncated 16-bit PE field")
    return struct.unpack_from("<H", data, offset)[0]


def u32(data: bytes, offset: int) -> int:
    if offset < 0 or offset + 4 > len(data):
        raise InvalidPE("truncated 32-bit PE field")
    return struct.unpack_from("<I", data, offset)[0]


def normalized_image(path: str, require_certificate: bool) -> tuple[bytes, bool]:
    data = bytearray(Path(path).read_bytes())
    if len(data) < 64 or data[:2] != b"MZ":
        raise InvalidPE("missing DOS header")
    pe = u32(data, 0x3C)
    if pe < 64 or pe + 24 > len(data) or data[pe : pe + 4] != b"PE\0\0":
        raise InvalidPE("missing or out-of-range PE header")
    coff = pe + 4
    sections = u16(data, coff + 2)
    optional_size = u16(data, coff + 16)
    optional = coff + 20
    optional_end = optional + optional_size
    section_headers_end = optional_end + sections * 40
    if optional_end > len(data) or section_headers_end > len(data):
        raise InvalidPE("truncated optional or section headers")
    magic = u16(data, optional)
    if magic == 0x10B:
        directory_count_offset, directories = optional + 92, optional + 96
    elif magic == 0x20B:
        directory_count_offset, directories = optional + 108, optional + 112
    else:
        raise InvalidPE("unsupported PE optional-header magic")
    if optional_size < directories - optional + 8 * 5:
        raise InvalidPE("optional header has no Security Directory entry")
    if u32(data, directory_count_offset) < 5:
        raise InvalidPE("PE declares no Security Directory entry")
    checksum = optional + 64
    security = directories + 8 * 4
    certificate_offset = u32(data, security)
    certificate_size = u32(data, security + 4)
    has_certificate = certificate_offset != 0 or certificate_size != 0
    if has_certificate:
        if not certificate_offset or certificate_size < 8:
            raise InvalidPE("incomplete certificate-table reference")
        if certificate_offset % 8:
            raise InvalidPE("certificate table is not 8-byte aligned")
        if certificate_offset < section_headers_end:
            raise InvalidPE("certificate table overlaps PE headers")
        if certificate_offset + certificate_size != len(data):
            raise InvalidPE("certificate table must be the terminal PE region")
        cursor = certificate_offset
        end = len(data)
        while cursor < end:
            length = u32(data, cursor)
            if length < 8 or cursor + length > end:
                raise InvalidPE("malformed WIN_CERTIFICATE record")
            cursor += (length + 7) & ~7
        if cursor != end:
            raise InvalidPE("certificate-table size does not match its records")
    elif require_certificate:
        raise InvalidPE("signed PE has no certificate table")

    # These are the only fixed-image bytes Authenticode signing is allowed to
    # update. The certificate table itself is excluded below.
    data[checksum : checksum + 4] = b"\0" * 4
    data[security : security + 8] = b"\0" * 8
    if has_certificate:
        del data[certificate_offset:]
    return bytes(data), has_certificate


def verify(original_path: str, signed_path: str) -> None:
    original, original_had_certificate = normalized_image(original_path, False)
    signed, _ = normalized_image(signed_path, True)
    if original_had_certificate:
        expected = original
    else:
        # The certificate table must start on an 8-byte boundary. A signer may
        # insert only the minimum zero padding needed to align an unsigned file.
        expected = original + b"\0" * ((-len(original)) % 8)
    if signed != expected:
        mismatch = next(
            (i for i, (before, after) in enumerate(zip(expected, signed)) if before != after),
            min(len(expected), len(signed)),
        )
        raise InvalidPE(f"non-Authenticode byte changed at file offset 0x{mismatch:x}")


def main() -> int:
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} ORIGINAL_PE SIGNED_PE", file=sys.stderr)
        return 2
    try:
        verify(sys.argv[1], sys.argv[2])
    except (InvalidPE, OSError) as error:
        print(f"Authenticode envelope verification failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
