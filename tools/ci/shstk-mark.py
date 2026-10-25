#!/usr/bin/env python3
"""Mark an x86-64 shared library as shadow-stack compatible, changing nothing else.

    shstk-mark.py LIBRARY

The Rust toolchain's own libstd-<hash>.so carries no GNU property note, so a
thin app, which loads it, runs without hardware shadow stacks: glibc enables
them only when the program and every library it loads say SHSTK. Rust code
needs no instrumentation for a shadow stack: `-Zcf-protection=return`, with
which distributions build their libstd, only adds this note, and the workspace
marks its own binaries the same way (`-z shstk` in .cargo/config.toml).

The library is rewritten in place by appending, never moving: the note, a copy
of the program headers with a PT_LOAD mapping the note and them (PT_PHDR, if
there is one, pointing at the copy), PT_NOTE and PT_GNU_PROPERTY entries for
it, and the section headers with a .note.gnu.property section. Every byte of the original past the ELF header is
kept, so its code, dynamic symbols and .rustc metadata are the toolchain's; the
script checks that before it replaces the file. A library that already has a
GNU property note is left alone if it says SHSTK and refused otherwise.
"""

import os
import struct
import sys

PT_LOAD, PT_NOTE, PT_PHDR, PT_GNU_PROPERTY = 1, 4, 6, 0x6474E553
PF_R = 4
SHT_NOTE, SHF_ALLOC = 7, 2
EM_X86_64, ET_DYN = 62, 3
NT_GNU_PROPERTY_TYPE_0 = 5
GNU_PROPERTY_X86_FEATURE_1_AND, GNU_PROPERTY_X86_FEATURE_1_SHSTK = 0xC0000002, 2
PAGE = 0x1000
EHDR = struct.Struct("<16sHHIQQQIHHHHHH")
PHDR = struct.Struct("<IIQQQQQQ")
SHDR = struct.Struct("<IIQQQQIIQQ")
NAME = b".note.gnu.property\0"


def align(value, to):
    return (value + to - 1) // to * to


def fail(message):
    sys.exit(f"shstk-mark: {message}")


def parse(data):
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        fail("not a little-endian ELF64 file")
    ehdr = list(EHDR.unpack_from(data))
    if ehdr[1] != ET_DYN or ehdr[2] != EM_X86_64:
        fail("not an x86-64 shared object")
    phoff, shoff, phnum, shnum, shstrndx = ehdr[5], ehdr[6], ehdr[10], ehdr[12], ehdr[13]
    phdrs = [list(PHDR.unpack_from(data, phoff + i * PHDR.size)) for i in range(phnum)]
    shdrs = [list(SHDR.unpack_from(data, shoff + i * SHDR.size)) for i in range(shnum)]
    return ehdr, phdrs, shdrs, shstrndx


def shstk_note():
    prop = struct.pack("<III", GNU_PROPERTY_X86_FEATURE_1_AND, 4, GNU_PROPERTY_X86_FEATURE_1_SHSTK)
    prop += b"\0" * (align(len(prop), 8) - len(prop))
    return struct.pack("<III", 4, len(prop), NT_GNU_PROPERTY_TYPE_0) + b"GNU\0" + prop


def mark(data):
    ehdr, phdrs, shdrs, shstrndx = parse(data)
    for p in phdrs:
        if p[0] == PT_GNU_PROPERTY:
            note = data[p[2]:p[2] + p[5]]
            want = struct.pack("<III", GNU_PROPERTY_X86_FEATURE_1_AND, 4, GNU_PROPERTY_X86_FEATURE_1_SHSTK)
            if want in note:
                return None
            fail("has a GNU property note without SHSTK; not rewriting a note it already has")
    loads = [p for p in phdrs if p[0] == PT_LOAD]
    vaddr = align(max(p[3] + p[6] for p in loads), PAGE)
    offset = align(len(data), PAGE)
    note = shstk_note()
    new_phoff = offset + len(note)
    segment = len(note) + (len(phdrs) + 3) * PHDR.size
    load = [PT_LOAD, PF_R, offset, vaddr, vaddr, segment, segment, PAGE]
    last_load = max(i for i, p in enumerate(phdrs) if p[0] == PT_LOAD)
    new_phdrs = phdrs[:last_load + 1] + [load] + phdrs[last_load + 1:] + [
        [PT_NOTE, PF_R, offset, vaddr, vaddr, len(note), len(note), 8],
        [PT_GNU_PROPERTY, PF_R, offset, vaddr, vaddr, len(note), len(note), 8],
    ]
    table = len(new_phdrs) * PHDR.size
    for p in new_phdrs:
        if p[0] == PT_PHDR:
            p[2:7] = [new_phoff, vaddr + len(note), vaddr + len(note), table, table]
    old_shstr = shdrs[shstrndx]
    shstr = data[old_shstr[4]:old_shstr[4] + old_shstr[5]] + NAME
    shstr_off = offset + segment
    new_shdrs = [list(s) for s in shdrs]
    new_shdrs[shstrndx][4], new_shdrs[shstrndx][5] = shstr_off, len(shstr)
    new_shdrs.append([old_shstr[5], SHT_NOTE, SHF_ALLOC, vaddr, offset, len(note), 0, 0, 8, 0])
    new_shoff = align(shstr_off + len(shstr), 8)
    ehdr[5], ehdr[6], ehdr[10], ehdr[12] = new_phoff, new_shoff, len(new_phdrs), len(new_shdrs)
    out = bytearray(data)
    out[:EHDR.size] = EHDR.pack(*ehdr)
    out += b"\0" * (offset - len(out)) + note
    for p in new_phdrs:
        out += PHDR.pack(*p)
    out += shstr
    out += b"\0" * (new_shoff - len(out))
    for s in new_shdrs:
        out += SHDR.pack(*s)
    return bytes(out)


def verify(original, marked):
    """Everything the original has is where it was: only the ELF header changed, the rest
    was appended, and every original section reads the same through the new headers."""
    if marked[EHDR.size:len(original)] != original[EHDR.size:]:
        fail("rewrite changed the original's bytes")
    _, _, old_shdrs, old_shstrndx = parse(original)
    _, phdrs, shdrs, shstrndx = parse(marked)
    for index, (old, new) in enumerate(zip(old_shdrs, shdrs)):
        if index == old_shstrndx:
            continue
        if old != new:
            fail(f"section header {index} changed")
    names = shdrs[shstrndx]
    if marked[names[4]:names[4] + old_shdrs[old_shstrndx][5]] != \
            original[old_shdrs[old_shstrndx][4]:old_shdrs[old_shstrndx][4] + old_shdrs[old_shstrndx][5]]:
        fail("section names changed")
    prop = next(p for p in phdrs if p[0] == PT_GNU_PROPERTY)
    if not any(p[0] == PT_LOAD and p[2] <= prop[2] and prop[2] + prop[5] <= p[2] + p[5] for p in phdrs):
        fail("the property note is not in a loaded segment")


def main(argv):
    if len(argv) != 1:
        fail("usage: shstk-mark.py LIBRARY")
    path = argv[0]
    with open(path, "rb") as f:
        original = f.read()
    marked = mark(original)
    if marked is None:
        print(f"{path}: already SHSTK-marked")
        return 0
    verify(original, marked)
    mode = os.stat(path).st_mode
    temporary = f"{path}.shstk"
    with open(temporary, "wb") as f:
        f.write(marked)
    os.chmod(temporary, mode)
    os.replace(temporary, path)
    print(f"{path}: SHSTK-marked")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
