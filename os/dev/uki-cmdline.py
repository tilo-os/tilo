#!/usr/bin/env python3
"""Read, or blank a key out of, a UKI's sealed command line.

    uki-cmdline.py show [UKI]
    uki-cmdline.py blank KEY [UKI]

Run on a booted machine (the ESP is mounted at /boot on the sealed layout, so the
UKI is at /boot/EFI/Linux/<kver>.efi and root can write it). With no path it finds
the single UKI there.

`blank` exists to make a negative result available. A check that reports the
generation as measured is only evidence if it can be shown to fail when the
generation is absent — otherwise "passed" might mean "asserted nothing". So this
overwrites `qos.generation=<N>` in the `.cmdline` section **with spaces of exactly
the same length**: the section keeps its size, every following section keeps its
file offset, and the PE stays loadable. Rewriting the section shorter would move
the rest of the image and change what fails, which would prove nothing about the
generation.

Two things it deliberately does not do:

  * It does not fix up the PE checksum. Nothing on this path verifies one, and a
    UKI whose signature had to be re-made would no longer be the artefact under
    test.
  * It does not touch anything but those bytes, so the boot it produces measures
    normally into PCR 11 — with a different value, because the command line
    changed, which is the whole point. The result is a UKI that measures perfectly
    and carries no generation.
"""

import mmap
import struct
import sys
from glob import glob


def sections(data):
    """Yield (name, file_offset, raw_size, virtual_size) for each PE section."""
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe : pe + 4] != b"PE\0\0":
        raise SystemExit("not a PE image: no PE signature at e_lfanew")
    nsec = struct.unpack_from("<H", data, pe + 6)[0]
    optsz = struct.unpack_from("<H", data, pe + 20)[0]
    base = pe + 24 + optsz
    for i in range(nsec):
        o = base + i * 40
        name = data[o : o + 8].rstrip(b"\0").decode("ascii", "replace")
        vsz = struct.unpack_from("<I", data, o + 8)[0]
        rsz = struct.unpack_from("<I", data, o + 16)[0]
        raw = struct.unpack_from("<I", data, o + 20)[0]
        yield name, raw, rsz, vsz


def find_cmdline(data):
    for name, raw, rsz, vsz in sections(data):
        if name == ".cmdline":
            return raw, min(rsz, vsz)
    raise SystemExit("this PE has no .cmdline section, so it is not a UKI")


def default_uki():
    found = sorted(glob("/boot/EFI/Linux/*.efi"))
    if not found:
        raise SystemExit(
            "no UKI under /boot/EFI/Linux — on the sealed layout the ESP is mounted "
            "at /boot, so either this is not a sealed install or /boot is not mounted"
        )
    if len(found) > 1:
        raise SystemExit(
            "more than one UKI under /boot/EFI/Linux; name the one to act on:\n  "
            + "\n  ".join(found)
        )
    return found[0]


def main(argv):
    if len(argv) < 2 or argv[1] not in ("show", "blank"):
        raise SystemExit(__doc__)
    action = argv[1]

    if action == "show":
        path = argv[2] if len(argv) > 2 else default_uki()
        with open(path, "rb") as fh:
            data = fh.read()
        off, size = find_cmdline(data)
        print(data[off : off + size].rstrip(b"\0").decode("utf-8", "replace"))
        return 0

    if len(argv) < 3:
        raise SystemExit("blank needs a key, e.g. `blank qos.generation`")
    key = argv[2].encode()
    path = argv[3] if len(argv) > 3 else default_uki()

    with open(path, "r+b") as fh:
        mm = mmap.mmap(fh.fileno(), 0)
        try:
            off, size = find_cmdline(mm)
            cmdline = bytes(mm[off : off + size])
            # The token to blank, taken from the command line as it stands rather
            # than assembled here: its length is what has to be preserved, and only
            # the file knows it.
            start = cmdline.find(key + b"=")
            if start < 0:
                raise SystemExit(f"{key.decode()}= is not on the sealed command line:\n  "
                                 + cmdline.rstrip(b"\0").decode("utf-8", "replace"))
            end = start
            while end < len(cmdline) and cmdline[end : end + 1] not in (b" ", b"\0", b"\n"):
                end += 1
            token = cmdline[start:end]
            mm[off + start : off + end] = b" " * len(token)
            mm.flush()
        finally:
            mm.close()

    print(f"blanked {token.decode()} ({len(token)} bytes -> {len(token)} spaces) in {path}")
    with open(path, "rb") as fh:
        data = fh.read()
    off, size = find_cmdline(data)
    print("sealed cmdline is now:")
    print("  " + data[off : off + size].rstrip(b"\0").decode("utf-8", "replace"))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
