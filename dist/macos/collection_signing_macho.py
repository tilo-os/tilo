"""Prove that codesign changed only the signature envelope of a thin arm64 image.

This is deliberately not a signature verifier. Call Apple's codesign verification
separately. Both inputs must be bounded, regular-file snapshots taken by the caller.
Supported images are little-endian arm64 (not arm64e) executables/dylibs/bundles.
"""

import hashlib
import struct


class MachOError(ValueError):
    """Malformed image or an executable-content mutation."""


def _require(condition, message):
    if not condition:
        raise MachOError(message)


def _range(offset, size, limit, label):
    _require(0 <= offset <= limit and 0 <= size <= limit - offset,
             f"{label} is outside its file extent")


def _round(value, alignment):
    return (value + alignment - 1) // alignment * alignment


def inspect_macho(data):
    """Parse a snapshot and reject unsupported or overlapping file structures.

    The returned dictionary contains internal byte values and is not a receipt.
    No signature blobs are interpreted or trusted by this parser.
    """
    _require(isinstance(data, bytes), "snapshot must be bytes")
    _require(32 <= len(data) <= 2 ** 31, "image size outside supported bounds")
    header = struct.unpack_from("<8I", data)
    magic, cpu, subtype, filetype, count, cmdbytes, flags, reserved = header
    _require(magic == 0xFEEDFACF and cpu == 0x100000C and subtype == 0,
             "only thin little-endian arm64 subtype 0 is supported")
    _require(filetype in (2, 6, 8) and reserved == 0, "unsupported Mach-O header")
    _require(1 <= count <= 4096 and count * 8 <= cmdbytes <= 1024 * 1024,
             "invalid load-command count or size")
    end = 32 + cmdbytes
    _range(32, cmdbytes, len(data), "load commands")
    cursor, commands, segments, sections, references = 32, [], [], [], []
    virtual_segments = []
    signature, linkedit = None, None
    # These commands contain a linkedit_data_command, never executable code.
    linkdata = {0x1D, 0x1E, 0x26, 0x29, 0x2B, 0x2E, 0x80000033,
                0x80000034, 0x36, 0x37, 0x38}
    for _ in range(count):
        _range(cursor, 8, end, "load-command header")
        cmd, size = struct.unpack_from("<II", data, cursor)
        _require(size >= 8 and size % 8 == 0, "invalid load-command size")
        _range(cursor, size, end, "load command")
        raw = data[cursor:cursor + size]
        if cmd == 0x19:
            _require(size >= 72, "short segment command")
            fields = struct.unpack_from("<II16sQQQQiiII", raw)
            name, vmaddr, vmsize, fileoff, filesize = fields[2:7]
            nsects = fields[9]
            _require(size == 72 + 80 * nsects, "invalid segment section table")
            _range(fileoff, filesize, len(data), "segment")
            _require(filesize <= vmsize, "segment exceeds its virtual extent")
            _require(vmaddr + vmsize < 2 ** 64, "virtual segment overflows")
            if vmsize:
                virtual_segments.append((vmaddr, vmaddr + vmsize))
            segment = (fileoff, fileoff + filesize, name, vmaddr, vmaddr + vmsize)
            if filesize:
                segments.append(segment)
            if name == b"__LINKEDIT\0\0\0\0\0\0":
                _require(linkedit is None and nsects == 0, "duplicate/nonempty __LINKEDIT")
                linkedit = {"fileoff": fileoff, "filesize": filesize,
                            "vmsize": vmsize, "command_index": len(commands)}
                raw = raw[:32] + bytes(8) + raw[40:48] + bytes(8) + raw[56:]
            for index in range(nsects):
                section = struct.unpack_from("<16s16sQQIIIIIIII", data,
                                             cursor + 72 + index * 80)
                _, segname, addr, length, offset, align, reloff, nreloc, sflags, *_ = section
                _require(segname == name, "section names a different segment")
                _require(vmaddr <= addr <= vmaddr + vmsize and
                         length <= vmaddr + vmsize - addr,
                         "section exceeds segment virtual extent")
                _require(align <= 31, "unsupported section alignment")
                # S_ZEROFILL, S_GB_ZEROFILL and S_THREAD_LOCAL_ZEROFILL have no file bytes.
                if sflags & 0xFF not in (1, 0xC, 0x12) and length:
                    _require(offset >= end, "section overlaps load commands")
                    _require(fileoff <= offset and offset + length <= fileoff + filesize,
                             "section exceeds its segment file extent")
                    sections.append((offset, offset + length))
                if nreloc:
                    references.append((reloff, nreloc * 8, "section relocations"))
        elif cmd in linkdata:
            _require(size == 16, "invalid linkedit data command")
            offset, length = struct.unpack_from("<II", raw, 8)
            if cmd == 0x1D:
                _require(signature is None and length > 0, "duplicate/empty signature")
                signature = {"offset": offset, "size": length,
                             "command_index": len(commands)}
            elif length:
                references.append((offset, length, "linkedit data"))
        elif cmd == 2:  # LC_SYMTAB, nlist_64 entries
            _require(size == 24, "invalid symtab command")
            symoff, nsyms, stroff, strsize = struct.unpack_from("<4I", raw, 8)
            references.extend(((symoff, nsyms * 16, "symbol table"),
                               (stroff, strsize, "string table")))
        elif cmd == 0xB:  # LC_DYSYMTAB: six file-backed tables after symbol indexes.
            _require(size == 80, "invalid dysymtab command")
            values = struct.unpack_from("<18I", raw, 8)
            for index, width in ((6, 8), (8, 56), (10, 4), (12, 4), (14, 8), (16, 8)):
                references.append((values[index], values[index + 1] * width, "dynamic symbols"))
        elif cmd in (0x22, 0x80000022):
            _require(size == 48, "invalid dyld-info command")
            values = struct.unpack_from("<10I", raw, 8)
            references.extend((values[i], values[i + 1], "dyld info") for i in range(0, 10, 2))
        elif cmd == 0x31:  # LC_NOTE
            _require(size == 40, "invalid note command")
            references.append((*struct.unpack_from("<QQ", raw, 24), "note"))
        elif cmd == 0x80000028:  # LC_MAIN
            _require(size == 24, "invalid entry-point command")
            references.append((struct.unpack_from("<Q", raw, 8)[0], 1, "entry point"))
        elif cmd not in {0xC, 0xD, 0xE, 0xF, 0x80000018, 0x8000001F,
                         0x80000023, 0x20, 0x8000001C, 0x1B, 0x24,
                         0x25, 0x2F, 0x30, 0x32, 0x2A, 0x1A}:
            raise MachOError(f"unsupported load command {cmd:#x}")
        commands.append((cmd, raw))
        cursor += size
    _require(cursor == end, "load commands do not fill sizeofcmds")
    _require(linkedit is not None, "missing __LINKEDIT")
    _require(linkedit["fileoff"] >= end and
             linkedit["fileoff"] + linkedit["filesize"] == len(data),
             "__LINKEDIT must be the final file segment")
    for previous, current in zip(sorted(segments), sorted(segments)[1:]):
        _require(previous[1] <= current[0], "overlapping file segments")
    for previous, current in zip(sorted(sections), sorted(sections)[1:]):
        _require(previous[1] <= current[0], "overlapping file sections")
    for previous, current in zip(sorted(virtual_segments), sorted(virtual_segments)[1:]):
        _require(previous[1] <= current[0], "overlapping virtual segments")
    code_end = len(data)
    if signature:
        code_end = signature["offset"]
        _require(code_end % 16 == 0, "unaligned signature")
        _require(code_end >= linkedit["fileoff"] and
                 code_end + signature["size"] == len(data),
                 "signature must be terminal inside __LINKEDIT")
    for offset, length, label in references:
        if length:
            _range(offset, length, code_end, label)
            _require(offset >= end, f"{label} overlaps load commands")
    _require(all(finish <= code_end for _, finish in sections), "signature overlaps a section")
    return {"header": data[:16] + data[24:32], "commands": commands,
            "commands_end": end, "code_end": code_end,
            "signature": signature, "linkedit": linkedit}


