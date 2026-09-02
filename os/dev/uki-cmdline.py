#!/usr/bin/env python3
"""Read, or rewrite a key in, a UKI's sealed command line or its .osrel.

    uki-cmdline.py show [UKI]
    uki-cmdline.py blank KEY [UKI]
    uki-cmdline.py set KEY=VALUE [UKI]
    uki-cmdline.py append KEY=VALUE [UKI]
    uki-cmdline.py replace OLD=VALUE NEW=VALUE [UKI]
    uki-cmdline.py osrel [UKI]
    uki-cmdline.py set-osrel KEY=VALUE [KEY=VALUE ...] UKI

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

`set-osrel` is the third of the family, and it is what makes a derived UKI
*identifiable*. systemd-boot discovers any `*.efi` at the top level of
`\\EFI\\Linux` as a Type #2 entry and takes its menu title from the `.osrel`
section's `PRETTY_NAME`, its ordering from `SORT_KEY`. A repair image derived
from a deployment therefore appears in the boot menu under the deployment's own
name — indistinguishable from the thing it exists to repair — unless those two
values are changed.

It cannot be length-preserving (the replacement is shorter, the added key is
new), so it works differently: `.osrel` is a small text section with raw slack
after it (629 bytes of content in a 1024-byte raw section, on the image this was
written against). The new content is written into that slack and the section's
**VirtualSize** header field is updated. No file offset moves, so every following
section is untouched — the same property the length-preserving edits have, bought
a different way. It refuses when the new content does not fit the raw section, or
when the raw section is larger than the gap to the next section's virtual
address.

Why not a Type #1 loader entry instead, which would carry a title in a plain
text file: bootc's composefs backend parses every `.conf` in
`/boot/loader/entries` as one of its own, and a foreign one makes `bootc status`
fail outright ("efi does not start with custom prefix"), taking `bootc upgrade`
with it. Type #2 needs no `.conf` at all, so bootc never sees it. Established by
putting one on a booted machine, not by reading.

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


def section_table(data):
    """Yield (header_offset, name, virtual_address, virtual_size, raw_size, raw_offset)."""
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
        va = struct.unpack_from("<I", data, o + 12)[0]
        rsz = struct.unpack_from("<I", data, o + 16)[0]
        raw = struct.unpack_from("<I", data, o + 20)[0]
        yield o, name, va, vsz, rsz, raw


def sections(data):
    """Yield (name, file_offset, raw_size, virtual_size) for each PE section."""
    for _o, name, _va, vsz, rsz, raw in section_table(data):
        yield name, raw, rsz, vsz


def find_cmdline(data):
    for name, raw, rsz, vsz in sections(data):
        if name == ".cmdline":
            return raw, min(rsz, vsz)
    raise SystemExit("this PE has no .cmdline section, so it is not a UKI")


def find_cmdline_room(data):
    """Return cmdline header/raw geometry without moving another PE section."""
    entries = list(section_table(data))
    for o, name, va, vsz, rsz, raw in entries:
        if name != ".cmdline":
            continue
        room = rsz
        nxt = [e for e in entries if e[2] > va]
        if nxt:
            room = min(room, min(e[2] for e in nxt) - va)
        return o, raw, rsz, vsz, room
    raise SystemExit("this PE has no .cmdline section, so it is not a UKI")


def append_cmdline(path, token):
    if "=" not in token or any(c.isspace() for c in token):
        raise SystemExit("append takes one complete KEY=VALUE token")
    with open(path, "r+b") as fh:
        mm = mmap.mmap(fh.fileno(), 0)
        try:
            hdr, raw, rsz, vsz, room = find_cmdline_room(mm)
            current = bytes(mm[raw : raw + min(rsz, vsz)]).rstrip(b"\0 \n")
            key = token.split("=", 1)[0].encode() + b"="
            if any(part.startswith(key) for part in current.split()):
                raise SystemExit(f"{key[:-1].decode()}= is already on the sealed command line")
            new = current + (b" " if current else b"") + token.encode()
            if len(new) > room:
                raise SystemExit(
                    f".cmdline would need {len(new)} bytes and only {room} are "
                    "available without moving another PE section"
                )
            mm[raw : raw + rsz] = new + b"\0" * (rsz - len(new))
            struct.pack_into("<I", mm, hdr + 8, len(new))
            mm.flush()
            return len(new), room
        finally:
            mm.close()


def find_osrel(data):
    """(header_offset, raw_offset, raw_size, virtual_size, room) for `.osrel`.

    `room` is how far the content may grow: the raw section's size, capped by the
    distance to the next section's virtual address. Growing past either would
    make a section overlap something, which is a UKI that does not load.
    """
    entries = list(section_table(data))
    for i, (o, name, va, vsz, rsz, raw) in enumerate(entries):
        if name != ".osrel":
            continue
        room = rsz
        nxt = [e for e in entries if e[2] > va]
        if nxt:
            gap = min(e[2] for e in nxt) - va
            room = min(room, gap)
        return o, raw, rsz, vsz, room
    raise SystemExit("this PE has no .osrel section, so systemd-boot would have "
                     "no title to show for it")


def read_osrel(data):
    _o, raw, rsz, vsz, _room = find_osrel(data)
    return data[raw : raw + min(rsz, vsz)].rstrip(b"\0").decode("utf-8", "replace")


def rewrite_osrel(path, assignments):
    """Set KEY=VALUE pairs in `.osrel`, in place, without moving any section."""
    with open(path, "rb") as fh:
        data = fh.read()
    hdr, raw, rsz, vsz, room = find_osrel(data)
    text = data[raw : raw + min(rsz, vsz)].rstrip(b"\0").decode("utf-8")

    lines = text.splitlines()
    for assign in assignments:
        if "=" not in assign:
            raise SystemExit(f"set-osrel takes KEY=VALUE, got {assign!r}")
        key = assign.split("=", 1)[0]
        replaced = False
        for i, line in enumerate(lines):
            if line.split("=", 1)[0] == key:
                lines[i] = assign
                replaced = True
                break
        if not replaced:
            lines.append(assign)
    new = ("\n".join(lines) + "\n").encode("utf-8")

    if len(new) > room:
        raise SystemExit(
            f".osrel would need {len(new)} bytes and only {room} are available "
            f"in place (raw size {rsz}). Growing the section would move every "
            f"following one and produce a PE that does not load."
        )

    with open(path, "r+b") as fh:
        mm = mmap.mmap(fh.fileno(), 0)
        try:
            mm[raw : raw + rsz] = new + b"\0" * (rsz - len(new))
            # The section's VirtualSize is what systemd reads the section's
            # length from; the raw size is unchanged, so nothing after this
            # section moves by a single byte.
            struct.pack_into("<I", mm, hdr + 8, len(new))
            mm.flush()
        finally:
            mm.close()
    return len(new), room


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
    if len(argv) < 2 or argv[1] not in (
        "show", "blank", "set", "append", "replace", "osrel", "set-osrel"
    ):
        raise SystemExit(__doc__)
    action = argv[1]

    if action == "show":
        path = argv[2] if len(argv) > 2 else default_uki()
        with open(path, "rb") as fh:
            data = fh.read()
        off, size = find_cmdline(data)
        print(data[off : off + size].rstrip(b"\0").decode("utf-8", "replace"))
        return 0

    if action == "osrel":
        path = argv[2] if len(argv) > 2 else default_uki()
        with open(path, "rb") as fh:
            print(read_osrel(fh.read()))
        return 0

    if action == "set-osrel":
        if len(argv) < 4:
            raise SystemExit("set-osrel needs at least one KEY=VALUE and a UKI path")
        *assignments, path = argv[2:]
        if not assignments:
            raise SystemExit("set-osrel needs at least one KEY=VALUE")
        used, room = rewrite_osrel(path, assignments)
        print(f"rewrote .osrel in {path} ({used} of {room} bytes available in place)")
        print("it now reads:")
        with open(path, "rb") as fh:
            for line in read_osrel(fh.read()).splitlines():
                print("  " + line)
        return 0

    if action == "append":
        if len(argv) not in (3, 4):
            raise SystemExit("append needs KEY=VALUE [UKI]")
        path = argv[3] if len(argv) == 4 else default_uki()
        used, room = append_cmdline(path, argv[2])
        print(f"appended {argv[2].split('=', 1)[0]}= to {path} ({used} of {room} bytes)")
        return 0

    if action == "replace":
        if len(argv) not in (4, 5):
            raise SystemExit("replace needs OLD=VALUE NEW=VALUE [UKI]")
        old = argv[2].encode()
        replacement = argv[3].encode()
        if b"=" not in old or b"=" not in replacement:
            raise SystemExit("replace arguments must both be complete KEY=VALUE tokens")
        if len(old) != len(replacement):
            raise SystemExit(
                f"replacement is {len(replacement)} bytes and the existing token is "
                f"{len(old)}; a length-changing edit would move following PE sections"
            )
        key = old.split(b"=", 1)[0]
        path = argv[4] if len(argv) == 5 else default_uki()
    elif len(argv) < 3:
        raise SystemExit(f"{action} needs an argument, e.g. `{action} qos.generation"
                         + ("=7`" if action == "set" else "`"))
    elif action == "set":
        if "=" not in argv[2]:
            raise SystemExit("set takes KEY=VALUE, e.g. `set composefs=<digest>`")
        key = argv[2].split("=", 1)[0].encode()
        replacement = argv[2].encode()
        path = argv[3] if len(argv) > 3 else default_uki()
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
            if action == "replace" and token != old:
                raise SystemExit(
                    f"refusing to replace an unexpected token\n  have: {token.decode()}\n"
                    f"  expected: {old.decode()}"
                )
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
