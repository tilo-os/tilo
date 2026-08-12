#!/usr/bin/env python3
"""Read, or rewrite a key in, a UKI's sealed command line.

    uki-cmdline.py show [UKI]
    uki-cmdline.py blank KEY [UKI]
    uki-cmdline.py set KEY=VALUE [UKI]

Run on a booted machine: the ESP is mounted at /boot on the sealed layout, so the
UKI is under /boot/EFI/Linux and root can write it. With no path it finds the single
UKI there, in either the image build's layout or the one `bootc install` leaves.

`blank` exists to make a negative result available. A check that reports the
generation as measured is only evidence if it can be shown to fail when the
generation is absent — otherwise "passed" might mean "asserted nothing". So this
overwrites `qos.generation=<N>` in the `.cmdline` section **with spaces of exactly
the same length**: the section keeps its size, every following section keeps its
file offset, and the PE stays loadable. Rewriting the section shorter would move
the rest of the image and change what fails, which would prove nothing about the
generation.

`set` is the other side of the same coin, and it is what closes the composefs seal.
`bootc container ukify` and `bootc install` compute different digests for the same
image (bootc#2334), so install refuses the UKI and names the value it wants.
*Rebuilding* the UKI with that value does not converge: a rebuild produces a new
rootfs layer, so install then computes a third digest, and a fourth. Rewriting the
digest inside the UKI that already exists cannot move the target — `/boot` is the one
tree excluded from the composefs digest — so it converges in exactly two passes. Both
digests are 128 hex characters, so the replacement is length-preserving like `blank`.

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
import os
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
    # Two layouts, both real:
    #   /boot/EFI/Linux/<kver>.efi                       as the image build writes it
    #   /boot/EFI/Linux/bootc/bootc_composefs-<digest>.efi   as `bootc install` writes it
    # The second is what an installed machine has — bootc renames the UKI after the
    # composefs digest and nests it a directory deeper — so a single-level glob finds
    # nothing on exactly the system this is meant to be run on.
    #
    # /boot is an autofs mount on the sealed layout, automounted on access. A glob
    # alone does not always trigger it, so stat the mount point first.
    try:
        os.stat("/boot")
    except OSError:
        pass
    found = sorted(glob("/boot/EFI/Linux/*.efi") + glob("/boot/EFI/Linux/*/*.efi"))
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
    if len(argv) < 2 or argv[1] not in ("show", "blank", "set"):
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
        raise SystemExit(f"{action} needs an argument, e.g. `{action} qos.generation"
                         + ("=7`" if action == "set" else "`"))
    if action == "set":
        if "=" not in argv[2]:
            raise SystemExit("set takes KEY=VALUE, e.g. `set composefs=<digest>`")
        key = argv[2].split("=", 1)[0].encode()
        replacement = argv[2].encode()
    else:
        key = argv[2].encode()
        replacement = None
    path = argv[3] if len(argv) > 3 else default_uki()

    with open(path, "r+b") as fh:
        mm = mmap.mmap(fh.fileno(), 0)
        try:
            off, size = find_cmdline(mm)
            cmdline = bytes(mm[off : off + size])
            # The token to replace, taken from the command line as it stands rather
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
            if replacement is None:
                new = b" " * len(token)
            else:
                # Length has to match exactly. Growing the section would move every
                # section after it and produce a PE that does not load; padding a
                # shorter value with spaces would work but hides a value that is not
                # the shape expected, so it is refused instead.
                if len(replacement) != len(token):
                    raise SystemExit(
                        f"replacement is {len(replacement)} bytes and the existing "
                        f"token is {len(token)}; a length-changing edit would move "
                        f"every following PE section\n  have: {token.decode()}\n"
                        f"  want: {replacement.decode()}")
                new = replacement
            mm[off + start : off + end] = new
            mm.flush()
        finally:
            mm.close()

    if replacement is None:
        print(f"blanked {token.decode()} ({len(token)} bytes -> {len(token)} spaces) in {path}")
    else:
        print(f"rewrote {key.decode()}= in {path} ({len(token)} bytes, unchanged)")
    with open(path, "rb") as fh:
        data = fh.read()
    off, size = find_cmdline(data)
    print("sealed cmdline is now:")
    print("  " + data[off : off + size].rstrip(b"\0").decode("utf-8", "replace"))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