def compare_signed_macho(before, after):
    """Require an added/replaced signature and byte-identical executable content.

    Return public sizes/hashes suitable for a receipt, or raise MachOError.
    New signatures may append only zero alignment bytes (at most 15). Existing
    signatures retain their original start offset. No general load-command,
    rpath, section, identifier, architecture or entitlement mutation is allowed.
    Entitlements live in the signature and require separate codesign validation.
    """
    old, new = inspect_macho(before), inspect_macho(after)
    _require(new["signature"] is not None, "signed image has no signature command")
    _require(old["header"] == new["header"], "Mach-O header changed")
    old_commands = [raw for cmd, raw in old["commands"] if cmd != 0x1D]
    new_commands = [raw for cmd, raw in new["commands"] if cmd != 0x1D]
    _require(old_commands == new_commands, "non-signature load commands changed")
    if old["signature"]:
        _require(old["signature"]["command_index"] == new["signature"]["command_index"],
                 "signature load command moved")
        _require(old["code_end"] == new["code_end"], "existing signature offset changed")
    else:
        _require(new["signature"]["command_index"] == len(old["commands"]),
                 "new signature load command must be appended")
        _require(new["commands_end"] == old["commands_end"] + 16,
                 "unexpected load-command growth")
        _require(before[old["commands_end"]:new["commands_end"]] == bytes(16),
                 "signature load command consumed nonzero header bytes")
        _require(new["code_end"] == _round(len(before), 16),
                 "unexpected padding before appended signature")
        _require(after[len(before):new["code_end"]] == bytes(new["code_end"] - len(before)),
                 "nonzero signature alignment padding")
    start = max(old["commands_end"], new["commands_end"])
    _require(before[start:old["code_end"]] == after[start:old["code_end"]],
             "non-signature file bytes changed")
    old_vm, new_vm = old["linkedit"]["vmsize"], new["linkedit"]["vmsize"]
    _require(new_vm == old_vm or new_vm == _round(new["linkedit"]["filesize"], 16384),
             "unexpected __LINKEDIT virtual-size change")
    return {"before_sha256": hashlib.sha256(before).hexdigest(),
            "after_sha256": hashlib.sha256(after).hexdigest(),
            "preserved_code_end": old["code_end"],
            "signature_offset": new["code_end"],
            "signature_size": new["signature"]["size"],
            "signature_only_change": True}
